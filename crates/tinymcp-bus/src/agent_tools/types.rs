//! The agent-tool payload types.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{McpResourceContents, McpToolResult};

/// Why a tool call's `arguments` could not be read as an object.
///
/// Each variant names what actually arrived, because the message is read by
/// the model that sent it and "invalid arguments" gives it nothing to change.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ArgsError {
    /// The value was neither an object nor a string that could hold one.
    NotAnObject {
        /// The JSON type that arrived, with its article (`"an array"`).
        actual: &'static str,
    },
    /// The value was a string that did not decode to a JSON object.
    StringNotAnObject {
        /// The JSON type the string decoded to, or `None` when it was not
        /// JSON at all.
        decoded: Option<&'static str>,
    },
}

impl std::fmt::Display for ArgsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("tool arguments must be a JSON object, not ")?;
        match self {
            Self::NotAnObject { actual } => f.write_str(actual),
            Self::StringNotAnObject {
                decoded: Some(decoded),
            } => write!(f, "a string holding {decoded}"),
            Self::StringNotAnObject { decoded: None } => f.write_str("a string that is not JSON"),
        }
    }
}

impl std::error::Error for ArgsError {}

/// What calling a tool can do to the world.
///
/// A host maps this onto its own permission model; the contract only says
/// which of the three a tool is. Declared in increasing order of consequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AgentToolEffect {
    /// Observes only: browsing a catalog, listing, reporting status.
    Read,
    /// Acts without changing what is configured: connecting, calling a tool.
    Execute,
    /// Changes persistent configuration: uninstalling a server.
    Write,
}

/// One tool as a model sees it: its identity and what it may do.
///
/// `name`, `description` and `parameters` are prompt-cache and transcript
/// identity — a model's cached prefix and a resumed session's replay both hold
/// them verbatim — so a spec built here must not drift between releases
/// without a reason worth invalidating every cached conversation for.
///
/// Execution is not here. A spec says what a tool is; running it, gating it
/// behind an approval, and deciding whether it is shown at all are the host's.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentToolSpec {
    /// The provider-safe tool name.
    pub name: String,
    /// What the model reads to decide whether to call it.
    pub description: String,
    /// The JSON Schema of the tool's arguments.
    pub parameters: Value,
    /// What calling it can do.
    pub effect: AgentToolEffect,
    /// Whether the tool is found through search rather than listed up front.
    ///
    /// A deferred tool costs nothing in the prompt until a model looks for it.
    pub deferred: bool,
}

/// The `kind` discriminator an [`McpCallOutcome`] carries.
///
/// A host that receives a tool result's metadata as an untyped JSON object
/// tells this payload apart from other tools' by this value.
pub const MCP_CALL_RESULT_KIND: &str = "mcp_call";

/// What one `mcp_call_tool` call did, as structured data for the host.
///
/// Attached to the call's result as host-only metadata, never rendered to the
/// model. A host reads it instead of parsing the result text to meter calls
/// that reached a server and to surface the ones that did not.
///
/// `ok` says whether the server answered the call. A tool that answered with
/// its own error result still has `ok: true`: the server was reached and the
/// failure is the tool's, reported in the result itself.
///
/// Decoding rejects a payload whose `kind` is not [`MCP_CALL_RESULT_KIND`] or
/// whose `ok` and `error` disagree, so a decoded outcome is one the
/// constructors could have produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "McpCallOutcomeWire")]
pub struct McpCallOutcome {
    /// Always [`MCP_CALL_RESULT_KIND`].
    pub kind: String,
    /// The server the call was aimed at, as the caller named it.
    pub server: String,
    /// The tool the call was aimed at, as the caller named it.
    pub tool: String,
    /// Whether the server answered the call.
    pub ok: bool,
    /// Why the call did not reach an answer. `None` when `ok` is `true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<McpCallError>,
}

