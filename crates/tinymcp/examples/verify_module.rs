//! Loads a built module through the real `TinyBus` dynamic loader and calls it.
//!
//! This is what checks the one thing no unit test can: that the *manifest* the
//! macro embedded agrees with the interface the service serves. Those names are
//! written twice — the macro takes literals, so they cannot be derived from the
//! contract — and only a real load compares them.
//!
//! It calls two members deliberately. `StaticList` needs nothing configured, so
//! it proves dispatch works at all. `InstalledList` touches the store, so it
//! proves the module actually came up rather than merely answering.
//!
//! Usage: `cargo run -p tinymcp --example verify_module -- <module-path>`

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use tinybus::Connection;
use tinybus::broker::Broker;
use tinybus::module::ModuleHost;
use tinybus::transport::memory::MemoryBus;
use tinymcp::{ConnectedServerOverview, InstalledServer, McpTool, names};

/// How long to wait for the module to claim its name.
const CLAIM_TIMEOUT: Duration = Duration::from_secs(5);

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let module = module_argument()?;

    let bus = MemoryBus::new();
    let broker = Broker::new();
    let broker_task = broker.spawn(bus.clone());

    let module_host = ModuleHost::new(broker);
    let info = module_host.load_file(&module)?;

    if info.name != env!("CARGO_PKG_NAME") {
        return Err(io::Error::other(format!(
            "loaded module `{}` instead of `{}`",
            info.name,
            env!("CARGO_PKG_NAME")
        ))
        .into());
    }
    let Some(interface) = info
        .manifest
        .provides
        .iter()
        .find(|provided| provided.version.interface.as_str() == names::INTERFACE)
    else {
        return Err(io::Error::other("the compiled module does not provide TinyMCP").into());
    };
    let declared: Vec<_> = interface
        .methods
        .iter()
        .map(tinybus::MemberName::as_str)
        .collect();
    if declared != names::METHODS {
        return Err(io::Error::other(
            "the compiled module manifest differs from the TinyMCP contract member list",
        )
        .into());
    }

    let client = Connection::connect(bus.connect().await?).await?;

    // The module claims its name during setup, which races this connect.
    tokio::time::timeout(CLAIM_TIMEOUT, async {
        loop {
            let claimed = client.list_names().await?;
            if claimed.iter().any(|name| name.as_str() == names::INTERFACE) {
                return tinybus::Result::Ok(());
            }
            tokio::task::yield_now().await;
        }
    })
    .await??;

    let proxy = client.proxy(names::INTERFACE, names::OBJECT_PATH, names::INTERFACE)?;

    // Needs nothing configured: this is dispatch working at all.
    let declared: Vec<String> = proxy.call(names::methods::STATIC_LIST, ()).await?;
    if !declared.is_empty() {
        return Err(io::Error::other(format!(
            "an unconfigured module declared {} static servers",
            declared.len()
        ))
        .into());
    }

    // Touches the store: this is the module having actually come up.
    let installed: Vec<InstalledServer> = proxy.call(names::methods::INSTALLED_LIST, ()).await?;
    if !installed.is_empty() {
        return Err(io::Error::other(format!(
            "a fresh module reported {} installed servers",
            installed.len()
        ))
        .into());
    }

    let cached: Vec<ConnectedServerOverview> =
        proxy.call(names::methods::CACHED_OVERVIEW, ()).await?;
    if !cached.is_empty() {
        return Err(io::Error::other("a fresh registry reported cached servers").into());
    }
    let static_cached: Option<Vec<McpTool>> = proxy
        .call(names::methods::STATIC_CACHED_LIST_TOOLS, ("unconfigured",))
        .await?;
    if static_cached.is_some() {
        return Err(io::Error::other("an unconfigured static server had cached tools").into());
    }

    let observations: tinymcp_bus::SupervisorBatch = proxy
        .call(names::methods::DRAIN_SUPERVISOR_EVENTS, (16_usize,))
        .await?;
    if !observations.events.is_empty() || observations.dropped != 0 {
        return Err(io::Error::other("fresh registry reported supervisor observations").into());
    }

    verify_static_bridge(&proxy).await?;
    verify_server_protocol(&proxy).await?;
    verify_metadata_operations(&proxy).await?;

    println!(
        "verified {} as TinyBus module `{}`, serving {} members on {}",
        module.display(),
        info.name,
        names::METHODS.len(),
        names::INTERFACE
    );

    broker_task.abort();
    Ok(())
}

