//! `progress_log` extractor — converts a batch of LedgerRows into
//! timestamped one-line progress entries suitable for appending to
//! `.carryover/progress.md`.

use super::util::truncate_at_word;
use crate::storage::LedgerRow;

pub const MAX_ENTRY_CHARS: usize = 150;

const SESSION_WATERMARK_PREFIX: &str = "<!-- session: ";
const SESSION_WATERMARK_SUFFIX: &str = " -->";

/// Return the session id stored in the watermark comment on line 1, or None.
fn extract_session_watermark(existing: &str) -> Option<&str> {
    let first_line = existing.lines().next()?;
    first_line
        .strip_prefix(SESSION_WATERMARK_PREFIX)?
        .strip_suffix(SESSION_WATERMARK_SUFFIX)
}

/// Strip the watermark line (line 1) if present, returning the rest.
fn strip_watermark(existing: &str) -> &str {
    let first_line = existing.lines().next().unwrap_or("");
    if first_line.starts_with(SESSION_WATERMARK_PREFIX) {
        existing.find('\n').map(|pos| &existing[pos + 1..]).unwrap_or("")
    } else {
        existing
    }
}

/// Build a one-liner summary of the previous session for use as a divider.
fn prev_session_summary(existing: &str) -> String {
    let stripped = strip_watermark(existing);
    let date = stripped
        .lines()
        .find(|l| l.starts_with("- ["))
        .and_then(|l| l.strip_prefix("- [").and_then(|s| s.split('T').next()))
        .unwrap_or("unknown");
    let task = stripped
        .lines()
        .find(|l| l.contains("] [user] "))
        .and_then(|l| l.split("] [user] ").nth(1))
        .unwrap_or("");
    if task.is_empty() {
        format!("_Previous session ({date})_")
    } else {
        format!("_Previous session ({date}): {}_", truncate_at_word(task, 80))
    }
}

/// Build one formatted progress entry per user/assistant turn in `rows`.
///
/// Format: `- [ISO_TS] [role] first meaningful line of content`
///
/// User rows with JSON-array content (tool results / skill injections)
/// are skipped. Assistant rows with JSON-array content are text-extracted
/// before formatting.
pub fn extract_progress_entries(rows: &[LedgerRow]) -> Vec<String> {
    let mut entries = Vec::new();
    for row in rows {
        if row.role != "user" && row.role != "assistant" {
            continue;
        }
        let trimmed = row.content.trim();
        if trimmed.is_empty() {
            continue;
        }

        let text: String;
        let prose = if trimmed.starts_with('[') {
            if row.role == "user" {
                continue; // tool result / skill injection
            }
            match extract_text_blocks(trimmed) {
                Some(t) if !t.is_empty() => {
                    text = t;
                    &text as &str
                }
                _ => continue,
            }
        } else {
            trimmed
        };

        let first_line = prose
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("");
        if first_line.is_empty() {
            continue;
        }

        let ts = ms_to_iso(row.ts);
        let entry = format!(
            "- [{}] [{}] {}",
            ts,
            row.role,
            truncate_at_word(first_line, MAX_ENTRY_CHARS)
        );
        entries.push(entry);
    }
    entries
}

/// Extract `{"type":"text"}` blocks from a JSON content array.
fn extract_text_blocks(s: &str) -> Option<String> {
    let arr: Vec<serde_json::Value> = serde_json::from_str(s).ok()?;
    let texts: Vec<&str> = arr
        .iter()
        .filter(|item| item.get("type").and_then(|t| t.as_str()) == Some("text"))
        .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
        .collect();
    if texts.is_empty() {
        None
    } else {
        Some(texts.join("\n"))
    }
}

