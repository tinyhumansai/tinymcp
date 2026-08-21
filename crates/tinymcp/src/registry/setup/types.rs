//! The secret handle and the vault behind it.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use tokio::sync::{Mutex, oneshot};

use crate::error::{Error, Result};

/// How long an unanswered request waits before giving up.
///
/// A model waiting forever on a prompt the user closed is a hung conversation
/// with no way out.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(300);

/// How long an answered but unused value survives.
///
/// Long enough for a few connection-test retries; short enough that an
/// abandoned setup does not leave a credential sitting in memory.
pub const IDLE_TTL: Duration = Duration::from_secs(900);

/// The scheme every handle carries.
const SCHEME: &str = "secret://";

/// How many hexadecimal characters a handle holds.
///
/// Forty-eight bits, from a version-4 identifier. Short enough to appear in a
/// log line without cluttering it, and far beyond collision within any setup
/// session — and it does not need to be unguessable, because knowing a handle
/// buys nothing: resolution happens inside the process that minted it.
const HANDLE_LENGTH: usize = 12;

/// An opaque handle to a credential the user supplied.
///
/// This is what a model is given. It carries no part of the value.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SecretRef(String);

impl SecretRef {
    /// The handle as it is written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Reads a handle a caller supplied.
    ///
    /// Accepts the written form and bare hexadecimal alike, since a caller may
    /// pass back either. Anything that is not hexadecimal is refused, so a
    /// model that invents a handle gets a clear rejection rather than a lookup
    /// miss that reads like an expiry.
    ///
    /// # Examples
    ///
    /// ```
    /// # use tinymcp::registry::setup::SecretRef;
    /// assert!(SecretRef::parse("secret://abc123").is_some());
    /// assert!(SecretRef::parse("abc123").is_some());
    /// assert!(SecretRef::parse("not-hex").is_none());
    /// ```
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        let trimmed = raw.strip_prefix(SCHEME).unwrap_or(raw).trim();

        if trimmed.is_empty() || !trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }

        Some(Self(format!("{SCHEME}{trimmed}")))
    }

    /// Mints a fresh handle.
    fn mint() -> Self {
        let raw = uuid::Uuid::new_v4().simple().to_string();
        let hex = raw.get(..HANDLE_LENGTH).unwrap_or(&raw);
        Self(format!("{SCHEME}{hex}"))
    }
}

/// One entry in the vault.
#[derive(Debug)]
struct SecretEntry {
    /// The name the credential is known by, such as `NOTION_API_KEY`.
    ///
    /// Safe to log; the value is not.
    key_name: String,
    /// The value, once the user has supplied one.
    value: Option<String>,
    /// When this was created or last used, for the idle sweep.
    last_touched: Instant,
    /// Wakes the request that is waiting on this handle.
    ///
    /// Taken on the first submission, so a second one is a refusal rather than
    /// a send on a closed channel.
    waiter: Option<oneshot::Sender<()>>,
}

/// The credentials a setup flow has collected, by handle.
///
/// One per host. Two would not see each other's handles, and a submission would
/// land in the wrong one.
#[derive(Debug, Default)]
pub struct SecretVault {
    entries: Mutex<HashMap<SecretRef, SecretEntry>>,
}

impl SecretVault {
    /// An empty vault.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Mints a handle and parks a waiter against it.
    ///
    /// Returns the handle and the receiver to await. The caller shows the user
    /// a prompt for `key_name` and waits with [`Self::await_fulfillment`].
    pub async fn request(&self, key_name: &str) -> (SecretRef, oneshot::Receiver<()>) {
        let (sender, receiver) = oneshot::channel();
        let handle = SecretRef::mint();

        self.entries.lock().await.insert(
            handle.clone(),
            SecretEntry {
                key_name: key_name.to_string(),
                value: None,
                last_touched: Instant::now(),
                waiter: Some(sender),
            },
        );

        tracing::debug!(handle = handle.as_str(), key_name, "minted a secret handle");
        (handle, receiver)
    }

    /// Records the value a user supplied.
    ///
    /// Returns whether it was accepted. A handle that is unknown, or that
    /// already holds a value, is refused: a second submission against one
    /// handle is either a duplicate or a mistake, and quietly overwriting the
    /// first would replace a credential the user already confirmed.
    pub async fn submit(&self, handle: &SecretRef, value: String) -> bool {
        let mut entries = self.entries.lock().await;

        let Some(entry) = entries.get_mut(handle) else {
            tracing::warn!(handle = handle.as_str(), "a submission named an unknown handle");
            return false;
        };

        if entry.value.is_some() {
            tracing::warn!(handle = handle.as_str(), "a handle was submitted against twice");
            return false;
        }

        entry.value = Some(value);
        entry.last_touched = Instant::now();
        if let Some(waiter) = entry.waiter.take() {
            // The receiver may already be gone if the request timed out. That
            // is not an error: the value is stored, and a retry will find it.
            let _ = waiter.send(());
        }

        tracing::debug!(handle = handle.as_str(), "a secret handle was fulfilled");
        true
    }

