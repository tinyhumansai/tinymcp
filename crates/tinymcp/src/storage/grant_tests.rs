use super::*;

use tinystoragedrivers_core::schema::Item;

#[test]
fn the_grant_allows_only_schema_collections_and_streams() {
    let declaration = storage_needs();
    let grant = grant();
    let declared_collections = declaration
        .needs
        .collections
        .iter()
        .map(|spec| spec.name.clone())
        .collect::<Vec<_>>();
    assert_eq!(grant.collections, declared_collections);

    let declared_searchable = declaration
        .needs
        .collections
        .iter()
        .filter(|spec| spec.search.is_some())
        .map(|spec| spec.name.clone())
        .collect::<Vec<_>>();
    assert_eq!(grant.searchable, declared_searchable);

    let schema_streams = storage_schema()
        .items
        .into_iter()
        .filter_map(|item| match item {
            Item::Stream { name, .. } => Some(name),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(grant.streams, schema_streams);

    assert!(grant.collections.contains(&SERVERS.to_owned()));
    assert!(!grant.collections.contains(&"mcp.unrequested".to_owned()));
    assert!(grant.streams.contains(&AUDIT_STREAM.to_owned()));
    assert!(!grant.streams.contains(&"mcp.unrequested".to_owned()));
}

#[test]
fn collection_specs_preserve_the_contract_indexes_and_options() {
    let declaration = storage_needs();
    let specs = collection_specs();
    assert_eq!(specs.len(), declaration.needs.collections.len());

    for (actual, declared) in specs.iter().zip(declaration.needs.collections) {
        assert_eq!(actual.name, declared.name);
        assert_eq!(actual.indexes.len(), declared.indexes.len());
        for (actual, declared) in actual.indexes.iter().zip(&declared.indexes) {
            assert_eq!(actual.name, declared.name);
            assert_eq!(actual.fields, declared.fields);
            assert_eq!(actual.unique, declared.unique);
        }
        assert_eq!(actual.ttl_field.as_deref(), declared.ttl_field.as_deref());
        assert_eq!(
            actual
                .search
                .as_ref()
                .map(|search| search.fields.as_slice()),
            declared
                .search
                .as_ref()
                .map(|search| search.fields.as_slice())
        );
    }
}

#[test]
fn secret_grants_keep_prefix_and_exact_name_boundaries() {
    let declaration = storage_needs();
    let grant = grant();
    let mut declared_secrets = declaration
        .needs
        .secret_prefixes
        .iter()
        .map(|prefix| format!("{prefix}*"))
        .collect::<Vec<_>>();
    declared_secrets.extend(
        declaration
            .exact_secret_names
            .iter()
            .map(|name| (*name).to_owned()),
    );

    assert_eq!(grant.secrets, declared_secrets);
    assert!(grant.secrets.contains(&format!("{ENV_SECRET_PREFIX}*")));
    assert!(grant.secrets.contains(&CACHE_KEY_SECRET.to_owned()));
    assert!(
        !grant
            .secrets
            .contains(&format!("{ENV_SECRET_PREFIX}unlisted"))
    );
    assert!(!grant.secrets.contains(&"mcp_unrequested_secret".to_owned()));
    assert!(
        !grant
            .secrets
            .contains(&format!("{CACHE_KEY_SECRET}_suffix"))
    );
}
