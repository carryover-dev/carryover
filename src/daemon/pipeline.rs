//! Event-driven pipeline: hook events + fs-watcher events → adapter
//! ingestion → ledger rows → distillation → handoff publish.
//!
//! The pipeline is the bridge that was missing in v0.1's initial build:
//! events arrived at the daemon but were dropped before reaching adapters
//! or the ledger.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::Utc;
use serde_json::json;

use crate::adapters::AdapterKind;
use crate::cli::config::Config;
use crate::daemon::{fs_watcher::WatchEvent, hook_endpoint::HookEvent};
use crate::distill::{
    cursor_activity::extract_cursor_activity,
    failed_approaches::extract_failed_approaches,
    git_context::extract_git_context,
    next_action::extract_next_action,
    open_questions::extract_open_questions,
    progress_log::{build_progress_log, extract_progress_entries},
    recent_files::extract_recent_files,
    task::extract_task,
};
use crate::publish::{publish, Distilled, PublishContext};
use crate::storage::{Ledger, LedgerRow};

/// Shared-state pipeline. Cheaply cloneable via the `Arc` on `Ledger`.
pub struct Pipeline {
    ledger: Ledger,
    /// tool name → adapter instance
    adapters: HashMap<String, AdapterKind>,
    home_dir: PathBuf,
    resume_mode: String,
    /// Append-only audit log at `~/.carryover/events.jsonl`.
    events_log: PathBuf,
}

impl Pipeline {
    /// Build a pipeline from the user's config and an open ledger.
    pub fn build(config: &Config, ledger: Ledger, home_dir: PathBuf) -> Self {
        let adapters = build_adapter_map(&config.tools);
        let events_log = home_dir.join(".carryover").join("events.jsonl");
        Self {
            ledger,
            adapters,
            home_dir,
            resume_mode: config.resume_mode.clone(),
            events_log,
        }
    }

    /// Handle a hook POST from a tool (e.g. Claude SessionEnd).
    pub fn process_hook(&self, evt: &HookEvent) {
        let session_id = evt
            .session_id
            .clone()
            .unwrap_or_else(|| "default".to_string());
        // Use the hook's cwd as project_dir if it's a real existing directory;
        // fall back to home_dir when absent or nonexistent.
        let project_dir = evt
            .cwd
            .as_ref()
            .filter(|p| p.is_dir())
            .cloned()
            .unwrap_or_else(|| self.home_dir.clone());
        if let Err(e) = self.ingest(&evt.tool, &session_id, false, &project_dir) {
            self.log_error(&evt.tool, &session_id, &format!("{e:#}"));
        }
    }

    /// Handle an fs-watcher event (transcript file changed).
    ///
    /// When `evt.rescan` is true (inotify overflow / FSEvents coalesce per
    /// decisions.md research finding 3), reset every adapter's cursor so
    /// the next ingest is a full re-read.
    pub fn process_watch(&self, evt: &WatchEvent) {
        if evt.rescan {
            // Reset all cursors; a rescan may have missed events for any tool.
            for tool in self.adapters.keys() {
                if let Err(e) = self.ledger.save_cursor(tool, "default", "") {
                    self.log_error(tool, "default", &format!("cursor reset failed: {e:#}"));
                }
            }
        }

        // Ingest all configured adapters. Derive project_dir from the stored
        // cursor's transcript path so the handoff is written to the right
        // project directory rather than always going to home_dir.
        for tool in self.adapters.keys() {
            let project_dir = infer_project_dir_from_cursor(&self.ledger, tool, &self.home_dir)
                .unwrap_or_else(|| self.home_dir.clone());
            if let Err(e) = self.ingest(tool, "default", evt.rescan, &project_dir) {
                self.log_error(tool, "default", &format!("{e:#}"));
            }
        }
    }

