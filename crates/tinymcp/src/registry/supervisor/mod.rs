//! Keeping installed servers connected.
//!
//! # The problem it exists for
//!
//! MCP transports drop silently over a long-running deployment: a subprocess
//! exits, an HTTP session expires. Connections are established once, at
//! startup, and nothing re-establishes them. After a few hours a deployment
//! ends up with no MCP tools at all and no way back short of a restart — and
//! nothing announces that, because from the map's point of view everything is
//! still connected.
//!
//! # What it does
//!
//! Every tick it walks the enabled installs and, for each:
//!
//! - if it is in the map, **probes** it, because membership alone is not
//!   evidence — a dropped transport looks identical to a live one until
//!   something asks it a question;
//! - if the probe fails, disconnects the dead transport and reconnects;
//! - if it is not connected, reconnects, subject to per-server backoff so a
//!   genuinely broken server is not hammered every minute forever.
//!
//! # What it deliberately does not do
//!
//! It publishes no health signal. A supervisor that reported a single
//! unreachable MCP server as a process-level health failure would take a whole
//! deployment out of rotation because one optional integration was down. It
//! logs, and it keeps trying.

mod backoff;
mod types;

pub use types::{Supervisor, SupervisorConfig};

#[cfg(test)]
mod test;
