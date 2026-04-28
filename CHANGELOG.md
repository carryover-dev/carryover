# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html) once v0.1.0 ships.

## 0.1.1 — 2026-04-28

### Fixed

- Cursor: inject handoff content directly via `beforeSubmitPrompt` stdout `context` field — AI models inside Cursor don't have a filesystem read tool, so the AGENTS.md pointer alone was silent.
- Cursor: per-project injection flag (`$PWD/.carryover/cursor-injected`) so switching between projects each gets a fresh inject; stop hook clears both project and global flags.
- Cursor/Codex hooks: send `$PWD` as `cwd` in hook payload so daemon writes project-level `<project>/.carryover/handoff.md` alongside the global `~/.carryover/handoff.md`.
- Pointer block: embed absolute `~/.carryover/handoff.md` path in global `~/AGENTS.md` / `~/CLAUDE.md`; use relative `.carryover/handoff.md` only in project-level files.
- Hook migration: detect and replace stale hook entries by port+path match so old format is cleanly upgraded on `refresh`.

## 0.1.0 — 2026-04-28

First public release. Working cross-tool context-handoff between Claude Code, Cursor, and Codex on Linux. macOS support documented in release notes.

### Added

- `carryoverd install` — writes correct hooks for Claude Code (`settings.json`), Cursor (`hooks.json` with wrapper scripts), and Codex (`config.toml` notify array + AGENTS.md pointer block).
- `carryoverd refresh` — re-applies hooks idempotently after config changes.
- `carryoverd uninstall [--purge]` — removes all hooks; `--purge` also wipes the ledger.
- `carryoverd status` — shows daemon liveness and configured tools.
- Background daemon (`carryoverd`) with systemd unit for Linux autostart.
- SQLite ledger for per-session handoff snapshots.
- Session-window tracking (5-minute idle timeout) to distinguish resuming from a fresh start.
- npm package (`npm install -g carryover`) with SHA-256–verified binary download.
- Homebrew formula (macOS, pending tap publication).
- cosign keyless signing for all release artifacts.

## [Unreleased]

Pre-launch — design docs only. The daemon is not yet implemented. See [`ROADMAP.md`](./ROADMAP.md) for what ships in v0.1.

### Added

- `ARCHITECTURE.md` — full contextless-design specification, transcript-location matrix, hook event matrix, and competitor comparison.
- `VISION.md` — project thesis.
- `ROADMAP.md` — v0.1, v0.2, and v0.3 plans.
- Project scaffolding: `README.md`, `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md`, `SECURITY.md`, `GOVERNANCE.md`, `AUTHORS`.
- GitHub templates: issue templates, pull request template, `CODEOWNERS`, `FUNDING.yml`.

### Changed

- _Nothing yet._

### Deprecated

- _Nothing yet._

### Removed

- _Nothing yet._

### Fixed

- _Nothing yet._

### Security

- _Nothing yet._
