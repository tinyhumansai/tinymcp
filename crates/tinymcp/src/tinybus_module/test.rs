//! Unit tests for the bus adapter.
//!
//! The assertion that earns its place here compares the members the service
//! actually serves against [`tinymcp_bus::names::METHODS`]. A member served but
//! not named in the contract is invisible to a host; one named but not served
//! is an unknown-method failure the first time a user reaches for it. Neither
//! shows up in a type check, and neither shows up until something is broken.
//!
//! The served list comes from the interface the macro generated, not from a
//! list restated here — a restatement would agree with itself no matter what
//! the code did.
//!
//! The *manifest* carries the same names a third time, because the macro needs
//! literals. Nothing safe can read those bytes back, so they are verified
//! end-to-end by `examples/verify_module.rs`, which loads the built library
//! through `TinyBus` and calls it.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use tinybus::service::Interface;

use super::config::ModuleConfig;
use super::service::McpService;
use tinymcp_bus::{McpClientConfig, McpServerConfig, names};

/// A service over nothing, for inspecting its interface.
fn service() -> McpService {
    McpService::new(&ModuleConfig::default()).expect("the service builds")
}

// ---------------------------------------------------------------------------
// The interface against the contract
// ---------------------------------------------------------------------------

#[test]
fn the_service_serves_exactly_the_members_the_contract_names() {
    // Order included: the contract lists them in dispatch order, and a member
    // that moved is worth noticing even though nothing depends on position.
    let served: Vec<String> = service()
        .members()
        .into_iter()
        .map(|member| member.as_str().to_string())
        .collect();

    assert_eq!(served, names::METHODS);
}

#[test]
fn the_service_claims_the_interface_the_contract_names() {
    assert_eq!(service().name().as_str(), names::INTERFACE);
}

#[test]
fn every_served_member_is_reachable_by_its_contract_constant() {
    // Spelled through the constants rather than as strings, so a rename in the
    // contract fails to compile here rather than failing at a host.
    let served = service().members();
    let has = |member: &str| served.iter().any(|found| found.as_str() == member);

    for member in [
        names::methods::REGISTRY_SEARCH,
        names::methods::INSTALL,
        names::methods::CONNECT,
        names::methods::TOOL_CALL,
        names::methods::OAUTH_BEGIN,
        names::methods::SETUP_INSTALL_AND_CONNECT,
        names::methods::STATIC_CALL_TOOL,
        names::methods::AUDIT_LIST_WRITES,
    ] {
        assert!(has(member), "{member} is not served");
    }
}

