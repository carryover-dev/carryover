//! `progress_log` extractor — converts a batch of LedgerRows into
//! timestamped one-line progress entries suitable for appending to
//! `.carryover/progress.md`.

use super::util::truncate_at_word;
use crate::storage::LedgerRow;

pub const MAX_ENTRY_CHARS: usize = 150;

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
    let dt: DateTime<Utc> = Utc
        .timestamp_opt(secs, 0)
        .single()
        .unwrap_or_else(Utc::now);
    dt.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Merge `new_entries` into `existing_log`, deduplicating by timestamp.
///
/// Returns the complete `.carryover/progress.md` contents: header,
/// entries (old + new, sorted), and a `## What to do next` footer.
pub fn build_progress_log(existing: &str, new_entries: &[String], next_action: &str) -> String {
    // Split existing into the entries block and the trailing "What to do next" block.
    let entries_block = if let Some(idx) = existing.find("\n## What to do next") {
        existing[..idx].trim_end()
    } else {
        existing.trim_end()
    };

    // Last logged timestamp (ISO string, lexicographically sortable).
    let last_ts: Option<String> = entries_block
        .lines()
        .rev()
        .filter(|l| l.starts_with("- ["))
        .find_map(|l| {
            l.strip_prefix("- [")
                .and_then(|s| s.split(']').next())
                .map(|s| s.to_string())
        });

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

    let mut out = if entries_block.is_empty() {
        "# Carryover Progress Log\n".to_string()
    } else {
        format!("{entries_block}\n")
    };

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
        let log = build_progress_log(existing, &new_entries, "next step");
        let count = log.lines().filter(|l| l.starts_with("- [")).count();
        assert_eq!(count, 2, "should have 2 entries (no duplicate), got:\n{log}");
    }

    #[test]
    fn appends_what_to_do_next() {
        let log = build_progress_log("", &[], "run cargo test");
        assert!(log.contains("## What to do next\nrun cargo test"));
    }

    #[test]
    fn updates_what_to_do_next_on_rebuild() {
        let first = build_progress_log("", &[], "do A");
        let new_entries = vec!["- [2026-04-28T13:00:00Z] [user] another prompt".to_string()];
        let second = build_progress_log(&first, &new_entries, "do B");
        assert!(second.contains("do B"), "next should be updated");
        assert!(!second.contains("do A"), "old next should be gone");
    }
}
