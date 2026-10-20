//! The store and its operations.

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tinystoragedrivers_core::{
    DocumentStoreExt as _, ErrorKind, Filter, Precondition, Query, Repo, Sort,
};

use crate::error::{Error, Result};
use crate::storage::{
    CACHE_KEY_SECRET, ENV_SECRET_PREFIX, REGISTRY_CACHE, SERVER_NAMES, SERVERS, StoragePorts,
    TOOL_CACHE, doc_id, write_options,
};
use crate::{CommandKindExt, TransportExt};
use tinymcp_bus::{CommandKind, InstalledServer, Transport};

const CACHE_FINGERPRINT_KEY_BYTES: usize = 32;

/// How long a cached browse response stays fresh.
const CACHE_TTL_MS: i64 = 10 * 60 * 1_000;

/// Persistence for installed servers, their credentials, and the browse cache.
///
/// Written against the storage ports (see [`crate::storage`]): documents for
/// the records, secrets for the credential values. The API is synchronous;
/// each call runs its port future on a shared bridge thread.
pub struct Store {
    pub(super) ports: StoragePorts,
    cache_fingerprint_key: [u8; CACHE_FINGERPRINT_KEY_BYTES],
}

impl std::fmt::Debug for Store {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Store").finish_non_exhaustive()
    }
}

/// An installed server as stored: the record's fields plus the provenance
/// flag, in the shape the `mcp_servers` collection indexes.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ServerDoc {
    server_id: String,
    qualified_name: String,
    display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    icon_url: Option<String>,
    command_kind: String,
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env_keys: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    config: Option<Value>,
    installed_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_connected_ms: Option<i64>,
    #[serde(default)]
    transport: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    deployment_url: Option<String>,
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default)]
    catalog_managed: bool,
}

const fn default_true() -> bool {
    true
}

impl ServerDoc {
    fn encode(server: &InstalledServer, catalog_managed: bool) -> Self {
        Self {
            server_id: server.server_id.clone(),
            qualified_name: server.qualified_name.clone(),
            display_name: server.display_name.clone(),
            description: server.description.clone(),
            icon_url: server.icon_url.clone(),
            command_kind: server.command_kind.as_str().to_string(),
            command: server.command.clone(),
            args: server.args.clone(),
            env_keys: server.env_keys.clone(),
            config: server.config.clone(),
            installed_ms: server.installed_at,
            last_connected_ms: server.last_connected_at,
            transport: server.transport.dispatch_kind().to_string(),
            deployment_url: server.transport.deployment_url().map(ToString::to_string),
            enabled: server.enabled,
            catalog_managed,
        }
    }

    fn decode(self) -> InstalledServer {
        InstalledServer {
            server_id: self.server_id,
            qualified_name: self.qualified_name,
            display_name: self.display_name,
            description: self.description,
            icon_url: self.icon_url,
            command_kind: CommandKind::parse(&self.command_kind),
            command: self.command,
            args: self.args,
            env_keys: self.env_keys,
            config: self.config,
            installed_at: self.installed_ms,
            last_connected_at: self.last_connected_ms,
            transport: Transport::parse(&self.transport, self.deployment_url.as_deref()),
            enabled: self.enabled,
        }
    }
}

/// The claim that keeps one qualified name to one install.
#[derive(Debug, Serialize, Deserialize)]
struct NameClaim {
    qualified_name: String,
    server_id: String,
    /// When the claim was made (Unix ms). A claim without its server only
    /// counts as abandoned once it is older than [`CLAIM_LEASE_MS`]; a claim
    /// that predates this field reads as `0`, i.e. long expired.
    #[serde(default)]
    claimed_ms: i64,
}

/// How long a claim may wait for its server row before another installer may
/// treat it as left behind by a crash.
const CLAIM_LEASE_MS: i64 = 60_000;

/// A cached browse response.
#[derive(Debug, Serialize, Deserialize)]
struct CacheDoc {
    cache_key: String,
    /// The upstream response, as the JSON it is.
    body: Value,
    cached_ms: i64,
}