    /// Core ingest: load cursor → read new records → parse → insert → advance
    /// cursor → distill → publish.
    fn ingest(
        &self,
        tool: &str,
        session_id: &str,
        force_rescan: bool,
        project_dir: &Path,
    ) -> Result<()> {
        let adapter = match self.adapters.get(tool) {
            Some(a) => a,
            None => return Ok(()), // tool not in config
        };

        let cursor_json = if force_rescan {
            String::new()
        } else {
            self.ledger
                .load_cursor(tool, session_id)?
                .unwrap_or_default()
        };

        // For Claude: ensure the cursor points to THIS project's CURRENT transcript.
        // All hook events share session_id="default", so the cursor may point to:
        //   (a) a different project's file, or
        //   (b) the right project but a stale session file (new session = new UUID file).
        // Re-seed whenever either condition is true.
        let cursor_json = if tool == "claude" {
            let canonical_home = self
                .home_dir
                .canonicalize()
                .unwrap_or_else(|_| self.home_dir.clone());
            let canonical_project = project_dir
                .canonicalize()
                .unwrap_or_else(|_| project_dir.to_path_buf());
            if canonical_project != canonical_home {
                let expected_slug = canonical_project.to_string_lossy().replace('/', "-");
                let newest = find_claude_project_transcript(&self.home_dir, project_dir);
                let needs_reseed = if cursor_json.is_empty() {
                    true
                } else {
                    let cursor_fp = serde_json::from_str::<serde_json::Value>(&cursor_json)
                        .ok()
                        .and_then(|v| {
                            v.get("file_path")
                                .and_then(|f| f.as_str())
                                .map(|s| s.to_string())
                        });
                    match (cursor_fp, &newest) {
                        // Wrong project
                        (Some(fp), _) if !fp.contains(&*expected_slug) => true,
                        // Right project but stale file (newer transcript exists)
                        (Some(fp), Some(newest_path))
                            if fp != newest_path.to_string_lossy().as_ref() =>
                        {
                            true
                        }
                        // No cursor at all
                        (None, _) => true,
                        _ => false,
                    }
                };
                if needs_reseed {
                    if let Some(transcript) = newest {
                        serde_json::json!({
                            "file_path": transcript.to_string_lossy(),
                            "byte_offset": 0,
                            "last_uuid": null
                        })
                        .to_string()
                    } else {
                        cursor_json
                    }
                } else {
                    cursor_json
                }
            } else {
                cursor_json
            }
        } else if tool == "codex" {
            // Codex stores each session in its own .jsonl. The cursor may be
            // empty (first run) or stale (pointing at an older session). If
            // either, find the newest codex transcript whose session_meta.cwd
            // matches the project_dir and reseed.
            let cursor_fp = serde_json::from_str::<serde_json::Value>(&cursor_json)
                .ok()
                .and_then(|v| {
                    v.get("file_path")
                        .and_then(|f| f.as_str())
                        .map(|s| s.to_string())
                })
                .unwrap_or_default();
            let newest = find_codex_session_transcript(&self.home_dir, project_dir);
            let needs_reseed = match (&cursor_fp.is_empty(), &newest) {
                (true, _) => true,
                (false, Some(newest_path))
                    if cursor_fp != newest_path.to_string_lossy().as_ref() =>
                {
                    true
                }
                _ => false,
            };
            if needs_reseed {
                if let Some(transcript) = newest {
                    serde_json::json!({
                        "file_path": transcript.to_string_lossy(),
                        "byte_offset": 0,
                        "last_event_seq": 0,
                    })
                    .to_string()
                } else {
                    cursor_json
                }
            } else {
                cursor_json
            }
        } else {
            cursor_json
        };

        let (raw_records, new_cursor_json) = adapter.read_new_records_erased(&cursor_json)?;
        if raw_records.is_empty() {
            // No new prompts, but the cursor (e.g. composer.lastUpdatedAt) may
            // have advanced — Cursor often updates this after the AI responds
            // and creates files. Save the advanced cursor and refresh the
            // Session activity section so newly-created files are picked up.
            if new_cursor_json != cursor_json && !new_cursor_json.is_empty() {
                let _ = self.ledger.save_cursor(tool, session_id, &new_cursor_json);
            }
            // Resolve project_dir from adapter's new cursor (most reliable).
            let refresh_dir = adapter_project_dir(&new_cursor_json, &self.home_dir)
                .unwrap_or_else(|| project_dir.to_path_buf());
            if refresh_dir != self.home_dir {
                let _ = refresh_session_activity(&refresh_dir);
            }
            return Ok(());
        }

        let rows: Vec<LedgerRow> = adapter.parse(raw_records)?;
        self.ledger.insert_batch(&rows)?;
        self.ledger
            .save_cursor(tool, session_id, &new_cursor_json)?;

        // If the adapter reports a project_dir in its new cursor, prefer that
        // over the project_dir argument. The Cursor adapter knows the active
        // workspace from composer.composerHeaders, which is more reliable than
        // the cached "default" cursor (which can be stale or empty on first
        // watch event after restart).
        let project_dir_owned: PathBuf;
        let project_dir: &Path = if let Some(adapter_dir) =
            serde_json::from_str::<serde_json::Value>(&new_cursor_json)
                .ok()
                .and_then(|v| {
                    v.get("project_dir")
                        .and_then(|d| d.as_str())
                        .map(|s| s.to_string())
                }) {
            let candidate = PathBuf::from(&adapter_dir);
            if candidate.is_dir() && candidate != self.home_dir {
                project_dir_owned = candidate;
                &project_dir_owned
            } else {
                project_dir
            }
        } else {
            project_dir
        };

        // Cache project_dir for the fs-watcher path: hook events have the real
        // cwd but watch events use session_id="default" and must look it up.
        // MERGE into the existing "default" cursor — overwriting would clobber
        // file_path/byte_offset and force a re-read from byte 0 next time.
        if session_id != "default" {
            let canonical_home = self
                .home_dir
                .canonicalize()
                .unwrap_or_else(|_| self.home_dir.clone());
            let canonical_project = project_dir
                .canonicalize()
                .unwrap_or_else(|_| project_dir.to_path_buf());
            if canonical_project != canonical_home {
                let existing = self
                    .ledger
                    .load_cursor(tool, "default")
                    .ok()
                    .flatten()
                    .unwrap_or_default();
                let mut v: serde_json::Value = if existing.is_empty() {
                    serde_json::json!({})
                } else {
                    serde_json::from_str(&existing).unwrap_or_else(|_| serde_json::json!({}))
                };
                v["project_dir"] = serde_json::json!(project_dir.to_string_lossy());
                let _ = self.ledger.save_cursor(tool, "default", &v.to_string());
            }
        }

        self.distill_and_publish(tool, session_id, &rows, project_dir)?;
        Ok(())
    }

