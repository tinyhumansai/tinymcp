//! The operation reply types.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{InstalledServer, McpAuthHint, McpTool, RegistryServerSummary};

/// One page of catalog results.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RegistrySearchPage {
    /// The rows on this page.
    #[serde(default)]
    pub servers: Vec<RegistryServerSummary>,
    /// Which page this is, one-based.
    pub page: u32,
    /// A best-effort upper bound on the page count.
    ///
    /// Not a total. Learning the true one would mean walking the whole catalog,
    /// which is the cost the paging model exists to avoid — so this is one page
    /// beyond the current one while more results exist, and the current page
    /// when they do not.
    pub total_pages: u32,
}

/// What installing produced.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstallOutcome {
    /// The install record, new or already present.
    pub server: InstalledServer,
    /// Whether this service was already installed.
    ///
    /// Installing is idempotent: a second install of the same service refreshes
    /// the credentials and configuration on the existing record rather than
    /// creating a second one. A caller shows "updated" rather than "installed"
    /// when this is set.
    #[serde(default)]
    pub already_installed: bool,
}

/// What connecting produced.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ConnectOutcome {
    /// The install that was connected.
    pub server_id: String,
    /// The tools it advertises.
    #[serde(default)]
    pub tools: Vec<McpTool>,
}

/// What calling a tool produced.
///
/// A tool that reports failure is a *successful call* with this flag set: the
/// request reached the server and the server answered. A caller that treats it
/// as a transport failure tells the user their network is broken when their
/// arguments were.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallOutcome {
    /// The reply, as the server sent it.
    pub result: Value,
    /// Whether the tool reported a failure.
    #[serde(default)]
    pub is_error: bool,
}

/// Where a server stands after its credentials were replaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum UpdateEnvStatus {
    /// The new credentials worked and the server is live again.
    Connected,
    /// The server is turned off, so nothing was reconnected. The credentials
    /// are stored and will be used when it is turned back on.
    Disabled,
    /// The server answered 401 with the new credentials.
    Unauthorized,
    /// Reconnecting failed for some other reason.
    Disconnected,
}

/// What replacing a server's credentials produced.
///
/// # The credentials are kept whatever happened
///
/// A failed reconnect does not roll them back. The user corrected a value; that
/// correction is theirs to keep, and throwing it away would make them type it
/// again to find out the server is still down for an unrelated reason.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpdateEnvOutcome {
    /// The install whose credentials were replaced.
    pub server_id: String,
    /// Where it stands now.
    pub status: UpdateEnvStatus,
    /// Every credential name now stored, including ones a partial update kept.
    #[serde(default)]
    pub env_keys: Vec<String>,
    /// The tools it advertises, when it reconnected.
    #[serde(default)]
    pub tools: Vec<McpTool>,
    /// Why a 401 happened, when one did.
    ///
    /// The code only. The raw 401 body and the OAuth metadata URL describe the
    /// server's authorization setup and are not a caller's to render.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_hint: Option<McpAuthHint>,
    /// The diagnostic, for a failure that was not a 401.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Which registry credentials are configured.
///
/// # No secret is here
///
/// The credential fields are booleans reporting whether a value is *set*. The
/// base URL is included because it is not a secret and a user who cannot see
/// which registry they are pointed at cannot debug it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistrySettings {
    /// Whether a Smithery key is configured, from settings or the environment.
    pub smithery_api_key_set: bool,
    /// Whether an official-registry token is configured.
    pub mcp_official_token_set: bool,
    /// The official-registry base URL override, when one is set.
    #[serde(default)]
    pub mcp_official_base: Option<String>,
}
