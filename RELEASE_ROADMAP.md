# Carryover Release Roadmap

> Each minor version delivers working features to a specific end user. Every release is a shippable increment.
> Testing & validation: See [TESTING_AND_VALIDATION.md](./TESTING_AND_VALIDATION.md) (lands with v0.1).
> Strategic direction: See [VISION.md](./VISION.md). Architecture: [ARCHITECTURE.md](./ARCHITECTURE.md). High-level milestone framing: [ROADMAP.md](./ROADMAP.md).

**Platform goal:** A user can compact, switch tools, or close their laptop, and the next AI session resumes the same task — across Claude Code, Cursor, Codex, Copilot, Windsurf, and Aider — with no manual re-explanation and without burning context.

**Platform context:**

- **Daemon:** Rust, single statically-linked binary (`carryoverd`). Long-lived, started by `launchd` (macOS) / `systemd --user` (Linux) at login.
- **Storage:** Single SQLite file at `~/.carryover/ledger.sqlite`. No server, no cloud.
- **Hook endpoint:** `axum` HTTP server bound to `127.0.0.1:47823` only.
- **fs watcher:** `notify` crate (cross-platform inotify / FSEvents / ReadDirectoryChangesW).
- **Distiller:** Pure-code extractors using regex + `tree-sitter` + git plumbing.
- **External systems:** Each AI agent's existing on-disk transcript format (Claude Code JSONL, Cursor SQLite, Codex JSONL, Copilot JSON, Aider Markdown). Carryover never asks the agent to do work — only reads what's already there.
- **Team:** Solo maintainer (`@rohitsux`) at v0.1; second admin onboarded before v0.2 announcement (per `LAUNCH_CHECKLIST.md`).
- **Validation baseline (already verified):** Linux + macOS transcript paths confirmed for Claude Code, Cursor, Codex via `~/workspace/carryover-context/detect_paths.sh`. Schema differences captured (Claude `parentUuid` DAG, Codex `session_meta` events, Cursor SQLite three keys).

---

## How to Use This Roadmap

1. Releases are defined by user value — each version must be usable by someone real.
2. Each version names its target user (`Delivers to:`) — usually a real engineer with a real workflow, not a generic persona.
3. Features are checkbox items so contributors can claim work.
4. The **Key Deliverables** table is how we *verify* the release is real, not how we describe it.
5. **Migration** section names what changes from the current codebase (vs greenfield).
6. **Cost** section is included only when a release has new compute / external API costs (mostly empty — Carryover is local-first by design).
7. Status flow: `PLANNED` → `BUILD` → `IN PROGRESS` → `DONE`.
8. Each feature lands with a test in `TESTING_AND_VALIDATION.md` (created in v0.1).

---

## v0.1 — Multi-tool MVP (Claude Code + Cursor + Codex)

**Status:** PLANNED
**Delivers to:** A developer who hits Claude rate limits or context loss mid-task and currently copes by *waiting* for Claude to come back, paying for multiple Claude accounts, or switching tools and accepting the context loss. The founder is this person (runs three Claude accounts to dodge rate limits). The founder's sister is also this person (full-stack SWE, switches tools when overloaded and re-explains the state from scratch). v0.1 removes the cost from "switch tools" so context-loss stops being the reason they stay locked in.
**Theme:** Cross-tool from day one. v0.1 is *not* "ship Claude support, then add Cursor and Codex later" — that would prove the daemon works but not the actual value prop. v0.1 is the differentiator: switch from Cursor to Claude Code mid-task, the next session picks up where the last left off, identical handoff regardless of source tool.

### Features

#### Capture & install

