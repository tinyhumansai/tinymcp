//! The interface implementation.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use serde_json::Value;

use super::config::ModuleConfig;
use super::directories::DirectoryOpener;
use super::maintenance::Maintenance;
use crate::audit::AuditStore;
use crate::config_servers::McpServerRegistry;
use crate::error::Result;
use crate::registry::{McpRegistry, SecretRef, Store, SupervisorConfig};
use tinybus::Connection;
use tinymcp_bus::{
    AuthDetection, ConnStatus, ConnectOutcome, ConnectedServerOverview, InstallOutcome,
    InstalledServer, McpTool, McpWriteListQuery, McpWriteRecord, NewMcpWriteRecord,
    RegistrySearchPage, RegistrySettings, SearchCuration, ServerDetail, ToolCallOutcome,
    UpdateEnvOutcome,
};

/// Everything the interface serves.
#[derive(Debug)]
pub struct McpService {
    /// Shared with the background work, which outlives no call but needs the
    /// same store and connections a call sees.
    dynamic: Arc<McpRegistry>,
    /// The servers the host declared in its own configuration.
    ///
    /// Separate from the dynamic registry because nothing about them is
    /// persisted or installed — they exist because the host said so, and they
    /// change only when its configuration does.
    static_servers: McpServerRegistry,
    audit: AuditStore,
    /// The boot pass and supervisor, when they have been started.
    ///
    /// Held so they live exactly as long as the service; see
    /// [`Self::with_maintenance`].
    maintenance: Option<Maintenance>,
    /// Present on the root object only: a directory opened through `Open`
    /// cannot open further ones.
    opener: Option<Arc<DirectoryOpener>>,
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

        let dynamic = Arc::new(McpRegistry::new(
            store,
            config.client.registry_auth.clone(),
            config.client.client_identity.clone(),
            config.client.proxy.clone(),
        )?);

        let static_servers = McpServerRegistry::from_config(&config.client)?;