#[test]
fn the_authorization_member_keeps_its_capitalisation() {
    // Derived from the method name it would be `OauthBegin`, which is not what
    // the contract says. It carries an explicit name for that reason, and this
    // is what notices if that annotation is dropped.
    assert!(
        service()
            .members()
            .iter()
            .any(|member| member.as_str() == "OAuthBegin")
    );
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

#[test]
fn an_absent_configuration_decodes_to_a_working_default() {
    // The loader passes the empty object when a host supplies nothing.
    let config: ModuleConfig = serde_json::from_str("{}").expect("the empty object decodes");

    assert_eq!(config.data_dir, None);
    assert!(config.client.servers.is_empty());
    assert!(config.client.enabled);
}

#[test]
fn a_configuration_round_trips() {
    let config = ModuleConfig {
        data_dir: Some("/data/tinymcp".into()),
        client: McpClientConfig {
            servers: vec![McpServerConfig {
                name: "weather".into(),
                endpoint: "https://example.test/mcp".into(),
                ..McpServerConfig::default()
            }],
            ..McpClientConfig::default()
        },
    };

    let encoded = serde_json::to_value(&config).unwrap();
    let decoded: ModuleConfig = serde_json::from_value(encoded).unwrap();

    assert_eq!(decoded.data_dir, config.data_dir);
    assert_eq!(decoded.client.servers.len(), 1);
}

// ---------------------------------------------------------------------------
// Building the service
// ---------------------------------------------------------------------------

#[test]
fn a_service_builds_from_nothing() {
    assert!(service().static_servers().is_empty());
}

#[test]
fn a_service_with_no_data_directory_persists_nothing() {
    // The right shape for a host that only wants its statically declared
    // servers: there is nothing to persist, and creating files for it would
    // leave state nobody asked for.
    assert!(service().dynamic().installed_list().unwrap().is_empty());
}

#[test]
fn a_service_with_a_data_directory_creates_its_stores() {
    let directory = tempfile::tempdir().unwrap();

    let _service = McpService::new(&ModuleConfig {
        data_dir: Some(directory.path().to_path_buf()),
        client: McpClientConfig::default(),
    })
    .expect("the service builds");

    assert!(crate::Store::path_for(directory.path()).exists());
    assert!(crate::AuditStore::path_for(directory.path()).exists());
}

#[test]
fn a_service_registers_the_statically_declared_servers() {
    let service = McpService::new(&ModuleConfig {
        data_dir: None,
        client: McpClientConfig {
            servers: vec![McpServerConfig {
                name: "weather".into(),
                endpoint: "https://example.test/mcp".into(),
                ..McpServerConfig::default()
            }],
            ..McpClientConfig::default()
        },
    })
    .expect("the service builds");

    assert_eq!(service.static_servers().len(), 1);
    assert!(service.static_servers().get("weather").is_some());
}

#[test]
fn a_service_reopens_an_existing_store() {
    let directory = tempfile::tempdir().unwrap();

    let first = McpService::new(&ModuleConfig {
        data_dir: Some(directory.path().to_path_buf()),
        client: McpClientConfig::default(),
    })
    .unwrap();
    first
        .audit()
        .record(&tinymcp_bus::NewMcpWriteRecord {
            timestamp_ms: 1_000,
            client_info: "claude".into(),
            tool_name: "memory_write".into(),
            args_summary: serde_json::json!(null),
            resulting_chunk_id: None,
            success: true,
            error_message: None,
        })
        .unwrap();
    drop(first);

    let second = McpService::new(&ModuleConfig {
        data_dir: Some(directory.path().to_path_buf()),
        client: McpClientConfig::default(),
    })
    .unwrap();

    assert_eq!(
        second
            .audit()
            .list(&tinymcp_bus::McpWriteListQuery::default())
            .unwrap()
            .len(),
        1
    );
}

// ---------------------------------------------------------------------------
// Every member, through the dispatch a host actually uses
// ---------------------------------------------------------------------------
//
// Called through `Interface::call` rather than as methods. That is what a host
// reaches: it exercises the generated match arm and the positional argument
// decode as well as the body, and those are the two halves most able to drift
// without a type error — `TinyBus` decodes arguments from a JSON array by
// position, so the *order* of a method's parameters is part of the contract.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::extract::State;
use axum::http::Uri;
use axum::routing::get;
use serde_json::{Value, json};
use tinybus::name::MemberName;

use tinymcp_bus::{McpRegistryAuthConfig, McpWriteListQuery, NewMcpWriteRecord};

/// Binds a loopback port and serves `app`, returning its base URL.
async fn serve(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

/// One server envelope, in the shape the official registry sends.
fn envelope() -> Value {
    json!({
        "server": {
            "name": "@acme/weather",
            "description": "forecasts",
            "packages": [{
                "registryType": "npm",
                "identifier": "@acme/weather",
                "environmentVariables": [{ "name": "API_KEY", "isSecret": true }],
            }],
        },
    })
}

/// A registry listing one installable server.
///
/// The members that browse a catalog would otherwise reach the real registry,
/// which no test may do. This stands in for it.
async fn catalog() -> (String, Arc<AtomicUsize>) {
    let hits = Arc::new(AtomicUsize::new(0));

    let app = Router::new()
        .route(
            "/v0/servers",
            get(|State(hits): State<Arc<AtomicUsize>>| async move {
                hits.fetch_add(1, Ordering::SeqCst);
                axum::Json(json!({ "servers": [envelope()] }))
            }),
        )
        .route(
            "/v0/servers/{*rest}",
            get(
                |State(hits): State<Arc<AtomicUsize>>, _uri: Uri| async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    axum::Json(json!({ "servers": [envelope()] }))
                },
            ),
        )
        .with_state(Arc::clone(&hits));

    (serve(app).await, hits)
}

/// A service over a temporary workspace, browsing `catalog_base`.
fn service_at(directory: &std::path::Path, catalog_base: &str) -> McpService {
    McpService::new(&ModuleConfig {
        data_dir: Some(directory.to_path_buf()),
        client: McpClientConfig {
            registry_auth: McpRegistryAuthConfig {
                mcp_official_base: Some(catalog_base.to_string()),
                ..McpRegistryAuthConfig::default()
            },
            ..McpClientConfig::default()
        },
    })
    .expect("the service builds")
}

/// Calls one member with positional arguments.
async fn call(service: &McpService, member: &str, args: Value) -> tinybus::Result<Value> {
    let member: MemberName = member.try_into().expect("a valid member name");
    service.call(&member, args).await
}

/// Calls one member and unwraps its reply.
async fn ok(service: &McpService, member: &str, args: Value) -> Value {
    call(service, member, args)
        .await
        .unwrap_or_else(|error| panic!("{member} failed: {error}"))
}

// ---------------------------------------------------------------------------
// Browsing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn registry_search_returns_a_page() {
    let directory = tempfile::tempdir().unwrap();
    let (base, _hits) = catalog().await;
    let service = service_at(directory.path(), &base);

    let page = ok(
        &service,
        names::methods::REGISTRY_SEARCH,
        json!([null, 1, 20]),
    )
    .await;

    assert!(
        page["servers"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty())
    );
}

