//! Checks the Rust-addressable `TinyBus` entries in a statically linked build.

#![cfg(feature = "static-link")]

use tinymcp::tinybus_module::{
    TINYBUS_MODULE_ABI_V1, tinybus_module_init_v1, tinybus_module_manifest_v1,
};

#[test]
fn linked_module_exposes_descriptor_manifest_and_init() {
    let descriptor = &TINYBUS_MODULE_ABI_V1;
    let mut expected_name = [0; 64];
    expected_name[..7].copy_from_slice(b"tinymcp");
    assert_eq!(descriptor.module_name, expected_name);
    assert!(tinybus_module_manifest_v1().len > 0);
    assert_ne!(tinybus_module_init_v1 as *const () as usize, 0);
}
