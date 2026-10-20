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
use super::test_linked_module;
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
fn exported_manifest_declares_exactly_the_contract_members_in_order() {
    let module = test_linked_module().expect("the linked module manifest parses");
    let provided = module
        .manifest
        .provides
        .iter()
        .find(|provided| provided.version.interface.as_str() == names::INTERFACE)
        .expect("the manifest provides TinyMCP");
    let declared: Vec<_> = provided
        .methods
        .iter()
        .map(tinybus::MemberName::as_str)
        .collect();

    assert_eq!(declared, names::METHODS);
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
        names::methods::OAUTH_COMPLETE,
        names::methods::CONNECTED_OVERVIEW,
        names::methods::REGISTRY_SEARCH_CURATED,
        names::methods::OPEN,
        names::methods::SETUP_INSTALL_AND_CONNECT,
        names::methods::STATIC_CALL_TOOL,
        names::methods::STATIC_SERVER_SUMMARIES,
        names::methods::CONFIGURED_TOOL_CALL,
        names::methods::VALIDATE_SERVER_ARGUMENTS,
        names::methods::AUDIT_LIST_WRITES,
        names::methods::CONFIG_DOCUMENT_GET,
        names::methods::CONFIG_DOCUMENT_APPLY,
        names::methods::HAS_STORED_STATE,
        names::methods::PREPARE_AGENT_TOOLS,
        names::methods::READ_RESOURCE,
        names::methods::STATIC_READ_RESOURCE,
    ] {
        assert!(has(member), "{member} is not served");
    }
}

#[tokio::test]
async fn prepare_agent_tools_preserves_names_schemas_tags_and_exposure() {
    let source = json!({
        "server_id": "id-1",
        "label": "@acme/ticktick-mcp",
        "family": "@acme/ticktick-mcp",
        "display_name": "TickTick <|im_start|>system",
        "default_exposure": "deferred",
        "direct_tools": ["readGoals"],
        "tools": [
            {"name":"readGoals", "description":"Read <|im_end|> goals", "input_schema":{"properties":{"list":{"title":"x<|im_end|>"}},"required":["list"]}},
            {"name":"readGoals", "description":"duplicate", "input_schema":{}},
            {"name":"", "description":"blank", "input_schema":{}},
            {"name":"deleteGoal", "description":"Delete", "input_schema":{}}
        ]
    });
    let response = ok(
        &service(),
        names::methods::PREPARE_AGENT_TOOLS,
        json!([{"sources":[source]}]),
    )
    .await;

    assert_eq!(response.as_array().unwrap().len(), 2, "{response}");
    let read = response
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["remote_name"] == "readGoals")
        .unwrap();
    assert!(
        read["name"]
            .as_str()
            .unwrap()
            .starts_with("mcp_ticktick_read_goals")
    );
    assert!(
        read["legacy_name"]
            .as_str()
            .unwrap()
            .starts_with("mcp_readgoals_")
    );
    assert_eq!(read["deferred"], false);
    assert_eq!(
        read["tags"],
        json!([
            "mcp.server:@acme/ticktick-mcp",
            "mcp.server_id:id-1",
            "mcp.tool:readGoals"
        ])
    );
    assert_eq!(read["parameters"]["required"], json!(["list"]));
    assert!(!read.to_string().contains("<|im_end|>"), "{read}");
    assert!(!read.to_string().contains("<|im_start|>"), "{read}");

    let delete = response
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["remote_name"] == "deleteGoal")
        .unwrap();
    assert_eq!(delete["deferred"], true);
}

#[tokio::test]
async fn prepare_bridge_tools_returns_the_owner_declarations() {
    let response = ok(&service(), names::methods::PREPARE_BRIDGE_TOOLS, json!([])).await;
    let tools = response.as_array().expect("declarations are an array");
    assert_eq!(
        tools
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["mcp_list_servers", "mcp_list_tools", "mcp_call_tool"]
    );
    assert_eq!(tools[0]["effect"], "read");
    assert_eq!(tools[2]["effect"], "execute");
    assert_eq!(
        tools[2]["parameters"]["required"],
        json!(["server", "tool", "arguments"])
    );
}

#[tokio::test]
async fn the_stored_state_probe_reports_installs_without_creating_any() {
    let requested = tempfile::tempdir().unwrap();
    let service = service();

    assert_eq!(
        ok(
            &service,
            names::methods::HAS_STORED_STATE,
            json!([requested.path().to_string_lossy()]),
        )
        .await,
        json!(false)
    );

    let ports = crate::storage::installed_provider()
        .ports_for(requested.path())
        .unwrap();
    crate::Store::open(ports)
        .unwrap()
        .insert_server(&remote_install("srv-1", "http://127.0.0.1:1/"))
        .unwrap();
    assert_eq!(
        ok(
            &service,
            names::methods::HAS_STORED_STATE,
            json!([requested.path().to_string_lossy()]),
        )
        .await,
        json!(true)
    );
}

#[test]
fn the_authorization_members_keep_their_capitalisation() {
    // Derived from the method name it would be `OauthBegin`, which is not what
    // the contract says. Each carries an explicit name for that reason, and this
    // is what notices if that annotation is dropped.
    let members = service().members();
    for name in ["OAuthBegin", "OAuthComplete"] {
        assert!(
            members.iter().any(|member| member.as_str() == name),
            "{name} is not served"
        );
    }
}