    fn distill_and_publish(
        &self,
        tool: &str,
        session_id: &str,
        new_rows: &[LedgerRow],
        project_dir: &Path,
    ) -> Result<()> {
        // Distill from the full session history so that task/next_action
        // reflect the complete conversation, not just the latest ingest batch.
        let real_session_id = new_rows
            .first()
            .map(|r| r.session_id.as_str())
            .unwrap_or(session_id);
        let all_rows = self.ledger.query_session(real_session_id)?;
        let rows = if all_rows.is_empty() {
            new_rows
        } else {
            &all_rows
        };

        let next_action = extract_next_action(rows);
        let task = extract_task(rows);
        let timestamp_iso = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

        // Build accumulated progress log: read existing file, append new entries.
        let progress_path = project_dir.join(".carryover").join("progress.md");
        let existing_progress = std::fs::read_to_string(&progress_path).unwrap_or_default();
        let new_entries = extract_progress_entries(new_rows);
        let progress_log = build_progress_log(
            &existing_progress,
            &new_entries,
            &next_action,
            real_session_id,
        );

        // Read existing handoff to accumulate Task and Next action history.
        let handoff_path = project_dir.join(".carryover").join("handoff.md");
        let existing_handoff = std::fs::read_to_string(&handoff_path).unwrap_or_default();
        let accumulated_task =
            accumulate_section(&existing_handoff, "## Task", &task, &timestamp_iso);
        let accumulated_next_action = accumulate_section(
            &existing_handoff,
            "## Next action",
            &next_action,
            &timestamp_iso,
        );

        // Session activity: prefer fresh scan, but preserve existing when scan
        // returns empty (e.g. files outside the recent-window). Never delete
        // what's already there.
        let fresh_activity = extract_cursor_activity(project_dir);
        let session_activity = if fresh_activity.is_empty() {
            preserved_session_activity(&existing_handoff)
        } else {
            fresh_activity
        };

        let distilled = Distilled {
            source_tool: tool.to_string(),
            session_id: session_id.to_string(),
            timestamp_iso,
            task: accumulated_task,
            open_questions: extract_open_questions(rows),
            next_action: accumulated_next_action,
            recent_files: extract_recent_files(rows),
            failed_approaches: extract_failed_approaches(rows),
            git_context: extract_git_context(rows, Some(Path::new(&self.home_dir))),
            progress_log,
            session_activity,
        };

        let ctx = PublishContext {
            home_dir: self.home_dir.clone(),
            project_dir: project_dir.to_path_buf(),
            resume_mode: self.resume_mode.clone(),
        };

        publish(&distilled, &ctx)?;
        Ok(())
    }

