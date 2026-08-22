//! The values the authorization flow passes around.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Error, Result};

/// What a server wants before it will talk.
///
/// Drives which control a caller offers the user, and getting it wrong costs
/// them a sign-in that cannot work or a token field for a server that will
/// never accept one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AuthDetection {
    /// `none`, `token`, or `oauth`.
    pub kind: AuthKind,
    /// Where to send the user, for an OAuth challenge.
    #[serde(default)]
    pub authorization_endpoint: Option<String>,
    /// The grant types the authorization server listed, if it listed any.
    #[serde(default)]
    pub grant_types: Vec<String>,
}

impl AuthDetection {
    /// A server that wants nothing.
    #[must_use]
    pub fn open() -> Self {
        Self {
            kind: AuthKind::None,
            authorization_endpoint: None,
            grant_types: Vec::new(),
        }
    }

    /// A server that wants a static credential.
    #[must_use]
    pub fn static_token() -> Self {
        Self {
            kind: AuthKind::Token,
            authorization_endpoint: None,
            grant_types: Vec::new(),
        }
    }

    /// A server that wants a browser sign-in.
    #[must_use]
    pub fn oauth(authorization_endpoint: String, grant_types: Vec<String>) -> Self {
        Self {
            kind: AuthKind::Oauth,
            authorization_endpoint: Some(authorization_endpoint),
            grant_types,
        }
    }
}

/// The three kinds of thing a server can want.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AuthKind {
    /// The server answered without a challenge. Nothing to supply.
    None,
    /// A static bearer token or API key, which the user pastes.
    Token,
    /// A browser sign-in.
    Oauth,
}

impl AuthKind {
    /// The stable string this kind is transmitted as.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Token => "token",
            Self::Oauth => "oauth",
        }
    }
}

/// An authorization parked between the browser redirect out and back.
#[derive(Debug, Clone)]
pub(super) struct PendingAuthorization {
    /// Which install this authorization is for.
    pub(super) server_id: String,
    /// The PKCE verifier, sent with the code exchange.
    pub(super) code_verifier: String,
    /// The dynamically registered client.
    pub(super) client_id: String,
    /// The secret, when the server issued a confidential client.
    pub(super) client_secret: Option<String>,
    /// Where to exchange the code.
    pub(super) token_endpoint: String,
    /// The redirect the authorization was started with.
    pub(super) redirect_uri: String,
    /// When this was parked, in Unix seconds.
    ///
    /// Present so an authorization the user abandoned does not sit in memory
    /// holding a client secret forever.
    pub(super) started_at: u64,
}

/// The bookkeeping needed to mint a new access token without another sign-in.
///
/// Stored beside the access token under the reserved bundle key. The access
/// token itself is the `Authorization` header value, so the ordinary connect
/// path needs no special case for an OAuth server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct OAuthBundle {
    /// The refresh token, when the server issued one.
    pub(super) refresh_token: Option<String>,
    /// The registered client.
    pub(super) client_id: String,
    /// The client secret, when there is one.
    pub(super) client_secret: Option<String>,
    /// Where to refresh.
    pub(super) token_endpoint: String,
    /// When the current access token expires, in Unix seconds. Best effort.
    pub(super) expires_at: u64,
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
