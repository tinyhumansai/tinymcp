//! Unit tests for the bus name table.
//!
//! The table is the one place a member name is spelled, so what matters here is
//! that it stays internally consistent: no duplicates, no empties, and a shape
//! every member agrees on. A host that calls a member this list does not
//! contain gets a runtime "unknown method", which is precisely the failure the
//! table exists to convert into a compile error.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::{INTERFACE, METHODS, OBJECT_PATH, methods};

#[test]
fn the_object_path_is_the_interface_in_path_form() {
    let expected = format!("/{}", INTERFACE.replace('.', "/"));
    assert_eq!(OBJECT_PATH, expected);
}

#[test]
fn the_interface_is_pinned() {
    // A host binds to this string. Changing it is a breaking contract change,
    // not a rename.
    assert_eq!(INTERFACE, "ai.tinyhumans.tinymcp.Mcp");
    assert_eq!(OBJECT_PATH, "/ai/tinyhumans/tinymcp/Mcp");
}

#[test]
fn every_member_is_listed_exactly_once() {
    let mut sorted = METHODS.to_vec();
    sorted.sort_unstable();
    let mut deduplicated = sorted.clone();
    deduplicated.dedup();
    assert_eq!(sorted, deduplicated);
}

#[test]
fn no_member_name_is_empty() {
    assert!(METHODS.iter().all(|method| !method.is_empty()));
}

#[test]
fn every_member_name_is_pascal_case() {
    // The bus convention. A member that breaks it is almost certainly a typo,
    // and a typo here is only discoverable at runtime.
    for method in METHODS {
        let mut characters = method.chars();
        let first = characters.next().expect("a member name is never empty");
        assert!(
            first.is_ascii_uppercase(),
            "{method} does not start with an uppercase letter"
        );
        assert!(
            method.chars().all(|c| c.is_ascii_alphanumeric()),
            "{method} contains something other than ASCII alphanumerics"
        );
    }
}

#[test]
fn the_method_table_holds_every_declared_member() {
    // Spelled out rather than counted: a member added to `methods` but not to
    // `METHODS` is invisible to the module's own manifest assertion, and this
    // is where that gap surfaces.
    assert_eq!(
        METHODS,
        [
            methods::REGISTRY_SEARCH,
            methods::REGISTRY_GET,
            methods::REGISTRY_SETTINGS_GET,
            methods::REGISTRY_SETTINGS_SET,
            methods::INSTALLED_LIST,
            methods::INSTALL,
            methods::UNINSTALL,
            methods::SET_ENABLED,
            methods::UPDATE_ENV,
            methods::CONNECT,
            methods::DISCONNECT,
            methods::STATUS,
            methods::DETECT_AUTH,
            methods::OAUTH_BEGIN,
            methods::LIST_TOOLS,
            methods::TOOL_CALL,
            methods::CONFIG_ASSIST,
            methods::SETUP_SEARCH,
            methods::SETUP_GET,
            methods::SETUP_REQUEST_SECRET,
            methods::SETUP_SUBMIT_SECRET,
            methods::SETUP_TEST_CONNECTION,
            methods::SETUP_INSTALL_AND_CONNECT,
            methods::STATIC_LIST,
            methods::STATIC_LIST_TOOLS,
            methods::STATIC_CALL_TOOL,
            methods::AUDIT_RECORD_WRITE,
            methods::AUDIT_LIST_WRITES,
        ]
    );
}

#[test]
fn the_setup_members_are_named_as_a_family() {
    // The four families are only distinguishable by prefix, and a member filed
    // under the wrong one reads as belonging to a flow it has nothing to do
    // with.
    for method in [
        methods::SETUP_SEARCH,
        methods::SETUP_GET,
        methods::SETUP_REQUEST_SECRET,
        methods::SETUP_SUBMIT_SECRET,
        methods::SETUP_TEST_CONNECTION,
        methods::SETUP_INSTALL_AND_CONNECT,
    ] {
        assert!(method.starts_with("Setup"), "{method} is not a Setup member");
    }
}

#[test]
fn the_static_members_are_named_as_a_family() {
    for method in [
        methods::STATIC_LIST,
        methods::STATIC_LIST_TOOLS,
        methods::STATIC_CALL_TOOL,
    ] {
        assert!(
            method.starts_with("Static"),
            "{method} is not a Static member"
        );
    }
}

#[test]
fn the_audit_members_are_named_as_a_family() {
    for method in [methods::AUDIT_RECORD_WRITE, methods::AUDIT_LIST_WRITES] {
        assert!(method.starts_with("Audit"), "{method} is not an Audit member");
    }
}