impl Store {
    /// Opens a store over `ports`, declaring the collections it uses and
    /// loading (or creating) the key the tool-cache fingerprints are keyed
    /// with.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the ports refuse the collections or the
    /// secret cannot be read or written.
    pub fn open(ports: StoragePorts) -> Result<Self> {
        ports.prepare()?;
        let cache_fingerprint_key = load_cache_fingerprint_key(&ports)?;
        Ok(Self {
            ports,
            cache_fingerprint_key,
        })
    }

    /// Opens a store over a private in-memory backend, for tests and for a
    /// host that wants no persistence.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the backend cannot be opened.
    pub fn open_in_memory() -> Result<Self> {
        Self::open(StoragePorts::memory()?)
    }

    /// The ports this store writes through.
    #[must_use]
    pub fn ports(&self) -> &StoragePorts {
        &self.ports
    }

    pub(super) fn keyed_cache_fingerprint(&self, fingerprint: &str) -> String {
        super::tool_cache::keyed_fingerprint(&self.cache_fingerprint_key, fingerprint)
    }

    // -- installed servers --------------------------------------------------

    /// Inserts a server row.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the insert fails, including when the
    /// identifier is already taken.
    pub fn insert_server(&self, server: &InstalledServer) -> Result<()> {
        let doc = serde_json::to_value(ServerDoc::encode(server, false))?;
        let id = key(&[&server.server_id])?;
        self.ports
            .run("inserting a server", move |ports| async move {
                ports
                    .documents
                    .put_with(SERVERS, &id, doc, Precondition::Absent, write_options())
                    .await
                    .map(|_| ())
            })
    }

    /// Inserts a server only if no row already carries its qualified name.
    ///
    /// Returns whether this call inserted it.
    ///
    /// # Why a claim
    ///
    /// The install flow reads for an existing install before writing, and an
    /// awaited registry lookup sits between that read and the write. The
    /// document key is the install identifier, not the qualified name, so two
    /// concurrent installs of the same service could both miss and both
    /// insert. A claim document keyed by the qualified name, written with
    /// `Precondition::Absent`, lets exactly one of them proceed.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the statement fails.
    pub fn insert_server_if_absent(&self, server: &InstalledServer) -> Result<bool> {
        self.insert_server_if_absent_with_origin(server, false)
    }

    /// Inserts a validated catalog install with launch-time provenance.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the statement fails.
    pub(crate) fn insert_catalog_server_if_absent(&self, server: &InstalledServer) -> Result<bool> {
        self.insert_server_if_absent_with_origin(server, true)
    }

    fn insert_server_if_absent_with_origin(
        &self,
        server: &InstalledServer,
        catalog_managed: bool,
    ) -> Result<bool> {
        let doc = serde_json::to_value(ServerDoc::encode(server, catalog_managed))?;
        let claim = serde_json::to_value(NameClaim {
            qualified_name: server.qualified_name.clone(),
            server_id: server.server_id.clone(),
            claimed_ms: now_ms(),
        })?;
        let id = server.server_id.clone();
        let server_key = key(&[&server.server_id])?;
        let claim_id = key(&[&server.qualified_name])?;
        let name = server.qualified_name.clone();
        self.ports
            .run("inserting a server if absent", move |ports| async move {
                let documents = ports.documents;
                // A row written without a claim (a plain insert) also counts.
                let existing = documents
                    .query(
                        SERVERS,
                        &Query::filter(Filter::eq("qualified_name", name.clone())).limit(1),
                    )
                    .await?;
                if !existing.items.is_empty() {
                    return Ok(false);
                }
                if !acquire_claim(documents.as_ref(), &claim_id, claim).await? {
                    return Ok(false);
                }
                let owns_claim = |held: Option<tinystoragedrivers_core::Versioned<Value>>| {
                    held.filter(|claim| {
                        claim.doc.get("server_id").and_then(Value::as_str) == Some(id.as_str())
                    })
                };
                if let Err(error) = documents
                    .put_with(
                        SERVERS,
                        &server_key,
                        doc,
                        Precondition::Absent,
                        write_options(),
                    )
                    .await
                {
                    // Release only a claim that is still ours, at the version
                    // we read, so a racing installer's takeover survives.
                    if let Ok(held) = documents.get(SERVER_NAMES, &claim_id).await
                        && let Some(own) = owns_claim(held)
                    {
                        let _ = documents
                            .delete(SERVER_NAMES, &claim_id, own.unchanged())
                            .await;
                    }
                    return Err(error);
                }
                // A racing installer may have judged our claim stale (our
                // server was not written yet) and taken it over. Re-read it:
                // if it no longer names us, back out so one name never holds
                // two installs.
                let still_ours = owns_claim(documents.get(SERVER_NAMES, &claim_id).await?);
                if still_ours.is_none() {
                    tracing::debug!(server_id = %id, "install claim taken over; backing out");
                    documents
                        .delete(SERVERS, &server_key, Precondition::None)
                        .await?;
                    return Ok(false);
                }
                Ok(true)
            })
    }

