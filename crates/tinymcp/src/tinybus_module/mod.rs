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
mod directories;
mod maintenance;
#[allow(clippy::unused_async)]
mod service;

pub use config::ModuleConfig;
pub use service::McpService;

use tinybus::{Connection, Result as TinyBusResult};
use tinymcp_bus::names;

use crate::registry::SupervisorConfig;

/// Builds the service and serves it.
///
/// A failure here fails the load. That is deliberate: a module that came up
/// without its store or without a working HTTP client would answer every call
/// with the same error, and failing at load says so once rather than on every
/// request afterwards.
///
/// Connecting is the opposite: the boot pass and the reconnect supervisor start
/// here as background work and `setup` does not wait for them, so a server that
/// is down delays nothing. See [`maintenance`].
pub(super) async fn setup(connection: Connection, config: ModuleConfig) -> TinyBusResult<()> {
    let supervisor = SupervisorConfig::default();
    let service = McpService::new(&config)
        .map_err(|error| tinybus::Error::failed(format!("tinymcp could not start: {error}")))?
        .with_maintenance(supervisor.clone())
        .await
        .with_opener(connection.clone(), &config, supervisor)
        .map_err(|error| tinybus::Error::failed(format!("tinymcp could not start: {error}")))?;

    connection
        .serve_at(names::OBJECT_PATH.try_into()?, service)
        .await?;
    connection.request_name(names::INTERFACE).await?;

    Ok(())
}

macro_rules! export_module {
    ($($declaration:tt)*) => {
        #[cfg(feature = "static-link")]
        mod linked_exports {
            // TinyBus generates these three ABI items without rustdoc.
            #![expect(missing_docs, reason = "generated TinyBus ABI entries")]
            use super::*;
            tinybus_module::module_export_static! { $($declaration)* }
        }
        #[cfg(feature = "static-link")]
        pub use linked_exports::{
            TINYBUS_MODULE_ABI_V1, tinybus_module_init_v1, tinybus_module_manifest_v1,
        };
        #[cfg(not(feature = "static-link"))]
        mod dynamic_exports {
            use super::*;
            tinybus_module::module_export! { $($declaration)* }
        }
    };
}

export_module! {
    setup = setup,
    config = ModuleConfig,
    // More than one, because a tool call on one server must not wait behind a
    // slow call on another: these are third-party endpoints and subprocesses,
    // and one of them being slow is routine.
    worker_threads = 4,
    provides = ["ai.tinyhumans.tinymcp.Mcp"],
    methods = [
        "RegistrySearch",
        "RegistrySearchCurated",
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
        "ConnectedOverview",
        "DetectAuth",
        "OAuthBegin",
        "OAuthComplete",
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
        "DrainSupervisorEvents",
        "Open",
    ],
    signals = [],
    requires = [],
    optional = [],
    // Not lazy: a host that loaded this module wants its servers connected, and
    // deferring the load would defer that until the first call — by which point
    // an agent has already been told it has no tools. `setup` starts the boot
    // connect pass and the supervisor for exactly that reason.
    lazy = false,
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod test;