/// Convert a millisecond Unix timestamp to an ISO-8601 UTC string.
fn ms_to_iso(ts_ms: i64) -> String {
    use chrono::{DateTime, TimeZone, Utc};
    let secs = ts_ms / 1000;
    let dt: DateTime<Utc> = Utc.timestamp_opt(secs, 0).single().unwrap_or_else(Utc::now);
    dt.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Merge `new_entries` into `existing_log`, deduplicating by timestamp.
///
/// Returns the complete `.carryover/progress.md` contents: header,
/// entries (old + new, sorted), and a `## What to do next` footer.
pub fn build_progress_log(existing: &str, new_entries: &[String], next_action: &str, session_id: &str) -> String {
    // Detect whether we're in a new session.
    let stored_session = extract_session_watermark(existing);
    let is_new_session = match stored_session {
        Some(id) => id != session_id && !session_id.is_empty(),
        None => false, // no watermark = legacy file, keep accumulating
    };

    // Build the base entries block.
    let base: String = if is_new_session {
        format!(
            "# Carryover Progress Log\n{}\n",
            prev_session_summary(existing)
        )
    } else {
        let stripped = strip_watermark(existing);
        let entries_block = if let Some(idx) = stripped.find("\n## What to do next") {
            stripped[..idx].trim_end()
        } else {
            stripped.trim_end()
        };
        if entries_block.is_empty() {
            "# Carryover Progress Log\n".to_string()
        } else {
            format!("{entries_block}\n")
        }
    };

    // Timestamp watermark for deduplication (only meaningful when same session).
    let last_ts: Option<String> = if is_new_session {
        None
    } else {
        base.lines()
            .rev()
            .filter(|l| l.starts_with("- ["))
            .find_map(|l| {
                l.strip_prefix("- [")
                    .and_then(|s| s.split(']').next())
                    .map(|s| s.to_string())
            })
    };

    // Only append entries newer than the watermark.
    let to_append: Vec<&str> = new_entries
        .iter()
        .filter(|e| {
            let entry_ts = e
                .strip_prefix("- [")
                .and_then(|s| s.split(']').next())
                .unwrap_or("");
            match &last_ts {
                Some(last) => entry_ts > last.as_str(),
                None => true,
            }
        })
        .map(|s| s.as_str())
        .collect();

    // Prepend the session watermark so the next call can detect session changes.
    let watermark = format!("{SESSION_WATERMARK_PREFIX}{session_id}{SESSION_WATERMARK_SUFFIX}\n");
    let mut out = format!("{watermark}{base}");
    for entry in &to_append {
        out.push_str(entry);
        out.push('\n');
    }
    out.push_str("\n## What to do next\n");
    out.push_str(next_action.trim());
    out.push('\n');
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_row(role: &str, content: &str, ts: i64) -> LedgerRow {
        LedgerRow {
            session_id: "s1".to_string(),
            tool: "claude".to_string(),
            ts,
            role: role.to_string(),
            content: content.to_string(),
            tool_calls_json: None,
            files_touched_json: None,
            parent_id: None,
        }
    }

    #[test]
    fn extracts_user_and_assistant_entries() {
        let rows = vec![
            make_row("user", "build me a map app", 1_000_000),
            make_row("assistant", "Built map.html with layers.", 2_000_000),
        ];
        let entries = extract_progress_entries(&rows);
        assert_eq!(entries.len(), 2);
        assert!(entries[0].contains("[user]"));
        assert!(entries[1].contains("[assistant]"));
    }

    #[test]
    fn skips_json_array_user_rows() {
        let rows = vec![make_row(
            "user",
            r#"[{"type":"text","text":"tool result"}]"#,
            1_000_000,
        )];
        assert!(extract_progress_entries(&rows).is_empty());
    }

    #[test]
    fn extracts_text_from_assistant_json_array() {
        let rows = vec![make_row(
            "assistant",
            r#"[{"type":"text","text":"Done. Run cargo test."}]"#,
            1_000_000,
        )];
        let entries = extract_progress_entries(&rows);
        assert_eq!(entries.len(), 1);
        assert!(entries[0].contains("Done. Run cargo test."));
    }

    #[test]
    fn deduplicates_by_timestamp() {
        let existing = "# Carryover Progress Log\n- [2026-04-28T12:30:00Z] [user] first\n";
        let new_entries = vec![
            "- [2026-04-28T12:30:00Z] [user] first".to_string(), // duplicate
            "- [2026-04-28T12:31:00Z] [assistant] second".to_string(),
        ];
        let log = build_progress_log(existing, &new_entries, "next step", "test-session");
        let count = log.lines().filter(|l| l.starts_with("- [")).count();
        assert_eq!(
            count, 2,
            "should have 2 entries (no duplicate), got:\n{log}"
        );
    }

    #[test]
    fn appends_what_to_do_next() {
        let log = build_progress_log("", &[], "run cargo test", "test-session");
        assert!(log.contains("## What to do next\nrun cargo test"));
    }

    #[test]
    fn updates_what_to_do_next_on_rebuild() {
        let first = build_progress_log("", &[], "do A", "test-session");
        let new_entries = vec!["- [2026-04-28T13:00:00Z] [user] another prompt".to_string()];
        let second = build_progress_log(&first, &new_entries, "do B", "test-session");
        assert!(second.contains("do B"), "next should be updated");
        assert!(!second.contains("do A"), "old next should be gone");
    }
}