#[tokio::test]
async fn registry_search_defaults_its_page_and_size() {
    // All three arguments are optional on the wire. A caller that sends nulls
    // must get the first page rather than page zero.
    let directory = tempfile::tempdir().unwrap();
    let (base, _hits) = catalog().await;
    let service = service_at(directory.path(), &base);

    let page = ok(
        &service,
        names::methods::REGISTRY_SEARCH,
        json!([null, null, null]),
    )
    .await;

    assert_eq!(page["page"], json!(1));
}

#[tokio::test]
async fn registry_get_returns_the_server_and_the_keys_it_needs() {
    // The key list is what a host turns into a form. A detail without it would
    // leave the user guessing what to paste.
    let directory = tempfile::tempdir().unwrap();
    let (base, _hits) = catalog().await;
    let service = service_at(directory.path(), &base);

    let detail = ok(
        &service,
        names::methods::REGISTRY_GET,
        json!(["@acme/weather"]),
    )
    .await;

    assert_eq!(detail["server"]["qualified_name"], json!("@acme/weather"));
    assert_eq!(detail["required_env_keys"], json!(["API_KEY"]));
}

#[tokio::test]
async fn config_assist_gathers_the_same_detail() {
    // Running the model turn is the host's; this member only gathers what the
    // turn needs.
    let directory = tempfile::tempdir().unwrap();
    let (base, _hits) = catalog().await;
    let service = service_at(directory.path(), &base);

    let detail = ok(
        &service,
        names::methods::CONFIG_ASSIST,
        json!(["@acme/weather"]),
    )
    .await;

    assert_eq!(detail["required_env_keys"], json!(["API_KEY"]));
}

// ---------------------------------------------------------------------------
// Registry settings
// ---------------------------------------------------------------------------

#[tokio::test]
async fn registry_settings_report_which_credentials_are_set_and_never_their_values() {
    // A getter that echoed a stored secret would put it in whatever a caller
    // does with a settings response: a form, a log, a diagnostic bundle.
    let directory = tempfile::tempdir().unwrap();
    let service = service_at(directory.path(), "http://127.0.0.1:1");

    ok(
        &service,
        names::methods::REGISTRY_SETTINGS_SET,
        json!([
            "sk-secret-value",
            "https://registry.test",
            "tok-secret-value"
        ]),
    )
    .await;
    let settings = ok(&service, names::methods::REGISTRY_SETTINGS_GET, json!([])).await;

    assert_eq!(settings["smithery_api_key_set"], json!(true));
    assert_eq!(settings["mcp_official_token_set"], json!(true));
    // The base is not a secret, and a user who cannot see which registry they
    // are pointed at cannot debug it.
    assert_eq!(
        settings["mcp_official_base"],
        json!("https://registry.test")
    );

    let rendered = settings.to_string();
    assert!(!rendered.contains("sk-secret-value"), "{rendered}");
    assert!(!rendered.contains("tok-secret-value"), "{rendered}");
}

