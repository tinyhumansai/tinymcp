//! The registry facade and its operations.

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

use super::install::{build_install_transport, collect_required_env_keys, pick_connection};
use crate::error::{Error, Result};
use crate::registry::{AuthDetection, Connections, OAuthFlow, Registries, SecretVault, Store};
use tinymcp_bus::{
    ConnStatus, ConnectOutcome, ConnectedServerOverview, InstallOutcome,
    InstalledServer, McpClientIdentityConfig, McpProxyConfig, McpRegistryAuthConfig, McpTool,
    RegistrySearchPage, RegistryServerDetail, RegistrySettings, ToolCallOutcome, UpdateEnvOutcome,
    UpdateEnvStatus,
};

/// The separator a source-routed name uses.
///
/// A caller installing from a catalog that is not in the default search set
/// prefixes the name with its source, so the detail lookup can be routed. The
/// prefix is addressing, not identity — see [`split_routing_name`].
const SOURCE_SEPARATOR: &str = "::";

/// Everything a host can ask the dynamic registry to do.
#[derive(Debug)]
pub struct McpRegistry {
    store: Store,
    connections: Connections,
    registries: Registries,
    oauth: OAuthFlow,
    vault: SecretVault,
    identity: McpClientIdentityConfig,
    proxy: Option<McpProxyConfig>,
}

impl McpRegistry {
    /// Builds the registry over an already-opened store.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ClientBuild`] when an HTTP client cannot be built.
    pub fn new(
        store: Store,
        registry_auth: McpRegistryAuthConfig,
        identity: McpClientIdentityConfig,
        proxy: Option<McpProxyConfig>,
    ) -> Result<Self> {
        Ok(Self {
            store,
            connections: Connections::new(),
            registries: Registries::new(registry_auth)?,
            oauth: OAuthFlow::new(proxy.clone())?,
            vault: SecretVault::new(),
            identity,
            proxy,
        })
    }

    /// The store, for a caller that needs it directly.
    #[must_use]
    pub fn store(&self) -> &Store {
        &self.store
    }

    /// The live connections.
    #[must_use]
    pub fn connections(&self) -> &Connections {
        &self.connections
    }

    /// The setup secret vault.
    #[must_use]
    pub fn vault(&self) -> &SecretVault {
        &self.vault
    }

    /// The authorization flow.
    #[must_use]
    pub fn oauth(&self) -> &OAuthFlow {
        &self.oauth
    }

    // -- browsing -----------------------------------------------------------

    /// Searches every catalog taking part in search, merged.
    ///
    /// The official catalog leads. Badging and the strict filter are applied by
    /// [`crate::registry::curation`] on top of this, by a caller that wants
    /// them — they are presentation choices, and a caller assembling its own
    /// view should not have to undo them.
    ///
    /// # Errors
    ///
    /// Returns whatever an upstream returns. A source that fails takes the call
    /// with it rather than silently returning a partial catalog that reads as
    /// "this server does not exist".
    pub async fn registry_search(
        &self,
        query: Option<&str>,
        page: u32,
        page_size: u32,
    ) -> Result<RegistrySearchPage> {
        let mut servers = Vec::new();
        let mut total_pages = page.max(1);

        for source in self.registries.searchable() {
            let (found, pages) = self
                .registries
                .search(&self.store, source, query, page, page_size)
                .await?;
            servers.extend(found);
            total_pages = total_pages.max(pages);
        }

        Ok(RegistrySearchPage {
            servers,
            page: page.max(1),
            total_pages,
        })
    }

    /// Fetches one server's detail, and the credential names installing it
    /// would need.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnknownServer`] when the name is blank or names no
    /// source, plus whatever the upstream returns.
    pub async fn registry_get(
        &self,
        qualified_name: &str,
    ) -> Result<(RegistryServerDetail, Vec<String>)> {
        let qualified_name = require_non_empty(qualified_name, "qualified_name")?;
        let (source, canonical) = split_routing_name(qualified_name);

        let detail = self.registries.get(&self.store, source, canonical).await?;
        let required = collect_required_env_keys(&detail);

        Ok((detail, required))
    }

    /// Reports which registry credentials are configured, with no values.
    #[must_use]
    pub fn registry_settings(&self) -> RegistrySettings {
        self.registries.settings()
    }

    // -- installs -----------------------------------------------------------

    /// Every installed server.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the store cannot be read.
    pub fn installed_list(&self) -> Result<Vec<InstalledServer>> {
        self.store.list_servers()
    }

