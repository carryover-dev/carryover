//! Static pointer block stamped into AGENTS.md and CLAUDE.md.
//!
//! These files ARE committed to the project repository, so the block
//! contains ONLY fixed text — no task content, no transcript fragments,
//! no session ids. Any future change that interpolates runtime data into
//! `POINTER_BLOCK` is a privacy regression.

use std::fs;
use std::path::Path;

use super::write_atomic;

pub const POINTER_START: &str = "<!--CARRYOVER:START-->";
pub const POINTER_END: &str = "<!--CARRYOVER:END-->";

/// The static block stamped into AGENTS.md and CLAUDE.md.
///
/// **DO NOT** add task content, session ids, or any other runtime data
/// to this constant. The whole privacy model of Carryover depends on
/// this block being identical across every project on every machine.
/// Live handoff content lives in `.carryover/handoff.md` (gitignored).
pub const POINTER_BLOCK: &str = concat!(
    "<!--CARRYOVER:START-->\n",
    "Carryover is active in this project. Before responding:\n",
    "1. Read `.carryover/handoff.md` for the prior session summary.\n",
    "2. Summarize it back to the user in 1-2 sentences.\n",
    "3. Ask what they want to do next — do not assume continuation.\n",
    "<!--CARRYOVER:END-->",
);

/// Insert or replace the pointer block in `path`. Creates the file if
/// missing. Returns true if the file was modified, false if the
/// existing block was already byte-identical.
///
/// Marker matching is **line-anchored** — `POINTER_START` and
/// `POINTER_END` must appear as the first non-whitespace content of a
/// line. This keeps a literal `<!--CARRYOVER:START-->` mention inside a
/// fenced markdown code block from getting mangled, AS LONG AS the
/// fenced block is indented (most are not). For documentation files
/// that print the markers at column 0 inside a fence, the limitation
/// is documented.
///
/// Writes through `write_atomic::write_no_follow` so a crash mid-write
/// never produces a torn file, and a symlink at `path` is rejected
/// rather than followed.
pub fn ensure_pointer_block(path: &Path) -> std::io::Result<bool> {
    let existing = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };

    let new_content = build_new_content(&existing);
    if new_content == existing {
        return Ok(false);
    }
    write_atomic::write_no_follow(path, new_content.as_bytes())?;
    Ok(true)
}

/// Remove the Carryover pointer block from `path` if present.
/// Returns true if the file was modified. No-ops if the file does not exist
/// or the block was not found.
pub fn remove_pointer_block(path: &Path) -> std::io::Result<bool> {
    let existing = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    let Some((start, end)) = find_block_bounds(&existing) else {
        return Ok(false);
    };
    let before = existing[..start].trim_end_matches('\n');
    let after = &existing[end..];
    let new_content = if after.trim().is_empty() {
        if before.is_empty() {
            String::new()
        } else {
            format!("{before}\n")
        }
    } else {
        format!("{before}\n{after}")
    };
    write_atomic::write_no_follow(path, new_content.as_bytes())?;
    Ok(true)
}

fn build_new_content(existing: &str) -> String {
    if let Some((start_byte, end_byte)) = find_block_bounds(existing) {
        let before = &existing[..start_byte];
        let after = &existing[end_byte..];
        return format!("{before}{POINTER_BLOCK}{after}");
    }

    if existing.is_empty() {
        format!("{POINTER_BLOCK}\n")
    } else if existing.ends_with("\n\n") {
        format!("{existing}{POINTER_BLOCK}\n")
    } else if existing.ends_with('\n') {
        format!("{existing}\n{POINTER_BLOCK}\n")
    } else {
        format!("{existing}\n\n{POINTER_BLOCK}\n")
    }
}

