//! Where a sign-in waits between the browser redirect out and back.
//!
//! [`OAuthFlow::begin`] parks the PKCE verifier and the registered client
//! under the authorization's `state`; [`OAuthFlow::complete`] takes them back
//! when the redirect returns. By default they wait in this process's memory
//! ([`MemoryPendingStore`]), which is right for a desktop host: the redirect
//! comes back to the same process that started it.
//!
//! A host running several instances behind one callback URL cannot rely on
//! that. The redirect lands on whichever instance the load balancer picks, so
//! the pending authorization has to live somewhere they all see — a shared
//! database, say. Such a host implements [`PendingAuthorizationStore`] and
//! hands it to [`OAuthFlow::with_pending_store`].
//!
//! The store keeps entries for at least [`PENDING_TTL_SECS`]; the flow itself
//! refuses any entry older than that, so a store that expires lazily (or
//! never) still cannot complete a stale sign-in.
//!
//! [`OAuthFlow::begin`]: super::OAuthFlow::begin
//! [`OAuthFlow::complete`]: super::OAuthFlow::complete
//! [`OAuthFlow::with_pending_store`]: super::OAuthFlow::with_pending_store

use std::collections::HashMap;

use futures_util::future::BoxFuture;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::error::Result;

/// How long an unfinished authorization stays parked, in seconds.
///
/// A user who starts a sign-in and closes the tab leaves an entry behind
/// holding a PKCE verifier and perhaps a client secret. Ten minutes is longer
/// than any real sign-in and short enough that abandoned ones do not
/// accumulate. A host store expiring entries itself (a database TTL index)
/// uses this as its lifetime.
pub const PENDING_TTL_SECS: u64 = 10 * 60;

/// An authorization parked between the browser redirect out and back.
///
/// Public so a host can keep it in its own [`PendingAuthorizationStore`]. It
/// holds the PKCE verifier and, for a confidential client, the client secret:
/// a host stores it sealed and never logs or displays it. Its `Debug` output
/// redacts both.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingAuthorization {
    /// Which install this authorization is for: the host's opaque server id.
    pub server_id: String,
    /// The PKCE verifier, sent with the code exchange.
    pub code_verifier: String,
    /// The dynamically registered client.
    pub client_id: String,
    /// The secret, when the server issued a confidential client.
    pub client_secret: Option<String>,
    /// Where to exchange the code.
    pub token_endpoint: String,
    /// The redirect the authorization was started with.
    pub redirect_uri: String,
    /// When this was parked, in Unix seconds.
    pub started_at: u64,
}

impl PendingAuthorization {
    /// Whether this authorization is older than [`PENDING_TTL_SECS`] at `now`
    /// (Unix seconds).
    #[must_use]
    pub fn expired_at(&self, now: u64) -> bool {
        self.started_at < now.saturating_sub(PENDING_TTL_SECS)
    }
}

impl std::fmt::Debug for PendingAuthorization {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingAuthorization")
            .field("server_id", &self.server_id)
            .field("code_verifier", &"[redacted]")
            .field("client_id", &self.client_id)
            .field(
                "client_secret",
                &self.client_secret.as_ref().map(|_| "[redacted]"),
            )
            .field("token_endpoint", &self.token_endpoint)
            .field("redirect_uri", &self.redirect_uri)
            .field("started_at", &self.started_at)
            .finish()
    }
}

/// Where [`OAuthFlow`](super::OAuthFlow) parks authorizations between the
/// redirect out and back.
///
/// `state` is the random value the flow put in the authorize URL. It is a
/// bearer secret for the duration of the sign-in: a host persisting it may key
/// entries on a digest of it instead.
///
/// Object-safe, so a flow holds any store behind one pointer.
pub trait PendingAuthorizationStore: Send + Sync + std::fmt::Debug {
    /// Parks `pending` under `state`, replacing any entry already there.
    ///
    /// # Errors
    ///
    /// Whatever the store reports when it cannot be written.
    fn park<'a>(
        &'a self,
        state: &'a str,
        pending: PendingAuthorization,
    ) -> BoxFuture<'a, Result<()>>;

    /// Removes and returns the entry parked under `state`, or `None` when there
    /// is none.
    ///
    /// Must be atomic: when two callers take the same `state` at once, at most
    /// one receives the entry. That is what stops one authorization code from
    /// being exchanged twice.
    ///
    /// # Errors
    ///
    /// Whatever the store reports when it cannot be read or written.
    fn take<'a>(&'a self, state: &'a str) -> BoxFuture<'a, Result<Option<PendingAuthorization>>>;

    /// Returns the entry parked under `state` without removing it.
    ///
    /// # Errors
    ///
    /// Whatever the store reports when it cannot be read.
    fn peek<'a>(&'a self, state: &'a str) -> BoxFuture<'a, Result<Option<PendingAuthorization>>>;

    /// How many entries are parked, expired ones included unless the store has
    /// already dropped them. For tests and diagnostics.
    ///
    /// # Errors
    ///
    /// Whatever the store reports when it cannot be read.
    fn count(&self) -> BoxFuture<'_, Result<usize>>;
}

/// The default store: pending authorizations in this process's memory.
///
/// Right when the redirect returns to the process that started the sign-in.
/// Expired entries are dropped whenever the store is touched. Its `Debug`
/// output shows only how many are parked: each key is a `state`, a bearer
/// secret while the sign-in is open.
#[derive(Default)]
pub struct MemoryPendingStore {
    entries: Mutex<HashMap<String, PendingAuthorization>>,
}

impl std::fmt::Debug for MemoryPendingStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryPendingStore")
            .field("parked", &self.entries.lock().len())
            .finish()
    }
}

impl MemoryPendingStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn pruned(&self) -> parking_lot::MutexGuard<'_, HashMap<String, PendingAuthorization>> {
        let mut entries = self.entries.lock();
        let now = super::tokens::now_unix();
        entries.retain(|_, pending| !pending.expired_at(now));
        entries
    }
}

impl PendingAuthorizationStore for MemoryPendingStore {
    fn park<'a>(
        &'a self,
        state: &'a str,
        pending: PendingAuthorization,
    ) -> BoxFuture<'a, Result<()>> {
        self.pruned().insert(state.to_string(), pending);
        Box::pin(std::future::ready(Ok(())))
    }

    fn take<'a>(&'a self, state: &'a str) -> BoxFuture<'a, Result<Option<PendingAuthorization>>> {
        let taken = self.pruned().remove(state);
        Box::pin(std::future::ready(Ok(taken)))
    }

    fn peek<'a>(&'a self, state: &'a str) -> BoxFuture<'a, Result<Option<PendingAuthorization>>> {
        let found = self.pruned().get(state).cloned();
        Box::pin(std::future::ready(Ok(found)))
    }

    fn count(&self) -> BoxFuture<'_, Result<usize>> {
        let count = self.pruned().len();
        Box::pin(std::future::ready(Ok(count)))
    }
}

#[cfg(test)]
#[path = "pending_tests.rs"]
mod tests;
