//! Collecting credentials during setup without them entering a model's context.
//!
//! # The problem
//!
//! A setup flow driven by a model has to ask the user for an API key. The model
//! must know that a key is *needed*, and must be able to hand it to a
//! connection test — but it must never see the key itself. Anything a model
//! sees is in a transcript, and a transcript is stored, sent onward, and
//! sometimes shown to someone else.
//!
//! # How it works
//!
//! The model never handles a value, only a handle to one.
//!
//! 1. The model asks for a secret by name. The vault mints an opaque
//!    [`SecretRef`] — `secret://<hex>` — and parks a waiter against it. The
//!    host prompts the user out of band.
//! 2. The user answers. The host submits the value against the handle, which
//!    wakes the waiter.
//! 3. The model passes the *handle* to a connection test or an install. The
//!    vault resolves handles to values at the last moment, inside the operation
//!    that needs them.
//!
//! The raw value crosses the model-facing surface at no point in that sequence.
//!
//! # Nothing here is persisted
//!
//! The vault is in memory and dies with the process. A value only becomes
//! durable when an install commits it to the credential store, which is the
//! step the user explicitly asked for.
//!
//! # Two lifetimes bound it
//!
//! An unanswered request gives up after [`REQUEST_TIMEOUT`], because a model
//! waiting forever on a prompt the user closed is a hung conversation. An
//! answered-but-unused value is swept after [`IDLE_TTL`], which is long enough
//! for a few connection-test retries and short enough that an abandoned setup
//! does not leave a credential sitting in memory.

mod types;

pub use types::{IDLE_TTL, REQUEST_TIMEOUT, SecretRef, SecretVault};

#[cfg(test)]
mod test;
