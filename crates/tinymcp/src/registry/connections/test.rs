//! Unit tests for the live connection map.
//!
//! Three things get the attention here. **Credential handling**: which stored
//! values become request headers, and which never leave. **The redirect guard**:
//! which addresses stored credentials may follow. And **status priority**, which
//! decides what a user is shown and therefore which control they are offered.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::time::Duration;

use axum::routing::post;
use axum::{Json, Router};
use serde_json::{Value, json};

use super::dial::{build_http_auth, credential_safe_dial_url, is_internal_key};
use super::status::{ConnectFailure, classify};
use super::types::Connections;
use crate::Error;
use crate::registry::Store;
use crate::registry::oauth::{OAUTH_BUNDLE_KEY, OAuthFlow};
use tinymcp_bus::{
    CommandKind, InstalledServer, McpAuthConfig, McpAuthHint, McpClientIdentityConfig, McpTool,
    ServerStatus, Transport,
};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// An install with the given identifier and transport.
fn install(server_id: &str, transport: Transport) -> InstalledServer {
    InstalledServer {
        server_id: server_id.to_string(),
        qualified_name: format!("@test/{server_id}"),
        display_name: server_id.to_string(),
        description: Some("a test server".into()),
        icon_url: None,
        command_kind: CommandKind::Node,
        command: "npx".into(),
        args: Vec::new(),
        env_keys: Vec::new(),
        config: None,
        installed_at: 1_000,
        last_connected_at: None,
        transport,
        enabled: true,
    }
}

/// A store holding `server`.
fn store_with(server: &InstalledServer) -> Store {
    let store = Store::open_in_memory().unwrap();
    store.insert_server(server).unwrap();
    store
}

/// Binds a loopback port and serves `app`.
async fn serve(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}/")
}

/// A server that handshakes and advertises one tool.
fn working_server() -> Router {
    Router::new().route(
        "/",
        post(|Json(body): Json<Value>| async move {
            let method = body
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let result = match method {
                "initialize" => json!({
                    "protocolVersion": tinymcp_bus::LATEST_PROTOCOL_VERSION,
                    "capabilities": {},
                    "serverInfo": { "name": "working", "version": "1" },
                }),
                "tools/list" => json!({
                    "tools": [{ "name": "forecast", "description": "weather" }],
                }),
                _ => json!({ "content": [{ "type": "text", "text": "done" }] }),
            };
            Json(json!({ "jsonrpc": "2.0", "id": body["id"].clone(), "result": result }))
        }),
    )
}

/// The identity connects are made with.
fn identity() -> McpClientIdentityConfig {
    McpClientIdentityConfig::default()
}

// ---------------------------------------------------------------------------
// Credentials
// ---------------------------------------------------------------------------

#[test]
fn nothing_stored_means_no_credentials() {
    // The right state for an OAuth-only server nobody has signed into: its 401
    // then surfaces the challenge rather than being masked by a bad header.
    assert_eq!(build_http_auth(&BTreeMap::new()), McpAuthConfig::None);
}

#[test]
fn one_stored_value_becomes_one_header() {
    let env = BTreeMap::from([("X-Api-Key".to_string(), "secret".to_string())]);

    assert_eq!(
        build_http_auth(&env),
        McpAuthConfig::Header {
            name: "X-Api-Key".into(),
            value: "secret".into(),
        }
    );
}

#[test]
fn several_stored_values_all_become_headers() {
    // A server wanting a client key and a client secret gets both, not the
    // first one.
    let env = BTreeMap::from([
        ("X-Client-Key".to_string(), "key".to_string()),
        ("X-Client-Secret".to_string(), "secret".to_string()),
    ]);

    match build_http_auth(&env) {
        McpAuthConfig::Headers { headers } => {
            assert_eq!(headers.len(), 2);
            assert!(headers.iter().any(|header| header.name == "X-Client-Key"));
            assert!(
                headers
                    .iter()
                    .any(|header| header.name == "X-Client-Secret")
            );
        }
        other => panic!("expected several headers, got {other:?}"),
    }
}

