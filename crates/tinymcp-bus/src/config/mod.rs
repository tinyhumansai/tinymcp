//! What a host tells the module about the servers it should be able to reach.
//!
//! [`McpClientConfig`] is the root: a statically declared server set, the
//! identity to present during the `initialize` handshake, credentials for the
//! registry browse APIs, and an already-resolved proxy.
//!
//! # This is an argument, not a settings file
//!
//! The implementation never reads TOML, never consults an environment for the
//! server set, and never has an opinion about where a host keeps its
//! configuration. Everything it needs arrives through these types. That is what
//! makes the module testable without a filesystem and what keeps a host free to
//! source its configuration however it likes.
//!
//! The one deliberate exception is [`McpRegistryAuthConfig`], whose fields fall
//! back to documented environment variables when unset — existing container
//! deployments set those variables and nothing else, and breaking them to
//! satisfy a principle would be a poor trade.
//!
//! # Optional `schemars`
//!
//! Every type here derives `schemars::JsonSchema` under the off-by-default
//! `schemars` feature, so a host that generates a settings schema for a user
//! interface can do it from these definitions rather than from a hand-kept
//! copy. The feature is off by default because a host that only makes calls
//! should not pay for it.

mod types;

pub use types::{
    HttpHeader, McpAuthConfig, McpClientConfig, McpClientIdentityConfig, McpProxyConfig,
    McpRegistryAuthConfig, McpServerConfig,
};

#[cfg(test)]
mod test;
