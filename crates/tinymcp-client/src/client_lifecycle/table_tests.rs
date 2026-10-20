//! Tests for client admission, retirement, and cleanup reservations.

use super::*;

use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

#[derive(Default, Debug)]
struct Client {
    retiring: AtomicBool,
}

impl Retire for Client {
    fn mark_retiring(&self) {
        self.retiring.store(true, Ordering::Release);
    }
    fn is_retiring(&self) -> bool {
        self.retiring.load(Ordering::Acquire)
    }
}

fn admit(table: &ClientTable<Client>, dir: &Path) -> Result<Arc<Client>, AdmitError<()>> {
    table.admit(dir, || false, || Ok(Arc::new(Client::default())))
}

#[test]
fn admission_reuses_the_cached_client() {
    let table = ClientTable::new();
    let dir = Path::new("/w/a");
    let first = admit(&table, dir).unwrap();
    assert!(Arc::ptr_eq(&first, &admit(&table, dir).unwrap()));
    assert_eq!(table.snapshot().len(), 1);
}

#[test]
fn constructor_errors_are_not_cached() {
    let table: ClientTable<Client> = ClientTable::new();
    let dir = Path::new("/w/a");
    assert_eq!(
        table.admit(dir, || false, || Err("boom")).unwrap_err(),
        AdmitError::Create("boom")
    );
    assert!(table.snapshot().is_empty());
}

#[test]
fn a_removed_context_is_refused_even_when_the_cache_is_empty() {
    let table = ClientTable::<Client>::new();
    let result = table.admit(Path::new("/w/a"), || true, || Ok::<_, ()>(Arc::default()));
    assert_eq!(result.unwrap_err(), AdmitError::Unavailable);
}

#[test]
fn retired_and_reserved_directories_are_unavailable() {
    let table = ClientTable::new();
    let dir = Path::new("/w/a");
    let client = admit(&table, dir).unwrap();
    assert!(Arc::ptr_eq(&table.retire(dir).unwrap(), &client));
    assert!(table.close_pending(dir));
    assert_eq!(admit(&table, dir).unwrap_err(), AdmitError::Unavailable);
    let reservation = table.reservation_for(dir);
    assert!(table.cleanup_pending(dir));
    assert!(table.finish_cleanup(dir, Some(&client), &reservation));
    assert!(!table.close_pending(dir));
    assert!(admit(&table, dir).is_ok());
}

#[test]
fn failed_action_keeps_client_and_reservation_for_retry() {
    let table = ClientTable::new();
    let dir = Path::new("/w/a");
    let client = admit(&table, dir).unwrap();
    let _ = table.retire(dir);
    let attempts = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&attempts);
    let reservation = table.reservation_with_action(
        dir,
        Arc::new(move || seen.fetch_add(1, Ordering::SeqCst) > 0),
    );
    assert!(!table.finish_cleanup(dir, Some(&client), &reservation));
    assert!(table.cleanup_pending(dir));
    assert_eq!(table.snapshot().len(), 1);
    assert!(table.finish_cleanup(dir, Some(&client), &reservation));
    assert!(table.snapshot().is_empty());
    assert!(!table.cleanup_pending(dir));
}

#[test]
fn a_stale_close_cannot_evict_a_replacement_entry() {
    let table = ClientTable::new();
    let dir = Path::new("/w/a");
    let old = admit(&table, dir).unwrap();
    let reservation = table.reservation_for(dir);
    table.insert(dir.to_path_buf(), Arc::new(Client::default()));
    assert!(!table.finish_cleanup(dir, Some(&old), &reservation));
    assert_eq!(table.snapshot().len(), 1);
}

#[test]
fn the_cleanup_action_may_use_the_table() {
    let table = Arc::new(ClientTable::new());
    let dir = Path::new("/w/a");
    let client = admit(&table, dir).unwrap();
    let _ = table.retire(dir);
    let inner = Arc::clone(&table);
    let reservation = table.reservation_with_action(
        dir,
        Arc::new(move || {
            let _ = inner.snapshot();
            inner.cleanup_pending(Path::new("/w/a"))
        }),
    );
    assert!(table.finish_cleanup(dir, Some(&client), &reservation));
}

#[test]
fn orphaned_reservations_are_retried_and_a_first_action_wins() {
    let table = ClientTable::<Client>::new();
    let dir = Path::new("/w/a");
    let runs = Arc::new(AtomicUsize::new(0));
    let first = Arc::clone(&runs);
    let _ = table.reservation_with_action(
        dir,
        Arc::new(move || {
            first.fetch_add(1, Ordering::SeqCst);
            true
        }),
    );
    let _ = table.reservation_with_action(dir, Arc::new(|| false));
    table.retry_orphaned();
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    assert!(!table.cleanup_pending(dir));
}

#[test]
fn closed_clients_finish_synchronously_and_open_ones_do_not() {
    let table = ClientTable::new();
    let dir = Path::new("/w/a");
    let client = admit(&table, dir).unwrap();
    let _ = table.retire(dir);
    let reservation = table.reservation_for(dir);
    assert_eq!(
        table.finish_if_closed(dir, &client, &reservation, || false),
        None
    );
    assert_eq!(
        table.finish_if_closed(dir, &client, &reservation, || true),
        Some(true)
    );
}

#[test]
fn host_completion_fences_orphaned_cleanup_until_explicitly_completed() {
    let table = ClientTable::<Client>::new();
    let dir = Path::new("/host/pending");
    let reservation = table.reserve_host_completion(dir);
    table.retry_orphaned();
    assert!(table.cleanup_pending(dir));
    assert!(!table.finish_cleanup(dir, None, &reservation));
    assert_eq!(admit(&table, dir).unwrap_err(), AdmitError::Unavailable);
    assert!(table.complete_host_cleanup(dir, &reservation));
    assert!(!table.cleanup_pending(dir));
    assert!(admit(&table, dir).is_ok());
}

#[test]
fn host_completion_preserves_callbacks_and_cannot_clear_a_successor() {
    let table = ClientTable::<Client>::new();
    let dir = Path::new("/host/callback");
    let reservation = table.reserve_host_completion(dir);
    let runs = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&runs);
    table.reservation_with_action(
        dir,
        Arc::new(move || {
            seen.fetch_add(1, Ordering::SeqCst);
            true
        }),
    );
    table.retry_orphaned();
    assert_eq!(runs.load(Ordering::SeqCst), 0);
    assert!(table.complete_host_cleanup(dir, &reservation));
    assert!(table.cleanup_pending(dir));
    table.retry_orphaned();
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    let successor = table.reserve_host_completion(dir);
    assert!(!table.complete_host_cleanup(dir, &reservation));
    assert!(table.cleanup_pending(dir));
    assert!(table.complete_host_cleanup(dir, &successor));
}

#[test]
fn evicting_a_captured_client_cannot_evict_its_successor_or_release_a_fence() {
    let table = ClientTable::<Client>::new();
    let dir = Path::new("/host/eviction");
    let original = admit(&table, dir).unwrap();
    let reservation = table.reserve_host_completion(dir);
    assert!(table.evict(dir, &original));
    assert!(table.cleanup_pending(dir));
    let successor = Arc::new(Client::default());
    table.insert(dir.to_path_buf(), Arc::clone(&successor));
    assert!(!table.evict(dir, &original));
    assert!(Arc::ptr_eq(&table.snapshot()[0].1, &successor));
    assert!(table.complete_host_cleanup(dir, &reservation));
}
