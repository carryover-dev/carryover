//! Linux systemd-user unit installer for `carryoverd`.
//!
//! Writes the unit file to ~/.config/systemd/user/carryoverd.service and
//! optionally calls `systemctl --user enable --now carryoverd` /
//! `systemctl --user disable --now carryoverd` to (un)register the daemon
//! so it survives logout / login.

use anyhow::{Context, Result};
use std::path::PathBuf;
use std::process::Command;

const UNIT_FILE_NAME: &str = "carryoverd.service";

/// Resolve `~/.config/systemd/user/carryoverd.service`.
pub fn unit_file_path() -> Result<PathBuf> {
    let config = dirs::config_dir().context("could not resolve $XDG_CONFIG_HOME or ~/.config")?;
    Ok(config.join("systemd").join("user").join(UNIT_FILE_NAME))
}

/// Render the unit file body. The ExecStart path is the absolute path of
/// the currently-running carryoverd binary (resolved via std::env::current_exe).
pub fn render_unit() -> Result<String> {
    let exe = std::env::current_exe()
        .context("could not resolve carryoverd binary path for ExecStart")?;
    let exe_str = exe.to_string_lossy();
    Ok(format!(
        r#"[Unit]
Description=Carryover daemon — zero-LLM-token context-handoff
After=default.target

[Service]
Type=simple
ExecStart="{exe}" start
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
"#,
        exe = exe_str
    ))
}

/// Write the unit file to disk. Returns the path written. Atomic write
/// (tempfile + rename) so a crash mid-write leaves the prior unit intact.
pub fn write_unit_file() -> Result<PathBuf> {
    let path = unit_file_path()?;
    let body = render_unit()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }

    let dir = path.parent().context("unit file path has no parent")?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir).context("create temp unit file")?;
    use std::io::Write as _;
    tmp.write_all(body.as_bytes()).context("write temp unit")?;
    tmp.as_file_mut().sync_all().context("sync temp unit")?;
    tmp.persist(&path)
        .map_err(|e| anyhow::anyhow!("{}", e))
        .context("persist unit file")?;
    Ok(path)
}

/// Run `systemctl --user enable --now carryoverd`. Tolerant of no-systemctl
/// environments (CI containers): if systemctl isn't on PATH we return Ok(false).
pub fn enable_and_start() -> Result<bool> {
    if !systemctl_available()? {
        return Ok(false);
    }
    let status = Command::new("systemctl")
        .args(["--user", "enable", "--now", UNIT_FILE_NAME])
        .status()
        .context("invoke systemctl --user enable --now")?;
    if !status.success() {
        anyhow::bail!("systemctl --user enable --now failed: {status}");
    }
    Ok(true)
}

/// Run `systemctl --user disable --now carryoverd`. Tolerant of no-systemctl.
pub fn disable_and_stop() -> Result<bool> {
    if !systemctl_available()? {
        return Ok(false);
    }
    let status = Command::new("systemctl")
        .args(["--user", "disable", "--now", UNIT_FILE_NAME])
        .status()
        .context("invoke systemctl --user disable --now")?;
    // disable on a non-installed unit returns nonzero; tolerate that.
    let _ = status;
    Ok(true)
}

/// Run `systemctl --user stop carryoverd`. Used by `carryover stop`.
pub fn stop_only() -> Result<bool> {
    if !systemctl_available()? {
        return Ok(false);
    }
    let status = Command::new("systemctl")
        .args(["--user", "stop", UNIT_FILE_NAME])
        .status()
        .context("invoke systemctl --user stop")?;
    let _ = status;
    Ok(true)
}

/// Remove the unit file from disk. Idempotent — missing file is fine.
pub fn remove_unit_file() -> Result<bool> {
    let path = unit_file_path()?;
    if !path.exists() {
        return Ok(false);
    }
    std::fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
    Ok(true)
}

fn systemctl_available() -> Result<bool> {
    Ok(Command::new("systemctl")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_file_path_under_xdg_config() {
        let p = unit_file_path().unwrap();
        let s = p.to_string_lossy();
        assert!(s.ends_with("/systemd/user/carryoverd.service"), "got: {s}");
    }

    #[test]
    fn render_unit_contains_expected_sections() {
        let body = render_unit().unwrap();
        assert!(body.contains("[Unit]"));
        assert!(body.contains("[Service]"));
        assert!(body.contains("[Install]"));
        assert!(body.contains("ExecStart="));
        assert!(body.contains("Restart=on-failure"));
        assert!(body.contains("WantedBy=default.target"));
    }

    #[test]
    fn render_unit_uses_current_exe_path() {
        let exe = std::env::current_exe().unwrap();
        let body = render_unit().unwrap();
        let exe_str = exe.to_string_lossy();
        assert!(body.contains(&*exe_str), "body should reference exe path");
    }

    #[test]
    fn systemctl_available_returns_ok() {
        // Just verify the helper returns Ok(_); true/false depends on the machine.
        let result = systemctl_available();
        assert!(result.is_ok(), "systemctl_available should not error");
    }
}
