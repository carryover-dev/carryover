//! `next_action` extractor — pulls the final actionable sentence from the
//! latest assistant turn.

use super::util::truncate_at_word;
use crate::storage::LedgerRow;
use serde_json;

pub const MAX_NEXT_ACTION_CHARS: usize = 2000;
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

        // Content may be a JSON array (Claude stores message.content as an array
        // of blocks). Extract text blocks; skip if there are none (tool-only turns).
        let prose: String;
        let text_input = if trimmed.starts_with('[') {
            match extract_text_from_content_array(trimmed) {
                Some(t) if !t.is_empty() => {
                    prose = t;
                    &prose as &str
                }
                _ => continue,
            }
        } else {
            trimmed
        };

        let stripped = strip_code_fences(text_input);
        let text = stripped.trim();

        if text.is_empty() {
            continue;
        }

        return truncate_at_word(text, MAX_NEXT_ACTION_CHARS);
    }

    // No assistant text found (e.g., Cursor sessions don't store responses
    // locally). Fall back to the latest user prompt so the handoff still has
    // a meaningful "Next action" — the user's most recent intent.
    for row in rows.iter().rev() {
        if row.role != "user" {
            continue;
        }
        let trimmed = row.content.trim();
        if trimmed.is_empty() || trimmed.starts_with('[') {
            continue; // skip tool-result wrappers
        }
        let first_line = trimmed.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
        if first_line.is_empty() {
            continue;
        }
        return format!(
            "Continue: {}",
            truncate_at_word(first_line.trim(), MAX_NEXT_ACTION_CHARS - 10)
        );
    }

    NO_NEXT_ACTION_SENTINEL.to_string()
}

/// Parse a JSON content array and join all `{"type":"text","text":"..."}` blocks.
/// Returns None if the input isn't a valid array or has no text blocks.
fn extract_text_from_content_array(s: &str) -> Option<String> {
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
    fn returns_full_last_assistant_response() {
        let rows = vec![make_row("assistant", "I did X. Now run Y.")];
        // Returns the full text, not just the last sentence.
        assert_eq!(extract_next_action(&rows), "I did X. Now run Y.");
    }

    #[test]
    fn strips_code_fences() {
        let content = "Here is the code:\n```\nlet x = 1;\n```\nNow compile it.";
        let rows = vec![make_row("assistant", content)];
        let result = extract_next_action(&rows);
        assert!(result.contains("Now compile it."), "got: {result}");
        assert!(
            !result.contains("let x = 1"),
            "code fence not stripped: {result}"
        );
    }

    #[test]
    fn handles_question_mark_in_response() {
        let rows = vec![make_row("assistant", "Step one done. Should we proceed?")];
        let result = extract_next_action(&rows);
        assert!(result.contains("Step one done"), "got: {result}");
        assert!(result.contains("Should we proceed?"), "got: {result}");
    }

    #[test]
    fn truncates_very_long_response_at_word_boundary() {
        let long = "word ".repeat(600); // well over 2000 chars
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
    fn falls_back_to_user_prompt_when_no_assistant() {
        // For Cursor-style sessions where AI responses aren't stored locally,
        // we fall back to the latest user prompt prefixed with "Continue: ".
        let rows = vec![make_row("user", "what should I do?")];
        let result = extract_next_action(&rows);
        assert!(
            result.starts_with("Continue: "),
            "expected user-prompt fallback, got: {result}"
        );
        assert!(result.contains("what should I do?"));
    }

    #[test]
    fn returns_sentinel_when_no_user_or_assistant() {
        let rows = vec![make_row("system", "boot")];
        assert_eq!(extract_next_action(&rows), NO_NEXT_ACTION_SENTINEL);
    }

    #[test]
    fn handles_empty_ledger() {
        assert_eq!(extract_next_action(&[]), NO_NEXT_ACTION_SENTINEL);
    }

    #[test]
    fn returns_full_response_including_bullets() {
        // Bullet prefixes are preserved in full-response mode.
        let rows = vec![make_row("assistant", "- Run cargo test.\n- Then push.")];
        let result = extract_next_action(&rows);
        assert!(result.contains("cargo test"), "got: {result}");
    }
}
