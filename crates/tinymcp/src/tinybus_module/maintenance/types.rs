//! The maintenance task and how it is paced.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use super::events::Events;

use crate::registry::{BootOutcome, McpRegistry, Supervisor, SupervisorConfig, TickReport};

/// Where the supervisor's cycles come from.
#[derive(Debug)]
pub(in crate::tinybus_module) enum Pacing {
    /// One cycle per interval, the first a whole interval after the boot pass
    /// finishes. The production setting.
    Interval(Duration),
    /// One cycle per message received. The channel closing ends the task.
    ///
    /// Compiled for tests only: production is paced by the clock, and a
    /// variant nothing constructs would be dead code.
    #[cfg(test)]
    Driven(mpsc::Receiver<()>),
}

impl Pacing {
    /// Waits for the next cycle. `false` means there will not be one.
    async fn next(&mut self, interval: &mut Option<tokio::time::Interval>) -> bool {
        match self {
            Self::Interval(period) => {
                let ticker = interval.get_or_insert_with(|| {
                    // The first cycle is a whole interval away so it does not
                    // race a connect that only just finished.
                    let start = tokio::time::Instant::now() + *period;
                    let mut ticker = tokio::time::interval_at(start, *period);
                    // A cycle walks every install and each probe can take its
                    // whole window, so a cycle can outlast its interval.
                    // Bursting the missed ones would re-probe servers that were
                    // just probed; pace from when the cycle finished instead.
                    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                    ticker
                });
                ticker.tick().await;
                true
            }
            #[cfg(test)]
            Self::Driven(cycles) => cycles.recv().await.is_some(),
        }
    }
}

/// The running boot pass and supervisor for one registry.
///
/// Dropping it ends the task, so the work lives exactly as long as the service
/// that owns it.
#[derive(Debug)]
pub(in crate::tinybus_module) struct Maintenance {
    booted: watch::Receiver<Option<BootOutcome>>,
    task: JoinHandle<()>,
    events: Arc<Events>,
}

impl Maintenance {
    /// Starts the boot pass and the supervisor, paced by the supervisor
    /// configuration's own interval.
    ///
    /// Must be called from within a Tokio runtime, which a module's `setup`
    /// always is. Returns immediately; see the module note.
    pub(in crate::tinybus_module) fn start(
        registry: Arc<McpRegistry>,
        config: SupervisorConfig,
    ) -> Self {
        let pacing = Pacing::Interval(config.tick_interval);
        Self::start_with(registry, config, pacing, None)
    }

    /// As [`Self::start`], with the pacing chosen and each cycle's report sent
    /// to `reports` when one is supplied.
    pub(in crate::tinybus_module) fn start_with(
        registry: Arc<McpRegistry>,
        config: SupervisorConfig,
        pacing: Pacing,
        reports: Option<mpsc::UnboundedSender<TickReport>>,
    ) -> Self {
        let (booted_tx, booted) = watch::channel(None);
        let events = Arc::new(Events::default());
        let task = tokio::spawn(run(
            registry,
            config,
            pacing,
            reports,
            booted_tx,
            events.clone(),
        ));

        Self {
            booted,
            task,
            events,
        }
    }

    /// Drains observations in order for the host's notification adapter.
    pub(in crate::tinybus_module) fn drain(
        &self,
        limit: usize,
    ) -> tinybus::Result<tinymcp_bus::SupervisorBatch> {
        self.events.drain(limit)
    }

    /// Waits for the boot pass to finish and returns what it did.
    ///
    /// Returns an empty outcome when the task ended before finishing it.
    pub(in crate::tinybus_module) async fn booted(&self) -> BootOutcome {
        let mut receiver = self.booted.clone();
        match receiver.wait_for(Option::is_some).await {
            Ok(outcome) => outcome.unwrap_or_default(),
            Err(_) => BootOutcome::default(),
        }
    }
}

impl Drop for Maintenance {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// The task body: connect everything, then keep it connected.
async fn run(
    registry: Arc<McpRegistry>,
    config: SupervisorConfig,
    mut pacing: Pacing,
    reports: Option<mpsc::UnboundedSender<TickReport>>,
    booted: watch::Sender<Option<BootOutcome>>,
    events: Arc<Events>,
) {
    let outcome = registry.connect_installed().await;
    tracing::info!(
        connected = outcome.connected,
        failed = outcome.failed,
        skipped = outcome.skipped,
        "the boot connect pass finished"
    );
    booted.send_replace(Some(outcome));

    let mut supervisor: Supervisor = registry.supervisor(config);
    let mut interval = None;

    while pacing.next(&mut interval).await {
        let report = supervisor
            .tick(
                registry.store(),
                registry.connections(),
                registry.oauth(),
                Instant::now(),
            )
            .await;

        events.record(report.clone());
        if let Some(reports) = &reports {
            // A receiver that went away is not a reason to stop supervising.
            let _ = reports.send(report.clone());
        }
    }
}
