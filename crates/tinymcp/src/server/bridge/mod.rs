//! Opaque sessions and bounded host callbacks for the compiled-module boundary.
//!
//! A session permits one active input line. The host observes callbacks, applies
//! its security policy, and replies; the existing protocol builds the MCP reply.

mod handler;
mod operations;

pub use operations::ServerSessions;
pub(super) const MAX_BYTES: usize = 1_048_576;

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
