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

pub mod boot;
pub mod config_doc;
pub mod connections;
pub mod curation;
pub mod oauth;
pub mod ops;
pub mod setup;
pub mod sources;
pub mod store;
pub mod supervisor;

pub use boot::{BootOutcome, connect_installed_servers};
pub use config_doc::{AppliedServer, ConfigApplyReport};
pub use connections::{Connections, ProbeOutcome, REMOTE_REQUEST_TIMEOUT};
pub use oauth::{
    AuthDetection, AuthKind, DEFAULT_CLIENT_NAME, OAuthBundle, OAuthCredentialStore, OAuthFlow,
};
pub use ops::McpRegistry;
pub use setup::{SecretRef, SecretVault};
pub use sources::{
    Registries, RegistryIndexSettings, RegistryOperation, RegistrySource, RegistryTimeouts,
};
pub use store::Store;
pub use supervisor::{
    ServerRef, SupervisedHost, Supervisor, SupervisorConfig, SupervisorEvent, TickReport,
};

mod command_kind;
mod config_redaction;
mod transport_kind;
pub use command_kind::CommandKindExt;
pub use config_redaction::McpRegistryAuthConfigExt;
pub use transport_kind::TransportExt;
#[cfg(test)]
#[path = "config_tests.rs"]
mod config_test;
#[cfg(test)]
#[path = "payload_tests.rs"]
mod payload_test;
