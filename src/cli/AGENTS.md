# AGENTS.md — cli

## Purpose
Provide the `carryoverd` CLI surface: parse arguments with clap derive API and dispatch to subsystem entry points.

## Architectural decisions that constrain this module
- D3 (decisions.md): error type is `anyhow::Result<T>` — CLI collapses typed lib errors into user-readable messages.
- RELEASE_ROADMAP.md: 6 subcommands for v0.1: install, refresh, status, start, stop, uninstall.
- decisions.md gotcha #4/#5 (dialoguer): guard all interactive prompts with `std::io::IsTerminal`; use `items_checked()` not `defaults()`.

## Files
- `mod.rs` — `Cli` struct, `Commands` enum, and `pub async fn run() -> anyhow::Result<()>` entry point

## Out of scope for v0.1
- Interactive multi-select install wizard (Phase 2 TASK-22)
- Shell completion generation
- Machine-readable JSON output mode