    /// Whether this row was installed from catalog-supplied connection data.
    /// Old rows and host-authored configuration default to false; their
    /// provenance cannot be inferred safely from the command text.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the lookup fails.
    pub(crate) fn is_catalog_server(&self, server_id: &str) -> Result<bool> {
        let id = key(&[server_id])?;
        self.ports
            .run("reading catalog provenance", move |ports| async move {
                Ok(ports
                    .documents
                    .get(SERVERS, &id)
                    .await?
                    .and_then(|found| found.doc.get("catalog_managed").and_then(Value::as_bool))
                    .unwrap_or(false))
            })
    }

    /// Every installed server, oldest install first.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the query fails or a row cannot be read.
    pub fn list_servers(&self) -> Result<Vec<InstalledServer>> {
        let rows = self.ports.run("listing servers", |ports| async move {
            ports
                .documents
                .query_all(SERVERS, &Query::all().sort(Sort::asc("installed_ms")))
                .await
        })?;
        rows.into_iter().map(|row| decode_server(row.doc)).collect()
    }

    /// One server by its install identifier.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnknownServer`] when there is no such install, and
    /// [`Error::Store`] when the query fails.
    pub fn get_server(&self, server_id: &str) -> Result<InstalledServer> {
        self.find_server(server_id)?
            .ok_or_else(|| Error::UnknownServer {
                server: server_id.to_string(),
            })
    }

    /// One server by its install identifier, or `None`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the query fails.
    pub fn find_server(&self, server_id: &str) -> Result<Option<InstalledServer>> {
        let id = key(&[server_id])?;
        let found = self
            .ports
            .run("looking a server up", move |ports| async move {
                ports.documents.get(SERVERS, &id).await
            })?;
        found.map(|row| decode_server(row.doc)).transpose()
    }

    /// The earliest install carrying `qualified_name`, if any.
    ///
    /// Several installs of one service are possible — the key is the install
    /// identifier — so this returns the oldest, which is what keeps
    /// installing idempotent.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the query fails.
    pub fn find_server_by_qualified_name(
        &self,
        qualified_name: &str,
    ) -> Result<Option<InstalledServer>> {
        let name = qualified_name.to_string();
        let page = self.ports.run(
            "looking a server up by qualified name",
            move |ports| async move {
                ports
                    .documents
                    .query(
                        SERVERS,
                        &Query::filter(Filter::eq("qualified_name", name))
                            .sort(Sort::asc("installed_ms"))
                            .limit(1),
                    )
                    .await
            },
        )?;
        page.items
            .into_iter()
            .next()
            .map(|row| decode_server(row.doc))
            .transpose()
    }

