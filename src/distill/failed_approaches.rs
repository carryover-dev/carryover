//! `failed_approaches` extractor — surfaces tool errors and retry patterns.
//!
//! Activates only when the ledger contains at least one row with
//! `tool_calls_json` populated (coding session). For non-coding sessions
//! returns an empty `Vec`.

use super::util::truncate_at_word;
use crate::storage::LedgerRow;
use std::collections::HashSet;

pub const MAX_FAILED_APPROACHES: usize = 3;
pub const MAX_BULLET_CHARS: usize = 80;

/// Error indicator substrings (checked case-insensitively).
const ERROR_INDICATORS: &[&str] = &[
    "error",
    "failed",
    "exception",
    "traceback",
    "panic",
    "sigsegv",
    "syntaxerror",
];

/// Retry pattern substrings (checked case-insensitively).
const RETRY_PATTERNS: &[&str] = &[
    "retry",
    "tried",
    "didn't work",
    "doesn't work",
    "still failing",
    "let me try",
    "approach 2",
    "next attempt",
];

/// Extract a short list of failed approaches from the ledger.
///
/// Algorithm:
/// 1. Activation guard: if no row has `tool_calls_json.is_some()`, return
///    empty Vec (non-coding session).
/// 2. Iterate rows in chronological order.
/// 3. Detect tool_result errors: `role == "tool_result"` rows whose content
///    contains any error indicator (case-insensitive).
/// 4. Detect retry patterns: `role == "assistant"` rows whose content
///    contains any retry keyword (case-insensitive).
/// 5. Summarize each hit as its first non-empty line, truncated to
///    `MAX_BULLET_CHARS`.
/// 6. Dedup via canonicalized whitespace. Cap at `MAX_FAILED_APPROACHES`.
pub fn extract_failed_approaches(rows: &[LedgerRow]) -> Vec<String> {
    // Activation guard.
    let is_coding = rows.iter().any(|r| r.tool_calls_json.is_some());
    if !is_coding {
        return Vec::new();
    }

    let mut seen: HashSet<String> = HashSet::new();
    let mut bullets: Vec<String> = Vec::new();

    for row in rows.iter() {
        if bullets.len() >= MAX_FAILED_APPROACHES {
            break;
        }

        let lower = row.content.to_lowercase();

        let is_hit = match row.role.as_str() {
            "tool_result" => ERROR_INDICATORS.iter().any(|ind| lower.contains(ind)),
            "assistant" => RETRY_PATTERNS.iter().any(|pat| lower.contains(pat)),
            _ => false,
        };

        if !is_hit {
            continue;
        }

        // Extract first non-empty line as summary.
        let summary = row
            .content
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or(&row.content);

        push_bullet(summary.to_string(), &mut seen, &mut bullets);
    }

    bullets
}

/// Canonicalize `raw` and push into `bullets` if unseen. Truncates to
/// `MAX_BULLET_CHARS`.
fn push_bullet(raw: String, seen: &mut HashSet<String>, bullets: &mut Vec<String>) {
    if bullets.len() >= MAX_FAILED_APPROACHES {
        return;
    }
    let canonical: String = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if canonical.is_empty() || seen.contains(&canonical) {
        return;
    }
    seen.insert(canonical.clone());
    bullets.push(truncate_at_word(&canonical, MAX_BULLET_CHARS));
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_row(role: &str, content: &str, tool_calls_json: Option<&str>) -> LedgerRow {
        LedgerRow {
            session_id: "s1".to_string(),
            tool: "claude".to_string(),
            ts: 0,
            role: role.to_string(),
            content: content.to_string(),
            tool_calls_json: tool_calls_json.map(str::to_string),
            files_touched_json: None,
            parent_id: None,
        }
    }

    /// A coding-session row with tool_calls populated.
    fn coding_row(role: &str, content: &str) -> LedgerRow {
        make_row(role, content, Some("[]"))
    }

    #[test]
    fn detects_tool_result_error() {
        let rows = vec![
            coding_row("tool_use", "run build"),
            make_row("tool_result", "Error: cannot find symbol", Some("[]")),
        ];
        let result = extract_failed_approaches(&rows);
        assert_eq!(result.len(), 1);
        assert!(
            result[0].contains("Error: cannot find symbol"),
            "got: {:?}",
            result
        );
    }

    #[test]
    fn detects_retry_pattern_in_assistant() {
        let rows = vec![
            coding_row("tool_use", "run build"),
            coding_row(
                "assistant",
                "Let me try a different approach: use async instead.",
            ),
        ];
        let result = extract_failed_approaches(&rows);
        assert_eq!(result.len(), 1);
        assert!(result[0].contains("Let me try"), "got: {:?}", result);
    }

    #[test]
    fn dedupes_repeated_errors() {
        let rows = vec![
            coding_row("tool_use", "run"),
            make_row("tool_result", "Error: cannot find symbol", Some("[]")),
            make_row("tool_result", "Error: cannot find symbol", Some("[]")),
            make_row("tool_result", "Error: cannot find symbol", Some("[]")),
        ];
        let result = extract_failed_approaches(&rows);
        let count = result
            .iter()
            .filter(|b| b.contains("cannot find symbol"))
            .count();
        assert_eq!(
            count, 1,
            "duplicate errors should be deduped, got: {:?}",
            result
        );
    }

    #[test]
    fn caps_at_3_bullets() {
        let rows: Vec<LedgerRow> = std::iter::once(coding_row("tool_use", "run"))
            .chain((0..5).map(|i| {
                make_row(
                    "tool_result",
                    &format!("Error: unique failure number {i}"),
                    Some("[]"),
                )
            }))
            .collect();
        let result = extract_failed_approaches(&rows);
        assert_eq!(result.len(), MAX_FAILED_APPROACHES);
    }

    #[test]
    fn truncates_long_failure_at_80_chars() {
        let long_err = format!("Error: {}", "word ".repeat(30));
        let rows = vec![
            coding_row("tool_use", "run"),
            make_row("tool_result", &long_err, Some("[]")),
        ];
        let result = extract_failed_approaches(&rows);
        assert!(!result.is_empty());
        assert!(
            result[0].ends_with('…'),
            "expected ellipsis on long bullet, got: {}",
            result[0]
        );
        assert!(
            result[0].chars().count() <= MAX_BULLET_CHARS + 1,
            "bullet too long: {} chars",
            result[0].chars().count()
        );
    }

    #[test]
    fn non_coding_returns_empty() {
        let rows = vec![make_row("tool_result", "Error: something failed", None)];
        let result = extract_failed_approaches(&rows);
        assert!(
            result.is_empty(),
            "non-coding session must return empty vec"
        );
    }

    #[test]
    fn handles_empty_ledger() {
        let result = extract_failed_approaches(&[]);
        assert!(result.is_empty());
    }
}
