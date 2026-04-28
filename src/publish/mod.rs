//! Privacy-split publisher: writes the 50-line handoff payload to two
//! locations under owner-only permissions, gitignores the project copy,
//! and stamps a static pointer block into AGENTS.md + CLAUDE.md.
//!
//! Privacy split rationale: the handoff content (which can contain pasted
//! secrets, file paths, error messages) lives only in `.carryover/` which
//! is gitignored. AGENTS.md and CLAUDE.md (which DO get committed) carry
//! only a fixed pointer text — no task content — so committing them is
//! safe.
//!
//! ORDER IS LOAD-BEARING: `.gitignore` is updated BEFORE any handoff
//! write so there is no window where `.carryover/handoff.md` exists as
//! an untracked file before git is told to ignore it. A concurrent
//! `git add -A` running between the two operations would otherwise
//! capture the handoff into a commit.

mod gitignore;
mod handoff;
mod pointer;
mod write_atomic;

pub use handoff::{render_handoff, Distilled, MAX_HANDOFF_LINES};
pub use pointer::{
    ensure_pointer_block, remove_pointer_block, POINTER_BLOCK, POINTER_END, POINTER_START,
};

use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PublishError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("project directory does not exist: {0}")]
    ProjectDirMissing(PathBuf),
    #[error("path traversal attempt: {0}")]
    PathTraversal(PathBuf),
}

