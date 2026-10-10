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
    assert_eq!(
        wire,
        json!({
            "state":"callback",
            "callback":{
                "id":"opaque",
                "call":{
                    "kind":"call_tool",
                    "source_type":"mcp:test",
                    "headers":{},
                    "name":"echo",
                    "arguments":{}
                }
            }
        })
    );
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
fn future_server_callback_kinds_and_operation_states_are_ignored() {
    let callback: ServerOperationState = serde_json::from_value(json!({
        "state":"callback",
        "callback":{"id":"opaque","call":{"kind":"future_call","data":{"x":1}}}
    }))
    .unwrap();
    assert_eq!(
        callback,
        ServerOperationState::Callback {
            callback: ServerCallback {
                id: "opaque".into(),
                call: ServerHostCall::Unknown,
            }
        }
    );
    assert_eq!(
        serde_json::from_value::<ServerOperationState>(json!({
            "state":"future_state",
            "data":{"x":1}
        }))
        .unwrap(),
        ServerOperationState::Unknown
    );
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
        let expected = match &call {
            ServerHostCall::ListTools {
                source_type,
                headers,
            } => json!({"kind":"list_tools","source_type":source_type,"headers":headers}),
            ServerHostCall::CallTool {
                source_type,
                headers,
                name,
                arguments,
            } => {
                json!({"kind":"call_tool","source_type":source_type,"headers":headers,"name":name,"arguments":arguments})
            }
            ServerHostCall::ReadResource {
                source_type,
                headers,
                uri,
            } => {
                json!({"kind":"read_resource","source_type":source_type,"headers":headers,"uri":uri})
            }
            ServerHostCall::ListPrompts {
                source_type,
                headers,
            } => json!({"kind":"list_prompts","source_type":source_type,"headers":headers}),
            ServerHostCall::GetPrompt {
                source_type,
                headers,
                name,
                arguments,
            } => {
                json!({"kind":"get_prompt","source_type":source_type,"headers":headers,"name":name,"arguments":arguments})
            }
            ServerHostCall::Unknown => json!({"kind":"unknown"}),
        };
        assert_eq!(serde_json::to_value(&call).unwrap(), expected);
        assert_eq!(
            serde_json::from_value::<ServerHostCall>(serde_json::to_value(&call).unwrap()).unwrap(),
            call
        );
    }
}

#[test]
fn server_operation_payloads_keep_explicit_complete_wire_forms() {
    let session = ServerSessionConfig {
        session_id: "session-1".into(),
        info: json!({"name":"server","version":"1"}),
        source_type_prefix: "mcp".into(),
        resources: vec![json!({"uri":"fixture://info","name":"info"})],
    };
    assert_eq!(
        serde_json::to_value(&session).unwrap(),
        json!({
            "session_id":"session-1",
            "info":{"name":"server","version":"1"},
            "source_type_prefix":"mcp",
            "resources":[{"uri":"fixture://info","name":"info"}]
        })
    );

    let input = ServerInput {
        operation_id: "operation-1".into(),
        line: "{}".into(),
    };
    assert_eq!(
        serde_json::to_value(&input).unwrap(),
        json!({"operation_id":"operation-1","line":"{}"})
    );
    let snapshot = ServerOperationSnapshot {
        operation_id: input.operation_id.clone(),
        state: ServerOperationState::Complete { response: None },
    };
    assert_eq!(
        serde_json::to_value(&snapshot).unwrap(),
        json!({"operation_id":"operation-1","state":{"state":"complete","response":null}})
    );
    for (state, expected) in [
        (ServerOperationState::Pending, json!({"state":"pending"})),
        (
            ServerOperationState::Cancelled,
            json!({"state":"cancelled"}),
        ),
        (
            ServerOperationState::Failed {
                message: "failed".into(),
            },
            json!({"state":"failed","message":"failed"}),
        ),
        (ServerOperationState::Unknown, json!({"state":"unknown"})),
    ] {
        assert_eq!(serde_json::to_value(state).unwrap(), expected);
    }
    let reference = ServerOperationRef {
        session_id: "session-1".into(),
        operation_id: "operation-1".into(),
    };
    assert_eq!(
        serde_json::to_value(&reference).unwrap(),
        json!({"session_id":"session-1","operation_id":"operation-1"})
    );

    for (reply, expected) in [
        (
            ServerHostReply::Success {
                value: json!({"ok":true}),
            },
            json!({"kind":"success","value":{"ok":true}}),
        ),
        (
            ServerHostReply::InvalidParams {
                message: "bad".into(),
            },
            json!({"kind":"invalid_params","message":"bad"}),
        ),
        (
            ServerHostReply::Internal {
                message: "failed".into(),
            },
            json!({"kind":"internal","message":"failed"}),
        ),
        (
            ServerHostReply::ResourceNotFound {
                message: "missing".into(),
            },
            json!({"kind":"resource_not_found","message":"missing"}),
        ),
    ] {
        assert_eq!(serde_json::to_value(reply).unwrap(), expected);
    }
}

#[test]
fn shared_server_declarations_headers_context_and_errors_pin_wire_forms() {
    let info = ServerInfo::new("fixture", "1");
    assert_eq!(
        serde_json::to_value(&info).unwrap(),
        serde_json::json!({"name":"fixture","version":"1"})
    );
    let tool = ServerToolSpec::new("echo", "echo", serde_json::json!({}));
    let wire = serde_json::json!({"name":"echo","description":"echo","inputSchema":{}});
    assert_eq!(serde_json::to_value(&tool).unwrap(), wire);
    assert_eq!(
        serde_json::from_value::<ServerToolSpec>(wire).unwrap(),
        tool
    );
    let resource = ResourceSpec::new("fixture://info", "info");
    assert_eq!(
        serde_json::to_value(resource).unwrap(),
        serde_json::json!({"uri":"fixture://info","name":"info"})
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