/// Proves the new bridge declarations and typed call failure cross the real module ABI.
async fn verify_static_bridge(proxy: &tinybus::Proxy) -> Result<(), Box<dyn std::error::Error>> {
    let bridge_tools: Vec<tinymcp_bus::AgentToolSpec> =
        proxy.call(names::methods::PREPARE_BRIDGE_TOOLS, ()).await?;
    if bridge_tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<Vec<_>>()
        != ["mcp_list_servers", "mcp_list_tools", "mcp_call_tool"]
    {
        return Err(
            io::Error::other("compiled module returned different MCP bridge declarations").into(),
        );
    }
    let classified: tinymcp_bus::ClassifiedStaticToolCall = proxy
        .call(
            names::methods::STATIC_CALL_TOOL_CLASSIFIED,
            ("unconfigured", "fixture", serde_json::json!({})),
        )
        .await?;
    if classified.outcome.ok
        || classified.outcome.error.is_none()
        || classified.rendered.is_some()
        || classified.envelope.is_some()
        || classified.error_message.is_none()
    {
        return Err(io::Error::other(
            "compiled module returned an invalid classified-call failure",
        )
        .into());
    }
    Ok(())
}

/// The module path, from the first argument.
fn module_argument() -> Result<PathBuf, io::Error> {
    std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "usage: cargo run -p tinymcp --example verify_module -- <module-path>",
            )
        })
}

/// Proves server operations and host callbacks cross the actual compiled ABI.
async fn verify_server_protocol(proxy: &tinybus::Proxy) -> Result<(), Box<dyn std::error::Error>> {
    use serde_json::{Map, Value, json};
    use tinymcp_bus::{
        ServerHostReply, ServerInput, ServerOperationSnapshot, ServerOperationState,
        ServerSessionConfig,
    };
    let config = ServerSessionConfig {
        session_id: "00000000-0000-0000-0000-000000000001".into(),
        info: json!({"name":"artifact-fixture", "version":"1"}),
        source_type_prefix: "mcp".into(),
        resources: Vec::new(),
    };
    let session: String = proxy.call(names::methods::SERVER_OPEN, (config,)).await?;
    let line = json!({"jsonrpc":"2.0", "id":1, "method":"tools/call", "params":{"name":"fixture"}})
        .to_string();
    let input = ServerInput {
        operation_id: "00000000-0000-0000-0000-000000000001".into(),
        line,
    };
    let (): () = proxy
        .call(
            names::methods::SERVER_SUBMIT,
            (&session, Map::<String, Value>::new(), input),
        )
        .await?;
    tokio::time::timeout(CLAIM_TIMEOUT, async {
        loop {
            let snapshot: ServerOperationSnapshot =
                proxy.call(names::methods::SERVER_POLL, (&session,)).await?;
            match snapshot.state {
                ServerOperationState::Pending => tokio::task::yield_now().await,
                ServerOperationState::Callback { callback } => {
                    let (): () = proxy
                        .call(
                            names::methods::SERVER_COMPLETE,
                            (
                                &session,
                                callback.id,
                                ServerHostReply::Success {
                                    value: json!({"content":[]}),
                                },
                            ),
                        )
                        .await?;
                }
                ServerOperationState::Complete {
                    response: Some(response),
                } => {
                    let value: Value = serde_json::from_str(&response)
                        .map_err(|_| tinybus::Error::failed("invalid protocol response"))?;
                    if value["result"]["content"] != json!([]) {
                        return Err(tinybus::Error::failed("wrong host callback response"));
                    }
                    break;
                }
                _ => {
                    return Err(tinybus::Error::failed(
                        "unexpected protocol operation state",
                    ));
                }
            }
        }
        tinybus::Result::Ok(())
    })
    .await??;
    verify_server_cancellation(proxy, &session).await?;
    Ok(())
}