    /// Removes a server, its credentials and its cached tools, and returns
    /// whether a row went.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the delete fails.
    pub fn delete_server(&self, server_id: &str) -> Result<bool> {
        let id = server_id.to_string();
        let server_key = key(&[server_id])?;
        self.ports
            .run("deleting a server", move |ports| async move {
                let documents = ports.documents;
                let existing = documents.get(SERVERS, &server_key).await?;
                let removed = documents
                    .delete(SERVERS, &server_key, Precondition::None)
                    .await?;
                // The tool cache is keyed by the same identifier but carries no
                // link to the server (static servers share the collection), so it
                // goes by hand. An uninstalled server must not keep surfacing
                // tools.
                documents
                    .delete(TOOL_CACHE, &server_key, Precondition::None)
                    .await?;
                ports.secrets.delete(&env_secret_name(&id)).await?;
                if let Some(name) = existing
                    .as_ref()
                    .and_then(|row| row.doc.get("qualified_name"))
                    .and_then(Value::as_str)
                {
                    let claim_id = doc_id(&[name])?;
                    // Only the claim this install holds; another install of the
                    // same name may have taken it over.
                    let claim = documents
                        .get(SERVER_NAMES, &claim_id)
                        .await?
                        .filter(|claim| {
                            claim.doc.get("server_id").and_then(Value::as_str) == Some(id.as_str())
                        });
                    if let Some(claim) = claim {
                        documents
                            .delete(SERVER_NAMES, &claim_id, claim.unchanged())
                            .await
                            .or_else(|error| {
                                if error.kind() == ErrorKind::Conflict {
                                    Ok(false)
                                } else {
                                    Err(error)
                                }
                            })?;
                    }
                }
                Ok(removed)
            })
    }

    /// Replaces a server's recorded environment variable *names*.
    ///
    /// The values live in the secret store; this is the list shown in a
    /// listing.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the update fails.
    pub fn update_env_keys(&self, server_id: &str, env_keys: &[String]) -> Result<()> {
        let env_keys = env_keys.to_vec();
        self.update_server(
            "updating a server's environment keys",
            server_id,
            move |doc| {
                doc.env_keys.clone_from(&env_keys);
            },
        )
    }

    /// Replaces a server's stored configuration blob. `None` clears it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the update fails.
    pub fn update_config(&self, server_id: &str, config: Option<&Value>) -> Result<()> {
        let config = config.cloned();
        self.update_server("updating a server's configuration", server_id, move |doc| {
            doc.config.clone_from(&config);
        })
    }

    /// Turns a server on or off without uninstalling it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the update fails.
    pub fn update_enabled(&self, server_id: &str, enabled: bool) -> Result<()> {
        self.update_server("updating a server's enabled flag", server_id, move |doc| {
            doc.enabled = enabled;
        })
    }

    /// Records that a server connected successfully, just now.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the update fails.
    pub fn touch_last_connected(&self, server_id: &str) -> Result<()> {
        let now = now_ms();
        self.update_server("recording a successful connection", server_id, move |doc| {
            doc.last_connected_ms = Some(now);
        })
    }

    /// Applies `change` to one server row, retrying when another writer got
    /// there first. A missing row is left alone, as an `UPDATE` matching no
    /// row would.
    fn update_server(
        &self,
        action: &'static str,
        server_id: &str,
        change: impl Fn(&mut ServerDoc) + Send + Sync + 'static,
    ) -> Result<()> {
        let id = key(&[server_id])?;
        self.ports.run(action, move |ports| async move {
            Repo::<ServerDoc>::new(&ports.documents, SERVERS)?
                .with_options(write_options())
                .update(&id, |current| {
                    Ok(current.map(|mut doc| {
                        change(&mut doc);
                        doc
                    }))
                })
                .await
                .map(|_| ())
        })
    }

    // -- credentials --------------------------------------------------------

    /// Replaces every stored credential for a server.
    ///
    /// All of a server's values are one secret, so a name removed from `env`
    /// stops being sent to the server rather than lingering, and the swap is
    /// a single write.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the write fails.
    pub fn set_env_values(&self, server_id: &str, env: &BTreeMap<String, String>) -> Result<()> {
        let name = env_secret_name(server_id);
        let bytes = serde_json::to_vec(env)?;
        self.ports
            .run("storing credentials", move |ports| async move {
                if bytes == b"{}" {
                    ports.secrets.delete(&name).await.map(|_| ())
                } else {
                    ports.secrets.set(&name, &bytes).await
                }
            })
    }

