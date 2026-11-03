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

#[cfg(test)]
pub(crate) fn test_service(config: &ModuleConfig) -> crate::Result<McpService> {
    McpService::with_provider(config, Arc::new(crate::storage::MemoryProvider::new()))
}

use std::sync::Arc;
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
    // A module that keeps state reaches the host's storage over the bus; the
    // host binds the scope and the grant. A host that linked the module into
    // its own process and installed a provider has already chosen. Failing
    // here fails the load, like any other missing dependency: falling back to
    // memory would quietly lose every install at the next restart.
    //
    // Directory operations (`Open`, `OpenConfigured`, `HasStoredState`) build
    // services over the installed provider whatever the load-time `data_dir`
    // was, so even a static-only module needs host storage. Missing storage is
    // a load failure; silently continuing in memory would lose state.
    let provider = provider_for_setup(
        connection.clone(),
        &config,
        crate::storage::provider_installed(),
    )
    .await?;
    if let Some(provider) = provider {
        crate::storage::install_provider(provider);
    }
    setup_over_installed_storage(connection, config).await
}

async fn provider_for_setup(
    connection: Connection,
    config: &ModuleConfig,
    provider_installed: bool,
) -> TinyBusResult<Option<Arc<dyn crate::storage::StorageProvider>>> {
    if provider_installed {
        Ok(None)
    } else {
        host_storage_for(connection, config).await
    }
}

/// The host-storage provider to install for `config`.
pub(super) async fn host_storage_for(
    connection: Connection,
    config: &ModuleConfig,
) -> TinyBusResult<Option<Arc<dyn crate::storage::StorageProvider>>> {
    match bus_provider(connection, config).await {
        Ok(provider) => Ok(Some(provider)),
        Err(error) => Err(error),
    }
}

/// The provider that reaches the host's storage over `connection`.
pub(super) async fn bus_provider(
    connection: Connection,
    config: &ModuleConfig,
) -> TinyBusResult<Arc<dyn crate::storage::StorageProvider>> {
    let destination = config
        .storage_callback
        .as_deref()
        .ok_or_else(|| tinybus::Error::failed("tinymcp has no host storage callback"))?;
    let destination = tinybus::BusName::new(destination)
        .map_err(|_| tinybus::Error::failed("tinymcp has an invalid host storage callback"))?;
    // A host that serves one storage service for every module names it
    // instead of a per-instance unique destination. The name is explicit and
    // exact: an absent destination still fails, and so does a service nobody
    // owns, so there is no fallback to memory.
    let provider = if destination.as_str() == crate::storage::STORAGE_SERVICE {
        crate::storage::BusProvider::connect_service(connection).await
    } else {
        crate::storage::BusProvider::connect_at(connection, destination).await
    }
    .map_err(|error| {
        tinybus::Error::failed(format!("tinymcp could not reach host storage: {error}"))
    })?;
    Ok(Arc::new(provider))
}

/// How `client`'s stdio servers start, given the bus `connection` to `TinyBox`.
///
/// A configured sandbox becomes a jailed launcher over `connection`. With no
/// sandbox configured, or none reachable, the result starts only
/// operator-trusted servers: the missing jail is a refusal, never a fallback.
pub(super) fn stdio_launchers(
    connection: Option<&Connection>,
    client: &tinymcp_bus::McpClientConfig,
) -> crate::transport::stdio::jailed::StdioLaunchers {
    use crate::transport::stdio::jailed::BusJail;
    stdio_launchers_for_bus(client, connection.map(BusJail::connect))
}

fn stdio_launchers_for_bus(
    client: &tinymcp_bus::McpClientConfig,
    bus: Option<Result<crate::transport::stdio::jailed::BusJail, tinybus::Error>>,
) -> crate::transport::stdio::jailed::StdioLaunchers {
    use crate::transport::stdio::jailed::{JailedSpawner, StdioLaunchers};
    let Some(sandbox) = client.stdio_sandbox.clone() else {
        return StdioLaunchers::new();
    };
    if let Some(Ok(bus)) = bus {
        return StdioLaunchers::sandboxed(Arc::new(JailedSpawner::new(Arc::new(bus), sandbox)));
    }
    tracing::warn!("a stdio sandbox is configured but the sandbox module is not reachable");
    StdioLaunchers::new()
}

