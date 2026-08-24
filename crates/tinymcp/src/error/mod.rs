//! Crate-wide error and result types.
//!
//! Every fallible public function in this crate returns [`Result`], and every
//! failure mode a caller might reasonably react to differently is a distinct
//! [`Error`] variant. Add a variant rather than encoding new context into an
//! existing message: callers match on variants, and message text is not a
//! stable API.
//!
//! # Why some variants carry structure
//!
//! [`Error::Unauthorized`] is one a caller acts on rather than reports. A
//! server that answers 401 is *working* — it is reachable, it understood the
//! request, and it wants credentials — so the right response is to offer the
//! user a way to authenticate, not to show them a failure. Distinguishing that
//! from a transport error by matching on message text is how it used to be
//! done, and text drifts. It carries its own fields so the decision is made on
//! data.
//!
//! [`Error::MissingRuntime`] is the other, and it is the same argument pointed
//! at the opposite conclusion. A 401 says *try again with credentials*; a
//! missing `uvx` says *stop*. No amount of retrying installs a binary, so a
//! caller that cannot tell this apart from a transport failure will schedule
//! reconnects forever against a host where the answer cannot change. It was a
//! [`Error::MalformedResponse`] carrying a formatted sentence, which was wrong
//! twice over — nothing was malformed and no response arrived — and which left
//! every caller that wanted to act on it substring-matching English.
//!
//! # Endpoints in messages are always redacted
//!
//! Any variant carrying an endpoint holds the output of
//! [`crate::redact_endpoint`], never the raw URL. Errors reach logs, telemetry,
//! and user interfaces, and a URL with credentials in its userinfo would reach
//! all three.

use tinymcp_bus::{CommandKind, McpAuthChallenge};

