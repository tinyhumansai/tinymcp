//! The generic MCP bridge: three tools over one configured-server registry.
//!
//! [`McpListServersTool`], [`McpListToolsTool`] and [`McpCallTool`] let a
//! model discover the servers a host configured, browse one server's tools,
//! and call one by name, without a tool per remote tool (that is
//! [`crate::tools::McpServerTool`]). Every string that leaves them passes
//! through a [`SecretScrubber`] for the server involved, and server listings
//! never carry credentials or endpoint query strings.
//!
//! What stays with the host is the decision to allow a call at all:
//! [`McpCallTool`] takes an [`ActGate`] it runs before anything is sent.
//!
//! Every result [`McpCallTool`] returns once the act gate has allowed the call
//! and it has a server, a tool and an `arguments` value carries a
//! [`McpCallOutcome`] in its metadata: inside an [`McpResultEnvelope`] when the
//! server answered, bare when it did not. [`McpCallOutcome::from_metadata`]
//! reads both. A call missing one of those, or refused by the gate, fails
//! before any result exists and carries none. The outcome says whether the
//! server answered and, when it did not, the error's wire name and whether it
//! was a 401 that advertised OAuth. The model never sees it; a host reads it
//! to meter calls and to surface failures without parsing the result text.

// The tool names and descriptions are fixed strings, and the rendered Markdown
// is built a line at a time; both are clearer as written.
#![allow(clippy::format_push_string, clippy::unnecessary_literal_bound)]

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};
use tinymcp_bus::{McpAuthConfig, McpCallError, McpCallOutcome, McpResultEnvelope};
use tinytools::{PermissionLevel, Tool, ToolCallOptions, ToolResult};

use super::naming::disambiguated_tool_name;
use super::scrub::SecretScrubber;
use crate::config_servers::{McpRegistrySource, McpServerRegistry};

/// The host's permission check for an acting call.
///
/// Called with the tool's name before [`McpCallTool`] sends anything; an error
/// refuses the call and is returned as the tool's error.
pub type ActGate = Arc<dyn Fn(&str) -> anyhow::Result<()> + Send + Sync>;

/// Lists the configured servers, without their credentials.
#[derive(Debug)]
pub struct McpListServersTool {
    registry: Arc<McpServerRegistry>,
}

impl McpListServersTool {
    /// A tool over `registry`.
    #[must_use]
    pub fn new(registry: Arc<McpServerRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Tool for McpListServersTool {
    fn name(&self) -> &str {
        "mcp_list_servers"
    }

    fn description(&self) -> &str {
        "List named remote MCP servers registered in OpenHuman core. Use this before browsing tools on a specific MCP server."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::ReadOnly
    }

    fn supports_markdown(&self) -> bool {
        true
    }

    async fn execute(&self, _args: Value) -> anyhow::Result<ToolResult> {
        let servers = self
            .registry
            .list()
            .into_iter()
            .map(|server| {
                json!({
                    "name": server.name,
                    "endpoint": endpoint_without_query(&server.endpoint),
                    "description": server.description,
                    "timeout_secs": server.timeout_secs,
                    "allowed_tools": server.allowed_tools,
                    "disallowed_tools": server.disallowed_tools,
                    "auth_configured": !matches!(server.auth, McpAuthConfig::None),
                    "auth_kind": auth_kind(&server.auth),
                    "source": server.source,
                })
            })
            .collect::<Vec<_>>();

        let markdown = if servers.is_empty() {
            "# MCP Servers\n\nNo remote MCP servers are registered.".to_string()
        } else {
            let mut md = String::from("# MCP Servers\n");
            for server in self.registry.list() {
                let source = match server.source {
                    McpRegistrySource::Config => "config",
                    McpRegistrySource::Host => "host",
                };
                md.push_str(&format!(
                    "\n- **{}** ({source})\n  - endpoint: `{}`\n  - auth: `{}`",
                    server.name,
                    endpoint_without_query(&server.endpoint),
                    auth_kind(&server.auth)
                ));
                if let Some(description) = server.description.as_deref() {
                    md.push_str(&format!("\n  - {description}"));
                }
                if !server.allowed_tools.is_empty() {
                    md.push_str(&format!(
                        "\n  - allowed tools: `{}`",
                        server.allowed_tools.join("`, `")
                    ));
                }
                if !server.disallowed_tools.is_empty() {
                    md.push_str(&format!(
                        "\n  - disallowed tools: `{}`",
                        server.disallowed_tools.join("`, `")
                    ));
                }
            }
            md
        };

        Ok(ToolResult::success_with_markdown(
            json!({ "servers": servers }),
            markdown,
        ))
    }
}

/// Lists the tools one configured server advertises.
#[derive(Debug)]
pub struct McpListToolsTool {
    registry: Arc<McpServerRegistry>,
}

impl McpListToolsTool {
    /// A tool over `registry`.
    #[must_use]
    pub fn new(registry: Arc<McpServerRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Tool for McpListToolsTool {
    fn name(&self) -> &str {
        "mcp_list_tools"
    }

    fn description(&self) -> &str {
        "List tools exposed by a named remote MCP server. Use this before calling `mcp_call_tool`."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "server": {
                    "type": "string",
                    "description": "Registered MCP server name from `mcp_list_servers`."
                }
            },
            "required": ["server"],
            "additionalProperties": false
        })
    }

    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::ReadOnly
    }

