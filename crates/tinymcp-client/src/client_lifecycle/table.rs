//! The process-wide configured-client cache and its cleanup reservations.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
};

/// A cache entry the table can mark unavailable.
pub trait Retire {
    /// Marks this client unavailable. Idempotent.
    fn mark_retiring(&self);
    /// Whether [`Self::mark_retiring`] was called.
    fn is_retiring(&self) -> bool;
}

/// Work to run once the module acknowledged the close.
pub type CleanupAction = Arc<dyn Fn() -> bool + Send + Sync>;

/// A directory held unavailable until its close (and optional follow-up
/// action) has been acknowledged.
#[derive(Default)]
pub struct CleanupReservation {
    after_close: Mutex<Option<CleanupAction>>,
    host_completion_pending: AtomicBool,
}

impl std::fmt::Debug for CleanupReservation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CleanupReservation").finish_non_exhaustive()
    }
}

/// Why [`ClientTable::admit`] refused.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AdmitError<E> {
    /// Retiring, reserved for cleanup, or removed by the caller's context.
    Unavailable,
    /// The caller's constructor failed.
    Create(E),
}

/// Clients keyed by the absolute directory they own.
pub struct ClientTable<C> {
    clients: Mutex<HashMap<PathBuf, Arc<C>>>,
    reservations: Mutex<HashMap<PathBuf, Arc<CleanupReservation>>>,
}

impl<C> std::fmt::Debug for ClientTable<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientTable").finish_non_exhaustive()
    }
}

