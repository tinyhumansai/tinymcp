//! The servers a user installs at runtime.
//!
//! This is the *dynamic* half of MCP client support: a user browses the
//! upstream registries, installs what they want, and the choice is remembered
//! across restarts. The *static* half — the set a host pins in its own
//! configuration — is [`crate::config_servers`].
//!
//! Both share the transports underneath. What is here and not there is
//! everything that follows from a user's choice outliving the process: a store,
//! credentials at rest, and a supervisor for what got spawned.

pub mod curation;
pub mod oauth;
pub mod store;

pub use oauth::{AuthDetection, OAuthFlow};
pub use store::Store;