    fn supports_markdown(&self) -> bool {
        true
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let server = required_string_arg(&args, "server")?;
        let scrubber = SecretScrubber::for_server(&self.registry, &server);
        let tools = match self.registry.list_tools(&server).await {
            Ok(tools) => tools,
            Err(err) => {
                return Ok(ToolResult::error(
                    scrubber.scrub(&format!("mcp_list_tools failed: {err}")),
                ));
            }
        };

        let payload = tools
            .iter()
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "title": tool.display_title(),
                    "description": tool.display_description(),
                    "input_schema": tool.input_schema,
                })
            })
            .collect::<Vec<_>>();

        let mut markdown = format!("# MCP Tools: `{server}`\n");
        if tools.is_empty() {
            markdown.push_str("\nNo tools were returned by the remote server.");
        } else {
            for tool in &tools {
                let desc = tool
                    .display_description()
                    .unwrap_or_else(|| "No description.".to_string());
                markdown.push_str(&format!(
                    "\n- **{}**: {}\n  - schema: `{}`",
                    tool.name,
                    desc,
                    serde_json::to_string(&tool.input_schema).unwrap_or_else(|_| "{}".into())
                ));
            }
        }

        Ok(scrubber.scrub_result(ToolResult::success_with_markdown(
            json!({ "server": server, "tools": payload }),
            markdown,
        )))
    }
}

/// Calls one tool on one configured server, behind the host's [`ActGate`].
pub struct McpCallTool {
    registry: Arc<McpServerRegistry>,
    act_gate: ActGate,
}

impl std::fmt::Debug for McpCallTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpCallTool")
            .field("registry", &self.registry)
            .finish_non_exhaustive()
    }
}

impl McpCallTool {
    /// A tool over `registry` that asks `act_gate` before each call.
    #[must_use]
    pub fn new(registry: Arc<McpServerRegistry>, act_gate: ActGate) -> Self {
        Self { registry, act_gate }
    }
}

#[async_trait]
impl Tool for McpCallTool {
    fn name(&self) -> &str {
        "mcp_call_tool"
    }

    fn description(&self) -> &str {
        "Call a tool on a named remote MCP server. First inspect available tools with `mcp_list_tools`, then pass the remote tool name and its JSON arguments here."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "server": {
                    "type": "string",
                    "description": "Registered MCP server name from `mcp_list_servers`."
                },
                "tool": {
                    "type": "string",
                    "description": "Remote MCP tool name from `mcp_list_tools`."
                },
                "arguments": {
                    "type": "object",
                    "description": "Arguments object passed through to the remote MCP tool."
                }
            },
            "required": ["server", "tool", "arguments"],
            "additionalProperties": false
        })
    }

    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::Execute
    }

    fn supports_markdown(&self) -> bool {
        true
    }

    /// The per-server tool this call reaches, described exactly as
    /// [`McpServerTool`](super::McpServerTool) describes it for a configured
    /// server: the same name, family and `mcp.*` tags. A host's tool rules then
    /// bind the operation whichever route the model takes, and the target is
    /// judged on the remote tool's own `arguments`.
    fn indirect_target(&self, args: &Value) -> Option<tinytools::IndirectCall> {
        // The same normalization dispatch applies (trim, fences, trailing
        // punctuation), so the rules judge the tool that will actually run.
        let server = required_string_arg(args, "server").ok()?;
        let tool = required_string_arg(args, "tool").ok()?;
        let (server, tool) = (server.as_str(), tool.as_str());
        let mut target =
            tinytools::ToolSubject::named(disambiguated_tool_name(server, server, tool))
                .with_family(server)
                .with_tag(format!("mcp.server:{server}"))
                .with_tag(format!("mcp.server_id:{server}"))
                .with_tag(format!("mcp.tool:{tool}"))
                .with_permission(PermissionLevel::Execute);
        target.category = Some(tinytools::ToolCategory::Workflow);
        let call = tinytools::IndirectCall::new(target);
        Some(
            match args
                .get("arguments")
                .cloned()
                .and_then(|arguments| tinymcp_bus::normalize_tool_arguments(arguments).ok())
            {
                Some(arguments) => call.with_arguments(Value::Object(arguments)),
                None => call,
            },
        )
    }

    async fn execute_with_options(
        &self,
        args: Value,
        options: ToolCallOptions,
    ) -> anyhow::Result<ToolResult> {
        (self.act_gate)(self.name())?;

        let server = required_string_arg(&args, "server")?;
        let tool = required_string_arg(&args, "tool")?;
        let scrubber = SecretScrubber::for_server(&self.registry, &server);
        let arguments = args
            .get("arguments")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("missing required `arguments` object"))?;
        // An object JSON-encoded into a string is decoded; anything that
        // cannot hold one is refused naming what arrived.
        let arguments = match tinymcp_bus::normalize_tool_arguments(arguments) {
            Ok(arguments) => Value::Object(arguments),
            Err(error) => {
                let outcome = McpCallOutcome::failed(
                    &server,
                    &tool,
                    McpCallError::new(tinymcp_bus::errors::INVALID_ARGUMENTS),
                );
                return Ok(with_outcome(
                    ToolResult::error(format!("`arguments`: {error}")),
                    &outcome,
                ));
            }
        };

        let (mut result, outcome) = match self.registry.call_tool(&server, &tool, arguments).await {
            Ok(result) => (result.rendered, McpCallOutcome::answered(&server, &tool)),
            Err(err) => {
                let outcome = McpCallOutcome::failed(&server, &tool, call_error(&err));
                tracing::debug!(
                    server = %scrubber.scrub(&server),
                    tool = %scrubber.scrub(&tool),
                    code = err.wire_name(),
                    "[mcp] mcp_call_tool failed"
                );
                return Ok(with_outcome(
                    ToolResult::error(scrubber.scrub(&format!("mcp_call_tool failed: {err}"))),
                    &outcome,
                ));
            }
        };

        if options.prefer_markdown && result.markdown_formatted.is_none() {
            result.markdown_formatted = Some(result.output());
        }
        let envelope = scrubbed_envelope(
            &scrubber,
            McpResultEnvelope::new(&server, &tool, &result).with_outcome(outcome),
        );
        let mut mapped = scrubber.scrub_result(super::tool_result(result));
        mapped.metadata = Some(envelope.to_metadata());
        Ok(mapped)
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        self.execute_with_options(args, ToolCallOptions::default())
            .await
    }
}

