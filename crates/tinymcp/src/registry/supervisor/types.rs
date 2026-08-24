//! The supervisor and its cycle.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::backoff::BackoffState;
use crate::registry::{Connections, OAuthFlow, ProbeOutcome, Store};
use tinymcp_bus::{InstalledServer, McpClientIdentityConfig, McpProxyConfig};

/// How many consecutive probe timeouts end the session.
///
/// One timeout is not evidence of a drop — see [`ProbeOutcome::TimedOut`]. It
/// takes a run of them, spread across [`SupervisorConfig::tick_interval`], for
/// "slow" to become "gone". Tearing down on the first would make the supervisor
/// the cause of the outage it then reports.
const CONSECUTIVE_TIMEOUTS_BEFORE_TEARDOWN: u32 = 3;

/// What [`Supervisor::judge_probe`] concluded about a connected server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AfterProbe {
    /// The session is usable; leave it alone.
    Keep,
    /// The session is finished; it has been ended and needs connecting again.
    Rebuild,
}

/// How the supervisor is paced.
#[derive(Debug, Clone)]
pub struct SupervisorConfig {
    /// How often to walk the installed servers.
    pub tick_interval: Duration,
    /// How long a liveness probe may take before it is recorded as a timeout.
    ///
    /// Deliberately shorter than [`REMOTE_REQUEST_TIMEOUT`](crate::REMOTE_REQUEST_TIMEOUT),
    /// the budget a real
    /// call gets: this is an early signal that a server has gone quiet, not a
    /// verdict on whether it is usable. Exceeding it therefore means *slow*,
    /// not *dead*, which is why a single timeout costs nothing and it takes a
    /// run of consecutive ones to tear a session down. That run is the
    /// reconciliation between the two deadlines — by the time one is acted on,
    /// the server has had far longer than a real request would ever get.
    ///
    /// # Why not simply widen it to the transport budget
    ///
    /// Because [`Self::tick`](super::Supervisor::tick) probes installs in
    /// sequence and each probe can consume the whole window, so the window
    /// bounds the worst-case cycle. Widening it to 30s multiplies that by
    /// ~3.75 and, past a handful of unresponsive installs, a cycle outruns
    /// `tick_interval`. [`Supervisor::run`](super::Supervisor::run) sets
    /// `MissedTickBehavior::Delay` so that does not become a burst of
    /// catch-up ticks — but a host that drives `tick` from its own timer gets
    /// no such protection, and at least one does. Raising this default is
    /// therefore not a local decision; it needs the probe loop bounded first
    /// (concurrent probes, or a cycle deadline).
    ///
    /// A host that knows its servers are slow can still raise it explicitly.
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
    /// Consecutive probe timeouts per server.
    ///
    /// Held here rather than in [`Connections`] on purpose: `disconnect` clears
    /// that map, so a counter kept there would erase the very history it exists
    /// to accumulate.
    timeouts: HashMap<String, u32>,
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
            timeouts: HashMap::new(),
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
        // A cycle walks every install in turn and each probe can take the whole
        // probe window, so a tick can outlast its own interval. The default
        // behaviour would then fire the missed ticks back to back, re-probing
        // servers that were just probed. Delay instead: pace from when the
        // cycle finished.
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

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
                // immediate attempt rather than inheriting an old penalty. The
                // timeout streak goes with it for the same reason.
                self.backoff.remove(&server_id);
                self.timeouts.remove(&server_id);
                continue;
            }

            if connections.is_connected(&server_id).await
                && self.judge_probe(connections, &server).await == AfterProbe::Keep
            {
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
                    self.timeouts.remove(&server_id);
                    tracing::info!(
                        server_id = %server_id,
                        qualified_name = %server.qualified_name,
                        tools = tools.len(),
                        "reconnected"
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

    /// Probes one connected server and decides what its answer means.
    ///
    /// Split out of [`Self::tick`] because the decision is the substance of
    /// this type and the loop around it is bookkeeping.
    async fn judge_probe(
        &mut self,
        connections: &Connections,
        server: &InstalledServer,
    ) -> AfterProbe {
        let server_id = server.server_id.clone();
        let outcome = connections
            .probe_alive(&server_id, self.config.probe_timeout)
            .await;

        match &outcome {
            ProbeOutcome::Alive { elapsed } => {
                tracing::trace!(
                    server_id = %server_id,
                    ?elapsed,
                    "the liveness probe answered"
                );
                self.backoff.remove(&server_id);
                self.timeouts.remove(&server_id);
                return AfterProbe::Keep;
            }
            // Slow, not gone. Say so, count it, and leave the session
            // alone until a run of them says otherwise — the warning
            // reports what was observed rather than asserting a cause
            // nothing measured.
            ProbeOutcome::TimedOut { after } => {
                let streak = self
                    .timeouts
                    .entry(server_id.clone())
                    .and_modify(|streak| *streak = streak.saturating_add(1))
                    .or_insert(1);
                let streak = *streak;

                if streak < CONSECUTIVE_TIMEOUTS_BEFORE_TEARDOWN {
                    tracing::warn!(
                        server_id = %server_id,
                        qualified_name = %server.qualified_name,
                        outcome = outcome.as_str(),
                        probe_timeout_seconds = after.as_secs(),
                        consecutive_timeouts = streak,
                        teardown_after = CONSECUTIVE_TIMEOUTS_BEFORE_TEARDOWN,
                        "the liveness probe did not answer in time; \
                         keeping the session"
                    );
                    return AfterProbe::Keep;
                }

                tracing::warn!(
                    server_id = %server_id,
                    qualified_name = %server.qualified_name,
                    outcome = outcome.as_str(),
                    probe_timeout_seconds = after.as_secs(),
                    consecutive_timeouts = streak,
                    "the liveness probe has not answered for \
                     {streak} consecutive ticks; reconnecting"
                );
                self.timeouts.remove(&server_id);
            }
            // Observed to fail, so there is nothing to wait for: this is
            // the case the supervisor was built for, and it still acts
            // on the first sighting.
            ProbeOutcome::Broken { error, elapsed } => {
                tracing::warn!(
                    server_id = %server_id,
                    qualified_name = %server.qualified_name,
                    outcome = outcome.as_str(),
                    ?elapsed,
                    "the transport failed its liveness probe; \
                     reconnecting: {error}"
                );
                self.timeouts.remove(&server_id);
            }
            // The entry went between the membership check and the probe.
            // Nothing to report and nothing to tear down, but the caller still
            // has to rebuild it.
            ProbeOutcome::Missing => {
                self.timeouts.remove(&server_id);
            }
        }

        connections.disconnect(&server_id).await;
        AfterProbe::Rebuild
    }

    /// How many servers currently carry a backoff penalty.
    #[must_use]
    pub fn backed_off_count(&self) -> usize {
        self.backoff.len()
    }

    /// How many consecutive probe timeouts `server_id` has accumulated.
    ///
    /// Zero for a server that answered its last probe, that was never probed,
    /// or that has just been torn down — the streak is what the next teardown
    /// decision is made on, so it is worth being able to read.
    #[must_use]
    pub fn consecutive_timeouts(&self, server_id: &str) -> u32 {
        self.timeouts.get(server_id).copied().unwrap_or(0)
    }
}
