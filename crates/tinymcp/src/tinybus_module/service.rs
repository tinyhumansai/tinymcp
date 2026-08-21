//! The interface implementation.

use std::collections::{BTreeMap, HashMap};

use serde_json::Value;

use super::config::ModuleConfig;
use crate::audit::AuditStore;
use crate::config_servers::McpServerRegistry;
use crate::error::Result;
use crate::registry::{AuthDetection, McpRegistry, SecretRef, Store};
use tinymcp_bus::{
    ConnStatus, ConnectOutcome, InstallOutcome, InstalledServer, McpTool, McpWriteListQuery,
    McpWriteRecord, NewMcpWriteRecord, RegistrySearchPage, RegistryServerDetail, RegistrySettings,
    ToolCallOutcome, UpdateEnvOutcome,
};

/// One server's detail plus the credentials installing it would need.
///
/// The two travel together because a caller rendering an install form needs
/// both, and fetching them separately would mean two catalog round trips for
/// one screen.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ServerDetail {
    /// What the catalog says about the server.
    pub server: RegistryServerDetail,
    /// The credential names an install will actually need.
    pub required_env_keys: Vec<String>,
}

/// Everything the interface serves.
#[derive(Debug)]
pub struct McpService {
    dynamic: McpRegistry,
    /// The servers the host declared in its own configuration.
    ///
    /// Separate from the dynamic registry because nothing about them is
    /// persisted or installed — they exist because the host said so, and they
    /// change only when its configuration does.
    static_servers: McpServerRegistry,
    audit: AuditStore,
}

impl McpService {
    /// Builds the service from a host's configuration.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::StoreIo`] or [`crate::Error::Store`] when the
    /// stores cannot be opened, and [`crate::Error::ClientBuild`] when an HTTP
    /// client cannot be built.
    pub fn new(config: &ModuleConfig) -> Result<Self> {
        // No data directory means nothing to persist, which is the right shape
        // for a host that only wants its statically declared servers.
        let (store, audit) = match config.data_dir.as_deref() {
            Some(dir) => (Store::open(dir)?, AuditStore::open(dir)?),
            None => (Store::open_in_memory()?, AuditStore::open_in_memory()?),
        };

        let dynamic = McpRegistry::new(
            store,
            config.client.registry_auth.clone(),
            config.client.client_identity.clone(),
            config.client.proxy.clone(),
        )?;

        let static_servers = McpServerRegistry::from_config(&config.client)?;

        Ok(Self {
            dynamic,
            static_servers,
            audit,
        })
    }

    /// The dynamic registry, for a host using this crate directly.
    #[must_use]
    pub fn dynamic(&self) -> &McpRegistry {
        &self.dynamic
    }

    /// The statically declared servers.
    #[must_use]
    pub fn static_servers(&self) -> &McpServerRegistry {
        &self.static_servers
    }

    /// The write-audit log.
    #[must_use]
    pub fn audit(&self) -> &AuditStore {
        &self.audit
    }

    /// Turns a crate error into a bus failure.
    ///
    /// The message is the error's own, which is already redacted: every variant
    /// carrying an endpoint holds the output of [`crate::redact_endpoint`], and
    /// the causes have had their URLs stripped.
    fn failed(error: &crate::Error) -> tinybus::Error {
        tinybus::Error::failed(error.to_string())
    }

    /// Reads a map of credential names to handles.
    fn parse_handles(raw: &HashMap<String, String>) -> Result<HashMap<String, SecretRef>> {
        raw.iter()
            .map(|(name, handle)| {
                SecretRef::parse(handle)
                    .map(|handle| (name.clone(), handle))
                    .ok_or_else(|| {
                        crate::Error::malformed(format!("`{handle}` is not a secret handle"))
                    })
            })
            .collect()
    }
}

// Every member is `async fn` because the interface macro requires it: a
// blocking one would stall the connection's dispatch task for every other
// caller. A handful of them have nothing to await, and that is fine — the
// uniformity is what the macro is buying.
#[allow(clippy::unused_async)]
#[tinybus::interface(name = "ai.tinyhumans.tinymcp.Mcp")]
impl McpService {
    // -- browsing -----------------------------------------------------------

    /// `(query, page, page_size)`
    async fn registry_search(
        &self,
        query: Option<String>,
        page: Option<u32>,
        page_size: Option<u32>,
    ) -> tinybus::Result<RegistrySearchPage> {
        self.dynamic
            .registry_search(query.as_deref(), page.unwrap_or(1), page_size.unwrap_or(20))
            .await
            .map_err(|error| Self::failed(&error))
    }

