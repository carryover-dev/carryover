//! `task` extractor — condenses the latest substantial user turn into one line.

use crate::storage::LedgerRow;

pub const MAX_TASK_CHARS: usize = 120;
pub const NO_TASK_SENTINEL: &str = "<no task captured>";

/// Extract a one-line summary of the user's current task from the latest
/// substantial user turn in the ledger.
///
/// Algorithm:
/// 1. Iterate rows in reverse (newest first).
/// 2. Find the most recent row with `role == "user"` whose `content.trim()` is
///    non-empty.
/// 3. Take only the first non-empty line of that content.
/// 4. If the line exceeds `MAX_TASK_CHARS`, truncate at the last word boundary
///    before the cap and append `…`.
/// 5. Return the result, or `NO_TASK_SENTINEL` when no qualifying row exists.
pub fn extract_task(rows: &[LedgerRow]) -> String {
    for row in rows.iter().rev() {
        if row.role != "user" {
            continue;
        }
        let trimmed = row.content.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Take the first non-empty line.
        let first_line = trimmed
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or(trimmed);

        return truncate_at_word(first_line, MAX_TASK_CHARS);
    }
    NO_TASK_SENTINEL.to_string()
}

/// Truncate `s` to at most `max_chars` characters (by char count). If truncation
/// is needed, cut at the last ASCII word boundary before the limit and append `…`.
fn truncate_at_word(s: &str, max_chars: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max_chars {
        return s.to_string();
    }
    // Collect up to max_chars chars.
    let prefix: String = s.chars().take(max_chars).collect();
    // Find last whitespace boundary in the prefix.
    if let Some(boundary) = prefix.rfind(|c: char| c.is_ascii_whitespace()) {
        let trimmed = prefix[..boundary].trim_end();
        format!("{}…", trimmed)
    } else {
        // No word boundary — hard-cut.
        format!("{}…", prefix)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_row(role: &str, content: &str) -> LedgerRow {
        LedgerRow {
            session_id: "s1".to_string(),
            tool: "claude".to_string(),
            ts: 0,
            role: role.to_string(),
            content: content.to_string(),
            tool_calls_json: None,
            files_touched_json: None,
            parent_id: None,
        }
    }

    #[test]
    fn extracts_latest_user_prompt() {
        let rows = vec![
            make_row("user", "first user message"),
            make_row("assistant", "some response"),
            make_row("user", "second user message"),
        ];
        assert_eq!(extract_task(&rows), "second user message");
    }

    #[test]
    fn truncates_long_prompt_at_word_boundary() {
        // Build a string with 130 chars that has a clear word boundary before 120.
        let long = format!("{} {}", "a".repeat(115), "b".repeat(14));
        let rows = vec![make_row("user", &long)];
        let result = extract_task(&rows);
        // Result should end with ellipsis.
        assert!(result.ends_with('…'), "expected ellipsis, got: {result}");
        // Char length: ≤ 121 (120 chars + 1 for '…', which is 3 UTF-8 bytes but 1 char).
        assert!(
            result.chars().count() <= 121,
            "result too long: {} chars",
            result.chars().count()
        );
    }

    #[test]
    fn condenses_multiline_to_first_line() {
        let content = "first line\nsecond line\nthird line";
        let rows = vec![make_row("user", content)];
        assert_eq!(extract_task(&rows), "first line");
    }

    #[test]
    fn handles_empty_ledger() {
        assert_eq!(extract_task(&[]), NO_TASK_SENTINEL);
    }

    #[test]
    fn handles_no_user_rows() {
        let rows = vec![
            make_row("assistant", "response"),
            make_row("system", "system msg"),
        ];
        assert_eq!(extract_task(&rows), NO_TASK_SENTINEL);
    }

    #[test]
    fn skips_empty_user_content() {
        let rows = vec![
            make_row("user", "earlier good message"),
            make_row("user", ""),
            make_row("user", "   "),
        ];
        // Both latter rows are whitespace-only; should fall back to the earliest good one.
        assert_eq!(extract_task(&rows), "earlier good message");
    }
}