    fn log_error(&self, tool: &str, session_id: &str, message: &str) {
        let entry = json!({
            "ts": Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            "level": "error",
            "tool": tool,
            "session_id": session_id,
            "message": message,
        });
        let line = format!("{}\n", entry);
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.events_log)
            .and_then(|mut f| f.write_all(line.as_bytes()));
    }
}

impl Pipeline {
    /// Refresh the preamble (top-of-file resume protocol header + strict
    /// response rules) of every known project's `.carryover/handoff.md`.
    ///
    /// Called on daemon start so that template/rule changes propagate to
    /// existing handoff files without waiting for a new prompt.
    pub fn refresh_all_preambles(&self) {
        let projects = self.known_project_dirs();
        let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        for (project_dir, source_tool) in projects {
            let handoff_path = project_dir.join(".carryover").join("handoff.md");
            if !handoff_path.exists() {
                continue;
            }
            let _ = rewrite_preamble(&handoff_path, &source_tool, &self.resume_mode, &now);
        }
    }

    /// Collect every (project_dir, source_tool) pair we have a cursor for.
    fn known_project_dirs(&self) -> Vec<(PathBuf, String)> {
        let mut out = Vec::new();
        for tool in self.adapters.keys() {
            if let Ok(Some(cursor_json)) = self.ledger.load_cursor(tool, "default") {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&cursor_json) {
                    if let Some(dir_str) = v.get("project_dir").and_then(|d| d.as_str()) {
                        let p = PathBuf::from(dir_str);
                        if p.is_dir() {
                            out.push((p, tool.clone()));
                        }
                    }
                }
            }
        }
        out
    }
}

/// Replace everything before (and including) the first `\n---\n` separator
/// with a freshly-rendered preamble. Idempotent — only writes if content changes.
fn rewrite_preamble(
    handoff_path: &Path,
    source_tool: &str,
    resume_mode: &str,
    timestamp_iso: &str,
) -> anyhow::Result<()> {
    let content = std::fs::read_to_string(handoff_path)?;
    let separator = "\n---\n";
    let after = match content.find(separator) {
        Some(i) => &content[i + separator.len()..],
        None => return Ok(()), // file is too malformed to safely edit
    };
    let new_preamble = crate::publish::render_preamble(source_tool, resume_mode, timestamp_iso);
    let updated = format!("{new_preamble}\n{after}");
    if updated != content {
        std::fs::write(handoff_path, updated)?;
    }
    Ok(())
}

/// Read the existing `## Session activity` block from a handoff file and
/// return its bullet-line entries (one entry per element). Used to preserve
/// the previous activity snapshot when a fresh scan returns empty.
fn preserved_session_activity(existing_handoff: &str) -> Vec<String> {
    let body = extract_section_content(existing_handoff, "## Session activity");
    body.lines()
        .map(|s| s.to_string())
        .filter(|s| !s.trim().is_empty())
        .collect()
}

/// Extract project_dir field from a serialized cursor JSON.
/// Returns None if the field is missing, empty, or points at home_dir / a
/// non-existent directory.
fn adapter_project_dir(cursor_json: &str, home_dir: &Path) -> Option<PathBuf> {
    let v: serde_json::Value = serde_json::from_str(cursor_json).ok()?;
    let dir = v.get("project_dir").and_then(|d| d.as_str())?;
    let candidate = PathBuf::from(dir);
    if candidate.is_dir() && candidate != home_dir {
        Some(candidate)
    } else {
        None
    }
}

