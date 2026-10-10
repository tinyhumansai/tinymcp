//! Queue ordering, memory bounds, explicit loss accounting and faults.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use super::*;
use tinymcp_bus::ServerRef;
fn event(id: usize) -> SupervisorEvent {
    SupervisorEvent::Parked {
        server: ServerRef {
            server_id: id.to_string(),
            qualified_name: "@test/server".into(),
            display_name: "Server".into(),
        },
        error: "missing runtime".into(),
    }
}
#[test]
fn drains_in_order_once_and_reports_oldest_drops_once() {
    let queue = Events::default();
    queue.record(TickReport {
        events: (0..MAX_EVENTS + 2).map(event).collect(),
    });
    assert!(queue.drain(0).is_err());
    assert!(queue.drain(MAX_DRAIN + 1).is_err());
    let batch = queue.drain(2).unwrap();
    assert_eq!(batch.dropped, 2);
    assert_eq!(batch.events, vec![event(2), event(3)]);
    let batch = queue.drain(1).unwrap();
    assert_eq!(batch.dropped, 0);
    assert_eq!(batch.events, vec![event(4)]);
    while !queue.drain(MAX_DRAIN).unwrap().events.is_empty() {}
    assert_eq!(queue.drain(1).unwrap(), SupervisorBatch::default());
}
#[test]
fn rejects_oversized_event_and_retains_following_observations() {
    let queue = Events::default();
    let mut oversized = event(0);
    if let SupervisorEvent::Parked { error, .. } = &mut oversized {
        *error = "x".repeat(MAX_EVENT_BYTES);
    }
    queue.record(TickReport {
        events: vec![oversized, event(1)],
    });
    assert_eq!(
        queue.drain(1).unwrap(),
        SupervisorBatch {
            events: vec![event(1)],
            dropped: 1
        }
    );
}
#[test]
fn poisoned_queue_returns_closed_fault_without_event_content() {
    let queue = Events::default();
    let _ = std::panic::catch_unwind(|| {
        let _guard = queue.0.lock().unwrap();
        panic!("fixture");
    });
    queue.record(TickReport {
        events: vec![event(9)],
    });
    let error = queue.drain(1).unwrap_err().to_string();
    assert!(error.contains("supervisor event state unavailable"));
    assert!(!error.contains("missing runtime"));
}
