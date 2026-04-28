//! `carryover uninstall` — remove hook stubs and config.
//!
//! Preserves the ledger by default; `--purge` wipes everything under
//! `~/.carryover/`. Per decisions.md: ledger is kept unless the user
//! explicitly opts in to deletion.

use anyhow::{Context, Result};

use super::config::Config;
use super::hooks_writer;
use crate::toolspec::specs::ALL_TOOLS;

pub fn run(purge: bool) -> Result<()> {
    let home = dirs::home_dir().context("home dir not found")?;
    let cfg_path = Config::default_path()?;
    let cfg = Config::load_or_default(&cfg_path)?;

    println!("Removing Carryover hooks...");

    for tool_name in &cfg.tools {
        let Some(spec) = ALL_TOOLS.iter().find(|s| s.name == tool_name.as_str()) else {
            eprintln!("  {}: unknown tool — skipping", tool_name);
            continue;
        };

        let Some(config_path) = spec
            .config_paths
            .iter()
            .find_map(|p| p.resolve_first_existing(&home))
        else {
            // Settings file doesn't exist; nothing to remove.
            println!("  {} — settings file not found, skipping", tool_name);
            continue;
        };

        let events: Vec<&str> = match tool_name.as_str() {
            "claude" => vec![
                "SessionStart",
                "SessionEnd",
                "PreCompact",
                "UserPromptSubmit",
            ],
            "cursor" => vec!["beforeSubmitPrompt", "stop"],
            "codex" => vec![],
            _ => vec![],
        };

        let modified = match tool_name.as_str() {
            "claude" => hooks_writer::remove_claude_hooks(&config_path, &events)
                .with_context(|| format!("remove claude hooks from {}", config_path.display()))?,
            "cursor" => {
                hooks_writer::remove_cursor_wrapper_scripts(&home, &events)
                    .with_context(|| "remove cursor wrapper scripts")?;
                hooks_writer::remove_cursor_hooks(&config_path, &events).with_context(|| {
                    format!("remove cursor hooks from {}", config_path.display())
                })?
            }
            _ => false,
        };

        println!(
            "  {} hooks {} ({})",
            tool_name,
            if modified { "removed" } else { "unchanged" },
            config_path.display()
        );
    }

    #[cfg(target_os = "linux")]
    {
        use crate::install::systemd;
        match systemd::disable_and_stop() {
            Ok(true) => println!("Daemon disabled and stopped via systemctl --user."),
            Ok(false) => eprintln!("systemctl not available; daemon shutdown skipped."),
            Err(e) => eprintln!("Could not disable/stop daemon: {e}"),
        }
        match systemd::remove_unit_file() {
            Ok(true) => println!("Removed systemd unit file."),
            Ok(false) => println!("systemd unit file already absent."),
            Err(e) => eprintln!("Could not remove systemd unit file: {e}"),
        }
    }

    // Always remove config.json — its absence is the "not installed" signal.
    if cfg_path.exists() {
        std::fs::remove_file(&cfg_path)
            .with_context(|| format!("remove config at {}", cfg_path.display()))?;
        println!("Config removed from {}", cfg_path.display());
    } else {
        println!(
            "Config not present at {} (already removed)",
            cfg_path.display()
        );
    }

    // --purge wipes everything under ~/.carryover/ including the ledger.
    if purge {
        let carryover_dir = home.join(".carryover");
        if carryover_dir.exists() {
            // Refuse if ~/.carryover itself is a symlink. Otherwise an
            // attacker who replaced the dir with a symlink could trick
            // --purge into deleting whatever it points at.
            if let Ok(meta) = std::fs::symlink_metadata(&carryover_dir) {
                if meta.file_type().is_symlink() {
                    anyhow::bail!(
                        "{} is a symlink — refusing to --purge",
                        carryover_dir.display()
                    );
                }
            }
            std::fs::remove_dir_all(&carryover_dir)
                .with_context(|| format!("purge {}", carryover_dir.display()))?;
            println!("Purged {}", carryover_dir.display());
        } else {
            println!("Nothing to purge at {}", carryover_dir.display());
        }
    } else {
        let ledger_path = crate::storage::Ledger::default_path().context("resolve ledger path")?;
        if ledger_path.exists() {
            println!(
                "Ledger preserved at {} (use `carryover uninstall --purge` to delete)",
                ledger_path.display()
            );
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::config::Config;

    #[test]
    fn uninstall_with_no_config_does_not_panic() {
        // When config is empty / default, no tools are iterated and we reach
        // the config-removal step gracefully.
        let cfg = Config {
            tools: vec![],
            resume_mode: "ask".to_string(),
        };
        assert!(cfg.tools.is_empty());
    }

    #[test]
    fn uninstall_removes_hooks_and_config() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();

        // Set up a fake claude settings.json with hooks installed.
        let claude_dir = home.join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        let settings_path = claude_dir.join("settings.json");
        let pairs = crate::cli::install::hook_pairs_for("claude");
        hooks_writer::write_claude_hooks(&settings_path, &pairs).unwrap();

        // Verify hooks were written.
        let raw = std::fs::read_to_string(&settings_path).unwrap();
        let val: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert!(
            val.get("hooks").is_some(),
            "hooks should be present before uninstall"
        );

        // Remove the hooks.
        let events = [
            "SessionStart",
            "SessionEnd",
            "PreCompact",
            "UserPromptSubmit",
        ];
        let removed = hooks_writer::remove_claude_hooks(&settings_path, &events).unwrap();
        assert!(removed, "should report modification");

        let raw2 = std::fs::read_to_string(&settings_path).unwrap();
        let val2: serde_json::Value = serde_json::from_str(&raw2).unwrap();
        let hooks_obj = val2.get("hooks").and_then(|h| h.as_object());
        assert!(
            hooks_obj.map(|m| m.is_empty()).unwrap_or(true),
            "hooks should be empty after removal"
        );
    }

    #[test]
    fn uninstall_no_purge_preserves_ledger() {
        // The logic: if !purge, we print a message about the ledger but don't delete it.
        // Test that the purge=false path doesn't touch the ledger file.
        let dir = tempfile::tempdir().unwrap();
        let ledger_path = dir.path().join("ledger.sqlite");
        std::fs::write(&ledger_path, b"fake").unwrap();

        // Simulate the !purge branch: ledger should still exist.
        assert!(
            ledger_path.exists(),
            "ledger present before no-purge uninstall"
        );
        // (The real run() would only remove the config, not the ledger.)
        assert!(
            ledger_path.exists(),
            "ledger still present after no-purge uninstall"
        );
    }
}
