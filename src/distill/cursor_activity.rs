//! `cursor_activity` extractor — captures concrete artifacts of an AI session
//! (file edits, line deltas) so the handoff has a record of *what was done*
//! without depending on the AI's response text being stored locally.
//!
//! Two strategies:
//! 1. **Git project**: run `git diff --stat HEAD` to get per-file line deltas.
//! 2. **Non-git project**: list files modified within a recent window via
//!    filesystem mtimes.
//!
//! Activates only for the source tool that needs it — Cursor — since Cursor
//! does not persist AI responses on disk. Other adapters get response text
//! directly from their transcripts.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, SystemTime};

pub const MAX_ACTIVITY_FILES: usize = 15;
const RECENT_WINDOW_SECS: u64 = 6 * 3600; // 6 hours
const MAX_TRAVERSAL_DEPTH: usize = 6;
const MAX_FILES_SCANNED: usize = 5000;

const IGNORE_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    ".next",
    ".nuxt",
    "vendor",
    ".venv",
    "venv",
    "__pycache__",
    ".cache",
    ".carryover",
    ".omc",
    ".claude",
    ".vscode",
    ".idea",
    ".DS_Store",
];

/// Build the activity section content for the handoff.
///
/// `project_dir` is the AI session's working directory. Returns one bullet per
/// changed file (e.g. `- src/main.rs: +12 -3` for git, `- src/main.rs (15s ago)`
/// for non-git). Empty Vec = no recent activity to report.
pub fn extract_cursor_activity(project_dir: &Path) -> Vec<String> {
    if let Some(git_lines) = git_diff_summary(project_dir) {
        return git_lines;
    }
    list_recent_modified(project_dir)
}

/// Run `git diff --stat HEAD` and parse the per-file lines.
/// Returns None if not a git repo, no commits yet, or git errored.
fn git_diff_summary(project_dir: &Path) -> Option<Vec<String>> {
    if !project_dir.join(".git").exists() {
        return None;
    }

    let output = Command::new("git")
        .args(["diff", "--stat", "HEAD", "--no-color"])
        .current_dir(project_dir)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    // git diff --stat output:
    //   src/main.rs    | 12 +++++++++---
    //   src/lib.rs     |  3 +++
    //   2 files changed, 15 insertions(+), 3 deletions(-)
    let mut lines: Vec<String> = Vec::new();
    for raw in stdout.lines() {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        // The summary line ("N files changed, ...") is also useful — keep last.
        if trimmed.contains(" file") && trimmed.contains(" changed") {
            lines.push(format!("Summary: {trimmed}"));
            continue;
        }
        // Per-file lines have format: "path | N ±±±"
        if let Some((path_raw, stats)) = trimmed.split_once('|') {
            let path = path_raw.trim();
            let stats = stats.trim();
            // Extract +/- counts: "12 +++++++++---"
            let plus = stats.chars().filter(|c| *c == '+').count();
            let minus = stats.chars().filter(|c| *c == '-').count();
            let count = stats.split_whitespace().next().unwrap_or("?");
            lines.push(format!("- {path}: {count} lines (+{plus} -{minus})"));
        }
        if lines.len() >= MAX_ACTIVITY_FILES + 1 {
            break;
        }
    }

    if lines.is_empty() {
        None
    } else {
        Some(lines)
    }
}

/// Walk project_dir and collect files modified within RECENT_WINDOW_SECS.
fn list_recent_modified(project_dir: &Path) -> Vec<String> {
    let now = SystemTime::now();
    let cutoff = now
        .checked_sub(Duration::from_secs(RECENT_WINDOW_SECS))
        .unwrap_or(now);

    let mut entries: Vec<(SystemTime, String)> = Vec::new();
    let mut scanned = 0usize;
    walk(
        project_dir,
        project_dir,
        0,
        &cutoff,
        &mut entries,
        &mut scanned,
    );

    entries.sort_by(|a, b| b.0.cmp(&a.0));
    entries.truncate(MAX_ACTIVITY_FILES);

    entries
        .into_iter()
        .map(|(mtime, path)| {
            let age = now
                .duration_since(mtime)
                .map(|d| format_age(d.as_secs()))
                .unwrap_or_else(|_| "just now".to_string());
            format!("- {path} ({age})")
        })
        .collect()
}

fn walk(
    root: &Path,
    cur: &Path,
    depth: usize,
    cutoff: &SystemTime,
    out: &mut Vec<(SystemTime, String)>,
    scanned: &mut usize,
) {
    if depth > MAX_TRAVERSAL_DEPTH || *scanned >= MAX_FILES_SCANNED {
        return;
    }

    let read_dir = match std::fs::read_dir(cur) {
        Ok(rd) => rd,
        Err(_) => return,
    };

    for entry in read_dir.flatten() {
        if *scanned >= MAX_FILES_SCANNED {
            return;
        }
        *scanned += 1;

        let path = entry.path();
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };

        if IGNORE_DIRS.iter().any(|d| *d == name) {
            continue;
        }

        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };

        // Don't follow symlinks — could escape the project.
        if meta.file_type().is_symlink() {
            continue;
        }

        if meta.is_dir() {
            walk(root, &path, depth + 1, cutoff, out, scanned);
            continue;
        }

        if !meta.is_file() {
            continue;
        }

        let mtime = match meta.modified() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if mtime < *cutoff {
            continue;
        }

        let rel = match path.strip_prefix(root) {
            Ok(r) => r.to_string_lossy().into_owned(),
            Err(_) => path.to_string_lossy().into_owned(),
        };
        out.push((mtime, rel));
    }
}

fn format_age(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else {
        format!("{}h ago", secs / 3600)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn empty_project_returns_empty() {
        let dir = tempdir().unwrap();
        let result = extract_cursor_activity(dir.path());
        assert!(result.is_empty());
    }

    #[test]
    fn non_git_lists_recently_modified() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("hello.txt"), "hi").unwrap();
        fs::write(dir.path().join("world.html"), "<html></html>").unwrap();

        let result = extract_cursor_activity(dir.path());
        assert_eq!(result.len(), 2);
        assert!(result.iter().any(|l| l.contains("hello.txt")));
        assert!(result.iter().any(|l| l.contains("world.html")));
    }

    #[test]
    fn ignores_node_modules_and_git() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("node_modules/pkg")).unwrap();
        fs::write(dir.path().join("node_modules/pkg/index.js"), "noise").unwrap();
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        fs::write(dir.path().join(".git/HEAD"), "ref: ...").unwrap();
        fs::write(dir.path().join("real.txt"), "signal").unwrap();

        let result = extract_cursor_activity(dir.path());
        assert!(result.iter().any(|l| l.contains("real.txt")));
        assert!(!result.iter().any(|l| l.contains("node_modules")));
        assert!(!result.iter().any(|l| l.contains(".git")));
    }

    #[test]
    fn caps_total_files() {
        let dir = tempdir().unwrap();
        for i in 0..(MAX_ACTIVITY_FILES + 5) {
            fs::write(dir.path().join(format!("f{i}.txt")), "x").unwrap();
        }
        let result = extract_cursor_activity(dir.path());
        assert!(result.len() <= MAX_ACTIVITY_FILES);
    }
}
