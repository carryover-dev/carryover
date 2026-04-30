# AGENTS.md — Carryover

This is the public Carryover repo. Phase 1 v0.1 scaffold.

## How to read this codebase
1. README.md — what Carryover is + install commands
2. ARCHITECTURE.md — system design (L1/L2/L3)
3. RELEASE_ROADMAP.md — v0.1 scope + checklist
4. src/lib.rs — module tree
5. Per-subsystem AGENTS.md in src/<subsystem>/

## Module tree
- `src/adapters/` — Claude / Cursor / Codex transcript readers
- `src/cli/`      — clap CLI surface (install/refresh/status/start/stop/uninstall)
- `src/daemon/`   — daemon lifecycle (hook endpoint + fs watcher + pipeline worker)
- `src/distill/`  — 6-extractor distillation pipeline → 50-line handoff
- `src/publish/`  — dual-write publisher with privacy split
- `src/storage/`  — SQLite ledger (append-only, WAL)
- `src/toolspec/` — per-tool version table (hook events + paths)

## Build
```sh
cargo check                                      # must pass
cargo build --release                            # produces ./target/release/carryoverd
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

<!--CARRYOVER:START-->
Carryover is active. Before responding:
1. Read `.carryover/handoff.md` for the full session context and progress log.
2. Find the `## What to do next` section and read it aloud to the user in 1-2 sentences.
3. Ask the user if they want to continue from there or do something different — do not assume continuation.
<!--CARRYOVER:END-->