- [ ] **Cross-OS path validation harness in CI.** Bring `detect_paths.sh` into the repo at `scripts/detect_paths.sh`. CI runs it on macOS and Linux runners every PR; fails the build if any v0.1-trio path resolves wrong.
- [ ] **`ToolSpec` table — three tools.** Rust struct with `detect_binary`, `detect_version`, `config_paths` (OS-aware), `transcript_paths` (OS-aware), and `hooks_by_version: &[(VersionRange, HookSet)]`. v0.1 ships exactly three entries: Claude Code, Cursor, Codex.
- [ ] **Interactive `carryover install`.** Single TUI question: *"Which AI agents do you use?"* Pre-checks detected tools. Auto-resolves config paths and hook event names from the `ToolSpec` table per detected tool version. Shows a per-tool summary line before writing. Saves selections to `~/.carryover/config.toml`.
- [ ] **`carryover refresh` command.** Re-runs detection, picks up new tool versions, applies hook-name updates if a tool renamed events between releases. Idempotent. Writes a diff-style summary of what changed.
- [ ] **`carryover status` command.** Shows which tools are configured, daemon uptime, last snapshot timestamp per tool, ledger size, current `.carryover/handoff.md` contents.
- [ ] **Daemon (`carryoverd`)** as a single Rust binary with `tokio` runtime. Subcommands: `install`, `refresh`, `status`, `start`, `stop`, `uninstall`.
- [ ] **fs watcher** via `notify` crate watching all three transcript directories simultaneously: `~/.claude/projects/`, `~/.config/Cursor/User/workspaceStorage/` (Linux) / `~/Library/Application Support/Cursor/User/workspaceStorage/` (macOS), `~/.codex/sessions/`. Filters new bytes / rows since last seek; pushes events to the pipeline queue.
- [ ] **HTTP hook endpoint** on `127.0.0.1:47823` via `axum`. Routes for each tool: `POST /hook/<tool>/<event>`. JSON envelope contains `transcript_path` + `session_id` + tool-specific metadata. Returns `200 OK` immediately; queues the work.
- [ ] **Hook stub installation per tool.** `carryover install` writes one-line `curl` stubs into each detected tool's settings: `~/.claude/settings.json` (`SessionStart`/`SessionEnd`/`PreCompact`), `~/.cursor/hooks.json` (`sessionStart`/`stop`), `~/.codex/config.toml` (`SessionStart`/`Stop`). Idempotent — won't duplicate stubs on re-run.

#### Adapters (per-tool — independent implementations behind a shared trait)

- [ ] **`Adapter` trait** in `src/adapters/mod.rs`. Methods: `detect`, `read_new_records(since: Cursor)`, `parse(records) -> Vec<LedgerRow>`. Each tool implements the trait independently — no shared parsing.
- [ ] **Claude JSONL adapter (`src/adapters/claude.rs`).** Reads `parentUuid`/`sessionId`/`role`/`content`/`attachment` schema. **Filters by `type` field** — skips `file-history-snapshot`, `permission-mode`, `queue-operation`, `summary` (per `MAC_VERIFICATION.md` finding). Emits unified `LedgerRow`.
- [ ] **Cursor SQLite adapter (`src/adapters/cursor.rs`).** Reads `state.vscdb` via `rusqlite`. Decodes JSON values from `aiService.generations`, `aiService.prompts`, `composer.composerData`, `workbench.backgroundComposer.workspacePersistentData`. Handles WAL-locking with copy-then-read fallback.
- [ ] **Codex JSONL adapter (`src/adapters/codex.rs`).** Reads `session_meta` event first, then `event_msg` payloads. Different schema from Claude — the adapters do NOT share JSONL parsing code.

#### Storage & distillation

- [ ] **SQLite ledger** at `~/.carryover/ledger.sqlite`. Schema: one table `events(session_id, tool, ts, role, content, tool_calls_json, files_touched_json, parent_id)`. Append-only. Indexed on `(session_id, ts)` and `(tool, ts)`.
- [ ] **Distiller — always-on extractors.** `task` (last user prompt → 1 line), `open_questions` (regex on TODOs / unresolved errors), `next_action` (final sentence of latest assistant turn). Run every snapshot, regardless of source tool.
- [ ] **Distiller — coding-only extractors.** `recent_files` (latest-write-wins, capped at 10), `failed_approaches` (`tool_result` errors + retry-pattern matches), `git_context` (`git rev-parse HEAD` + `git diff --stat`). Activate only when ledger contains `tool_use` rows for the current session.

