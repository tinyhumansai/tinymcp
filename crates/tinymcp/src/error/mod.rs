//! Crate-wide error and result types.
//!
//! Every fallible public function in this crate returns [`Result`], and every
//! failure mode a caller might reasonably react to differently is a distinct
//! [`Error`] variant. Add a variant rather than encoding new context into an
//! existing message: callers match on variants, and message text is not a
//! stable API.
//!
//! # Why one variant carries structure
//!
//! [`Error::Unauthorized`] is the one a caller acts on rather than reports. A
//! server that answers 401 is *working* — it is reachable, it understood the
//! request, and it wants credentials — so the right response is to offer the
//! user a way to authenticate, not to show them a failure. Distinguishing that
//! from a transport error by matching on message text is how it used to be
//! done, and text drifts. It carries its own fields so the decision is made on
//! data.
//!
//! # Endpoints in messages are always redacted
//!
//! Any variant carrying an endpoint holds the output of
//! [`crate::redact_endpoint`], never the raw URL. Errors reach logs, telemetry,
//! and user interfaces, and a URL with credentials in its userinfo would reach
//! all three.

use tinymcp_bus::McpAuthChallenge;

/// Errors returned by this crate.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A remote server answered HTTP 401.
    ///
    /// The server is reachable and needs credentials. Callers should offer an
    /// authentication path rather than report a failure; see the module note.
    #[error("mcp unauthorized for `{endpoint}`")]
    Unauthorized {
        /// The redacted endpoint the 401 came from.
        endpoint: String,
        /// The `resource_metadata` URL the challenge advertised, when it did.
        ///
        /// Its presence is what distinguishes a server that wants OAuth from
        /// one that wants a static credential, so it drives which affordance a
        /// caller offers.
        resource_metadata: Option<String>,
    },

    /// A remote server answered with a status other than success.
    #[error("mcp http {status} from `{endpoint}`")]
    Http {
        /// The redacted endpoint.
        endpoint: String,
        /// The status code.
        status: u16,
        /// The response body, for diagnosis.
        body: String,
    },

    /// The transport itself failed: connection refused, timeout, TLS, DNS.
    #[error("mcp transport failure for `{endpoint}`: {source}")]
    Transport {
        /// The redacted endpoint.
        endpoint: String,
        /// What the HTTP client reported.
        #[source]
        source: Box<reqwest::Error>,
    },

    /// A server negotiated a protocol version this client does not speak.
    ///
    /// Continuing would mean guessing at framing that has never been tested,
    /// so the handshake fails instead.
    #[error("unsupported mcp protocol version negotiated by server: {version}")]
    UnsupportedProtocolVersion {
        /// What the server asked for.
        version: String,
    },

    /// A response was not the shape the protocol requires.
    #[error("malformed mcp response: {detail}")]
    MalformedResponse {
        /// What was wrong with it.
        detail: String,
    },

    /// A server returned a JSON-RPC error object.
    #[error("mcp error response: {message}")]
    Rpc {
        /// The `error` member, rendered.
        message: String,
    },

    /// A 401 arrived without a challenge discovery could work from.
    #[error("401 response missing a parseable www-authenticate challenge")]
    MissingAuthChallenge,

    /// Authorization discovery ran but could not complete.
    #[error("mcp authorization discovery failed: {detail}")]
    AuthDiscovery {
        /// What went wrong.
        detail: String,
        /// The challenge discovery started from.
        challenge: Box<McpAuthChallenge>,
    },

    /// A tool was blocked before any request was made.
    ///
    /// The allow and deny lists are enforced ahead of the transport, so a
    /// blocked call never reaches the network or a subprocess.
    #[error("tool `{tool}` is not permitted on server `{server}`")]
    ToolNotAllowed {
        /// The server the call was aimed at.
        server: String,
        /// The tool that was blocked.
        tool: String,
    },

    /// A named server is not configured or not installed.
    #[error("unknown mcp server `{server}`")]
    UnknownServer {
        /// The name that did not resolve.
        server: String,
    },

    /// The HTTP client could not be constructed from the supplied settings.
    ///
    /// In practice this is a malformed proxy URL or an unusable TLS
    /// configuration. The URL is stripped from the cause for the reason given
    /// on [`Self::transport`] — a proxy URL can carry credentials too.
    #[error("could not build an http client: {source}")]
    ClientBuild {
        /// What the HTTP client reported.
        #[source]
        source: Box<reqwest::Error>,
    },

    /// A payload could not be encoded or decoded.
    #[error("mcp payload serialization failed: {source}")]
    Serialization {
        /// What serde reported.
        #[source]
        source: Box<serde_json::Error>,
    },

    /// The installed-server store could not do what was asked of it.
    #[error("mcp store failure while {action}: {source}")]
    Store {
        /// What was being attempted, in the present participle.
        action: String,
        /// What SQLite reported.
        #[source]
        source: Box<rusqlite::Error>,
    },

    /// The store's directory or file could not be reached.
    #[error("mcp store is unreachable at `{}`: {source}", path.display())]
    StoreIo {
        /// The path that could not be reached.
        path: std::path::PathBuf,
        /// What the filesystem reported.
        #[source]
        source: Box<std::io::Error>,
    },
}

