//! `carryover status` — diagnostics output: config + ledger stats + recent events.

use anyhow::{Context, Result};

use super::config::Config;
use crate::storage::Ledger;

pub fn run() -> Result<()> {
    let cfg_path = Config::default_path()?;
    let cfg = Config::load_or_default(&cfg_path)?;

    println!("Carryover status");
    println!("================");
    println!("Config:        {}", cfg_path.display());
    println!("Resume mode:   {}", cfg.resume_mode);
    println!(
        "Tools:         {}",
        if cfg.tools.is_empty() {
            "<none configured>".to_string()
        } else {
            cfg.tools.join(", ")
        }
    );

    let ledger_path = Ledger::default_path().context("resolve ledger path")?;
    let ledger_size = std::fs::metadata(&ledger_path)
        .map(|m| m.len())
        .unwrap_or(0);
    println!(
        "Ledger:        {} ({} bytes)",
        ledger_path.display(),
        ledger_size
    );

    if ledger_path.exists() {
        let ledger = Ledger::open(&ledger_path).context("open ledger")?;
        println!();
        println!("Recent events:");
        for tool in &cfg.tools {
            let recent = ledger.query_recent(tool, 3).unwrap_or_default();
            if recent.is_empty() {
                println!("  {} — no events", tool);
            } else {
                println!("  {} ({} recent):", tool, recent.len());
                for row in &recent {
                    let preview: String = row.content.chars().take(60).collect();
                    println!("    [{}] {}: {}", row.ts, row.role, preview);
                }
            }
        }
    } else {
        println!();
        println!(
            "Ledger does not exist yet — run `carryoverd install` and let the daemon capture events."
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify that status-related logic does not panic when the ledger is absent.
    /// We can't call run() directly without affecting the real ~/.carryover dir,
    /// so we exercise the key sub-paths inline.
    #[test]
    fn status_with_no_ledger_does_not_panic() {
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join("config.json");
        // Config missing → load_or_default returns default.
        let cfg = Config::load_or_default(&cfg_path).unwrap();
        assert_eq!(cfg.resume_mode, "ask");
        assert!(cfg.tools.is_empty());

        // Ledger path points to something that doesn't exist.
        let ledger_path = dir.path().join("ledger.sqlite");
        assert!(!ledger_path.exists());
        // Simulates the else branch: no panic.
        let size = std::fs::metadata(&ledger_path)
            .map(|m| m.len())
            .unwrap_or(0);
        assert_eq!(size, 0);
    }

    #[test]
    fn status_with_empty_tools_shows_placeholder() {
        let cfg = Config {
            tools: vec![],
            resume_mode: "ask".to_string(),
        };
        let display = if cfg.tools.is_empty() {
            "<none configured>".to_string()
        } else {
            cfg.tools.join(", ")
        };
        assert_eq!(display, "<none configured>");
    }

    #[test]
    fn status_with_tools_joins_names() {
        let cfg = Config {
            tools: vec!["claude".to_string(), "cursor".to_string()],
            resume_mode: "ask".to_string(),
        };
        let display = if cfg.tools.is_empty() {
            "<none configured>".to_string()
        } else {
            cfg.tools.join(", ")
        };
        assert_eq!(display, "claude, cursor");
    }

    #[test]
    fn status_ledger_query_returns_empty_for_unknown_tool() {
        let dir = tempfile::tempdir().unwrap();
        let ledger_path = dir.path().join("ledger.sqlite");
        let ledger = Ledger::open(&ledger_path).unwrap();
        let rows = ledger.query_recent("no-such-tool", 3).unwrap();
        assert!(rows.is_empty());
    }
}