    /// Loads a server's credentials, for spawning it.
    ///
    /// The values returned here must never be serialized into a response or
    /// written to a log.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the read fails.
    pub fn load_env_values(&self, server_id: &str) -> Result<BTreeMap<String, String>> {
        let name = env_secret_name(server_id);
        let bytes = self
            .ports
            .run("loading credentials", move |ports| async move {
                ports.secrets.get(&name).await
            })?;
        match bytes {
            None => Ok(BTreeMap::new()),
            Some(bytes) => Ok(serde_json::from_slice(&bytes)?),
        }
    }

    // -- browse cache -------------------------------------------------------

    /// A cached browse response, if one is present and still fresh.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the query fails.
    pub fn cached(&self, cache_key: &str) -> Result<Option<String>> {
        Ok(self
            .read_cache(cache_key)?
            .filter(|entry| now_ms() - entry.cached_ms < CACHE_TTL_MS)
            .map(|entry| entry.body.to_string()))
    }

    /// A cached browse response, however old.
    ///
    /// For serving something when the upstream cannot answer; a fresh answer
    /// reads [`Self::cached`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the query fails.
    pub(crate) fn cached_stale(&self, cache_key: &str) -> Result<Option<String>> {
        Ok(self
            .read_cache(cache_key)?
            .map(|entry| entry.body.to_string()))
    }

    fn read_cache(&self, cache_key: &str) -> Result<Option<CacheDoc>> {
        let id = key(&[cache_key])?;
        let found = self
            .ports
            .run("reading the browse cache", move |ports| async move {
                ports.documents.get(REGISTRY_CACHE, &id).await
            })?;
        found
            .map(|row| serde_json::from_value(row.doc).map_err(Error::from))
            .transpose()
    }

    /// Every cached browse response whose key starts with `prefix`, however
    /// old, newest first.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the query fails.
    pub(crate) fn cached_with_prefix(&self, prefix: &str) -> Result<Vec<String>> {
        let lower = prefix.to_string();
        let upper = format!("{prefix}\u{10FFFF}");
        let rows = self
            .ports
            .run("reading the browse cache", move |ports| async move {
                ports
                    .documents
                    .query_all(
                        REGISTRY_CACHE,
                        &Query::filter(
                            Filter::gte("cache_key", lower).and(Filter::lt("cache_key", upper)),
                        )
                        .sort(Sort::desc("cached_ms"))
                        .sort(Sort::asc("cache_key")),
                    )
                    .await
            })?;
        let mut bodies = Vec::new();
        for row in rows {
            let entry: CacheDoc = serde_json::from_value(row.doc)?;
            if entry.cache_key.starts_with(prefix) {
                bodies.push(entry.body.to_string());
            }
        }
        Ok(bodies)
    }

    /// Caches a browse response against the current time.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Serialization`] when `body_json` is not JSON, and
    /// [`Error::Store`] when the write fails.
    pub fn cache(&self, cache_key: &str, body_json: &str) -> Result<()> {
        self.write_cache(cache_key, body_json, now_ms())
    }

    fn write_cache(&self, cache_key: &str, body_json: &str, cached_ms: i64) -> Result<()> {
        let id = key(&[cache_key])?;
        let doc = serde_json::to_value(CacheDoc {
            cache_key: cache_key.to_string(),
            body: serde_json::from_str(body_json)?,
            cached_ms,
        })?;
        self.ports
            .run("writing to the browse cache", move |ports| async move {
                ports
                    .documents
                    .put_with(
                        REGISTRY_CACHE,
                        &id,
                        doc,
                        Precondition::None,
                        write_options(),
                    )
                    .await
                    .map(|_| ())
            })
    }
}

