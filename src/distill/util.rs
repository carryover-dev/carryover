//! Shared helpers for distillers.

/// Truncate `s` to at most `max_chars` Unicode scalar values, breaking on
/// the last word boundary before the limit and appending `…` when truncation
/// occurs. Returns the input unchanged if it already fits.
///
/// UTF-8 safe: the prefix is built char-by-char so it never lands mid-codepoint.
pub fn truncate_at_word(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let prefix: String = s.chars().take(max_chars).collect();
    let trimmed = match prefix.rfind(char::is_whitespace) {
        Some(boundary) => prefix[..boundary].trim_end(),
        None => &prefix,
    };
    format!("{trimmed}…")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_short_string_unchanged() {
        assert_eq!(truncate_at_word("hello world", 20), "hello world");
    }

    #[test]
    fn truncates_at_word_boundary() {
        let s = "one two three four five";
        let result = truncate_at_word(s, 12);
        assert!(result.ends_with('…'), "expected ellipsis, got: {result}");
        assert!(
            result.chars().count() <= 13,
            "too long: {} chars",
            result.chars().count()
        );
        assert!(result.starts_with("one two"), "got: {result}");
    }

    #[test]
    fn hard_cuts_when_no_word_boundary() {
        let s = "abcdefghijklmnopqrstuvwxyz";
        let result = truncate_at_word(s, 10);
        assert!(result.ends_with('…'), "expected ellipsis, got: {result}");
        assert_eq!(result.chars().count(), 11); // 10 chars + ellipsis
    }

    #[test]
    fn utf8_safe() {
        // "你好世界" = 4 chars; should not truncate at 4
        let s = "你好世界 hello";
        let result = truncate_at_word(s, 5);
        // 5 chars = "你好世界 ", but boundary is at the space before "hello"
        assert!(result.ends_with('…') || result == "你好世界 hello");
    }

    #[test]
    fn exact_length_no_truncation() {
        let s = "abc";
        assert_eq!(truncate_at_word(s, 3), "abc");
    }
}
