//! Tests for configured-client lifecycle transitions.

use super::*;

use crate::{McpClientConfig, OpenConfiguredResponse, TransitionStatus};

fn config(tag: &str) -> McpClientConfig {
    let mut config = McpClientConfig::default();
    config.client_identity.name = tag.to_owned();
    config
}

fn receipt(id: &str) -> OpenConfiguredResponse {
    OpenConfiguredResponse {
        object_path: format!("/obj/{id}"),
        transition_id: id.to_owned(),
    }
}

fn open(m: &mut ConfiguredLifecycle, cfg: &McpClientConfig) -> OpenObject {
    loop {
        match m.open_step(cfg) {
            Step::Ready(object) => return object,
            Step::CallOpen(_) => m.open_replied(Ok(receipt("t"))),
            Step::PollOpen(_) => {
                m.open_polled(TransitionStatus::Complete);
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}

#[test]
fn same_config_is_reused_and_a_changed_config_reopens() {
    let mut m = ConfiguredLifecycle::new();
    let a = config("a");
    assert_eq!(open(&mut m, &a).object_path, "/obj/t");
    assert_eq!(
        m.open_step(&a),
        Step::Ready(m.open_object().unwrap().clone())
    );
    let b = config("b");
    assert_eq!(m.open_step(&b), Step::CallOpen(b.clone()));
    m.open_replied(Ok(receipt("u")));
    assert_eq!(m.open_step(&b), Step::PollOpen("u".into()));
}

#[test]
fn rejected_reopen_restores_the_previous_object() {
    let mut m = ConfiguredLifecycle::new();
    let a = config("a");
    open(&mut m, &a);
    let b = config("b");
    assert!(matches!(m.open_step(&b), Step::CallOpen(_)));
    m.open_replied(Err(()));
    assert_eq!(m.open_object().unwrap().config, a);
}

#[test]
fn rejected_first_open_returns_to_closed() {
    let mut m = ConfiguredLifecycle::new();
    assert!(matches!(m.open_step(&config("a")), Step::CallOpen(_)));
    m.open_replied(Err(()));
    assert!(m.is_closed());
}

#[test]
fn a_failed_open_transition_is_retried_once_then_faults() {
    let mut m = ConfiguredLifecycle::new();
    let a = config("a");
    m.open_step(&a);
    m.open_replied(Ok(receipt("1")));
    assert_eq!(m.open_polled(TransitionStatus::Failed), Poll::Continue);
    assert_eq!(m.open_step(&a), Step::CallOpen(a.clone()));
    m.open_replied(Ok(receipt("2")));
    assert_eq!(
        m.open_polled(TransitionStatus::Unknown),
        Poll::Fault(Fault::Transport)
    );
}

#[test]
fn pending_keeps_the_receipt_so_a_later_call_resumes_polling() {
    let mut m = ConfiguredLifecycle::new();
    let a = config("a");
    m.open_step(&a);
    m.open_replied(Ok(receipt("1")));
    assert_eq!(m.open_polled(TransitionStatus::Pending), Poll::Continue);
    assert_eq!(m.open_step(&a), Step::PollOpen("1".into()));
}

#[test]
fn close_finishes_an_inflight_open_then_closes() {
    let mut m = ConfiguredLifecycle::new();
    let a = config("a");
    m.open_step(&a);
    m.open_replied(Ok(receipt("1")));
    assert_eq!(m.close_step(), Step::PollOpen("1".into()));
    assert_eq!(m.open_polled(TransitionStatus::Complete), Poll::Done);
    assert_eq!(m.close_step(), Step::CallClose);
    m.close_replied(Ok(Some("c".into())));
    assert_eq!(m.close_step(), Step::PollClose("c".into()));
    assert_eq!(m.close_polled(TransitionStatus::Complete), Poll::Done);
    assert_eq!(m.close_step(), Step::Closed);
}

#[test]
fn close_with_no_receipt_means_already_closed() {
    let mut m = ConfiguredLifecycle::new();
    open(&mut m, &config("a"));
    assert_eq!(m.close_step(), Step::CallClose);
    m.close_replied(Ok(None));
    assert!(m.is_closed());
}

#[test]
fn failed_close_call_keeps_state_for_retry_and_terminal_failure_retries_once() {
    let mut m = ConfiguredLifecycle::new();
    open(&mut m, &config("a"));
    m.close_step();
    m.close_replied(Err(()));
    assert_eq!(m.close_step(), Step::CallClose);
    m.close_replied(Ok(Some("c".into())));
    assert_eq!(m.close_polled(TransitionStatus::Failed), Poll::Continue);
    assert_eq!(m.close_step(), Step::CallClose);
    m.close_replied(Ok(Some("d".into())));
    assert_eq!(
        m.close_polled(TransitionStatus::Failed),
        Poll::Fault(Fault::Transport)
    );
}

#[test]
fn open_during_a_close_resolves_the_close_first() {
    let mut m = ConfiguredLifecycle::new();
    open(&mut m, &config("a"));
    m.close_step();
    m.close_replied(Ok(Some("c".into())));
    let b = config("b");
    assert_eq!(m.open_step(&b), Step::PollClose("c".into()));
    m.close_polled(TransitionStatus::Complete);
    assert_eq!(m.open_step(&b), Step::CallOpen(b.clone()));
}

#[test]
fn replies_and_polls_outside_their_state_change_nothing() {
    let mut m = ConfiguredLifecycle::default();
    m.open_replied(Ok(receipt("x")));
    m.close_replied(Ok(Some("c".into())));
    assert_eq!(m.open_polled(TransitionStatus::Complete), Poll::Continue);
    assert_eq!(m.close_polled(TransitionStatus::Complete), Poll::Continue);
    assert!(m.is_closed());
    assert!(m.open_object().is_none());
}

#[test]
fn close_before_the_open_reply_repeats_the_open_call() {
    let mut m = ConfiguredLifecycle::new();
    let a = config("a");
    assert_eq!(m.open_step(&a), Step::CallOpen(a.clone()));
    assert_eq!(m.open_step(&a), Step::CallOpen(a.clone()));
    assert_eq!(m.close_step(), Step::CallOpen(a));
}

#[test]
fn a_close_in_flight_is_repeated_or_polled_by_both_entry_points() {
    let mut m = ConfiguredLifecycle::new();
    let a = config("a");
    open(&mut m, &a);
    assert_eq!(m.close_step(), Step::CallClose);
    assert_eq!(m.open_step(&a), Step::CallClose);
    assert_eq!(m.close_step(), Step::CallClose);
    m.close_replied(Ok(Some("c".into())));
    assert_eq!(m.close_step(), Step::PollClose("c".into()));
    assert_eq!(m.close_polled(TransitionStatus::Pending), Poll::Continue);
    assert_eq!(m.close_step(), Step::PollClose("c".into()));
}

#[test]
fn a_pending_open_is_polled_again_by_open_and_close() {
    let mut m = ConfiguredLifecycle::new();
    let a = config("a");
    m.open_step(&a);
    m.open_replied(Ok(receipt("1")));
    assert_eq!(m.open_step(&a), Step::PollOpen("1".into()));
    assert_eq!(m.close_step(), Step::PollOpen("1".into()));
}