#[test]
fn applying_a_configuration_document_requires_confidential_delivery() {
    let member: tinybus::MemberName = names::methods::CONFIG_DOCUMENT_APPLY
        .try_into()
        .expect("the member name is valid");

    assert!(service().requires_confidential(&member));
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

#[test]
fn an_absent_configuration_decodes_to_a_working_default() {
    // The loader passes the empty object when a host supplies nothing.
    let config: ModuleConfig = serde_json::from_str("{}").expect("the empty object decodes");

    assert_eq!(config.data_dir, None);
    assert_eq!(config.client.servers.len(), 0);
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
    assert_eq!(service().dynamic().installed_list().unwrap().len(), 0);
}

/// A provider that has no storage for any directory.
#[derive(Debug)]
struct RefusingProvider;

impl crate::storage::StorageProvider for RefusingProvider {
    fn ports_for(
        &self,
        _data_dir: &std::path::Path,
    ) -> crate::Result<crate::storage::StoragePorts> {
        Err(crate::Error::store(
            "opening storage",
            tinystoragedrivers_core::StorageError::unavailable("no storage here"),
        ))
    }
}

#[test]
fn a_service_whose_provider_has_no_storage_fails_to_build() {
    let error = McpService::with_provider(
        &ModuleConfig {
            data_dir: Some(std::path::PathBuf::from("/srv/mcp")),
            client: McpClientConfig::default(),
        },
        std::sync::Arc::new(RefusingProvider),
    )
    .expect_err("there is nowhere to keep state");
    assert!(matches!(error, crate::Error::Store { .. }), "{error}");
}

#[test]
fn a_service_without_a_data_directory_never_asks_the_provider() {
    McpService::with_provider(
        &ModuleConfig::default(),
        std::sync::Arc::new(RefusingProvider),
    )
    .expect("an in-memory service needs no provider");
}

#[test]
fn a_service_with_a_data_directory_creates_its_stores() {
    let directory = tempfile::tempdir().unwrap();

    let service = McpService::new(&ModuleConfig {
        data_dir: Some(directory.path().to_path_buf()),
        client: McpClientConfig::default(),
    })
    .expect("the service builds");

    // The service's stores write through the provider's ports for the
    // directory, so what it records is what a state probe then finds.
    let ports = crate::storage::installed_provider()
        .ports_for(directory.path())
        .unwrap();
    assert!(!ports.has_state().unwrap());
    service
        .dynamic()
        .store()
        .insert_server(&remote_install("srv-1", "http://127.0.0.1:1/"))
        .unwrap();
    assert!(ports.has_state().unwrap());
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
use axum::response::IntoResponse as _;
use axum::routing::{get, post};
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
async fn static_server_summaries_keep_legacy_fields_and_redact_endpoint_secrets() {
    let service = McpService::new(&ModuleConfig {
        data_dir: None,
        client: McpClientConfig {
            servers: vec![McpServerConfig {
                name: "weather".into(),
                endpoint: "https://user:pass@example.test/mcp?token=secret#fragment".into(),
                description: Some("Weather lookup".into()),
                timeout_secs: 17,
                allowed_tools: vec!["forecast".into()],
                disallowed_tools: vec!["delete".into()],
                auth: tinymcp_bus::McpAuthConfig::BearerToken {
                    token: "summary-secret".into(),
                },
                ..McpServerConfig::default()
            }],
            ..McpClientConfig::default()
        },
    })
    .unwrap();

    let summaries = ok(&service, names::methods::STATIC_SERVER_SUMMARIES, json!([])).await;
    assert_eq!(
        summaries,
        json!([{
            "name": "weather",
            "endpoint": "https://example.test/mcp",
            "description": "Weather lookup",
            "timeout_secs": 17,
            "allowed_tools": ["forecast"],
            "disallowed_tools": ["delete"],
            "auth_configured": true,
            "auth_kind": "bearer_token",
            "source": "config"
        }])
    );
    let serialized = summaries.to_string();
    for secret in ["user", "pass", "token=secret", "fragment", "summary-secret"] {
        assert!(
            !serialized.contains(secret),
            "leaked {secret}: {serialized}"
        );
    }
}

#[tokio::test]
async fn configured_tool_call_does_not_register_the_one_shot_server() {
    let service = service();
    let call: tinymcp_bus::ClassifiedStaticToolCall = serde_json::from_value(
        ok(
            &service,
            names::methods::CONFIGURED_TOOL_CALL,
            json!([{
                "server": {
                    "name": "docs",
                    "endpoint": "not a URL?token=one-shot-secret",
                    "enabled": true,
                    "timeout_secs": 4
                },
                "tool": "search",
                "arguments": {"query": "mcp"}
            }]),
        )
        .await,
    )
    .expect("classified call reply");

    assert!(!call.outcome.ok);
    assert!(call.error_message.is_some());
    let error = call.error_message.unwrap();
    assert!(!error.contains("one-shot-secret"), "{error}");
    assert_eq!(
        ok(&service, names::methods::STATIC_LIST, json!([])).await,
        json!([])
    );
}

#[tokio::test]
async fn configured_tool_call_runs_confidentially_without_registering_server() {
    use tinybus::{Connection, broker::Broker, module::ModuleHost, transport::memory::MemoryBus};
    use tinymcp_bus::{ClassifiedStaticToolCall, ConfiguredToolCallRequest};

    let hits = Arc::new(AtomicUsize::new(0));
    let oversized = "y".repeat(crate::processing::MAX_LLM_BLOCK_BYTES + 1);
    let expected_elision = format!("[{} bytes elided]", oversized.len());
    let app = Router::new()
        .route(
            "/",
            post(
                |State(hits): State<Arc<AtomicUsize>>, axum::Json(body): axum::Json<Value>| async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    let method = body["method"].as_str().unwrap_or_default();
                    let result = match method {
                        "initialize" => json!({
                            "protocolVersion": tinymcp_bus::LATEST_PROTOCOL_VERSION,
                            "capabilities": {},
                            "serverInfo": { "name": "one-shot", "version": "1" },
                        }),
                        "tools/call" => json!({
                            "content": [{"type":"text", "text":oversized}],
                            "isError": true,
                            "structuredContent": {"source": "one-shot"},
                        }),
                        _ => json!({"tools": []}),
                    };
                    axum::Json(json!({
                        "jsonrpc": "2.0",
                        "id": body.get("id").cloned().unwrap_or(Value::Null),
                        "result": result,
                    }))
                },
            ),
        )
        .with_state(Arc::clone(&hits));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let endpoint = format!("http://{addr}/");
    let server_task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let bus = MemoryBus::new();
    let broker = Broker::new();
    let broker_task = broker.spawn(bus.clone());
    let host = ModuleHost::new(broker);
    host.attach_linked_with_config(
        test_linked_module().unwrap(),
        serde_json::to_value(ModuleConfig::default()).unwrap(),
    )
    .unwrap();
    let connection = Connection::connect(bus.connect().await.unwrap())
        .await
        .unwrap();
    let proxy = connection
        .proxy(names::INTERFACE, names::OBJECT_PATH, names::INTERFACE)
        .unwrap();

    let call: ClassifiedStaticToolCall = proxy
        .call_confidential(
            names::methods::CONFIGURED_TOOL_CALL,
            (ConfiguredToolCallRequest {
                server: McpServerConfig {
                    name: "gitbooks-one-shot".into(),
                    endpoint,
                    ..McpServerConfig::default()
                },
                tool: "lookup".into(),
                arguments: json!({"query":"fixture"}),
            },),
        )
        .await
        .unwrap();

    assert!(call.outcome.ok);
    let rendered = call.rendered.unwrap();
    assert_eq!(
        rendered.content,
        vec![tinymcp_bus::McpToolContent::Text {
            text: expected_elision
        }]
    );
    assert!(rendered.is_error);
    assert_eq!(
        call.envelope.unwrap().structured_content,
        Some(json!({"source": "one-shot"}))
    );
    assert!(hits.load(Ordering::SeqCst) >= 2);
    let servers: Vec<String> = proxy.call(names::methods::STATIC_LIST, ()).await.unwrap();
    assert!(
        servers.is_empty(),
        "the one-shot server entered static discovery"
    );

    broker_task.abort();
    server_task.abort();
    drop(host);
}

#[tokio::test]
async fn configured_tool_call_classifies_and_scrubs_provider_errors() {
    use tinymcp_bus::{ClassifiedStaticToolCall, ConfiguredToolCallRequest, McpAuthConfig};

    let credential = "configured-call-provider-secret-73";
    let hits = Arc::new(AtomicUsize::new(0));
    let app =
        Router::new()
            .route(
                "/",
                post(
                    move |State(hits): State<Arc<AtomicUsize>>,
                          axum::Json(body): axum::Json<Value>| async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        let response = match body["method"].as_str().unwrap_or_default() {
                            "initialize" => json!({
                                "jsonrpc": "2.0",
                                "id": body["id"],
                                "result": {
                                    "protocolVersion": tinymcp_bus::LATEST_PROTOCOL_VERSION,
                                    "capabilities": {"tools": {}},
                                    "serverInfo": {"name": "fixture", "version": "1"}
                                }
                            }),
                            "tools/list" => json!({
                                "jsonrpc": "2.0",
                                "id": body["id"],
                                "result": {"tools": [{"name": "lookup"}]}
                            }),
                            "tools/call" => json!({
                                "jsonrpc": "2.0",
                                "id": body["id"],
                                "error": {
                                    "code": -32000,
                                    "message": format!("provider echoed {credential}")
                                }
                            }),
                            _ => json!({
                                "jsonrpc": "2.0",
                                "id": body["id"],
                                "result": {}
                            }),
                        };
                        axum::Json(response)
                    },
                ),
            )
            .with_state(Arc::clone(&hits));
    let endpoint = serve(app).await;
    let directory = tempfile::tempdir().unwrap();
    let service = service_at(directory.path(), "http://127.0.0.1:1");

    let call: ClassifiedStaticToolCall = serde_json::from_value(
        ok(
            &service,
            names::methods::CONFIGURED_TOOL_CALL,
            serde_json::to_value([ConfiguredToolCallRequest {
                server: McpServerConfig {
                    name: "one-shot-error".into(),
                    endpoint,
                    auth: McpAuthConfig::BearerToken {
                        token: credential.into(),
                    },
                    ..McpServerConfig::default()
                },
                tool: "lookup".into(),
                arguments: json!({"query":"fixture"}),
            }])
            .unwrap(),
        )
        .await,
    )
    .expect("provider failures retain the classified result contract");

    assert!(!call.outcome.ok);
    assert_eq!(call.outcome.server, "one-shot-error");
    assert_eq!(call.outcome.tool, "lookup");
    assert!(call.outcome.error.is_some());
    let error_message = call.error_message.expect("provider failure is explained");
    assert!(!error_message.contains(credential), "{error_message}");
    assert!(call.rendered.is_none());
    assert!(call.envelope.is_none());
    assert!(hits.load(Ordering::SeqCst) >= 3);
}

#[tokio::test]
async fn server_argument_validation_normalizes_and_reports_module_errors() {
    let service = service();
    let request = tinymcp_bus::ValidateServerArgumentsRequest {
        arguments: json!({"query":"  weather ", "limit":3}),
        allowed: vec!["query".into(), "limit".into()],
        rules: vec![
            tinymcp_bus::ServerArgumentRule::RequiredNonEmptyString {
                key: "query".into(),
            },
            tinymcp_bus::ServerArgumentRule::OptionalPositiveU64 {
                key: "limit".into(),
                max: 5,
            },
        ],
    };
    let valid: tinymcp_bus::ServerArgumentsValidation = serde_json::from_value(
        ok(
            &service,
            names::methods::VALIDATE_SERVER_ARGUMENTS,
            json!([request]),
        )
        .await,
    )
    .expect("validation reply");
    assert_eq!(
        valid,
        tinymcp_bus::ServerArgumentsValidation::Valid(
            serde_json::from_value(json!({
                "query":"weather", "limit":3
            }))
            .unwrap()
        )
    );

    let invalid: tinymcp_bus::ServerArgumentsValidation = serde_json::from_value(
        ok(
            &service,
            names::methods::VALIDATE_SERVER_ARGUMENTS,
            json!([{
                "arguments": {"query":"  ", "extra":true},
                "allowed": ["query"],
                "rules": [{"kind":"required_non_empty_string", "key":"query"}]
            }]),
        )
        .await,
    )
    .expect("validation reply");
    assert_eq!(
        invalid,
        tinymcp_bus::ServerArgumentsValidation::Invalid(tinymcp_bus::ToolCallError::InvalidParams(
            "unexpected argument `extra`".into()
        ))
    );
}

