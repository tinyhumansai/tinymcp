//! Bringing installed servers up at startup.
//!
//! Every enabled install is connected as the host comes up, so a user's tools
//! are there before they ask for one rather than after the first call pays for
//! a handshake.
//!
//! # Failures never block startup
//!
//! A server that cannot connect is logged and skipped. One misbehaving
//! integration must not stop a host from starting — and MCP servers are
//! third-party subprocesses and third-party endpoints, so one of them being
//! broken is the expected case rather than the exceptional one. The supervisor
//! picks them up afterwards.
//!
//! # Connects overlap
//!
//! Each connect spawns a subprocess or dials an endpoint and then waits on a
//! handshake, so doing them one at a time makes startup cost the *sum* of every
//! server's warm-up. They run concurrently, but bounded: a user with dozens of
//! installed servers should not fork all of them at once.

mod types;

pub use types::{BOOT_CONCURRENCY, BootOutcome, connect_installed_servers};

#[cfg(test)]
mod test;