/// `result` carrying `outcome` as its host-only metadata.
fn with_outcome(mut result: ToolResult, outcome: &McpCallOutcome) -> ToolResult {
    result.metadata = serde_json::to_value(outcome).ok();
    result
}

/// `envelope` with every known secret replaced in the server-supplied parts.
pub(super) fn scrubbed_envelope(
    scrubber: &SecretScrubber,
    mut envelope: McpResultEnvelope,
) -> McpResultEnvelope {
    if let Some(structured) = envelope.structured_content.as_mut() {
        scrubber.scrub_value(structured);
    }
    if let Some(meta) = envelope.meta.as_mut() {
        scrubber.scrub_value(meta);
    }
    for resource in &mut envelope.resources {
        if let Some(text) = resource.text.as_mut() {
            *text = scrubber.scrub(text);
        }
        if let Some(meta) = resource.meta.as_mut() {
            scrubber.scrub_value(meta);
        }
    }
    envelope
}

/// The host-facing classification of a failed call.
fn call_error(error: &crate::Error) -> McpCallError {
    McpCallError {
        code: error.wire_name().to_string(),
        unauthorized: error.is_unauthorized(),
        advertises_oauth: error.advertises_oauth(),
    }
}

fn auth_kind(auth: &McpAuthConfig) -> &'static str {
    match auth {
        McpAuthConfig::None => "none",
        McpAuthConfig::BearerToken { .. } => "bearer_token",
        McpAuthConfig::Basic { .. } => "basic",
        McpAuthConfig::Header { .. } => "header",
        McpAuthConfig::Headers { .. } => "headers",
        McpAuthConfig::QueryParam { .. } => "query_param",
        _ => "unknown",
    }
}

pub(super) fn endpoint_without_query(endpoint: &str) -> String {
    if let Ok(mut url) = url::Url::parse(endpoint) {
        let _ = url.set_username("");
        let _ = url.set_password(None);
        url.set_query(None);
        url.set_fragment(None);
        return url.to_string();
    }
    endpoint
        .find(['?', '#'])
        .map_or(endpoint, |cut| &endpoint[..cut])
        .to_string()
}

/// A required identifier argument, trimmed and without the markdown a model
/// wraps it in when it answers in prose (`` `docs` ``, `*docs*`, `` `docs`. ``).
///
/// Backticks and asterisks go from either end, and sentence punctuation goes
/// only when it follows one of them. Every other character, `_` and `.`
/// included, reaches the server as typed.
pub(super) fn required_string_arg(args: &Value, key: &str) -> anyhow::Result<String> {
    let missing = || anyhow::anyhow!("missing required `{key}`");
    let value = args
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(missing)?;
    let unpunctuated = value.trim_end_matches(['.', ',', ';', ':', '!']);
    let fenced = if unpunctuated.ends_with(['`', '*']) {
        unpunctuated
    } else {
        value
    };
    let cleaned = fenced.trim_matches(['`', '*']);
    if cleaned.is_empty() {
        return Err(missing());
    }
    Ok(cleaned.to_string())
}