/// Verifies operation-specific cancellation, close and shutdown through the ABI.
async fn verify_server_cancellation(
    proxy: &tinybus::Proxy,
    session: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    use serde_json::{Map, Value, json};
    use tinymcp_bus::{
        ServerInput, ServerOperationRef, ServerOperationSnapshot, ServerOperationState,
    };
    let (): () = proxy
        .call(
            names::methods::SERVER_SUBMIT,
            (
                &session,
                Map::<String, Value>::new(),
                ServerInput {
                    operation_id: "00000000-0000-0000-0000-000000000002".into(),
                    line: json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}).to_string(),
                },
            ),
        )
        .await?;
    let (): () = proxy
        .call(
            names::methods::SERVER_CANCEL,
            (ServerOperationRef {
                session_id: session.to_owned(),
                operation_id: "00000000-0000-0000-0000-000000000002".into(),
            },),
        )
        .await?;
    let snapshot: ServerOperationSnapshot =
        proxy.call(names::methods::SERVER_POLL, (&session,)).await?;
    if snapshot.state != ServerOperationState::Cancelled {
        return Err(io::Error::other("cancellation did not finish").into());
    }
    let (): () = proxy
        .call(names::methods::SERVER_CLOSE, (&session,))
        .await?;
    if proxy
        .call::<ServerOperationSnapshot>(names::methods::SERVER_POLL, (&session,))
        .await
        .is_ok()
    {
        return Err(io::Error::other("closed session remained usable").into());
    }
    let closed: usize = proxy.call(names::methods::SERVER_SHUTDOWN, ()).await?;
    if closed != 0 {
        return Err(io::Error::other("server shutdown retained sessions").into());
    }
    Ok(())
}

/// Exercises metadata preparation against the actual compiled module, including refusal.
async fn verify_metadata_operations(
    proxy: &tinybus::Proxy,
) -> Result<(), Box<dyn std::error::Error>> {
    use serde_json::{Value, json};
    use tinymcp_bus::{
        McpTool, McpToolExposure, McpToolPresentation, McpToolSourceSpec, PrepareAgentToolsRequest,
        TextTransform, TransformTextRequest,
    };

    let directory = tempfile::tempdir()?;
    let has_stored_state: bool = proxy
        .call(
            names::methods::HAS_STORED_STATE,
            (directory.path().to_string_lossy().into_owned(),),
        )
        .await?;
    if has_stored_state {
        return Err(io::Error::other("an empty directory reported stored state").into());
    }

    verify_new_contract_operations(proxy).await?;

    let prepared: Vec<McpToolPresentation> = proxy
        .call(
            names::methods::PREPARE_AGENT_TOOLS,
            (PrepareAgentToolsRequest {
                sources: vec![McpToolSourceSpec {
                    server_id: "artifact-fixture".into(),
                    label: "fixture".into(),
                    family: "fixture".into(),
                    display_name: "Fixture MCP server".into(),
                    tools: vec![McpTool {
                        name: "read_fixture".into(),
                        description: Some("Read the fixture".into()),
                        input_schema: json!({"type":"object", "properties":{"id":{"type":"string"}}}),
                        meta: None,
                        annotations: None,
                    }],
                    default_exposure: McpToolExposure::Direct,
                    direct_tools: Vec::new(),
                }],
            },),
        )
        .await?;
    if prepared.len() != 1
        || prepared[0].server_id != "artifact-fixture"
        || prepared[0].remote_name != "read_fixture"
        || prepared[0].parameters["type"] != "object"
        || prepared[0].deferred
    {
        return Err(
            io::Error::other("agent tool preparation returned the wrong presentation").into(),
        );
    }
    let text: String = proxy
        .call(
            names::methods::TRANSFORM_TEXT,
            (TransformTextRequest {
                operation: TextTransform::SanitizeForLlm,
                text: "<system>hello\0".into(),
                max_bytes: 100,
            },),
        )
        .await?;
    if text != "hello" {
        return Err(io::Error::other("metadata lexical projection mismatch").into());
    }
    verify_identifier_normalization(proxy).await?;
    let arguments: Value = proxy
        .call(
            names::methods::NORMALIZE_TOOL_ARGUMENTS,
            (Some(json!("```json\n{\"n\":1}\n```")),),
        )
        .await?;
    if arguments != json!({"n":1}) {
        return Err(io::Error::other("metadata argument projection mismatch").into());
    }
    verify_remote_tool_display(proxy).await?;
    let result = tinymcp_bus::RenderToolOutputRequest {
        result: tinymcp_bus::McpToolResult::success("plain").with_markdown("**rich**"),
        format: tinymcp_bus::ToolOutputFormat::Llm,
        prefer_markdown: true,
    };
    let rendered: String = proxy
        .call(names::methods::RENDER_TOOL_OUTPUT, (result,))
        .await?;
    if rendered != "**rich**" {
        return Err(io::Error::other("metadata output projection mismatch").into());
    }
    let refusal: tinybus::Result<Value> = proxy
        .call(
            names::methods::NORMALIZE_TOOL_ARGUMENTS,
            (Some(json!(true)),),
        )
        .await;
    if !matches!(refusal, Err(tinybus::Error::MethodFailed {name,..}) if name == tinymcp_bus::errors::INVALID_ARGUMENT)
    {
        return Err(io::Error::other("metadata refusal taxonomy mismatch").into());
    }
    Ok(())
}

