//! The supervisor and its cycle.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use super::backoff::BackoffState;
use super::report::{ServerRef, SupervisorEvent, TickReport};
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
    /// Servers whose last attempt failed in a way retrying cannot fix.
    ///
    /// Today that is exactly [`Error::MissingRuntime`](crate::Error::MissingRuntime):
    /// the launcher is not installed, so the process never started and the next
    /// attempt will not start it either. These are skipped entirely rather than
    /// given a backoff, because a backoff is a promise that waiting helps.
    ///
    /// Distinct from [`Self::timeouts`], which counts a *live* session going
    /// quiet: that is a reason to wait longer, this is a reason to stop.
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
            timeouts: HashMap::new(),
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
            // The report is for a host that drives `tick` itself. This loop
            // has no one to hand it to, and everything in it was logged as it
            // happened.
            self.tick(store, connections, oauth, Instant::now()).await;
        }
    }

    /// Runs exactly one cycle.
    ///
    /// `now` is supplied rather than read so backoff timing is deterministic
    /// under test.
    ///
    /// The report says what the cycle observed and did, install by install.
    /// All of it was logged as it happened; the report exists for a host that
    /// wants to put those observations somewhere a log line cannot go.
    pub async fn tick(
        &mut self,
        store: &Store,
        connections: &Connections,
        oauth: &OAuthFlow,
        now: Instant,
    ) -> TickReport {
        let mut report = TickReport::default();

        let servers = match store.list_servers() {
            Ok(servers) => servers,
            Err(error) => {
                tracing::warn!("the supervisor could not list installed servers: {error}");
                return report;
            }
        };

        for server in servers {
            let server_id = server.server_id.clone();

            if !server.enabled {
                // The disable path owns tearing the connection down. All that
                // is left here is to forget any backoff, so re-enabling gets an
                // immediate attempt rather than inheriting an old penalty. The
                // timeout streak goes with it for the same reason, and so does
                // a terminal verdict — which is the only way back from one: a
                // user who installs the missing runtime toggles the server off
                // and on to have it tried again.
                self.backoff.remove(&server_id);
                self.timeouts.remove(&server_id);
                self.terminal.remove(&server_id);
                continue;
            }

            if connections.is_connected(&server_id).await {
                let (verdict, event) = self.judge_probe(connections, &server).await;
                report.push(event);
                if verdict == AfterProbe::Keep {
                    continue;
                }
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

            let event = self
                .attempt_connect(store, connections, oauth, &server, now)
                .await;
            report.push(event);
        }

        report
    }

    /// Dials one install that is not connected, and records what came of it.
    ///
    /// Split out of [`Self::tick`] for the same reason as
    /// [`Self::judge_probe`]: what an attempt's outcome *means* — a penalty, a
    /// parking, a recovery — is the substance, and the loop is bookkeeping.
    async fn attempt_connect(
        &mut self,
        store: &Store,
        connections: &Connections,
        oauth: &OAuthFlow,
        server: &InstalledServer,
        now: Instant,
    ) -> SupervisorEvent {
        let server_id = server.server_id.clone();

        match connections
            .connect(store, oauth, &self.identity, self.proxy.as_ref(), server)
            .await
        {
            Ok(tools) => {
                // Read before it is forgotten: how many attempts this success
                // took is what tells a host whether the server had been
                // unavailable across cycles or was merely rebuilt within one.
                let after_failures = self
                    .backoff
                    .remove(&server_id)
                    .map_or(0, |state| state.failures);
                self.timeouts.remove(&server_id);
                self.terminal.remove(&server_id);
                tracing::info!(
                    server_id = %server_id,
                    qualified_name = %server.qualified_name,
                    tools = tools.len(),
                    after_failures,
                    "reconnected"
                );
                SupervisorEvent::Reconnected {
                    server: ServerRef::from(server),
                    tools: tools.len(),
                    after_failures,
                }
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
                SupervisorEvent::Parked {
                    server: ServerRef::from(server),
                    error: error.to_string(),
                }
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
                SupervisorEvent::ReconnectFailed {
                    server: ServerRef::from(server),
                    error: error.to_string(),
                    failures: state.failures,
                    retry_in: state.current_delay(),
                }
            }
        }
    }

    /// Probes one connected server and decides what its answer means.
    ///
    /// Split out of [`Self::tick`] because the decision is the substance of
    /// this type and the loop around it is bookkeeping. Returns the verdict
    /// and the event that records it; a `Rebuild` verdict has already ended
    /// the session by the time this returns.
    async fn judge_probe(
        &mut self,
        connections: &Connections,
        server: &InstalledServer,
    ) -> (AfterProbe, SupervisorEvent) {
        let server_id = server.server_id.clone();
        let outcome = connections
            .probe_alive(&server_id, self.config.probe_timeout)
            .await;

        let (verdict, event) = match &outcome {
            ProbeOutcome::Alive { elapsed } => {
                tracing::trace!(
                    server_id = %server_id,
                    ?elapsed,
                    "the liveness probe answered"
                );
                self.backoff.remove(&server_id);
                self.timeouts.remove(&server_id);
                (
                    AfterProbe::Keep,
                    SupervisorEvent::ProbeAnswered {
                        server: ServerRef::from(server),
                        elapsed: *elapsed,
                    },
                )
            }
            // Slow, not gone. Say so, count it, and leave the session
            // alone until a run of them says otherwise — the warning
            // reports what was observed rather than asserting a cause
            // nothing measured.
            ProbeOutcome::TimedOut { after } => self.judge_timeout(server, *after),
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
                (
                    AfterProbe::Rebuild,
                    SupervisorEvent::TransportDropped {
                        server: ServerRef::from(server),
                        outcome: outcome.clone(),
                        consecutive_timeouts: 0,
                    },
                )
            }
            // The entry went between the membership check and the probe.
            // Nothing was observed to fail and there is nothing to tear down,
            // but the caller still has to rebuild it, and a host still wants
            // to know that it did.
            ProbeOutcome::Missing => {
                self.timeouts.remove(&server_id);
                (
                    AfterProbe::Rebuild,
                    SupervisorEvent::TransportDropped {
                        server: ServerRef::from(server),
                        outcome: ProbeOutcome::Missing,
                        consecutive_timeouts: 0,
                    },
                )
            }
        };

        if verdict == AfterProbe::Rebuild {
            connections.disconnect(&server_id).await;
        }

        (verdict, event)
    }

    /// Counts one probe timeout and decides whether the run has become a drop.
    ///
    /// The timeout half of [`Self::judge_probe`], on its own because it is the
    /// one outcome with history: a single timeout keeps the session, and only
    /// [`CONSECUTIVE_TIMEOUTS_BEFORE_TEARDOWN`] of them in a row end it.
    fn judge_timeout(
        &mut self,
        server: &InstalledServer,
        after: Duration,
    ) -> (AfterProbe, SupervisorEvent) {
        let server_id = server.server_id.clone();
        let outcome = ProbeOutcome::TimedOut { after };
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
            return (
                AfterProbe::Keep,
                SupervisorEvent::ProbeTimedOut {
                    server: ServerRef::from(server),
                    after,
                    consecutive: streak,
                    teardown_after: CONSECUTIVE_TIMEOUTS_BEFORE_TEARDOWN,
                },
            );
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
        (
            AfterProbe::Rebuild,
            SupervisorEvent::TransportDropped {
                server: ServerRef::from(server),
                outcome,
                consecutive_timeouts: streak,
            },
        )
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

    /// How many servers the supervisor has parked as unretryable.
    ///
    /// Disjoint from [`Self::backed_off_count`] by construction: a terminal
    /// verdict removes any penalty rather than adding to one.
    #[must_use]
    pub fn terminally_failed_count(&self) -> usize {
        self.terminal.len()
    }
}