impl McpCallOutcome {
    /// A call the server answered.
    #[must_use]
    pub fn answered(server: impl Into<String>, tool: impl Into<String>) -> Self {
        Self {
            kind: MCP_CALL_RESULT_KIND.to_string(),
            server: server.into(),
            tool: tool.into(),
            ok: true,
            error: None,
        }
    }

    /// A call that failed before the server answered it.
    #[must_use]
    pub fn failed(server: impl Into<String>, tool: impl Into<String>, error: McpCallError) -> Self {
        Self {
            kind: MCP_CALL_RESULT_KIND.to_string(),
            server: server.into(),
            tool: tool.into(),
            ok: false,
            error: Some(error),
        }
    }

    /// Reads an outcome out of a tool result's metadata.
    ///
    /// Accepts both shapes a call's metadata takes: a bare outcome whose
    /// `kind` is [`MCP_CALL_RESULT_KIND`], and an [`McpResultEnvelope`]
    /// (`kind` [`MCP_RESULT_KIND`]) carrying one under `outcome`.
    ///
    /// `None` when `metadata` is neither, or does not decode as an outcome.
    ///
    /// # Examples
    ///
    /// ```
    /// # use tinymcp_bus::{McpCallOutcome, McpResultEnvelope, McpToolResult};
    /// let outcome = McpCallOutcome::answered("docs", "search");
    /// let envelope = McpResultEnvelope::new("docs", "search", &McpToolResult::success("ok"))
    ///     .with_outcome(outcome.clone());
    /// assert_eq!(McpCallOutcome::from_metadata(&envelope.to_metadata()), Some(outcome.clone()));
    /// let bare = serde_json::to_value(&outcome).unwrap();
    /// assert_eq!(McpCallOutcome::from_metadata(&bare), Some(outcome));
    /// ```
    #[must_use]
    pub fn from_metadata(metadata: &Value) -> Option<Self> {
        if metadata.get("kind").and_then(Value::as_str) == Some(MCP_RESULT_KIND) {
            return metadata
                .get("outcome")
                .and_then(|outcome| serde_json::from_value(outcome.clone()).ok());
        }
        serde_json::from_value(metadata.clone()).ok()
    }
}

#[derive(Deserialize)]
struct McpCallOutcomeWire {
    kind: String,
    server: String,
    tool: String,
    ok: bool,
    #[serde(default)]
    error: Option<McpCallError>,
}

impl TryFrom<McpCallOutcomeWire> for McpCallOutcome {
    type Error = String;

    fn try_from(wire: McpCallOutcomeWire) -> Result<Self, Self::Error> {
        if wire.kind != MCP_CALL_RESULT_KIND {
            return Err(format!(
                "expected kind `{MCP_CALL_RESULT_KIND}`, got `{}`",
                wire.kind
            ));
        }
        if wire.ok != wire.error.is_none() {
            return Err(if wire.ok {
                "an answered call cannot carry an error".to_string()
            } else {
                "a failed call must carry an error".to_string()
            });
        }
        Ok(Self {
            kind: wire.kind,
            server: wire.server,
            tool: wire.tool,
            ok: wire.ok,
            error: wire.error,
        })
    }
}

/// Why an MCP call failed, classified for a host.
///
/// Decoding rejects `advertises_oauth: true` without `unauthorized: true`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "McpCallErrorWire")]
pub struct McpCallError {
    /// The error's wire name: one of the constants in [`crate::errors`].
    pub code: String,
    /// Whether the server answered HTTP 401 and wants credentials.
    pub unauthorized: bool,
    /// Whether that 401 advertised OAuth, so the host offers a sign-in rather
    /// than a token field. Always `false` when `unauthorized` is `false`.
    pub advertises_oauth: bool,
}

#[derive(Deserialize)]
struct McpCallErrorWire {
    code: String,
    unauthorized: bool,
    advertises_oauth: bool,
}

impl TryFrom<McpCallErrorWire> for McpCallError {
    type Error = &'static str;