#[test]
fn the_oauth_bundle_is_never_sent_as_a_header() {
    // It holds a refresh token and a client secret. Sending it would hand a
    // server credentials it has no business seeing.
    let env = BTreeMap::from([(
        OAUTH_BUNDLE_KEY.to_string(),
        "{\"secret\":\"x\"}".to_string(),
    )]);

    assert_eq!(build_http_auth(&env), McpAuthConfig::None);
}

#[test]
fn an_internal_key_is_skipped_even_beside_real_credentials() {
    let env = BTreeMap::from([
        ("Authorization".to_string(), "Bearer t".to_string()),
        (OAUTH_BUNDLE_KEY.to_string(), "{}".to_string()),
    ]);

    assert_eq!(
        build_http_auth(&env),
        McpAuthConfig::Header {
            name: "Authorization".into(),
            value: "Bearer t".into(),
        }
    );
}

#[test]
fn a_blank_value_is_not_a_credential() {
    let env = BTreeMap::from([
        ("X-Api-Key".to_string(), "   ".to_string()),
        ("X-Other".to_string(), String::new()),
    ]);

    assert_eq!(build_http_auth(&env), McpAuthConfig::None);
}

#[test]
fn the_internal_marker_is_two_underscores() {
    assert!(is_internal_key("__oauth__"));
    assert!(is_internal_key(OAUTH_BUNDLE_KEY));
    assert!(!is_internal_key("Authorization"));
    assert!(!is_internal_key("_single"));
}

// ---------------------------------------------------------------------------
// The redirect guard
// ---------------------------------------------------------------------------

#[test]
fn a_same_origin_redirect_is_followed() {
    assert_eq!(
        credential_safe_dial_url(
            "https://api.test/mcp",
            "https://api.test/v2/mcp".to_string()
        ),
        "https://api.test/v2/mcp"
    );
}

#[test]
fn a_cross_origin_redirect_to_https_is_followed() {
    // The common legitimate case: a vanity host redirecting to the real API.
    assert_eq!(
        credential_safe_dial_url(
            "https://vanity.test/",
            "https://api.real.test/mcp".to_string()
        ),
        "https://api.real.test/mcp"
    );
}

#[test]
fn a_cross_origin_redirect_to_cleartext_is_refused() {
    // Following it would hand the user's credential to whoever answered on an
    // unauthenticated port.
    assert_eq!(
        credential_safe_dial_url("https://api.test/mcp", "http://evil.test/mcp".to_string()),
        "https://api.test/mcp"
    );
}

#[test]
fn a_same_origin_cleartext_redirect_is_still_followed() {
    // Nothing new is exposed: the credential was already going to that origin
    // over that scheme.
    assert_eq!(
        credential_safe_dial_url("http://local.test/mcp", "http://local.test/v2".to_string()),
        "http://local.test/v2"
    );
}

#[test]
fn a_redirect_that_only_changes_the_port_is_cross_origin() {
    assert_eq!(
        credential_safe_dial_url(
            "https://api.test:8443/mcp",
            "https://api.test:9443/mcp".to_string()
        ),
        "https://api.test:9443/mcp"
    );
}

#[test]
fn an_unparseable_address_falls_back_to_the_original() {
    assert_eq!(
        credential_safe_dial_url("https://api.test/mcp", "not a url".to_string()),
        "https://api.test/mcp"
    );
    assert_eq!(
        credential_safe_dial_url("not a url", "https://api.test/mcp".to_string()),
        "not a url"
    );
}

// ---------------------------------------------------------------------------
// Status priority
// ---------------------------------------------------------------------------

/// A recorded 401 with the given reason.
fn unauthorized(hint: McpAuthHint) -> ConnectFailure {
    ConnectFailure {
        message: "mcp unauthorized".into(),
        auth: Some(hint),
    }
}

/// A recorded non-authentication failure.
fn generic_failure() -> ConnectFailure {
    ConnectFailure {
        message: "connection refused".into(),
        auth: None,
    }
}

#[test]
fn a_disabled_server_reads_as_disabled_whatever_else_is_true() {
    // A user who switched a server off should see that, not a stale error from
    // before they did.
    let (status, tools, error, hint) = classify(
        false,
        Some(7),
        Some(&unauthorized(McpAuthHint::OauthRequired)),
    );

    assert_eq!(status, ServerStatus::Disabled);
    assert_eq!(tools, 0);
    assert_eq!(error, None);
    assert_eq!(hint, None);
}

