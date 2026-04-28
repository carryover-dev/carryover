//! Carryover's own config file at `~/.carryover/config.json`.
//!
//! Stores user choices made at install time (which tools to wire up,
//! resume mode preference). Read at daemon startup; rewritten by
//! `carryover refresh` and `carryover install`.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Config {
    /// Tool names the user opted into (e.g. ["claude", "cursor"]).
    pub tools: Vec<String>,
    /// Resume protocol mode. Default "ask"; v0.1 only "ask" has effect.
    pub resume_mode: String,
}

impl Config {
    pub fn default_path() -> Result<PathBuf> {
        let home = dirs::home_dir().context("could not resolve home directory")?;
        Ok(home.join(".carryover").join("config.json"))
    }

    pub fn load_or_default(path: &Path) -> Result<Config> {
        match std::fs::read_to_string(path) {
            Ok(s) => serde_json::from_str(&s).context("parse config.json"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config {
                tools: Vec::new(),
                resume_mode: "ask".to_string(),
            }),
            Err(e) => Err(e).context("read config.json"),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        // Reject symlinked target. Without this an attacker who drops a
        // symlink at ~/.carryover/config.json could redirect the write.
        if let Ok(meta) = std::fs::symlink_metadata(path) {
            if meta.file_type().is_symlink() {
                return Err(anyhow::anyhow!(
                    "refusing to follow symlink at {}",
                    path.display()
                ));
            }
        }

        if let Some(parent) = path.parent() {
            create_owner_only_dir(parent).context("create config dir")?;
        }
        let json = serde_json::to_string_pretty(self).context("serialize config")?;

        // Atomic write: tempfile in same dir → fsync → rename. On unix
        // the tempfile is created with mode 0o600 *at creation time* so
        // there is no window where the destination exists at the
        // umask-default mode.
        let dir = path
            .parent()
            .context("config path has no parent directory")?;

        #[cfg(unix)]
        let mut tmp = {
            use std::os::unix::fs::PermissionsExt;
            tempfile::Builder::new()
                .permissions(std::fs::Permissions::from_mode(0o600))
                .tempfile_in(dir)
                .context("create temp config file")?
        };
        #[cfg(not(unix))]
        let mut tmp = tempfile::NamedTempFile::new_in(dir).context("create temp config file")?;

        use std::io::Write as _;
        tmp.write_all(json.as_bytes())
            .context("write temp config")?;
        tmp.as_file_mut().sync_all().context("sync temp config")?;
        tmp.persist(path)
            .map_err(|e| anyhow::anyhow!("{}", e))
            .context("persist config atomic write")?;

        // Belt-and-braces re-chmod after rename (no-op on non-unix).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                .context("set config.json permissions")?;
        }
        Ok(())
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_then_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.json");
        let cfg = Config {
            tools: vec!["claude".to_string(), "cursor".to_string()],
            resume_mode: "ask".to_string(),
        };
        cfg.save(&p).unwrap();
        let loaded = Config::load_or_default(&p).unwrap();
        assert_eq!(loaded, cfg);
    }

    #[test]
    fn load_or_default_returns_default_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("does-not-exist.json");
        let cfg = Config::load_or_default(&p).unwrap();
        assert_eq!(cfg.resume_mode, "ask");
        assert!(cfg.tools.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn save_sets_0600_on_unix() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.json");
        Config::default().save(&p).unwrap();
        let mode = std::fs::metadata(&p).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn default_resume_mode_is_ask() {
        let cfg = Config::default();
        // default() yields empty resume_mode via Default derive; load_or_default
        // explicitly sets "ask". Test that load_or_default on missing file gives "ask".
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("missing.json");
        let loaded = Config::load_or_default(&p).unwrap();
        assert_eq!(loaded.resume_mode, "ask");
        drop(cfg);
    }

    #[cfg(unix)]
    #[test]
    fn save_rejects_symlinked_target() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.json");
        let link = dir.path().join("config.json");
        std::fs::write(&real, b"{}").unwrap();
        symlink(&real, &link).unwrap();

        let err = Config::default()
            .save(&link)
            .expect_err("symlinked config target must be rejected");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("symlink"),
            "expected symlink-rejection error, got: {msg}"
        );
        // Real target is unchanged — proves we did not follow.
        assert_eq!(std::fs::read(&real).unwrap(), b"{}");
    }
}
