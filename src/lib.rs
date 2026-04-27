//! Carryover — zero-LLM-token context-handoff daemon.
//! See ARCHITECTURE.md for system design.

pub mod adapters;
pub mod cli;
pub mod daemon;
pub mod distill;
pub mod publish;
pub mod storage;
pub mod toolspec;
