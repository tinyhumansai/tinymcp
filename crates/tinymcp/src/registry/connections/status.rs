//! Deciding what state a server is in.

use crate::error::Error;
use tinymcp_bus::{McpAuthHint, ServerStatus};

/// The most recent failed connection attempt for one server.
///
/// The message and the classification live in one record so a status read
/// cannot see a fresh message beside a stale reason.
#[derive(Debug, Clone)]
pub(super) struct ConnectFailure {
    /// The diagnostic, for logs and for the generic error case.
    pub(super) message: String,
    /// Why the server answered 401, when that is what happened.
    ///
    /// `None` for any other failure.
    pub(super) auth: Option<McpAuthHint>,
}

impl ConnectFailure {
    /// Records a failure, classifying it if it was an authentication one.
    ///
    /// `has_credential` is whether anything was actually sent, which is what
    /// separates "your token is wrong" from "you have not supplied one".
    pub(super) fn new(error: &Error, has_credential: bool) -> Self {
        let auth = error
            .is_unauthorized()
            .then(|| McpAuthHint::classify(error.advertises_oauth(), has_credential));

        Self {
            message: error.to_string(),
            auth,
        }
    }
}

/// Decides one server's status.
///
/// Priority: disabled, then connected, then unauthorized, then errored, then
/// disconnected. Pure, so the ordering is testable without a live connection or
/// a store.
///
/// Returns the status, its tool count, the message to surface, and the
/// authentication reason.
///
/// # Why an unauthorized server surfaces no message
///
/// The raw 401 body and the OAuth metadata URL describe a server's authorization
/// setup. A caller needs the *reason* in order to offer the right control, and
/// the reason crosses as a code. The text would only be rendered at a user who
/// cannot act on it, in a place it is likely to be logged.
pub(super) fn classify(
    enabled: bool,
    connected_tool_count: Option<u32>,
    failure: Option<&ConnectFailure>,
) -> (ServerStatus, u32, Option<String>, Option<McpAuthHint>) {
    if !enabled {
        return (ServerStatus::Disabled, 0, None, None);
    }

    if let Some(count) = connected_tool_count {
        return (ServerStatus::Connected, count, None, None);
    }

    match failure {
        Some(ConnectFailure {
            auth: Some(hint), ..
        }) => (ServerStatus::Unauthorized, 0, None, Some(*hint)),
        Some(ConnectFailure { message, .. }) => {
            (ServerStatus::Error, 0, Some(message.clone()), None)
        }
        None => (ServerStatus::Disconnected, 0, None, None),
    }
}
