//! The `TinyBus` module: the interface, its state, and the ABI exports.
//!
//! This adapter is the only thing in the crate that knows about `TinyBus`.
//! Everything it serves is the ordinary library API underneath, so the crate
//! stays usable as a plain dependency — which is what lets a host consume it as
//! a path dependency first and a loadable module later without the code in
//! between changing.
//!
//! # The names come from the contract
//!
//! Every member name, the interface, and the object path are spelled once, in
//! [`tinymcp_bus::names`]. The manifest below repeats them because the macro
//! needs literals, and a test asserts the two agree — a member served but not
//! declared is invisible to a host, and one declared but not served is an
//! unknown-method failure at the worst possible moment.
//!
//! # Arguments are positional
//!
//! `TinyBus` decodes a member's arguments from a JSON array by position. That
//! makes the *order* of a method's parameters part of the contract, not just
//! their types: swapping two parameters of the same type is a silent breaking
//! change. Each signature below matches the order documented on its member.

mod config;
mod service;

pub use config::ModuleConfig;
pub use service::{McpService, ServerDetail};

use tinybus::{Connection, Result as TinyBusResult};
use tinymcp_bus::names;

/// Builds the service and serves it.
///
/// A failure here fails the load. That is deliberate: a module that came up
/// without its store or without a working HTTP client would answer every call
/// with the same error, and failing at load says so once rather than on every
/// request afterwards.
async fn setup(connection: Connection, config: ModuleConfig) -> TinyBusResult<()> {
    let service = McpService::new(config)
        .map_err(|error| tinybus::Error::failed(format!("tinymcp could not start: {error}")))?;

    connection
        .serve_at(names::OBJECT_PATH.try_into()?, service)
        .await?;
    connection.request_name(names::INTERFACE).await?;

    Ok(())
}

tinybus_module::module_export! {
    setup = setup,
    config = ModuleConfig,
    // More than one, because a tool call on one server must not wait behind a
    // slow call on another: these are third-party endpoints and subprocesses,
    // and one of them being slow is routine.
    worker_threads = 4,
    provides = ["ai.tinyhumans.tinymcp.Mcp"],
    methods = [
        "RegistrySearch",
        "RegistryGet",
        "RegistrySettingsGet",
        "RegistrySettingsSet",
        "InstalledList",
        "Install",
        "Uninstall",
        "SetEnabled",
        "UpdateEnv",
        "Connect",
        "Disconnect",
        "Status",
        "DetectAuth",
        "OAuthBegin",
        "ListTools",
        "ToolCall",
        "ConfigAssist",
        "SetupSearch",
        "SetupGet",
        "SetupRequestSecret",
        "SetupSubmitSecret",
        "SetupTestConnection",
        "SetupInstallAndConnect",
        "StaticList",
        "StaticListTools",
        "StaticCallTool",
        "AuditRecordWrite",
        "AuditListWrites",
    ],
    signals = [],
    requires = [],
    optional = [],
    // Not lazy: a host that loaded this module wants its servers connected, and
    // deferring the load would defer that until the first call — by which point
    // an agent has already been told it has no tools.
    lazy = false,
}

#[cfg(test)]
mod test;
