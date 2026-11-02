//! Per-transport stream parsers normalizing agent stdout lines into events.

pub mod claude;
pub mod codex;
pub mod opencode;
pub use codex::CodexParser;
