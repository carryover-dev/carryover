//! `open_questions` extractor — detects unresolved TODOs and open questions.
//!
//! No regex crate. Matching is done with `.find()`, `.contains()`, and
//! sentence splitting via a tiny inline helper. Patterns checked:
//! - Lines containing `TODO`, `FIXME`, or `XXX` followed by optional `:` and
//!   text — the rest of the line after the marker is the bullet.
//! - Sentences (delimited by `. `, `! `, `? `, or end-of-string) that end
//!   with a `?` character.

use super::util::truncate_at_word;
use crate::storage::LedgerRow;
use std::collections::HashSet;

pub const MAX_OPEN_QUESTIONS: usize = 5;
pub const MAX_BULLET_CHARS: usize = 80;

/// Detect unresolved questions and TODO-style markers across the session.
/// Returns up to 5 deduplicated bullets.
///
/// Only prose rows are scanned — tool result rows (content is a JSON array
/// with no `type:text` blocks) and non-user/assistant roles are skipped.
pub fn extract_open_questions(rows: &[LedgerRow]) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut bullets: Vec<String> = Vec::new();

    for row in rows.iter() {
        if row.role != "user" && row.role != "assistant" {
            continue;
        }
        let trimmed = row.content.trim();
        if trimmed.is_empty() {
            continue;
        }
        // For user rows: only scan plain-text content (actual prompts).
        // JSON array content in user rows is always a tool result or skill
        // injection — never a user-authored question.
        // For assistant rows: extract text blocks from JSON content arrays.
        let text = if row.role == "user" {
            if trimmed.starts_with('[') {
                continue; // tool result / skill injection — skip entirely
            }
            trimmed.to_string()
        } else {
            // assistant row
            if trimmed.starts_with('[') {
                match extract_prose_from_array(trimmed) {
                    Some(t) if !t.is_empty() => t,
                    _ => continue,
                }
            } else {
                trimmed.to_string()
            }
        };
        collect_bullets(&text, &mut seen, &mut bullets);
        if bullets.len() >= MAX_OPEN_QUESTIONS {
            break;
        }
    }

    bullets.truncate(MAX_OPEN_QUESTIONS);
    bullets
}

/// Extract only `{"type":"text"}` blocks from a JSON content array.
/// Returns None if the array has no text blocks (tool-only turn).
fn extract_prose_from_array(s: &str) -> Option<String> {
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

/// Scan `text` for TODO/FIXME/XXX markers and `?`-terminated sentences;
/// push new (deduplicated) bullets into `bullets`.
fn collect_bullets(text: &str, seen: &mut HashSet<String>, bullets: &mut Vec<String>) {
    if bullets.len() >= MAX_OPEN_QUESTIONS {
        return;
    }

    for line in text.lines() {
        // Check for TODO / FIXME / XXX markers.
        for marker in &["TODO", "FIXME", "XXX"] {
            if let Some(capture) = extract_marker_capture(line, marker) {
                push_bullet(capture, seen, bullets);
            }
        }

        // Check for sentences ending with `?`.
        for sentence in split_sentences(line) {
            let s = sentence.trim();
            if s.ends_with('?') && !s.is_empty() {
                push_bullet(s.to_string(), seen, bullets);
            }
        }

        if bullets.len() >= MAX_OPEN_QUESTIONS {
            return;
        }
    }
}

/// Extract the text after `MARKER` (optionally followed by `:`) on `line`.
/// Returns `None` if the marker is not present or the capture is empty.
fn extract_marker_capture(line: &str, marker: &str) -> Option<String> {
    // Find the marker as a word: it must start at position 0 or after a
    // non-alphanumeric character, and end before an alphanumeric character.
    let mut search_from = 0usize;
    while let Some(pos) = line[search_from..].find(marker) {
        let abs_pos = search_from + pos;
        // Check left boundary.
        let left_ok = abs_pos == 0
            || !line
                .as_bytes()
                .get(abs_pos - 1)
                .map(|b| b.is_ascii_alphanumeric() || *b == b'_')
                .unwrap_or(false);
        // Check right boundary.
        let after = abs_pos + marker.len();
        let right_ok = !line
            .as_bytes()
            .get(after)
            .map(|b| b.is_ascii_alphanumeric() || *b == b'_')
            .unwrap_or(false);

        if left_ok && right_ok {
            let rest = line[after..].trim_start_matches(':').trim();
            if !rest.is_empty() {
                return Some(rest.to_string());
            }
            return None;
        }
        search_from = abs_pos + 1;
    }
    None
}

/// Split `text` into sentences on `.`, `!`, or `?` followed by whitespace or
/// end-of-string. The delimiter is kept with the preceding sentence.
fn split_sentences(text: &str) -> Vec<String> {
    let mut sentences: Vec<String> = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        let c = chars[i];
        current.push(c);
        if matches!(c, '.' | '!' | '?') {
            // Peek: next char is whitespace or end-of-string.
            let next_is_boundary = i + 1 >= len || chars[i + 1].is_whitespace();
            if next_is_boundary {
                sentences.push(current.trim().to_string());
                current = String::new();
            }
        }
        i += 1;
    }
    if !current.trim().is_empty() {
        sentences.push(current.trim().to_string());
    }
    sentences
}

