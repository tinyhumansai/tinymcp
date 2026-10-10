//! Cancellation and teardown retain ownership until protocol workers finish.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use super::*;
use serde_json::json;

fn config() -> ServerSessionConfig {
    ServerSessionConfig {
        session_id: next_id(),
        info: json!({"name":"fixture","version":"1"}),
        source_type_prefix: "mcp".into(),
        resources: vec![],
    }
}
#[tokio::test]
async fn abort_is_joined_before_cancel_returns_and_task_failure_releases_operation() {
    let sessions = ServerSessions::default();
    let id = sessions.open(config()).await.unwrap();
    sessions
        .submit(
            &id,
            Map::new(),
            ServerInput {
                operation_id: next_id(),
                line: json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}).to_string(),
            },
        )
        .await
        .unwrap();
    cancel(&sessions, &id).await.unwrap();
    {
        let state = sessions.sessions.lock().await;
        let op = state.active[&id].operation.as_ref().unwrap();
        assert!(op.task.is_none());
        assert!(op.waiting.is_none());
        assert!(op.receiver.is_closed());
    }
    assert_eq!(
        sessions.poll(&id).await.unwrap().state,
        ServerOperationState::Cancelled
    );
    sessions
        .submit(
            &id,
            Map::new(),
            ServerInput {
                operation_id: next_id(),
                line: "{}".into(),
            },
        )
        .await
        .unwrap();
    {
        let mut state = sessions.sessions.lock().await;
        let op = state
            .active
            .get_mut(&id)
            .unwrap()
            .operation
            .as_mut()
            .unwrap();
        op.task.as_ref().unwrap().abort();
    }
    tokio::task::yield_now().await;
    assert!(matches!(
        sessions.poll(&id).await.unwrap().state,
        ServerOperationState::Failed { .. }
    ));
    assert!(matches!(
        sessions.poll(&id).await.unwrap().state,
        ServerOperationState::Failed { .. }
    ));
}
#[tokio::test]
async fn closed_session_slots_are_reclaimed_beyond_previous_lifetime_limit() {
    let sessions = ServerSessions::default();
    let mut first = None;
    for sequence in 1..=4100 {
        let cfg = ServerSessionConfig {
            session_id: uuid::Uuid::from_u128(sequence).to_string(),
            ..config()
        };
        first.get_or_insert_with(|| cfg.clone());
        let id = sessions.open(cfg).await.unwrap();
        sessions.close(&id).await.unwrap();
    }
    assert!(sessions.open(first.unwrap()).await.is_err());
    let state = sessions.sessions.lock().await;
    assert!(state.active.is_empty());
    assert_eq!(state.reserved.len(), ADMISSION_WINDOW as usize);
}

#[tokio::test]
async fn operations_reclaim_history_beyond_previous_lifetime_limit_without_replaying() {
    let sessions = ServerSessions::default();
    let id = sessions.open(config()).await.unwrap();
    let first = ServerInput {
        operation_id: uuid::Uuid::from_u128(1).to_string(),
        line: "{}".into(),
    };
    for sequence in 1..=4100 {
        let input = ServerInput {
            operation_id: uuid::Uuid::from_u128(sequence).to_string(),
            line: "{}".into(),
        };
        sessions
            .submit(&id, Map::new(), input.clone())
            .await
            .unwrap();
        sessions.cancel(&id, &input.operation_id).await.unwrap();
        let discarded = sessions.poll(&id).await.unwrap();
        sessions.submit(&id, Map::new(), input).await.unwrap();
        assert_eq!(sessions.poll(&id).await.unwrap(), discarded);
    }
    assert!(
        sessions
            .submit(&id, Map::new(), first.clone())
            .await
            .is_err()
    );
    assert!(sessions.cancel(&id, &first.operation_id).await.is_err());
}

#[tokio::test]
async fn static_batch_budget_stops_before_later_host_dispatch() {
    let sessions = ServerSessions::default();
    let cfg = ServerSessionConfig {
        resources: vec![
            json!({"uri":"fixture://large","name":"large","description":"x".repeat(40_000)}),
        ],
        ..config()
    };
    let id = sessions.open(cfg).await.unwrap();
    let mut batch = vec![json!({"jsonrpc":"2.0","id":1,"method":"resources/list"}); 256];
    batch.push(json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}));
    batch.remove(0);
    sessions
        .submit(
            &id,
            Map::new(),
            ServerInput {
                operation_id: next_id(),
                line: json!(batch).to_string(),
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        visible(&sessions, &id).await.state,
        ServerOperationState::Failed { .. }
    ));
}

