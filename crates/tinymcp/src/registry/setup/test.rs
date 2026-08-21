//! Unit tests for the setup secret vault.
//!
//! The property the whole design exists for is that a value never crosses the
//! model-facing surface. That is asserted directly: the handle is checked for
//! not containing the value, and the only accessor that reads an entry back
//! returns the credential's *name*.
//!
//! The vault is owned rather than global, so these tests need no shared guard
//! and run in parallel like everything else.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;
use std::time::Duration;

use super::types::{IDLE_TTL, REQUEST_TIMEOUT, SecretRef, SecretVault};
use crate::Error;

/// A map from one credential name to one handle.
fn handles(key_name: &str, handle: &SecretRef) -> HashMap<String, SecretRef> {
    HashMap::from([(key_name.to_string(), handle.clone())])
}

// ---------------------------------------------------------------------------
// Handles
// ---------------------------------------------------------------------------

#[test]
fn a_handle_is_read_from_either_form() {
    // A caller may pass back what it was given, or just the hexadecimal.
    let written = SecretRef::parse("secret://abc123").expect("the written form");
    let bare = SecretRef::parse("abc123").expect("bare hexadecimal");

    assert_eq!(written, bare);
    assert_eq!(written.as_str(), "secret://abc123");
}

#[test]
fn a_handle_is_read_after_trimming() {
    assert_eq!(
        SecretRef::parse("  secret://abc123  ").map(|handle| handle.as_str().to_string()),
        Some("secret://abc123".to_string())
    );
}

#[test]
fn anything_that_is_not_hexadecimal_is_refused() {
    // A model that invents a handle gets a clear rejection rather than a lookup
    // miss that reads like an expiry.
    for invalid in ["not-hex", "", "secret://", "   ", "abc-123", "zzz"] {
        assert!(
            SecretRef::parse(invalid).is_none(),
            "{invalid:?} was accepted"
        );
    }
}

#[tokio::test]
async fn two_minted_handles_differ() {
    let vault = SecretVault::new();
    let (first, _) = vault.request("A").await;
    let (second, _) = vault.request("B").await;

    assert_ne!(first, second);
}

#[tokio::test]
async fn a_handle_carries_no_part_of_its_value() {
    // This is the whole point: the handle is what a model sees.
    let vault = SecretVault::new();
    let (handle, _receiver) = vault.request("API_KEY").await;
    vault.submit(&handle, "super-secret-value".into()).await;

    assert!(!handle.as_str().contains("super-secret-value"));
    assert!(handle.as_str().starts_with("secret://"));
}

// ---------------------------------------------------------------------------
// The request and answer cycle
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_request_waits_and_an_answer_wakes_it() {
    let vault = std::sync::Arc::new(SecretVault::new());
    let (handle, receiver) = vault.request("API_KEY").await;

    let answering = {
        let vault = std::sync::Arc::clone(&vault);
        let handle = handle.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            assert!(vault.submit(&handle, "shh".into()).await);
        })
    };

    vault
        .await_fulfillment(&handle, receiver)
        .await
        .expect("the request was answered");
    answering.await.unwrap();

    let resolved = vault.resolve(&handles("API_KEY", &handle)).await.unwrap();
    assert_eq!(resolved, vec![("API_KEY".to_string(), "shh".to_string())]);
}

#[tokio::test]
async fn an_answer_that_arrives_first_is_still_seen() {
    // Nothing orders the user's answer against the wait.
    let vault = SecretVault::new();
    let (handle, receiver) = vault.request("API_KEY").await;

    vault.submit(&handle, "early".into()).await;

    vault
        .await_fulfillment(&handle, receiver)
        .await
        .expect("an answer that arrived before the wait");
}

#[tokio::test]
async fn a_second_answer_against_one_handle_is_refused() {
    // Quietly overwriting would replace a credential the user already
    // confirmed.
    let vault = SecretVault::new();
    let (handle, _receiver) = vault.request("K").await;

    assert!(vault.submit(&handle, "first".into()).await);
    assert!(!vault.submit(&handle, "second".into()).await);

    let resolved = vault.resolve(&handles("K", &handle)).await.unwrap();
    assert_eq!(resolved[0].1, "first");
}

#[tokio::test]
async fn an_answer_against_an_unknown_handle_is_refused() {
    let vault = SecretVault::new();
    let invented = SecretRef::parse("secret://deadbeef").unwrap();

    assert!(!vault.submit(&invented, "value".into()).await);
}

// ---------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_unanswered_handle_cannot_be_resolved() {
    let vault = SecretVault::new();
    let (handle, _receiver) = vault.request("UNSET").await;

    let error = vault
        .resolve(&handles("UNSET", &handle))
        .await
        .expect_err("not answered yet");

    assert!(
        matches!(error, Error::MalformedResponse { .. }),
        "{error:?}"
    );
    assert!(error.to_string().contains("not been answered"), "{error}");
}