#### Publisher (privacy split + cross-tool dual-write)

- [ ] **Privacy-split publisher.** Writes 50-line payload to `~/.carryover/handoff.md` (global) AND `<project>/.carryover/handoff.md` (project-local). Adds `.carryover/` to project's `.gitignore` on first encounter (idempotent — checks before appending). Writes static pointer block to `<project>/AGENTS.md` AND `<project>/CLAUDE.md` (dual-write — Claude Code does NOT auto-read AGENTS.md). Hard 50-line cap enforced before write.
- [ ] **Cross-tool handoff identity.** The pointer block content is identical regardless of source tool. The `from <tool>` field in the payload header is the only thing that varies between snapshots. A handoff captured from Cursor reads correctly when injected into Claude Code's next session, and vice versa.

#### Resume

- [ ] **Resume protocol — `ask` mode** (default, only mode in v0.1). Meta-instruction header at top of payload tells the agent to summarize handoff + ask user before acting. Identical header regardless of which tool reads it.

#### OS integration

- [ ] **launchd / systemd-user unit installation.** `carryover install` writes the unit file and registers via `launchctl load` (macOS) or `systemctl --user enable --now carryoverd` (Linux).

#### Distribution

- [ ] **Homebrew tap formula.** `Formula/carryover.rb` in `carryover-dev/homebrew-tap`. Points at signed binary uploaded to GitHub Releases. SHA256 verified.
- [ ] **npm install path.** Real `carryover` package (replaces the `0.0.0-pre.1` placeholder) — postinstall script downloads the platform-correct binary from GitHub Releases.

#### CI / tests / release

- [ ] **Test fixture corpus.** Real Claude / Cursor / Codex transcripts captured during dev (sanitized — secret patterns redacted, paths anonymized). Lives in `tests/fixtures/`. Used by adapter unit tests.
- [ ] **`TESTING_AND_VALIDATION.md`** — full test matrix. One row per feature in this file with the corresponding test path.
- [ ] **CI on macOS-13, macOS-14, Ubuntu 22.04, Ubuntu 24.04:** `cargo build --release`, `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`, `bash scripts/detect_paths.sh` per OS. Required status checks before merge to `main`.
- [ ] **Cross-tool integration test.** Simulate session A in Claude Code → snapshot → session B in Cursor → verify Cursor reads `.carryover/handoff.md` on `SessionStart` and resumes. Then reverse direction. Then Claude → Codex. Then Codex → Cursor.
- [ ] **Release workflow.** Tag-triggered GitHub Actions builds binaries for `aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-unknown-linux-gnu`, uploads to Release, generates checksums.

#### Docs & demo

- [ ] **README quickstart updated** with the real install command + a 60-second demo GIF showing **Claude → snapshot → Cursor resume** (the cross-tool flex, not single-tool).

### Key Deliverables

| Deliverable | Verification |
|---|---|
| `brew install carryover-dev/tap/carryover && carryover install` works on a clean macOS machine | CI provisions a fresh macOS runner, runs the install, exits 0 |
| `npm i -g carryover && carryover install` works on a clean Linux machine | CI provisions a fresh Ubuntu runner, runs the install, exits 0 |
| Daemon survives logout/login and laptop suspend/resume | Integration test: `launchctl unload && launchctl load`, then verify the hook endpoint is back up |
| `PreCompact` hook produces a fresh `.carryover/handoff.md` within 200ms | Integration test sends a fake hook payload, asserts file mtime updated within 200ms |
| **Cross-tool resume:** Claude → snapshot → Cursor reads identical handoff and resumes | Recorded fixture run; manual verification |
| **Cross-tool resume (reverse):** Cursor → snapshot → Codex resumes | Same |
| `.carryover/handoff.md` is gitignored automatically on first run | Test in a fresh git repo: run daemon, then `git status` shows zero tracked files in `.carryover/` |
| Static pointer block is in both AGENTS.md and CLAUDE.md, identical bytes | grep `<!--CARRYOVER:START-->` in both files, `diff` the blocks |
| All three v0.1-trio adapters parse 5+ real transcript fixtures without errors | `cargo test --features adapter-tests` |
| Founder's sister (the named v0.1 user) installs Carryover, switches between Claude Code and Cursor over a one-week period, reports zero context-loss incidents | "Watch over her shoulder" install per the office-hours design doc, then weekly check-in |

