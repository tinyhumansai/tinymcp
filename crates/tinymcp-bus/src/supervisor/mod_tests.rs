//! Serialized supervisor vocabulary round trips without losing observation order.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use super::*;

fn fixture_events(server: &ServerRef) -> Vec<SupervisorEvent> {
    vec![
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
    ]
}

fn expected_batch_wire() -> serde_json::Value {
    serde_json::json!({
        "events":[
            {"ProbeAnswered":{"server":{"server_id":"id","qualified_name":"@test/server","display_name":"Server"},"elapsed":{"secs":0,"nanos":12_000_000}}},
            {"ProbeTimedOut":{"server":{"server_id":"id","qualified_name":"@test/server","display_name":"Server"},"after":{"secs":8,"nanos":0},"consecutive":2,"teardown_after":3}},
            {"TransportDropped":{"server":{"server_id":"id","qualified_name":"@test/server","display_name":"Server"},"outcome":{"Broken":{"error":"closed","elapsed":{"secs":0,"nanos":0}}},"consecutive_timeouts":0}},
            {"Reconnected":{"server":{"server_id":"id","qualified_name":"@test/server","display_name":"Server"},"tools":4,"after_failures":1}},
            {"ReconnectFailed":{"server":{"server_id":"id","qualified_name":"@test/server","display_name":"Server"},"error":"unavailable","failures":2,"retry_in":{"secs":10,"nanos":0}}},
            {"Parked":{"server":{"server_id":"id","qualified_name":"@test/server","display_name":"Server"},"error":"runtime missing"}}
        ],
        "dropped":7
    })
}

#[test]
fn ordered_observations_and_loss_counts_round_trip() {
    let server = ServerRef {
        server_id: "id".into(),
        qualified_name: "@test/server".into(),
        display_name: "Server".into(),
    };
    let events = fixture_events(&server);
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
    assert_eq!(wire, expected_batch_wire());
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
}

#[test]
fn probe_outcomes_pin_every_wire_variant() {
    let cases = [
        (
            ProbeOutcome::Missing,
            serde_json::json!("Missing"),
            "missing",
        ),
        (
            ProbeOutcome::Alive {
                elapsed: Duration::from_millis(12),
            },
            serde_json::json!({"Alive":{"elapsed":{"secs":0,"nanos":12_000_000}}}),
            "alive",
        ),
        (
            ProbeOutcome::Broken {
                error: "closed".into(),
                elapsed: Duration::ZERO,
            },
            serde_json::json!({"Broken":{"error":"closed","elapsed":{"secs":0,"nanos":0}}}),
            "broken",
        ),
        (
            ProbeOutcome::TimedOut {
                after: Duration::from_secs(8),
            },
            serde_json::json!({"TimedOut":{"after":{"secs":8,"nanos":0}}}),
            "timed_out",
        ),
    ];
    for (probe, expected, kind) in cases {
        assert_eq!(serde_json::to_value(&probe).unwrap(), expected);
        assert_eq!(
            serde_json::from_value::<ProbeOutcome>(expected).unwrap(),
            probe
        );
        assert_eq!(probe.as_str(), kind);
        assert_eq!(probe.is_alive(), kind == "alive");
    }
}

#[test]
fn future_observations_and_probe_outcomes_decode_as_unknown() {
    assert_eq!(
        serde_json::from_value::<ProbeOutcome>(serde_json::json!({"FutureProbe":{"elapsed":1}}))
            .unwrap(),
        ProbeOutcome::Unknown
    );
    assert_eq!(
        serde_json::from_value::<SupervisorEvent>(
            serde_json::json!({"FutureObservation":{"server_id":"future"}})
        )
        .unwrap(),
        SupervisorEvent::Unknown
    );
    let batch: SupervisorBatch = serde_json::from_value(serde_json::json!({
        "events":[{"FutureObservation":{"server_id":"future"}}],
        "dropped":0
    }))
    .unwrap();
    assert_eq!(batch.events, vec![SupervisorEvent::Unknown]);
    assert_eq!(batch.events[0].server(), None);
    assert!(
        serde_json::from_value::<SupervisorEvent>(serde_json::json!({"ProbeAnswered":{}})).is_err()
    );
}

#[test]
fn server_reference_keeps_host_routing_identity() {
    let installed: InstalledServer=serde_json::from_value(serde_json::json!({"server_id":"id","qualified_name":"@test/server","display_name":"Server","command_kind":"node","command":"node","args":[],"env_keys":[],"installed_at":0,"transport":{"kind":"stdio"},"enabled":true})).unwrap();
    let reference = ServerRef::from(&installed);
    assert_eq!(reference.server_id, "id");
    assert_eq!(reference.qualified_name, "@test/server");
    assert_eq!(reference.display_name, "Server");
}
