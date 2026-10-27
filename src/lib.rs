//! Syntax-aware duplicate code detection for coding agents.
//!
//! Files are parsed with tree-sitter, every node is hashed with names and
//! literals anonymised, and equal hashes become clone groups. Each group
//! reports what varies between its occurrences so an agent can extract a
//! helper without re-deriving the parameters itself.

pub mod baseline;
pub mod detect;
pub mod engine;
mod git;
mod hash;
pub mod index;
pub mod lang;
pub mod mcp;
pub mod near;
pub mod report;
mod suffix;
pub mod variation;
pub mod walk;
