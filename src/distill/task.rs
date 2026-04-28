//! `task` extractor — condenses the latest substantial user turn into one line.

use super::util::truncate_at_word;
use crate::storage::LedgerRow;

pub const MAX_TASK_CHARS: usize = 120;
pub const NO_TASK_SENTINEL: &str = "<no task captured>";

/// Extract a one-line summary of the user's task from the session.
///
/// Uses the FIRST substantial user prompt (original intent of the session),
/// not the most recent one — follow-up messages like "sure", "go on", or
/// "anything" are poor task descriptions.
///
/// Algorithm:
/// 1. Iterate rows in forward order (oldest first).
/// 2. Find the first row with `role == "user"` whose plain-text content is
///    non-empty and at least 10 characters (skip trivial follow-ups).
/// 3. Take only the first non-empty line of that content.
/// 4. Truncate to `MAX_TASK_CHARS` at word boundary if needed.
/// 5. Return the result, or `NO_TASK_SENTINEL` when no qualifying row exists.
pub fn extract_task(rows: &[LedgerRow]) -> String {
    for row in rows.iter() {
        if row.role != "user" {
            continue;
        }
        let trimmed = row.content.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Skip tool-result / skill-injection rows (JSON arrays).
        if trimmed.starts_with('[') {
            continue;
        }
        // Skip trivial follow-ups ("ok", "go on", "sure", "anything", etc.)
        if trimmed.len() < 10 {
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
    fn extracts_first_substantial_user_prompt() {
        let rows = vec![
            make_row("user", "first substantial message here"),
            make_row("assistant", "some response"),
            make_row("user", "ok"),
        ];
        assert_eq!(extract_task(&rows), "first substantial message here");
    }

    #[test]
    fn skips_trivial_follow_ups() {
        let rows = vec![
            make_row("user", "ok"),
            make_row("user", "sure"),
            make_row("user", "build me a web app with auth"),
        ];
        assert_eq!(extract_task(&rows), "build me a web app with auth");
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