impl<C> Default for ClientTable<C> {
    fn default() -> Self {
        Self {
            clients: Mutex::new(HashMap::new()),
            reservations: Mutex::new(HashMap::new()),
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl<C: Retire> ClientTable<C> {
    /// An empty table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a cleanup reservation holds `dir`.
    #[must_use]
    pub fn cleanup_pending(&self, dir: &Path) -> bool {
        lock(&self.reservations).contains_key(dir)
    }

    /// Whether `dir` is reserved or its cached client is retiring.
    #[must_use]
    pub fn close_pending(&self, dir: &Path) -> bool {
        self.cleanup_pending(dir)
            || lock(&self.clients)
                .get(dir)
                .is_some_and(|client| client.is_retiring())
    }

    /// Returns the cached client for `dir`, creating one with `create` if
    /// absent. A cleanup reservation can appear between the caller's first
    /// check and taking the cache lock, so it is rechecked here together with
    /// `removed`.
    ///
    /// # Errors
    ///
    /// [`AdmitError::Unavailable`] when retiring, reserved or removed;
    /// [`AdmitError::Create`] with the constructor's error.
    pub fn admit<E>(
        &self,
        dir: &Path,
        removed: impl Fn() -> bool,
        create: impl FnOnce() -> Result<Arc<C>, E>,
    ) -> Result<Arc<C>, AdmitError<E>> {
        let mut clients = lock(&self.clients);
        if removed() || self.cleanup_pending(dir) {
            return Err(AdmitError::Unavailable);
        }
        if let Some(client) = clients.get(dir) {
            if client.is_retiring() {
                return Err(AdmitError::Unavailable);
            }
            return Ok(Arc::clone(client));
        }
        let client = create().map_err(AdmitError::Create)?;
        clients.insert(dir.to_path_buf(), Arc::clone(&client));
        Ok(client)
    }

    /// Inserts or replaces the client for `dir` without admission checks.
    pub fn insert(&self, dir: PathBuf, client: Arc<C>) {
        lock(&self.clients).insert(dir, client);
    }

    /// A snapshot of every retained client.
    #[must_use]
    pub fn snapshot(&self) -> Vec<(PathBuf, Arc<C>)> {
        lock(&self.clients)
            .iter()
            .map(|(dir, client)| (dir.clone(), Arc::clone(client)))
            .collect()
    }

    /// Marks the exact cached client for `dir` retiring and returns it.
    pub fn retire(&self, dir: &Path) -> Option<Arc<C>> {
        let clients = lock(&self.clients);
        let client = clients.get(dir)?;
        client.mark_retiring();
        Some(Arc::clone(client))
    }

    /// The current reservation for `dir`, created if absent.
    pub fn reservation_for(&self, dir: &Path) -> Arc<CleanupReservation> {
        let mut reservations = lock(&self.reservations);
        Arc::clone(
            reservations
                .entry(dir.to_path_buf())
                .or_insert_with(|| Arc::new(CleanupReservation::default())),
        )
    }

    /// Fence a directory until the host confirms its own filesystem cleanup.
    /// Module close and orphan retries cannot release this reservation.
    pub fn reserve_host_completion(&self, dir: &Path) -> Arc<CleanupReservation> {
        let mut reservations = lock(&self.reservations);
        let reservation = reservations.entry(dir.to_path_buf()).or_default();
        reservation
            .host_completion_pending
            .store(true, Ordering::Release);
        Arc::clone(reservation)
    }

    /// Confirm host cleanup for the exact reservation. An independent callback
    /// remains reserved for the normal cleanup path. Returns false for a stale
    /// token, which can never clear its successor's fence.
    pub fn complete_host_cleanup(&self, dir: &Path, reservation: &Arc<CleanupReservation>) -> bool {
        let after_close = lock(&reservation.after_close);
        let mut reservations = lock(&self.reservations);
        if !reservations
            .get(dir)
            .is_some_and(|stored| Arc::ptr_eq(stored, reservation))
        {
            return false;
        }
        reservation
            .host_completion_pending
            .store(false, Ordering::Release);
        if after_close.is_none() {
            reservations.remove(dir);
        }
        true
    }

    /// Evict only the captured client, preserving any directory reservation.
    /// A stale completion cannot remove a replacement client.
    pub fn evict(&self, dir: &Path, client: &Arc<C>) -> bool {
        let mut clients = lock(&self.clients);
        if !clients
            .get(dir)
            .is_some_and(|current| Arc::ptr_eq(current, client))
        {
            return false;
        }
        clients.remove(dir);
        true
    }

    /// A reservation for `dir` carrying `action`, kept if one is already set.
    pub fn reservation_with_action(
        &self,
        dir: &Path,
        action: CleanupAction,
    ) -> Arc<CleanupReservation> {
        loop {
            let reservation = self.reservation_for(dir);
            let mut after_close = lock(&reservation.after_close);
            let current = lock(&self.reservations)
                .get(dir)
                .is_some_and(|stored| Arc::ptr_eq(stored, &reservation));
            if !current {
                continue;
            }
            after_close.get_or_insert(action);
            drop(after_close);
            return reservation;
        }
    }

    /// Finishes cleanup now when `is_closed` says the client needs no close.
    /// `None` means a close is still needed.
    pub fn finish_if_closed(
        &self,
        dir: &Path,
        client: &Arc<C>,
        reservation: &Arc<CleanupReservation>,
        is_closed: impl FnOnce() -> bool,
    ) -> Option<bool> {
        is_closed().then(|| self.finish_cleanup(dir, Some(client), reservation))
    }

    /// Runs the reservation's action and drops the client and reservation.
    /// The reservation keeps `dir` unavailable while the action runs, without
    /// the cache lock held, so the action may use the table. Returns false
    /// (leaving everything for a retry) if the action fails or a replacement
    /// entry now owns `dir`.
    pub fn finish_cleanup(
        &self,
        dir: &Path,
        client: Option<&Arc<C>>,
        reservation: &Arc<CleanupReservation>,
    ) -> bool {
        let after_close = lock(&reservation.after_close);
        let is_current = lock(&self.reservations)
            .get(dir)
            .is_some_and(|stored| Arc::ptr_eq(stored, reservation));
        if !is_current || reservation.host_completion_pending.load(Ordering::Acquire) {
            return false;
        }
        if client.is_some_and(|client| !self.owns(dir, client)) {
            return false;
        }
        if after_close.as_ref().is_some_and(|action| !action()) {
            return false;
        }
        if let Some(client) = client {
            let mut clients = lock(&self.clients);
            if !clients
                .get(dir)
                .is_some_and(|current| Arc::ptr_eq(current, client))
            {
                return false;
            }
            clients.remove(dir);
        }
        let mut reservations = lock(&self.reservations);
        if !reservation.host_completion_pending.load(Ordering::Acquire)
            && reservations
                .get(dir)
                .is_some_and(|stored| Arc::ptr_eq(stored, reservation))
        {
            reservations.remove(dir);
            true
        } else {
            false
        }
    }

    /// Retries every reservation that no longer has a cached client.
    pub fn retry_orphaned(&self) {
        let reservations: Vec<_> = lock(&self.reservations)
            .iter()
            .map(|(dir, reservation)| (dir.clone(), Arc::clone(reservation)))
            .collect();
        for (dir, reservation) in reservations {
            if !lock(&self.clients).contains_key(&dir) {
                self.finish_cleanup(&dir, None, &reservation);
            }
        }
    }

    /// The reservation for `dir` if present, without creating one.
    #[must_use]
    pub fn existing_reservation(&self, dir: &Path) -> Option<Arc<CleanupReservation>> {
        lock(&self.reservations).get(dir).cloned()
    }

    fn owns(&self, dir: &Path, client: &Arc<C>) -> bool {
        lock(&self.clients)
            .get(dir)
            .is_some_and(|current| Arc::ptr_eq(current, client))
    }
}

#[cfg(test)]
#[path = "table_race_tests.rs"]
mod tests;
