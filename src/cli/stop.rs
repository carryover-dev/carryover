//! `carryover stop` — sends SIGTERM via systemctl --user stop.

use anyhow::Result;

#[cfg(target_os = "linux")]
pub fn run() -> Result<()> {
    use crate::install::systemd;
    let stopped = systemd::stop_only()?;
    if stopped {
        println!("Sent stop to carryoverd via systemctl --user.");
    } else {
        eprintln!("systemctl not available; nothing to stop.");
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn run() -> Result<()> {
    eprintln!("`carryover stop` on this OS is deferred (see MAC_HANDOVER.md for macOS launchd).");
    Ok(())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    /// systemctl is present on Linux CI runners; this test just checks the
    /// function returns Ok without panicking. Whether it returns true/false
    /// depends on the machine.
    #[test]
    fn stop_run_returns_ok() {
        // run() calls stop_only() which calls systemctl --user stop.
        // On a machine without a running carryoverd unit this still returns Ok.
        let result = run();
        assert!(result.is_ok(), "stop::run should not error: {result:?}");
    }
}
