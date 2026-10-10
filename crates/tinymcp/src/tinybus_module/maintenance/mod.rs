//! The background work a loaded module owns: the boot connect pass and the
//! reconnect supervisor.
//!
//! # Why the module runs these and a host does not
//!
//! A host that loaded this module wants its installed servers connected before
//! an agent first asks for a tool, and wants a dropped connection restored
//! without anyone noticing it dropped. Both need state only the module has: the
//! supervisor's per-server backoff and timeout streaks, and a liveness probe
//! that is a real round trip rather than a look at the connection map. A host
//! driving this over `Status` polling would be reimplementing it worse, so
//! neither is a member. They start when the service is built and stop when it
//! is dropped.
//!
//! # Loading never waits on connecting
//!
//! [`Maintenance::start`] spawns and returns. Connecting is a subprocess spawn
//! or a network handshake per server, and a module whose `setup` waited on them
//! would make a broken third-party endpoint a load failure. The pass runs in
//! the background; [`Maintenance::booted`] says when it has finished, for a
//! caller — chiefly a test — that needs to know.
//!
//! # Determinism
//!
//! Where the supervisor's cycles come from is a [`Pacing`]. Production uses
//! [`Pacing::Interval`]; a test uses [`Pacing::Driven`] and sends each cycle
//! itself, so nothing waits on a wall clock.

mod events;
mod types;

pub(super) use types::Maintenance;

#[cfg(test)]
#[path = "mod_tests.rs"]
mod test;
