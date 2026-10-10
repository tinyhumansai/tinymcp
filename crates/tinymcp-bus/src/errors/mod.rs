//! The names a failed call travels under.
//!
//! A member that fails answers with a `(name, message)` pair: the message is
//! prose for a person, the name is what a host matches on. This module is the
//! table of those names — one constant per failure class the module reports,
//! and [`ALL`] listing them — so a host classifies on a constant rather than on
//! wording that can change.
//!
//! # Why a table and not the message
//!
//! A host reporting errors to its own telemetry has, once a failure has crossed
//! the bus, only the name and the text. Classifying "the server wants
//! credentials" by searching the text for `HTTP 401` couples the host to a
//! sentence the module is free to reword, and the two drift without either side
//! noticing. Classifying on [`UNAUTHORIZED`] does not.
//!
//! The message still carries its own wording — including the status for a 401
//! — because a person reading a re-reported string should not need the table.
//!
//! # One name per failure class
//!
//! Each name corresponds to one variant of the module crate's `Error`. The
//! module maps variants to names through an exhaustive `match`, so a variant
//! added without a name here is a compile error there, and a test asserts that
//! every name in [`ALL`] is produced by some variant. A name is never reused
//! for a different failure.
//!
//! `Error` is non-exhaustive and so is this table: a host receiving a name it
//! does not know should treat it as a generic failure rather than reject it.
//! Adding a name is a minor contract bump.

/// The prefix every name in this table shares.
pub const PREFIX: &str = "ai.tinyhumans.tinymcp.Error.";

/// A remote server answered HTTP 401: reachable, and wanting credentials.
///
/// A host should offer an authentication path rather than report a failure.
pub const UNAUTHORIZED: &str = "ai.tinyhumans.tinymcp.Error.Unauthorized";
/// A stdio server's launcher is not installed. Terminal: retrying cannot help.
pub const MISSING_RUNTIME: &str = "ai.tinyhumans.tinymcp.Error.MissingRuntime";
/// A remote server answered with an unsuccessful HTTP status other than 401.
pub const HTTP: &str = "ai.tinyhumans.tinymcp.Error.Http";
/// The transport failed: connection refused, timeout, TLS, DNS.
pub const TRANSPORT: &str = "ai.tinyhumans.tinymcp.Error.Transport";
/// A server negotiated a protocol version the client does not speak.
pub const UNSUPPORTED_PROTOCOL_VERSION: &str =
    "ai.tinyhumans.tinymcp.Error.UnsupportedProtocolVersion";
/// A response was not the shape the protocol requires.
pub const MALFORMED_RESPONSE: &str = "ai.tinyhumans.tinymcp.Error.MalformedResponse";
/// A server returned a JSON-RPC error object.
pub const RPC: &str = "ai.tinyhumans.tinymcp.Error.Rpc";
/// A 401 arrived without a challenge authorization discovery could use.
pub const MISSING_AUTH_CHALLENGE: &str = "ai.tinyhumans.tinymcp.Error.MissingAuthChallenge";
/// Authorization discovery ran but could not complete.
pub const AUTH_DISCOVERY: &str = "ai.tinyhumans.tinymcp.Error.AuthDiscovery";
/// A tool was blocked by the allow or deny list before any request was made.
pub const TOOL_NOT_ALLOWED: &str = "ai.tinyhumans.tinymcp.Error.ToolNotAllowed";
/// A server is installed but has no live connection.
pub const NOT_CONNECTED: &str = "ai.tinyhumans.tinymcp.Error.NotConnected";
/// A resource's contents exceeded the size the client will hold.
pub const RESOURCE_TOO_LARGE: &str = "ai.tinyhumans.tinymcp.Error.ResourceTooLarge";
/// A server is installed but turned off by the user.
pub const SERVER_DISABLED: &str = "ai.tinyhumans.tinymcp.Error.ServerDisabled";
/// A named server is not configured or not installed.
pub const UNKNOWN_SERVER: &str = "ai.tinyhumans.tinymcp.Error.UnknownServer";
/// The module could not build an HTTP client from the supplied settings.
pub const CLIENT_BUILD: &str = "ai.tinyhumans.tinymcp.Error.ClientBuild";
/// A payload could not be encoded or decoded.
pub const SERIALIZATION: &str = "ai.tinyhumans.tinymcp.Error.Serialization";
/// The installed-server store could not do what was asked of it.
pub const STORE: &str = "ai.tinyhumans.tinymcp.Error.Store";
/// A host-supplied credential store could not do what was asked of it.
pub const CREDENTIAL_STORE: &str = "ai.tinyhumans.tinymcp.Error.CredentialStore";
/// The store's directory or file could not be reached.
pub const STORE_IO: &str = "ai.tinyhumans.tinymcp.Error.StoreIo";
/// The module could not do something it needed of the bus, such as serving a
/// new object.
pub const BUS: &str = "ai.tinyhumans.tinymcp.Error.Bus";
/// An argument was well-formed on the wire but unusable, such as a data
/// directory that is not an absolute path.
pub const INVALID_ARGUMENT: &str = "ai.tinyhumans.tinymcp.Error.InvalidArgument";

/// MCP server request or response I/O failed.
pub const SERVER_IO: &str = "ai.tinyhumans.tinymcp.Error.ServerIo";
/// The MCP server could not bind its listening address.
pub const SERVER_BIND: &str = "ai.tinyhumans.tinymcp.Error.ServerBind";
/// A registry configuration document could not be applied.
pub const CONFIG_DOC: &str = "ai.tinyhumans.tinymcp.Error.ConfigDoc";
/// Tool arguments were valid JSON but did not match the tool schema.
pub const INVALID_ARGUMENTS: &str = "ai.tinyhumans.tinymcp.Error.InvalidArguments";
/// An upstream registry did not answer within its time budget.
///
/// Transient: a host shows the catalog as unavailable for now and offers a
/// retry, rather than reporting a failure.
pub const REGISTRY_TIMEOUT: &str = "ai.tinyhumans.tinymcp.Error.RegistryTimeout";

/// Every name in this table.
pub const ALL: &[&str] = &[
    UNAUTHORIZED,
    MISSING_RUNTIME,
    HTTP,
    TRANSPORT,
    UNSUPPORTED_PROTOCOL_VERSION,
    MALFORMED_RESPONSE,
    RPC,
    MISSING_AUTH_CHALLENGE,
    AUTH_DISCOVERY,
    TOOL_NOT_ALLOWED,
    NOT_CONNECTED,
    SERVER_DISABLED,
    UNKNOWN_SERVER,
    CLIENT_BUILD,
    SERIALIZATION,
    STORE,
    CREDENTIAL_STORE,
    STORE_IO,
    BUS,
    INVALID_ARGUMENT,
    SERVER_IO,
    SERVER_BIND,
    CONFIG_DOC,
    INVALID_ARGUMENTS,
    REGISTRY_TIMEOUT,
    RESOURCE_TOO_LARGE,
];

#[cfg(test)]
#[path = "mod_tests.rs"]
mod test;