        Ok(Self {
            dynamic,
            static_servers,
            audit,
            maintenance: None,
            opener: None,
        })
    }

    /// Starts connecting the installed servers and keeping them connected.
    ///
    /// Returns without waiting for either: connecting is a handshake per
    /// server, and a load that waited on it would turn one broken third-party
    /// endpoint into a failed load. See the module notes on `maintenance`.
    ///
    /// Must be awaited from within a Tokio runtime, which a module's `setup`
    /// always is. A service that never calls this — a host using the crate as
    /// a plain library, or a test inspecting the interface — does no
    /// background work at all.
    pub async fn with_maintenance(mut self, config: SupervisorConfig) -> Self {
        // A poll of `ready` keeps the signature `async`, which is what ties the
        // spawn inside to a runtime rather than to whichever thread built the
        // service.
        std::future::ready(()).await;
        self.maintenance = Some(Maintenance::start(Arc::clone(&self.dynamic), config));
        self
    }

    /// Makes this the root object: able to serve further data directories
    /// through `Open`.
    pub(super) fn with_opener(
        mut self,
        connection: Connection,
        config: &ModuleConfig,
        supervisor: SupervisorConfig,
    ) -> Result<Self> {
        self.opener = Some(Arc::new(DirectoryOpener::new(
            connection, config, supervisor,
        )?));
        Ok(self)
    }

    /// Waits for the boot connect pass to finish, when maintenance was started.
    ///
    /// `None` for a service that never started it.
    pub async fn booted(&self) -> Option<crate::registry::BootOutcome> {
        match &self.maintenance {
            Some(maintenance) => Some(maintenance.booted().await),
            None => None,
        }
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
    ///
    /// The bus name is [`crate::Error::wire_name`], so a host classifies on a
    /// constant from [`tinymcp_bus::errors`] rather than on the message. The
    /// message is unchanged and still carries its own wording — including the
    /// status of a 401 — for whoever reads a re-reported string.
    fn failed(error: &crate::Error) -> tinybus::Error {
        tinybus::Error::MethodFailed {
            name: error.wire_name().to_string(),
            message: error.to_string(),
        }
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

// Every member is `async fn` because the interface macro requires it: it
// rejects a blocking method outright, on the grounds that one would stall the
// connection's dispatch task for every other caller. Synchronous members await
// an immediately ready future so both supported Clippy versions accept the
// required async signature without changing scheduling.
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

    /// `(query, page, page_size, curation)`
    ///
    /// `curation` is a [`SearchCuration`]; `{}` applies none and answers as
    /// `RegistrySearch` does.
    async fn registry_search_curated(
        &self,
        query: Option<String>,
        page: Option<u32>,
        page_size: Option<u32>,
        curation: SearchCuration,
    ) -> tinybus::Result<RegistrySearchPage> {
        self.dynamic
            .registry_search_curated(
                query.as_deref(),
                page.unwrap_or(1),
                page_size.unwrap_or(20),
                curation,
            )
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
        std::future::ready(()).await;
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
        std::future::ready(()).await;
        Ok(self.dynamic.set_registry_settings(
            smithery_api_key,
            mcp_official_base,
            mcp_official_token,
        ))
    }

    // -- installs -----------------------------------------------------------

    /// `()`
    async fn installed_list(&self) -> tinybus::Result<Vec<InstalledServer>> {
        std::future::ready(()).await;
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

    /// `()` — every connected server's identity and tools.
    ///
    /// Live connections only; a server that is not connected is absent.
    async fn connected_overview(&self) -> tinybus::Result<Vec<ConnectedServerOverview>> {
        Ok(self.dynamic.connected_overview().await)
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

    /// `(state, code)`
    ///
    /// Finishes the authorization `OAuthBegin` started. `state` is what the
    /// redirect carried back; the module resolves it to the server it began
    /// for, so the host never learns or supplies a server identifier here. A
    /// stored token followed by a failed connect is a success with no tools.
    #[tinybus(name = "OAuthComplete")]
    async fn oauth_complete(&self, state: String, code: String) -> tinybus::Result<ConnectOutcome> {
        self.dynamic
            .oauth_complete(&state, &code)
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
        std::future::ready(()).await;
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

        Ok(ToolCallOutcome::from(result))
    }

    // -- the write-audit log -------------------------------------------------

    /// `(record)` — returns the row identifier.
    async fn audit_record_write(&self, record: NewMcpWriteRecord) -> tinybus::Result<i64> {
        std::future::ready(()).await;
        self.audit
            .record(&record)
            .map_err(|error| Self::failed(&error))
    }

    /// `(query)`
    async fn audit_list_writes(
        &self,
        query: McpWriteListQuery,
    ) -> tinybus::Result<Vec<McpWriteRecord>> {
        std::future::ready(()).await;
        self.audit
            .list(&query)
            .map_err(|error| Self::failed(&error))
    }

    /// `(limit)` — drains observations for this registry object, once.
    async fn drain_supervisor_events(
        &self,
        limit: usize,
    ) -> tinybus::Result<tinymcp_bus::SupervisorBatch> {
        std::future::ready(()).await;
        if !(1..=256).contains(&limit) {
            return Err(tinybus::Error::failed("invalid supervisor drain limit"));
        }
        match &self.maintenance {
            Some(maintenance) => maintenance.drain(limit),
            None => Ok(tinymcp_bus::SupervisorBatch::default()),
        }
    }

    // -- directories ----------------------------------------------------------

    /// `(data_dir)` — returns the object path serving that directory.
    ///
    /// Answers only on the root object. Idempotent per directory; the load-time
    /// directory answers with the root path itself.
    async fn open(&self, data_dir: String) -> tinybus::Result<String> {
        let Some(opener) = &self.opener else {
            return Err(Self::failed(&crate::Error::invalid_argument(
                "only the root object can open data directories",
            )));
        };

        opener
            .open(&data_dir)
            .await
            .map_err(|error| Self::failed(&error))
    }
}

/// Resolves a configured data directory to the absolute spelling used by Open.
pub(super) fn absolute_data_dir(path: &std::path::Path) -> Result<std::path::PathBuf> {
    std::path::absolute(path).map_err(|source| crate::error::Error::StoreIo {
        path: path.to_path_buf(),
        source: Box::new(source),
    })
}
