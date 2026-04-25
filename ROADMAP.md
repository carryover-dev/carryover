# Roadmap

This roadmap is intentionally narrow. Items not listed are out of scope until they are listed.

## v0.1 — MVP

Target: a working `carryoverd` daemon that captures and restores task state across the three most-used agents, all of which have confirmed transcript paths and hook systems.

- **First task — path validation script (cross-OS).** A repeatable detection script that confirms each v0.1 tool's transcript path + format on macOS, Linux, and Windows. Linux baseline already captured in `~/workspace/carryover-context/LINUX_VERIFICATION.md`. macOS and Windows runs must complete and match before any daemon code lands.
- **Interactive `carryover install`** — TUI picker that detects installed tools, asks the user which to enable, confirms hook config paths per tool, shows a plan, and writes hook stubs only after confirmation. Saves selections to `~/.carryover/config.toml`.
- **`carryover refresh`** — re-runs detection with prior selections pre-checked; applies path changes when a tool's config moves between versions; idempotent.
- **Privacy split** — publisher writes payload to `<project>/.carryover/handoff.md` (gitignored), only static pointer text to `AGENTS.md` + `CLAUDE.md`. Install adds `.carryover/` to each project's `.gitignore` on first encounter.
- `carryoverd` daemon: long-lived, started by `launchd` (macOS) and `systemd` (Linux) at login.
- Filesystem watcher (Rust `notify` crate) on transcript directories.
- Local hook endpoint on `localhost:47823` (axum).
- SQLite ledger at `~/.carryover/ledger.sqlite`.
- Normalizer for JSONL (Claude Code, Codex) and SQLite (Cursor) transcripts.
- Distiller: regex + tree-sitter + git metadata, fully deterministic.
- Publisher: writes `~/.carryover/handoff.md` (≤50 lines) and the bounded `[CARRYOVER]` block in `AGENTS.md`.
- Hook stubs and one-shot installer for **Claude Code**, **Cursor**, and **Codex**.
- Restore via `SessionStart` stdout injection where supported, and the `AGENTS.md` rail as the universal fallback.
- Resume protocol with `ask` / `brief` / `silent` modes (default: `ask`) — meta-instruction header in the injected handoff that gates the agent's first response on user confirmation.
- Packaging: Homebrew tap and an npm global binary fallback.
- CI with required status checks, `dependabot`, and `semantic-release`.

## v0.2 — Cross-tool completeness

Add the remaining three agents from the MVP target list.

- **Copilot (VS Code)** — JSON transcripts under VS Code workspace storage, hooks via `chat.hookFilesLocations`. Validate whether `PreCompact` is GA or still preview.
- **Windsurf** — no on-disk transcript exposed. Capture via Cascade Hooks into Carryover's own store. Validate exact event names and stdout-injection behavior.
- **Aider** — Markdown transcript at `$CWD/.aider.chat.history.md` plus `.aider.llm.history`. No hook system, so capture is filesystem-watch-only and restore is via the `AGENTS.md` rail.

## v0.3 — Beyond MVP

Items pulled forward from [`ARCHITECTURE.md`](./ARCHITECTURE.md)'s open questions and design extensions.

- Validation pass on the speculative items in `ARCHITECTURE.md` (Cursor `sessionStart.additional_context` reliability, Copilot `PreCompact` status, Aider tool-call structure).
- Multi-project ledger with project-scoped handoff selection.
- Configurable distiller policies (per-project rules for what counts as "recent files," "failed approaches," etc.).
- Metrics surface: how many sessions, how much state preserved, distilled at what latency — local only, opt-in, never phoned home.
- Optional encrypted ledger at rest.
- A documented integration contract for new agents added by the community.

Out of scope for the foreseeable future: cloud-hosted ledgers, agent-side prompt summarization, and general-purpose agent memory.
