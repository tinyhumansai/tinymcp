//! Stateful helpers for the host's configured-client lifecycle.
//!
//! These types guide a host through `TinyBus` `OpenConfigured`, `Close`, and
//! `TransitionStatus` calls. They perform no bus I/O themselves.

mod machine;
mod table;

pub use machine::{ConfiguredLifecycle, Fault, OpenObject, Poll, Step};
pub use table::{AdmitError, CleanupAction, CleanupReservation, ClientTable, Retire};

#[cfg(test)]
#[path = "machine_tests.rs"]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod machine_tests;
#[cfg(test)]
#[path = "table_tests.rs"]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod table_tests;
