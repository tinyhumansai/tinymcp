//! The interface implementation.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use serde_json::Value;

use super::config::ModuleConfig;
use super::directories::DirectoryOpener;
use super::maintenance::{MAX_DRAIN, Maintenance};
use crate::agent_tools::SecretScrubber;
use crate::audit::AuditStore;
use crate::config_servers::McpServerRegistry;
use crate::error::Result;
use crate::processing::bound_mcp_tool_result;
use crate::registry::{McpRegistry, SecretRef, Store, SupervisorConfig};
use crate::transport::stdio::jailed::StdioLaunchers;
use tinybus::Connection;
use tinymcp_bus::{
    AuthDetection, ClassifiedStaticToolCall, ConfigApplyReport, ConfigApplyResponse,
    ConfiguredToolCallRequest, ConnStatus, ConnectOutcome, ConnectedServerOverview, InstallOutcome,
    InstalledServer, McpResourceContents, McpTool, McpWriteListQuery, McpWriteRecord,
    NewMcpWriteRecord, OpenConfiguredRequest, OpenConfiguredResponse, RegistrySearchPage,
    RegistrySettings, ScrubStaticToolsRequest, SearchCuration, ServerArgumentRule,
    ServerArgumentsValidation, ServerDetail, ToolCallOutcome, TransitionStatus, UpdateEnvOutcome,
    ValidateServerArgumentsRequest,
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
    /// How stdio servers start, for every registry this service builds
    /// (including the one-shot registry of a configured call).
    stdio: StdioLaunchers,
    audit: AuditStore,
    /// The storage provider this service was built over, so a state probe
    /// asks the same backend the service reads and writes.
    provider: Arc<dyn crate::storage::StorageProvider>,
    server_sessions: crate::server::bridge::ServerSessions,
    /// The boot pass and supervisor, when they have been started.
    ///
    /// Held so they live exactly as long as the service; see
    /// [`Self::with_maintenance`].
    maintenance: Option<Maintenance>,
    /// Present on the root object only: a directory opened through `Open`
    /// cannot open further ones.
    opener: Option<Arc<DirectoryOpener>>,
    /// Admission gate shared by the exported wrapper and lifecycle owner.
    dispatch_gate: Arc<tokio::sync::RwLock<bool>>,
}

impl McpService {
    /// Builds the service from a host's configuration.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Store`] when the stores cannot be opened, and
    /// [`crate::Error::ClientBuild`] when an HTTP client cannot be built.
    pub fn new(config: &ModuleConfig) -> Result<Self> {
        Self::with_provider(config, crate::storage::installed_provider()?)
    }

    /// Builds the service over the storage `provider` hands out for the
    /// configured data directory.
    ///
    /// # Errors
    ///
    /// As [`Self::new`].
    pub fn with_provider(
        config: &ModuleConfig,
        provider: Arc<dyn crate::storage::StorageProvider>,
    ) -> Result<Self> {
        Self::with_provider_and_stdio(config, provider, StdioLaunchers::new())
    }

    /// [`Self::new`], starting stdio servers through `stdio`.
    ///
    /// # Errors
    ///
    /// As [`Self::new`].
    pub fn new_with_stdio(config: &ModuleConfig, stdio: StdioLaunchers) -> Result<Self> {
        Self::with_provider_and_stdio(config, crate::storage::installed_provider()?, stdio)
    }

