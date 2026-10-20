//! The handles the module works through, and the bridge to its synchronous API.

use std::future::Future;
use std::sync::{Arc, OnceLock};

use tinymcp_bus::CollectionSpec as BusCollectionSpec;
use tinystoragedrivers_core::{
    Blocking, CollectionSpec, DocumentStore, IndexSpec, MemoryStorage, Scope, ScopedStorage,
    SearchSpec, StorageBackend, StreamStore,
};
use tinystoragedrivers_secrets::{MemorySecrets, SecretStore};

use super::{AUDIT_STREAM, SERVERS, storage_needs};
use crate::error::{Error, Result};

/// The three ports the module persists through.
///
/// Cheap to clone: every field is a shared handle. The module's stores are
/// synchronous, so each call runs its port future on one shared
/// [`Blocking`] bridge thread.
#[derive(Clone)]
pub struct StoragePorts {
    pub(crate) documents: Arc<dyn DocumentStore>,
    pub(crate) streams: Arc<dyn StreamStore>,
    pub(crate) secrets: Arc<dyn SecretStore>,
}

impl std::fmt::Debug for StoragePorts {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StoragePorts")
            .finish_non_exhaustive()
    }
}

/// The handles a port future owns while it runs on the bridge thread.
pub(crate) type Handles = StoragePorts;

impl StoragePorts {
    /// Wraps already-scoped handles.
    #[must_use]
    pub fn new(
        documents: Arc<dyn DocumentStore>,
        streams: Arc<dyn StreamStore>,
        secrets: Arc<dyn SecretStore>,
    ) -> Self {
        Self {
            documents,
            streams,
            secrets,
        }
    }

    /// Uses a scoped backend handle for documents and streams, and `secrets`
    /// for secret values.
    #[must_use]
    pub fn from_scoped(scoped: &ScopedStorage, secrets: Arc<dyn SecretStore>) -> Self {
        Self::new(
            Arc::clone(scoped.documents()),
            Arc::clone(scoped.streams()),
            secrets,
        )
    }

    /// A fresh, private in-memory backend: nothing survives the handles.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the scope cannot be opened.
    pub fn memory() -> Result<Self> {
        Self::memory_scoped(&MemoryStorage::new(), "local")
    }

    /// The `scope` of a shared in-memory backend.
    ///
    /// Each call attaches a fresh secret store: keep and clone the returned
    /// ports (as `MemoryProvider` does) rather than reopening a scope, or the
    /// credentials and cache key written through the first open are not seen.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the scope is invalid.
    pub fn memory_scoped(backend: &MemoryStorage, scope: &str) -> Result<Self> {
        let scope = Scope::new(scope).map_err(|source| Error::store("opening a scope", source))?;
        let scoped = backend
            .for_scope(&scope)
            .map_err(|source| Error::store("opening a scope", source))?;
        Ok(Self::from_scoped(&scoped, Arc::new(MemorySecrets::new())))
    }

    /// Asks the documents port for the collections declared by
    /// [`storage_needs`]. Idempotent.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the port refuses a collection.
    pub fn prepare(&self) -> Result<()> {
        let specs = storage_needs().needs.collections.to_vec();
        self.run("declaring the collections", |ports| async move {
            for spec in specs {
                let spec = to_core_collection_spec(&spec);
                ports.documents.ensure_collection(&spec).await?;
            }
            Ok(())
        })
    }

    /// Whether anything has been installed here, for a host asking whether a
    /// data directory already holds state.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the port fails.
    pub fn has_state(&self) -> Result<bool> {
        self.run("checking for stored state", |ports| async move {
            let servers = ports
                .documents
                .count(SERVERS, &tinystoragedrivers_core::Filter::default())
                .await
                .or_else(|error| {
                    // A collection never declared here holds nothing, and the
                    // probe must not create it; any other failure (an
                    // unavailable or refusing port) is not "no installs".
                    if error.kind() == tinystoragedrivers_core::ErrorKind::NotFound {
                        Ok(0)
                    } else {
                        Err(error)
                    }
                })?;
            let audited = ports.streams.len(AUDIT_STREAM).await?;
            Ok(servers > 0 || audited > 0)
        })
    }

    /// Runs one port future to completion and maps its failure to
    /// [`Error::Store`] under `action`.
    pub(crate) fn run<T, F>(
        &self,
        action: &'static str,
        work: impl FnOnce(Handles) -> F,
    ) -> Result<T>
    where
        T: Send + 'static,
        F: Future<Output = tinystoragedrivers_core::Result<T>> + Send + 'static,
    {
        let future = work(self.clone());
        bridge()
            .map_err(|source| Error::store(action, source))?
            .run(future)
            .map_err(|source| Error::store(action, source))?
            .map_err(|source| Error::store(action, source))
    }
}

/// Converts the canonical wire declaration into the in-process storage port
/// schema without changing its indexes, expiry, or search fields.
pub(super) fn to_core_collection_spec(spec: &BusCollectionSpec) -> CollectionSpec {
    CollectionSpec {
        name: spec.name.clone(),
        indexes: spec
            .indexes
            .iter()
            .map(|index| IndexSpec {
                name: index.name.clone(),
                fields: index.fields.clone(),
                unique: index.unique,
            })
            .collect(),
        ttl_field: spec.ttl_field.clone(),
        search: spec.search.as_ref().map(|search| SearchSpec {
            fields: search.fields.clone(),
        }),
    }
}

/// The one bridge thread every store in the process shares.
fn bridge() -> tinystoragedrivers_core::Result<&'static Blocking> {
    static BRIDGE: OnceLock<Blocking> = OnceLock::new();
    if let Some(bridge) = BRIDGE.get() {
        return Ok(bridge);
    }
    let started = Blocking::new()?;
    Ok(BRIDGE.get_or_init(|| started))
}