    /// `(qualified_name)`
    async fn registry_get(&self, qualified_name: String) -> tinybus::Result<ServerDetail> {
        let (server, required_env_keys) = self
            .dynamic
            .registry_get(&qualified_name)
            .await
            .map_err(|error| Self::failed(&error))?;

        Ok(ServerDetail {
            server,
            required_env_keys,
        })
    }

    /// `()`
    async fn registry_settings_get(&self) -> tinybus::Result<RegistrySettings> {
        Ok(self.dynamic.registry_settings())
    }

    /// `(smithery_api_key, mcp_official_base, mcp_official_token)`
    ///
    /// Each is optional: absent leaves the stored value, and a blank string
    /// clears it. Persisting is the host's.
    async fn registry_settings_set(
        &self,
        smithery_api_key: Option<String>,
        mcp_official_base: Option<String>,
        mcp_official_token: Option<String>,
    ) -> tinybus::Result<RegistrySettings> {
        Ok(self.dynamic.set_registry_settings(
            smithery_api_key,
            mcp_official_base,
            mcp_official_token,
        ))
    }

    // -- installs -----------------------------------------------------------

    /// `()`
    async fn installed_list(&self) -> tinybus::Result<Vec<InstalledServer>> {
        self.dynamic
            .installed_list()
            .map_err(|error| Self::failed(&error))
    }

    /// `(qualified_name, env, config)`
    async fn install(
        &self,
        qualified_name: String,
        env: BTreeMap<String, String>,
        config: Option<Value>,
    ) -> tinybus::Result<InstallOutcome> {
        self.dynamic
            .install(&qualified_name, env, config)
            .await
            .map_err(|error| Self::failed(&error))
    }

    /// `(server_id)`
    async fn uninstall(&self, server_id: String) -> tinybus::Result<bool> {
        self.dynamic
            .uninstall(&server_id)
            .await
            .map_err(|error| Self::failed(&error))
    }

    /// `(server_id, enabled)`
    async fn set_enabled(&self, server_id: String, enabled: bool) -> tinybus::Result<()> {
        self.dynamic
            .set_enabled(&server_id, enabled)
            .await
            .map_err(|error| Self::failed(&error))
    }

    /// `(server_id, env)`
    async fn update_env(
        &self,
        server_id: String,
        env: BTreeMap<String, String>,
    ) -> tinybus::Result<UpdateEnvOutcome> {
        self.dynamic
            .update_env(&server_id, env)
            .await
            .map_err(|error| Self::failed(&error))
    }

    // -- connections --------------------------------------------------------

    /// `(server_id)`
    async fn connect(&self, server_id: String) -> tinybus::Result<ConnectOutcome> {
        self.dynamic
            .connect(&server_id)
            .await
            .map_err(|error| Self::failed(&error))
    }

    /// `(server_id)`
    async fn disconnect(&self, server_id: String) -> tinybus::Result<bool> {
        self.dynamic
            .disconnect(&server_id)
            .await
            .map_err(|error| Self::failed(&error))
    }

    /// `()`
    async fn status(&self) -> tinybus::Result<Vec<ConnStatus>> {
        self.dynamic
            .status()
            .await
            .map_err(|error| Self::failed(&error))
    }

    // -- authorization ------------------------------------------------------

    /// `(server_id)`
    async fn detect_auth(&self, server_id: String) -> tinybus::Result<AuthDetection> {
        self.dynamic
            .detect_auth(&server_id)
            .await
            .map_err(|error| Self::failed(&error))
    }

    /// `(server_id, redirect_uri)`
    ///
    /// The redirect is the host's loopback address; only it knows which port it
    /// actually bound.
    #[tinybus(name = "OAuthBegin")]
    async fn oauth_begin(
        &self,
        server_id: String,
        redirect_uri: String,
    ) -> tinybus::Result<String> {
        self.dynamic
            .oauth_begin(&server_id, &redirect_uri)
            .await
            .map_err(|error| Self::failed(&error))
    }

    // -- tools --------------------------------------------------------------

    /// `(server_id)`
    async fn list_tools(&self, server_id: String) -> tinybus::Result<Vec<McpTool>> {
        self.dynamic
            .list_tools(&server_id)
            .await
            .map_err(|error| Self::failed(&error))
    }

    /// `(server_id, tool_name, arguments)`
    async fn tool_call(
        &self,
        server_id: String,
        tool_name: String,
        arguments: Value,
    ) -> tinybus::Result<ToolCallOutcome> {
        self.dynamic
            .tool_call(&server_id, &tool_name, arguments)
            .await
            .map_err(|error| Self::failed(&error))
    }

