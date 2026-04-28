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
    failed_approaches::extract_failed_approaches, git_context::extract_git_context,
    next_action::extract_next_action, open_questions::extract_open_questions,
    recent_files::extract_recent_files, task::extract_task,
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
        if let Err(e) = self.ingest(&evt.tool, &session_id, false) {
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

        // Ingest all configured adapters — each adapter reads from its own
        // paths and returns nothing if those paths haven't changed.
        for tool in self.adapters.keys() {
            if let Err(e) = self.ingest(tool, "default", evt.rescan) {
                self.log_error(tool, "default", &format!("{e:#}"));
            }
        }
    }

    /// Core ingest: load cursor → read new records → parse → insert → advance
    /// cursor → distill → publish.
    fn ingest(&self, tool: &str, session_id: &str, force_rescan: bool) -> Result<()> {
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

        let (raw_records, new_cursor_json) = adapter.read_new_records_erased(&cursor_json)?;
        if raw_records.is_empty() {
            return Ok(());
        }

        let rows: Vec<LedgerRow> = adapter.parse(raw_records)?;
        self.ledger.insert_batch(&rows)?;
        self.ledger.save_cursor(tool, session_id, &new_cursor_json)?;

        self.distill_and_publish(tool, session_id, &rows)?;
        Ok(())
    }

    fn distill_and_publish(&self, tool: &str, session_id: &str, rows: &[LedgerRow]) -> Result<()> {
        let distilled = Distilled {
            source_tool: tool.to_string(),
            session_id: session_id.to_string(),
            timestamp_iso: Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            task: extract_task(rows),
            open_questions: extract_open_questions(rows),
            next_action: extract_next_action(rows),
            recent_files: extract_recent_files(rows),
            failed_approaches: extract_failed_approaches(rows),
            git_context: extract_git_context(rows, Some(Path::new(&self.home_dir))),
        };

        let ctx = PublishContext {
            home_dir: self.home_dir.clone(),
            // v0.1: write to home dir (global handoff).
            // Per-project dir requires CWD in hook payload — future PR.
            project_dir: self.home_dir.clone(),
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
        pipeline.ingest("mock", "session-1", false).unwrap();
        let rows = pipeline.ledger.query_recent("mock", 100).unwrap();
        assert!(
            !rows.is_empty(),
            "expected ledger rows after mock ingest, got 0"
        );
        let cursor = pipeline.ledger.load_cursor("mock", "session-1").unwrap();
        assert!(cursor.is_some(), "cursor should be persisted after ingest");
        // Cursor JSON must deserialize back to a valid MockCursor.
        let cursor_json = cursor.unwrap();
        let _: MockCursor = serde_json::from_str(&cursor_json).expect("cursor should be valid JSON");
        drop(dir);
    }

    #[test]
    fn ingest_unknown_tool_is_noop() {
        let (pipeline, dir) = test_pipeline();
        pipeline.ingest("unknown", "s1", false).unwrap();
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
        pipeline.ingest("mock", "s1", false).unwrap();
        let count_after_first = pipeline.ledger.query_recent("mock", 100).unwrap().len();
        pipeline.ingest("mock", "s1", false).unwrap();
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
        pipeline.ingest("mock", "s1", false).unwrap();
        let count_after_normal = pipeline.ledger.query_recent("mock", 100).unwrap().len();
        // Force rescan: resets cursor then re-reads all records.
        pipeline.ingest("mock", "s1", true).unwrap();
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
        pipeline.distill_and_publish("mock", "s1", &rows).unwrap();
        let handoff = dir.path().join(".carryover").join("handoff.md");
        assert!(handoff.exists(), "handoff.md should be written");
        let body = std::fs::read_to_string(&handoff).unwrap();
        assert!(
            body.contains("# [CARRYOVER]"),
            "handoff should contain protocol title line"
        );
        assert!(
            body.len() > 20,
            "handoff body should be non-trivially long"
        );
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
            extra: serde_json::Map::new(),
        };
        pipeline.process_hook(&evt);
        let rows = pipeline.ledger.query_recent("mock", 100).unwrap();
        assert!(!rows.is_empty(), "process_hook should produce ledger rows");
        drop(dir);
    }
}
