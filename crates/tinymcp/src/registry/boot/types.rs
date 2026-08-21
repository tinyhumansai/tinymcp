//! The startup connect pass.

use futures_util::StreamExt as _;

use crate::registry::{Connections, OAuthFlow, Store};
use tinymcp_bus::{McpClientIdentityConfig, McpProxyConfig};

/// How many servers are brought up at once.
///
/// Enough to overlap the handshakes that dominate startup, low enough that a
/// user with dozens of installs does not spawn all of them in the same instant.
pub const BOOT_CONCURRENCY: usize = 8;

/// What the startup pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BootOutcome {
    /// How many servers connected.
    pub connected: usize,
    /// How many failed. Each was logged; none stopped the pass.
    pub failed: usize,
    /// How many were skipped because the user had turned them off.
    pub skipped: usize,
}

impl BootOutcome {
    /// How many installs the pass considered.
    #[must_use]
    pub const fn total(&self) -> usize {
        self.connected + self.failed + self.skipped
    }
}

/// Connects every enabled installed server.
///
/// Never fails: a store that cannot be listed and a server that cannot be
/// connected are both logged and reflected in the outcome. See the module note
/// on why startup does not stop for either.
pub async fn connect_installed_servers(
    store: &Store,
    connections: &Connections,
    oauth: &OAuthFlow,
    identity: &McpClientIdentityConfig,
    proxy: Option<&McpProxyConfig>,
) -> BootOutcome {
    let servers = match store.list_servers() {
        Ok(servers) => servers,
        Err(error) => {
            tracing::warn!("could not list installed servers at startup: {error}");
            return BootOutcome::default();
        }
    };

    if servers.is_empty() {
        tracing::debug!("no installed mcp servers to connect at startup");
        return BootOutcome::default();
    }

    let (enabled, skipped): (Vec<_>, Vec<_>) =
        servers.into_iter().partition(|server| server.enabled);

    for server in &skipped {
        tracing::info!(
            server_id = %server.server_id,
            qualified_name = %server.qualified_name,
            "skipping a server the user turned off"
        );
    }

    tracing::info!(count = enabled.len(), "connecting installed mcp servers");

    // Counted through an atomic rather than by collecting results, so the
    // concurrent stream does not have to buffer one entry per server.
    let connected = std::sync::atomic::AtomicUsize::new(0);
    let failed = std::sync::atomic::AtomicUsize::new(0);

    futures_util::stream::iter(enabled)
        .for_each_concurrent(BOOT_CONCURRENCY, |server| {
            let connected = &connected;
            let failed = &failed;
            async move {
                match connections
                    .connect(store, oauth, identity, proxy, &server)
                    .await
                {
                    Ok(tools) => {
                        connected.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        tracing::info!(
                            server_id = %server.server_id,
                            qualified_name = %server.qualified_name,
                            tools = tools.len(),
                            "connected at startup"
                        );
                    }
                    Err(error) => {
                        failed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        tracing::warn!(
                            server_id = %server.server_id,
                            qualified_name = %server.qualified_name,
                            "could not connect at startup: {error}"
                        );
                    }
                }
            }
        })
        .await;

    BootOutcome {
        connected: connected.into_inner(),
        failed: failed.into_inner(),
        skipped: skipped.len(),
    }
}