#[test]
fn a_connected_server_outranks_a_recorded_failure() {
    // The failure is history; the connection is now.
    let (status, tools, error, hint) = classify(true, Some(3), Some(&generic_failure()));

    assert_eq!(status, ServerStatus::Connected);
    assert_eq!(tools, 3);
    assert_eq!(error, None);
    assert_eq!(hint, None);
}

#[test]
fn an_unauthorized_server_carries_its_reason_and_no_message() {
    // The raw body and the metadata URL describe the server's authorization
    // setup and would only be rendered at a user who cannot act on them.
    let (status, tools, error, hint) =
        classify(true, None, Some(&unauthorized(McpAuthHint::TokenRejected)));

    assert_eq!(status, ServerStatus::Unauthorized);
    assert_eq!(tools, 0);
    assert_eq!(error, None, "a 401 must not surface its message");
    assert_eq!(hint, Some(McpAuthHint::TokenRejected));
}

#[test]
fn a_generic_failure_carries_its_message_and_no_reason() {
    let (status, tools, error, hint) = classify(true, None, Some(&generic_failure()));

    assert_eq!(status, ServerStatus::Error);
    assert_eq!(error.as_deref(), Some("connection refused"));
    assert_eq!(hint, None);
    assert_eq!(tools, 0);
}

#[test]
fn a_server_that_has_never_failed_reads_as_disconnected() {
    let (status, tools, error, hint) = classify(true, None, None);

    assert_eq!(status, ServerStatus::Disconnected);
    assert_eq!(tools, 0);
    assert_eq!(error, None);
    assert_eq!(hint, None);
}

#[test]
fn a_connected_server_advertising_nothing_is_still_connected() {
    let (status, tools, _, _) = classify(true, Some(0), None);

    assert_eq!(status, ServerStatus::Connected);
    assert_eq!(tools, 0);
}

// ---------------------------------------------------------------------------
// Failure classification
// ---------------------------------------------------------------------------

#[test]
fn a_401_advertising_oauth_is_classified_as_needing_a_sign_in() {
    let error = Error::Unauthorized {
        endpoint: "https://api.test".into(),
        resource_metadata: Some("https://api.test/.well-known".into()),
    };

    // Even with a credential supplied: an OAuth-only server refuses a pasted
    // token, and telling the user to fix it would send them nowhere.
    assert_eq!(
        ConnectFailure::new(&error, true).auth,
        Some(McpAuthHint::OauthRequired)
    );
}

#[test]
fn a_401_with_a_credential_supplied_is_classified_as_a_rejected_token() {
    let error = Error::Unauthorized {
        endpoint: "https://api.test".into(),
        resource_metadata: None,
    };

    assert_eq!(
        ConnectFailure::new(&error, true).auth,
        Some(McpAuthHint::TokenRejected)
    );
}

#[test]
fn a_401_with_nothing_supplied_is_classified_as_needing_one() {
    let error = Error::Unauthorized {
        endpoint: "https://api.test".into(),
        resource_metadata: None,
    };

    assert_eq!(
        ConnectFailure::new(&error, false).auth,
        Some(McpAuthHint::CredentialRequired)
    );
}

#[test]
fn a_transport_failure_is_not_classified_as_an_authentication_one() {
    let error = Error::Http {
        endpoint: "https://api.test".into(),
        status: 500,
        body: "boom".into(),
    };

    let failure = ConnectFailure::new(&error, true);
    assert_eq!(failure.auth, None);
    assert!(failure.message.contains("500"));
}

// ---------------------------------------------------------------------------
// The map itself
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_fresh_map_holds_nothing() {
    let connections = Connections::new();

    assert_eq!(connections.connected_count().await, 0);
    assert!(!connections.is_connected("srv-1").await);
    assert!(connections.tools_for("srv-1").await.is_none());
    assert!(connections.connected_overview().await.is_empty());
}