### Migration from Current Codebase

- Net new project — `Cargo.toml` does not yet exist. v0.1 *is* the codebase landing.
- The `Adapter` trait is born in v0.1 (not retrofitted in v0.2). Three implementations land together. No shared JSONL parsing.
- Pre-existing `architecture/d2/*.d2` diagrams stay; CI also asserts they re-render without diff (catches accidental edits).
- `scripts/detect_paths.sh` is moved from internal `~/workspace/carryover-context/` into the public repo. (It stops being internal once v0.1 ships — anyone debugging install can run it.)
- `.gitignore` already has `.carryover/` defensively listed; daemon's first-run check stays idempotent.
- New crate dependencies: `axum`, `tokio`, `notify`, `rusqlite`, `serde`, `tree-sitter`, `clap`, `dialoguer`, `dirs`, `semver`.

---

## v0.2 — Cross-tool completeness (add Copilot + Windsurf + Aider)

**Status:** PLANNED
**Delivers to:** The team that has standardized on Copilot Chat in VS Code, plus the indie hacker who only uses Aider, plus the Windsurf early adopter. Anyone with a supported tool gets the same value v0.1 users got — bounded handoff, dual-write, resume protocol.
**Theme:** All six target tools supported. Carryover stops being "the Claude / Cursor / Codex tool" and becomes "the AI agent context tool."

### Features

