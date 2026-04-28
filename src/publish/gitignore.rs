//! Idempotent append of `.carryover/` to `<project>/.gitignore`.
//!
//! Writes go through `write_atomic::write_no_follow` (tempfile-then-rename)
//! so a crash mid-write never produces a truncated `.gitignore` that
//! silently un-gitignores other entries (e.g. `.env`).

use std::fs;
use std::path::Path;

use super::write_atomic;

const CARRYOVER_GITIGNORE_LINE: &str = ".carryover/";

/// Append `.carryover/` to `<project>/.gitignore` if it isn't already
/// present. Returns true if the file was modified.
///
/// Match semantics: line-based; we treat any line whose content (after
/// trim) equals `.carryover/` or `.carryover` as already covering us.
pub fn ensure_carryover_ignored(project_dir: &Path) -> std::io::Result<bool> {
    let gitignore = project_dir.join(".gitignore");
    let existing = match fs::read_to_string(&gitignore) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };

    if already_covered(&existing) {
        return Ok(false);
    }

    let separator = if existing.is_empty() || existing.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    let new = format!("{existing}{separator}{CARRYOVER_GITIGNORE_LINE}\n");

    write_atomic::write_no_follow(&gitignore, new.as_bytes())?;
    Ok(true)
}

fn already_covered(content: &str) -> bool {
    content.lines().any(|l| {
        let t = l.trim();
        t == ".carryover/" || t == ".carryover"
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_gitignore_with_carryover_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let modified = ensure_carryover_ignored(dir.path()).unwrap();
        assert!(modified);
        let gi = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert!(gi.contains(".carryover/"));
    }

    #[test]
    fn appends_to_existing_gitignore() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".gitignore"), "target/\n").unwrap();
        let modified = ensure_carryover_ignored(dir.path()).unwrap();
        assert!(modified);
        let gi = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert_eq!(gi, "target/\n.carryover/\n");
    }

    #[test]
    fn idempotent_does_not_double_append() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".gitignore"), "target/\n.carryover/\n").unwrap();
        let modified = ensure_carryover_ignored(dir.path()).unwrap();
        assert!(!modified);
        let gi = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert_eq!(gi, "target/\n.carryover/\n");
    }

    #[test]
    fn accepts_with_or_without_trailing_slash() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".gitignore"), "target/\n.carryover\n").unwrap();
        let modified = ensure_carryover_ignored(dir.path()).unwrap();
        assert!(!modified, ".carryover (no slash) should already cover us");
    }

    #[test]
    fn appends_when_file_lacks_trailing_newline() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".gitignore"), "target/").unwrap();
        let modified = ensure_carryover_ignored(dir.path()).unwrap();
        assert!(modified);
        let gi = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert_eq!(gi, "target/\n.carryover/\n");
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_gitignore() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.txt");
        let link = dir.path().join(".gitignore");
        fs::write(&real, b"target/\n").unwrap();
        symlink(&real, &link).unwrap();
        let err = ensure_carryover_ignored(dir.path()).expect_err("symlink rejected");
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
        assert_eq!(fs::read_to_string(&real).unwrap(), "target/\n");
    }
}
