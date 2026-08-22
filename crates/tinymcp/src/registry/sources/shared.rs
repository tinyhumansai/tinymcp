//! Helpers both catalog adapters need.

use crate::registry::Store;

/// How much of an upstream failure body to keep.
///
/// These bodies reach a log line and an error message, and an upstream that
/// answers a failure with a whole HTML page would otherwise put all of it
/// there.
pub(super) const MAX_ERROR_BODY_BYTES: usize = 200;

/// Truncates on a character boundary.
pub(super) fn truncate(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }

    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }

    text.get(..end).unwrap_or_default().to_string()
}

/// Writes a response to the cache, treating a failure as unimportant.
///
/// A cache that cannot be written costs a round trip next time. Failing the
/// user's search over it would cost them the search.
pub(super) fn cache(store: &Store, cache_key: &str, body: &str) {
    if let Err(error) = store.cache(cache_key, body) {
        tracing::debug!(cache_key, "could not cache an upstream response: {error}");
    }
}
