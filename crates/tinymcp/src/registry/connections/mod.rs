//! The live connections to installed servers.
//!
//! [`Connections`] owns what is currently running: one entry per connected
//! install, with the tools it advertised cached beside it, plus the most recent
//! failure for anything that could not connect.
//!
//! # Owned, not global
//!
//! The map is a field, not a process-wide static. Before the extraction it was
//! a `OnceLock`, which meant two hosts in one process would have silently shared
//! connections and every test ran against the same map in whatever order the
//! runner chose. A host constructs one of these and holds it.
//!
//! # Being in the map is not the same as being usable
//!
//! An MCP transport can drop silently — a subprocess exits, an HTTP session
//! expires — while its entry sits in the map looking fine. [`Connections::is_connected`]
//! answers "is there an entry"; [`Connections::probe_alive`] answers "does it
//! still respond", which is what the supervisor needs in order to notice a dead
//! transport before a user's next tool call does.
//!
//! # A failure is recorded as one record under one lock
//!
//! The message and the authentication classification are written together. Two
//! separate maps would let a status read land between the two writes and report
//! a fresh message with a stale reason — which is how a user ends up being told
//! to fix a token on a server that wants a browser sign-in.
//!
//! # What a 401 does and does not surface
//!
//! A server that answers 401 is reachable and wants credentials, so it gets its
//! own status rather than being reported as broken. The *reason* crosses as a
//! stable code; the raw 401 body and the OAuth metadata URL do not. Those carry
//! details of a server's authorization setup that no caller needs in order to
//! offer the right affordance.

mod dial;
mod status;
mod types;

/// Building request credentials from stored values, shared with the setup
/// flow's connection test so a test dials exactly as a real connect would.
pub(crate) use dial::build_http_auth;

pub use types::Connections;

#[cfg(test)]
mod test;