#[tokio::test]
async fn an_unknown_handle_cannot_be_resolved() {
    let vault = SecretVault::new();
    let invented = SecretRef::parse("secret://deadbeef").unwrap();

    let error = vault
        .resolve(&handles("X", &invented))
        .await
        .expect_err("no such handle");

    assert!(
        error.to_string().contains("no such secret handle"),
        "{error}"
    );
}

#[tokio::test]
async fn resolution_is_all_or_nothing() {
    // A connection test run with half a server's credentials fails in a way
    // that tells the user nothing useful.
    let vault = SecretVault::new();
    let (answered, _first) = vault.request("ANSWERED").await;
    let (unanswered, _second) = vault.request("UNANSWERED").await;
    vault.submit(&answered, "value".into()).await;

    let mut both = handles("ANSWERED", &answered);
    both.insert("UNANSWERED".to_string(), unanswered);

    assert!(vault.resolve(&both).await.is_err());
}

#[tokio::test]
async fn resolving_leaves_the_handles_in_place() {
    // A connection test may be run several times before an install.
    let vault = SecretVault::new();
    let (handle, _receiver) = vault.request("API_KEY").await;
    vault.submit(&handle, "value".into()).await;

    vault.resolve(&handles("API_KEY", &handle)).await.unwrap();
    vault.resolve(&handles("API_KEY", &handle)).await.unwrap();

    assert_eq!(vault.key_name(&handle).await.as_deref(), Some("API_KEY"));
}

#[tokio::test]
async fn resolving_nothing_yields_nothing() {
    let vault = SecretVault::new();
    assert!(vault.resolve(&HashMap::new()).await.unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// Consumption
// ---------------------------------------------------------------------------

#[tokio::test]
async fn consuming_returns_the_values_and_drops_the_handles() {
    let vault = SecretVault::new();
    let (handle, _receiver) = vault.request("TOKEN").await;
    vault.submit(&handle, "value".into()).await;

    let consumed = vault.consume(&handles("TOKEN", &handle)).await.unwrap();

    assert_eq!(consumed, vec![("TOKEN".to_string(), "value".to_string())]);
    assert!(vault.key_name(&handle).await.is_none());
    assert!(vault.is_empty().await);
}

#[tokio::test]
async fn a_failed_consumption_drops_nothing() {
    // So the caller can fix the problem and retry without asking the user
    // again.
    let vault = SecretVault::new();
    let (answered, _first) = vault.request("ANSWERED").await;
    let (unanswered, _second) = vault.request("UNANSWERED").await;
    vault.submit(&answered, "value".into()).await;

    let mut both = handles("ANSWERED", &answered);
    both.insert("UNANSWERED".to_string(), unanswered.clone());

    assert!(vault.consume(&both).await.is_err());

    assert_eq!(vault.key_name(&answered).await.as_deref(), Some("ANSWERED"));
    assert_eq!(
        vault.key_name(&unanswered).await.as_deref(),
        Some("UNANSWERED")
    );
    assert_eq!(vault.len().await, 2);
}

// ---------------------------------------------------------------------------
// Forgetting and sweeping
// ---------------------------------------------------------------------------

#[tokio::test]
async fn forgetting_reports_whether_there_was_a_handle() {
    let vault = SecretVault::new();
    let (handle, _receiver) = vault.request("K").await;

    assert!(vault.forget(&handle).await);
    assert!(!vault.forget(&handle).await);
}

#[tokio::test]
async fn a_sweep_leaves_a_recently_used_handle_alone() {
    let vault = SecretVault::new();
    let (handle, _receiver) = vault.request("K").await;
    vault.submit(&handle, "value".into()).await;

    assert_eq!(vault.sweep().await, 0);
    assert_eq!(vault.len().await, 1);
}

#[tokio::test]
async fn a_sweep_of_an_empty_vault_reaps_nothing() {
    assert_eq!(SecretVault::new().sweep().await, 0);
}

// ---------------------------------------------------------------------------
// The bounds
// ---------------------------------------------------------------------------

#[test]
fn an_unanswered_request_gives_up_after_five_minutes() {
    // A model waiting forever on a prompt the user closed is a hung
    // conversation with no way out.
    assert_eq!(REQUEST_TIMEOUT, Duration::from_secs(300));
}

#[test]
fn an_unused_value_survives_long_enough_for_a_few_retries() {
    assert_eq!(IDLE_TTL, Duration::from_secs(900));
    assert!(
        IDLE_TTL > REQUEST_TIMEOUT,
        "a value must outlive the request that collected it"
    );
}

#[tokio::test]
async fn an_unanswered_request_times_out_and_forgets_its_handle() {
    // Driven on a paused clock so the test does not wait five real minutes.
    tokio::time::pause();

    let vault = SecretVault::new();
    let (handle, receiver) = vault.request("NEVER_ANSWERED").await;

    let waiting = vault.await_fulfillment(&handle, receiver);
    tokio::time::advance(REQUEST_TIMEOUT + Duration::from_secs(1)).await;

    let error = waiting.await.expect_err("the request went unanswered");
    assert!(error.to_string().contains("unanswered"), "{error}");
    assert!(vault.key_name(&handle).await.is_none());
}
