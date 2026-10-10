//! Unit tests for the discovery-endpoint guard.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::IpAddr;

use super::{checked_addresses, client_pinned_to, guard_endpoint, guarded_client, is_blocked_ip};
use crate::Error;

#[test]
fn resolver_failures_and_empty_answers_remain_retryable() {
    let failed = checked_addresses(
        "MCP discovery",
        Err(std::io::Error::other("temporary resolver failure")),
    )
    .unwrap_err();
    assert!(matches!(failed, Error::EndpointResolution { .. }));
    let empty = checked_addresses("MCP discovery", Ok(Vec::new())).unwrap_err();
    assert!(matches!(empty, Error::EndpointResolution { .. }));
    assert_eq!(
        checked_addresses("MCP", Ok(vec!["8.8.8.8:443".parse().unwrap()]))
            .unwrap()
            .len(),
        1
    );
}

fn ip(raw: &str) -> IpAddr {
    raw.parse().unwrap()
}

#[test]
fn internal_and_special_addresses_are_blocked() {
    for raw in [
        "127.0.0.1",
        "10.1.2.3",
        "172.16.0.1",
        "192.168.1.1",
        "169.254.169.254",
        "0.0.0.0",
        "0.1.2.3",
        "100.64.0.1",
        "100.127.255.254",
        "198.18.0.1",
        "198.19.255.254",
        "240.0.0.1",
        "255.255.255.255",
        "192.0.2.1",
        "224.0.0.1",
        "::1",
        "::",
        "fd00::1",
        "fe80::1",
        "ff02::1",
        "64:ff9b::7f00:1",
        "fec0::1",
        "::127.0.0.1",
        "::ffff:127.0.0.1",
        "::ffff:169.254.169.254",
    ] {
        assert!(is_blocked_ip(&ip(raw)), "{raw} should be blocked");
    }
}

#[test]
fn public_addresses_are_allowed() {
    for raw in [
        "8.8.8.8",
        "1.1.1.1",
        "2606:4700:4700::1111",
        "::ffff:8.8.8.8",
        "2001:4860:4860::8888",
    ] {
        assert!(!is_blocked_ip(&ip(raw)), "{raw} should be allowed");
    }
}

#[tokio::test]
async fn a_public_https_literal_passes() {
    let addresses = guard_endpoint("https://8.8.8.8/token", "token")
        .await
        .expect("public https");
    assert_eq!(addresses[0].ip(), ip("8.8.8.8"));
}

#[tokio::test]
async fn a_public_https_literal_builds_a_pinned_client() {
    guarded_client("https://8.8.8.8/token", "token")
        .await
        .expect("pinned public https client");
}

#[test]
fn a_hostname_client_pins_the_addresses_that_were_checked() {
    let addresses = ["8.8.8.8:443".parse().expect("socket address")];
    client_pinned_to("public.example", &addresses).expect("pinned hostname client");
}

#[tokio::test]
async fn plain_http_is_refused_even_to_a_public_host() {
    let error = guard_endpoint("http://8.8.8.8/token", "token")
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("https"), "{error}");
}

#[tokio::test]
async fn the_cloud_metadata_address_is_refused() {
    let error = guard_endpoint("https://169.254.169.254/latest", "registration")
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("registration"), "{error}");
    assert!(error.contains("disallowed"), "{error}");
}

#[tokio::test]
async fn a_hostname_resolving_to_loopback_is_refused() {
    assert!(
        guard_endpoint("https://localhost/token", "token")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn an_unparseable_endpoint_is_refused() {
    assert!(guard_endpoint("not a url", "token").await.is_err());
}