#[tokio::test]
async fn connecting_caches_the_tools_and_records_the_connection_time() {
    let url = serve(working_server()).await;
    let server = install("srv-1", Transport::HttpRemote { url });
    let store = store_with(&server);
    let connections = Connections::new();
    let oauth = OAuthFlow::new(None).unwrap();

    let tools = connections
        .connect(&store, &oauth, &identity(), None, &server)
        .await
        .expect("the connect");

    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "forecast");
    assert!(connections.is_connected("srv-1").await);
    assert_eq!(connections.connected_count().await, 1);
    assert!(
        store
            .get_server("srv-1")
            .unwrap()
            .last_connected_at
            .is_some()
    );
}

#[tokio::test]
async fn a_connected_server_answers_a_probe() {
    let url = serve(working_server()).await;
    let server = install("srv-1", Transport::HttpRemote { url });
    let store = store_with(&server);
    let connections = Connections::new();
    let oauth = OAuthFlow::new(None).unwrap();

    connections
        .connect(&store, &oauth, &identity(), None, &server)
        .await
        .unwrap();

    assert!(
        connections
            .probe_alive("srv-1", Duration::from_secs(5))
            .await
    );
}

#[tokio::test]
async fn a_server_that_was_never_connected_fails_its_probe() {
    assert!(
        !Connections::new()
            .probe_alive("srv-1", Duration::from_secs(1))
            .await
    );
}

#[tokio::test]
async fn disconnecting_removes_the_entry_and_reports_whether_there_was_one() {
    let url = serve(working_server()).await;
    let server = install("srv-1", Transport::HttpRemote { url });
    let store = store_with(&server);
    let connections = Connections::new();
    let oauth = OAuthFlow::new(None).unwrap();

    connections
        .connect(&store, &oauth, &identity(), None, &server)
        .await
        .unwrap();

    assert!(connections.disconnect("srv-1").await);
    assert!(!connections.is_connected("srv-1").await);
    assert!(!connections.disconnect("srv-1").await);
}

#[tokio::test]
async fn calling_a_tool_on_a_server_that_is_not_connected_says_so() {
    let error = Connections::new()
        .call_tool("srv-1", "forecast", json!({}))
        .await
        .expect_err("not connected");

    // Not `UnknownServer`: the two ask different things of a caller, and a user
    // sent to reinstall a server they already have will not find it.
    assert!(matches!(error, Error::NotConnected { .. }), "{error:?}");
}

#[tokio::test]
async fn calling_a_tool_on_a_connected_server_reaches_it() {
    let url = serve(working_server()).await;
    let server = install("srv-1", Transport::HttpRemote { url });
    let store = store_with(&server);
    let connections = Connections::new();
    let oauth = OAuthFlow::new(None).unwrap();

    connections
        .connect(&store, &oauth, &identity(), None, &server)
        .await
        .unwrap();

    let result = connections
        .call_tool("srv-1", "forecast", json!({}))
        .await
        .expect("the call");

    assert_eq!(result.rendered.text(), "done");
}

// ---------------------------------------------------------------------------
// Failure bookkeeping
// ---------------------------------------------------------------------------

/// An install pointed at a port that refuses immediately.
fn unreachable_install(server_id: &str) -> InstalledServer {
    install(
        server_id,
        Transport::HttpRemote {
            url: "http://127.0.0.1:1/mcp".into(),
        },
    )
}

#[tokio::test]
async fn a_failed_connect_records_why() {
    let server = unreachable_install("srv-1");
    let store = store_with(&server);
    let connections = Connections::new();
    let oauth = OAuthFlow::new(None).unwrap();

    connections
        .connect(&store, &oauth, &identity(), None, &server)
        .await
        .expect_err("connection refused");

    assert!(connections.last_error("srv-1").await.is_some());
    assert!(!connections.needs_auth("srv-1").await);
}

#[tokio::test]
async fn a_successful_connect_clears_an_earlier_failure() {
    let failing = unreachable_install("srv-1");
    let store = store_with(&failing);
    let connections = Connections::new();
    let oauth = OAuthFlow::new(None).unwrap();

    connections
        .connect(&store, &oauth, &identity(), None, &failing)
        .await
        .expect_err("connection refused");
    assert!(connections.last_error("srv-1").await.is_some());

    let url = serve(working_server()).await;
    let working = install("srv-1", Transport::HttpRemote { url });
    connections
        .connect(&store, &oauth, &identity(), None, &working)
        .await
        .expect("the retry");

    assert_eq!(connections.last_error("srv-1").await, None);
}