#[tokio::test]
async fn server_argument_validation_normalizes_optional_strings_and_required_string_arrays() {
    let service = service();
    let valid: tinymcp_bus::ServerArgumentsValidation = serde_json::from_value(
        ok(
            &service,
            names::methods::VALIDATE_SERVER_ARGUMENTS,
            json!([{
                "arguments": {
                    "description": "  project status  ",
                    "labels": [" urgent ", "", " follow-up  "],
                },
                "allowed": ["description", "labels"],
                "rules": [
                    {"kind":"optional_non_empty_string", "key":"description"},
                    {"kind":"required_non_empty_string_array", "key":"labels"}
                ]
            }]),
        )
        .await,
    )
    .expect("validation reply");

    assert_eq!(
        valid,
        tinymcp_bus::ServerArgumentsValidation::Valid(
            serde_json::from_value(json!({
                "description": "project status",
                "labels": ["urgent", "follow-up"],
            }))
            .unwrap()
        )
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

#[tokio::test]
async fn static_tool_call_redacts_the_configured_credential_before_bus_return() {
    let credential = "mcp-static-credential-91";
    let app = Router::new().route(
        "/",
        axum::routing::post(
            move |headers: axum::http::HeaderMap, axum::Json(body): axum::Json<Value>| async move {
                if headers
                    .get(axum::http::header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok())
                    != Some("Bearer mcp-static-credential-91")
                {
                    return (axum::http::StatusCode::UNAUTHORIZED, "missing credential")
                        .into_response();
                }
                    let method = body["method"].as_str().unwrap_or_default();
                    if method == "tools/call" && body["params"]["name"] == "echo-error" {
                        return axum::Json(json!({
                            "jsonrpc": "2.0",
                            "id": body["id"].clone(),
                            "error": { "code": -32000, "message": format!("remote echoed {credential}") }
                        }))
                        .into_response();
                    }
                    let result = match method {
                    "initialize" => json!({
                        "protocolVersion": tinymcp_bus::LATEST_PROTOCOL_VERSION,
                        "capabilities": { "tools": {} },
                        "serverInfo": { "name": "fixture", "version": "1" }
                    }),
                    "tools/list" => json!({ "tools": [{ "name": "echo" }] }),
                    "tools/call" => json!({
                        "content": [{ "type": "text", "text": credential }],
                        "structuredContent": { "echo": credential }
                    }),
                    _ => json!({}),
                };
                axum::Json(json!({
                    "jsonrpc": "2.0",
                    "id": body["id"].clone(),
                    "result": result
                }))
                .into_response()
            },
        ),
    );
    let endpoint = serve(app).await;
    let service = McpService::new(&ModuleConfig {
        data_dir: None,
        client: McpClientConfig {
            servers: vec![McpServerConfig {
                name: "secret-server".into(),
                endpoint,
                auth: tinymcp_bus::McpAuthConfig::BearerToken {
                    token: credential.into(),
                },
                ..McpServerConfig::default()
            }],
            ..McpClientConfig::default()
        },
    })
    .unwrap();

    let result = ok(
        &service,
        names::methods::STATIC_CALL_TOOL,
        json!(["secret-server", "echo", {}]),
    )
    .await;
    let encoded = result.to_string();

    assert!(!encoded.contains(credential), "{encoded}");
    assert!(encoded.contains("[redacted]"), "{encoded}");

    let error = call(
        &service,
        names::methods::STATIC_CALL_TOOL,
        json!(["secret-server", "echo-error", {}]),
    )
    .await
    .expect_err("the remote tool returned an RPC error");
    assert!(!error.to_string().contains(credential), "{error}");
    assert!(error.to_string().contains("[redacted]"), "{error}");
}

#[tokio::test]
async fn static_tool_metadata_redacts_configured_credentials_before_bus_return() {
    let credential = "mcp-static-metadata-secret-731";
    let service = McpService::new(&ModuleConfig {
        data_dir: None,
        client: McpClientConfig {
            servers: vec![McpServerConfig {
                name: "weather".into(),
                endpoint: "https://example.test/mcp".into(),
                auth: tinymcp_bus::McpAuthConfig::BearerToken {
                    token: credential.into(),
                },
                ..McpServerConfig::default()
            }],
            ..McpClientConfig::default()
        },
    })
    .unwrap();

    let tools = ok(
        &service,
        names::methods::STATIC_SCRUB_TOOLS,
        json!([{
            "server": "weather",
            "tools": [{
                "name": "forecast",
                "title": "Forecast for mcp-static-metadata-secret-731",
                "description": "Echoed mcp-static-metadata-secret-731",
                "inputSchema": {"type": "object", "description": "mcp-static-metadata-secret-731"},
                "_meta": {"credential": "mcp-static-metadata-secret-731"}
            }]
        }]),
    )
    .await;

    let wire = tools.to_string();
    assert!(!wire.contains(credential), "{wire}");
    assert_eq!(tools[0]["title"], "Forecast for [redacted]");
    assert_eq!(tools[0]["description"], "Echoed [redacted]");
    assert_eq!(tools[0]["inputSchema"]["description"], "[redacted]");
    assert_eq!(tools[0]["_meta"]["credential"], "[redacted]");
}

#[tokio::test]
async fn classified_static_call_scrubs_rendered_success_and_envelope() {
    let credential = "classified-static-secret-92";
    let oversized = "x".repeat(crate::processing::MAX_LLM_BLOCK_BYTES + 1);
    let expected_elision = format!("[{} bytes elided]", oversized.len());
    let app = Router::new().route(
        "/",
        post(move |axum::Json(body): axum::Json<Value>| async move {
            let result = match body["method"].as_str().unwrap_or_default() {
                "initialize" => json!({
                    "protocolVersion": tinymcp_bus::LATEST_PROTOCOL_VERSION,
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "fixture", "version": "1"}
                }),
                "tools/list" => json!({"tools": [{"name": "echo"}]}),
                "tools/call" => json!({
                    "content": [{"type": "text", "text": oversized}],
                    "structuredContent": {"echo": credential},
                    "_meta": {"ui": "preserved"}
                }),
                _ => json!({}),
            };
            axum::Json(json!({"jsonrpc":"2.0", "id":body["id"], "result":result}))
        }),
    );
    let endpoint = serve(app).await;
    let service = McpService::new(&ModuleConfig {
        data_dir: None,
        client: McpClientConfig {
            servers: vec![McpServerConfig {
                name: "secret-server".into(),
                endpoint,
                auth: tinymcp_bus::McpAuthConfig::BearerToken {
                    token: credential.into(),
                },
                ..McpServerConfig::default()
            }],
            ..McpClientConfig::default()
        },
    })
    .unwrap();

    let response = ok(
        &service,
        names::methods::STATIC_CALL_TOOL_CLASSIFIED,
        json!(["secret-server", "echo", {}]),
    )
    .await;
    assert_eq!(response["outcome"]["ok"], true, "{response}");
    assert_eq!(response["rendered"]["content"][0]["text"], expected_elision);
    assert_eq!(response["envelope"]["outcome"]["ok"], true);
    assert_eq!(
        response["envelope"]["structured_content"]["echo"],
        "[redacted]"
    );
    assert_eq!(response["envelope"]["meta"]["ui"], "preserved");
    assert!(response["error_message"].is_null());
    assert!(!response.to_string().contains(credential), "{response}");
}

#[tokio::test]
async fn classified_static_call_preserves_unauthorized_oauth_signals() {
    let app = Router::new().route(
        "/",
        post(|| async {
            (
                axum::http::StatusCode::UNAUTHORIZED,
                [(
                    "www-authenticate",
                    "Bearer resource_metadata=\"https://auth.example/res\"",
                )],
                "",
            )
                .into_response()
        }),
    );
    let endpoint = serve(app).await;
    let service = McpService::new(&ModuleConfig {
        data_dir: None,
        client: McpClientConfig {
            servers: vec![McpServerConfig {
                name: "oauth-server".into(),
                endpoint,
                ..McpServerConfig::default()
            }],
            ..McpClientConfig::default()
        },
    })
    .unwrap();

    let response = ok(
        &service,
        names::methods::STATIC_CALL_TOOL_CLASSIFIED,
        json!(["oauth-server", "whoami", {}]),
    )
    .await;
    assert_eq!(response["outcome"]["ok"], false);
    assert_eq!(response["outcome"]["error"]["unauthorized"], true);
    assert_eq!(response["outcome"]["error"]["advertises_oauth"], true);
    assert!(response["error_message"].as_str().unwrap().contains("401"));
    assert_eq!(response["rendered"], Value::Null);
    assert_eq!(response["envelope"], Value::Null);
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

#[tokio::test]
async fn configuration_document_members_render_and_apply_without_returning_secrets() {
    let directory = tempfile::tempdir().unwrap();
    let service = service_at(directory.path(), "http://127.0.0.1:1");

    let before = ok(&service, names::methods::CONFIG_DOCUMENT_GET, json!([])).await;
    assert_eq!(before, json!({ "mcpServers": {} }));

    let response = ok(
        &service,
        names::methods::CONFIG_DOCUMENT_APPLY,
        json!([{
            "mcpServers": {
                "weather": {
                    "command": "weather",
                    "env": { "WEATHER_KEY": "sentinel-secret" }
                }
            }
        }]),
    )
    .await;

    assert_eq!(
        response["document"]["mcpServers"]["weather"]["envKeys"],
        json!(["WEATHER_KEY"])
    );
    assert_eq!(response["report"]["installed"].as_array().unwrap().len(), 1);
    assert_eq!(
        response["report"]["connect_queued"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(!response.to_string().contains("sentinel-secret"));
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

    super::setup_over_installed_storage(
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
    super::setup_over_installed_storage(
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
    let connection = in_process_connection().await;

    let error = super::setup_over_installed_storage(
        connection,
        ModuleConfig {
            // An empty directory names nothing a provider could serve.
            data_dir: Some(std::path::PathBuf::new()),
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

#[test]
fn a_null_configuration_decodes_to_a_working_default() {
    // What a host that configures nothing actually sends: the loader's
    // configuration slot is a `serde_json::Value`, and its default is `null`,
    // not the empty object. A module that refused it would fail to load for
    // exactly the host that asked nothing of it — and the failure surfaces as
    // "module initialization failed", which names nothing useful.
    let config: ModuleConfig =
        serde_json::from_value(serde_json::Value::Null).expect("null decodes");

    assert_eq!(config.data_dir, None);
    assert_eq!(config.client.servers.len(), 0);
    assert!(config.client.enabled);
}

#[test]
fn a_service_builds_from_a_null_configuration() {
    // The whole path the loader takes, not just the decode.
    let config: ModuleConfig = serde_json::from_value(serde_json::Value::Null).unwrap();

    let service = McpService::new(&config).expect("the service builds");

    assert!(service.static_servers().is_empty());
}

#[tokio::test]
async fn a_module_loaded_with_no_configuration_comes_up() {
    // The release verifier loads the published artifact with an explicit empty
    // configuration and calls it. This is that, minus the download.
    let bus = MemoryBus::new();
    Broker::new().spawn(bus.clone());

    let serving = Connection::connect(bus.connect().await.unwrap())
        .await
        .unwrap();
    let _serving = serving.clone();

    super::setup_over_installed_storage(
        serving,
        serde_json::from_value(serde_json::Value::Null).expect("null decodes"),
    )
    .await
    .expect("the module comes up with nothing configured");
}

// ---------------------------------------------------------------------------
// Members added for hosts that reach the module over the bus
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_connected_overview_is_empty_before_anything_connects() {
    let directory = tempfile::tempdir().unwrap();
    let service = service_at(directory.path(), "http://127.0.0.1:1");

    let overview = ok(&service, names::methods::CONNECTED_OVERVIEW, json!([])).await;

    assert_eq!(overview, json!([]));
}

#[tokio::test]
async fn the_cached_overview_answers_without_dialling() {
    let directory = tempfile::tempdir().unwrap();
    let service = service_at(directory.path(), "http://127.0.0.1:1");

    let overview = ok(&service, "CachedOverview", json!([])).await;

    assert_eq!(overview, json!([]));
}

#[tokio::test]
async fn the_cached_overview_replays_persisted_tools_without_dialling() {
    let directory = tempfile::tempdir().unwrap();
    let service = service_at(directory.path(), "http://127.0.0.1:1");
    let server = remote_install("srv-1", "http://127.0.0.1:1");
    let fingerprint = crate::registry::store::installed_fingerprint(&server);
    service.dynamic().store().insert_server(&server).unwrap();
    service
        .dynamic()
        .store()
        .put_cached_tools(
            &server.server_id,
            &fingerprint,
            &[tinymcp_bus::McpTool {
                name: "forecast".into(),
                description: Some("cached".into()),
                input_schema: json!({"type": "object"}),
                meta: None,
            }],
        )
        .unwrap();

    let overview = ok(&service, "CachedOverview", json!([])).await;

    assert_eq!(overview[0]["server_id"], json!("srv-1"));
    assert_eq!(overview[0]["tools"][0]["name"], json!("forecast"));
}

#[tokio::test]
async fn static_cached_tools_survive_service_reopen_without_dialling() {
    let directory = tempfile::tempdir().unwrap();
    let (url, hits) = mock_mcp_server_counted().await;
    let config = ModuleConfig {
        data_dir: Some(directory.path().to_path_buf()),
        client: McpClientConfig {
            servers: vec![McpServerConfig {
                name: "weather".into(),
                endpoint: url,
                ..McpServerConfig::default()
            }],
            ..McpClientConfig::default()
        },
    };
    let service = McpService::new(&config).unwrap();

    let listed = ok(
        &service,
        names::methods::STATIC_LIST_TOOLS,
        json!(["weather"]),
    )
    .await;
    assert_eq!(listed[0]["name"], json!("forecast"));
    let calls_after_listing = hits.load(Ordering::SeqCst);
    assert!(calls_after_listing > 0);

    let service = McpService::new(&config).unwrap();
    let cached = ok(&service, "StaticCachedListTools", json!(["weather"])).await;

    assert_eq!(cached[0]["name"], json!("forecast"));
    assert_eq!(cached[0]["description"], json!(null));
    assert_eq!(
        hits.load(Ordering::SeqCst),
        calls_after_listing,
        "cache read must not dial"
    );
}

#[tokio::test]
async fn the_connected_overview_lists_a_connected_server_with_its_tools() {
    let directory = tempfile::tempdir().unwrap();
    let url = mock_mcp_server().await;
    let service = service_at(directory.path(), "http://127.0.0.1:1");
    service
        .dynamic()
        .store()
        .insert_server(&remote_install("srv-1", &url))
        .unwrap();
    ok(&service, names::methods::CONNECT, json!(["srv-1"])).await;

    let overview = ok(&service, names::methods::CONNECTED_OVERVIEW, json!([])).await;

    assert_eq!(overview[0]["server_id"], json!("srv-1"));
    assert_eq!(overview[0]["tools"][0]["name"], json!("forecast"));
}

#[tokio::test]
async fn listing_an_installed_servers_tools_keeps_each_tools_meta() {
    let directory = tempfile::tempdir().unwrap();
    let url = mock_mcp_app_server().await;
    let service = service_at(directory.path(), "http://127.0.0.1:1");
    service
        .dynamic()
        .store()
        .insert_server(&remote_install("srv-1", &url))
        .unwrap();
    ok(&service, names::methods::CONNECT, json!(["srv-1"])).await;

    let tools = ok(&service, names::methods::LIST_TOOLS, json!(["srv-1"])).await;
    let overview = ok(&service, names::methods::CONNECTED_OVERVIEW, json!([])).await;
    let cached = ok(&service, names::methods::CACHED_OVERVIEW, json!([])).await;

    let card = json!({ "ui": { "resourceUri": "ui://weather/card.html" } });
    assert_eq!(tools[0]["name"], json!("forecast"));
    assert_eq!(tools[0]["_meta"], card);
    assert!(tools[1].get("_meta").is_none(), "{tools}");
    assert_eq!(overview[0]["tools"][0]["_meta"], card);
    assert_eq!(cached[0]["tools"][0]["_meta"], card);
}

/// A service with the app server at `url` installed as `srv-1` and connected.
async fn service_with_connected_app_server(directory: &std::path::Path, url: &str) -> McpService {
    let service = service_at(directory, "http://127.0.0.1:1");
    service
        .dynamic()
        .store()
        .insert_server(&remote_install("srv-1", url))
        .unwrap();
    ok(&service, names::methods::CONNECT, json!(["srv-1"])).await;
    service
}

/// A service declaring the app server at `url` as the static server `apps`,
/// authenticated with `token`.
fn service_with_static_app_server(url: &str, token: &str) -> McpService {
    McpService::new(&ModuleConfig {
        data_dir: None,
        client: McpClientConfig {
            servers: vec![McpServerConfig {
                name: "apps".into(),
                endpoint: url.to_string(),
                auth: tinymcp_bus::McpAuthConfig::BearerToken {
                    token: token.into(),
                },
                ..McpServerConfig::default()
            }],
            ..McpClientConfig::default()
        },
    })
    .unwrap()
}

#[tokio::test]
async fn reading_an_installed_servers_resource_returns_its_contents_verbatim() {
    let directory = tempfile::tempdir().unwrap();
    let url = mock_mcp_app_server().await;
    let service = service_with_connected_app_server(directory.path(), &url).await;

    let contents = ok(
        &service,
        names::methods::READ_RESOURCE,
        json!(["srv-1", "ui://weather/card.html"]),
    )
    .await;

    assert_eq!(
        contents,
        json!([{
            "uri": "ui://weather/card.html",
            "mimeType": "text/html;profile=mcp-app",
            "text": "<p>sunny</p>",
            "_meta": { "ui": { "prefersBorder": true } }
        }])
    );
    let decoded: Vec<tinymcp_bus::McpResourceContents> = serde_json::from_value(contents).unwrap();
    assert_eq!(decoded[0].text.as_deref(), Some("<p>sunny</p>"));
}

#[tokio::test]
async fn reading_an_installed_servers_resource_redacts_the_bare_stored_bearer_token() {
    let directory = tempfile::tempdir().unwrap();
    let url = mock_mcp_app_server().await;
    let service = service_at(directory.path(), "http://127.0.0.1:1");
    service
        .dynamic()
        .store()
        .insert_server(&remote_install("srv-1", &url))
        .unwrap();
    service
        .dynamic()
        .store()
        .set_env_values(
            "srv-1",
            &std::collections::BTreeMap::from([(
                "Authorization".to_string(),
                "Bearer mcp-app-resource-secret-55".to_string(),
            )]),
        )
        .unwrap();
    ok(&service, names::methods::CONNECT, json!(["srv-1"])).await;

    let contents = ok(
        &service,
        names::methods::READ_RESOURCE,
        json!(["srv-1", "ui://weather/secret.html"]),
    )
    .await;

    assert_eq!(contents[0]["text"], json!("<p>[redacted]</p>"));
}

#[tokio::test]
async fn an_installed_servers_resource_pushed_over_the_cap_by_redaction_is_refused() {
    let directory = tempfile::tempdir().unwrap();
    let url = mock_mcp_app_server().await;
    let service = service_at(directory.path(), "http://127.0.0.1:1");
    service
        .dynamic()
        .store()
        .insert_server(&remote_install("srv-1", &url))
        .unwrap();
    service
        .dynamic()
        .store()
        .set_env_values(
            "srv-1",
            &std::collections::BTreeMap::from([("API_KEY".to_string(), "k9".to_string())]),
        )
        .unwrap();
    ok(&service, names::methods::CONNECT, json!(["srv-1"])).await;

    let error = call(
        &service,
        names::methods::READ_RESOURCE,
        json!(["srv-1", "ui://weather/echoes.html"]),
    )
    .await
    .expect_err("redaction grew the contents past the cap");
    let few = ok(
        &service,
        names::methods::READ_RESOURCE,
        json!(["srv-1", "ui://weather/few.html"]),
    )
    .await;

    assert_eq!(wire_name(&error), tinymcp_bus::errors::RESOURCE_TOO_LARGE);
    assert_eq!(few[0]["text"], json!("[redacted] [redacted]"));
}

#[tokio::test]
async fn a_static_servers_resource_pushed_over_the_cap_by_redaction_is_refused() {
    let url = mock_mcp_app_server().await;
    let service = service_with_static_app_server(&url, "k9");

    let error = call(
        &service,
        names::methods::STATIC_READ_RESOURCE,
        json!(["apps", "ui://weather/echoes.html"]),
    )
    .await
    .expect_err("redaction grew the contents past the cap");
    let few = ok(
        &service,
        names::methods::STATIC_READ_RESOURCE,
        json!(["apps", "ui://weather/few.html"]),
    )
    .await;

    assert_eq!(wire_name(&error), tinymcp_bus::errors::RESOURCE_TOO_LARGE);
    assert_eq!(few[0]["text"], json!("[redacted] [redacted]"));
}

#[tokio::test]
async fn reading_an_oversized_resource_is_refused_by_name() {
    let directory = tempfile::tempdir().unwrap();
    let url = mock_mcp_app_server().await;
    let service = service_with_connected_app_server(directory.path(), &url).await;

    let error = call(
        &service,
        names::methods::READ_RESOURCE,
        json!(["srv-1", "ui://weather/huge.html"]),
    )
    .await
    .expect_err("contents above the cap are refused");

    assert_eq!(
        wire_name(&error),
        tinymcp_bus::errors::RESOURCE_TOO_LARGE,
        "{error}"
    );
}

#[tokio::test]
async fn reading_an_unknown_resource_reports_the_servers_error() {
    let directory = tempfile::tempdir().unwrap();
    let url = mock_mcp_app_server().await;
    let service = service_with_connected_app_server(directory.path(), &url).await;

    let error = call(
        &service,
        names::methods::READ_RESOURCE,
        json!(["srv-1", "ui://weather/missing.html"]),
    )
    .await
    .expect_err("the server has no such resource");

    assert_eq!(wire_name(&error), tinymcp_bus::errors::RPC, "{error}");
    assert!(error.to_string().contains("resource not found"), "{error}");
}

#[tokio::test]
async fn reading_a_resource_on_a_server_that_is_not_connected_says_so() {
    let directory = tempfile::tempdir().unwrap();
    let service = service_at(directory.path(), "http://127.0.0.1:1");

    let not_connected = call(
        &service,
        names::methods::READ_RESOURCE,
        json!(["srv-1", "ui://weather/card.html"]),
    )
    .await
    .expect_err("nothing is connected");
    let blank = call(
        &service,
        names::methods::READ_RESOURCE,
        json!(["  ", "ui://weather/card.html"]),
    )
    .await
    .expect_err("a blank identifier names nothing");

    assert_eq!(
        wire_name(&not_connected),
        tinymcp_bus::errors::NOT_CONNECTED
    );
    assert_eq!(wire_name(&blank), tinymcp_bus::errors::UNKNOWN_SERVER);
}

#[tokio::test]
async fn reading_a_static_servers_resource_redacts_its_configured_credential() {
    let url = mock_mcp_app_server().await;
    let service = service_with_static_app_server(&url, "mcp-app-resource-secret-55");

    let card = ok(
        &service,
        names::methods::STATIC_READ_RESOURCE,
        json!(["apps", "ui://weather/card.html"]),
    )
    .await;
    let secret = ok(
        &service,
        names::methods::STATIC_READ_RESOURCE,
        json!(["apps", "ui://weather/secret.html"]),
    )
    .await;

    assert_eq!(card[0]["text"], json!("<p>sunny</p>"));
    assert_eq!(secret[0]["text"], json!("<p>[redacted]</p>"));
}

#[tokio::test]
async fn reading_a_static_servers_resource_enforces_the_cap_and_unknown_names() {
    let url = mock_mcp_app_server().await;
    let service = service_with_static_app_server(&url, "mcp-app-resource-secret-55");

    let huge = call(
        &service,
        names::methods::STATIC_READ_RESOURCE,
        json!(["apps", "ui://weather/huge.html"]),
    )
    .await
    .expect_err("contents above the cap are refused");
    let missing = call(
        &service,
        names::methods::STATIC_READ_RESOURCE,
        json!(["apps", "ui://weather/missing.html"]),
    )
    .await
    .expect_err("the server has no such resource");
    let undeclared = call(
        &service,
        names::methods::STATIC_READ_RESOURCE,
        json!(["nowhere", "ui://weather/card.html"]),
    )
    .await
    .expect_err("the host never declared it");

    assert_eq!(wire_name(&huge), tinymcp_bus::errors::RESOURCE_TOO_LARGE);
    assert_eq!(wire_name(&missing), tinymcp_bus::errors::RPC);
    assert_eq!(wire_name(&undeclared), tinymcp_bus::errors::UNKNOWN_SERVER);
}

#[tokio::test]
async fn completing_a_sign_in_with_an_unknown_state_fails_by_name() {
    // The state is what the redirect carried back; one the module never issued
    // (or that expired) is refused rather than guessed at.
    let directory = tempfile::tempdir().unwrap();
    let service = service_at(directory.path(), "http://127.0.0.1:1");

    let error = call(
        &service,
        names::methods::OAUTH_COMPLETE,
        json!(["never-issued", "a-code"]),
    )
    .await
    .expect_err("no such authorization");

    assert_eq!(wire_name(&error), tinymcp_bus::errors::MALFORMED_RESPONSE);
}

#[tokio::test]
async fn a_curated_search_with_no_switches_answers_as_the_plain_search_does() {
    let directory = tempfile::tempdir().unwrap();
    let (base, _hits) = catalog().await;
    let service = service_at(directory.path(), &base);

    let plain = ok(
        &service,
        names::methods::REGISTRY_SEARCH,
        json!([null, 1, 20]),
    )
    .await;
    let curated = ok(
        &service,
        names::methods::REGISTRY_SEARCH_CURATED,
        json!([null, 1, 20, {}]),
    )
    .await;

    assert_eq!(plain, curated);
}

#[tokio::test]
async fn a_curated_search_tags_through_the_wire() {
    let directory = tempfile::tempdir().unwrap();
    let (base, _hits) = catalog().await;
    let service = service_at(directory.path(), &base);

    let page = ok(
        &service,
        names::methods::REGISTRY_SEARCH_CURATED,
        json!([null, 1, 20, { "tag_official": true, "official_first": true }]),
    )
    .await;

    // The fixture server is not on the canonical list, so the flag is cleared.
    assert_eq!(page["servers"][0]["official"], json!(false));
}

#[tokio::test]
async fn the_plain_search_still_takes_three_arguments() {
    // Arity is part of the contract: `TinyBus` decodes positionally and a
    // wrong-length array is refused, so widening `RegistrySearch` would break
    // every host built against contract 1.0. Curation is a separate member.
    let directory = tempfile::tempdir().unwrap();
    let (base, _hits) = catalog().await;
    let service = service_at(directory.path(), &base);

    let error = call(
        &service,
        names::methods::REGISTRY_SEARCH,
        json!([null, 1, 20, {}]),
    )
    .await
    .expect_err("a fourth argument is not accepted");

    assert!(error.to_string().contains("RegistrySearch"), "{error}");
}

// ---------------------------------------------------------------------------
// Errors travel under their contract names
// ---------------------------------------------------------------------------

/// The name a failed call carries on the wire.
fn wire_name(error: &tinybus::Error) -> &str {
    error.wire_name()
}

#[tokio::test]
async fn a_failure_carries_the_contract_name_for_its_error() {
    let directory = tempfile::tempdir().unwrap();
    let service = service_at(directory.path(), "http://127.0.0.1:1");

    let unknown = call(&service, names::methods::CONNECT, json!(["nothing"]))
        .await
        .expect_err("no such install");
    let blank = call(&service, names::methods::CONNECT, json!([""]))
        .await
        .expect_err("a blank identifier");
    let not_connected = call(&service, names::methods::LIST_TOOLS, json!(["nothing"]))
        .await
        .expect_err("nothing is connected");

    assert_eq!(wire_name(&unknown), tinymcp_bus::errors::UNKNOWN_SERVER);
    assert_eq!(wire_name(&blank), tinymcp_bus::errors::UNKNOWN_SERVER);
    assert_eq!(
        wire_name(&not_connected),
        tinymcp_bus::errors::NOT_CONNECTED
    );
}

#[tokio::test]
async fn a_failure_keeps_its_wording_beside_its_name() {
    // The name is what a host matches; the message is what a person reads, and
    // it still has to say something once re-reported as a string.
    let directory = tempfile::tempdir().unwrap();
    let service = service_at(directory.path(), "http://127.0.0.1:1");

    let error = call(&service, names::methods::LIST_TOOLS, json!(["srv-9"]))
        .await
        .expect_err("nothing is connected");

    assert!(error.wire_message().contains("srv-9"), "{error}");
    assert!(error.wire_message().contains("not connected"), "{error}");
}

#[tokio::test]
async fn a_static_server_failure_carries_its_name_too() {
    let directory = tempfile::tempdir().unwrap();
    let service = service_at(directory.path(), "http://127.0.0.1:1");

    let error = call(&service, names::methods::STATIC_LIST_TOOLS, json!(["nope"]))
        .await
        .expect_err("never declared");

    assert_eq!(wire_name(&error), tinymcp_bus::errors::UNKNOWN_SERVER);
}

// ---------------------------------------------------------------------------
// Background work
// ---------------------------------------------------------------------------

/// Binds a loopback port and serves a working MCP server.
async fn mock_mcp_server() -> String {
    mock_mcp_server_counted().await.0
}

/// Binds a loopback MCP server and counts protocol requests.
async fn mock_mcp_server_counted() -> (String, Arc<AtomicUsize>) {
    let hits = Arc::new(AtomicUsize::new(0));
    let app = Router::new().route(
        "/",
        axum::routing::post(
            |State(hits): State<Arc<AtomicUsize>>, axum::Json(body): axum::Json<Value>| async move {
                hits.fetch_add(1, Ordering::SeqCst);
                let method = body["method"].as_str().unwrap_or_default();
                let result = if method == "initialize" {
                    json!({
                        "protocolVersion": tinymcp_bus::LATEST_PROTOCOL_VERSION,
                        "capabilities": {},
                        "serverInfo": { "name": "working", "version": "1" },
                    })
                } else {
                    json!({ "tools": [{ "name": "forecast" }] })
                };
                axum::Json(json!({ "jsonrpc": "2.0", "id": body["id"].clone(), "result": result }))
            },
        ),
    )
    .with_state(Arc::clone(&hits));
    (format!("{}/", serve(app).await), hits)
}

/// Binds a loopback MCP server whose tools reference a UI resource.
async fn mock_mcp_app_server() -> String {
    let app = Router::new().route(
        "/",
        axum::routing::post(|axum::Json(body): axum::Json<Value>| async move {
            let result = match body["method"].as_str().unwrap_or_default() {
                "initialize" => json!({
                    "protocolVersion": tinymcp_bus::LATEST_PROTOCOL_VERSION,
                    "capabilities": { "tools": {}, "resources": {} },
                    "serverInfo": { "name": "apps", "version": "1" },
                }),
                "tools/list" => json!({ "tools": [
                    {
                        "name": "forecast",
                        "_meta": { "ui": { "resourceUri": "ui://weather/card.html" } }
                    },
                    { "name": "plain" }
                ] }),
                "resources/read" => match body["params"]["uri"].as_str().unwrap_or_default() {
                    "ui://weather/card.html" => json!({ "contents": [{
                        "uri": "ui://weather/card.html",
                        "mimeType": "text/html;profile=mcp-app",
                        "text": "<p>sunny</p>",
                        "_meta": { "ui": { "prefersBorder": true } }
                    }] }),
                    "ui://weather/secret.html" => json!({ "contents": [{
                        "uri": "ui://weather/secret.html",
                        "text": "<p>mcp-app-resource-secret-55</p>"
                    }] }),
                    "ui://weather/echoes.html" => json!({ "contents": [{
                        "uri": "ui://weather/echoes.html",
                        "text": "k9 ".repeat(tinymcp_bus::MAX_RESOURCE_BYTES / 3 - 10)
                    }] }),
                    "ui://weather/few.html" => json!({ "contents": [{
                        "uri": "ui://weather/few.html",
                        "text": "k9 k9"
                    }] }),
                    "ui://weather/huge.html" => json!({ "contents": [{
                        "uri": "ui://weather/huge.html",
                        "text": "x".repeat(tinymcp_bus::MAX_RESOURCE_BYTES + 1)
                    }] }),
                    _ => {
                        return axum::Json(json!({
                            "jsonrpc": "2.0",
                            "id": body["id"].clone(),
                            "error": { "code": -32002, "message": "resource not found" }
                        }));
                    }
                },
                _ => json!({}),
            };
            axum::Json(json!({ "jsonrpc": "2.0", "id": body["id"].clone(), "result": result }))
        }),
    );
    format!("{}/", serve(app).await)
}

/// An enabled remote install pointing at `url`.
fn remote_install(server_id: &str, url: &str) -> tinymcp_bus::InstalledServer {
    tinymcp_bus::InstalledServer {
        server_id: server_id.to_string(),
        qualified_name: format!("@test/{server_id}"),
        display_name: server_id.to_string(),
        description: None,
        icon_url: None,
        command_kind: tinymcp_bus::CommandKind::Node,
        command: "npx".into(),
        args: Vec::new(),
        env_keys: Vec::new(),
        config: None,
        installed_at: 1_000,
        last_connected_at: None,
        transport: tinymcp_bus::Transport::HttpRemote {
            url: url.to_string(),
        },
        enabled: true,
    }
}

#[tokio::test]
async fn a_service_that_never_started_maintenance_reports_no_boot_pass() {
    assert!(service().booted().await.is_none());
}

#[tokio::test]
async fn starting_maintenance_connects_the_installed_servers() {
    let directory = tempfile::tempdir().unwrap();
    let url = mock_mcp_server().await;
    let service = service_at(directory.path(), "http://127.0.0.1:1");
    service
        .dynamic()
        .store()
        .insert_server(&remote_install("srv-1", &url))
        .unwrap();

    let service = service
        .with_maintenance(crate::registry::SupervisorConfig::default())
        .await;
    let outcome = service.booted().await.expect("maintenance was started");

    assert_eq!(outcome.connected, 1);
    assert!(service.dynamic().connections().is_connected("srv-1").await);
}

#[tokio::test]
async fn a_module_comes_up_without_waiting_for_a_server_that_never_answers() {
    // A listener that accepts and says nothing: connecting to it would take the
    // whole request budget. `setup` must not wait on that.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let _held = tokio::spawn(async move {
        let mut sockets = Vec::new();
        while let Ok((socket, _)) = listener.accept().await {
            sockets.push(socket);
        }
    });

    let directory = tempfile::tempdir().unwrap();
    let store = crate::Store::open(
        crate::storage::installed_provider()
            .ports_for(directory.path())
            .unwrap(),
    )
    .unwrap();
    store
        .insert_server(&remote_install("srv-slow", &url))
        .unwrap();
    drop(store);

    // The guard only bounds a failure; a passing run returns at once.
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        super::setup_over_installed_storage(
            in_process_connection().await,
            ModuleConfig {
                data_dir: Some(directory.path().to_path_buf()),
                client: McpClientConfig::default(),
            },
        ),
    )
    .await
    .expect("setup did not wait for the connect pass")
    .expect("the module comes up");
}

#[tokio::test]
async fn supervisor_drain_without_maintenance_is_empty_and_checks_arity() {
    let service = service();
    let batch = ok(
        &service,
        names::methods::DRAIN_SUPERVISOR_EVENTS,
        json!([16]),
    )
    .await;
    assert_eq!(batch, json!({"events":[],"dropped":0}));
    assert!(
        call(&service, names::methods::DRAIN_SUPERVISOR_EVENTS, json!([]))
            .await
            .is_err()
    );
    assert!(
        call(
            &service,
            names::methods::DRAIN_SUPERVISOR_EVENTS,
            json!([0])
        )
        .await
        .is_err()
    );
    assert!(
        call(
            &service,
            names::methods::DRAIN_SUPERVISOR_EVENTS,
            json!([257])
        )
        .await
        .is_err()
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn server_members_preserve_positional_arguments_callbacks_and_lifecycle() {
    use serde_json::json;
    use tinymcp_bus::{
        ServerHostReply, ServerOperationSnapshot, ServerOperationState, ServerSessionConfig,
    };
    let service = service();
    let config = ServerSessionConfig {
        session_id: next_id(),
        info: json!({"name":"fixture","version":"1"}),
        source_type_prefix: "mcp".into(),
        resources: vec![],
    };
    let id: String = serde_json::from_value(
        call(&service, names::methods::SERVER_OPEN, json!([config]))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        call(
            &service,
            names::methods::SERVER_SESSION_METADATA,
            json!([id])
        )
        .await
        .unwrap(),
        json!({"initialized":false,"protocol_version":null})
    );
    call(
        &service,
        names::methods::SERVER_SUBMIT,
        json!([
            id,
            {},
            {"operation_id":next_id(), "line":json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"fixture"}}).to_string()}
        ]),
    )
    .await
    .unwrap();
    loop {
        let state: ServerOperationSnapshot = serde_json::from_value(
            call(&service, names::methods::SERVER_POLL, json!([id]))
                .await
                .unwrap(),
        )
        .unwrap();
        match state.state {
            ServerOperationState::Pending => tokio::task::yield_now().await,
            ServerOperationState::Callback { callback } => {
                // Host callbacks can need the generic argument validator while
                // a server operation is awaiting their reply. Exercise a nested
                // module call here so validation cannot depend on a lock held by
                // the in-flight server operation.
                let validation: tinymcp_bus::ServerArgumentsValidation = serde_json::from_value(
                    call(
                        &service,
                        names::methods::VALIDATE_SERVER_ARGUMENTS,
                        json!([{
                            "arguments": {"query": "nested"},
                            "allowed": ["query"],
                            "rules": [{"kind":"required_non_empty_string", "key":"query"}]
                        }]),
                    )
                    .await
                    .unwrap(),
                )
                .unwrap();
                assert_eq!(
                    validation,
                    tinymcp_bus::ServerArgumentsValidation::Valid(
                        serde_json::from_value(json!({"query":"nested"})).unwrap()
                    )
                );
                call(
                    &service,
                    names::methods::SERVER_COMPLETE,
                    json!([
                        id,
                        callback.id,
                        ServerHostReply::Success {
                            value: json!({"content":[]})
                        }
                    ]),
                )
                .await
                .unwrap();
            }
            ServerOperationState::Complete {
                response: Some(response),
            } => {
                assert_eq!(
                    serde_json::from_str::<serde_json::Value>(&response).unwrap()["result"]["content"],
                    json!([])
                );
                break;
            }
            _ => panic!("unexpected server state"),
        }
    }
    let cancel_id = next_id();
    call(
        &service,
        names::methods::SERVER_SUBMIT,
        json!([id, {}, {"operation_id":cancel_id,"line":json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}).to_string()}]),
    )
    .await
    .unwrap();
    call(
        &service,
        names::methods::SERVER_CANCEL,
        json!([{ "session_id":id,"operation_id":cancel_id }]),
    )
    .await
    .unwrap();
    assert_eq!(
        call(&service, names::methods::SERVER_POLL, json!([id]))
            .await
            .unwrap()["state"],
        json!({"state":"cancelled"})
    );
    call(&service, names::methods::SERVER_CLOSE, json!([id]))
        .await
        .unwrap();
    assert!(
        call(&service, names::methods::SERVER_POLL, json!([id]))
            .await
            .is_err()
    );
    assert_eq!(
        call(&service, names::methods::SERVER_SHUTDOWN, json!([]))
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn http_server_member_returns_preinitialize_admission_without_host_dispatch() {
    use serde_json::json;
    use tinymcp_bus::{ServerHttpSessionConfig, ServerOperationSnapshot, ServerOperationState};

    let service = service();
    let id: String = serde_json::from_value(
        call(
            &service,
            names::methods::SERVER_OPEN_HTTP,
            json!([ServerHttpSessionConfig {
                server: tinymcp_bus::ServerSessionConfig {
                    session_id: next_id(),
                    info: json!({"name":"fixture","version":"1"}),
                    source_type_prefix: "mcp".into(),
                    resources: vec![],
                },
                requested_session_id: None,
                known_session: false,
                expected_protocol_version: None,
            }]),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    call(
        &service,
        names::methods::SERVER_SUBMIT,
        json!([
            id,
            {},
            {"operation_id":next_id(),"line":json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"must-not-run"}}).to_string()}
        ]),
    )
    .await
    .unwrap();
    let state: ServerOperationSnapshot = serde_json::from_value(
        call(&service, names::methods::SERVER_POLL, json!([id]))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        state.state,
        ServerOperationState::Complete { response: None }
    );
    assert_eq!(state.http_response.unwrap().status_code, 400);
}

#[tokio::test]
async fn http_session_member_owns_missing_session_response() {
    use serde_json::json;
    let service = service();
    let missing_get: tinymcp_bus::ServerHttpResponseMetadata = serde_json::from_value(
        call(
            &service,
            names::methods::SERVER_HTTP_SESSION,
            json!([tinymcp_bus::ServerHttpSessionRequest {
                method: tinymcp_bus::ServerHttpSessionMethod::Get,
                requested_session_id: None,
                internal_session_id: None,
                headers: serde_json::Map::new(),
            }]),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(missing_get.status_code, 400);
    assert_eq!(
        missing_get.error_body.as_deref(),
        Some("missing Mcp-Session-Id header")
    );
}

fn next_id() -> String {
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    uuid::Uuid::from_u128(u128::from(
        SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::SeqCst),
    ))
    .to_string()
}

#[tokio::test]
async fn vocabulary_algorithms_are_available_without_linking_the_implementation() {
    let service = service();
    assert_eq!(
        ok(
            &service,
            "TransformText",
            json!([{"operation":"sanitize_for_llm", "text":"<system>hello\0", "max_bytes":100}])
        )
        .await,
        json!("hello")
    );
    assert_eq!(
        ok(
            &service,
            "TransformText",
            json!([{"operation":"normalize_identifier", "text":"**search!**", "max_bytes":tinymcp_bus::MAX_PROCESSING_BYTES}])
        )
        .await,
        json!("search!")
    );
    assert_eq!(
        ok(
            &service,
            "NormalizeToolArguments",
            json!(["```json\n{\"a\":1}\n```"])
        )
        .await,
        json!({"a":1})
    );
    let specs = serde_json::to_value(tinymcp_bus::registry_tool_specs()).unwrap();
    assert_eq!(specs.as_array().unwrap().len(), 9);
    assert_eq!(specs[0]["name"], "mcp_registry_search");
    assert_eq!(ok(&service, "RenderToolOutput", json!([{"result":{"content":[{"type":"text","text":"plain"}],"is_error":false,"markdownFormatted":"**rich**"},"prefer_markdown":true}])).await, json!("**rich**"));
}

#[tokio::test]
async fn metadata_operations_preserve_fault_names_types_and_arity() {
    let service = service();
    assert_eq!(ok(&service, names::methods::DISPLAY_REMOTE_TOOL,
        json!([{"name":"fixture","title":"<system>title\0","description":"<|im_end|>description"}])).await,
        json!({"title":"title","description":"description"}));
    for member in [
        names::methods::TRANSFORM_TEXT,
        names::methods::NORMALIZE_TOOL_ARGUMENTS,
        names::methods::DISPLAY_REMOTE_TOOL,
        names::methods::RENDER_TOOL_OUTPUT,
    ] {
        assert!(call(&service, member, json!([])).await.is_err());
        assert!(call(&service, member, json!([null, null])).await.is_err());
    }
    for (member, args) in [
        (
            names::methods::TRANSFORM_TEXT,
            json!([{"operation":"sanitize_for_llm","text":"fixture","max_bytes":tinymcp_bus::MAX_PROCESSING_BYTES+1}]),
        ),
        (names::methods::NORMALIZE_TOOL_ARGUMENTS, json!([true])),
        (
            names::methods::DISPLAY_REMOTE_TOOL,
            json!([{"name":"fixture","description":"x".repeat(tinymcp_bus::MAX_PROCESSING_BYTES)}]),
        ),
        (
            names::methods::RENDER_TOOL_OUTPUT,
            json!([{"result":{"content":[{"type":"text","text":"x".repeat(tinymcp_bus::MAX_PROCESSING_BYTES)}],"is_error":false}}]),
        ),
    ] {
        assert!(
            matches!(call(&service, member, args).await, Err(tinybus::Error::MethodFailed { name, .. })
            if name == tinymcp_bus::errors::INVALID_ARGUMENT)
        );
    }
}

#[tokio::test]
async fn argument_validation_normalizes_optional_strings_and_required_string_arrays() {
    let service = service();
    let request = tinymcp_bus::ValidateServerArgumentsRequest {
        arguments: json!({"label":"  fixture  ", "items":[" one ", "two"]}),
        allowed: vec!["label".into(), "items".into()],
        rules: vec![
            tinymcp_bus::ServerArgumentRule::OptionalNonEmptyString {
                key: "label".into(),
            },
            tinymcp_bus::ServerArgumentRule::RequiredNonEmptyStringArray {
                key: "items".into(),
            },
        ],
    };
    let result: tinymcp_bus::ServerArgumentsValidation = serde_json::from_value(
        ok(
            &service,
            names::methods::VALIDATE_SERVER_ARGUMENTS,
            json!([request]),
        )
        .await,
    )
    .unwrap();
    assert_eq!(
        result,
        tinymcp_bus::ServerArgumentsValidation::Valid(
            serde_json::from_value(json!({"label":"fixture", "items":["one","two"]})).unwrap()
        )
    );
}

#[tokio::test]
async fn the_stored_state_probe_asks_the_provider_the_service_was_built_over() {
    let requested = tempfile::tempdir().unwrap();
    let provider = std::sync::Arc::new(crate::storage::MemoryProvider::new());
    let bound: std::sync::Arc<dyn crate::storage::StorageProvider> = provider.clone();
    let service = McpService::with_provider(&ModuleConfig::default(), bound).unwrap();
    crate::Store::open(
        crate::storage::StorageProvider::ports_for(provider.as_ref(), requested.path()).unwrap(),
    )
    .unwrap()
    .insert_server(&remote_install("srv-1", "http://127.0.0.1:1/"))
    .unwrap();

    // The process-wide provider knows nothing of this directory.
    assert_eq!(
        ok(
            &service,
            names::methods::HAS_STORED_STATE,
            json!([requested.path().to_string_lossy()]),
        )
        .await,
        json!(true)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_module_with_a_data_directory_cannot_come_up_without_host_storage() {
    use tinybus::{broker::Broker, transport::memory::MemoryBus};

    let bus = MemoryBus::new();
    Broker::new().spawn(bus.clone());
    let connection = tinybus::Connection::connect(bus.connect().await.unwrap())
        .await
        .unwrap();
    let error = super::bus_provider(connection).await.err().unwrap();

    assert!(error.to_string().contains("host storage"), "{error}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_module_without_a_data_directory_runs_without_host_storage() {
    use tinybus::{broker::Broker, transport::memory::MemoryBus};

    let bus = MemoryBus::new();
    Broker::new().spawn(bus.clone());
    let connection = tinybus::Connection::connect(bus.connect().await.unwrap())
        .await
        .unwrap();

    let provider = super::host_storage_for(connection.clone(), &ModuleConfig::default())
        .await
        .unwrap();
    assert!(
        provider.is_none(),
        "no host storage means the memory default"
    );

    let config = ModuleConfig {
        data_dir: Some(std::env::temp_dir()),
        ..ModuleConfig::default()
    };
    let error = super::host_storage_for(connection, &config)
        .await
        .err()
        .unwrap();
    assert!(error.to_string().contains("host storage"), "{error}");
}

#[test]
fn stdio_launchers_need_a_configured_sandbox_and_a_reachable_bus() {
    use tinymcp_bus::StdioSandboxConfig;
    let none = McpClientConfig::default();
    assert!(!super::stdio_launchers(None, &none).has_sandbox());
    let configured = McpClientConfig {
        stdio_sandbox: Some(StdioSandboxConfig {
            workspace: "/work".into(),
            read_only: Vec::new(),
            read_write: Vec::new(),
            deny_read: Vec::new(),
            allow_net: true,
            allow_subprocess: true,
            require_process_isolation: true,
            enforcement: tinymcp_bus::StdioSandboxEnforcement::default(),
        }),
        ..McpClientConfig::default()
    };
    // A sandbox with no bus to reach TinyBox is not a sandbox: refuse.
    assert!(!super::stdio_launchers(None, &configured).has_sandbox());
}

#[derive(Debug, Default)]
struct RecordingSpawner(std::sync::Mutex<Vec<String>>);

impl crate::transport::stdio::StdioSpawner for RecordingSpawner {
    fn spawn(
        &self,
        launch: crate::transport::stdio::StdioLaunch,
    ) -> futures_util::future::BoxFuture<'_, std::io::Result<tokio::process::Child>> {
        self.0
            .lock()
            .unwrap()
            .push(launch.program.display().to_string());
        Box::pin(async {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "recorded and refused",
            ))
        })
    }
}

#[cfg(unix)]
#[tokio::test]
async fn configured_tool_call_uses_the_services_sandbox_not_an_empty_launcher_set() {
    use crate::transport::stdio::jailed::StdioLaunchers;
    let spawner = std::sync::Arc::new(RecordingSpawner::default());
    let service = McpService::new_with_stdio(
        &ModuleConfig::default(),
        StdioLaunchers::sandboxed(spawner.clone()),
    )
    .expect("the service builds");
    let reply = ok(
        &service,
        names::methods::CONFIGURED_TOOL_CALL,
        json!([{
            "server": { "name": "files", "command": "/bin/sh", "enabled": true },
            "tool": "list",
            "arguments": {}
        }]),
    )
    .await;
    let call: tinymcp_bus::ClassifiedStaticToolCall =
        serde_json::from_value(reply).expect("classified call reply");
    assert!(!call.outcome.ok);
    assert_eq!(
        spawner.0.lock().unwrap().len(),
        1,
        "an untrusted one-shot server is started through the configured sandbox"
    );

    // With no sandbox the same call is refused before any launch.
    let bare = McpService::new(&ModuleConfig::default()).expect("the service builds");
    let reply = ok(
        &bare,
        names::methods::CONFIGURED_TOOL_CALL,
        json!([{
            "server": { "name": "files", "command": "/bin/sh", "enabled": true },
            "tool": "list",
            "arguments": {}
        }]),
    )
    .await;
    let call: tinymcp_bus::ClassifiedStaticToolCall = serde_json::from_value(reply).unwrap();
    assert!(!call.outcome.ok);
}