impl Error {
    /// Builds a [`Self::Transport`] for `endpoint`, redacting it.
    ///
    /// The URL is stripped from the underlying `reqwest` error before it is
    /// stored. That error's own `Display` renders the request URL in full —
    /// query string included — so keeping it would print the credential this
    /// crate redacts everywhere else, through the `#[source]` chain that every
    /// logger walks. Redacting one field and leaving the cause intact is worse
    /// than not redacting at all, because it reads as safe.
    pub(crate) fn transport(endpoint: &str, source: reqwest::Error) -> Self {
        Self::Transport {
            endpoint: crate::redact_endpoint(endpoint),
            source: Box::new(source.without_url()),
        }
    }

    /// Builds a [`Self::Store`] describing what was being attempted.
    pub(crate) fn store(action: impl Into<String>, source: rusqlite::Error) -> Self {
        Self::Store {
            action: action.into(),
            source: Box::new(source),
        }
    }

    /// Builds a [`Self::MalformedResponse`] from anything printable.
    pub(crate) fn malformed(detail: impl std::fmt::Display) -> Self {
        Self::MalformedResponse {
            detail: detail.to_string(),
        }
    }

    /// Whether this error means "the server wants credentials".
    ///
    /// Callers use this instead of inspecting a message, which is the whole
    /// reason [`Self::Unauthorized`] is a variant.
    ///
    /// # Examples
    ///
    /// ```
    /// # use tinymcp::Error;
    /// let error = Error::Unauthorized {
    ///     endpoint: "https://example.test".into(),
    ///     resource_metadata: None,
    /// };
    /// assert!(error.is_unauthorized());
    /// ```
    #[must_use]
    pub const fn is_unauthorized(&self) -> bool {
        matches!(self, Self::Unauthorized { .. })
    }

    /// Whether the 401 advertised OAuth.
    ///
    /// `false` for every error that is not a 401. A server that advertises
    /// OAuth will refuse a pasted static token however valid it looks, so this
    /// is what decides between offering a sign-in and offering a token field.
    #[must_use]
    pub const fn advertises_oauth(&self) -> bool {
        matches!(
            self,
            Self::Unauthorized {
                resource_metadata: Some(_),
                ..
            }
        )
    }
}

impl From<serde_json::Error> for Error {
    fn from(source: serde_json::Error) -> Self {
        Self::Serialization {
            source: Box::new(source),
        }
    }
}

/// The crate's standard result type.
///
/// Use this alias in public signatures instead of spelling out
/// `std::result::Result<T, Error>`.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod test;
