//! OS-level installation helpers — daemon registration via systemd-user
//! on Linux, launchd on macOS (deferred to v0.1.x per MAC_HANDOVER.md).

#[cfg(target_os = "linux")]
pub mod systemd;
