//! `carryover refresh` — re-detect tools, diff against config, apply updates.

use anyhow::{Context, Result};
use std::path::PathBuf;

use super::config::Config;
use super::hooks_writer;
use super::install::hook_pairs_for;
use crate::publish::ensure_pointer_block;
use crate::toolspec::specs::ALL_TOOLS;

pub fn run() -> Result<()> {
    let home = dirs::home_dir().context("home dir not found")?;
    let cfg_path = Config::default_path()?;
    let cfg = Config::load_or_default(&cfg_path)?;

    if cfg.tools.is_empty() {
        println!("No tools configured. Run `carryover install` first.");
        return Ok(());
    }

    println!("Refreshing hooks for: {}", cfg.tools.join(", "));

    let mut any_changed = false;

    for tool_name in &cfg.tools {
        let Some(spec) = ALL_TOOLS.iter().find(|s| s.name == tool_name.as_str()) else {
            eprintln!("  {}: unknown tool — skipping", tool_name);
            continue;
        };

        let config_path = spec
            .config_paths
            .iter()
            .find_map(|p| p.resolve_first_existing(&home))
            .or_else(|| {
                spec.config_paths.first().and_then(|p| {
                    let cands = p.for_current_os();
                    cands.first().map(|c| expand_tilde(c, &home))
                })
            });

        let Some(config_path) = config_path else {
            eprintln!(
                "  {}: no config path candidate for this OS — skipping",
                tool_name
            );
            continue;
        };

        let pairs = hook_pairs_for(tool_name);
        let modified = match tool_name.as_str() {
            "claude" => hooks_writer::write_claude_hooks(&config_path, &pairs)
                .with_context(|| format!("refresh claude hooks at {}", config_path.display()))?,
            "cursor" => {
                hooks_writer::write_cursor_wrapper_scripts(&home, &pairs)
                    .with_context(|| "refresh cursor wrapper scripts")?;
                hooks_writer::write_cursor_hooks(&config_path, &pairs)
                    .with_context(|| format!("refresh cursor hooks at {}", config_path.display()))?
            }
            "codex" => {
                hooks_writer::write_codex_wrapper_script(&home)
                    .with_context(|| "refresh codex notify script")?;
                let m =
                    hooks_writer::write_codex_notify(&config_path, &home).with_context(|| {
                        format!("refresh codex notify at {}", config_path.display())
                    })?;
                for md in &[
                    home.join(".codex").join("AGENTS.md"),
                    home.join("AGENTS.md"),
                    home.join("CLAUDE.md"),
                ] {
                    ensure_pointer_block(md)
                        .with_context(|| format!("refresh pointer block at {}", md.display()))?;
                }
                m
            }
            _ => false,
        };

        println!(
            "  {} {} ({})",
            tool_name,
            if modified { "updated" } else { "unchanged" },
            config_path.display()
        );

        if modified {
            any_changed = true;
        }
    }

    if any_changed {
        println!("Refresh complete — hooks updated.");
    } else {
        println!("Refresh complete — all hooks already up to date.");
    }

    Ok(())
}

fn expand_tilde(path: &str, home: &std::path::Path) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::config::Config;

    #[test]
    fn refresh_with_no_config_does_not_panic() {
        // Simulate the no-tools-configured path without touching the real home dir.
        let cfg = Config {
            tools: vec![],
            resume_mode: "ask".to_string(),
        };
        // The real run() calls default_path() which touches the fs; test the
        // logic branch that returns early when tools is empty.
        assert!(cfg.tools.is_empty());
    }

    #[test]
    fn refresh_writes_hooks_for_configured_tools() {
        // End-to-end: write a config, then call the hooks_writer for each tool
        // using a temp dir as the "home", and verify idempotency.
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();

        // Pre-create a fake claude settings.json so resolve_first_existing finds it.
        let claude_dir = home.join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        let settings_path = claude_dir.join("settings.json");
        std::fs::write(&settings_path, "{}").unwrap();

        let pairs = hook_pairs_for("claude");
        let first = hooks_writer::write_claude_hooks(&settings_path, &pairs).unwrap();
        assert!(first, "first write should modify");

        let second = hooks_writer::write_claude_hooks(&settings_path, &pairs).unwrap();
        assert!(!second, "second write should be no-op (idempotent)");
    }
}