/// [`setup`] over whatever storage provider is installed already.
pub(super) async fn setup_over_installed_storage(
    connection: Connection,
    config: ModuleConfig,
) -> TinyBusResult<()> {
    let supervisor = SupervisorConfig::default();
    let stdio = stdio_launchers(Some(&connection), &config.client);
    let service = Arc::new(
        McpService::new_with_stdio(&config, stdio)
            .map_err(|error| tinybus::Error::failed(format!("tinymcp could not start: {error}")))?
            .with_maintenance(supervisor.clone())
            .await
            .with_opener(connection.clone(), &config, supervisor)
            .map_err(|error| tinybus::Error::failed(format!("tinymcp could not start: {error}")))?,
    );
    let service = Arc::new(service);

    service.register_root();

    connection
        .serve_at(names::OBJECT_PATH.try_into()?, service.dispatch_interface())
        .await?;
    connection.request_name(names::INTERFACE).await?;

    Ok(())
}

macro_rules! export_module {
    ($($declaration:tt)*) => {
        #[cfg(any(feature = "static-link", test))]
        mod linked_exports {
            // TinyBus generates these three ABI items without rustdoc.
            #![cfg_attr(not(test), allow(missing_docs))]
            use super::*;
            tinybus_module::module_export_static! { $($declaration)* }
        }
        #[cfg(all(feature = "static-link", not(test)))]
        pub use linked_exports::{
            TINYBUS_MODULE_ABI_V1, linked_module, tinybus_module_init_v1,
            tinybus_module_manifest_v1,
        };
        #[cfg(test)]
        pub(crate) use linked_exports::linked_module as test_linked_module;
        #[cfg(not(any(feature = "static-link", test)))]
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
        "ConfigDocumentGet",
        "ConfigDocumentApply",
        "HasStoredState",
        "InstalledList",
        "Install",
        "Uninstall",
        "SetEnabled",
        "UpdateEnv",
        "Connect",
        "Disconnect",
        "Status",
        "ConnectedOverview",
        "CachedOverview",
        "DetectAuth",
        "OAuthBegin",
        "OAuthComplete",
        "ListTools",
        "ToolCall",
        "ReadResource",
        "ConfigAssist",
        "SetupSearch",
        "SetupGet",
        "SetupRequestSecret",
        "SetupSubmitSecret",
        "SetupTestConnection",
        "SetupInstallAndConnect",
        "StaticList",
        "StaticServerSummaries",
        "ConfiguredToolCall",
        "ValidateServerArguments",
        "StaticListTools",
        "StaticScrubTools",
        "StaticCachedListTools",
        "StaticCallTool",
        "StaticReadResource",
        "PrepareBridgeTools",
        "StaticCallToolClassified",
        "AuditRecordWrite",
        "AuditListWrites",
        "DrainSupervisorEvents",
        "ServerOpen",
        "ServerOpenHttp",
        "ServerSubmit",
        "ServerPoll",
        "ServerComplete",
        "ServerCancel",
        "ServerClose",
        "ServerShutdown",
        "ServerSessionMetadata",
        "ServerHttpSession",
        "TransformText",
        "NormalizeToolArguments",
        "DisplayRemoteTool",
        "RenderToolOutput",
        "PrepareAgentTools",
        "Open",
        "OpenConfigured",
        "Close",
        "TransitionStatus",
    ],
    signals = [],
    requires = [],
    // The sandbox module, used only when the host configures a stdio sandbox.
    optional = ["ai.tinyhumans.tinybox.Box"],
    // Not lazy: a host that loaded this module wants its servers connected, and
    // deferring the load would defer that until the first call — by which point
    // an agent has already been told it has no tools. `setup` starts the boot
    // connect pass and the supervisor for exactly that reason.
    lazy = false,
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod test;
