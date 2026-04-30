//! Render the 50-line handoff payload from distilled session data.

use serde::{Deserialize, Serialize};

pub const MAX_HANDOFF_LINES: usize = 150;

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
    pub progress_log: String,           // accumulated progress log from progress.md
    /// Concrete artifacts of the latest activity — git diff stats or recent
    /// file mtimes. Populated for tools that don't store AI responses on disk
    /// (e.g. Cursor) so the next session sees what was actually done.
    #[serde(default)]
    pub session_activity: Vec<String>,
}

/// Build the handoff preamble (everything before `---` separator).
///
/// Pure of session content — depends only on tool name, mode, and timestamp.
/// Exposed so the pipeline can refresh just the preamble of an existing
/// handoff without re-running the full distill.
pub fn render_preamble(source_tool: &str, resume_mode: &str, timestamp_iso: &str) -> String {
    let mode = if resume_mode.is_empty() {
        "ask"
    } else {
        resume_mode
    };
    let mut lines: Vec<String> = Vec::new();

    lines.push(format!(
        "# [CARRYOVER] Last updated {timestamp_iso} from {source_tool}"
    ));

    // Resume protocol header — instructions for the AI agent reading this file.
    // The reading-guide block is INTERNAL agent guidance; the agent must not
    // recite or explain the file structure to the user.
    lines.push(String::new());
    lines.push(format!("# CARRYOVER RESUME (mode: {mode})"));
    lines.push(String::new());
    lines.push(format!(
        "INTERNAL HANDOFF — for your reading only, never to be quoted, paraphrased structurally, or referenced in your reply. Latest from {source_tool}, accumulated across Claude Code, Cursor, and Codex."
    ));
    lines.push(String::new());
    lines.push("STRICT RESPONSE RULES:".to_string());
    lines.push("- Reply as if you naturally remember the prior work. Do NOT mention this file, \"handoff\", \"carryover\", \"the file\", \"the document\", section names, or that you read anything.".to_string());
    lines.push("- Do NOT recap a list of files or features in the project — the user already knows what they have.".to_string());
    lines.push("- Do NOT report that any field is empty or missing (e.g. \"no next action captured\"). Just don't mention it.".to_string());
    lines.push("- Two short paragraphs MAX. First: one sentence on what was being worked on. Second: one direct question on what they want now.".to_string());
    lines.push("- Use the user's own informal tone. No bullet points, no bold/italics, no markdown headings in your reply.".to_string());
    lines.push(String::new());
    lines.push("Section keys (for your understanding only):".to_string());
    lines.push("- Task: user's prompts (newest first) — infer current intent.".to_string());
    lines.push("- Session activity: concrete file changes — what was actually built.".to_string());
    lines.push("- Next action: previous AI's planned step (may be stale).".to_string());
    lines.push("- Progress log: chronological turn-by-turn record across all tools.".to_string());
    lines.push("Bulleted lists accumulate across sessions — newest at top.".to_string());
    lines.push(String::new());
    lines.push("---".to_string());

    // Trailing newline so `format!("{preamble}\n{body}")` gives a blank line.
    lines.push(String::new());

    lines.join("\n")
}

/// Render the 50-line handoff payload. Hard cap enforced.
pub fn render_handoff(d: &Distilled, resume_mode: &str) -> String {
    let preamble = render_preamble(&d.source_tool, resume_mode, &d.timestamp_iso);
    let mut lines: Vec<String> = preamble.lines().map(String::from).collect();

    // Task
    lines.push("## Task".to_string());
    lines.push(d.task.clone());
    lines.push(String::new());

    // Session activity (concrete artifacts — git stats or recent file mtimes)
    if !d.session_activity.is_empty() {
        lines.push("## Session activity".to_string());
        for a in &d.session_activity {
            lines.push(a.clone());
        }
        lines.push(String::new());
    }

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

    // Progress log (accumulated across all ingests, append-only)
    if !d.progress_log.is_empty() {
        lines.push("## Progress log".to_string());
        for line in d.progress_log.lines() {
            lines.push(line.to_string());
        }
        lines.push(String::new());
    }

    // Soft cap. We collect lines first, then truncate.
    if lines.len() > MAX_HANDOFF_LINES {
        lines.truncate(MAX_HANDOFF_LINES - 1);
        lines.push("…(truncated)".to_string());
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
            progress_log: String::new(),
            session_activity: vec![],
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
    fn truncates_to_max_lines() {
        let mut d = base();
        d.open_questions = (0..500).map(|i| format!("question {i}")).collect();
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