    /// Installs a server from a catalog.
    ///
    /// # Installing is idempotent
    ///
    /// One record per service. A second install refreshes the credentials and
    /// configuration on the existing record rather than writing another. That
    /// matters because an install form is usually followed straight away by a
    /// connect: a user re-running it to replace an expired token must not
    /// silently reconnect with the old one.
    ///
    /// The check and the write are separated by an awaited catalog lookup, so
    /// the insert is conditional on the name still being absent. Losing that
    /// race refreshes the winner's record rather than leaving a duplicate.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnknownServer`] when the name is blank,
    /// [`Error::MalformedResponse`] when the server offers no way to connect,
    /// plus whatever the upstream and the store return.
    pub async fn install(
        &self,
        qualified_name: &str,
        env: BTreeMap<String, String>,
        config: Option<Value>,
    ) -> Result<InstallOutcome> {
        let qualified_name = require_non_empty(qualified_name, "qualified_name")?;
        let (source, canonical) = split_routing_name(qualified_name);

        if let Some(existing) = self.store.find_server_by_qualified_name(canonical)? {
            return self.refresh_install(existing, &env, config.as_ref());
        }

        let detail = self.registries.get(&self.store, source, canonical).await?;

        let picked = pick_connection(&detail.connections).ok_or_else(|| {
            Error::malformed(format!(
                "`{canonical}` offers neither a hosted endpoint nor a package; there is nothing to install"
            ))
        })?;
        let (transport, command_kind, command, args) =
            build_install_transport(canonical, picked)?;

        let server = InstalledServer {
            server_id: uuid::Uuid::new_v4().to_string(),
            qualified_name: canonical.to_string(),
            display_name: detail.display_name,
            description: detail.description,
            icon_url: detail.icon_url,
            command_kind,
            command,
            args,
            env_keys: env.keys().cloned().collect(),
            config,
            installed_at: now_ms(),
            last_connected_at: None,
            transport,
            enabled: true,
        };

        // Conditional on the name still being absent — see the note above.
        if !self.store.insert_server_if_absent(&server)? {
            let winner = self
                .store
                .find_server_by_qualified_name(canonical)?
                .ok_or_else(|| {
                    Error::malformed("an install lost a race but the winning record is gone")
                })?;
            return self.refresh_install(winner, &env, server.config.as_ref());
        }

        self.store.set_env_values(&server.server_id, &env)?;

        tracing::debug!(
            server_id = %server.server_id,
            qualified_name = canonical,
            "installed"
        );

        Ok(InstallOutcome {
            server,
            already_installed: false,
        })
    }

    /// Refreshes credentials and configuration onto an existing record.
    ///
    /// Credentials are *merged*, not replaced: an install form that sends only
    /// the one field the user retyped must not erase the ones it could not
    /// display.
    fn refresh_install(
        &self,
        mut existing: InstalledServer,
        env: &BTreeMap<String, String>,
        config: Option<&Value>,
    ) -> Result<InstallOutcome> {
        if !env.is_empty() {
            let mut merged = self.store.load_env_values(&existing.server_id)?;
            merged.extend(env.clone());
            self.store.set_env_values(&existing.server_id, &merged)?;

            let names: Vec<String> = merged.keys().cloned().collect();
            if existing.env_keys != names {
                self.store.update_env_keys(&existing.server_id, &names)?;
                existing.env_keys = names;
            }
        }

        if let Some(config) = config {
            self.store.update_config(&existing.server_id, Some(config))?;
            existing.config = Some(config.clone());
        }

        Ok(InstallOutcome {
            server: existing,
            already_installed: true,
        })
    }

    /// Removes an install, disconnecting it first.
    ///
    /// Returns whether a record went. The credentials go with it, through the
    /// store's cascade.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnknownServer`] when the identifier is blank, plus
    /// [`Error::Store`] when the delete fails.
    pub async fn uninstall(&self, server_id: &str) -> Result<bool> {
        let server_id = require_non_empty(server_id, "server_id")?;
        self.connections.disconnect(server_id).await;
        self.store.delete_server(server_id)
    }

    /// Turns an install on or off.
    ///
    /// Turning it off disconnects any live session, so its tools disappear at
    /// once, and keeps the record and its credentials so turning it back on
    /// needs no retyping. Turning it on does *not* connect: being enabled is a
    /// setting, being connected is a state, and conflating them means a user
    /// cannot enable a server without also dialling it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnknownServer`] when the identifier is blank or names
    /// no install, plus [`Error::Store`] when the update fails.
    pub async fn set_enabled(&self, server_id: &str, enabled: bool) -> Result<()> {
        let server_id = require_non_empty(server_id, "server_id")?;

        // Checked before the write, so a bad identifier is a clear error rather
        // than a silent no-op.
        self.store.get_server(server_id)?;
        self.store.update_enabled(server_id, enabled)?;

        if !enabled {
            self.connections.disconnect(server_id).await;
            self.connections.clear_last_error(server_id).await;
        }

        Ok(())
    }

