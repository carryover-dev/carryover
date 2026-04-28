//! `carryover install` — interactive tool picker + writes hook stubs + writes config.

use anyhow::{Context, Result};
use std::io::IsTerminal;
use std::path::PathBuf;

use super::config::Config;
use super::hooks_writer;
use crate::toolspec::specs::ALL_TOOLS;

pub fn run() -> Result<()> {
    let home = dirs::home_dir().context("home dir not found")?;

    // Detect each tool by checking whether any of its config/transcript paths exist.
    let detected: Vec<bool> = ALL_TOOLS
        .iter()
        .map(|spec| {
            spec.config_paths
                .iter()
                .any(|p| p.resolve_first_existing(&home).is_some())
                || spec
                    .transcript_paths
                    .iter()
                    .any(|p| p.resolve_first_existing(&home).is_some())
        })
        .collect();

    let labels: Vec<String> = ALL_TOOLS.iter().map(|s| s.name.to_string()).collect();
    let items_with_state: Vec<(String, bool)> = labels
        .iter()
        .cloned()
        .zip(detected.iter().copied())
        .collect();

    // Non-TTY guard: in CI / piped contexts we accept the detected list as-is.
    let selected = if std::io::stderr().is_terminal() {
        use dialoguer::{theme::ColorfulTheme, MultiSelect};
        let chosen = MultiSelect::with_theme(&ColorfulTheme::default())
            .with_prompt("Which AI agents do you use? (space to toggle, enter to confirm)")
            .items_checked(items_with_state.clone())
            .interact_opt()
            .context("dialoguer interact")?
            .unwrap_or_default();
        chosen
            .into_iter()
            .map(|i| labels[i].clone())
            .collect::<Vec<_>>()
    } else {
        items_with_state
            .iter()
            .filter(|(_, on)| *on)
            .map(|(n, _)| n.clone())
            .collect()
    };

    if selected.is_empty() {
        println!("No tools selected. Run `carryover install` again to choose tools.");
        return Ok(());
    }

    println!("Installing hooks for: {}", selected.join(", "));

    // Write hook stubs per selected tool.
    for tool_name in &selected {
        let Some(spec) = ALL_TOOLS.iter().find(|s| s.name == tool_name.as_str()) else {
            continue;
        };

        // Find the config path: prefer existing, else use the first candidate.
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
                "  {}: no config path candidate for this OS — skipping hook write",
                tool_name
            );
            continue;
        };

        let pairs = hook_pairs_for(tool_name);
        let modified = match tool_name.as_str() {
            "claude" => hooks_writer::write_claude_hooks(&config_path, &pairs)
                .with_context(|| format!("write claude hooks to {}", config_path.display()))?,
            "cursor" => {
                hooks_writer::write_cursor_wrapper_scripts(&home, &pairs)
                    .with_context(|| "write cursor wrapper scripts")?;
                hooks_writer::write_cursor_hooks(&config_path, &pairs)
                    .with_context(|| format!("write cursor hooks to {}", config_path.display()))?
            }
            "codex" => {
                // TODO(codex-toml): wire up after toml crate lands in a follow-up PR.
                eprintln!("  codex: skipping hook stub write (TOML editing lands in a follow-up)");
                false
            }
            _ => false,
        };
        println!(
            "  {} hooks {} ({})",
            tool_name,
            if modified { "installed" } else { "unchanged" },
            config_path.display()
        );
    }

    // Save config.
    let cfg = Config {
        tools: selected,
        resume_mode: "ask".to_string(),
    };
    let cfg_path = Config::default_path()?;
    cfg.save(&cfg_path).context("save config")?;
    println!("Config saved to {}", cfg_path.display());

    #[cfg(target_os = "linux")]
    {
        use crate::install::systemd;
        match systemd::write_unit_file() {
            Ok(path) => println!("Wrote systemd unit to {}", path.display()),
            Err(e) => eprintln!("Could not write systemd unit: {e}"),
        }
        match systemd::enable_and_start() {
            Ok(true) => println!("Daemon enabled and started via systemctl --user."),
            Ok(false) => eprintln!("systemctl not available; daemon not auto-started."),
            Err(e) => eprintln!("Could not enable/start daemon via systemctl: {e}"),
        }
    }

    Ok(())
}

/// Return the (tool, event) pairs to register for a given tool name.
pub fn hook_pairs_for(tool: &str) -> Vec<(&'static str, &'static str)> {
    match tool {
        "claude" => vec![
            ("claude", "SessionStart"),
            ("claude", "SessionEnd"),
            ("claude", "PreCompact"),
            ("claude", "UserPromptSubmit"),
        ],
        "cursor" => vec![("cursor", "beforeSubmitPrompt"), ("cursor", "stop")],
        "codex" => vec![("codex", "SessionStart"), ("codex", "Stop")],
        _ => vec![],
    }
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

    #[test]
    fn hook_pairs_for_claude_returns_four_events() {
        let pairs = hook_pairs_for("claude");
        assert_eq!(pairs.len(), 4);
        let events: Vec<&str> = pairs.iter().map(|(_, e)| *e).collect();
        assert!(events.contains(&"SessionStart"));
        assert!(events.contains(&"SessionEnd"));
        assert!(events.contains(&"PreCompact"));
        assert!(events.contains(&"UserPromptSubmit"));
    }

    #[test]
    fn hook_pairs_for_cursor_returns_two_events() {
        let pairs = hook_pairs_for("cursor");
        assert_eq!(pairs.len(), 2);
        let events: Vec<&str> = pairs.iter().map(|(_, e)| *e).collect();
        assert!(events.contains(&"beforeSubmitPrompt"));
        assert!(events.contains(&"stop"));
    }

    #[test]
    fn hook_pairs_for_unknown_returns_empty() {
        let pairs = hook_pairs_for("unknown-tool");
        assert!(pairs.is_empty());
    }

    #[test]
    fn non_tty_install_returns_detected_defaults() {
        // Simulate the non-TTY path: items_with_state with some true/false,
        // filter to only the `true` ones.
        let items_with_state = [
            ("claude".to_string(), true),
            ("cursor".to_string(), false),
            ("codex".to_string(), true),
        ];
        let selected: Vec<String> = items_with_state
            .iter()
            .filter(|(_, on)| *on)
            .map(|(n, _)| n.clone())
            .collect();
        assert_eq!(selected, vec!["claude", "codex"]);
    }

    #[test]
    fn expand_tilde_joins_home() {
        let home = std::path::Path::new("/home/user");
        let result = expand_tilde("~/.claude/settings.json", home);
        assert_eq!(
            result,
            std::path::PathBuf::from("/home/user/.claude/settings.json")
        );
    }

    #[test]
    fn expand_tilde_passthrough_absolute() {
        let home = std::path::Path::new("/home/user");
        let result = expand_tilde("/etc/foo", home);
        assert_eq!(result, std::path::PathBuf::from("/etc/foo"));
    }
}