#[tokio::test]
async fn an_absent_setting_leaves_the_stored_value_and_a_blank_one_clears_it() {
    let directory = tempfile::tempdir().unwrap();
    let service = service_at(directory.path(), "http://127.0.0.1:1");

    ok(
        &service,
        names::methods::REGISTRY_SETTINGS_SET,
        json!(["sk-test", null, null]),
    )
    .await;
    // `null` leaves it alone.
    let kept = ok(
        &service,
        names::methods::REGISTRY_SETTINGS_SET,
        json!([null, null, null]),
    )
    .await;
    assert_eq!(kept["smithery_api_key_set"], json!(true));

    // A blank string clears it.
    let cleared = ok(
        &service,
        names::methods::REGISTRY_SETTINGS_SET,
        json!(["   ", null, null]),
    )
    .await;
    assert_eq!(cleared["smithery_api_key_set"], json!(false));
}

// ---------------------------------------------------------------------------
// Installs
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_install_is_listed_then_uninstalled() {
    let directory = tempfile::tempdir().unwrap();
    let (base, _hits) = catalog().await;
    let service = service_at(directory.path(), &base);

    let outcome = ok(
        &service,
        names::methods::INSTALL,
        json!(["@acme/weather", { "API_KEY": "sekrit" }, null]),
    )
    .await;
    let server_id = outcome["server"]["server_id"]
        .as_str()
        .expect("an install reports its identifier")
        .to_string();

    let installed = ok(&service, names::methods::INSTALLED_LIST, json!([])).await;
    assert_eq!(installed.as_array().map(Vec::len), Some(1));

    let removed = ok(&service, names::methods::UNINSTALL, json!([server_id])).await;
    assert_eq!(removed, json!(true));
    assert_eq!(
        ok(&service, names::methods::INSTALLED_LIST, json!([]))
            .await
            .as_array()
            .map(Vec::len),
        Some(0)
    );
}

#[tokio::test]
async fn uninstalling_something_that_was_never_installed_reports_false() {
    // Not an error: the caller's intent — that this server not be installed —
    // already holds.
    let directory = tempfile::tempdir().unwrap();
    let service = service_at(directory.path(), "http://127.0.0.1:1");

    assert_eq!(
        ok(&service, names::methods::UNINSTALL, json!(["nothing"])).await,
        json!(false)
    );
}

#[tokio::test]
async fn a_credential_is_never_echoed_back_by_the_install_that_stored_it() {
    let directory = tempfile::tempdir().unwrap();
    let (base, _hits) = catalog().await;
    let service = service_at(directory.path(), &base);

    let outcome = ok(
        &service,
        names::methods::INSTALL,
        json!(["@acme/weather", { "API_KEY": "sekrit-value" }, null]),
    )
    .await;

    let rendered = outcome.to_string();
    assert!(!rendered.contains("sekrit-value"), "{rendered}");
    // The *name* is reported, so a caller can show what is configured.
    assert!(rendered.contains("API_KEY"), "{rendered}");
}

#[tokio::test]
async fn set_enabled_and_update_env_reach_an_installed_server() {
    let directory = tempfile::tempdir().unwrap();
    let (base, _hits) = catalog().await;
    let service = service_at(directory.path(), &base);

    let outcome = ok(
        &service,
        names::methods::INSTALL,
        json!(["@acme/weather", {}, null]),
    )
    .await;
    let server_id = outcome["server"]["server_id"].as_str().unwrap().to_string();

    ok(
        &service,
        names::methods::SET_ENABLED,
        json!([server_id, false]),
    )
    .await;

    // Turned off, so updating credentials persists without reconnecting.
    let updated = ok(
        &service,
        names::methods::UPDATE_ENV,
        json!([server_id, { "API_KEY": "new" }]),
    )
    .await;
    assert_eq!(updated["status"], json!("disabled"));
}

// ---------------------------------------------------------------------------
// Connections
// ---------------------------------------------------------------------------

#[tokio::test]
async fn connecting_an_unknown_server_fails_rather_than_reporting_success() {
    let directory = tempfile::tempdir().unwrap();
    let service = service_at(directory.path(), "http://127.0.0.1:1");

    let error = call(&service, names::methods::CONNECT, json!(["nothing"]))
        .await
        .expect_err("no such install");

    assert!(error.to_string().contains("nothing"), "{error}");
}

#[tokio::test]
async fn disconnecting_something_that_is_not_connected_reports_false() {
    let directory = tempfile::tempdir().unwrap();
    let service = service_at(directory.path(), "http://127.0.0.1:1");

    assert_eq!(
        ok(&service, names::methods::DISCONNECT, json!(["nothing"])).await,
        json!(false)
    );
}