/// Writes the claim on a qualified name. Returns whether this caller holds it.
///
/// A claim whose server is gone (a crash between the two writes) is stale and
/// is taken over once, but only after its lease: within it the holder may
/// simply not have written its server yet.
async fn acquire_claim(
    documents: &dyn tinystoragedrivers_core::DocumentStore,
    claim_id: &str,
    claim: Value,
) -> tinystoragedrivers_core::Result<bool> {
    match documents
        .put_with(
            SERVER_NAMES,
            claim_id,
            claim.clone(),
            Precondition::Absent,
            write_options(),
        )
        .await
    {
        Ok(_) => return Ok(true),
        Err(error) if error.kind() != ErrorKind::Conflict => return Err(error),
        Err(_) => {}
    }
    let Some(held) = documents.get(SERVER_NAMES, claim_id).await? else {
        return Ok(false);
    };
    let holder = held
        .doc
        .get("server_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if documents
        .get(SERVERS, &doc_id(&[&holder])?)
        .await?
        .is_some()
    {
        return Ok(false);
    }
    let claimed_ms = held
        .doc
        .get("claimed_ms")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    if now_ms().saturating_sub(claimed_ms) < CLAIM_LEASE_MS {
        return Ok(false);
    }
    match documents
        .put_with(
            SERVER_NAMES,
            claim_id,
            claim,
            held.unchanged(),
            write_options(),
        )
        .await
    {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == ErrorKind::Conflict => Ok(false),
        Err(error) => Err(error),
    }
}

/// The document id for `parts`, as a store error when it cannot be built.
fn key(parts: &[&str]) -> Result<String> {
    doc_id(parts).map_err(|source| Error::store("building a document id", source))
}

fn decode_server(doc: Value) -> Result<InstalledServer> {
    Ok(serde_json::from_value::<ServerDoc>(doc)?.decode())
}

/// The secret holding one server's environment and OAuth values.
fn env_secret_name(server_id: &str) -> String {
    format!("{ENV_SECRET_PREFIX}{server_id}")
}

/// Serialises first-time creation of the cache fingerprint key in this process.
static CACHE_KEY_INIT: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Reads the tool-cache fingerprint key from the secret store, creating it
/// the first time. A store whose secret cannot be written fails to open
/// rather than keying the cache with a key it cannot keep.
///
/// The secret port has no create-if-absent operation, so concurrent openers in
/// this process are serialised by [`CACHE_KEY_INIT`]: the first creates the key
/// and the rest read it. Two *processes* sharing one backend can still race;
/// that needs a compare-and-set on the secret port (tinystoragedrivers).
fn load_cache_fingerprint_key(ports: &StoragePorts) -> Result<[u8; CACHE_FINGERPRINT_KEY_BYTES]> {
    let _serialised = CACHE_KEY_INIT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let candidate = super::tool_cache::hex(&new_cache_fingerprint_key());
    // Hex text, so any secret driver can hold it.
    let stored = ports.run("loading the cache key", move |ports| async move {
        if let Some(bytes) = ports.secrets.get(CACHE_KEY_SECRET).await? {
            return Ok(bytes.to_vec());
        }
        ports
            .secrets
            .set(CACHE_KEY_SECRET, candidate.as_bytes())
            .await?;
        // Another opener may have written between the read and the write;
        // whatever is stored now is the key.
        Ok(ports
            .secrets
            .get(CACHE_KEY_SECRET)
            .await?
            .map_or_else(|| candidate.into_bytes(), |bytes| bytes.to_vec()))
    })?;
    decode_hex_key(&stored).ok_or_else(|| {
        Error::store(
            "loading the cache key",
            tinystoragedrivers_core::StorageError::serialization(
                "the stored cache fingerprint key is malformed",
            ),
        )
    })
}

/// Parses the stored 64-character hex key.
fn decode_hex_key(text: &[u8]) -> Option<[u8; CACHE_FINGERPRINT_KEY_BYTES]> {
    let text = std::str::from_utf8(text).ok()?;
    if text.len() != CACHE_FINGERPRINT_KEY_BYTES * 2 || !text.is_ascii() {
        return None;
    }
    let mut key = [0; CACHE_FINGERPRINT_KEY_BYTES];
    for (slot, pair) in key.iter_mut().zip(text.as_bytes().chunks(2)) {
        *slot = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(key)
}

fn new_cache_fingerprint_key() -> [u8; CACHE_FINGERPRINT_KEY_BYTES] {
    let mut key = [0; CACHE_FINGERPRINT_KEY_BYTES];
    key[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    key[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    key
}

/// The current time in Unix epoch milliseconds.
///
/// A clock set before the epoch reads as zero rather than failing. Nothing here
/// makes a decision that a wrong timestamp could make unsafe: the worst case is
/// a cache entry that looks stale.
pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_millis()).ok())
        .unwrap_or(0)
}
