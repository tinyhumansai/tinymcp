//! The server callback wire form is stable across host and module.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use serde_json::json;
#[test]
fn callbacks_replies_and_states_round_trip() {
    let callback = ServerCallback {
        id: "opaque".into(),
        call: ServerHostCall::CallTool {
            source_type: "mcp:test".into(),
            headers: serde_json::Map::new(),
            name: "echo".into(),
            arguments: serde_json::Map::new(),
        },
    };
    let state = ServerOperationState::Callback { callback };
    let wire = serde_json::to_value(&state).unwrap();
    assert_eq!(wire["state"], "callback");
    assert_eq!(wire["callback"]["call"]["kind"], "call_tool");
    assert_eq!(
        serde_json::from_value::<ServerOperationState>(wire).unwrap(),
        state
    );
    for reply in [
        ServerHostReply::Success { value: json!({}) },
        ServerHostReply::InvalidParams {
            message: "denied".into(),
        },
        ServerHostReply::Internal {
            message: "unavailable".into(),
        },
        ServerHostReply::ResourceNotFound {
            message: "missing".into(),
        },
    ] {
        assert_eq!(
            serde_json::from_value::<ServerHostReply>(serde_json::to_value(&reply).unwrap())
                .unwrap(),
            reply
        );
    }
}

#[test]
fn callback_debug_never_contains_headers_arguments_or_resource_names() {
    let calls = [
        ServerHostCall::ListTools {
            source_type: "private".into(),
            headers: serde_json::from_value(json!({"authorization":"secret"})).unwrap(),
        },
        ServerHostCall::CallTool {
            source_type: "private".into(),
            headers: serde_json::Map::new(),
            name: "private".into(),
            arguments: serde_json::Map::new(),
        },
        ServerHostCall::ReadResource {
            source_type: "private".into(),
            headers: serde_json::Map::new(),
            uri: "private".into(),
        },
        ServerHostCall::ListPrompts {
            source_type: "private".into(),
            headers: serde_json::Map::new(),
        },
        ServerHostCall::GetPrompt {
            source_type: "private".into(),
            headers: serde_json::Map::new(),
            name: "private".into(),
            arguments: serde_json::Map::new(),
        },
    ];
    for call in calls {
        let debug = format!("{call:?}");
        assert!(!debug.contains("secret"));
        assert!(!debug.contains("private"));
        assert!(debug.contains(".."));
        assert_eq!(
            serde_json::from_value::<ServerHostCall>(serde_json::to_value(&call).unwrap()).unwrap(),
            call
        );
    }
}
