//! Exercises catalog admission through the non-test library build, where the
//! unit-test-only HTTP loopback fixture exception must not exist.

use tinymcp::registry::ops::build_install_transport;
use tinymcp_bus::RegistryConnection;

fn hosted(url: &str) -> RegistryConnection {
    serde_json::from_value(serde_json::json!({
        "type": "http",
        "published": true,
        "deployment_url": url,
    }))
    .expect("hosted catalog fixture")
}

#[test]
fn production_catalog_admission_rejects_http_loopback() {
    assert!(
        build_install_transport("com.vendor/server", &hosted("http://127.0.0.1:1234/mcp")).is_err()
    );
    assert!(build_install_transport("com.vendor/server", &hosted("https://8.8.8.8/mcp")).is_ok());
}