/// Errors returned by this crate.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A remote server answered HTTP 401.
    ///
    /// The server is reachable and needs credentials. Callers should offer an
    /// authentication path rather than report a failure; see the module note.
    ///
    /// The status is in the message on purpose. A host classifying errors for
    /// its own reporting has only the rendered text to go on when the failure
    /// has crossed an RPC boundary and been re-reported as a string — and
    /// misclassifying this one turns preventable user state into an error
    /// report, once per retry.
    ///
    /// The `resource_metadata` URL is deliberately *not* in the message. It
    /// describes the server's authorization setup, it reaches logs and
    /// telemetry, and a caller that needs it has the field.
    #[error("mcp unauthorized for `{endpoint}` (HTTP 401)")]
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

    /// A stdio server's launcher is not installed on this host.
    ///
    /// Terminal, and that is the point: the command was not found on the
    /// resolved `PATH`, so the process was never started and no reconnect can
    /// change the outcome. A caller should stop attempting and surface an
    /// install path — see the module note on why this is a variant rather than
    /// a message.
    ///
    /// `runtime` is what to install, not what was typed. `command` is the
    /// launcher as configured, kept verbatim so a user can see the exact string
    /// that was looked up — an absolute path that is wrong for this machine
    /// reads very differently from a bare `uvx`.
    ///
    /// The guidance is in the message for the reason given on
    /// [`Self::Unauthorized`]: an error that has crossed an RPC boundary and
    /// been re-reported as a string still has to be useful to whoever reads it.
    #[error("{}", missing_runtime_guidance(command, *runtime))]
    MissingRuntime {
        /// The launcher that was not found, exactly as it was configured.
        command: String,
        /// The runtime that launcher belongs to, and therefore what to install.
        ///
        /// [`CommandKind::Binary`] means the command was not recognised as
        /// belonging to a known ecosystem, so there is nothing to name beyond
        /// the command itself.
        runtime: CommandKind,
    },

    /// A remote server answered with a status other than success.
    ///
    /// The body is rendered, bounded, because it is where the server says
    /// *why*: a token endpoint answering `invalid_grant` reads differently from
    /// one answering `invalid_client`, and a bare status cannot tell them
    /// apart. Every producer of this variant already truncates what it stores;
    /// the bound here is a second one, for a producer that forgets.
    #[error("mcp http {status} from `{endpoint}`{}", rendered_body(body))]
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

    /// A server is installed but has no live connection.
    ///
    /// Distinct from [`Error::UnknownServer`] because the two ask different
    /// things of a caller: one has to install the server, the other has to
    /// connect the one they already have. Collapsing them sends a user looking
    /// for something they already installed.
    #[error("mcp server `{server}` is not connected; connect it first")]
    NotConnected {
        /// The identifier of the server with no live connection.
        server: String,
    },

    /// A server is installed but turned off.
    ///
    /// Being off is a setting the user chose, not a failure, so it is neither a
    /// transport error nor a malformed reply. A caller offers to turn it back
    /// on rather than reporting that something broke.
    #[error("mcp server `{server}` is disabled; turn it on before connecting")]
    ServerDisabled {
        /// The identifier of the server that is turned off.
        server: String,
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
    /// on [`Self::Transport`] — a proxy URL can carry credentials too.
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
        /// What `SQLite` reported.
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

    /// Builds a [`Self::MissingRuntime`] for a launcher that was not found.
    ///
    /// The runtime is classified from the command name rather than passed in,
    /// so every producer of this variant agrees about which ecosystem a
    /// launcher belongs to.
    pub(crate) fn missing_runtime(command: impl Into<String>) -> Self {
        let command = command.into();
        let runtime = crate::transport::stdio::spawn_env::required_runtime(&command);
        Self::MissingRuntime { command, runtime }
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

    /// Whether this error means "the runtime this server needs is not here".
    ///
    /// Terminal. A caller uses this to stop retrying and offer an install path,
    /// which is the whole reason [`Self::MissingRuntime`] is a variant: the
    /// condition used to be reachable only by substring-matching a sentence,
    /// and a supervisor that could not see it scheduled reconnects on a
    /// five-minute ceiling against a binary that was never going to appear.
    ///
    /// # Examples
    ///
    /// ```
    /// # use tinymcp::{CommandKind, Error};
    /// let error = Error::MissingRuntime {
    ///     command: "uvx".into(),
    ///     runtime: CommandKind::Python,
    /// };
    /// assert!(error.is_missing_runtime());
    /// assert!(!error.is_unauthorized());
    /// ```
    #[must_use]
    pub const fn is_missing_runtime(&self) -> bool {
        matches!(self, Self::MissingRuntime { .. })
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

/// The sentence a user reads when a stdio launcher is not installed.
///
/// "Not found" is almost never what they need to hear; "this server needs
/// Node.js" is. The recognised runtimes get a name and an address, and anything
/// else gets a path hint, because naming the wrong ecosystem is worse than
/// naming none.
///
/// Lives here, beside the variant, so [`Error::MissingRuntime`]'s `Display` and
/// [`crate::transport::stdio::spawn_env::missing_command_error`] — which
/// delegates to it — cannot drift into two different sentences.
pub(crate) fn missing_runtime_guidance(command: &str, runtime: CommandKind) -> String {
    match runtime {
        CommandKind::Node => format!(
            "`{command}` was not found. This MCP server needs Node.js, which does not appear \
             to be installed, or is not on this application's PATH. Install Node.js from \
             https://nodejs.org and restart the application."
        ),
        CommandKind::Python => format!(
            "`{command}` was not found. This MCP server needs uv (Python), which does not \
             appear to be installed. Install it from https://docs.astral.sh/uv/ and restart \
             the application."
        ),
        // `CommandKind::Binary`, and whatever is added to that non-exhaustive
        // enum next. A runtime this crate cannot name is exactly the case the
        // generic sentence exists for, so a new variant degrades to correct
        // guidance rather than to a compile error in every downstream crate.
        _ => format!(
            "`{command}` was not found on this application's PATH. Install it, or its runtime, \
             make sure it is available in your shell, then restart the application."
        ),
    }
}

/// How much of a failure body to put in a message.
///
/// These reach logs, telemetry, and user-facing errors. An upstream answering a
/// failure with a whole HTML page would otherwise put all of it in every one.
const MAX_RENDERED_BODY_BYTES: usize = 200;

/// The body as it appears in a message: nothing at all when it is blank, and
/// bounded on a character boundary when it is not.
fn rendered_body(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    if trimmed.len() <= MAX_RENDERED_BODY_BYTES {
        return format!(": {trimmed}");
    }

    let mut end = MAX_RENDERED_BODY_BYTES;
    while end > 0 && !trimmed.is_char_boundary(end) {
        end -= 1;
    }

    format!(": {}…", trimmed.get(..end).unwrap_or_default())
}

#[cfg(test)]
mod test;
