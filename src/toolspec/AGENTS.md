# AGENTS.md — toolspec

## Purpose
Define the per-tool version table and resolve the correct `HookSet` for a detected tool version, including the newer-than-known fallback (decision D2).

## Architectural decisions that constrain this module
- D2 (decisions.md): newer-than-known version → use highest-known `HookSet` + emit structured warning event to `~/.carryover/events.jsonl`. Install succeeds; `carryover refresh` re-checks.
- D2: older-than-known version → refuse install with a clear error message. No best-effort fallback.
- Locked types (decisions.md D2): `VersionRange` (min inclusive, max exclusive/None), `HookSet` (session_start, session_end, pre_compact, user_prompt_submit), `PathSpec` (linux/macos/windows fields), `ToolSpec`, `FallbackKind` (Exact | NewerThanKnown | OlderThanKnown).
- `resolve_hookset()` iterates highest→lowest; returns `(HookSet, FallbackKind)`.
- semver 1 crate used for `VersionRange` bounds.
- D3 (decisions.md): error type is `ToolSpecError` (thiserror). No anyhow in this module.

## Files
- `mod.rs` — type stubs for `VersionRange`, `HookSet`, `PathSpec`, `ToolSpec`, `FallbackKind`; `resolve_hookset()` stub lands in Phase 2 (TASK-04)

## Out of scope for v0.1
- Windows hook paths (`PathSpec::windows` field reserved but unused)
- Dynamic / runtime-loaded tool specs
- Telemetry on fallback events (Architecture Principle #3 — never)
