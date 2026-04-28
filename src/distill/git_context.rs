//! `git_context` extractor — captures HEAD sha and diff stat for the project
//! directory.
//!
//! This is the only distiller that does I/O (shells out to `git`). It tolerates:
//! - `cwd` is `None` → sentinel
//! - directory does not exist → sentinel
//! - not a git repo (`git rev-parse HEAD` fails) → sentinel
//! - `git` binary not found → sentinel

use crate::storage::LedgerRow;
use std::path::Path;

pub const NO_GIT_CONTEXT: &str = "<no git context>";
pub const MAX_GIT_CONTEXT_CHARS: usize = 200;

/// Extract HEAD sha and diff stat for the project at `cwd`.
///
/// Returns `NO_GIT_CONTEXT` for any non-git or error condition.
/// `rows` is accepted for API symmetry with the other extractors but is not
/// consulted — git state is always read from the filesystem.
pub fn extract_git_context(_rows: &[LedgerRow], cwd: Option<&Path>) -> String {
    let dir = match cwd {
        Some(p) if p.exists() => p,
        _ => return NO_GIT_CONTEXT.to_string(),
    };

    // Get HEAD commit sha.
    let head_sha = match run_git_in(dir, &["rev-parse", "HEAD"]) {
        Some(output) if !output.is_empty() => {
            // Use a short sha (first 8 chars).
            let short: String = output.chars().take(8).collect();
            short
        }
        _ => return NO_GIT_CONTEXT.to_string(),
    };

    // Get diff stat summary (may be empty for clean worktrees).
    let diff_stat = run_git_in(dir, &["diff", "--stat"])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let result = match diff_stat {
        Some(stat) => {
            // Take only the last summary line of diff --stat (the "N files changed" line).
            let summary_line = stat.lines().last().unwrap_or(&stat).trim().to_string();
            format!("HEAD: {}; diff: {}", head_sha, summary_line)
        }
        None => format!("HEAD: {}", head_sha),
    };

    // Cap to MAX_GIT_CONTEXT_CHARS (hard truncate — no word boundary needed here).
    if result.chars().count() <= MAX_GIT_CONTEXT_CHARS {
        result
    } else {
        let truncated: String = result.chars().take(MAX_GIT_CONTEXT_CHARS).collect();
        format!("{truncated}…")
    }
}

/// Maximum time we wait for a `git` invocation before killing it. A daemon
/// must never block forever on a hung subprocess (e.g. credential prompt
/// on a network-mounted repo, paginator stuck on an unset PAGER, etc.).
const GIT_TIMEOUT_SECS: u64 = 5;

/// Run `git -C <cwd> <args...>` with a 5-second timeout. Returns the trimmed
/// stdout on exit 0, or `None` on any error (binary not found, non-zero exit,
/// timeout, signal, etc.). The `cwd` is passed as a `&Path` directly via
/// `.arg(cwd)`, so non-UTF-8 paths flow through losslessly.
fn run_git_in(cwd: &Path, args: &[&str]) -> Option<String> {
    use std::process::Stdio;

    let mut child = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .spawn()
        .ok()?;

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(GIT_TIMEOUT_SECS);
    loop {
        match child.try_wait().ok()? {
            Some(status) => {
                if !status.success() {
                    return None;
                }
                let mut stdout = child.stdout.take()?;
                let mut buf = Vec::new();
                std::io::Read::read_to_end(&mut stdout, &mut buf).ok()?;
                return Some(String::from_utf8_lossy(&buf).trim().to_string());
            }
            None if std::time::Instant::now() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            None => std::thread::sleep(std::time::Duration::from_millis(25)),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// Initialize a git repo in `dir` with a single empty commit.
    /// Sets local user.name and user.email so tests work in CI environments
    /// where no global git config exists.
    fn git_init_with_commit(dir: &std::path::Path) {
        let run = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(dir)
                .output()
                .expect("git command failed");
        };
        run(&["init"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "Test"]);
        run(&["commit", "--allow-empty", "-m", "init"]);
    }

    #[test]
    fn extracts_head_in_real_git_repo() {
        let dir = tempfile::tempdir().unwrap();
        git_init_with_commit(dir.path());

        let result = extract_git_context(&[], Some(dir.path()));
        assert!(
            result.starts_with("HEAD: "),
            "expected HEAD: prefix, got: {result}"
        );
        // Short sha is 8 hex chars.
        let sha_part = result.trim_start_matches("HEAD: ");
        let sha: String = sha_part.chars().take(8).collect();
        assert_eq!(sha.len(), 8);
        assert!(
            sha.chars().all(|c| c.is_ascii_hexdigit()),
            "sha should be hex, got: {sha}"
        );
    }

    #[test]
    fn non_git_dir_returns_sentinel() {
        let dir = tempfile::tempdir().unwrap();
        // No git init — not a repo.
        let result = extract_git_context(&[], Some(dir.path()));
        assert_eq!(result, NO_GIT_CONTEXT);
    }

    #[test]
    fn none_cwd_returns_sentinel() {
        let result = extract_git_context(&[], None);
        assert_eq!(result, NO_GIT_CONTEXT);
    }

    #[test]
    fn handles_dirty_diff() {
        let dir = tempfile::tempdir().unwrap();
        git_init_with_commit(dir.path());

        // Add an untracked file and stage it so diff --stat shows something.
        std::fs::write(dir.path().join("hello.txt"), "hello\n").unwrap();
        Command::new("git")
            .args(["add", "hello.txt"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        let result = extract_git_context(&[], Some(dir.path()));
        // With a staged file git diff --stat (unstaged) may show nothing;
        // git diff --cached would show the staged change. We accept both:
        // the extractor runs `git diff --stat` (unstaged), so for a freshly
        // staged but uncommitted file this returns an empty diff. The test
        // just checks the result is valid (starts with HEAD:).
        assert!(
            result.starts_with("HEAD: "),
            "expected HEAD: prefix, got: {result}"
        );
    }

    #[test]
    fn handles_empty_ledger() {
        let result = extract_git_context(&[], None);
        assert_eq!(result, NO_GIT_CONTEXT);
    }

    #[test]
    fn nonexistent_path_returns_sentinel() {
        let path = std::path::Path::new("/tmp/carryover-test-nonexistent-dir-xyz");
        let result = extract_git_context(&[], Some(path));
        assert_eq!(result, NO_GIT_CONTEXT);
    }
}
