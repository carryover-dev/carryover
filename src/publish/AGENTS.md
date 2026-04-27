# AGENTS.md — publish

## Purpose
Dual-write the distilled handoff to `~/.carryover/handoff.md` and `<project>/.carryover/handoff.md`, and update the static pointer block in AGENTS.md + CLAUDE.md.

## Architectural decisions that constrain this module
- Privacy split (memory/feedback_carryover_privacy_split.md): handoff content lives in `.carryover/handoff.md` (gitignored). AGENTS.md and CLAUDE.md receive only the fixed pointer text — never the payload.
- Pointer marker bytes are locked: `<!--CARRYOVER:START-->` / `<!--CARRYOVER:END-->`. Do not change the marker strings.
- Dual-write rule: AGENTS.md + CLAUDE.md pointer blocks must be byte-identical.
- 50-line cap: publisher enforces the cap before writing (distill module is the source; publish is the gate).
- D3 (decisions.md): error type is `PublishError` (thiserror). No anyhow in this module.

## Files
- `mod.rs` — dual-write stub; file I/O and marker injection land in Phase 2 (TASK-20)

## Out of scope for v0.1
- Encrypted handoff storage (v0.5+)
- Remote / cloud publish targets
- Per-tool handoff customization