    /// Waits for a handle to be fulfilled, giving up after [`REQUEST_TIMEOUT`].
    ///
    /// The handle is forgotten on either failure, so a timed-out request does
    /// not leave an entry nothing will ever answer.
    ///
    /// # Errors
    ///
    /// Returns [`Error::MalformedResponse`] when the wait times out or the
    /// request is cancelled.
    pub async fn await_fulfillment(
        &self,
        handle: &SecretRef,
        receiver: oneshot::Receiver<()>,
    ) -> Result<()> {
        match tokio::time::timeout(REQUEST_TIMEOUT, receiver).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) => {
                // The sender was dropped, which means the sweep took the entry.
                self.forget(handle).await;
                Err(Error::malformed(format!(
                    "the request for {} was cancelled before the user answered",
                    handle.as_str()
                )))
            }
            Err(_) => {
                self.forget(handle).await;
                Err(Error::malformed(format!(
                    "the request for {} went unanswered for {} seconds",
                    handle.as_str(),
                    REQUEST_TIMEOUT.as_secs()
                )))
            }
        }
    }

    /// Resolves handles to values, leaving them in the vault.
    ///
    /// All or nothing: an unknown or unfulfilled handle fails the whole call
    /// rather than returning a partial set. A connection test run with half a
    /// server's credentials fails in a way that tells the user nothing useful.
    ///
    /// Touches every handle it resolved, so repeated connection tests keep the
    /// idle sweep at bay.
    ///
    /// # Errors
    ///
    /// Returns [`Error::MalformedResponse`] naming the first handle that is
    /// unknown or not yet answered.
    pub async fn resolve(
        &self,
        handles: &HashMap<String, SecretRef>,
    ) -> Result<Vec<(String, String)>> {
        let mut entries = self.entries.lock().await;
        Self::resolve_locked(&mut entries, handles)
    }

    /// Resolves handles and removes them.
    ///
    /// For the step that has committed the values somewhere durable. Both
    /// halves happen under one lock, so nothing can submit against or sweep a
    /// handle between resolving it and dropping it.
    ///
    /// # Errors
    ///
    /// As [`Self::resolve`]. Nothing is removed when it fails, so a caller can
    /// fix the problem and retry without asking the user again.
    pub async fn consume(
        &self,
        handles: &HashMap<String, SecretRef>,
    ) -> Result<Vec<(String, String)>> {
        let mut entries = self.entries.lock().await;
        let resolved = Self::resolve_locked(&mut entries, handles)?;

        for handle in handles.values() {
            entries.remove(handle);
        }

        Ok(resolved)
    }

    /// The resolution both paths share, with the lock already held.
    fn resolve_locked(
        entries: &mut HashMap<SecretRef, SecretEntry>,
        handles: &HashMap<String, SecretRef>,
    ) -> Result<Vec<(String, String)>> {
        let mut resolved = Vec::with_capacity(handles.len());

        for (key_name, handle) in handles {
            let entry = entries.get_mut(handle).ok_or_else(|| {
                Error::malformed(format!("no such secret handle: {}", handle.as_str()))
            })?;

            let value = entry.value.clone().ok_or_else(|| {
                Error::malformed(format!(
                    "the secret handle {} has not been answered yet",
                    handle.as_str()
                ))
            })?;

            entry.last_touched = Instant::now();
            resolved.push((key_name.clone(), value));
        }

        Ok(resolved)
    }

    /// Drops one handle, reporting whether there was one.
    ///
    /// For a setup the user walked away from half-finished.
    pub async fn forget(&self, handle: &SecretRef) -> bool {
        self.entries.lock().await.remove(handle).is_some()
    }

    /// Drops every handle idle longer than [`IDLE_TTL`], reporting how many.
    ///
    /// Cheap enough to call often.
    pub async fn sweep(&self) -> usize {
        let now = Instant::now();
        let mut entries = self.entries.lock().await;

        let before = entries.len();
        entries.retain(|_, entry| now.duration_since(entry.last_touched) < IDLE_TTL);
        let reaped = before - entries.len();

        if reaped > 0 {
            tracing::debug!(reaped, "swept idle secret handles");
        }
        reaped
    }

    /// How many handles the vault holds.
    pub async fn len(&self) -> usize {
        self.entries.lock().await.len()
    }

    /// Whether the vault holds nothing.
    pub async fn is_empty(&self) -> bool {
        self.len().await == 0
    }

    /// The name a handle was requested under.
    ///
    /// The *name*, never the value. A caller re-prompting after a timeout needs
    /// to know what to ask for again.
    pub async fn key_name(&self, handle: &SecretRef) -> Option<String> {
        self.entries
            .lock()
            .await
            .get(handle)
            .map(|entry| entry.key_name.clone())
    }
}
