# Carryover

> Keeps AI agents on-task across sessions, tool switches, and compaction — without burning context.

**Status: alpha — design docs only, not production ready.** No binaries are shipped yet. The repository today contains the architecture, vision, and roadmap for the v0.1 daemon.

## What is Carryover?

Carryover is a long-lived local daemon that captures AI agent context from on-disk transcripts and republishes a bounded, fifty-line handoff. Your agent stays focused across session boundaries, tool switches, and compaction events — whether you're shipping a feature, mapping a competitive landscape, or drafting an investor memo. The bookkeeping happens locally so the agent itself never has to do the work.

## Why does this exist?

AI agents already write a complete transcript of every session to disk — Claude Code as JSONL under `~/.claude/projects/`, Codex CLI under `~/.codex/sessions/`, Cursor as SQLite under VS Code's workspace storage, Aider as Markdown in the working tree, and so on. Yet every existing handoff tool asks the agent to summarize itself, write a markdown file, or call an MCP tool — work the agent has to perform inside its own context window.

Carryover takes a different path. The state is already on disk. A local daemon reads it, distills a bounded handoff, and writes it back where the next session — same tool or different — will pick it up. The agent never sees the bookkeeping, so the work happens without burning context.

That gives Carryover three properties no competitor has at once:

1. **Capture happens without burning context.** Hooks fire locally, the daemon reads transcripts directly, and the agent does no writing.
2. **Restore is bounded.** A fifty-line handoff is the contract. Older state lives in the local SQLite ledger, not in the agent's prompt.
3. **It works across tools.** Claude Code, Cursor, Codex, Copilot, Windsurf, and Aider all converge on the same `AGENTS.md` rail.

## Architecture

A long-lived user daemon (`carryoverd`) started by `launchd` or `systemd` watches each tool's transcript directory and exposes a local hook endpoint at `localhost:47823`. Captured state flows through a normalizer and a deterministic distiller (regex, AST via tree-sitter, git metadata) into a SQLite ledger at `~/.carryover/ledger.sqlite`, then out to a fifty-line handoff at `~/.carryover/handoff.md` and a bounded `[CARRYOVER]` block inside the project's `AGENTS.md`.

For the full design — including the confirmed transcript-location matrix, the per-tool hook event matrix, the restore paths, and the comparison against existing tools — see [`ARCHITECTURE.md`](./ARCHITECTURE.md).

## Quickstart

Carryover is pre-code. There is no binary to install yet. The shipping plan is:

- Homebrew tap (`brew install carryover`) for macOS and Linux.
- npm global binary (`npm install -g carryover`) as a fallback.
- A one-shot `carryover init` that writes hook stubs into each detected tool's settings file.

For now, the right entry points are:

- Read [`ARCHITECTURE.md`](./ARCHITECTURE.md) for the design.
- Read [`VISION.md`](./VISION.md) for the thesis.
- Read [`ROADMAP.md`](./ROADMAP.md) for what ships when.
- Watch the repository for the v0.1.0 release.

## Where to get help

- **Bugs and feature requests** — open a GitHub issue using the templates in `.github/ISSUE_TEMPLATE/`.
- **Design discussion and questions** — GitHub Discussions is the single focal point at launch. Chat platforms will be added only when traffic exceeds maintainer bandwidth.
- **Security disclosures** — see [`SECURITY.md`](./SECURITY.md). Do not file public issues for security reports.

The maintainer responds to new issues within seven days. See [`CONTRIBUTING.md`](./CONTRIBUTING.md) for the full commitment.

## Who is Carryover for?

Three target users, stated plainly:

- I think Carryover would really help **developers who switch between Cursor, Claude Code, and Codex on the same project**, who are trying to **carry intent and recent decisions across tools without manually re-explaining the state**.
- I think Carryover would really help **founders running long sessions on strategy, fundraising, market research, or competitive analysis with Claude Code**, who are trying to **keep weeks of context across `PreCompact` and tool restarts without losing the thread**.
- I think Carryover would really help **researchers and long-form writers using AI agents for synthesis, drafting, and source-tracking**, who are trying to **preserve outlines, decisions, and reference lists across session boundaries**.

## Project links

- [Architecture](./ARCHITECTURE.md)
- [Vision](./VISION.md)
- [Roadmap](./ROADMAP.md)
- [Governance](./GOVERNANCE.md)
- [Contributing](./CONTRIBUTING.md)
- [Code of Conduct](./CODE_OF_CONDUCT.md)
- [Security policy](./SECURITY.md)
- [Changelog](./CHANGELOG.md)
- [License (Apache 2.0)](./LICENSE)