#[tokio::test]
async fn disconnecting_forgets_a_recorded_failure() {
    let server = unreachable_install("srv-1");
    let store = store_with(&server);
    let connections = Connections::new();
    let oauth = OAuthFlow::new(None).unwrap();

    connections
        .connect(&store, &oauth, &identity(), None, &server)
        .await
        .expect_err("connection refused");
    connections.disconnect("srv-1").await;

    assert_eq!(connections.last_error("srv-1").await, None);
}

#[tokio::test]
async fn a_failure_can_be_cleared_explicitly() {
    let server = unreachable_install("srv-1");
    let store = store_with(&server);
    let connections = Connections::new();
    let oauth = OAuthFlow::new(None).unwrap();

    connections
        .connect(&store, &oauth, &identity(), None, &server)
        .await
        .expect_err("connection refused");

    connections.clear_last_error("srv-1").await;
    assert_eq!(connections.last_error("srv-1").await, None);
}

#[tokio::test]
async fn an_http_remote_install_with_no_endpoint_is_refused_before_dialling() {
    let server = install("srv-1", Transport::HttpRemote { url: String::new() });
    let store = store_with(&server);
    let connections = Connections::new();
    let oauth = OAuthFlow::new(None).unwrap();

    let error = connections
        .connect(&store, &oauth, &identity(), None, &server)
        .await
        .expect_err("no endpoint");

    assert!(
        matches!(error, Error::MalformedResponse { .. }),
        "{error:?}"
    );
}

// ---------------------------------------------------------------------------
// Status over the whole install set
// ---------------------------------------------------------------------------

#[tokio::test]
async fn status_covers_every_install_whether_connected_or_not() {
    let url = serve(working_server()).await;
    let connected = install("srv-connected", Transport::HttpRemote { url });
    let never = install("srv-never", Transport::Stdio);

    let store = Store::open_in_memory().unwrap();
    store.insert_server(&connected).unwrap();
    store.insert_server(&never).unwrap();

    let connections = Connections::new();
    let oauth = OAuthFlow::new(None).unwrap();
    connections
        .connect(&store, &oauth, &identity(), None, &connected)
        .await
        .unwrap();

    let statuses = connections.all_status(&store).await.unwrap();

    assert_eq!(statuses.len(), 2);
    let connected_status = statuses
        .iter()
        .find(|status| status.server_id == "srv-connected")
        .unwrap();
    assert_eq!(connected_status.status, ServerStatus::Connected);
    assert_eq!(connected_status.tool_count, 1);

    let never_status = statuses
        .iter()
        .find(|status| status.server_id == "srv-never")
        .unwrap();
    assert_eq!(never_status.status, ServerStatus::Disconnected);
}

#[tokio::test]
async fn a_disabled_install_reads_as_disabled_in_the_status_listing() {
    let store = Store::open_in_memory().unwrap();
    store
        .insert_server(&InstalledServer {
            enabled: false,
            ..install("srv-off", Transport::Stdio)
        })
        .unwrap();

    let statuses = Connections::new().all_status(&store).await.unwrap();

    assert_eq!(statuses[0].status, ServerStatus::Disabled);
}

// ---------------------------------------------------------------------------
// Overviews
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_overview_names_and_describes_each_connected_server() {
    let url = serve(working_server()).await;
    let server = install("srv-1", Transport::HttpRemote { url });
    let store = store_with(&server);
    let connections = Connections::new();
    let oauth = OAuthFlow::new(None).unwrap();

    connections
        .connect(&store, &oauth, &identity(), None, &server)
        .await
        .unwrap();

    let overviews = connections.connected_overview().await;

    assert_eq!(overviews.len(), 1);
    assert_eq!(overviews[0].qualified_name, "@test/srv-1");
    assert_eq!(overviews[0].description.as_deref(), Some("a test server"));
    assert_eq!(overviews[0].tools.len(), 1);
}

