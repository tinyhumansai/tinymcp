//! Cancellation and teardown retain ownership until protocol workers finish.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use super::*;
use serde_json::json;

fn config() -> ServerSessionConfig {
    ServerSessionConfig {
        session_id: uuid::Uuid::new_v4().to_string(),
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
                operation_id: uuid::Uuid::new_v4().to_string(),
                line: json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}).to_string(),
            },
        )
        .await
        .unwrap();
    cancel(&sessions, &id).await.unwrap();
    {
        let state = sessions.sessions.lock().await;
        let op = state[&id].as_ref().unwrap().operation.as_ref().unwrap();
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
                operation_id: uuid::Uuid::new_v4().to_string(),
                line: "{}".into(),
            },
        )
        .await
        .unwrap();
    {
        let mut state = sessions.sessions.lock().await;
        let op = state
            .get_mut(&id)
            .unwrap()
            .as_mut()
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
async fn reservation_limit_bounds_tombstones_and_refuses_new_late_starts() {
    let sessions = ServerSessions::default();
    for _ in 0..4096 {
        sessions
            .close(&uuid::Uuid::new_v4().to_string())
            .await
            .unwrap();
    }
    assert!(sessions.open(config()).await.is_err());
    assert!(
        sessions
            .close(&uuid::Uuid::new_v4().to_string())
            .await
            .is_err()
    );
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
        operation_id: uuid::Uuid::new_v4().to_string(),
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
        operation_id: uuid::Uuid::new_v4().to_string(),
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
                operation_id: uuid::Uuid::new_v4().to_string(),
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
                    operation_id: uuid::Uuid::new_v4().to_string(),
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
        operation_id: uuid::Uuid::new_v4().to_string(),
        line: "{}".into(),
    };
    sessions
        .submit(&id, Map::new(), first.clone())
        .await
        .unwrap();
    sessions.cancel(&id, &first.operation_id).await.unwrap();
    let next = ServerInput {
        operation_id: uuid::Uuid::new_v4().to_string(),
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