    /// `(qualified_name)`
    ///
    /// Gathers what a model needs to help configure a server. Running the turn
    /// is the host's.
    async fn config_assist(&self, qualified_name: String) -> tinybus::Result<ServerDetail> {
        let (server, required_env_keys) = self
            .dynamic
            .config_assist(&qualified_name)
            .await
            .map_err(|error| Self::failed(&error))?;

        Ok(ServerDetail {
            server,
            required_env_keys,
        })
    }

    // -- the guided setup flow ----------------------------------------------

    /// `(query, page, page_size)`
    async fn setup_search(
        &self,
        query: Option<String>,
        page: Option<u32>,
        page_size: Option<u32>,
    ) -> tinybus::Result<RegistrySearchPage> {
        self.dynamic
            .registry_search(query.as_deref(), page.unwrap_or(1), page_size.unwrap_or(20))
            .await
            .map_err(|error| Self::failed(&error))
    }

    /// `(qualified_name)`
    async fn setup_get(&self, qualified_name: String) -> tinybus::Result<ServerDetail> {
        let (server, required_env_keys) = self
            .dynamic
            .registry_get(&qualified_name)
            .await
            .map_err(|error| Self::failed(&error))?;

        Ok(ServerDetail {
            server,
            required_env_keys,
        })
    }

    /// `(key_name)` — returns the handle to prompt against.
    async fn setup_request_secret(&self, key_name: String) -> tinybus::Result<String> {
        self.dynamic
            .setup_request_secret(&key_name)
            .await
            .map_err(|error| Self::failed(&error))
    }

    /// `(handle, value)`
    async fn setup_submit_secret(&self, handle: String, value: String) -> tinybus::Result<bool> {
        self.dynamic
            .setup_submit_secret(&handle, value)
            .await
            .map_err(|error| Self::failed(&error))
    }

    /// `(qualified_name, secrets)`
    ///
    /// `secrets` maps a credential name to a handle. Nothing is installed and
    /// nothing joins the connection map.
    async fn setup_test_connection(
        &self,
        qualified_name: String,
        secrets: HashMap<String, String>,
    ) -> tinybus::Result<Vec<McpTool>> {
        let handles = Self::parse_handles(&secrets).map_err(|error| Self::failed(&error))?;

        self.dynamic
            .setup_test_connection(&qualified_name, &handles)
            .await
            .map_err(|error| Self::failed(&error))
    }

    /// `(qualified_name, secrets, config)`
    async fn setup_install_and_connect(
        &self,
        qualified_name: String,
        secrets: HashMap<String, String>,
        config: Option<Value>,
    ) -> tinybus::Result<ConnectOutcome> {
        let handles = Self::parse_handles(&secrets).map_err(|error| Self::failed(&error))?;

        self.dynamic
            .setup_install_and_connect(&qualified_name, &handles, config)
            .await
            .map_err(|error| Self::failed(&error))
    }

    // -- the statically declared servers -------------------------------------

    /// `()` — the names the host declared.
    async fn static_list(&self) -> tinybus::Result<Vec<String>> {
        Ok(self
            .static_servers
            .list()
            .into_iter()
            .map(|server| server.name.clone())
            .collect())
    }

    /// `(server)`
    async fn static_list_tools(
        &self,
        server: String,
    ) -> tinybus::Result<Vec<tinymcp_bus::McpRemoteTool>> {
        self.static_servers
            .list_tools(&server)
            .await
            .map_err(|error| Self::failed(&error))
    }

    /// `(server, tool, arguments)`
    async fn static_call_tool(
        &self,
        server: String,
        tool: String,
        arguments: Value,
    ) -> tinybus::Result<ToolCallOutcome> {
        let result = self
            .static_servers
            .call_tool(&server, &tool, arguments)
            .await
            .map_err(|error| Self::failed(&error))?;

        Ok(ToolCallOutcome {
            is_error: result.rendered.is_error,
            result: result.raw_result,
        })
    }

    // -- the write-audit log -------------------------------------------------

    /// `(record)` — returns the row identifier.
    async fn audit_record_write(&self, record: NewMcpWriteRecord) -> tinybus::Result<i64> {
        self.audit
            .record(&record)
            .map_err(|error| Self::failed(&error))
    }

    /// `(query)`
    async fn audit_list_writes(
        &self,
        query: McpWriteListQuery,
    ) -> tinybus::Result<Vec<McpWriteRecord>> {
        self.audit
            .list(&query)
            .map_err(|error| Self::failed(&error))
    }
}