/// Refresh just the `## Session activity` section of an existing handoff.md
/// without re-running the full distill. Catches files created by the AI tool
/// AFTER its prompt was captured (e.g., Cursor creates a file 5 seconds after
/// you submit). Idempotent — only writes if the new section differs.
fn refresh_session_activity(project_dir: &Path) -> anyhow::Result<()> {
    let handoff_path = project_dir.join(".carryover").join("handoff.md");
    if !handoff_path.exists() {
        return Ok(());
    }

    let content = std::fs::read_to_string(&handoff_path)?;
    let new_activity = crate::distill::cursor_activity::extract_cursor_activity(project_dir);

    // If the fresh scan is empty (e.g. nothing modified in the last 6h), do
    // NOT wipe the existing section — leave the previous snapshot intact.
    if new_activity.is_empty() {
        return Ok(());
    }

    let mut new_section_body = String::from("## Session activity\n");
    for line in &new_activity {
        new_section_body.push_str(line);
        new_section_body.push('\n');
    }

    let updated = replace_section(&content, "## Session activity", &new_section_body);
    if updated != content {
        std::fs::write(&handoff_path, updated)?;
    }
    Ok(())
}

/// Replace the contents of a `## Header` section in `text` with `new_section_body`.
/// If the section doesn't exist and `new_section_body` is non-empty, insert it
/// before the next section after `## Task` (best-effort).
fn replace_section(text: &str, header: &str, new_section_body: &str) -> String {
    let header_line = format!("{header}\n");
    if let Some(start) = text.find(&header_line) {
        // Find the end of this section: next "## " on its own line, or EOF.
        let after = start + header_line.len();
        let rest = &text[after..];
        let end = rest
            .match_indices("\n## ")
            .next()
            .map(|(i, _)| after + i + 1)
            .unwrap_or(text.len());

        let mut out = String::new();
        out.push_str(&text[..start]);
        if !new_section_body.is_empty() {
            out.push_str(new_section_body);
            // Ensure trailing blank line before next section.
            if !out.ends_with("\n\n") {
                out.push('\n');
            }
        }
        out.push_str(&text[end..]);
        out
    } else if !new_section_body.is_empty() {
        // No existing section — insert before "## Next action" if present, else append.
        let insert_at = text
            .find("## Next action")
            .or_else(|| text.find("## Progress log"))
            .unwrap_or(text.len());
        let mut out = String::new();
        out.push_str(&text[..insert_at]);
        out.push_str(new_section_body);
        out.push('\n');
        out.push_str(&text[insert_at..]);
        out
    } else {
        text.to_string()
    }
}

/// Append a new value to an accumulating section of the handoff (Task / Next action).
///
/// Reads the existing section content from `existing_handoff`, prepends the new
/// timestamped entry, and returns the combined block. Skips the append if the
/// new value matches the most recent entry (dedupe by content). Empty new
/// values are dropped — they don't create entries.
///
/// Output format:
/// ```text
/// - [2026-04-29T16:30:00Z] latest task
/// - [2026-04-29T15:45:00Z] earlier task
/// ```
fn accumulate_section(
    existing_handoff: &str,
    header: &str,
    new_value: &str,
    timestamp_iso: &str,
) -> String {
    let new_value = new_value.trim();
    let raw_existing = extract_section_content(existing_handoff, header);

    // Keep ONLY clean bullet entries (drop orphan plain-text from old formats).
    let bullet_lines: Vec<&str> = raw_existing
        .lines()
        .filter(|l| l.starts_with("- ["))
        .collect();
    let cleaned_existing = bullet_lines.join("\n");

    // Empty / sentinel new values: keep existing bullets (don't append a no-op).
    if new_value.is_empty() || new_value.starts_with("<no ") {
        return cleaned_existing;
    }

    let first_line = new_value.lines().next().unwrap_or(new_value).trim();

    // Dedupe against ALL existing values, not just the latest. The user's
    // task or next_action may oscillate between two values across composers
    // (e.g. "lets start" / "build cat app") and we don't want every flip
    // appended again.
    let new_value_norm = first_line.trim();
    let already_present = bullet_lines.iter().any(|l| {
        l.find("] ")
            .map(|i| l[i + 2..].trim() == new_value_norm)
            .unwrap_or(false)
    });
    if already_present {
        return cleaned_existing;
    }

    let new_entry = format!("- [{timestamp_iso}] {first_line}");
    if cleaned_existing.is_empty() {
        new_entry
    } else {
        format!("{new_entry}\n{cleaned_existing}")
    }
}