#[derive(Clone, Debug)]
pub struct PublishContext {
    pub home_dir: PathBuf,
    pub project_dir: PathBuf,
    /// Resume mode for the protocol header. v0.1 only `ask` is honored;
    /// other values are accepted but treated as `ask`.
    pub resume_mode: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishOutcome {
    pub user_handoff: PathBuf,
    pub project_handoff: PathBuf,
    pub gitignore_modified: bool,
    pub agents_md_modified: bool,
    pub claude_md_modified: bool,
}

/// Public entry point. Each individual file write uses tempfile-then-
/// rename so a crash mid-write leaves the destination unchanged.
pub fn publish(
    distilled: &Distilled,
    ctx: &PublishContext,
) -> Result<PublishOutcome, PublishError> {
    // Path validation — lexical guard first, then existence check, then
    // canonicalize and re-check so a symlink with `..` segments cannot
    // route writes outside the intended project directory.
    if path_has_parent_component(&ctx.project_dir) {
        return Err(PublishError::PathTraversal(ctx.project_dir.clone()));
    }
    if !ctx.project_dir.exists() {
        return Err(PublishError::ProjectDirMissing(ctx.project_dir.clone()));
    }
    let canonical_project = ctx
        .project_dir
        .canonicalize()
        .map_err(|_| PublishError::ProjectDirMissing(ctx.project_dir.clone()))?;
    if path_has_parent_component(&canonical_project) {
        return Err(PublishError::PathTraversal(ctx.project_dir.clone()));
    }

    // Render the handoff body once so both writes are guaranteed identical.
    let body = handoff::render_handoff(distilled, &ctx.resume_mode);

    // STEP 1 (FIRST). Update .gitignore so the project-side handoff.md
    // never exists as an untracked file before git is told to ignore it.
    let gitignore_modified = gitignore::ensure_carryover_ignored(&canonical_project)?;

    // STEP 2. Create both .carryover/ dirs with mode 0o700 on unix.
    let user_carryover_dir = ctx.home_dir.join(".carryover");
    let user_handoff = user_carryover_dir.join("handoff.md");
    let project_carryover_dir = canonical_project.join(".carryover");
    let project_handoff = project_carryover_dir.join("handoff.md");

    create_owner_only_dir(&user_carryover_dir)?;
    create_owner_only_dir(&project_carryover_dir)?;

    // STEP 3. Write both handoffs (0o600, atomic, symlink-rejected).
    write_atomic::write_owner_only(&user_handoff, body.as_bytes())?;
    write_atomic::write_owner_only(&project_handoff, body.as_bytes())?;

    // Dual-write integrity: read both back and assert byte-identity.
    // assert! (not debug_assert!) so a release-build regression cannot
    // silently emit divergent bytes.
    let user_bytes = std::fs::read(&user_handoff)?;
    let project_bytes = std::fs::read(&project_handoff)?;
    assert_eq!(user_bytes, project_bytes, "dual-write divergence");

    // STEP 4. Pointer block in AGENTS.md and CLAUDE.md (byte-identical,
    // atomic, symlink-rejected).
    let agents_md = canonical_project.join("AGENTS.md");
    let claude_md = canonical_project.join("CLAUDE.md");
    let agents_md_modified = pointer::ensure_pointer_block(&agents_md)?;
    let claude_md_modified = pointer::ensure_pointer_block(&claude_md)?;

    Ok(PublishOutcome {
        user_handoff,
        project_handoff,
        gitignore_modified,
        agents_md_modified,
        claude_md_modified,
    })
}

#[cfg(unix)]
fn create_owner_only_dir(p: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(p)
}
#[cfg(not(unix))]
fn create_owner_only_dir(p: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(p)
}

fn path_has_parent_component(p: &Path) -> bool {
    p.components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn sample_distilled() -> Distilled {
        Distilled {
            source_tool: "claude".to_string(),
            session_id: "sess-1".to_string(),
            timestamp_iso: "2026-04-28T00:00:00Z".to_string(),
            task: "Build the publisher".to_string(),
            open_questions: vec!["What about windows?".to_string()],
            next_action: "Write tests".to_string(),
            recent_files: vec!["src/publish/mod.rs".to_string()],
            failed_approaches: vec![],
            git_context: "branch publisher / clean".to_string(),
        }
    }

    fn ctx_in(dir: &Path) -> PublishContext {
        PublishContext {
            home_dir: dir.to_path_buf(),
            project_dir: dir.to_path_buf(),
            resume_mode: "ask".to_string(),
        }
    }

    #[test]
    fn publish_writes_both_handoffs_byte_identical() {
        let dir = tempfile::tempdir().unwrap();
        let outcome = publish(&sample_distilled(), &ctx_in(dir.path())).unwrap();
        let user_bytes = fs::read(&outcome.user_handoff).unwrap();
        let project_bytes = fs::read(&outcome.project_handoff).unwrap();
        assert_eq!(user_bytes, project_bytes);
    }

    #[test]
    fn publish_appends_carryover_to_gitignore() {
        let dir = tempfile::tempdir().unwrap();
        publish(&sample_distilled(), &ctx_in(dir.path())).unwrap();
        let gi = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert!(gi.contains(".carryover/"));
    }

    #[test]
    fn publish_stamps_pointer_in_both_md_files() {
        let dir = tempfile::tempdir().unwrap();
        publish(&sample_distilled(), &ctx_in(dir.path())).unwrap();
        let agents = fs::read_to_string(dir.path().join("AGENTS.md")).unwrap();
        let claude = fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
        assert!(agents.contains(POINTER_START));
        assert!(claude.contains(POINTER_START));
    }

    #[test]
    fn publish_idempotent_second_call_no_changes_to_md() {
        let dir = tempfile::tempdir().unwrap();
        publish(&sample_distilled(), &ctx_in(dir.path())).unwrap();
        let second = publish(&sample_distilled(), &ctx_in(dir.path())).unwrap();
        assert!(!second.agents_md_modified);
        assert!(!second.claude_md_modified);
        assert!(!second.gitignore_modified);
    }

    #[cfg(unix)]
    #[test]
    fn publish_creates_carryover_dirs_with_0700_on_unix() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        publish(&sample_distilled(), &ctx_in(dir.path())).unwrap();
        let mode = fs::metadata(dir.path().join(".carryover"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    #[cfg(unix)]
    #[test]
    fn publish_handoff_files_have_0600_on_unix() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let outcome = publish(&sample_distilled(), &ctx_in(dir.path())).unwrap();
        for f in [&outcome.user_handoff, &outcome.project_handoff] {
            let mode = fs::metadata(f).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "expected 0o600 on {f:?}");
        }
    }

    #[test]
    fn publish_rejects_path_traversal_in_project_dir() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("..").join("evil");
        let ctx = PublishContext {
            home_dir: dir.path().to_path_buf(),
            project_dir: bad,
            resume_mode: "ask".to_string(),
        };
        let err = publish(&sample_distilled(), &ctx).unwrap_err();
        assert!(
            matches!(
                err,
                PublishError::PathTraversal(_) | PublishError::ProjectDirMissing(_)
            ),
            "expected PathTraversal or ProjectDirMissing, got {err:?}"
        );
    }

    #[test]
    fn publish_rejects_nonexistent_project_dir() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = PublishContext {
            home_dir: dir.path().to_path_buf(),
            project_dir: dir.path().join("does-not-exist"),
            resume_mode: "ask".to_string(),
        };
        let err = publish(&sample_distilled(), &ctx).unwrap_err();
        assert!(matches!(err, PublishError::ProjectDirMissing(_)));
    }

    #[test]
    fn gitignore_is_written_before_handoff_directory_exists() {
        // We can't directly observe ordering with a single-threaded
        // test, but we can verify that on a fresh project dir, the
        // .gitignore exists alongside the .carryover/ handoff after
        // a successful publish.
        let dir = tempfile::tempdir().unwrap();
        publish(&sample_distilled(), &ctx_in(dir.path())).unwrap();
        let gitignore = dir.path().join(".gitignore");
        let project_handoff = dir.path().join(".carryover").join("handoff.md");
        assert!(gitignore.exists());
        assert!(project_handoff.exists());
        let gi = fs::read_to_string(&gitignore).unwrap();
        assert!(gi.contains(".carryover/"));
    }
}
