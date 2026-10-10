//! One server tool as a `tinytools::Tool`.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tinymcp_bus::sanitize::sanitize_for_llm;
use tinymcp_bus::{McpTool, normalize_tool_arguments};
use tinytools::{PermissionLevel, Tool, ToolCategory, ToolExposure, ToolResult};

use super::invoker::McpToolInvoker;
use super::naming::legacy_tool_name;
use super::result::tool_result_for;
use super::schema::tool_parameters;
use super::source::McpToolSource;

/// The longest the server label in a description may be.
const MAX_LABEL_BYTES: usize = 120;
/// The longest a tool description may be.
const MAX_DESCRIPTION_BYTES: usize = 500;

/// One tool on one MCP server, callable by a model under its own name.
///
/// Built by [`crate::tools::tools_for`]. Declares what a remote call is: it
/// needs execute permission and has an effect outside the machine, so a host
/// routes it through its approval gate. Its family is the server, which is
/// what a tool-search index groups and ranks it by.
#[derive(Debug, Clone)]
pub struct McpServerTool {
    name: String,
    server_id: String,
    remote_name: String,
    family: String,
    /// The server label as configured, before model-facing sanitization:
    /// what a host's tool rules name, so policy tags must not drift from it.
    raw_family: String,
    description: String,
    parameters: Value,
    exposure: ToolExposure,
    invoker: Arc<dyn McpToolInvoker>,
}

impl McpServerTool {
    /// A tool for `tool` on `source`'s server, named `name`.
    #[must_use]
    pub fn new(
        name: String,
        source: &McpToolSource,
        tool: &McpTool,
        invoker: Arc<dyn McpToolInvoker>,
    ) -> Self {
        let label = if source.display_name.trim().is_empty() {
            &source.family
        } else {
            &source.display_name
        };
        let description = format!(
            "MCP server {}: {}",
            sanitize_for_llm(label, MAX_LABEL_BYTES),
            sanitize_for_llm(
                tool.description.as_deref().unwrap_or(&tool.name),
                MAX_DESCRIPTION_BYTES
            ),
        );
        Self {
            exposure: source.exposure.for_tool(&tool.name),
            name,
            server_id: source.server_id.clone(),
            remote_name: tool.name.clone(),
            family: sanitize_for_llm(&source.family, MAX_LABEL_BYTES),
            raw_family: source.family.clone(),
            description,
            parameters: tool_parameters(&tool.input_schema),
            invoker,
        }
    }

    /// The same tool under another name.
    ///
    /// For a host restoring a conversation that recorded the tool under an
    /// earlier name; see [`Self::legacy_name`].
    #[must_use]
    pub fn renamed(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// The same tool with another exposure.
    #[must_use]
    pub fn with_exposure(mut self, exposure: ToolExposure) -> Self {
        self.exposure = exposure;
        self
    }

    /// The server this tool is called on, as its invoker addresses it.
    #[must_use]
    pub fn server_id(&self) -> &str {
        &self.server_id
    }

    /// The tool's name on the server.
    #[must_use]
    pub fn remote_name(&self) -> &str {
        &self.remote_name
    }

    /// The name earlier builds gave this tool, before readable names.
    #[must_use]
    pub fn legacy_name(&self) -> String {
        legacy_tool_name(&self.server_id, &self.remote_name)
    }
}

#[async_trait]
impl Tool for McpServerTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> Value {
        self.parameters.clone()
    }

    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::Execute
    }

    fn external_effect(&self) -> bool {
        true
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Workflow
    }

    fn exposure(&self) -> ToolExposure {
        self.exposure
    }

    fn family(&self) -> Option<&str> {
        Some(&self.family)
    }

    /// `mcp.server:<label>`, `mcp.server_id:<id>` and `mcp.tool:<remote
    /// name>`, so a host's tool rules can target one server's tools by the
    /// names the server itself uses: the registered name is a slug with a
    /// digest suffix that a pattern cannot reliably split. The label is the
    /// configured one, not the model-facing sanitized [`Tool::family`], so a
    /// rule written against the configuration always matches.
    fn tags(&self) -> Vec<String> {
        vec![
            format!("mcp.server:{}", self.raw_family),
            format!("mcp.server_id:{}", self.server_id),
            format!("mcp.tool:{}", self.remote_name),
        ]
    }

    async fn execute(&self, arguments: Value) -> anyhow::Result<ToolResult> {
        // MCP requires an object. Some providers JSON-encode it, or send
        // nothing; read it the way every other call path does, and answer a
        // value that is not an object without calling the server.
        let arguments = match normalize_tool_arguments(Some(arguments)) {
            Ok(object) => Value::Object(object),
            Err(error) => {
                tracing::debug!(tool = %self.name, "refused MCP tool arguments: {error}");
                return Ok(ToolResult::error(format!(
                    "invalid arguments for MCP tool `{}`: {error}",
                    self.remote_name
                )));
            }
        };
        tracing::debug!(
            tool = %self.name,
            server_id = %self.server_id,
            remote = %self.remote_name,
            "calling an MCP tool"
        );
        match self
            .invoker
            .invoke(&self.server_id, &self.remote_name, arguments)
            .await
        {
            Ok(result) => Ok(tool_result_for(&self.server_id, &self.remote_name, result)),
            // A server that is gone or refuses is an answer the model can act
            // on, not a crash of the turn.
            Err(error) => {
                tracing::debug!(tool = %self.name, "the MCP call failed: {error}");
                Ok(ToolResult::error(format!(
                    "MCP tool `{}` failed: {error}",
                    self.remote_name
                )))
            }
        }
    }
}