/// Canonicalize `raw` (collapse whitespace) and push into `bullets` if it has
/// not been seen before. Truncates to `MAX_BULLET_CHARS` at a word boundary.
fn push_bullet(raw: String, seen: &mut HashSet<String>, bullets: &mut Vec<String>) {
    if bullets.len() >= MAX_OPEN_QUESTIONS {
        return;
    }
    // Canonicalize: collapse runs of whitespace to single space.
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
    fn detects_todo_marker() {
        let rows = vec![make_row("user", "TODO: fix the bug")];
        let result = extract_open_questions(&rows);
        assert!(!result.is_empty(), "expected at least one bullet");
        assert!(
            result[0].contains("fix the bug"),
            "expected capture, got: {:?}",
            result
        );
    }

    #[test]
    fn detects_fixme_marker() {
        let rows = vec![make_row("assistant", "FIXME: handle the error case")];
        let result = extract_open_questions(&rows);
        assert!(!result.is_empty());
        assert!(result[0].contains("handle the error case"));
    }

    #[test]
    fn detects_question_at_end_of_sentence() {
        let rows = vec![make_row("user", "Should we use Postgres?")];
        let result = extract_open_questions(&rows);
        assert!(!result.is_empty(), "expected question bullet");
        assert!(
            result.iter().any(|b| b.contains("Should we use Postgres?")),
            "got: {:?}",
            result
        );
    }

    #[test]
    fn dedupes_repeated_markers() {
        let rows = vec![
            make_row("user", "TODO: same"),
            make_row("assistant", "TODO: same"),
            make_row("user", "TODO: same"),
        ];
        let result = extract_open_questions(&rows);
        let count = result.iter().filter(|b| b.contains("same")).count();
        assert_eq!(
            count, 1,
            "expected exactly 1 deduplicated entry, got: {:?}",
            result
        );
    }

    #[test]
    fn caps_at_5_bullets() {
        let rows: Vec<LedgerRow> = (0..10)
            .map(|i| make_row("user", &format!("TODO: unique item {i}")))
            .collect();
        let result = extract_open_questions(&rows);
        assert_eq!(result.len(), MAX_OPEN_QUESTIONS);
    }

    #[test]
    fn truncates_long_bullets() {
        let long_todo = format!("TODO: {}", "word ".repeat(30));
        let rows = vec![make_row("user", &long_todo)];
        let result = extract_open_questions(&rows);
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
    fn handles_empty_ledger() {
        let result = extract_open_questions(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn handles_array_content_in_assistant_row() {
        // Assistant rows with JSON content arrays are scanned.
        let content = r#"[{"type":"text","text":"TODO: x"}]"#;
        let rows = vec![make_row("assistant", content)];
        let result = extract_open_questions(&rows);
        assert!(
            !result.is_empty(),
            "expected bullet from assistant JSON array content"
        );
        assert!(result[0].contains('x'), "got: {:?}", result);
    }

    #[test]
    fn skips_json_array_user_rows() {
        // User rows with JSON arrays (tool results / skill injections) are skipped.
        let content = r#"[{"type":"text","text":"Can someone read this?"}]"#;
        let rows = vec![make_row("user", content)];
        let result = extract_open_questions(&rows);
        assert!(
            result.is_empty(),
            "user JSON array rows must be skipped, got: {:?}",
            result
        );
    }
}
