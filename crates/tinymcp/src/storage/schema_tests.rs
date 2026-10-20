//! Tests for the module's storage declaration.

use super::*;

use tinystoragedrivers_core::schema::{Item, Registry, ScopeClass};

#[test]
fn the_schema_composes_under_the_mcp_owner() {
    let registry = Registry::compose([storage_schema()]).unwrap();
    assert_eq!(registry.owners().collect::<Vec<_>>(), [OWNER]);
    for collection in [
        SERVERS,
        SERVER_NAMES,
        REGISTRY_CACHE,
        TOOL_CACHE,
        REGISTRY_INDEX,
        REGISTRY_INDEX_STATE,
    ] {
        assert_eq!(
            registry.collection_version(collection),
            Some(SCHEMA_VERSION)
        );
    }
}

#[test]
fn every_item_is_module_scoped_and_named_under_the_owner() {
    let schema = storage_schema();
    assert!(!schema.items.is_empty());
    for item in &schema.items {
        assert_eq!(item.scope(), ScopeClass::Module, "{}", item.name());
        assert!(item.name().starts_with("mcp."), "{}", item.name());
    }
}

#[test]
fn the_audit_log_is_a_declared_stream() {
    let schema = storage_schema();
    assert!(
        schema
            .items
            .iter()
            .any(|item| matches!(item, Item::Stream { name, .. } if name == AUDIT_STREAM))
    );
}

#[test]
fn the_declared_indexes_use_v2_field_names() {
    let schema = storage_schema();
    let servers = schema
        .items
        .iter()
        .find_map(|item| match item {
            Item::Collection { name, spec, .. } if name == SERVERS => Some(spec),
            _ => None,
        })
        .unwrap();
    let indexed: Vec<&str> = servers
        .indexes
        .iter()
        .map(|index| index.name.as_str())
        .collect();
    assert_eq!(indexed, ["qualified_name", "installed_ms"]);
}

#[test]
fn ids_are_escaped_keys() {
    assert_eq!(doc_id(&["@acme/files"]).unwrap(), "@acme%2Ffiles");
    assert_eq!(doc_id(&["official", "@a/b"]).unwrap(), "official/@a%2Fb");
    assert!(doc_id(&[""]).is_err());
}

#[test]
fn writes_declare_the_schema_version() {
    assert_eq!(write_options().schema, SCHEMA_VERSION);
}
