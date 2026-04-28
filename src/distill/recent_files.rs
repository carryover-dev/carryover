//! `recent_files` extractor — lists the most recently touched source files.
//!
//! Activates only when the ledger contains at least one row with
//! `tool_calls_json` or `files_touched_json` populated (coding session).
//! For non-coding sessions returns an empty `Vec`.

use crate::storage::LedgerRow;
use std::cmp::Reverse;
use std::collections::HashMap;

pub const MAX_RECENT_FILES: usize = 10;

/// Extract the most recently touched file paths from the ledger.
///
/// Algorithm:
/// 1. Activation guard: if no row has `tool_calls_json.is_some()` or
///    `files_touched_json.is_some()`, return empty Vec (non-coding session).
/// 2. Iterate rows in chronological order (oldest first).
/// 3. For each row with `files_touched_json` populated, deserialize as
///    `Vec<String>` and record path → most-recent row index (last-write-wins).
/// 4. Sort paths by tracked index descending (newest-first), take at most
///    `MAX_RECENT_FILES`.
pub fn extract_recent_files(rows: &[LedgerRow]) -> Vec<String> {
    // Activation guard.
    let is_coding = rows
        .iter()
        .any(|r| r.tool_calls_json.is_some() || r.files_touched_json.is_some());
    if !is_coding {
        return Vec::new();
    }

    // Map path → latest row index seen.
    let mut path_index: HashMap<String, usize> = HashMap::new();

    for (idx, row) in rows.iter().enumerate() {
        let Some(ref json) = row.files_touched_json else {
            continue;
        };
        let paths: Vec<String> = match serde_json::from_str(json) {
            Ok(v) => v,
            Err(_) => continue, // malformed — skip silently
        };
        for path in paths {
            path_index.insert(path, idx);
        }
    }

    // Sort by index descending (newest first), take MAX_RECENT_FILES.
    let mut entries: Vec<(String, usize)> = path_index.into_iter().collect();
    entries.sort_by_key(|e| Reverse(e.1));
    entries.truncate(MAX_RECENT_FILES);
    entries.into_iter().map(|(path, _)| path).collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_row(tool_calls_json: Option<&str>, files_touched_json: Option<&str>) -> LedgerRow {
        LedgerRow {
            session_id: "s1".to_string(),
            tool: "claude".to_string(),
            ts: 0,
            role: "tool_use".to_string(),
            content: String::new(),
            tool_calls_json: tool_calls_json.map(str::to_string),
            files_touched_json: files_touched_json.map(str::to_string),
            parent_id: None,
        }
    }

    #[test]
    fn extracts_paths_from_files_touched() {
        let rows = vec![
            make_row(Some("[]"), Some(r#"["src/a.rs"]"#)),
            make_row(Some("[]"), Some(r#"["src/b.rs"]"#)),
            make_row(Some("[]"), Some(r#"["src/c.rs"]"#)),
        ];
        let result = extract_recent_files(&rows);
        assert_eq!(result.len(), 3);
        assert!(result.contains(&"src/a.rs".to_string()));
        assert!(result.contains(&"src/b.rs".to_string()));
        assert!(result.contains(&"src/c.rs".to_string()));
    }

    #[test]
    fn last_write_wins_per_path() {
        let rows = vec![
            make_row(Some("[]"), Some(r#"["src/a.rs"]"#)),
            make_row(Some("[]"), Some(r#"["src/b.rs"]"#)),
            make_row(Some("[]"), Some(r#"["src/a.rs"]"#)), // a.rs again at index 2
        ];
        let result = extract_recent_files(&rows);
        // Only one entry for src/a.rs; it should be at the front (newest index=2)
        let count = result.iter().filter(|p| p.as_str() == "src/a.rs").count();
        assert_eq!(count, 1, "duplicate paths should be deduplicated");
        assert_eq!(result[0], "src/a.rs", "newest path should be first");
    }

    #[test]
    fn caps_at_10_files() {
        let rows: Vec<LedgerRow> = (0..15)
            .map(|i| make_row(Some("[]"), Some(&format!(r#"["src/file{i}.rs"]"#))))
            .collect();
        let result = extract_recent_files(&rows);
        assert_eq!(result.len(), MAX_RECENT_FILES);
    }

    #[test]
    fn non_coding_session_returns_empty() {
        let rows = vec![LedgerRow {
            session_id: "s1".to_string(),
            tool: "claude".to_string(),
            ts: 0,
            role: "user".to_string(),
            content: "hello".to_string(),
            tool_calls_json: None,
            files_touched_json: None,
            parent_id: None,
        }];
        let result = extract_recent_files(&rows);
        assert!(
            result.is_empty(),
            "non-coding session must return empty vec"
        );
    }

    #[test]
    fn handles_malformed_files_touched_json() {
        let rows = vec![
            make_row(Some("[]"), Some("not json")), // bad — skipped
            make_row(Some("[]"), Some(r#"["src/good.rs"]"#)), // good
        ];
        let result = extract_recent_files(&rows);
        assert_eq!(result, vec!["src/good.rs".to_string()]);
    }

    #[test]
    fn preserves_chronological_priority() {
        // Files referenced earlier appear LATER in the result (newest-first).
        let rows = vec![
            make_row(Some("[]"), Some(r#"["src/old.rs"]"#)),
            make_row(Some("[]"), Some(r#"["src/new.rs"]"#)),
        ];
        let result = extract_recent_files(&rows);
        assert_eq!(result[0], "src/new.rs", "newest file should come first");
        assert_eq!(result[1], "src/old.rs");
    }

    #[test]
    fn handles_empty_ledger() {
        let result = extract_recent_files(&[]);
        assert!(result.is_empty());
    }
}
