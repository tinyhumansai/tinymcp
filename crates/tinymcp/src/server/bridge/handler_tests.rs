//! Closed, cancelled and oversized host callback channels fail without executing.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use super::*;
use crate::server::RequestHeaders;
use serde_json::json;
fn handler(sender: mpsc::Sender<WaitingCallback>) -> Arc<CallbackHandler> {
    Arc::new(CallbackHandler {
        info: ServerInfo::new("fixture", "1"),
        prefix: "mcp".into(),
        resources: vec![],
        sender,
    })
}
#[tokio::test]
async fn callback_channel_faults_are_internal_errors() {
    let (sender, receiver) = mpsc::channel(1);
    let handler = handler(sender);
    assert_eq!(handler.source_type_prefix(), "mcp");
    let ctx = RequestContext::new("mcp", RequestHeaders::new());
    drop(receiver);
    assert!(
        handler
            .call_tool(&ctx, "fixture", Map::new())
            .await
            .is_err()
    );
    assert_eq!(handler.list_tools(&ctx).await, Vec::new());
    assert!(
        handler
            .call_tool(&ctx, &"x".repeat(super::super::MAX_BYTES), Map::new())
            .await
            .is_err()
    );
}
#[tokio::test]
async fn dropped_reply_sender_wakes_host_waiter() {
    let (sender, mut receiver) = mpsc::channel(1);
    let handler = handler(sender);
    let task = tokio::spawn(async move {
        handler
            .request(ServerHostCall::ReadResource {
                source_type: "mcp".into(),
                headers: Map::new(),
                uri: "local://fixture".into(),
            })
            .await
    });
    drop(receiver.recv().await.unwrap());
    assert!(matches!(
        task.await.unwrap(),
        Err(ToolCallError::Internal(_))
    ));
}
#[tokio::test]
async fn host_returned_internal_errors_remain_protocol_errors() {
    let (sender, mut receiver) = mpsc::channel(1);
    let handler = handler(sender);
    let task = tokio::spawn(async move {
        handler
            .request(ServerHostCall::ListTools {
                source_type: "mcp".into(),
                headers: Map::new(),
            })
            .await
    });
    let waiting = receiver.recv().await.unwrap();
    waiting
        .reply
        .send(ServerHostReply::Success { value: json!([]) })
        .unwrap();
    assert_eq!(task.await.unwrap().unwrap(), json!([]));
}