#[tokio::test]
async fn status_is_empty_before_anything_is_installed() {
    let directory = tempfile::tempdir().unwrap();
    let service = service_at(directory.path(), "http://127.0.0.1:1");

    assert_eq!(
        ok(&service, names::methods::STATUS, json!([])).await,
        json!([])
    );
}

#[tokio::test]
async fn listing_tools_on_a_server_that_is_not_connected_says_so() {
    // Distinct from an unknown server, because the two ask different things of
    // a caller: connect the one you have, or install the one you do not.
    let directory = tempfile::tempdir().unwrap();
    let service = service_at(directory.path(), "http://127.0.0.1:1");

    let error = call(&service, names::methods::LIST_TOOLS, json!(["srv-1"]))
        .await
        .expect_err("not connected");

    assert!(error.to_string().contains("not connected"), "{error}");
}

#[tokio::test]
async fn calling_a_tool_on_a_server_that_is_not_connected_says_so() {
    let directory = tempfile::tempdir().unwrap();
    let service = service_at(directory.path(), "http://127.0.0.1:1");

    let error = call(
        &service,
        names::methods::TOOL_CALL,
        json!(["srv-1", "forecast", {}]),
    )
    .await
    .expect_err("not connected");

    assert!(error.to_string().contains("not connected"), "{error}");
}

// ---------------------------------------------------------------------------
// Authorization
// ---------------------------------------------------------------------------

