//! `mcpify` internals exposed as a library so the `tests/` integration harness
//! can drive the MCP server/reload paths directly. This is an internal facade —
//! there is no external consumer of the library crate; the shipped artifact is
//! the `mcpify` binary.

pub mod adapters;
pub mod cli;
pub mod config;
pub mod errors;
pub mod mcp;
pub mod observability;
pub mod runtime;
pub mod supervisor;
pub mod template;