    /// Replaces an install's credentials and reconnects it.
    ///
    /// Merged over what is stored, for the reason in [`Self::refresh_install`].
    /// The credentials are persisted *before* the reconnect and kept whatever
    /// it does — see [`UpdateEnvOutcome`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnknownServer`] when the identifier is blank or names
    /// no install, plus [`Error::Store`] when the write fails. A failed
    /// reconnect is reported in the outcome, not as an error: the operation
    /// asked for — storing the credentials — succeeded.
    pub async fn update_env(
        &self,
        server_id: &str,
        env: BTreeMap<String, String>,
    ) -> Result<UpdateEnvOutcome> {
        let server_id = require_non_empty(server_id, "server_id")?;

        let mut merged = self.store.load_env_values(server_id)?;
        merged.extend(env);
        self.store.set_env_values(server_id, &merged)?;

        self.connections.disconnect(server_id).await;

        let mut server = self.store.get_server(server_id)?;

        let names: Vec<String> = merged.keys().cloned().collect();
        if server.env_keys != names {
            self.store.update_env_keys(server_id, &names)?;
            server.env_keys = names;
        }

        // A server the user turned off must not come back up because its
        // credentials changed. The new values are stored and will be used when
        // they turn it on.
        if !server.enabled {
            return Ok(UpdateEnvOutcome {
                server_id: server_id.to_string(),
                status: UpdateEnvStatus::Disabled,
                env_keys: server.env_keys,
                tools: Vec::new(),
                auth_hint: None,
                error: None,
            });
        }

        match self
            .connections
            .connect(
                &self.store,
                &self.oauth,
                &self.identity,
                self.proxy.as_ref(),
                &server,
            )
            .await
        {
            Ok(tools) => Ok(UpdateEnvOutcome {
                server_id: server_id.to_string(),
                status: UpdateEnvStatus::Connected,
                env_keys: server.env_keys,
                tools,
                auth_hint: None,
                error: None,
            }),
            Err(error) => {
                let auth_hint = self.connections.auth_hint(server_id).await;
                Ok(UpdateEnvOutcome {
                    server_id: server_id.to_string(),
                    status: if auth_hint.is_some() {
                        UpdateEnvStatus::Unauthorized
                    } else {
                        UpdateEnvStatus::Disconnected
                    },
                    env_keys: server.env_keys,
                    tools: Vec::new(),
                    // A 401's message is withheld; only its code crosses.
                    error: auth_hint.is_none().then(|| error.to_string()),
                    auth_hint,
                })
            }
        }
    }

    // -- connections --------------------------------------------------------

    /// Connects an install.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnknownServer`] when the identifier is blank or names
    /// no install, [`Error::MalformedResponse`] when it is turned off, plus
    /// whatever the transport returns.
    pub async fn connect(&self, server_id: &str) -> Result<ConnectOutcome> {
        let server_id = require_non_empty(server_id, "server_id")?;
        let server = self.store.get_server(server_id)?;

        if !server.enabled {
            return Err(Error::malformed(format!(
                "`{server_id}` is turned off; turn it on before connecting"
            )));
        }

        let tools = self
            .connections
            .connect(
                &self.store,
                &self.oauth,
                &self.identity,
                self.proxy.as_ref(),
                &server,
            )
            .await?;

        Ok(ConnectOutcome {
            server_id: server_id.to_string(),
            tools,
        })
    }

    /// Disconnects an install, reporting whether one was live.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnknownServer`] when the identifier is blank.
    pub async fn disconnect(&self, server_id: &str) -> Result<bool> {
        let server_id = require_non_empty(server_id, "server_id")?;
        Ok(self.connections.disconnect(server_id).await)
    }

    /// Where every install stands.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the installs cannot be listed.
    pub async fn status(&self) -> Result<Vec<ConnStatus>> {
        self.connections.all_status(&self.store).await
    }

    /// Every connected server's identity and tools.
    pub async fn connected_overview(&self) -> Vec<ConnectedServerOverview> {
        self.connections.connected_overview().await
    }

    // -- authorization ------------------------------------------------------

