//! Where the flow keeps what it mints.
//!
//! The flow needs three things from storage: the endpoint of the server being
//! signed in to, and read/write access to that server's credential map — the
//! `Authorization` value and the refresh bundle under [`OAUTH_BUNDLE_KEY`].
//! [`OAuthCredentialStore`] is exactly that, so a host can back sign-in and
//! refresh with its own secret store (a per-tenant vault, say) instead of the
//! `SQLite` [`Store`]. [`Store`] implements it, so existing callers are
//! unchanged.
//!
//! [`OAUTH_BUNDLE_KEY`]: super::OAUTH_BUNDLE_KEY

use std::collections::BTreeMap;
use std::future::Future;

use crate::error::Result;
use crate::registry::Store;
use tinymcp_bus::Transport;

/// The storage the OAuth flow and [`refresh_if_expired`] read and write.
///
/// `server_id` is the host's key for the server. It is opaque to the flow, so a
/// multi-tenant host can qualify it (`"<tenant>/<server>"`) and route each call
/// to the right tenant's secrets; [`OAuthFlow::pending_server`] hands it back
/// from a redirect's `state` before completing.
///
/// Credentials are a whole map, replaced on write. The flow always reads,
/// merges and writes back, so a value the user set beside their sign-in
/// survives it.
///
/// The futures are `Send` so the flow can run on a multi-threaded runtime.
///
/// [`refresh_if_expired`]: super::refresh_if_expired
/// [`OAuthFlow::pending_server`]: super::OAuthFlow::pending_server
pub trait OAuthCredentialStore: Send + Sync {
    /// The endpoint of an HTTP-remote server, or `None` when the server is not
    /// one (a subprocess has no HTTP authorization).
    ///
    /// # Errors
    ///
    /// Whatever the store reports when the server cannot be looked up.
    fn remote_url(&self, server_id: &str) -> impl Future<Output = Result<Option<String>>> + Send;

    /// Every credential stored for `server_id`; empty when there are none.
    ///
    /// # Errors
    ///
    /// Whatever the store reports when it cannot be read.
    fn load_credentials(
        &self,
        server_id: &str,
    ) -> impl Future<Output = Result<BTreeMap<String, String>>> + Send;

    /// Replaces every credential stored for `server_id` with `credentials`.
    ///
    /// # Errors
    ///
    /// Whatever the store reports when it cannot be written.
    fn store_credentials(
        &self,
        server_id: &str,
        credentials: &BTreeMap<String, String>,
    ) -> impl Future<Output = Result<()>> + Send;
}

impl OAuthCredentialStore for Store {
    async fn remote_url(&self, server_id: &str) -> Result<Option<String>> {
        let server = self.get_server(server_id)?;
        Ok(match server.transport {
            Transport::HttpRemote { url } if !url.is_empty() => Some(url),
            _ => None,
        })
    }

    async fn load_credentials(&self, server_id: &str) -> Result<BTreeMap<String, String>> {
        self.load_env_values(server_id)
    }

    async fn store_credentials(
        &self,
        server_id: &str,
        credentials: &BTreeMap<String, String>,
    ) -> Result<()> {
        self.set_env_values(server_id, credentials)?;
        // The listing of credential *names* lives on the server row and is
        // what a caller shows the user, so it has to learn about new keys.
        let names: Vec<String> = credentials.keys().cloned().collect();
        self.update_env_keys(server_id, &names)
    }
}