async fn visible(sessions: &ServerSessions, id: &str) -> ServerOperationSnapshot {
    loop {
        let state = sessions.poll(id).await.unwrap();
        if state.state != ServerOperationState::Pending {
            return state;
        }
        tokio::task::yield_now().await;
    }
}
#[tokio::test]
async fn discarded_terminal_replies_and_retry_submit_never_repeat_host_side_effects() {
    let sessions = ServerSessions::default();
    let id = sessions.open(config()).await.unwrap();
    let input = ServerInput {
        operation_id: next_id(),
        line: json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"write"}})
            .to_string(),
    };
    sessions
        .submit(&id, Map::new(), input.clone())
        .await
        .unwrap();
    sessions
        .submit(&id, Map::new(), input.clone())
        .await
        .unwrap();
    let ServerOperationState::Callback { callback } = visible(&sessions, &id).await.state else {
        panic!("callback missing")
    };
    assert!(
        sessions
            .submit(
                &id,
                Map::new(),
                ServerInput {
                    operation_id: input.operation_id.clone(),
                    line: "different".into()
                }
            )
            .await
            .is_err()
    );
    sessions
        .complete(
            &id,
            &callback.id,
            ServerHostReply::Success {
                value: json!({"content":[],"write_count":1}),
            },
        )
        .await
        .unwrap();
    let discarded = visible(&sessions, &id).await;
    assert!(matches!(
        discarded.state,
        ServerOperationState::Complete { .. }
    ));
    assert_eq!(sessions.poll(&id).await.unwrap(), discarded);
    sessions
        .submit(&id, Map::new(), input.clone())
        .await
        .unwrap();
    assert_eq!(sessions.poll(&id).await.unwrap(), discarded);
    let next = ServerInput {
        operation_id: next_id(),
        line: "{}".into(),
    };
    sessions
        .submit(&id, Map::new(), next.clone())
        .await
        .unwrap();
    cancel(&sessions, &id).await.unwrap();
    let cancelled = sessions.poll(&id).await.unwrap();
    assert_eq!(cancelled.state, ServerOperationState::Cancelled);
    assert_eq!(sessions.poll(&id).await.unwrap(), cancelled);
    sessions.submit(&id, Map::new(), next).await.unwrap();
    assert_eq!(sessions.poll(&id).await.unwrap(), cancelled);
    assert!(sessions.submit(&id, Map::new(), input).await.is_err());
}
#[tokio::test]
async fn aggregate_callback_replies_and_batch_counts_are_bounded() {
    let sessions = ServerSessions::default();
    let id = sessions.open(config()).await.unwrap();
    let batch =
        vec![json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"read"}}); 2];
    sessions
        .submit(
            &id,
            Map::new(),
            ServerInput {
                operation_id: next_id(),
                line: json!(batch).to_string(),
            },
        )
        .await
        .unwrap();
    for index in 0..2 {
        let ServerOperationState::Callback { callback } = visible(&sessions, &id).await.state
        else {
            panic!("callback missing")
        };
        let result = sessions
            .complete(
                &id,
                &callback.id,
                ServerHostReply::Success {
                    value: json!({"text":"x".repeat(600_000)}),
                },
            )
            .await;
        if index == 0 {
            result.unwrap();
        } else {
            assert!(result.is_err());
        }
    }
    cancel(&sessions, &id).await.unwrap();
    assert!(
        sessions
            .submit(
                &id,
                Map::new(),
                ServerInput {
                    operation_id: next_id(),
                    line: json!(vec![json!({}); 257]).to_string()
                }
            )
            .await
            .is_err()
    );
    assert!(
        sessions
            .submit(
                &id,
                Map::new(),
                ServerInput {
                    operation_id: "invalid".into(),
                    line: "{}".into()
                }
            )
            .await
            .is_err()
    );
}

async fn cancel(sessions: &ServerSessions, id: &str) -> crate::Result<()> {
    let operation_id = sessions
        .poll(id)
        .await
        .map_or_else(|_| String::new(), |state| state.operation_id);
    sessions.cancel(id, &operation_id).await
}

#[tokio::test]
async fn stale_cancel_cannot_abort_a_successor_operation() {
    let sessions = ServerSessions::default();
    let id = sessions.open(config()).await.unwrap();
    let first = ServerInput {
        operation_id: next_id(),
        line: "{}".into(),
    };
    sessions
        .submit(&id, Map::new(), first.clone())
        .await
        .unwrap();
    sessions.cancel(&id, &first.operation_id).await.unwrap();
    let next = ServerInput {
        operation_id: next_id(),
        line: json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}).to_string(),
    };
    sessions
        .submit(&id, Map::new(), next.clone())
        .await
        .unwrap();
    assert!(sessions.cancel(&id, &first.operation_id).await.is_err());
    let current = visible(&sessions, &id).await;
    assert_eq!(current.operation_id, next.operation_id);
    assert!(matches!(
        current.state,
        ServerOperationState::Callback { .. }
    ));
    sessions.cancel(&id, &next.operation_id).await.unwrap();
}

fn next_id() -> String {
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    uuid::Uuid::from_u128(u128::from(
        SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::SeqCst),
    ))
    .to_string()
}

#[tokio::test]
async fn admission_window_allows_reordered_opens_and_close_only_retires_its_identifier() {
    let sessions = ServerSessions::default();
    let lower = ServerSessionConfig {
        session_id: uuid::Uuid::from_u128(1).to_string(),
        ..config()
    };
    let cancelled = ServerSessionConfig {
        session_id: uuid::Uuid::from_u128(2).to_string(),
        ..config()
    };
    let higher = ServerSessionConfig {
        session_id: uuid::Uuid::from_u128(3).to_string(),
        ..config()
    };
    sessions.open(higher.clone()).await.unwrap();
    sessions.close(&cancelled.session_id).await.unwrap();
    sessions.open(lower.clone()).await.unwrap();
    assert!(sessions.open(cancelled).await.is_err());
    let distant = ServerSessionConfig {
        session_id: uuid::Uuid::from_u128(10_000).to_string(),
        ..config()
    };
    sessions.open(distant).await.unwrap();
    // Active retries remain recoverable even after their admission window rolls away.
    sessions.open(lower.clone()).await.unwrap();
    sessions.close(&lower.session_id).await.unwrap();
    assert!(sessions.open(lower).await.is_err());
    assert_eq!(sessions.shutdown().await, 2);
    let state = sessions.sessions.lock().await;
    assert!(state.active.is_empty());
    assert!(state.reserved.len() <= ADMISSION_WINDOW as usize);
}