    /// Classifies what a server wants before it will talk.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnknownServer`] when the identifier is blank or names
    /// no install, plus [`Error::Store`] when it cannot be read.
    pub async fn detect_auth(&self, server_id: &str) -> Result<AuthDetection> {
        let server_id = require_non_empty(server_id, "server_id")?;
        self.oauth.detect(&self.store, server_id).await
    }

    /// Starts a browser sign-in and returns the URL to open.
    ///
    /// `redirect_uri` is the loopback address the host is listening on.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnknownServer`] when the identifier is blank, plus
    /// whatever discovery and registration return.
    pub async fn oauth_begin(&self, server_id: &str, redirect_uri: &str) -> Result<String> {
        let server_id = require_non_empty(server_id, "server_id")?;
        self.oauth
            .begin(&self.store, server_id, redirect_uri)
            .await
    }

    /// Finishes a browser sign-in and connects the server.
    ///
    /// The connect is here rather than in the authorization flow because this
    /// is the layer that owns both — see that module's note on why it does not
    /// reach for the connection map itself.
    ///
    /// # Errors
    ///
    /// Returns whatever the token exchange returns. A stored token followed by
    /// a failed connect is *not* an error: the sign-in worked, and the outcome
    /// carries no tools.
    pub async fn oauth_complete(&self, state: &str, code: &str) -> Result<ConnectOutcome> {
        let server_id = self.oauth.complete(&self.store, state, code).await?;

        match self.connect(&server_id).await {
            Ok(outcome) => Ok(outcome),
            Err(error) => {
                tracing::warn!(
                    server_id,
                    "the sign-in succeeded but connecting afterwards did not: {error}"
                );
                Ok(ConnectOutcome {
                    server_id,
                    tools: Vec::new(),
                })
            }
        }
    }

    // -- tools --------------------------------------------------------------

    /// The tools a connected server advertises.
    ///
    /// Reads the cached snapshot rather than re-handshaking, which is what
    /// makes this cheap enough to call before every use.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnknownServer`] when the identifier is blank or the
    /// server is not connected.
    pub async fn list_tools(&self, server_id: &str) -> Result<Vec<McpTool>> {
        let server_id = require_non_empty(server_id, "server_id")?;

        self.connections
            .tools_for(server_id)
            .await
            .ok_or_else(|| Error::UnknownServer {
                server: server_id.to_string(),
            })
    }

    /// Calls a tool on a connected server.
    ///
    /// A tool that reports failure comes back as a successful call with the
    /// flag set; see [`ToolCallOutcome`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnknownServer`] when either name is blank or the server
    /// is not connected, plus whatever the transport returns.
    pub async fn tool_call(
        &self,
        server_id: &str,
        tool_name: &str,
        arguments: Value,
    ) -> Result<ToolCallOutcome> {
        let server_id = require_non_empty(server_id, "server_id")?;
        let tool_name = require_non_empty(tool_name, "tool_name")?;

        let result = self
            .connections
            .call_tool(server_id, tool_name, arguments)
            .await?;

        Ok(ToolCallOutcome {
            is_error: result.rendered.is_error,
            result: result.raw_result,
        })
    }

    /// Gathers what a model would need to help a user configure a server.
    ///
    /// Returns the catalog detail and the credential names an install needs.
    /// Running a model over it is the host's — see the module note.
    ///
    /// # Errors
    ///
    /// As [`Self::registry_get`].
    pub async fn config_assist(
        &self,
        qualified_name: &str,
    ) -> Result<(RegistryServerDetail, Vec<String>)> {
        self.registry_get(qualified_name).await
    }
}

/// Splits a possibly source-routed name into its source and the bare name.
///
/// A caller installing from a catalog outside the default search set writes
/// `<source>::<name>` so the detail lookup can be routed. The prefix is
/// addressing, not identity: the store keys on the bare name, so an install
/// that kept the prefix would write a second record for a service already
/// installed under its plain name.
fn split_routing_name(qualified_name: &str) -> (&str, &str) {
    match qualified_name.split_once(SOURCE_SEPARATOR) {
        Some((source, name)) => (source, name),
        // No prefix means the official catalog, which is where an unrouted name
        // comes from.
        None => (
            crate::registry::sources::SOURCE_MCP_OFFICIAL,
            qualified_name,
        ),
    }
}

/// Trims an identifier and refuses a blank one.
fn require_non_empty<'a>(value: &'a str, field: &str) -> Result<&'a str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(Error::UnknownServer {
            server: format!("<blank {field}>"),
        });
    }
    Ok(trimmed)
}

/// The current time in Unix epoch milliseconds.
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_millis()).ok())
        .unwrap_or(0)
}