/// Extract the text between `## <header>` and the next `## ` header (or end).
fn extract_section_content(text: &str, header: &str) -> String {
    let needle = format!("{header}\n");
    let start = match text.find(&needle) {
        Some(i) => i + needle.len(),
        None => return String::new(),
    };
    let rest = &text[start..];
    // Find the next "## " on its own line.
    let end = rest
        .match_indices("\n## ")
        .next()
        .map(|(i, _)| i)
        .unwrap_or(rest.len());
    rest[..end].trim().to_string()
}

/// Find the newest Codex session transcript whose session_meta.cwd matches
/// `project_dir`. Codex writes transcripts to
/// `~/.codex/sessions/<year>/<month>/<day>/rollout-*.jsonl` — one file per
/// session. Each starts with a `session_meta` line that includes `cwd`.
///
/// Walk recently-modified .jsonl files, peek the first line, return the
/// newest one whose cwd matches. Falls back to newest globally if no match.
fn find_codex_session_transcript(home_dir: &Path, project_dir: &Path) -> Option<PathBuf> {
    let sessions_root = home_dir.join(".codex").join("sessions");
    if !sessions_root.is_dir() {
        return None;
    }

    // Collect candidate jsonl files with mtimes.
    let mut candidates: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    walk_codex_sessions(&sessions_root, 0, &mut candidates);
    if candidates.is_empty() {
        return None;
    }
    candidates.sort_by(|a, b| b.0.cmp(&a.0));
    // Limit how many we peek to avoid heavy I/O on long-lived installs.
    candidates.truncate(20);

    let target = project_dir
        .canonicalize()
        .unwrap_or_else(|_| project_dir.to_path_buf());
    let target_str = target.to_string_lossy().to_string();

    // Prefer transcripts where session_meta.cwd matches.
    for (_, path) in &candidates {
        if let Some(cwd) = peek_codex_session_cwd(path) {
            if cwd == target_str {
                return Some(path.clone());
            }
        }
    }

    // No cwd-match — fall back to the newest transcript globally.
    candidates.into_iter().next().map(|(_, p)| p)
}

fn walk_codex_sessions(cur: &Path, depth: usize, out: &mut Vec<(std::time::SystemTime, PathBuf)>) {
    if depth > 4 {
        return;
    }
    let read_dir = match std::fs::read_dir(cur) {
        Ok(rd) => rd,
        Err(_) => return,
    };
    for entry in read_dir.flatten() {
        let path = entry.path();
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.is_dir() {
            walk_codex_sessions(&path, depth + 1, out);
        } else if meta.is_file() && path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            if let Ok(mtime) = meta.modified() {
                out.push((mtime, path));
            }
        }
    }
}

/// Read the first line of a Codex session jsonl, parse session_meta, return cwd.
fn peek_codex_session_cwd(path: &Path) -> Option<String> {
    use std::io::{BufRead, BufReader};
    let f = std::fs::File::open(path).ok()?;
    let mut reader = BufReader::new(f);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let v: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    if v.get("type").and_then(|t| t.as_str()) != Some("session_meta") {
        return None;
    }
    v.get("payload")
        .and_then(|p| p.get("cwd"))
        .and_then(|c| c.as_str())
        .map(String::from)
}

/// Find the newest Claude transcript for a specific project directory.
/// Claude encodes project paths as the full path with '/' replaced by '-'.
/// Returns the newest .jsonl in that project subdir, or None if not found.
fn find_claude_project_transcript(home_dir: &Path, project_dir: &Path) -> Option<PathBuf> {
    let projects_root = home_dir.join(".claude").join("projects");
    // Encode: "/home/rohit/workspace/test-web" → "-home-rohit-workspace-test-web"
    let encoded = project_dir.to_string_lossy().replace('/', "-");
    let project_subdir = projects_root.join(&encoded);
    if !project_subdir.is_dir() {
        return None;
    }
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(&project_subdir).ok()?.flatten() {
        let p = entry.path();
        if p.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        if let Ok(meta) = p.metadata() {
            if let Ok(modified) = meta.modified() {
                if newest.as_ref().map(|(t, _)| modified > *t).unwrap_or(true) {
                    newest = Some((modified, p));
                }
            }
        }
    }
    newest.map(|(_, p)| p)
}

