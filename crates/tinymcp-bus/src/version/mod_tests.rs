//! Unit tests for the contract version and its bind rule.

use super::{CONTRACT_VERSION, binds, is_compatible};

#[test]
fn the_shipped_contract_version_is_pinned() {
    assert_eq!(CONTRACT_VERSION, (1, 6));
}

#[test]
fn a_host_on_this_contract_refuses_a_module_from_before_it() {
    // 1.6 added the tool UI presentation vocabulary; a 1.5 module predates it.
    assert!(!is_compatible((1, 0)));
    assert!(!is_compatible((1, 5)));
}

#[test]
fn the_contract_binds_to_itself() {
    assert!(is_compatible(CONTRACT_VERSION));
}

#[test]
fn a_newer_minor_on_the_module_side_binds() {
    assert!(is_compatible((1, 7)));
    assert!(is_compatible((1, 97)));
}

#[test]
fn an_older_minor_on_the_module_side_is_rejected() {
    // A host built against 1.4 cannot call a 1.2 module: the members it names
    // may not be served.
    assert!(!binds((1, 4), (1, 2)));
    assert!(binds((1, 4), (1, 4)));
}

#[test]
fn a_different_major_is_rejected() {
    assert!(!is_compatible((0, 0)));
    assert!(!is_compatible((2, 0)));
    assert!(!is_compatible((2, 97)));
}
