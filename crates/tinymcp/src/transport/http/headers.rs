//! Request and response header handling for the HTTP transport.
//!
//! Three jobs: reading a `WWW-Authenticate` challenge off a 401 so OAuth
//! discovery has somewhere to start, mirroring schema-tagged tool arguments
//! into `Mcp-Param-*` request headers, and applying a server's configured
//! credentials.

use base64::Engine;
use reqwest::RequestBuilder;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;
use std::collections::BTreeMap;

use crate::error::{Error, Result};
use tinymcp_bus::{McpAuthChallenge, McpAuthConfig, McpRemoteTool};

/// The header prefix a schema-tagged argument is mirrored into.
const MCP_PARAM_PREFIX: &str = "Mcp-Param-";

/// Reads a `WWW-Authenticate` challenge, or `None` if there is none to read.
///
/// The scheme is the first token; the rest is a comma-separated attribute list.
/// Only `realm` and `resource_metadata` are extracted, because those are the
/// two OAuth discovery needs.
pub(super) fn parse_www_authenticate_challenge(headers: &HeaderMap) -> Option<McpAuthChallenge> {
    let raw = headers.get("WWW-Authenticate")?.to_str().ok()?.trim();
    let mut parts = raw.splitn(2, ' ');
    let scheme = parts.next()?.trim().to_string();
    let attributes = parse_auth_attribute_list(parts.next().unwrap_or_default().trim());

    Some(McpAuthChallenge {
        scheme,
        realm: attributes.get("realm").cloned(),
        resource_metadata: attributes.get("resource_metadata").cloned(),
    })
}

/// Splits a challenge's `key=value, key="value"` attribute list.
///
/// A fragment with no `=` is skipped rather than treated as a valueless key —
/// these lists come from arbitrary servers and a malformed one should cost the
/// caller nothing more than the attribute it could not read.
pub(super) fn parse_auth_attribute_list(input: &str) -> BTreeMap<String, String> {
    let mut attributes = BTreeMap::new();
    for part in input.split(',') {
        let Some((key, value)) = part.split_once('=') else {
            continue;
        };
        attributes.insert(
            key.trim().to_string(),
            value.trim().trim_matches('"').to_string(),
        );
    }
    attributes
}

/// Reads one header as a `String`, or `None` if it is absent or not text.
pub(super) fn header_to_string(headers: &HeaderMap, name: &str) -> Option<String> {
    headers.get(name)?.to_str().ok().map(ToString::to_string)
}

/// Builds the `Mcp-Param-*` headers a tool's schema asks for.
///
/// A property tagged `x-mcp-header: Suffix` in the tool's input schema has its
/// argument value mirrored into an `Mcp-Param-Suffix` request header, in
/// addition to staying in the JSON-RPC arguments. Some servers route or
/// authorize on the header rather than the body.
///
/// A tagged property with no corresponding argument contributes no header.
///
/// # Errors
///
/// Returns [`Error::MalformedResponse`] when a tag produces a header name or a
/// value that cannot be encoded. This is deliberately fatal rather than
/// skipped: the server asked for the header, so sending the request without it
/// would be sending something the server did not ask for and letting it fail
/// somewhere less legible.
pub(super) fn mcp_param_headers_from_schema(
    tool: &McpRemoteTool,
    arguments: &Value,
) -> Result<Vec<(HeaderName, HeaderValue)>> {
    let mut headers = Vec::new();
    let Some(arguments) = arguments.as_object() else {
        return Ok(headers);
    };
    let Some(properties) = tool
        .input_schema
        .get("properties")
        .and_then(Value::as_object)
    else {
        return Ok(headers);
    };

    for (property, schema) in properties {
        let Some(suffix) = schema.get("x-mcp-header").and_then(Value::as_str) else {
            continue;
        };
        let Some(value) = arguments.get(property) else {
            continue;
        };

        let name = HeaderName::from_bytes(format!("{MCP_PARAM_PREFIX}{suffix}").as_bytes())
            .map_err(|error| {
                Error::malformed(format!(
                    "tool property `{property}` requested an unusable header name: {error}"
                ))
            })?;
        let value = match value {
            Value::String(text) => HeaderValue::from_str(text),
            other => HeaderValue::from_str(&other.to_string()),
        }
        .map_err(|error| {
            Error::malformed(format!(
                "tool property `{property}` produced an unusable header value: {error}"
            ))
        })?;

        headers.push((name, value));
    }

    Ok(headers)
}

/// Applies a server's configured credentials to a request.
///
/// A header whose name or value cannot be encoded is skipped rather than
/// treated as fatal. That asymmetry with [`mcp_param_headers_from_schema`] is
/// deliberate: these values come from the *user's own configuration*, and
/// failing the whole request over one unusable header would leave them unable
/// to reach a server that the remaining headers might well authenticate.
pub(super) fn apply_auth(request: RequestBuilder, auth: &McpAuthConfig) -> RequestBuilder {
    match auth {
        McpAuthConfig::BearerToken { token } => {
            request.header(AUTHORIZATION, format!("Bearer {}", token.trim()))
        }
        McpAuthConfig::Basic { username, password } => {
            let encoded =
                base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"));
            request.header(AUTHORIZATION, format!("Basic {encoded}"))
        }
        McpAuthConfig::Header { name, value } => apply_one_header(request, name, value),
        McpAuthConfig::Headers { headers } => headers.iter().fold(request, |request, header| {
            apply_one_header(request, &header.name, &header.value)
        }),
        McpAuthConfig::QueryParam { name, value } => {
            request.query(&[(name.as_str(), value.as_str())])
        }
        // `McpAuthConfig::None`, and — because the contract's auth enum is
        // `#[non_exhaustive]` — any variant a newer contract adds. Sending no
        // credential is the safe reading of "a credential this build does not
        // understand": the request fails with a 401 the caller can act on,
        // rather than with a header the server rejects for reasons nobody can
        // see.
        McpAuthConfig::None | _ => request,
    }
}

/// Adds one header, skipping it if either half cannot be encoded.
fn apply_one_header(request: RequestBuilder, name: &str, value: &str) -> RequestBuilder {
    if let (Ok(header), Ok(value)) = (HeaderName::try_from(name), HeaderValue::from_str(value)) {
        return request.header(header, value);
    }
    tracing::warn!(
        header = %name,
        "skipping a configured auth header that cannot be encoded"
    );
    request
}
