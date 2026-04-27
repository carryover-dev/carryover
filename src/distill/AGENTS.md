# AGENTS.md — distill

## Purpose
Run the 6-extractor distillation pipeline over `LedgerRow` records and produce a ≤50-line handoff summary.

## Architectural decisions that constrain this module
- ARCHITECTURE.md: 3 always-on extractors (task, open_questions, next_action) + 3 coding-only (recent_files, failed_approaches, git_context). The split is fixed for v0.1.
- D3 (decisions.md): error type is `DistillError` (thiserror). No anyhow in this module.
- 50-line hard cap on extractor output — enforced at the pipeline level, not per-extractor.
- Zero LLM token consumption: all extraction is rule-based regex/heuristic, not model inference.

## Files
- `mod.rs` — pipeline entry point stub; individual extractor modules land in Phase 2 (TASK-12–TASK-17)

## Out of scope for v0.1
- LLM-assisted summarization
- User-configurable extractor weights
- Streaming / incremental distillation
