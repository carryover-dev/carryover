//! Render the 50-line handoff payload from distilled session data.

use serde::{Deserialize, Serialize};

pub const MAX_HANDOFF_LINES: usize = 50;

/// All extractor outputs assembled into one input for the publisher.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Distilled {
    pub source_tool: String, // e.g. "claude"
    pub session_id: String,
    pub timestamp_iso: String, // for the title line
    pub task: String,
    pub open_questions: Vec<String>,
    pub next_action: String,
    pub recent_files: Vec<String>,      // empty for non-coding sessions
    pub failed_approaches: Vec<String>, // empty for non-coding sessions
    pub git_context: String,            // sentinel for non-git
}

/// Render the 50-line handoff payload. Hard cap enforced.
pub fn render_handoff(d: &Distilled, resume_mode: &str) -> String {
    let mode = if resume_mode.is_empty() {
        "ask"
    } else {
        resume_mode
    };
    let mut lines: Vec<String> = Vec::new();

    // Title line: "# [CARRYOVER] Last updated <ISO> from <tool>"
    lines.push(format!(
        "# [CARRYOVER] Last updated {} from {}",
        d.timestamp_iso, d.source_tool
    ));

    // Resume protocol header — verbose template per the locked decision.
    lines.push(String::new());
    lines.push(format!("# CARRYOVER RESUME (mode: {mode})"));
    lines.push(String::new());
    lines.push(format!(
        "This file contains a 50-line summary of your prior session in {}. Before acting:",
        d.source_tool
    ));
    lines.push("1. Summarize the carryover content back to the user in 1-2 sentences.".to_string());
    lines.push("2. Ask the user what they want to do next.".to_string());
    lines.push("3. Do not assume continuation — wait for confirmation.".to_string());
    lines.push(String::new());
    lines.push("---".to_string());
    lines.push(String::new());

    // Task
    lines.push("## Task".to_string());
    lines.push(d.task.clone());
    lines.push(String::new());

    // Recent files (only if populated)
    if !d.recent_files.is_empty() {
        lines.push("## Recent files".to_string());
        for f in &d.recent_files {
            lines.push(format!("- {f}"));
        }
        lines.push(String::new());
    }

    // Failed approaches
    if !d.failed_approaches.is_empty() {
        lines.push("## Failed approaches".to_string());
        for fa in &d.failed_approaches {
            lines.push(format!("- {fa}"));
        }
        lines.push(String::new());
    }

    // Open questions
    if !d.open_questions.is_empty() {
        lines.push("## Open questions".to_string());
        for q in &d.open_questions {
            lines.push(format!("- {q}"));
        }
        lines.push(String::new());
    }

    // Next action
    lines.push("## Next action".to_string());
    lines.push(d.next_action.clone());
    lines.push(String::new());

    // Git context
    if !d.git_context.is_empty() && d.git_context != "<no git context>" {
        lines.push("## Git context".to_string());
        lines.push(d.git_context.clone());
        lines.push(String::new());
    }

    // Hard cap. We collect lines first, then truncate.
    if lines.len() > MAX_HANDOFF_LINES {
        lines.truncate(MAX_HANDOFF_LINES - 1);
        lines.push("…(truncated to 50 lines)".to_string());
    }

    let mut out = lines.join("\n");
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Distilled {
        Distilled {
            source_tool: "claude".to_string(),
            session_id: "sess-1".to_string(),
            timestamp_iso: "2026-04-28".to_string(),
            task: "do the thing".to_string(),
            open_questions: vec![],
            next_action: "next step".to_string(),
            recent_files: vec![],
            failed_approaches: vec![],
            git_context: "<no git context>".to_string(),
        }
    }

    #[test]
    fn renders_title_line_with_tool_and_timestamp() {
        let out = render_handoff(&base(), "ask");
        assert!(
            out.starts_with("# [CARRYOVER] Last updated 2026-04-28 from claude"),
            "title mismatch: {out}"
        );
    }

    #[test]
    fn renders_resume_protocol_header_with_mode() {
        let out = render_handoff(&base(), "ask");
        assert!(out.contains("CARRYOVER RESUME (mode: ask)"));
    }

    #[test]
    fn truncates_to_50_lines() {
        let mut d = base();
        d.open_questions = (0..200).map(|i| format!("question {i}")).collect();
        let out = render_handoff(&d, "ask");
        let line_count = out.lines().count();
        assert!(
            line_count <= MAX_HANDOFF_LINES,
            "expected ≤{MAX_HANDOFF_LINES} lines, got {line_count}"
        );
    }

    #[test]
    fn non_coding_session_omits_recent_files() {
        let out = render_handoff(&base(), "ask");
        assert!(!out.contains("## Recent files"));
    }

    #[test]
    fn git_context_sentinel_is_omitted() {
        let out = render_handoff(&base(), "ask");
        assert!(!out.contains("## Git context"));
    }

    #[test]
    fn output_ends_with_newline() {
        let out = render_handoff(&base(), "ask");
        assert!(out.ends_with('\n'));
    }

    #[test]
    fn empty_resume_mode_defaults_to_ask() {
        let out = render_handoff(&base(), "");
        assert!(out.contains("CARRYOVER RESUME (mode: ask)"));
    }
}
