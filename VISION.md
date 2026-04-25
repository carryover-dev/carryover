# Vision

## The thesis

AI agents lose state. They lose it when a session ends, when the user switches from Cursor to Claude Code mid-task, and most violently when context compaction fires in the middle of a working set. The work people do with these agents is no longer just code — it is research, founder work, market analysis, long-form writing, drafting and synthesis. The state-loss problem is the same regardless of domain. Every existing handoff tool tries to fix this by asking the agent to summarize itself, write a markdown file, or call an MCP tool — work the agent has to perform inside its own context window.

Carryover's premise is that this is unnecessary. The state is already on disk. Claude Code writes JSONL transcripts to `~/.claude/projects/`. Codex CLI writes JSONL to `~/.codex/sessions/`. Cursor writes SQLite to VS Code's workspace storage. Aider writes Markdown to the working tree. Copilot writes JSON under VS Code's user data directory.

If a local daemon reads those files, distills the relevant subset deterministically, and republishes a bounded handoff, the agent never has to do the bookkeeping. **The work happens locally — the agent never sees it, and nothing is burned from the agent's context budget.**

## Design commitments

- **Contextless design.** Capture and distillation are pure code: filesystem watching, regex, AST via tree-sitter, git metadata, SQLite. No call ever asks the agent to summarize itself.
- **Bounded restore.** A single fifty-line handoff is the contract surface. Older state lives in the local SQLite ledger, not in the agent's prompt.
- **Local-first.** No cloud, no remote dependency. The ledger is a SQLite file under `~/.carryover/`. The hook endpoint is on `localhost`.
- **Cross-tool by default.** Carryover converges on a single rail — the bounded `[CARRYOVER]` block inside `AGENTS.md` — so a session in Cursor can pick up where Claude Code left off.

## Cross-tool reach

The MVP target is the six AI agents that own the developer-tool market today: Claude Code, Cursor, Codex, Copilot, Windsurf, and Aider. They are the integration surface because they expose hooks and write transcripts; the work users do inside them spans code, research, strategy, and writing. Five of the six already write authoritative transcripts to disk; Windsurf is the only gap and is handled via a Cascade hook that captures into Carryover's own store.

Carryover's success metric is not "more state captured" or "longer histories." It is: **a user can compact, switch tools, or close their laptop, and the next session resumes the same task — whether that task is code, research, founder work, or long-form writing — with no manual re-explanation and without burning context.**

## What Carryover is not

- Not an agent memory system. Long-term cross-project memory is a different problem.
- Not a cloud service. There is no hosted ledger and no plan for one.
- Not a prompt-engineering layer. The handoff is a deterministic distillation of on-disk state, not a generated summary.

For the technical realization of this vision, see [`ARCHITECTURE.md`](./ARCHITECTURE.md). For the shipping plan, see [`ROADMAP.md`](./ROADMAP.md).
