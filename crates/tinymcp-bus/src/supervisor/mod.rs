//! Supervisor observation vocabulary. Reconnect behavior lives in the module.
use crate::InstalledServer;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

/// What a liveness probe observed.
///
/// The failing outcomes are kept apart because a caller has a genuinely
/// different correct response to each, which is the whole reason this is not a
/// `bool`. A transport that answered with an error is broken now and there is
/// nothing to wait for. A transport that did not answer inside the probe window
/// may simply be slower than that window. The window is at most
/// the transport request timeout and is normally configured shorter — it is an
/// early signal, not the budget a real call gets — so exceeding it does not
/// mean the server would have failed a real call. Collapsing the two lets a
/// supervisor tear down a working session and then report a drop that never
/// happened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub enum ProbeOutcome {
    /// The server answered inside the probe window.
    Alive {
        /// How long the round trip took.
        elapsed: Duration,
    },
    /// There is no entry for this server, so there was nothing to probe.
    Missing,
    /// The transport answered with an error.
    Broken {
        /// What the transport reported, already rendered.
        error: String,
        /// How long it took to fail.
        elapsed: Duration,
    },
    /// The server did not answer inside the probe window.
    ///
    /// Not the same as broken: nothing was observed to fail, only to be slow.
    TimedOut {
        /// The window that elapsed without an answer.
        after: Duration,
    },
    /// An outcome added by a newer compatible contract version.
    Unknown,
}

#[derive(Deserialize)]
enum KnownProbeOutcome {
    Alive { elapsed: Duration },
    Missing,
    Broken { error: String, elapsed: Duration },
    TimedOut { after: Duration },
}

impl<'de> Deserialize<'de> for ProbeOutcome {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        let known = match &value {
            Value::String(name) => name == "Missing",
            Value::Object(fields) if fields.len() == 1 => fields
                .keys()
                .next()
                .is_some_and(|name| matches!(name.as_str(), "Alive" | "Broken" | "TimedOut")),
            _ => false,
        };
        if !known {
            return Ok(Self::Unknown);
        }
        match serde_json::from_value::<KnownProbeOutcome>(value)
            .map_err(<D::Error as serde::de::Error>::custom)?
        {
            KnownProbeOutcome::Alive { elapsed } => Ok(Self::Alive { elapsed }),
            KnownProbeOutcome::Missing => Ok(Self::Missing),
            KnownProbeOutcome::Broken { error, elapsed } => Ok(Self::Broken { error, elapsed }),
            KnownProbeOutcome::TimedOut { after } => Ok(Self::TimedOut { after }),
        }
    }
}

impl ProbeOutcome {
    /// Whether the server answered.
    #[must_use]
    pub const fn is_alive(&self) -> bool {
        matches!(self, Self::Alive { .. })
    }

    /// A stable one-word label, for structured log fields.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Alive { .. } => "alive",
            Self::Missing => "missing",
            Self::Broken { .. } => "broken",
            Self::TimedOut { .. } => "timed_out",
            Self::Unknown => "unknown",
        }
    }
}

/// Which install an event is about.
///
/// The three names a host needs to route or render an event, copied out of
/// the install so the report owns its data and outlives the store read that
/// produced it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerRef {
    /// The install's identifier.
    pub server_id: String,
    /// The registry's qualified name, such as `@scope/server`.
    pub qualified_name: String,
    /// The registry's display name.
    pub display_name: String,
}

impl From<&InstalledServer> for ServerRef {
    fn from(server: &InstalledServer) -> Self {
        Self {
            server_id: server.server_id.clone(),
            qualified_name: server.qualified_name.clone(),
            display_name: server.display_name.clone(),
        }
    }
}

/// One thing the supervisor observed or did during a cycle.
///
/// Non-exhaustive: a host matches with a wildcard, so a later cycle step can
/// report itself without breaking the hosts that do not care about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub enum SupervisorEvent {
    /// A connected server answered its liveness probe.
    ///
    /// The nominal case, reported so a host can watch a server's latency drift
    /// before it starts missing the window.
    ProbeAnswered {
        /// The server that answered.
        server: ServerRef,
        /// How long the round trip took.
        elapsed: Duration,
    },
    /// A connected server did not answer inside the probe window, and the
    /// session was kept.
    ///
    /// Slow, not gone — see [`ProbeOutcome::TimedOut`]. The session ends only
    /// once `consecutive` reaches `teardown_after`, and that cycle reports
    /// [`Self::TransportDropped`] instead of this.
    ProbeTimedOut {
        /// The server that went quiet.
        server: ServerRef,
        /// The window that elapsed without an answer.
        after: Duration,
        /// How many probes in a row have now timed out, this one included.
        consecutive: u32,
        /// The streak length at which the session is torn down.
        teardown_after: u32,
    },
    /// A session was ended because its probe found it unusable, and a
    /// reconnect follows in the same cycle.
    ///
    /// What that reconnect came to is reported separately, as
    /// [`Self::Reconnected`], [`Self::ReconnectFailed`] or [`Self::Parked`].
    TransportDropped {
        /// The server whose session ended.
        server: ServerRef,
        /// What the probe observed. Never [`ProbeOutcome::Alive`].
        outcome: ProbeOutcome,
        /// The timeout streak that ended the session when the outcome was a
        /// timeout; zero for a transport that was observed to fail.
        consecutive_timeouts: u32,
    },
    /// A server was connected, either freshly or after its session was ended.
    Reconnected {
        /// The server that connected.
        server: ServerRef,
        /// How many tools it advertises.
        tools: usize,
        /// How many consecutive attempts had failed before this one succeeded.
        ///
        /// Zero when the session was rebuilt in the same cycle that ended it,
        /// which no user was around to notice; anything else means the server
        /// had been unavailable across at least one whole cycle.
        after_failures: u32,
    },
    /// A connection attempt failed and will be retried after a backoff.
    ReconnectFailed {
        /// The server that could not be connected.
        server: ServerRef,
        /// What the attempt reported, already rendered.
        error: String,
        /// How many consecutive attempts have now failed, this one included.
        failures: u32,
        /// How long the supervisor waits before the next attempt.
        retry_in: Duration,
    },
    /// A connection attempt failed in a way retrying cannot fix, so the server
    /// is parked until it is disabled and re-enabled.
    ///
    /// Today that is exactly a missing runtime.
    Parked {
        /// The server that was parked.
        server: ServerRef,
        /// What the attempt reported, already rendered.
        error: String,
    },
    /// An observation added by a newer compatible contract version.
    Unknown,
}