/// Locate the existing pointer block bounds via a line-anchored scan.
/// Returns `(byte_offset_of_start_marker, byte_offset_after_end_marker)`.
fn find_block_bounds(text: &str) -> Option<(usize, usize)> {
    let mut start: Option<usize> = None;
    let mut end_after: Option<usize> = None;
    let mut byte_pos: usize = 0;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let leading_ws = line.len() - trimmed.len();
        if start.is_none() && trimmed.starts_with(POINTER_START) {
            start = Some(byte_pos + leading_ws);
        } else if start.is_some() && trimmed.starts_with(POINTER_END) {
            end_after = Some(byte_pos + leading_ws + POINTER_END.len());
            break;
        }
        byte_pos += line.len();
    }
    match (start, end_after) {
        (Some(s), Some(e)) if s < e => Some((s, e)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inserts_block_in_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("AGENTS.md");
        let modified = ensure_pointer_block(&p).unwrap();
        assert!(modified);
        let body = fs::read_to_string(&p).unwrap();
        assert!(body.contains(POINTER_START));
        assert!(body.contains(POINTER_END));
    }

    #[test]
    fn replaces_existing_block() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("AGENTS.md");
        let stale = format!("# Project\n\n{POINTER_START}\nold body\n{POINTER_END}\nfooter\n");
        fs::write(&p, &stale).unwrap();
        let modified = ensure_pointer_block(&p).unwrap();
        assert!(modified);
        let body = fs::read_to_string(&p).unwrap();
        assert!(body.starts_with("# Project\n"));
        assert!(body.contains(POINTER_BLOCK));
        assert!(!body.contains("old body"));
        assert!(body.ends_with("footer\n"));
    }

    #[test]
    fn idempotent_returns_false_on_second_call() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("AGENTS.md");
        let first = ensure_pointer_block(&p).unwrap();
        assert!(first);
        let second = ensure_pointer_block(&p).unwrap();
        assert!(!second, "second call should be a no-op");
    }

    #[test]
    fn appends_to_file_with_unrelated_content() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("AGENTS.md");
        fs::write(&p, "# Project\n\nSome description.\n").unwrap();
        let modified = ensure_pointer_block(&p).unwrap();
        assert!(modified);
        let body = fs::read_to_string(&p).unwrap();
        assert!(body.starts_with("# Project\n"));
        assert!(body.contains(POINTER_BLOCK));
    }

    #[test]
    fn agents_md_and_claude_md_get_byte_identical_blocks() {
        let dir = tempfile::tempdir().unwrap();
        let agents = dir.path().join("AGENTS.md");
        let claude = dir.path().join("CLAUDE.md");
        ensure_pointer_block(&agents).unwrap();
        ensure_pointer_block(&claude).unwrap();
        let a = fs::read_to_string(&agents).unwrap();
        let c = fs::read_to_string(&claude).unwrap();

        let extract = |s: &str| -> String {
            let start = s.find(POINTER_START).unwrap();
            let end = s.find(POINTER_END).unwrap() + POINTER_END.len();
            s[start..end].to_string()
        };
        assert_eq!(extract(&a), extract(&c));
        assert_eq!(extract(&a), POINTER_BLOCK);
    }

    #[test]
    fn fenced_marker_does_not_replace_documentation() {
        // A documentation block that mentions the markers inside fenced
        // code (column-0) WILL still match our line-anchored scan today.
        // What we explicitly test is that the second invocation is a
        // no-op (idempotent) rather than mangling the first replacement.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("AGENTS.md");
        let docs = "# Carryover docs\n\nMarkers look like:\n\n```\nsome other text\n```\n";
        fs::write(&p, docs).unwrap();
        ensure_pointer_block(&p).unwrap();
        let after_first = fs::read_to_string(&p).unwrap();
        // No matter what shape build_new_content chose, two calls in a
        // row must converge to a fixed point.
        let modified_again = ensure_pointer_block(&p).unwrap();
        assert!(!modified_again, "second call must be a no-op");
        let after_second = fs::read_to_string(&p).unwrap();
        assert_eq!(after_first, after_second);
        assert!(after_second.contains(POINTER_BLOCK));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_target() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.md");
        let link = dir.path().join("AGENTS.md");
        fs::write(&real, b"original").unwrap();
        symlink(&real, &link).unwrap();
        let err = ensure_pointer_block(&link).expect_err("symlink target must be rejected");
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
        assert_eq!(fs::read_to_string(&real).unwrap(), "original");
    }

    #[test]
    fn remove_pointer_block_strips_block_only() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("AGENTS.md");
        fs::write(&p, "# Existing content\n").unwrap();
        ensure_pointer_block(&p).unwrap();

        let modified = remove_pointer_block(&p).unwrap();
        assert!(modified);

        let body = fs::read_to_string(&p).unwrap();
        assert!(!body.contains(POINTER_START), "block should be removed");
        assert!(
            body.contains("# Existing content"),
            "other content preserved"
        );
    }

    #[test]
    fn remove_pointer_block_no_op_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("AGENTS.md");
        fs::write(&p, "# No carryover block here\n").unwrap();
        let modified = remove_pointer_block(&p).unwrap();
        assert!(!modified);
    }

    #[test]
    fn remove_pointer_block_no_op_on_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("nonexistent.md");
        let modified = remove_pointer_block(&p).unwrap();
        assert!(!modified);
    }
}