- [ ] **Copilot Chat adapter.** Reads `~/.config/Code/User/workspaceStorage/<hash>/chatSessions/*.json` (Linux) / `~/Library/Application Support/Code/User/...` (macOS). Confirmed paths verified in CI before this version ships (per the open `[help wanted]` issue #14).
- [ ] **Windsurf Cascade hook adapter.** No on-disk transcript — capture happens through Cascade hook payloads delivered to `localhost:47823`. Carryover stores its own per-session ledger entries since `notify` has nothing to watch.
- [ ] **Aider Markdown adapter.** Reads `<project>/.aider.chat.history.md` and `<project>/.aider.llm.history`. Section-based parser. Coding-only extractors gracefully degrade if the markdown lacks structured tool-call data (per the speculative item flagged in `ARCHITECTURE.md`).
- [ ] **Per-tool README install snippets.** Copy-paste install snippets for each of the six tools, with verified expected output.
- [ ] **`carryover doctor` command.** Diagnostic output: which tools were detected vs configured vs working, last hook fire time per tool, common-error decoder. Output is paste-ready into bug reports.
- [ ] **Resume protocol modes.** Add `brief` and `silent` — configurable globally and per-project (`.carryover/config.toml`). Default stays `ask`.
- [ ] **Updated README** with comparison table from `ARCHITECTURE.md` lifted up — one row per supported tool, ✅ for what's verified, link to fixture.

### Key Deliverables

| Deliverable | Verification |
|---|---|
| Each of Copilot, Windsurf, Aider has a fixture-backed adapter test | `cargo test --features adapter-tests` |
| `carryover doctor` output is reproducible across same-OS machines (no machine-specific paths leak into output) | Integration test against a known fixture state |
| All six tools share one resume protocol UX (mode selection works for every tool's `SessionStart` injection path) | Manual cross-tool acceptance test |
| Windsurf path: documented as "Cascade hook only — no transcript file expected" | Documentation review |

### Migration from Current Codebase

- Extend the `Adapter` trait with optional methods for tools without on-disk transcripts (Windsurf).
- New module: `src/adapters/cascade_hook.rs` — handles incoming hook payloads as the source of truth for Windsurf, since fs watcher has nothing to read.

---

## v0.3 — Robustness, Windows, recovery

**Status:** PLANNED
**Delivers to:** Cross-platform dev teams. Anyone whose machine has been running Carryover long enough that something has gone wrong — corrupted SQLite, moved transcript path, daemon stuck.
**Theme:** Boring reliability. The daemon survives the worst week of the user's life.

### Features

- [ ] **Windows support.** Path resolution branch for `%APPDATA%`. Service installation via `sc.exe` or scheduled task at logon. Verified by `detect_paths.sh` Windows branch (currently a stub — closing the open `[help wanted]` issue #12).
- [ ] **`carryover repair` command.** Rebuild ledger from on-disk transcripts. Useful when the SQLite file gets corrupted (rare but real on power-loss).
- [ ] **Graceful adapter failure.** If one adapter throws, others keep working. Daemon logs the failure, exposes it via `carryover doctor`, never panics the process.
- [ ] **Automatic ledger compaction.** SQLite `VACUUM` weekly; trims rows older than configurable retention (default 90 days).
- [ ] **Hook endpoint health check.** Daemon self-pings `localhost:47823/health` on startup; if unhealthy, retries with backoff, reports via `carryover status`.
- [ ] **Better install prerequisite checks.** Detect missing `git`, missing `tree-sitter` parsers, missing OS support before writing any hook stubs.

### Key Deliverables

| Deliverable | Verification |
|---|---|
| `carryover install` works end-to-end on Windows 11 | Manual + CI runner |
| `carryover repair` reconstructs a corrupted ledger from raw transcripts in under 60 seconds for a 30-day archive | Benchmark in CI |
| Crash-recovery test: kill daemon mid-snapshot, restart, no data loss | Chaos test fixture |

### Cost Model

| Scenario | Cost |
|---|---|
| Idle daemon, no sessions | Negligible — RAM ~10 MB, CPU ~0%, disk: ledger growth halted |
| Heavy day, 50 hook fires | RAM ~40 MB peak, CPU bursts to 5–10% during distill, disk ~100 KB/day |
| 90-day retention, heavy user | Ledger ~10 MB total before compaction |

(All local. No external API or compute cost.)

---

## v0.4 — Configurability + multi-project

**Status:** PLANNED
**Delivers to:** Power users with multiple projects + custom workflows. People who want to tune the distiller per-project ("for my fundraising-research project, don't extract failed approaches — extract decisions").
**Theme:** Carryover stops being one-size-fits-all and becomes the user's instrument.

### Features

- [ ] **Per-project `.carryover/config.toml`.** Override resume mode, distiller toggles, custom regex patterns for `task` extractor.
- [ ] **Distiller — domain extractors.** Add `decisions` (regex on "we decided X because Y"), `references` (URLs, citations, document mentions), `action_items` (assignments + TODOs). Always-on by default — useful for non-coding work (the founder's actual use case spans research, strategy, fundraising).
- [ ] **Multi-project ledger.** Single SQLite file at `~/.carryover/ledger.sqlite`, but indexed by project; `carryover status --project <name>` filters.
- [ ] **`carryover history --project <name>`** — show the last N handoffs for a project, with timestamps + tool source.
- [ ] **Per-tool transcript redaction.** Hook to drop env-var values, secret-pattern matches, etc. before they land in the ledger.

### Key Deliverables

| Deliverable | Verification |
|---|---|
| Same machine running 3+ projects produces correct per-project handoff blocks (no cross-contamination) | Integration test with 5 simulated projects |
| `decisions`/`references`/`action_items` extractors fire on a synthetic non-coding session (founder writing fundraising research) | Fixture-backed test |
| Redaction strips a known secret pattern before it hits SQLite | Unit test |

---

## v0.5 — Optional encryption

**Status:** PLANNED
**Delivers to:** Engineers in regulated industries (healthcare, finance, defense) who can't have plaintext task state on disk by policy. Also: anyone running Carryover on a shared machine.
**Theme:** Local-first AND privacy-preserving.

### Features

- [ ] **Encrypted SQLite at rest.** SQLCipher integration; passphrase derived from OS keychain (Keychain on macOS, libsecret on Linux, Credential Manager on Windows).
- [ ] **Encrypted `handoff.md`** — written as `handoff.md.enc`, decrypted only when hook reads it. Plaintext mode remains the default for unchanged ergonomics.
- [ ] **Per-project key option.** Different projects can use different passphrases.
- [ ] **`carryover encrypt` migration command.** Converts an existing ledger to encrypted format.

### Key Deliverables

| Deliverable | Verification |
|---|---|
| Existing ledger migrates without data loss | Migration test on 30-day fixture |
| Encrypted ledger remains searchable via `carryover history` (with passphrase) | Integration test |
| Wrong passphrase fails closed (no plaintext leak in error messages) | Security test |

---

## v0.6 — Local metrics + observability

**Status:** PLANNED
**Delivers to:** Anyone who wants to understand their own usage. (Not analytics for marketing — telemetry stays opt-in and never leaves the machine.)
**Theme:** "How much of my time has Carryover saved me?"

### Features

- [ ] **Local metrics.** Sessions captured per tool, distill latency, hook-fire frequency, restore success rate. Stored in the same SQLite, separate table.
- [ ] **`carryover stats` command.** ASCII-rendered weekly + monthly summaries.
- [ ] **Optional dashboard.** Static HTML at `~/.carryover/dashboard.html`, generated by daemon. Uses the same color palette as the brand. Offline-only.
- [ ] **Telemetry opt-in.** If user enables, anonymous aggregate counters can be uploaded to a community dashboard. Off by default. Clear consent flow.

### Key Deliverables

| Deliverable | Verification |
|---|---|
| `carryover stats` shows accurate counts for a synthetic 30-day fixture | Test |
| Telemetry off by default; the daemon never reaches the network without explicit opt-in | Network policy test (firewalled CI runner) |

---

## v0.7 — Integration contract + community adapters

**Status:** PLANNED
**Delivers to:** Community contributors adding new agents (Claude.ai web, Hermes, OpenClaw, etc.). Plus: the maintainer, who stops being the only person who can add a tool.
**Theme:** Carryover scales beyond the v0.1–v0.2 hand-coded six.

### Features

- [ ] **Documented adapter interface.** Public Rust trait + a stable JSON-over-stdin protocol for non-Rust adapters.
- [ ] **Adapter loader.** Daemon loads third-party adapters from `~/.carryover/adapters/*/` at startup. Sandboxed (each adapter runs as a subprocess with restricted permissions).
- [ ] **Adapter cookbook.** Step-by-step doc with a worked example (e.g., "writing a Cline adapter").
- [ ] **Adapter test harness.** Generic fixture runner — third-party adapters run against the same correctness tests as the built-in ones.
- [ ] **Reference third-party adapter.** One community-built adapter (selected from RFC issue) that lands in v0.7 to validate the contract.

### Key Deliverables

| Deliverable | Verification |
|---|---|
| Reference adapter from a non-maintainer ships with full test coverage | PR merged, CI green |
| Daemon stays stable when an adapter crashes | Chaos test: panic the third-party adapter, daemon survives |

---

## v1.0 — Production-ready (the milestone)

**Status:** PLANNED
**Delivers to:** All stakeholders. Anyone who's been waiting for "is this safe to depend on yet?"
**Theme:** Stable contract. Documented commitments. Carryover is now boring infrastructure that disappears into the user's workflow.

### Features

- [ ] **Stable adapter contract** — semver-locked. Breaking changes require a major version.
- [ ] **`ToolSpec` for v0.1-trio is current within 7 days of any upstream change** (the maintenance promise made in `CONTRIBUTING.md`).
- [ ] **All v0.x features stable, all tests green** for at least 30 days on `main`.
- [ ] **macOS, Linux, Windows verified** by CI on every PR.
- [ ] **Signed binaries** via `cosign` or equivalent. Notarized for macOS distribution outside of the App Store.
- [ ] **Homebrew core eligibility** — submit PR to homebrew-core. May or may not land; submission itself is the deliverable.
- [ ] **Apt + RPM packages** for Linux distributions (eligibility-based).
- [ ] **Documented integration contract used by ≥3 community adapters.**
- [ ] **Public security audit** — at least one external reviewer signs off on the daemon + hook endpoint surface area.
- [ ] **README + ARCHITECTURE + ROADMAP all aligned** to the shipped product. Documentation no longer says "alpha — design docs only."

### Key Deliverables

| Deliverable | Verification |
|---|---|
| `brew install carryover` (no tap prefix) works on macOS | If accepted into homebrew-core; otherwise document as v1.1 work |
| Three independent users (no relationship to maintainer) testify Carryover has saved them measurable time | Public testimonials + handoff fixtures from each |
| Zero P0/P1 bugs open at v1.0 tag | GitHub issues filter |
| Founder's sister (the v0.1 named user) installs unassisted, uses for a week, reports usage delta | Documented walk-through |

---

## v1.0+ Future

- **Cross-machine sync (opt-in, encrypted, peer-to-peer).** No central server. Walks the line of "local-first" — only ships if community asks.
- **AI-side awareness.** Eventually upstream tools (Claude Code, Cursor, etc.) build hooks specifically for Carryover. We don't ship that — we collaborate.
- **Plugin marketplace.** Vetted community adapters with reputation system. Only viable if v0.7 contract proves stable.
- **Domain extractors as first-class.** Domain-specific extractor packs (research, fundraising, technical writing). Out-of-scope until clear demand.
- **Carryover-the-protocol vs Carryover-the-daemon.** If multiple implementations emerge (someone writes a Go version), publish a protocol spec.

---

## Release Process

1. All features for the version checked off in this file.
2. All tests in `TESTING_AND_VALIDATION.md` for that version pass on macOS + Linux + Windows (or marked deferred with rationale).
3. PR to `main` containing a `CHANGELOG.md` entry, README updates if user-visible.
4. Branch protection requires 1 review + green CI.
5. Tag merged commit (`vX.Y.Z`) — release workflow auto-builds binaries, publishes to GitHub Release.
6. Homebrew tap formula updated (PR to `carryover-dev/homebrew-tap`).
7. npm + crates.io publish (matching version).
8. Monitor for 24h via real users + `carryover status`. If clean, announce on the project's X handle (`@carryoverhq`).
9. Update memory + open the next version's tracking issue.

---

## Cross-references

- Problem framing + thesis: [VISION.md](./VISION.md)
- Technical architecture (every component named in this roadmap): [ARCHITECTURE.md](./ARCHITECTURE.md)
- High-level milestone summary (the public-friendly one-pager): [ROADMAP.md](./ROADMAP.md)
- Verification harness: `scripts/detect_paths.sh` (added in v0.1; currently lives in the internal carryover-context bundle)
- Test matrix: [TESTING_AND_VALIDATION.md](./TESTING_AND_VALIDATION.md) (added in v0.1)
- Contribution + maintenance procedure: [CONTRIBUTING.md](./CONTRIBUTING.md)
- Community rules: [CODE_OF_CONDUCT.md](./CODE_OF_CONDUCT.md)
- Disclosure: [SECURITY.md](./SECURITY.md)
