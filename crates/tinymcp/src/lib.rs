//! A production-ready starting point for an installable `TinyBus` module.
//!
//! This crate is a template. It ships the layout, lint configuration, error
//! handling, testing, and documentation conventions described in `AGENTS.md`.
//! The compiled `cdylib` exports `TinyBus` module ABI v1 and serves the example
//! [`greet`] behavior over the bus.
//!
//! # Layout
//!
//! This is the implementation half of a two-crate workspace:
//!
//! - [`tinymcp_bus`] — the wire contract. Member names, payload types, and the
//!   contract version, with no transport and no behavior. A host that only
//!   makes calls depends on that crate alone.
//! - `template` — this crate. The behavior, the crate-wide error type, and the
//!   `TinyBus` adapter that serves them, built as both an `rlib` and the
//!   `cdylib` the loader consumes.
//!
//! Within this crate:
//!
//! - `src/error/` holds the crate-wide [`Error`] enum and the [`Result`] alias
//!   returned by every fallible public function.
//! - Each feature area lives in its own module directory with a `mod.rs`
//!   module root, an optional `types.rs`, and a `test.rs` holding its unit
//!   tests.
//! - Every public item is re-exported from here — including all of
//!   [`tinymcp_bus`] — so downstream users have a single predictable surface
//!   and `tinymcp::GreetRequest` is the *same type* as
//!   `tinymcp_bus::GreetRequest`, not a structural twin.
//! - `tinybus_module` adapts the public behavior to `TinyBus` and exports the
//!   module descriptor, embedded manifest, and initialization entrypoint.
//!
//! # Example
//!
//! ```
//! use tinymcp::{greet, Error, GreetRequest};
//!
//! assert_eq!(greet("Ferris")?, "Hello, Ferris!");
//! assert_eq!(greet("   ").unwrap_err(), Error::EmptyName);
//! assert_eq!(GreetRequest::new("Ferris").name, "Ferris");
//! # Ok::<(), tinymcp::Error>(())
//! ```
//!
//! Replace the `greeting` module with the first real feature area, keep the
//! conventions, and update this documentation to describe the new crate.

mod error;
mod greeting;
mod tinybus_module;

pub use error::{Error, Result};
pub use greeting::greet;

// The wire contract, re-exported by module rather than by item so every path
// through this crate resolves to the same definitions the contract crate
// publishes. A host may depend on `tinymcp-bus` directly and get exactly these
// types; nothing here redefines them.
pub use tinymcp_bus;
pub use tinymcp_bus::{
    CONTRACT_VERSION, GreetRequest, GreetResponse, INTERFACE, METHODS, OBJECT_PATH, is_compatible,
    names, version,
};