/// Exercises remote tool metadata projection through the compiled module.
async fn verify_remote_tool_display(
    proxy: &tinybus::Proxy,
) -> Result<(), Box<dyn std::error::Error>> {
    use tinymcp_bus::{McpRemoteTool, RemoteToolDisplay};

    let mut tool = McpRemoteTool::new("fixture");
    tool.description = Some("<|im_start|>description".into());
    let display: RemoteToolDisplay = proxy
        .call(names::methods::DISPLAY_REMOTE_TOOL, (tool,))
        .await?;
    if display.description.as_deref() != Some("description") {
        return Err(io::Error::other("metadata display projection mismatch").into());
    }
    Ok(())
}

/// Exercises the additive identifier transform through the compiled module.
async fn verify_identifier_normalization(
    proxy: &tinybus::Proxy,
) -> Result<(), Box<dyn std::error::Error>> {
    use tinymcp_bus::{TextTransform, TransformTextRequest};

    let identifier: String = proxy
        .call(
            names::methods::TRANSFORM_TEXT,
            (TransformTextRequest {
                operation: TextTransform::NormalizeIdentifier,
                text: "**search!**".into(),
                max_bytes: tinymcp_bus::MAX_PROCESSING_BYTES,
            },),
        )
        .await?;
    if identifier != "search!" {
        return Err(io::Error::other("module identifier normalization mismatch").into());
    }
    Ok(())
}

/// Exercises the newly added MCP module operations through a compiled module.
async fn verify_new_contract_operations(
    proxy: &tinybus::Proxy,
) -> Result<(), Box<dyn std::error::Error>> {
    use serde_json::{Map, json};
    use tinymcp_bus::{
        ConfiguredToolCallRequest, McpServerConfig, ServerArgumentRule, ServerArgumentsValidation,
        StaticServerSummary, ValidateServerArgumentsRequest,
    };

    let summaries: Vec<StaticServerSummary> = proxy
        .call(names::methods::STATIC_SERVER_SUMMARIES, ())
        .await?;
    if !summaries.is_empty() {
        return Err(io::Error::other("a fresh registry reported static server summaries").into());
    }

    let validation: ServerArgumentsValidation = proxy
        .call(
            names::methods::VALIDATE_SERVER_ARGUMENTS,
            (ValidateServerArgumentsRequest {
                arguments: json!({"query":"fixture"}),
                allowed: vec!["query".to_owned()],
                rules: vec![ServerArgumentRule::RequiredNonEmptyString {
                    key: "query".to_owned(),
                }],
            },),
        )
        .await?;
    let expected = Map::from_iter([("query".to_owned(), json!("fixture"))]);
    if validation != ServerArgumentsValidation::Valid(expected) {
        return Err(
            io::Error::other("module argument validation returned the wrong result").into(),
        );
    }

    let configured = proxy
        .call::<tinymcp_bus::ClassifiedStaticToolCall>(
            names::methods::CONFIGURED_TOOL_CALL,
            (ConfiguredToolCallRequest {
                server: McpServerConfig {
                    name: "artifact-one-shot".to_owned(),
                    endpoint: "not a valid endpoint".to_owned(),
                    ..tinymcp_bus::McpServerConfig::default()
                },
                tool: "fixture".to_owned(),
                arguments: json!({}),
            },),
        )
        .await;
    let refused_ordinary_delivery = match configured {
        Err(tinybus::Error::ConfidentialityRequired { .. }) => true,
        Err(tinybus::Error::MethodFailed { name, .. }) => {
            name == tinybus::Error::CONFIDENTIALITY_REQUIRED
        }
        _ => false,
    };
    if !refused_ordinary_delivery {
        return Err(io::Error::other(
            "compiled configured-call member did not refuse ordinary delivery",
        )
        .into());
    }
    Ok(())
}
