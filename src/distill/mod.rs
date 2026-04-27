//! Distillation pipeline: 3 always-on extractors (task, open_questions, next_action)
//! + 3 coding-only (recent_files, failed_approaches, git_context).
//!
//! 50-line cap on output. No LLM tokens consumed; all extraction is rule-based.
//! See ARCHITECTURE.md for extractor contract and RELEASE_ROADMAP.md for v0.1 scope.
