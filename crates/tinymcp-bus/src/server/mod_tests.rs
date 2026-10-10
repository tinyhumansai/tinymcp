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

#[test]
fn shared_server_declarations_headers_context_and_errors_pin_wire_forms() {
    let info = ServerInfo::new("fixture", "1");
    assert_eq!(
        serde_json::to_value(&info).unwrap(),
        serde_json::json!({"name":"fixture","version":"1","instructions":null})
    );
    let tool = ServerToolSpec::new("echo", "echo", serde_json::json!({}));
    let wire = serde_json::json!({"name":"echo","title":null,"description":"echo","inputSchema":{},"annotations":null});
    assert_eq!(serde_json::to_value(&tool).unwrap(), wire);
    assert_eq!(
        serde_json::from_value::<ServerToolSpec>(wire).unwrap(),
        tool
    );
    let resource = ResourceSpec::new("fixture://info", "info");
    assert_eq!(
        serde_json::to_value(resource).unwrap(),
        serde_json::json!({"uri":"fixture://info","name":"info","description":null,"mimeType":null})
    );
    let headers = RequestHeaders {
        entries: std::collections::BTreeMap::from([(
            "authorization".into(),
            "fixture-secret".into(),
        )]),
    };
    assert!(!format!("{headers:?}").contains("fixture-secret"));
    let ctx = RequestContext::new("mcp", headers);
    let wire =
        serde_json::json!({"source_type":"mcp","headers":{"authorization":"fixture-secret"}});
    assert_eq!(serde_json::to_value(&ctx).unwrap(), wire);
    assert_eq!(serde_json::from_value::<RequestContext>(wire).unwrap(), ctx);
    for error in [
        ToolCallError::InvalidParams("denied".into()),
        ToolCallError::Internal("failed".into()),
        ToolCallError::ResourceNotFound("missing".into()),
    ] {
        assert_eq!(
            serde_json::from_value::<ToolCallError>(serde_json::to_value(&error).unwrap()).unwrap(),
            error
        );
        assert_eq!(error.to_string(), error.message());
    }
}