#[derive(Deserialize)]
enum KnownSupervisorEvent {
    ProbeAnswered {
        server: ServerRef,
        elapsed: Duration,
    },
    ProbeTimedOut {
        server: ServerRef,
        after: Duration,
        consecutive: u32,
        teardown_after: u32,
    },
    TransportDropped {
        server: ServerRef,
        outcome: ProbeOutcome,
        consecutive_timeouts: u32,
    },
    Reconnected {
        server: ServerRef,
        tools: usize,
        after_failures: u32,
    },
    ReconnectFailed {
        server: ServerRef,
        error: String,
        failures: u32,
        retry_in: Duration,
    },
    Parked {
        server: ServerRef,
        error: String,
    },
}

impl<'de> Deserialize<'de> for SupervisorEvent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        let known = value.as_object().is_some_and(|fields| {
            fields.len() == 1
                && fields.keys().next().is_some_and(|name| {
                    matches!(
                        name.as_str(),
                        "ProbeAnswered"
                            | "ProbeTimedOut"
                            | "TransportDropped"
                            | "Reconnected"
                            | "ReconnectFailed"
                            | "Parked"
                    )
                })
        });
        if !known {
            return Ok(Self::Unknown);
        }
        match serde_json::from_value::<KnownSupervisorEvent>(value)
            .map_err(<D::Error as serde::de::Error>::custom)?
        {
            KnownSupervisorEvent::ProbeAnswered { server, elapsed } => {
                Ok(Self::ProbeAnswered { server, elapsed })
            }
            KnownSupervisorEvent::ProbeTimedOut {
                server,
                after,
                consecutive,
                teardown_after,
            } => Ok(Self::ProbeTimedOut {
                server,
                after,
                consecutive,
                teardown_after,
            }),
            KnownSupervisorEvent::TransportDropped {
                server,
                outcome,
                consecutive_timeouts,
            } => Ok(Self::TransportDropped {
                server,
                outcome,
                consecutive_timeouts,
            }),
            KnownSupervisorEvent::Reconnected {
                server,
                tools,
                after_failures,
            } => Ok(Self::Reconnected {
                server,
                tools,
                after_failures,
            }),
            KnownSupervisorEvent::ReconnectFailed {
                server,
                error,
                failures,
                retry_in,
            } => Ok(Self::ReconnectFailed {
                server,
                error,
                failures,
                retry_in,
            }),
            KnownSupervisorEvent::Parked { server, error } => Ok(Self::Parked { server, error }),
        }
    }
}

impl SupervisorEvent {
    /// The server this event is about, if this version knows the event kind.
    #[must_use]
    pub fn server(&self) -> Option<&ServerRef> {
        match self {
            Self::ProbeAnswered { server, .. }
            | Self::ProbeTimedOut { server, .. }
            | Self::TransportDropped { server, .. }
            | Self::Reconnected { server, .. }
            | Self::ReconnectFailed { server, .. }
            | Self::Parked { server, .. } => Some(server),
            Self::Unknown => None,
        }
    }

    /// A stable one-word label, for structured log fields.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::ProbeAnswered { .. } => "probe_answered",
            Self::ProbeTimedOut { .. } => "probe_timed_out",
            Self::TransportDropped { .. } => "transport_dropped",
            Self::Reconnected { .. } => "reconnected",
            Self::ReconnectFailed { .. } => "reconnect_failed",
            Self::Parked { .. } => "parked",
            Self::Unknown => "unknown",
        }
    }
}

/// Everything one cycle observed, in the order it happened.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TickReport {
    /// The events, in observation order. A server that was torn down and
    /// reconnected in one cycle appears twice, the drop first.
    pub events: Vec<SupervisorEvent>,
}

impl TickReport {
    /// Whether the cycle observed nothing at all.
    ///
    /// True for an empty store, and for one whose every install is disabled,
    /// parked, or waiting out a backoff. A healthy connected server is *not*
    /// nothing — it is a [`SupervisorEvent::ProbeAnswered`].
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Append an observation in its original order.
    pub fn push(&mut self, event: SupervisorEvent) {
        self.events.push(event);
    }
}

/// Ordered terminal and probe observations drained by one host consumer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupervisorBatch {
    /// Retained observations in original order.
    pub events: Vec<SupervisorEvent>,
    /// Old observations dropped since the last successful drain.
    pub dropped: u64,
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