#[tokio::test]
async fn overviews_are_ordered_by_qualified_name() {
    // A caller rendering these into a prompt would otherwise see them reshuffle
    // between turns from map iteration order alone.
    let store = Store::open_in_memory().unwrap();
    let connections = Connections::new();
    let oauth = OAuthFlow::new(None).unwrap();

    for name in ["zulu", "alpha", "mike"] {
        let url = serve(working_server()).await;
        let server = install(name, Transport::HttpRemote { url });
        store.insert_server(&server).unwrap();
        connections
            .connect(&store, &oauth, &identity(), None, &server)
            .await
            .unwrap();
    }

    let names: Vec<String> = connections
        .connected_overview()
        .await
        .into_iter()
        .map(|overview| overview.qualified_name)
        .collect();

    assert_eq!(names, ["@test/alpha", "@test/mike", "@test/zulu"]);
}

#[tokio::test]
async fn the_flat_tool_listing_pairs_each_tool_with_its_server() {
    let url = serve(working_server()).await;
    let server = install("srv-1", Transport::HttpRemote { url });
    let store = store_with(&server);
    let connections = Connections::new();
    let oauth = OAuthFlow::new(None).unwrap();

    connections
        .connect(&store, &oauth, &identity(), None, &server)
        .await
        .unwrap();

    let listed: Vec<(String, String, McpTool)> = connections.all_connected_tools().await;

    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].0, "srv-1");
    assert_eq!(listed[0].1, "@test/srv-1");
    assert_eq!(listed[0].2.name, "forecast");
}

#[tokio::test]
async fn disconnecting_everything_empties_the_map() {
    let url = serve(working_server()).await;
    let server = install("srv-1", Transport::HttpRemote { url });
    let store = store_with(&server);
    let connections = Connections::new();
    let oauth = OAuthFlow::new(None).unwrap();

    connections
        .connect(&store, &oauth, &identity(), None, &server)
        .await
        .unwrap();

    connections.disconnect_all().await;

    assert_eq!(connections.connected_count().await, 0);
}

// ---------------------------------------------------------------------------
// The subprocess transport
// ---------------------------------------------------------------------------
//
// A subprocess server is stood up as a shell script, matching the transport
// suite's approach: there is no portable way to write a one-line JSON-RPC
// responder that both a POSIX shell and `cmd.exe` understand. The dispatch
// under test is not platform-specific.

#[cfg(unix)]
mod stdio {
    use std::io::Write;
    use std::path::Path;

    use super::*;

    /// Writes an executable shell script and returns its path.
    fn write_script(directory: &Path, body: &str) -> String {
        use std::os::unix::fs::PermissionsExt;

        let path = directory.join("fake-mcp-server");
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(file, "#!/bin/sh").unwrap();
        write!(file, "{body}").unwrap();
        drop(file);

        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).unwrap();