#[tokio::test]
async fn detecting_auth_on_an_unknown_server_fails() {
    let directory = tempfile::tempdir().unwrap();
    let service = service_at(directory.path(), "http://127.0.0.1:1");

    assert!(
        call(&service, names::methods::DETECT_AUTH, json!(["nothing"]))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn beginning_a_sign_in_for_an_unknown_server_fails() {
    let directory = tempfile::tempdir().unwrap();
    let service = service_at(directory.path(), "http://127.0.0.1:1");

    assert!(
        call(
            &service,
            names::methods::OAUTH_BEGIN,
            json!(["nothing", "http://127.0.0.1:7788/callback"]),
        )
        .await
        .is_err()
    );
}

// ---------------------------------------------------------------------------
// The guided setup flow
// ---------------------------------------------------------------------------

#[tokio::test]
async fn setup_search_and_get_answer_the_same_shapes_as_browsing() {
    // They are separate members because the setup agent's surface is scoped
    // separately, not because they answer differently.
    let directory = tempfile::tempdir().unwrap();
    let (base, _hits) = catalog().await;
    let service = service_at(directory.path(), &base);

    let page = ok(&service, names::methods::SETUP_SEARCH, json!([null, 1, 20])).await;
    assert!(
        page["servers"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty())
    );

    let detail = ok(
        &service,
        names::methods::SETUP_GET,
        json!(["@acme/weather"]),
    )
    .await;
    assert_eq!(detail["required_env_keys"], json!(["API_KEY"]));
}

#[tokio::test]
async fn a_secret_is_requested_as_a_handle_and_submitted_against_it() {
    // The value never crosses back: the requester gets an opaque handle, and
    // the caller that holds the value submits it against that handle.
    let directory = tempfile::tempdir().unwrap();
    let service = service_at(directory.path(), "http://127.0.0.1:1");

    let handle = ok(
        &service,
        names::methods::SETUP_REQUEST_SECRET,
        json!(["API_KEY"]),
    )
    .await;
    let handle = handle.as_str().expect("a handle").to_string();
    assert!(handle.starts_with("secret://"), "{handle}");

    let accepted = ok(
        &service,
        names::methods::SETUP_SUBMIT_SECRET,
        json!([handle, "sekrit"]),
    )
    .await;
    assert_eq!(accepted, json!(true));
}

#[tokio::test]
async fn submitting_against_an_unknown_handle_is_refused() {
    let directory = tempfile::tempdir().unwrap();
    let service = service_at(directory.path(), "http://127.0.0.1:1");

    let accepted = ok(
        &service,
        names::methods::SETUP_SUBMIT_SECRET,
        json!(["secret://deadbeef", "sekrit"]),
    )
    .await;

    assert_eq!(accepted, json!(false));
}

#[tokio::test]
async fn a_secrets_map_holding_something_that_is_not_a_handle_is_refused() {
    // The map carries handles, never values. One that is not a handle is most
    // likely a caller about to send the secret itself.
    let directory = tempfile::tempdir().unwrap();
    let service = service_at(directory.path(), "http://127.0.0.1:1");

    let error = call(
        &service,
        names::methods::SETUP_TEST_CONNECTION,
        json!(["@acme/weather", { "API_KEY": "plainly-the-value" }]),
    )
    .await
    .expect_err("not a handle");

    assert!(error.to_string().contains("not a secret handle"), "{error}");
}

#[tokio::test]
async fn installing_and_connecting_with_a_bad_handle_is_refused_before_anything_is_stored() {
    let directory = tempfile::tempdir().unwrap();
    let (base, _hits) = catalog().await;
    let service = service_at(directory.path(), &base);

    let error = call(
        &service,
        names::methods::SETUP_INSTALL_AND_CONNECT,
        json!(["@acme/weather", { "API_KEY": "plainly-the-value" }, null]),
    )
    .await
    .expect_err("not a handle");
    assert!(error.to_string().contains("not a secret handle"), "{error}");

    // Nothing was installed on the way to that refusal.
    assert_eq!(
        ok(&service, names::methods::INSTALLED_LIST, json!([]))
            .await
            .as_array()
            .map(Vec::len),
        Some(0)
    );
}

// ---------------------------------------------------------------------------
// The statically declared servers
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_static_list_names_what_the_host_declared() {
    let service = McpService::new(&ModuleConfig {
        data_dir: None,
        client: McpClientConfig {
            servers: vec![McpServerConfig {
                name: "weather".into(),
                endpoint: "https://example.test/mcp".into(),
                ..McpServerConfig::default()
            }],
            ..McpClientConfig::default()
        },
    })
    .unwrap();

    assert_eq!(
        ok(&service, names::methods::STATIC_LIST, json!([])).await,
        json!(["weather"])
    );
}

#[tokio::test]
async fn a_static_server_the_host_never_declared_is_unknown() {
    let directory = tempfile::tempdir().unwrap();
    let service = service_at(directory.path(), "http://127.0.0.1:1");

    let error = call(
        &service,
        names::methods::STATIC_LIST_TOOLS,
        json!(["nothing"]),
    )
    .await
    .expect_err("never declared");

    assert!(error.to_string().contains("nothing"), "{error}");
}

#[tokio::test]
async fn calling_a_tool_on_a_static_server_the_host_never_declared_is_unknown() {
    let directory = tempfile::tempdir().unwrap();
    let service = service_at(directory.path(), "http://127.0.0.1:1");

    assert!(
        call(
            &service,
            names::methods::STATIC_CALL_TOOL,
            json!(["nothing", "forecast", {}]),
        )
        .await
        .is_err()
    );
}

// ---------------------------------------------------------------------------
// The write-audit log
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_recorded_write_comes_back_in_the_listing() {
    let directory = tempfile::tempdir().unwrap();
    let service = service_at(directory.path(), "http://127.0.0.1:1");

    let record = serde_json::to_value(NewMcpWriteRecord {
        timestamp_ms: 1_700_000_000_000,
        client_info: "claude".into(),
        tool_name: "memory_write".into(),
        args_summary: json!({ "title": "a note" }),
        resulting_chunk_id: Some("chunk-1".into()),
        success: true,
        error_message: None,
    })
    .unwrap();

    let id = ok(
        &service,
        names::methods::AUDIT_RECORD_WRITE,
        json!([record]),
    )
    .await;
    assert!(id.as_i64().is_some_and(|id| id > 0), "{id}");

    let query = serde_json::to_value(McpWriteListQuery::default()).unwrap();
    let rows = ok(&service, names::methods::AUDIT_LIST_WRITES, json!([query])).await;

    assert_eq!(rows.as_array().map(Vec::len), Some(1));
    assert_eq!(rows[0]["tool_name"], json!("memory_write"));
}

// ---------------------------------------------------------------------------
// Dispatch itself
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_member_the_interface_does_not_serve_is_reported_as_unknown() {
    let directory = tempfile::tempdir().unwrap();
    let service = service_at(directory.path(), "http://127.0.0.1:1");

    let error = call(&service, "NoSuchMember", json!([]))
        .await
        .expect_err("not a member");

    assert!(error.to_string().contains("NoSuchMember"), "{error}");
}

#[tokio::test]
async fn a_member_called_with_the_wrong_argument_shape_is_refused() {
    // Arguments decode from a JSON array by position, so this is the failure a
    // caller sees when it sends the wrong count or the wrong types.
    let directory = tempfile::tempdir().unwrap();
    let service = service_at(directory.path(), "http://127.0.0.1:1");

    assert!(
        call(&service, names::methods::UNINSTALL, json!([42]))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn every_member_the_contract_names_is_dispatchable() {
    // The complement of the interface test above: that one asserts the *names*
    // agree, this one asserts each name actually reaches an arm. A member in
    // the list with no arm would answer "unknown method" to the first host that
    // called it.
    let directory = tempfile::tempdir().unwrap();
    let service = service_at(directory.path(), "http://127.0.0.1:1");

    for member in names::METHODS {
        let error = call(&service, member, json!([]))
            .await
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default();

        assert!(
            !error.contains("unknown method"),
            "{member} has no dispatch arm"
        );
    }
}

// ---------------------------------------------------------------------------
// Serving the interface on a real bus
// ---------------------------------------------------------------------------
//
// `setup` is what a host's module loader calls. It is short, but everything it
// does is a place the module can fail to come up: an object path that does not
// parse, a name the broker refuses, a service that cannot be built. A failure
// here fails the load on purpose — a module that came up without its store
// would answer every call with the same error.

use tinybus::Connection;
use tinybus::broker::Broker;
use tinybus::transport::memory::MemoryBus;

/// A connection to a broker running in this process.
async fn in_process_connection() -> Connection {
    let bus = MemoryBus::new();
    Broker::new().spawn(bus.clone());
    Connection::connect(bus.connect().await.expect("a transport"))
        .await
        .expect("a connection")
}

#[tokio::test]
async fn setting_up_serves_the_interface_and_claims_its_name() {
    let directory = tempfile::tempdir().unwrap();
    let connection = in_process_connection().await;

    super::setup(
        connection,
        ModuleConfig {
            data_dir: Some(directory.path().to_path_buf()),
            client: McpClientConfig::default(),
        },
    )
    .await
    .expect("the module comes up");
}

#[tokio::test]
async fn a_module_that_came_up_answers_a_call_on_its_object_path() {
    // The end-to-end shape a host sees: it calls a member at the contract's
    // object path and gets a reply, without knowing anything about this crate.
    let directory = tempfile::tempdir().unwrap();
    let bus = MemoryBus::new();
    Broker::new().spawn(bus.clone());

    let serving = Connection::connect(bus.connect().await.unwrap())
        .await
        .unwrap();
    // Kept alive for the length of the test: the name and the served object
    // belong to the connection, and a module's loader holds it for the module's
    // life. Dropping it here would release both before the call.
    let _serving = serving.clone();
    super::setup(
        serving,
        ModuleConfig {
            data_dir: Some(directory.path().to_path_buf()),
            client: McpClientConfig::default(),
        },
    )
    .await
    .expect("the module comes up");

    let caller = Connection::connect(bus.connect().await.unwrap())
        .await
        .unwrap();

    let installed: serde_json::Value = caller
        .call(
            names::INTERFACE.try_into().expect("a bus name"),
            names::OBJECT_PATH.try_into().expect("an object path"),
            names::INTERFACE.try_into().expect("an interface name"),
            names::methods::INSTALLED_LIST
                .try_into()
                .expect("a member name"),
            serde_json::json!([]),
        )
        .await
        .expect("the call reaches the module");

    assert_eq!(installed, serde_json::json!([]));
}

#[tokio::test]
async fn a_module_that_cannot_open_its_store_fails_to_come_up() {
    // Rather than coming up and answering every call with the same error: a
    // load failure says so once, at the moment a host can still react.
    let directory = tempfile::tempdir().unwrap();
    let occupied = directory.path().join("mcp_clients");
    std::fs::create_dir_all(occupied.join("mcp_clients.db")).unwrap();

    let connection = in_process_connection().await;

    let error = super::setup(
        connection,
        ModuleConfig {
            data_dir: Some(directory.path().to_path_buf()),
            client: McpClientConfig::default(),
        },
    )
    .await
    .expect_err("the store cannot be opened");

    assert!(
        error.to_string().contains("tinymcp could not start"),
        "{error}"
    );
}
