//! Everything a host can ask the dynamic registry to do.
//!
//! [`McpRegistry`] is the one object a host holds. It owns the store, the live
//! connections, the catalogs, the authorization flow, and the setup vault, and
//! its methods are the operations — browse, install, connect, call, configure.
//!
//! # Why a facade rather than free functions
//!
//! Each operation needs several of those pieces, and the pieces need each other
//! in a fixed order — refresh a token, then read credentials, then dial. Free
//! functions would mean every caller assembling that themselves, and every
//! caller getting a chance to assemble it differently.
//!
//! # What it returns
//!
//! Typed replies from [`tinymcp_bus::method`], not a generic envelope. A
//! failure is a failure; these describe what a successful call produced.
//!
//! # What it does not do
//!
//! **No events.** The operations report what happened in their return value. A
//! host that wants to publish an event, write an audit row, or update a user
//! interface does so from the result — it knows its own vocabulary, and this
//! module would only be guessing at it.
//!
//! **No model turns.** [`McpRegistry::config_assist`] gathers what a model
//! would need to help a user configure a server and stops there. Running the
//! turn is the host's: it owns the model, the budget, and the conversation.

mod install;
mod types;

pub use install::{build_install_transport, collect_required_env_keys, pick_connection};
pub use types::McpRegistry;

#[cfg(test)]
mod test;