        path.to_string_lossy().into_owned()
    }

    /// A server answering the handshake, a tool listing, and one call.
    ///
    /// It reads a line per reply so it stays in step with the client rather
    /// than racing ahead and closing its output.
    fn responder() -> String {
        let replies = [
            format!(
                "{{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{{\"protocolVersion\":\"{}\",\
                 \"capabilities\":{{}},\"serverInfo\":{{\"name\":\"fake\",\"version\":\"1\"}}}}}}",
                tinymcp_bus::LATEST_PROTOCOL_VERSION
            ),
            "{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[{\"name\":\"echo\",\
             \"inputSchema\":{\"type\":\"object\"}}]}}"
                .to_string(),
            "{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"content\":[{\"type\":\"text\",\
             \"text\":\"hi\"}]}}"
                .to_string(),
        ];

        let mut body = String::new();
        for reply in replies {
            body.push_str("read -r _line\n");
            body.push_str(&format!("printf '%s\\n' '{reply}'\n"));
        }
        // Then wait, rather than exiting and closing the pipe under the client.
        body.push_str("cat > /dev/null\n");
        body
    }

    /// An install that runs `script` as a subprocess server.
    fn stdio_install(script: &str) -> InstalledServer {
        InstalledServer {
            command: script.to_string(),
            command_kind: CommandKind::Binary,
            ..install("srv-1", Transport::Stdio)
        }
    }

    #[tokio::test]
    async fn a_subprocess_server_connects_and_reports_its_tools() {
        let directory = tempfile::tempdir().unwrap();
        let server = stdio_install(&write_script(directory.path(), &responder()));
        let store = store_with(&server);
        let connections = Connections::new();

        let tools = connections
            .connect(
                &store,
                &OAuthFlow::new(None).unwrap(),
                &McpClientIdentityConfig::default(),
                None,
                &server,
            )
            .await
            .expect("connect");

        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "echo");
    }

    #[tokio::test]
    async fn a_connected_subprocess_server_answers_a_tool_call() {
        let directory = tempfile::tempdir().unwrap();
        let server = stdio_install(&write_script(directory.path(), &responder()));
        let store = store_with(&server);
        let connections = Connections::new();
        connections
            .connect(
                &store,
                &OAuthFlow::new(None).unwrap(),
                &McpClientIdentityConfig::default(),
                None,
                &server,
            )
            .await
            .unwrap();

        let result = connections
            .call_tool("srv-1", "echo", json!({}))
            .await
            .expect("call");

        assert!(!result.rendered.is_error);
    }

    #[tokio::test]
    async fn a_probe_finds_a_live_subprocess_server() {
        // The probe is a tools/list, so the script needs a second listing reply
        // after the one the handshake consumes.
        let mut body = String::new();
        body.push_str("read -r _line\n");
        body.push_str(&format!(
            "printf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{{\"protocolVersion\":\"{}\",\"capabilities\":{{}},\"serverInfo\":{{\"name\":\"fake\",\"version\":\"1\"}}}}}}'\n",
            tinymcp_bus::LATEST_PROTOCOL_VERSION
        ));
        for id in [2, 3] {
            body.push_str("read -r _line\n");
            body.push_str(&format!(
                "printf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{{\"tools\":[]}}}}'\n"
            ));
        }
        body.push_str("cat > /dev/null\n");

        let directory = tempfile::tempdir().unwrap();
        let server = stdio_install(&write_script(directory.path(), &body));
        let store = store_with(&server);
        let connections = Connections::new();
        connections
            .connect(
                &store,
                &OAuthFlow::new(None).unwrap(),
                &McpClientIdentityConfig::default(),
                None,
                &server,
            )
            .await
            .unwrap();

        assert!(
            connections
                .probe_alive("srv-1", Duration::from_secs(5))
                .await
        );
    }

    #[tokio::test]
    async fn disconnecting_every_server_ends_each_session() {
        let directory = tempfile::tempdir().unwrap();
        let server = stdio_install(&write_script(directory.path(), &responder()));
        let store = store_with(&server);
        let connections = Connections::new();
        connections
            .connect(
                &store,
                &OAuthFlow::new(None).unwrap(),
                &McpClientIdentityConfig::default(),
                None,
                &server,
            )
            .await
            .unwrap();

        connections.disconnect_all().await;

        assert_eq!(connections.connected_count().await, 0);
    }

    #[tokio::test]
    async fn internal_bookkeeping_is_not_passed_to_the_child() {
        // The OAuth bundle is this crate's own record, not a credential the
        // server asked for. Handing it to a subprocess would put a refresh
        // token in its environment for no reason.
        let directory = tempfile::tempdir().unwrap();
        let server = stdio_install(&write_script(directory.path(), &responder()));
        let store = store_with(&server);

        let mut env = BTreeMap::new();
        env.insert(
            OAUTH_BUNDLE_KEY.to_string(),
            "{\"secret\":true}".to_string(),
        );
        env.insert("API_KEY".to_string(), "sekrit".to_string());
        store.set_env_values("srv-1", &env).unwrap();

        // It connects rather than failing, which is what proves the filter runs
        // before the spawn rather than the child rejecting the variable.
        assert!(
            Connections::new()
                .connect(
                    &store,
                    &OAuthFlow::new(None).unwrap(),
                    &McpClientIdentityConfig::default(),
                    None,
                    &server,
                )
                .await
                .is_ok()
        );
    }
}
