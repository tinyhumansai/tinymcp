//! Serialized supervisor vocabulary round trips without losing observation order.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use super::*;

#[test]
fn ordered_observations_and_loss_counts_round_trip() {
    let server = ServerRef {
        server_id: "id".into(),
        qualified_name: "@test/server".into(),
        display_name: "Server".into(),
    };
    let events = vec![
        SupervisorEvent::ProbeAnswered {
            server: server.clone(),
            elapsed: Duration::from_millis(12),
        },
        SupervisorEvent::ProbeTimedOut {
            server: server.clone(),
            after: Duration::from_secs(8),
            consecutive: 2,
            teardown_after: 3,
        },
        SupervisorEvent::TransportDropped {
            server: server.clone(),
            outcome: ProbeOutcome::Broken {
                error: "closed".into(),
                elapsed: Duration::ZERO,
            },
            consecutive_timeouts: 0,
        },
        SupervisorEvent::Reconnected {
            server: server.clone(),
            tools: 4,
            after_failures: 1,
        },
        SupervisorEvent::ReconnectFailed {
            server: server.clone(),
            error: "unavailable".into(),
            failures: 2,
            retry_in: Duration::from_secs(10),
        },
        SupervisorEvent::Parked {
            server: server.clone(),
            error: "runtime missing".into(),
        },
    ];
    let expected = [
        "probe_answered",
        "probe_timed_out",
        "transport_dropped",
        "reconnected",
        "reconnect_failed",
        "parked",
    ];
    for (event, kind) in events.iter().zip(expected) {
        assert_eq!(event.server(), Some(&server));
        assert_eq!(event.kind(), kind);
    }
    let batch = SupervisorBatch {
        events: events.clone(),
        dropped: 7,
    };
    let wire = serde_json::to_value(&batch).unwrap();
    assert_eq!(
        serde_json::from_value::<SupervisorBatch>(wire.clone()).unwrap(),
        batch
    );
    assert_eq!(
        wire["events"][0]["ProbeAnswered"]["elapsed"],
        serde_json::json!({"secs":0,"nanos":12_000_000})
    );
    let mut report = TickReport::default();
    assert!(report.is_empty());
    for event in events {
        report.push(event);
    }
    assert!(!report.is_empty());
    for (probe, kind) in [
        (ProbeOutcome::Missing, "missing"),
        (
            ProbeOutcome::Alive {
                elapsed: Duration::ZERO,
            },
            "alive",
        ),
        (
            ProbeOutcome::Broken {
                error: "closed".into(),
                elapsed: Duration::ZERO,
            },
            "broken",
        ),
        (
            ProbeOutcome::TimedOut {
                after: Duration::ZERO,
            },
            "timed_out",
        ),
    ] {
        assert_eq!(probe.as_str(), kind);
        assert_eq!(probe.is_alive(), kind == "alive");
        assert_eq!(
            serde_json::from_value::<ProbeOutcome>(serde_json::to_value(&probe).unwrap()).unwrap(),
            probe
        );
    }
}

#[test]
fn server_reference_keeps_host_routing_identity() {
    let installed: InstalledServer=serde_json::from_value(serde_json::json!({"server_id":"id","qualified_name":"@test/server","display_name":"Server","command_kind":"node","command":"node","args":[],"env_keys":[],"installed_at":0,"transport":{"kind":"stdio"},"enabled":true})).unwrap();
    let reference = ServerRef::from(&installed);
    assert_eq!(reference.server_id, "id");
    assert_eq!(reference.qualified_name, "@test/server");
    assert_eq!(reference.display_name, "Server");
}

#[test]
fn newer_supervisor_variants_decode_as_unknown() {
    let probe = serde_json::json!({"future_probe": {"after": {"secs": 1, "nanos": 0}}});
    assert_eq!(
        serde_json::from_value::<ProbeOutcome>(probe).unwrap(),
        ProbeOutcome::Unknown
    );

    let event = serde_json::json!({"future_event": {"server_id": "id"}});
    let event = serde_json::from_value::<SupervisorEvent>(event).unwrap();
    assert_eq!(event, SupervisorEvent::Unknown);
    assert_eq!(event.server(), None);
    assert_eq!(event.kind(), "unknown");
}

#[test]
fn malformed_known_supervisor_variants_are_still_rejected() {
    assert!(serde_json::from_value::<ProbeOutcome>(serde_json::json!({"Alive": {}})).is_err());
    assert!(
        serde_json::from_value::<SupervisorEvent>(serde_json::json!({
            "ProbeAnswered": {}
        }))
        .is_err()
    );
}
