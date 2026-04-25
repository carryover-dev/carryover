# Contributing to Carryover

Thank you for considering a contribution to Carryover. This project exists because AI agents lose state across session boundaries and tool switches — and the work users do with them spans code, research, founder work, and long-form writing — yet the cleanest fix is the same in every case: do the bookkeeping locally so the agent never has to. If you want to help make that real — whether by writing code, filing a thoughtful bug, improving documentation, or testing on a platform we have not yet covered — you are welcome here.

This document describes how to contribute, what to expect from the maintainer, and what is in and out of scope at this stage.

## Maintainer commitment

- I spend ~5 hours/week on this.
- The maintainer responds to new issues and pull requests within seven days. If you have not heard back in that window, please reply on the thread to bump it — it is not deliberate silence, it is bandwidth.
- All decisions and discussion happen in public on GitHub. Private decisions, when they happen, are summarized back into the relevant public thread.

## Ground rules

- Be kind. The [Code of Conduct](./CODE_OF_CONDUCT.md) applies to every space connected to this project.
- Open an issue before opening a non-trivial pull request. Drive-by PRs that change architecture or add new tool integrations without prior discussion will be closed warmly with a pointer back here.
- Carryover is a contextless-design project. Any proposal that requires the agent to do bookkeeping inside its own context window is out of scope by construction.

## Scope

### In scope for v0.1

- The `carryoverd` daemon: filesystem watching, local hook endpoint on `localhost:47823`, SQLite ledger, normalizer, distiller, publisher.
- Interactive `carryover install` (single tool-selection picker, auto-resolves config paths and hook event names per detected tool version).
- `carryover refresh` for re-detection on tool upgrades.
- The `ToolSpec` table — per-tool, per-version hook event names, config-file path candidates, and transcript-path candidates for Claude Code, Cursor, and Codex.
- Privacy split publisher (payload to gitignored `.carryover/handoff.md`, static pointer to `AGENTS.md` + `CLAUDE.md`).
- Documentation, tests, packaging, and install ergonomics for the above.

### Ongoing maintenance — keep `ToolSpec` current

When any supported tool ships a new version that changes hook event names, config-file paths, or transcript-path layouts, Carryover ships a release with an updated `ToolSpec` for that tool. This is the project's continuous job — and it's what justifies the value proposition: contributors and users do not have to track hook-name drift across six tools, the `ToolSpec` table does. Procedure for adding a new version entry:

1. Run `~/workspace/carryover-context/detect_paths.sh` (or the equivalent harness) on a machine with the new tool version installed.
2. Capture observed hook names from the tool's release notes or by triggering a session.
3. Add a `(VersionRange, HookSet)` entry to the appropriate `ToolSpec.hooks_by_version` array.
4. Add a corresponding test against a fixture transcript / config file.
5. Cite the upstream change (release notes URL) in the commit message.

### Deferred to v0.2

- Copilot, Windsurf, and Aider integrations. Issues, design discussion, and exploratory work are welcome now; merged production code for these tools waits until v0.1 ships.

### Out of scope

- Anything that asks the agent to summarize, save, or restore its own state inside a prompt.
- Cloud-hosted ledgers or remote dependencies. Carryover is local-first by design.
- A general-purpose agent memory system. Carryover captures and restores task state across sessions; long-term memory is a different problem.

## Filing a bug

Use the [bug report template](./.github/ISSUE_TEMPLATE/bug_report.md). The most useful bugs include the tool and version, the host OS, the relevant transcript path (sanitized), and a minimal reproduction.

## Suggesting a feature

Use the [feature request template](./.github/ISSUE_TEMPLATE/feature_request.md). Please open an issue and reach agreement before sending a pull request — this is for your protection as much as the project's.

## Development environment

Carryover is pre-code at the time of writing. The intended toolchain is:

- Rust stable (the version pinned in `rust-toolchain.toml` once shipped).
- `cargo` for build and test.
- SQLite available on the host.
- A supported AI agent installed for end-to-end testing.

Setup, build, and test commands will be filled in here once the daemon lands. Track [`ROADMAP.md`](./ROADMAP.md) for status.

## Pull request process

1. Open or claim an issue first.
2. Fork the repository and create a topic branch off `main`.
3. Make your change. Keep commits focused and reviewable.
4. Add or update tests. The CI pipeline will block merges on red tests once it is wired up.
5. Update [`CHANGELOG.md`](./CHANGELOG.md) under the `[Unreleased]` section.
6. Open a pull request using the [pull request template](./.github/PULL_REQUEST_TEMPLATE.md).
7. The maintainer will review within seven days.

## Commit convention

This project uses [Conventional Commits](https://www.conventionalcommits.org/). Examples:

- `feat(daemon): add localhost:47823 hook endpoint`
- `fix(distiller): handle empty Cursor SQLite rows`
- `docs(readme): clarify install fallback path`

This convention drives changelog generation and semantic versioning once `semantic-release` is wired up.

## Your first contribution

If you are new here, look for issues labeled [`good first issue`](https://github.com/rohitsux/carryover/labels/good%20first%20issue) or [`first timers only`](https://github.com/rohitsux/carryover/labels/first%20timers%20only). These are scoped so you can land a meaningful change in an evening.

Documentation contributions count. So do test cases against new tool versions. So does saying "the install instructions did not work for me on this OS."

## Code of Conduct

Participation in this project is governed by the [Code of Conduct](./CODE_OF_CONDUCT.md). Reports go to `contact@voura.app`.