    /// [`Self::with_provider`], starting stdio servers through `stdio`.
    ///
    /// Without a sandbox in `stdio`, only servers the config marks
    /// operator-trusted can start; installed servers never can.
    ///
    /// # Errors
    ///
    /// As [`Self::new`].
    pub fn with_provider_and_stdio(
        config: &ModuleConfig,
        provider: Arc<dyn crate::storage::StorageProvider>,
        stdio: StdioLaunchers,
    ) -> Result<Self> {
        // Even a static-only service receives explicit ports from its host;
        // the module never chooses an in-memory backend on the host's behalf.
        let ports = match config.data_dir.as_deref() {
            Some(dir) => provider.ports_for(&absolute_data_dir(dir)?)?,
            None => provider.default_ports()?,
        };
        let (store, audit) = (Store::open(ports.clone())?, AuditStore::open(ports)?);

        let dynamic = Arc::new(
            McpRegistry::new(
                store,
                config.client.registry_auth.clone(),
                config.client.client_identity.clone(),
                config.client.proxy.clone(),
            )?
            .with_stdio(stdio.clone()),
        );

        let static_servers = McpServerRegistry::from_config_with(&config.client, stdio.clone())?;

        Ok(Self {
            dynamic,
            static_servers,
            stdio,
            audit,
            provider,
            server_sessions: crate::server::bridge::ServerSessions::default(),
            maintenance: None,
            opener: None,
            dispatch_gate: Arc::new(tokio::sync::RwLock::new(true)),
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

    /// Keeps the root opener when its own data directory is reconfigured.
    pub(super) fn with_directory_opener(mut self, opener: Arc<DirectoryOpener>) -> Self {
        self.opener = Some(opener);
        self
    }

    pub(super) fn register_root(self: &Arc<Self>) {
        if let Some(opener) = &self.opener {
            opener.register_root(self);
        }
    }

    /// Stops all module-owned work before a replaced object is released.
    pub(super) async fn shutdown(&self) {
        self.quiesce().await;
        if let Some(maintenance) = &self.maintenance {
            maintenance.shutdown().await;
        }
        self.server_sessions.shutdown().await;
        self.dynamic.connections().disconnect_all().await;
    }

    /// Stops new dispatches after existing handlers have returned.
    pub(super) async fn quiesce(&self) {
        let mut accepting = self.dispatch_gate.write().await;
        *accepting = false;
    }

    pub(super) async fn resume_dispatch(&self) {
        let mut accepting = self.dispatch_gate.write().await;
        *accepting = true;
    }

    /// Wraps this service with its dispatch admission gate for `TinyBus` export.
    pub(super) fn dispatch_interface(self: &Arc<Self>) -> Arc<dyn tinybus::service::Interface> {
        Arc::new(DispatchInterface {
            service: Arc::clone(self),
        })
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

    /// The same service over `registry`, for tests that stand in for an
    /// upstream catalog behind the bus members.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_dynamic_registry(mut self, registry: McpRegistry) -> Self {
        self.dynamic = Arc::new(registry);
        self
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

    /// As [`Self::failed`], also removing any secret the remote server could echo.
    fn failed_scrubbed(error: &crate::Error, scrubber: &SecretScrubber) -> tinybus::Error {
        tinybus::Error::MethodFailed {
            name: error.wire_name().to_string(),
            message: scrubber.scrub(&error.to_string()),
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

    /// `()` — renders the document without returning stored credential values.
    async fn config_document_get(&self) -> tinybus::Result<Value> {
        std::future::ready(()).await;
        self.dynamic
            .render_config_doc()
            .map_err(|error| Self::failed(&error))
    }

    /// `(document)` — replaces installs and returns the resulting document and changes.
    #[tinybus(confidential)]
    async fn config_document_apply(&self, document: Value) -> tinybus::Result<ConfigApplyResponse> {
        let report = self
            .dynamic
            .apply_config_doc(&document)
            .await
            .map_err(|error| Self::failed(&error))?;
        let document = self
            .dynamic
            .render_config_doc()
            .map_err(|error| Self::failed(&error))?;

        Ok(ConfigApplyResponse {
            document,
            report: ConfigApplyReport {
                installed: report
                    .installed
                    .into_iter()
                    .map(|server| tinymcp_bus::AppliedServer {
                        server_id: server.server_id,
                        name: server.name,
                    })
                    .collect(),
                updated: report
                    .updated
                    .into_iter()
                    .map(|server| tinymcp_bus::AppliedServer {
                        server_id: server.server_id,
                        name: server.name,
                    })
                    .collect(),
                removed: report
                    .removed
                    .into_iter()
                    .map(|server| tinymcp_bus::AppliedServer {
                        server_id: server.server_id,
                        name: server.name,
                    })
                    .collect(),
                connect_queued: report.connect_queued,
            },
        })
    }

    /// `(data_dir)` — checks whether the directory's storage already holds
    /// installs or audit records, without creating anything.
    async fn has_stored_state(&self, data_dir: String) -> tinybus::Result<bool> {
        std::future::ready(()).await;
        // Same normalisation and same provider as `with_provider`, so the
        // probe asks about the directory the service itself would open.
        absolute_data_dir(std::path::Path::new(&data_dir))
            .and_then(|dir| self.provider.ports_for(&dir))
            .and_then(|ports| ports.has_state())
            .map_err(|error| {
                tinybus::Error::failed(format!("could not inspect MCP state: {error}"))
            })
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

    /// `()`
    async fn cached_overview(&self) -> tinybus::Result<Vec<ConnectedServerOverview>> {
        self.dynamic
            .cached_overview()
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
        // Resolve credentials before calling the remote tool. If the store
        // cannot provide them, do not send a result across the bus unscreened.
        let secrets = self
            .dynamic
            .store()
            .load_env_values(&server_id)
            .map_err(|error| Self::failed(&error))?;
        let scrubber = SecretScrubber::new(&tinymcp_bus::McpAuthConfig::None, "")
            .with_secrets(secrets.into_values());
        let mut outcome = self
            .dynamic
            .tool_call(&server_id, &tool_name, arguments)
            .await
            .map_err(|error| Self::failed_scrubbed(&error, &scrubber))?;
        scrubber.scrub_tool_call_outcome(&mut outcome);
        Ok(outcome)
    }

    /// `(server_id, uri)` — one resource's contents, verbatim.
    ///
    /// Contents above `MAX_RESOURCE_BYTES` are refused with `ResourceTooLarge`.
    async fn read_resource(
        &self,
        server_id: String,
        uri: String,
    ) -> tinybus::Result<Vec<McpResourceContents>> {
        let secrets = self
            .dynamic
            .store()
            .load_env_values(&server_id)
            .map_err(|error| Self::failed(&error))?;
        let scrubber = SecretScrubber::new(&tinymcp_bus::McpAuthConfig::None, "")
            .with_secrets(secrets.into_values());
        let mut contents = self
            .dynamic
            .read_resource(&server_id, &uri)
            .await
            .map_err(|error| Self::failed_scrubbed(&error, &scrubber))?;
        scrubber.scrub_resource_contents(&mut contents);
        crate::transport::enforce_resource_cap(&uri, &contents)
            .map_err(|error| Self::failed_scrubbed(&error, &scrubber))?;
        Ok(contents)
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

    /// `()` — safe summaries of the statically declared servers.
    async fn static_server_summaries(
        &self,
    ) -> tinybus::Result<Vec<tinymcp_bus::StaticServerSummary>> {
        std::future::ready(()).await;
        Ok(self.static_servers.summaries())
    }

    /// `(request)` — makes one module-owned call outside static discovery.
    ///
    /// The classified rendering caps text, JSON, and Markdown fields at 64 KiB;
    /// larger fields are replaced by byte-count elision markers. The result
    /// envelope is built from the scrubbed reply before that rendering is bounded.
    #[tinybus(confidential)]
    async fn configured_tool_call(
        &self,
        request: ConfiguredToolCallRequest,
    ) -> tinybus::Result<ClassifiedStaticToolCall> {
        let server = request.server.name.clone();
        let tool = request.tool;
        let config = tinymcp_bus::McpClientConfig {
            servers: vec![request.server],
            ..tinymcp_bus::McpClientConfig::default()
        };
        let registry = match McpServerRegistry::from_config_with(&config, self.stdio.clone()) {
            Ok(registry) => registry,
            Err(error) => {
                let outcome = tinymcp_bus::McpCallOutcome::failed(
                    &server,
                    &tool,
                    tinymcp_bus::McpCallError {
                        code: error.wire_name().to_owned(),
                        unauthorized: error.is_unauthorized(),
                        advertises_oauth: error.advertises_oauth(),
                    },
                );
                return Ok(ClassifiedStaticToolCall {
                    outcome,
                    rendered: None,
                    envelope: None,
                    error_message: Some("could not configure the remote MCP server".to_owned()),
                });
            }
        };
        let scrubber = SecretScrubber::for_server(&registry, &server);
        match registry.call_tool(&server, &tool, request.arguments).await {
            Ok(result) => {
                let mut call = ToolCallOutcome::from(result);
                scrubber.scrub_tool_call_outcome(&mut call);
                let outcome = tinymcp_bus::McpCallOutcome::answered(&server, &tool);
                let envelope = tinymcp_bus::McpResultEnvelope::new(&server, &tool, &call.rendered)
                    .with_outcome(outcome.clone());
                let rendered = bound_mcp_tool_result(call.rendered);
                Ok(ClassifiedStaticToolCall {
                    outcome,
                    rendered: Some(rendered),
                    envelope: Some(envelope),
                    error_message: None,
                })
            }
            Err(error) => {
                let outcome = tinymcp_bus::McpCallOutcome::failed(
                    &server,
                    &tool,
                    tinymcp_bus::McpCallError {
                        code: error.wire_name().to_owned(),
                        unauthorized: error.is_unauthorized(),
                        advertises_oauth: error.advertises_oauth(),
                    },
                );
                Ok(ClassifiedStaticToolCall {
                    outcome,
                    rendered: None,
                    envelope: None,
                    error_message: Some(scrubber.scrub(&format!("mcp_call_tool failed: {error}"))),
                })
            }
        }
    }

    /// `(request)` — applies the implementation's generic MCP argument rules.
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "TinyBus service methods share an async interface"
    )]
    async fn validate_server_arguments(
        &self,
        request: ValidateServerArgumentsRequest,
    ) -> tinybus::Result<ServerArgumentsValidation> {
        let result = (|| {
            let mut arguments = crate::server::args::object_arguments(request.arguments)?;
            let allowed = request
                .allowed
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>();
            crate::server::args::reject_unexpected_arguments(&arguments, &allowed)?;
            for rule in request.rules {
                match rule {
                    ServerArgumentRule::RequiredNonEmptyString { key } => {
                        let value =
                            crate::server::args::required_non_empty_string(&arguments, &key)?;
                        arguments.insert(key, Value::String(value));
                    }
                    ServerArgumentRule::OptionalNonEmptyString { key } => {
                        if let Some(value) =
                            crate::server::args::optional_non_empty_string(&arguments, &key)?
                        {
                            arguments.insert(key, Value::String(value));
                        }
                    }
                    ServerArgumentRule::RequiredNonEmptyStringArray { key } => {
                        let values =
                            crate::server::args::required_non_empty_string_array(&arguments, &key)?;
                        arguments.insert(key, Value::from(values));
                    }
                    ServerArgumentRule::OptionalPositiveU64 { key, max } => {
                        crate::server::args::optional_positive_u64(&arguments, &key, max)?;
                    }
                }
            }
            Ok(arguments)
        })();
        Ok(match result {
            Ok(arguments) => ServerArgumentsValidation::Valid(arguments),
            Err(error) => ServerArgumentsValidation::Invalid(error),
        })
    }

    /// `(server)`
    async fn static_list_tools(
        &self,
        server: String,
    ) -> tinybus::Result<Vec<tinymcp_bus::McpRemoteTool>> {
        self.static_servers
            .list_tools_caching(&server, self.dynamic.store())
            .await
            .map_err(|error| Self::failed(&error))
    }

    /// `(request)` — scrub credentials from metadata returned by a static server.
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "the TinyBus interface requires async service methods even for local processing"
    )]
    async fn static_scrub_tools(
        &self,
        request: ScrubStaticToolsRequest,
    ) -> tinybus::Result<Vec<tinymcp_bus::McpRemoteTool>> {
        let scrubber = SecretScrubber::for_server(&self.static_servers, &request.server);
        crate::processing::scrub_static_tools(&request, &scrubber)
            .map_err(|error| Self::failed(&error))
    }

    /// `(server)` — cached tools for the host's static server declaration.
    async fn static_cached_list_tools(
        &self,
        server: String,
    ) -> tinybus::Result<Option<Vec<McpTool>>> {
        std::future::ready(()).await;
        Ok(self
            .static_servers
            .cached_tools(&server, self.dynamic.store()))
    }

    /// `(server, tool, arguments)`
    async fn static_call_tool(
        &self,
        server: String,
        tool: String,
        arguments: Value,
    ) -> tinybus::Result<ToolCallOutcome> {
        let scrubber = SecretScrubber::for_server(&self.static_servers, &server);
        let result = self
            .static_servers
            .call_tool(&server, &tool, arguments)
            .await
            .map_err(|error| Self::failed_scrubbed(&error, &scrubber))?;

        let mut outcome = ToolCallOutcome::from(result);
        scrubber.scrub_tool_call_outcome(&mut outcome);
        Ok(outcome)
    }

    /// `(server, uri)` — one resource's contents, verbatim.
    ///
    /// Contents above `MAX_RESOURCE_BYTES` are refused with `ResourceTooLarge`.
    async fn static_read_resource(
        &self,
        server: String,
        uri: String,
    ) -> tinybus::Result<Vec<McpResourceContents>> {
        let scrubber = SecretScrubber::for_server(&self.static_servers, &server);
        let mut contents = self
            .static_servers
            .read_resource(&server, &uri)
            .await
            .map_err(|error| Self::failed_scrubbed(&error, &scrubber))?;
        scrubber.scrub_resource_contents(&mut contents);
        crate::transport::enforce_resource_cap(&uri, &contents)
            .map_err(|error| Self::failed_scrubbed(&error, &scrubber))?;
        Ok(contents)
    }

    /// `()` — returns the owner's fixed generic MCP bridge tool declarations.
    async fn prepare_bridge_tools(&self) -> tinybus::Result<Vec<tinymcp_bus::AgentToolSpec>> {
        std::future::ready(()).await;
        Ok(tinymcp_bus::bridge_tool_specs())
    }

    /// `(server, tool, arguments)` — returns typed auth classification and a
    /// scrubbed, bounded rendering. Text, JSON, and Markdown fields above 64 KiB
    /// are replaced by byte-count elision markers; the host-only envelope keeps
    /// structured content, metadata, and embedded resources.
    async fn static_call_tool_classified(
        &self,
        server: String,
        tool: String,
        arguments: Value,
    ) -> tinybus::Result<ClassifiedStaticToolCall> {
        let scrubber = SecretScrubber::for_server(&self.static_servers, &server);
        match self
            .static_servers
            .call_tool(&server, &tool, arguments)
            .await
        {
            Ok(result) => {
                let mut call = ToolCallOutcome::from(result);
                scrubber.scrub_tool_call_outcome(&mut call);
                let outcome = tinymcp_bus::McpCallOutcome::answered(&server, &tool);
                let envelope = tinymcp_bus::McpResultEnvelope::new(&server, &tool, &call.rendered)
                    .with_outcome(outcome.clone());
                let rendered = bound_mcp_tool_result(call.rendered);
                Ok(ClassifiedStaticToolCall {
                    outcome,
                    rendered: Some(rendered),
                    envelope: Some(envelope),
                    error_message: None,
                })
            }
            Err(error) => {
                let outcome = tinymcp_bus::McpCallOutcome::failed(
                    &server,
                    &tool,
                    tinymcp_bus::McpCallError {
                        code: error.wire_name().to_string(),
                        unauthorized: error.is_unauthorized(),
                        advertises_oauth: error.advertises_oauth(),
                    },
                );
                Ok(ClassifiedStaticToolCall {
                    outcome,
                    rendered: None,
                    envelope: None,
                    error_message: Some(scrubber.scrub(&format!("mcp_call_tool failed: {error}"))),
                })
            }
        }
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
        if !(1..=MAX_DRAIN).contains(&limit) {
            return Err(tinybus::Error::failed("invalid supervisor drain limit"));
        }
        match &self.maintenance {
            Some(maintenance) => maintenance.drain(limit),
            None => Ok(tinymcp_bus::SupervisorBatch::default()),
        }
    }

    /// `(config)` — opens opaque module-owned protocol state.
    async fn server_open(
        &self,
        config: tinymcp_bus::ServerSessionConfig,
    ) -> tinybus::Result<String> {
        self.server_sessions
            .open(config)
            .await
            .map_err(|error| Self::failed(&error))
    }
    /// `(config)` — opens session state with module-owned HTTP admission.
    async fn server_open_http(
        &self,
        config: tinymcp_bus::ServerHttpSessionConfig,
    ) -> tinybus::Result<String> {
        self.server_sessions
            .open_http(config)
            .await
            .map_err(|error| Self::failed(&error))
    }
    /// `(session, headers, input)` — submits or retries caller-known input.
    async fn server_submit(
        &self,
        session: String,
        headers: serde_json::Map<String, Value>,
        input: tinymcp_bus::ServerInput,
    ) -> tinybus::Result<()> {
        self.server_sessions
            .submit(&session, headers, input)
            .await
            .map_err(|error| Self::failed(&error))
    }
    /// `(session)` — observes a replayable operation snapshot.
    async fn server_poll(
        &self,
        session: String,
    ) -> tinybus::Result<tinymcp_bus::ServerOperationSnapshot> {
        self.server_sessions
            .poll(&session)
            .await
            .map_err(|error| Self::failed(&error))
    }
    /// `(session, callback, reply)` — completes host policy and dispatch.
    async fn server_complete(
        &self,
        session: String,
        callback: String,
        reply: tinymcp_bus::ServerHostReply,
    ) -> tinybus::Result<()> {
        self.server_sessions
            .complete(&session, &callback, reply)
            .await
            .map_err(|error| Self::failed(&error))
    }
    /// `(operation_ref)` — aborts only the caller-known target operation.
    async fn server_cancel(
        &self,
        operation: tinymcp_bus::ServerOperationRef,
    ) -> tinybus::Result<()> {
        self.server_sessions
            .cancel(&operation.session_id, &operation.operation_id)
            .await
            .map_err(|error| Self::failed(&error))
    }
    /// `(session)` — closes a session and all its protocol work.
    async fn server_close(&self, session: String) -> tinybus::Result<()> {
        self.server_sessions
            .close(&session)
            .await
            .map_err(|error| Self::failed(&error))
    }
    /// `()` — closes every protocol session owned by this object.
    async fn server_shutdown(&self) -> tinybus::Result<usize> {
        Ok(self.server_sessions.shutdown().await)
    }
    /// `(session)` — reports protocol initialization state selected by the module.
    async fn server_session_metadata(
        &self,
        session: String,
    ) -> tinybus::Result<tinymcp_bus::ServerSessionMetadata> {
        self.server_sessions
            .metadata(&session)
            .await
            .map_err(|error| Self::failed(&error))
    }
    /// `(request)` — applies HTTP GET/DELETE session admission and cleanup.
    async fn server_http_session(
        &self,
        request: tinymcp_bus::ServerHttpSessionRequest,
    ) -> tinybus::Result<tinymcp_bus::ServerHttpResponseMetadata> {
        std::future::ready(()).await;
        Ok(self.server_sessions.http_session(request).await)
    }

    /// `(request)` — bounded shared lexical text processing.
    async fn transform_text(
        &self,
        request: tinymcp_bus::TransformTextRequest,
    ) -> tinybus::Result<String> {
        std::future::ready(()).await;
        crate::processing::transform_text(&request).map_err(|error| Self::failed(&error))
    }
    /// `(arguments)` — tolerant argument decoding owned by the module.
    async fn normalize_tool_arguments(
        &self,
        arguments: Option<Value>,
    ) -> tinybus::Result<serde_json::Map<String, Value>> {
        std::future::ready(()).await;
        crate::processing::normalize_arguments(arguments).map_err(|error| Self::failed(&error))
    }
    /// `(tool)` — sanitized remote title/description metadata.
    async fn display_remote_tool(
        &self,
        tool: tinymcp_bus::DisplayRemoteToolRequest,
    ) -> tinybus::Result<tinymcp_bus::RemoteToolDisplay> {
        std::future::ready(()).await;
        crate::processing::display_remote_tool(&tool).map_err(|error| Self::failed(&error))
    }
    /// `(request)` — bounded tool output projection.
    async fn render_tool_output(
        &self,
        request: tinymcp_bus::RenderToolOutputRequest,
    ) -> tinybus::Result<String> {
        std::future::ready(()).await;
        crate::processing::render_tool_output(&request).map_err(|error| Self::failed(&error))
    }
    /// `(request)` — prepares module-owned names and schemas for host-screened tools.
    async fn prepare_agent_tools(
        &self,
        request: tinymcp_bus::PrepareAgentToolsRequest,
    ) -> tinybus::Result<Vec<tinymcp_bus::McpToolPresentation>> {
        std::future::ready(()).await;
        crate::processing::prepare_agent_tools(&request).map_err(|error| Self::failed(&error))
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

    /// `(request)` — opens one directory with its own client configuration.
    #[tinybus(confidential)]
    async fn open_configured(
        &self,
        request: OpenConfiguredRequest,
    ) -> tinybus::Result<OpenConfiguredResponse> {
        let Some(opener) = &self.opener else {
            return Err(Self::failed(&crate::Error::invalid_argument(
                "only the root object can open data directories",
            )));
        };

        opener
            .open_configured(&request.data_dir, &request.client)
            .await
            .map_err(|error| Self::failed(&error))
    }

    /// `(data_dir)` — starts closing one directory and returns a transition id.
    async fn close(&self, data_dir: String) -> tinybus::Result<Option<String>> {
        let Some(opener) = &self.opener else {
            return Err(Self::failed(&crate::Error::invalid_argument(
                "only the root object can close data directories",
            )));
        };

        opener
            .close(&data_dir)
            .await
            .map_err(|error| Self::failed(&error))
    }

    /// `(transition_id)` — observes lifecycle progress without waiting.
    async fn transition_status(&self, transition_id: String) -> tinybus::Result<TransitionStatus> {
        let Some(opener) = &self.opener else {
            return Err(Self::failed(&crate::Error::invalid_argument(
                "only the root object can observe directory transitions",
            )));
        };

        let status = opener.transition_status(&transition_id);
        Ok(std::future::ready(status).await)
    }
}

struct DispatchInterface {
    service: Arc<McpService>,
}

#[tinybus::__private::async_trait]
impl tinybus::service::Interface for DispatchInterface {
    fn name(&self) -> tinybus::InterfaceName {
        tinybus::service::Interface::name(self.service.as_ref())
    }

    fn members(&self) -> Vec<tinybus::MemberName> {
        tinybus::service::Interface::members(self.service.as_ref())
    }

    fn requires_confidential(&self, member: &tinybus::MemberName) -> bool {
        tinybus::service::Interface::requires_confidential(self.service.as_ref(), member)
    }

    async fn call(&self, member: &tinybus::MemberName, arguments: Value) -> tinybus::Result<Value> {
        // Lifecycle calls must reach their nonblocking transition gate while
        // the old root is draining; waiting on the dispatch gate while holding
        // the global object-tree reader would prevent the pending replacement
        // writer from acquiring that tree. Status is read-only and must remain
        // observable throughout the transition.
        if [
            tinymcp_bus::names::methods::OPEN,
            tinymcp_bus::names::methods::OPEN_CONFIGURED,
            tinymcp_bus::names::methods::CLOSE,
            tinymcp_bus::names::methods::TRANSITION_STATUS,
        ]
        .contains(&member.as_str())
        {
            return tinybus::service::Interface::call(self.service.as_ref(), member, arguments)
                .await;
        }
        let accepting = self.service.dispatch_gate.read().await;
        if !*accepting {
            return Err(tinybus::Error::MethodFailed {
                name: "ai.tinyhumans.tinymcp.Error.InvalidArgument".into(),
                message: "the directory service is closed".into(),
            });
        }
        tinybus::service::Interface::call(self.service.as_ref(), member, arguments).await
    }
}

/// Resolves a configured data directory to the absolute spelling used by Open.
pub(super) fn absolute_data_dir(path: &std::path::Path) -> Result<std::path::PathBuf> {
    std::path::absolute(path).map_err(|source| crate::error::Error::StoreIo {
        path: path.to_path_buf(),
        source: Box::new(source),
    })
}
