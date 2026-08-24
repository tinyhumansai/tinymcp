//! The supervisor and its cycle.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use super::backoff::BackoffState;
use crate::registry::{Connections, OAuthFlow, Store};
use tinymcp_bus::{McpClientIdentityConfig, McpProxyConfig};

/// How the supervisor is paced.
#[derive(Debug, Clone)]
pub struct SupervisorConfig {
    /// How often to walk the installed servers.
    pub tick_interval: Duration,
    /// How long a liveness probe may take before the server counts as dropped.
    ///
    /// A tool listing is a cheap round trip; a server that cannot answer one
    /// within this window is not usable for a real call either.
    pub probe_timeout: Duration,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self {
            tick_interval: Duration::from_secs(60),
            probe_timeout: Duration::from_secs(8),
        }
    }
}

/// Keeps installed servers connected.
///
/// Built once and either driven by [`Self::run`] or stepped by [`Self::tick`] —
/// the second is what makes the cycle testable without waiting on a timer.
#[derive(Debug)]
pub struct Supervisor {
    config: SupervisorConfig,
    identity: McpClientIdentityConfig,
    proxy: Option<McpProxyConfig>,
    backoff: HashMap<String, BackoffState>,
    /// Servers whose last attempt failed in a way retrying cannot fix.
    ///
    /// Today that is exactly [`Error::MissingRuntime`](crate::Error::MissingRuntime):
    /// the launcher is not installed, so the process never started and the next
    /// attempt will not start it either. These are skipped entirely rather than
    /// given a backoff, because a backoff is a promise that waiting helps.
    ///
    /// Cleared when the user disables the server, so toggling it off and on is
    /// the recovery path after installing the runtime — the same gesture that
    /// already clears a backoff penalty.
    terminal: HashSet<String>,
}

impl Supervisor {
    /// Builds a supervisor.
    #[must_use]
    pub fn new(
        config: SupervisorConfig,
        identity: McpClientIdentityConfig,
        proxy: Option<McpProxyConfig>,
    ) -> Self {
        Self {
            config,
            identity,
            proxy,
            backoff: HashMap::new(),
            terminal: HashSet::new(),
        }
    }

    /// Runs until the future is dropped.
    ///
    /// The first tick is delayed by a whole interval so it does not race the
    /// startup connect pass — reconnecting a server that is halfway through
    /// connecting would tear down work already in flight.
    pub async fn run(mut self, store: &Store, connections: &Connections, oauth: &OAuthFlow) {
        let start = tokio::time::Instant::now() + self.config.tick_interval;
        let mut interval = tokio::time::interval_at(start, self.config.tick_interval);

        tracing::info!(
            tick_seconds = self.config.tick_interval.as_secs(),
            probe_seconds = self.config.probe_timeout.as_secs(),
            "the mcp supervisor started"
        );

        loop {
            interval.tick().await;
            self.tick(store, connections, oauth, Instant::now()).await;
        }
    }

    /// Runs exactly one cycle.
    ///
    /// `now` is supplied rather than read so backoff timing is deterministic
    /// under test.
    pub async fn tick(
        &mut self,
        store: &Store,
        connections: &Connections,
        oauth: &OAuthFlow,
        now: Instant,
    ) {
        let servers = match store.list_servers() {
            Ok(servers) => servers,
            Err(error) => {
                tracing::warn!("the supervisor could not list installed servers: {error}");
                return;
            }
        };

        for server in servers {
            let server_id = server.server_id.clone();

            if !server.enabled {
                // The disable path owns tearing the connection down. All that
                // is left here is to forget any backoff, so re-enabling gets an
                // immediate attempt rather than inheriting an old penalty. A
                // terminal verdict is forgotten for the same reason, and it is
                // the only way back: a user who installs the missing runtime
                // toggles the server off and on to have it tried again.
                self.backoff.remove(&server_id);
                self.terminal.remove(&server_id);
                continue;
            }

            if connections.is_connected(&server_id).await {
                if connections
                    .probe_alive(&server_id, self.config.probe_timeout)
                    .await
                {
                    self.backoff.remove(&server_id);
                    continue;
                }

                tracing::warn!(
                    server_id = %server_id,
                    qualified_name = %server.qualified_name,
                    "the transport dropped; reconnecting"
                );
                connections.disconnect(&server_id).await;
            }

            // Checked after the liveness block, not before it: a live
            // connection is still worth probing and tearing down, and only the
            // attempt that follows is pointless.
            if self.terminal.contains(&server_id) {
                continue;
            }

            if !self
                .backoff
                .entry(server_id.clone())
                .or_default()
                .ready(now)
            {
                continue;
            }

            match connections
                .connect(store, oauth, &self.identity, self.proxy.as_ref(), &server)
                .await
            {
                Ok(tools) => {
                    self.backoff.remove(&server_id);
                    self.terminal.remove(&server_id);
                    tracing::info!(
                        server_id = %server_id,
                        qualified_name = %server.qualified_name,
                        tools = tools.len(),
                        "reconnected"
                    );
                }
                Err(error) if error.is_missing_runtime() => {
                    // No backoff entry: a penalty says "wait, then try again",
                    // and there is nothing to wait for. The server is parked
                    // until the user disables and re-enables it.
                    self.backoff.remove(&server_id);
                    self.terminal.insert(server_id.clone());
                    tracing::warn!(
                        server_id = %server_id,
                        qualified_name = %server.qualified_name,
                        "connecting failed and will not be retried: {error}"
                    );
                }
                Err(error) => {
                    let state = self.backoff.entry(server_id.clone()).or_default();
                    state.record_failure(now);
                    tracing::warn!(
                        server_id = %server_id,
                        qualified_name = %server.qualified_name,
                        failures = state.failures,
                        retry_in_seconds = state.current_delay().as_secs(),
                        "reconnecting failed: {error}"
                    );
                }
            }
        }
    }

    /// How many servers currently carry a backoff penalty.
    #[must_use]
    pub fn backed_off_count(&self) -> usize {
        self.backoff.len()
    }

    /// How many servers the supervisor has parked as unretryable.
    ///
    /// Disjoint from [`Self::backed_off_count`] by construction: a terminal
    /// verdict removes any penalty rather than adding to one.
    #[must_use]
    pub fn terminally_failed_count(&self) -> usize {
        self.terminal.len()
    }
}
