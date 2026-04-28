//! Pointer block stamped into AGENTS.md and CLAUDE.md.
//!
//! The block contains ONLY fixed structural text plus the absolute path to
//! the handoff file — no task content, no transcript fragments, no session ids.
//! The handoff path is the only runtime value: it varies by user home dir but
//! is never project-specific data.

use std::fs;
use std::path::Path;

use super::write_atomic;

pub const POINTER_START: &str = "<!--CARRYOVER:START-->";
pub const POINTER_END: &str = "<!--CARRYOVER:END-->";

/// Build the pointer block for a given handoff path.
/// The path is embedded as an absolute path so the block works when
/// stamped into home-level AGENTS.md / CLAUDE.md (global instructions)
/// where a relative `.carryover/handoff.md` would resolve to the wrong
/// project directory.
pub fn pointer_block(handoff_path: &Path) -> String {
    let path_str = handoff_path.display();
    format!(
        "<!--CARRYOVER:START-->\n\
         Carryover is active. Before responding:\n\
         1. Read `{path_str}` for the prior session summary.\n\
         2. Summarize it back to the user in 1-2 sentences.\n\
         3. Ask what they want to do next — do not assume continuation.\n\
         <!--CARRYOVER:END-->"
    )
}

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
    // Derive handoff path: same dir as path's parent's parent + .carryover/handoff.md,
    // but for global (home-level) installs use ~/.carryover/handoff.md.
    // Simplest v0.1 approach: resolve home dir and use absolute path.
    let handoff_path = dirs::home_dir()
        .map(|h| h.join(".carryover").join("handoff.md"))
        .unwrap_or_else(|| Path::new(".carryover/handoff.md").to_path_buf());
    ensure_pointer_block_with_path(path, &handoff_path)
}

/// Insert the pointer block using a relative `.carryover/handoff.md` path.
/// Use this for project-level AGENTS.md / CLAUDE.md that live inside the repo;
/// the relative path resolves correctly from any checkout location.
pub fn ensure_pointer_block_relative(path: &Path) -> std::io::Result<bool> {
    ensure_pointer_block_with_path(path, Path::new(".carryover/handoff.md"))
}

/// Like `ensure_pointer_block` but with an explicit handoff path (used in tests
/// and by the per-project publish path in future versions).
pub fn ensure_pointer_block_with_path(path: &Path, handoff_path: &Path) -> std::io::Result<bool> {
    let existing = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };

    let block = pointer_block(handoff_path);
    let new_content = build_new_content(&existing, &block);
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

fn build_new_content(existing: &str, block: &str) -> String {
    if let Some((start_byte, end_byte)) = find_block_bounds(existing) {
        let before = &existing[..start_byte];
        let after = &existing[end_byte..];
        return format!("{before}{block}{after}");
    }

    if existing.is_empty() {
        format!("{block}\n")
    } else if existing.ends_with("\n\n") {
        format!("{existing}{block}\n")
    } else if existing.ends_with('\n') {
        format!("{existing}\n{block}\n")
    } else {
        format!("{existing}\n\n{block}\n")
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

    fn test_handoff() -> std::path::PathBuf {
        Path::new("/home/testuser/.carryover/handoff.md").to_path_buf()
    }

    fn ep(p: &Path) -> std::io::Result<bool> {
        ensure_pointer_block_with_path(p, &test_handoff())
    }

    fn expected_block() -> String {
        pointer_block(&test_handoff())
    }

    #[test]
    fn inserts_block_in_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("AGENTS.md");
        let modified = ep(&p).unwrap();
        assert!(modified);
        let body = fs::read_to_string(&p).unwrap();
        assert!(body.contains(POINTER_START));
        assert!(body.contains(POINTER_END));
        assert!(body.contains("/home/testuser/.carryover/handoff.md"));
    }

    #[test]
    fn replaces_existing_block() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("AGENTS.md");
        let stale = format!("# Project\n\n{POINTER_START}\nold body\n{POINTER_END}\nfooter\n");
        fs::write(&p, &stale).unwrap();
        let modified = ep(&p).unwrap();
        assert!(modified);
        let body = fs::read_to_string(&p).unwrap();
        assert!(body.starts_with("# Project\n"));
        assert!(body.contains(&expected_block()));
        assert!(!body.contains("old body"));
        assert!(body.ends_with("footer\n"));
    }

    #[test]
    fn idempotent_returns_false_on_second_call() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("AGENTS.md");
        let first = ep(&p).unwrap();
        assert!(first);
        let second = ep(&p).unwrap();
        assert!(!second, "second call should be a no-op");
    }

    #[test]
    fn appends_to_file_with_unrelated_content() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("AGENTS.md");
        fs::write(&p, "# Project\n\nSome description.\n").unwrap();
        let modified = ep(&p).unwrap();
        assert!(modified);
        let body = fs::read_to_string(&p).unwrap();
        assert!(body.starts_with("# Project\n"));
        assert!(body.contains(&expected_block()));
    }

    #[test]
    fn agents_md_and_claude_md_get_byte_identical_blocks() {
        let dir = tempfile::tempdir().unwrap();
        let agents = dir.path().join("AGENTS.md");
        let claude = dir.path().join("CLAUDE.md");
        ep(&agents).unwrap();
        ep(&claude).unwrap();
        let a = fs::read_to_string(&agents).unwrap();
        let c = fs::read_to_string(&claude).unwrap();

        let extract = |s: &str| -> String {
            let start = s.find(POINTER_START).unwrap();
            let end = s.find(POINTER_END).unwrap() + POINTER_END.len();
            s[start..end].to_string()
        };
        assert_eq!(extract(&a), extract(&c));
        assert_eq!(extract(&a), expected_block());
    }

    #[test]
    fn fenced_marker_does_not_replace_documentation() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("AGENTS.md");
        let docs = "# Carryover docs\n\nMarkers look like:\n\n```\nsome other text\n```\n";
        fs::write(&p, docs).unwrap();
        ep(&p).unwrap();
        let after_first = fs::read_to_string(&p).unwrap();
        let modified_again = ep(&p).unwrap();
        assert!(!modified_again, "second call must be a no-op");
        let after_second = fs::read_to_string(&p).unwrap();
        assert_eq!(after_first, after_second);
        assert!(after_second.contains(&expected_block()));
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
