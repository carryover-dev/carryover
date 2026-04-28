//! Atomic write helpers: tempfile-then-rename with optional owner-only
//! permissions and explicit symlink rejection.
//!
//! Two flavors:
//! - `write_owner_only(path, bytes)` — for sensitive files (handoff.md).
//!   Creates the tempfile with mode 0o600 *at creation* on unix so there
//!   is no TOCTOU window where the destination exists with the process
//!   umask before chmod is applied.
//! - `write_no_follow(path, bytes)` — for committable files (AGENTS.md,
//!   CLAUDE.md, .gitignore). Default permissions, but rejects an
//!   existing symlink at `path` so an attacker cannot turn the publisher
//!   into an arbitrary-write primitive by symlinking the target.

use std::io::Write;
use std::path::Path;

/// Atomically write `bytes` to `path` with mode 0o600 on unix.
///
/// On unix the tempfile is opened with O_CREAT and mode 0o600 *before*
/// any data is written, so the file never exists with a wider mode.
/// Then we sync, rename, and additionally re-chmod the destination
/// (defense-in-depth in case any platform-specific rename behavior
/// preserved the source mode).
pub fn write_owner_only(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    reject_symlink(path)?;
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "no parent dir"))?;

    #[cfg(unix)]
    let mut tmp = {
        use std::os::unix::fs::PermissionsExt;
        tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o600))
            .tempfile_in(dir)?
    };
    #[cfg(not(unix))]
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;

    tmp.write_all(bytes)?;
    tmp.as_file_mut().sync_all()?;
    tmp.persist(path).map_err(|e| e.error)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Atomically write `bytes` to `path` without forcing 0o600. Used for
/// committable files (AGENTS.md, CLAUDE.md, .gitignore) which are meant
/// to be readable. Still rejects an existing symlink at `path` so the
/// publisher cannot be turned into an arbitrary-write primitive.
pub fn write_no_follow(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    reject_symlink(path)?;
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "no parent dir"))?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(bytes)?;
    tmp.as_file_mut().sync_all()?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

/// Reject if `path` exists and is a symlink. We use `symlink_metadata`
/// so we look at the link itself, not its target. Non-existent paths
/// are fine (they will be created).
fn reject_symlink(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!("refusing to follow symlink at {}", path.display()),
                ))
            } else {
                Ok(())
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[cfg(unix)]
    #[test]
    fn write_owner_only_sets_0600_on_unix() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("h.md");
        write_owner_only(&p, b"hello").unwrap();
        let mode = fs::metadata(&p).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(fs::read(&p).unwrap(), b"hello");
    }

    #[test]
    fn no_tempfile_leftovers_after_write() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("h.md");
        write_owner_only(&p, b"hello").unwrap();
        let mut entries: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        entries.sort();
        assert_eq!(entries, vec!["h.md".to_string()]);
    }

    #[test]
    fn overwrites_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("h.md");
        fs::write(&p, b"old").unwrap();
        write_owner_only(&p, b"new").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"new");
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_target() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let real_target = dir.path().join("real.txt");
        let link_path = dir.path().join("link.md");
        fs::write(&real_target, b"existing").unwrap();
        symlink(&real_target, &link_path).unwrap();

        let err =
            write_owner_only(&link_path, b"new").expect_err("symlink target must be rejected");
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);

        // The symlink target file is unchanged — proof we did not follow.
        assert_eq!(fs::read(&real_target).unwrap(), b"existing");
    }

    #[test]
    fn write_no_follow_atomic_overwrites_committable_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("AGENTS.md");
        fs::write(&p, b"# old").unwrap();
        write_no_follow(&p, b"# new").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"# new");
    }

    #[cfg(unix)]
    #[test]
    fn write_no_follow_rejects_symlinked_target() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let real_target = dir.path().join("real.md");
        let link_path = dir.path().join("AGENTS.md");
        fs::write(&real_target, b"original").unwrap();
        symlink(&real_target, &link_path).unwrap();

        let err = write_no_follow(&link_path, b"hijacked")
            .expect_err("symlinked AGENTS.md must be rejected");
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
        assert_eq!(fs::read(&real_target).unwrap(), b"original");
    }
}