/// Infer the project directory from the stored cursor for `tool`.
///
/// Claude encodes project paths as the transcript file's grandparent directory
/// name: `/home/rohit/.claude/projects/-home-rohit-workspace-test4/<uuid>.jsonl`.
/// The slug `-home-rohit-workspace-test4` is the full absolute path with every
/// `/` replaced by `-`. Reversing it: replace all `-` with `/`.
///
/// Returns `None` when:
/// - No cursor is stored yet for the tool.
/// - The cursor JSON has no `file_path` key.
/// - The decoded path does not exist as a directory.
fn infer_project_dir_from_cursor(ledger: &Ledger, tool: &str, home_dir: &Path) -> Option<PathBuf> {
    let cursor_json = ledger.load_cursor(tool, "default").ok()??;
    if cursor_json.is_empty() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(&cursor_json).ok()?;

    // Fast path: hook events write an explicit project_dir into this cursor.
    if let Some(dir_str) = v.get("project_dir").and_then(|d| d.as_str()) {
        let candidate = PathBuf::from(dir_str);
        if candidate.is_dir() && candidate != home_dir {
            return Some(candidate);
        }
    }

    // Slow path: decode project dir from the Claude transcript slug.
    let file_path = v.get("file_path")?.as_str()?;
    let path = Path::new(file_path);
    let slug = path.parent()?.file_name()?.to_string_lossy();
    let decoded = slug.replace('-', "/");
    let candidate = PathBuf::from(&decoded);
    if candidate.is_dir() && candidate != home_dir {
        return Some(candidate);
    }
    None
}

