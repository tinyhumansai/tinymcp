//! Deciding how a server from a catalog will actually be run.

use serde_json::Value;

use crate::error::{Error, Result};
use tinymcp_bus::{CommandKind, RegistryConnection, RegistryServerDetail, Transport};

/// Normalizes a connection's declared type into the vocabulary installs use.
///
/// Catalogs say `http`; an install record says `http_remote`. Server-sent
/// events are a hosted endpoint too. Mapping them here means one spelling
/// downstream instead of three checks at every site.
#[must_use]
pub(super) fn transport_kind(connection: &RegistryConnection) -> &str {
    match connection.r#type.as_str() {
        "stdio" => "stdio",
        "http" | "http_remote" | "sse" => "http_remote",
        other => other,
    }
}

/// Chooses the connection an install should use.
///
/// # Hosted first
///
/// In order: a published hosted endpoint, any hosted endpoint, a published
/// subprocess package, any subprocess package.
///
/// Most catalog servers offer both. Preferring the subprocess means every
/// install has to find a runtime, resolve a package, and locate credentials on
/// the user's machine — three ways to fail before the server is even reached,
/// and they fail for most community packages. A hosted endpoint removes all
/// three and routes authentication to the server's own challenge, which is
/// something the user can act on.
///
/// The trade is real and deliberate: data goes to the hosted endpoint and a
/// network is required. It favours connecting over running locally.
#[must_use]
pub fn pick_connection(connections: &[RegistryConnection]) -> Option<&RegistryConnection> {
    let published_http = connections
        .iter()
        .find(|connection| transport_kind(connection) == "http_remote" && connection.published);
    if published_http.is_some() {
        return published_http;
    }

    let any_http = connections
        .iter()
        .find(|connection| transport_kind(connection) == "http_remote");
    if any_http.is_some() {
        return any_http;
    }

    let published_stdio = connections
        .iter()
        .find(|connection| transport_kind(connection) == "stdio" && connection.published);
    if published_stdio.is_some() {
        return published_stdio;
    }

    connections
        .iter()
        .find(|connection| transport_kind(connection) == "stdio")
}

/// The credential names an install will actually need.
///
/// Read from the connection [`pick_connection`] chose, not from every
/// connection the catalog listed. A server offering both a hosted endpoint and
/// a subprocess package must not demand the package's environment variables for
/// an install that connects over HTTP and never reads them — that is a form the
/// user cannot complete for a server that would have worked.
#[must_use]
pub fn collect_required_env_keys(detail: &RegistryServerDetail) -> Vec<String> {
    let Some(connection) = pick_connection(&detail.connections) else {
        return Vec::new();
    };

    let Some(properties) = connection
        .config_schema
        .as_ref()
        .and_then(|schema| schema.get("properties"))
        .and_then(Value::as_object)
    else {
        return Vec::new();
    };

    let mut names: Vec<String> = Vec::with_capacity(properties.len());
    for name in properties.keys() {
        if !names.contains(name) {
            names.push(name.clone());
        }
    }
    names
}

/// Works out how an install will be launched, from the chosen connection.
///
/// A hosted install carries its endpoint and no command; a subprocess install
/// carries a command and no endpoint.
///
/// # Errors
///
/// Returns [`Error::MalformedResponse`] when a hosted connection declares no
/// endpoint — there is nothing to dial, and installing it would produce a
/// record that fails on every connect with no way to fix it.
pub fn build_install_transport(
    qualified_name: &str,
    connection: &RegistryConnection,
) -> Result<(Transport, CommandKind, String, Vec<String>)> {
    if transport_kind(connection) == "http_remote" {
        let url = connection.deployment_url.clone().unwrap_or_default();
        if url.trim().is_empty() {
            return Err(Error::malformed(format!(
                "the hosted connection for `{qualified_name}` declares no endpoint"
            )));
        }

        return Ok((
            Transport::HttpRemote { url },
            // Unused for a hosted install; the record still needs a value.
            CommandKind::Node,
            String::new(),
            Vec::new(),
        ));
    }

    let (kind, command, args) = resolve_command(qualified_name, Some(connection));
    Ok((Transport::Stdio, kind, command, args))
}

/// Works out the command a subprocess install is launched with.
///
/// The catalog's worked example when it supplied one, and otherwise `npx -y`
/// against the qualified name — which is how both scoped and plain package
/// names launch.
#[must_use]
pub(super) fn resolve_command(
    qualified_name: &str,
    connection: Option<&RegistryConnection>,
) -> (CommandKind, String, Vec<String>) {
    let example = connection
        .and_then(|connection| connection.example_config.as_ref())
        .and_then(|example| {
            let command = example.get("command").and_then(Value::as_str)?;
            let args = example
                .get("args")
                .and_then(Value::as_array)
                .map(|args| {
                    args.iter()
                        .filter_map(Value::as_str)
                        .map(String::from)
                        .collect()
                })
                .unwrap_or_default();
            Some((command.to_string(), args))
        });

    if let Some((command, args)) = example {
        // The launcher names the ecosystem: `uvx` and `python` are the Python
        // ones, everything else is Node.
        let kind = if command.contains("uvx") || command.contains("python") {
            CommandKind::Python
        } else {
            CommandKind::Node
        };
        return (kind, command, args);
    }

    (
        CommandKind::Node,
        "npx".to_string(),
        vec!["-y".to_string(), qualified_name.to_string()],
    )
}
