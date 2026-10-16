//! The values the authorization flow passes around.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Error, Result};

// Defined in the contract crate so a host names the same types; re-exported
// here so every path through this module keeps resolving.
pub use tinymcp_bus::{AuthDetection, AuthKind};

/// The bookkeeping needed to mint a new access token without another sign-in.
///
/// Stored as JSON beside the access token under
/// [`OAUTH_BUNDLE_KEY`](super::OAUTH_BUNDLE_KEY). The access token itself is
/// the `Authorization` header value, so the ordinary connect path needs no
/// special case for an OAuth server. Public so a host keeping credentials in
/// its own store can read the bundle it holds — for an expiry it shows, or to
/// migrate one — without restating its shape. It holds a client secret and a
/// refresh token: never log or display it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OAuthBundle {
    /// The refresh token, when the server issued one.
    pub refresh_token: Option<String>,
    /// The registered client.
    pub client_id: String,
    /// The client secret, when there is one.
    pub client_secret: Option<String>,
    /// Where to refresh.
    pub token_endpoint: String,
    /// When the current access token expires, in Unix seconds. Best effort.
    pub expires_at: u64,
}

impl std::fmt::Debug for OAuthBundle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let redacted = |secret: &Option<String>| secret.as_ref().map(|_| "[redacted]");
        f.debug_struct("OAuthBundle")
            .field("refresh_token", &redacted(&self.refresh_token))
            .field("client_id", &self.client_id)
            .field("client_secret", &redacted(&self.client_secret))
            .field("token_endpoint", &self.token_endpoint)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// A parsed token-endpoint reply.
#[derive(Debug, Clone)]
pub(super) struct TokenResponse {
    /// The access token.
    pub(super) access_token: String,
    /// A rotated refresh token, when the server sent one.
    pub(super) refresh_token: Option<String>,
    /// How long the access token lasts, in seconds.
    pub(super) expires_in: Option<u64>,
}

impl TokenResponse {
    /// Reads a token-endpoint reply.
    ///
    /// Only the access token is required. A server that omits the refresh token
    /// or the lifetime is normal, and refusing its reply would break sign-in
    /// for it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::MalformedResponse`] when there is no access token,
    /// which is the one thing the flow cannot proceed without.
    pub(super) fn parse(body: &Value) -> Result<Self> {
        let access_token = body
            .get("access_token")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::malformed("token response has no access_token"))?
            .to_string();

        Ok(Self {
            access_token,
            refresh_token: body
                .get("refresh_token")
                .and_then(Value::as_str)
                .map(ToString::to_string),
            expires_in: body.get("expires_in").and_then(Value::as_u64),
        })
    }
}
