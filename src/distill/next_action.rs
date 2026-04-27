//! `next_action` extractor — pulls the final actionable sentence from the
//! latest assistant turn.

use crate::storage::LedgerRow;

pub const MAX_NEXT_ACTION_CHARS: usize = 120;
pub const NO_NEXT_ACTION_SENTINEL: &str = "<no next action captured>";

/// Extract the final actionable sentence from the latest assistant turn.
///
/// Algorithm:
/// 1. Iterate rows in reverse; find the most recent `role == "assistant"` row
///    with non-empty content.
/// 2. Strip Markdown code fences (lines starting with ` ``` ` and all lines
///    they enclose).
/// 3. Strip Markdown bullet markers (`-`, `*`, `+`, `1.` etc.) at line starts.
/// 4. Find the last non-empty sentence (split on `.`, `!`, `?` followed by
///    space or end-of-string).
/// 5. Truncate to `MAX_NEXT_ACTION_CHARS` at word boundary if needed.
/// 6. Return the sentence, or `NO_NEXT_ACTION_SENTINEL` if no assistant row
///    exists.
pub fn extract_next_action(rows: &[LedgerRow]) -> String {
    for row in rows.iter().rev() {
        if row.role != "assistant" {
            continue;
        }
        let trimmed = row.content.trim();
        if trimmed.is_empty() {
            continue;
        }

        let stripped = strip_code_fences(trimmed);
        let cleaned = strip_bullet_prefixes(&stripped);
        let text = cleaned.trim();

        if text.is_empty() {
            continue;
        }

        if let Some(sentence) = last_sentence(text) {
            return truncate_at_word(&sentence, MAX_NEXT_ACTION_CHARS);
        }
    }
    NO_NEXT_ACTION_SENTINEL.to_string()
}

/// Remove Markdown code-fence blocks. Lines that start with ` ``` ` (ignoring
/// leading whitespace) toggle an "inside fence" state; all lines inside (and
/// the fence lines themselves) are dropped.
///
/// If the input contains an unclosed fence (toggled-on but no matching close),
/// the fenced lines are NOT silently dropped — they are restored at end of
/// scan and treated as prose. Without this guard a malformed assistant turn
/// could lose all its actionable content to a stray triple-backtick.
fn strip_code_fences(text: &str) -> String {
    let mut result: Vec<&str> = Vec::new();
    let mut buffered: Vec<&str> = Vec::new();
    let mut inside_fence = false;

    for line in text.lines() {
        let ltrimmed = line.trim_start();
        if ltrimmed.starts_with("```") {
            if inside_fence {
                // Closing the fence — discard the buffered lines (real fence).
                buffered.clear();
            }
            inside_fence = !inside_fence;
            // Drop the fence line itself either way.
            continue;
        }
        if inside_fence {
            buffered.push(line);
        } else {
            result.push(line);
        }
    }
    // Unclosed fence: restore the buffered content as prose.
    if inside_fence {
        result.extend(buffered);
    }
    result.join("\n")
}

/// Strip common Markdown bullet/list prefixes from the start of each line.
/// Handles `-`, `*`, `+` (with optional space) and ordered `1.`, `2.` etc.
fn strip_bullet_prefixes(text: &str) -> String {
    let lines: Vec<String> = text
        .lines()
        .map(|line| {
            let t = line.trim_start();
            // Ordered list: one or more digits followed by `.` and whitespace.
            if let Some(rest) = strip_ordered_prefix(t) {
                return rest.to_string();
            }
            // Unordered list: `-`, `*`, or `+` followed by whitespace.
            if let Some(stripped) = t
                .strip_prefix("- ")
                .or_else(|| t.strip_prefix("* "))
                .or_else(|| t.strip_prefix("+ "))
            {
                return stripped.to_string();
            }
            line.to_string()
        })
        .collect();
    lines.join("\n")
}

/// If `s` starts with digits followed by `.` and at least one space, return
/// the remainder after that prefix; otherwise return `None`.
fn strip_ordered_prefix(s: &str) -> Option<&str> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == 0 {
        return None;
    }
    if bytes.get(i) == Some(&b'.') && bytes.get(i + 1) == Some(&b' ') {
        Some(&s[i + 2..])
    } else {
        None
    }
}