    fn try_from(wire: McpCallErrorWire) -> Result<Self, Self::Error> {
        if wire.advertises_oauth && !wire.unauthorized {
            return Err("`advertises_oauth` requires `unauthorized`");
        }
        Ok(Self {
            code: wire.code,
            unauthorized: wire.unauthorized,
            advertises_oauth: wire.advertises_oauth,
        })
    }
}

impl McpCallError {
    /// An error classified under `code` with no authorization signal.
    #[must_use]
    pub fn new(code: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            unauthorized: false,
            advertises_oauth: false,
        }
    }
}

/// The `kind` discriminator an [`McpResultEnvelope`] carries.
pub const MCP_RESULT_KIND: &str = "mcp_result";

/// The host-only metadata attached to an answered MCP tool call.
///
/// Carries what the model-facing rendering drops — the reply's
/// `structuredContent`, its `_meta`, and any embedded resources — so a host
/// can render tool UI without re-calling the server. `outcome` is present
/// when the call path classifies outcomes (`mcp_call_tool`), and is what
/// [`McpCallOutcome::from_metadata`] reads.
///
/// Decoding rejects a payload whose `kind` is not [`MCP_RESULT_KIND`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "McpResultEnvelopeWire")]
pub struct McpResultEnvelope {
    /// Always [`MCP_RESULT_KIND`].
    pub kind: String,
    /// The server the call went to, as the caller addressed it.
    pub server: String,
    /// The tool's name on that server.
    pub tool: String,
    /// The reply's `structuredContent`, verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<Value>,
    /// The reply's `_meta`, verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
    /// Resources the reply embedded.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resources: Vec<McpResourceContents>,
    /// The call's classified outcome, when the call path reports one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<McpCallOutcome>,
}

impl McpResultEnvelope {
    /// The envelope for `result`, returned by `tool` on `server`.
    #[must_use]
    pub fn new(server: impl Into<String>, tool: impl Into<String>, result: &McpToolResult) -> Self {
        Self {
            kind: MCP_RESULT_KIND.to_string(),
            server: server.into(),
            tool: tool.into(),
            structured_content: result.structured_content.clone(),
            meta: result.meta.clone(),
            resources: result.resources.clone(),
            outcome: None,
        }
    }

    /// The same envelope carrying `outcome`.
    #[must_use]
    pub fn with_outcome(mut self, outcome: McpCallOutcome) -> Self {
        self.outcome = Some(outcome);
        self
    }

    /// The envelope as a tool result's metadata value.
    #[must_use]
    pub fn to_metadata(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }

    /// Reads an envelope out of a tool result's metadata.
    ///
    /// `None` when `metadata` is not an object whose `kind` is
    /// [`MCP_RESULT_KIND`], or does not decode as an envelope.
    #[must_use]
    pub fn from_metadata(metadata: &Value) -> Option<Self> {
        serde_json::from_value(metadata.clone()).ok()
    }
}

#[derive(Deserialize)]
struct McpResultEnvelopeWire {
    kind: String,
    server: String,
    tool: String,
    #[serde(default)]
    structured_content: Option<Value>,
    #[serde(default)]
    meta: Option<Value>,
    #[serde(default)]
    resources: Vec<McpResourceContents>,
    #[serde(default)]
    outcome: Option<McpCallOutcome>,
}

impl TryFrom<McpResultEnvelopeWire> for McpResultEnvelope {
    type Error = String;

    fn try_from(wire: McpResultEnvelopeWire) -> Result<Self, Self::Error> {
        if wire.kind != MCP_RESULT_KIND {
            return Err(format!(
                "expected kind `{MCP_RESULT_KIND}`, got `{}`",
                wire.kind
            ));
        }
        Ok(Self {
            kind: wire.kind,
            server: wire.server,
            tool: wire.tool,
            structured_content: wire.structured_content,
            meta: wire.meta,
            resources: wire.resources,
            outcome: wire.outcome,
        })
    }
}
