//! Bounded observations retained for one host consumer per registry object.
use std::collections::VecDeque;
use std::sync::Mutex;
use tinymcp_bus::{SupervisorBatch, SupervisorEvent, TickReport};

const MAX_EVENTS: usize = 1024;
const MAX_EVENT_BYTES: usize = 16 * 1024;
/// Largest number of events one drain can return.
pub(super) const MAX_DRAIN: usize = 256;

#[derive(Debug, Default)]
struct State {
    events: VecDeque<SupervisorEvent>,
    dropped: u64,
}
#[derive(Debug, Default)]
pub(super) struct Events(Mutex<State>);
impl Events {
    pub(super) fn record(&self, report: TickReport) {
        let Ok(mut state) = self.0.lock() else {
            return;
        };
        for event in report.events {
            if serde_json::to_vec(&event).map_or(true, |bytes| bytes.len() > MAX_EVENT_BYTES) {
                state.dropped = state.dropped.saturating_add(1);
                continue;
            }
            if state.events.len() == MAX_EVENTS {
                state.events.pop_front();
                state.dropped = state.dropped.saturating_add(1);
            }
            state.events.push_back(event);
        }
    }
    pub(super) fn drain(&self, limit: usize) -> tinybus::Result<SupervisorBatch> {
        if !(1..=MAX_DRAIN).contains(&limit) {
            return Err(tinybus::Error::failed("invalid supervisor drain limit"));
        }
        let mut state = self
            .0
            .lock()
            .map_err(|_| tinybus::Error::failed("supervisor event state unavailable"))?;
        let count = limit.min(state.events.len());
        Ok(SupervisorBatch {
            events: state.events.drain(..count).collect(),
            dropped: std::mem::take(&mut state.dropped),
        })
    }
}

#[cfg(test)]
#[path = "events_tests.rs"]
mod tests;