/// Split `text` into sentences on `.`, `!`, or `?` followed by a space or
/// end-of-line/end-of-string. Newlines are also treated as hard sentence
/// boundaries so that multi-line text is split correctly. The terminator is
/// kept with the sentence. Returns the last non-empty sentence, or `None` if
/// the text is empty.
fn last_sentence(text: &str) -> Option<String> {
    let mut sentences: Vec<String> = Vec::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let chars: Vec<char> = line.chars().collect();
        let len = chars.len();
        let mut start = 0usize;
        let mut i = 0usize;

        while i < len {
            let c = chars[i];
            if matches!(c, '.' | '!' | '?') {
                let next_is_boundary = i + 1 >= len || chars[i + 1].is_whitespace();
                if next_is_boundary {
                    let sentence: String = chars[start..=i].iter().collect();
                    let s = sentence.trim().to_string();
                    if !s.is_empty() {
                        sentences.push(s);
                    }
                    start = i + 1;
                    while start < len && chars[start].is_whitespace() {
                        start += 1;
                    }
                    i = start;
                    continue;
                }
            }
            i += 1;
        }

        // Remainder of this line (no trailing terminator).
        if start < len {
            let remainder: String = chars[start..].iter().collect();
            let s = remainder.trim().to_string();
            if !s.is_empty() {
                sentences.push(s);
            }
        }
    }

    sentences.into_iter().rev().find(|s| !s.is_empty())
}

/// Truncate `s` to at most `max_chars` characters. If truncation is needed,
/// cut at the last ASCII whitespace boundary before the cap and append `…`.
fn truncate_at_word(s: &str, max_chars: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max_chars {
        return s.to_string();
    }
    let prefix: String = s.chars().take(max_chars).collect();
    if let Some(boundary) = prefix.rfind(|c: char| c.is_ascii_whitespace()) {
        let trimmed = prefix[..boundary].trim_end();
        format!("{}…", trimmed)
    } else {
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
    fn extracts_final_sentence_of_latest_assistant_turn() {
        let rows = vec![make_row("assistant", "I did X. Now run Y.")];
        assert_eq!(extract_next_action(&rows), "Now run Y.");
    }

    #[test]
    fn strips_code_fences() {
        let content = "Here is the code:\n```\nlet x = 1;\n```\nNow compile it.";
        let rows = vec![make_row("assistant", content)];
        let result = extract_next_action(&rows);
        assert_eq!(result, "Now compile it.");
    }

    #[test]
    fn handles_question_mark_terminator() {
        let rows = vec![make_row("assistant", "Step one done. Should we proceed?")];
        let result = extract_next_action(&rows);
        assert_eq!(result, "Should we proceed?");
    }

    #[test]
    fn truncates_long_sentence_at_word_boundary() {
        let long = format!("First step done. {} action.", "do the next ".repeat(12));
        let rows = vec![make_row("assistant", &long)];
        let result = extract_next_action(&rows);
        assert!(result.ends_with('…'), "expected ellipsis, got: {result}");
        assert!(
            result.chars().count() <= MAX_NEXT_ACTION_CHARS + 1,
            "too long: {} chars",
            result.chars().count()
        );
    }

    #[test]
    fn unclosed_code_fence_recovers_content() {
        // Malformed assistant turn: opens a fence but never closes it.
        // The trailing prose must survive — without the recovery guard
        // strip_code_fences would silently drop everything after ```rust.
        let rows = vec![make_row(
            "assistant",
            "Here is the plan.\n```rust\nfn main() {}\nNow run cargo build.",
        )];
        let result = extract_next_action(&rows);
        assert!(
            result.contains("cargo build"),
            "unclosed-fence content must be recovered, got: {result}"
        );
    }

    #[test]
    fn falls_back_to_sentinel_when_no_assistant() {
        let rows = vec![make_row("user", "what should I do?")];
        assert_eq!(extract_next_action(&rows), NO_NEXT_ACTION_SENTINEL);
    }

    #[test]
    fn handles_empty_ledger() {
        assert_eq!(extract_next_action(&[]), NO_NEXT_ACTION_SENTINEL);
    }

    #[test]
    fn strips_markdown_bullet_prefix() {
        let rows = vec![make_row("assistant", "- Run cargo test.")];
        let result = extract_next_action(&rows);
        assert_eq!(result, "Run cargo test.");
    }
}