fn build_adapter_map(tools: &[String]) -> HashMap<String, AdapterKind> {
    let mut map = HashMap::new();
    for tool in tools {
        let kind = match tool.as_str() {
            "claude" => Some(AdapterKind::Claude(
                crate::adapters::claude::ClaudeAdapter::new(),
            )),
            "cursor" => Some(AdapterKind::Cursor(
                crate::adapters::cursor::CursorAdapter::new(),
            )),
            "codex" => Some(AdapterKind::Codex(
                crate::adapters::codex::CodexAdapter::new(),
            )),
            _ => None,
        };
        if let Some(k) = kind {
            map.insert(tool.clone(), k);
        }
    }
    map
}

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// Construct a Pipeline with a caller-supplied adapter map.
/// Intended for tests only — not part of the public stable API.
pub fn build_for_test(
    adapters: HashMap<String, AdapterKind>,
    ledger: Ledger,
    home_dir: PathBuf,
) -> Pipeline {
    let events_log = home_dir.join(".carryover").join("events.jsonl");
    Pipeline {
        ledger,
        adapters,
        home_dir,
        resume_mode: "ask".to_string(),
        events_log,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::mock::{MockAdapter, MockCursor};
    use crate::adapters::RawRecord;
    use crate::storage::LedgerRow;
    use tempfile::tempdir;

    /// A `RawRecord` whose payload is a valid JSON-serialized `LedgerRow`.
    fn mock_record(tool: &str, offset: u64) -> RawRecord {
        let row = LedgerRow {
            session_id: "test-session".to_string(),
            tool: tool.to_string(),
            ts: offset as i64 * 1000,
            role: "user".to_string(),
            content: format!("turn {offset}"),
            tool_calls_json: None,
            files_touched_json: None,
            parent_id: None,
        };
        RawRecord {
            tool: tool.to_string(),
            payload: serde_json::to_vec(&row).unwrap(),
            offset,
        }
    }

    fn test_pipeline() -> (Pipeline, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let ledger = Ledger::open(&dir.path().join("ledger.sqlite")).unwrap();
        let home = dir.path().to_path_buf();
        let adapter = MockAdapter {
            name: "mock",
            binary: None,
            records: vec![mock_record("mock", 1), mock_record("mock", 2)],
        };
        let mut adapters = HashMap::new();
        adapters.insert("mock".to_string(), AdapterKind::Mock(adapter));
        let p = build_for_test(adapters, ledger, home);
        (p, dir)
    }

    #[test]
    fn ingest_mock_produces_ledger_rows() {
        let (pipeline, dir) = test_pipeline();
        pipeline
            .ingest("mock", "session-1", false, &pipeline.home_dir.clone())
            .unwrap();
        let rows = pipeline.ledger.query_recent("mock", 100).unwrap();
        assert!(
            !rows.is_empty(),
            "expected ledger rows after mock ingest, got 0"
        );
        let cursor = pipeline.ledger.load_cursor("mock", "session-1").unwrap();
        assert!(cursor.is_some(), "cursor should be persisted after ingest");
        // Cursor JSON must deserialize back to a valid MockCursor.
        let cursor_json = cursor.unwrap();
        let _: MockCursor =
            serde_json::from_str(&cursor_json).expect("cursor should be valid JSON");
        drop(dir);
    }

    #[test]
    fn ingest_unknown_tool_is_noop() {
        let (pipeline, dir) = test_pipeline();
        pipeline
            .ingest("unknown", "s1", false, &pipeline.home_dir.clone())
            .unwrap();
        assert_eq!(
            pipeline.ledger.query_recent("unknown", 10).unwrap().len(),
            0
        );
        drop(dir);
    }

    #[test]
    fn ingest_idempotent_after_cursor_advance() {
        // Second ingest with an advanced cursor should return 0 new rows.
        let (pipeline, dir) = test_pipeline();
        pipeline
            .ingest("mock", "s1", false, &pipeline.home_dir.clone())
            .unwrap();
        let count_after_first = pipeline.ledger.query_recent("mock", 100).unwrap().len();
        pipeline
            .ingest("mock", "s1", false, &pipeline.home_dir.clone())
            .unwrap();
        let count_after_second = pipeline.ledger.query_recent("mock", 100).unwrap().len();
        assert_eq!(
            count_after_first, count_after_second,
            "second ingest should insert 0 new rows (cursor advanced)"
        );
        drop(dir);
    }

    #[test]
    fn force_rescan_re_reads_from_start() {
        let (pipeline, dir) = test_pipeline();
        pipeline
            .ingest("mock", "s1", false, &pipeline.home_dir.clone())
            .unwrap();
        let count_after_normal = pipeline.ledger.query_recent("mock", 100).unwrap().len();
        // Force rescan: resets cursor then re-reads all records.
        pipeline
            .ingest("mock", "s1", true, &pipeline.home_dir.clone())
            .unwrap();
        let count_after_rescan = pipeline.ledger.query_recent("mock", 100).unwrap().len();
        // Rescan should produce ≥ as many rows as the first ingest.
        assert!(
            count_after_rescan >= count_after_normal,
            "rescan should produce at least as many rows as initial ingest"
        );
        drop(dir);
    }

    #[test]
    fn distill_and_publish_writes_handoff() {
        let (pipeline, dir) = test_pipeline();
        let rows = vec![LedgerRow {
            session_id: "s1".to_string(),
            tool: "mock".to_string(),
            ts: 1_000_000,
            role: "user".to_string(),
            content: "working on the login feature".to_string(),
            tool_calls_json: None,
            files_touched_json: None,
            parent_id: None,
        }];
        pipeline
            .distill_and_publish("mock", "s1", &rows, &pipeline.home_dir.clone())
            .unwrap();
        let handoff = dir.path().join(".carryover").join("handoff.md");
        assert!(handoff.exists(), "handoff.md should be written");
        let body = std::fs::read_to_string(&handoff).unwrap();
        assert!(
            body.contains("# [CARRYOVER]"),
            "handoff should contain protocol title line"
        );
        assert!(body.len() > 20, "handoff body should be non-trivially long");
        drop(dir);
    }

    #[test]
    fn process_hook_end_to_end() {
        let (pipeline, dir) = test_pipeline();
        let evt = HookEvent {
            tool: "mock".to_string(),
            event: "SessionEnd".to_string(),
            transcript_path: None,
            session_id: Some("hook-session".to_string()),
            cwd: None,
            extra: serde_json::Map::new(),
        };
        pipeline.process_hook(&evt);
        let rows = pipeline.ledger.query_recent("mock", 100).unwrap();
        assert!(!rows.is_empty(), "process_hook should produce ledger rows");
        drop(dir);
    }
}
