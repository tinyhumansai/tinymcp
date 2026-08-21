//! Storing and refreshing OAuth tokens.

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

use super::types::{OAuthBundle, TokenResponse};
use crate::error::{Error, Result};
use crate::registry::Store;

/// The reserved credential key holding the refresh bundle.
///
/// The two leading underscores are the marker meaning "internal": the connect
/// path skips such keys when building request headers, so this is never sent to
/// a server, and a credential listing hides them from the user.
pub const OAUTH_BUNDLE_KEY: &str = "__oauth__";

/// The credential key the access token is stored under.
///
/// It is the header name on purpose. Storing the token as an ordinary
/// `Authorization` value means the connect path needs no special case for an
/// OAuth server — it builds headers from stored credentials either way.
pub(super) const ACCESS_TOKEN_KEY: &str = "Authorization";

/// How long before expiry a token is treated as already expired.
///
/// A token that expires during the request it was attached to is a 401 the user
/// sees for no reason.
const REFRESH_SKEW_SECS: u64 = 60;

/// The lifetime assumed when a server does not state one.
const DEFAULT_TOKEN_LIFETIME_SECS: u64 = 3600;

/// Mints a new access token when the stored one has expired, or is about to.
///
/// Returns whether a refresh happened. Does nothing — and reports `false` — for
/// a server with no bundle, a bundle with no refresh token, or a token that is
/// still good. A server with no refresh token simply gets a 401 on its next
/// call, which prompts the user to sign in again.
///
/// Called from the connect path so a session never opens with a stale token.
///
/// # Errors
///
/// Returns [`Error::MalformedResponse`] when the stored bundle cannot be read
/// or the token endpoint answers with something unusable, [`Error::Http`] when
/// it answers with a failure status, and [`Error::Transport`] when it cannot be
/// reached.
pub async fn refresh_if_expired(
    store: &Store,
    http: &reqwest::Client,
    server_id: &str,
) -> Result<bool> {
    let env = store.load_env_values(server_id)?;
    let Some(raw_bundle) = env.get(OAUTH_BUNDLE_KEY) else {
        return Ok(false);
    };

    let bundle: OAuthBundle = serde_json::from_str(raw_bundle)
        .map_err(|error| Error::malformed(format!("stored oauth bundle is unreadable: {error}")))?;

    if bundle.expires_at > now_unix() + REFRESH_SKEW_SECS {
        return Ok(false);
    }

    let Some(refresh_token) = bundle
        .refresh_token
        .as_deref()
        .filter(|token| !token.trim().is_empty())
    else {
        return Ok(false);
    };

    let mut form: Vec<(&str, &str)> = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", bundle.client_id.as_str()),
    ];
    if let Some(secret) = bundle.client_secret.as_deref() {
        form.push(("client_secret", secret));
    }

    let body = post_form(http, &bundle.token_endpoint, &form).await?;
    let mut tokens = TokenResponse::parse(&body)?;

    // Some servers do not rotate the refresh token. Keeping the existing one is
    // what stops a refresh from being the last one possible.
    if tokens.refresh_token.is_none() {
        tokens.refresh_token.clone_from(&bundle.refresh_token);
    }

    persist(
        store,
        server_id,
        &bundle.client_id,
        bundle.client_secret.as_deref(),
        &bundle.token_endpoint,
        &tokens,
    )?;

    tracing::info!(server_id, "refreshed an expired access token");
    Ok(true)
}

/// Stores an access token and its refresh bundle.
///
/// Merged over the existing credentials rather than replacing them. Storing is
/// replace-all, so starting from an empty map would silently erase any custom
/// header the user configured alongside their sign-in.
///
/// # Errors
///
/// Returns [`Error::Serialization`] when the bundle cannot be encoded, and
/// [`Error::Store`] when it cannot be written.
pub(super) fn persist(
    store: &Store,
    server_id: &str,
    client_id: &str,
    client_secret: Option<&str>,
    token_endpoint: &str,
    tokens: &TokenResponse,
) -> Result<()> {
    let bundle = OAuthBundle {
        refresh_token: tokens.refresh_token.clone(),
        client_id: client_id.to_string(),
        client_secret: client_secret.map(ToString::to_string),
        token_endpoint: token_endpoint.to_string(),
        expires_at: now_unix() + tokens.expires_in.unwrap_or(DEFAULT_TOKEN_LIFETIME_SECS),
    };

    let mut env = store.load_env_values(server_id)?;
    env.insert(
        ACCESS_TOKEN_KEY.to_string(),
        format!("Bearer {}", tokens.access_token),
    );
    env.insert(OAUTH_BUNDLE_KEY.to_string(), serde_json::to_string(&bundle)?);

    store.set_env_values(server_id, &env)?;

    // The listing of credential *names* lives on the server row and is what a
    // caller shows the user, so it has to learn about the two new keys.
    let names: Vec<String> = env.keys().cloned().collect();
    store.update_env_keys(server_id, &names)?;

    Ok(())
}

/// Posts a form to a token endpoint and returns its JSON body.
///
/// # Errors
///
/// Returns [`Error::Transport`] when the endpoint cannot be reached,
/// [`Error::Http`] when it answers with a failure status, and
/// [`Error::MalformedResponse`] when the body is not JSON.
pub(super) async fn post_form(
    http: &reqwest::Client,
    endpoint: &str,
    form: &[(&str, &str)],
) -> Result<Value> {
    let response = http
        .post(endpoint)
        .form(form)
        .send()
        .await
        .map_err(|error| Error::transport(endpoint, error))?;

    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|error| Error::transport(endpoint, error))?;

    // The body is read before the status is judged, because a token endpoint's
    // failure body is where it says *why* — `invalid_grant`, `invalid_client`.
    // Discarding it would leave a caller with a bare status code.
    if !status.is_success() {
        return Err(Error::Http {
            endpoint: crate::redact_endpoint(endpoint),
            status: status.as_u16(),
            body: text,
        });
    }

    serde_json::from_str(&text)
        .map_err(|error| Error::malformed(format!("token endpoint replied with non-json: {error}")))
}

/// The current time in Unix seconds.
///
/// A clock before the epoch reads as zero, which makes every token look expired
/// and triggers a refresh. That is the safe direction to be wrong in.
pub(super) fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}
