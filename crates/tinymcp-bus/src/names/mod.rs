//! The bus identity of the `tinymcp` module: interface name, object path, and
//! one constant per member.
//!
//! Nothing here is a string literal at a call site. A host names a member
//! through [`methods`] and the object through [`OBJECT_PATH`], so a rename is a
//! compile error in every consumer rather than a runtime "unknown method".
//!
//! # The three families
//!
//! [`METHODS`] is one flat list, but it covers three groups that answer to
//! different parts of a host:
//!
//! - **Registry** — browsing upstream catalogs, installing, connecting, and
//!   calling tools on servers the *user* chose at runtime. Backed by a store.
//! - **Setup** — the guided flow that walks a user through configuring a server
//!   they have just found, including handling its secrets.
//! - **Static** — the servers a *host* declared in its own configuration. No
//!   store, no install step; they exist because the host said so.
//! - **Audit** — the durable record of every write an MCP tool performed.
//!
//! They share one interface rather than claiming four objects because they
//! share every transport primitive underneath, and splitting them would
//! multiply a host's connection bookkeeping without isolating anything.

/// The well-known interface name the module claims on the bus.
pub const INTERFACE: &str = "ai.tinyhumans.tinymcp.Mcp";

/// The object path the module serves its interface at.
pub const OBJECT_PATH: &str = "/ai/tinyhumans/tinymcp/Mcp";

/// One constant per member of [`INTERFACE`].
pub mod methods {
    // -- Registry: browsing -------------------------------------------------

    /// Searches the upstream registries for servers.
    pub const REGISTRY_SEARCH: &str = "RegistrySearch";
    /// Fetches one server's detail record from its upstream registry.
    pub const REGISTRY_GET: &str = "RegistryGet";
    /// Reports the registry-browse credentials, with secrets redacted.
    pub const REGISTRY_SETTINGS_GET: &str = "RegistrySettingsGet";
    /// Replaces the registry-browse credentials.
    pub const REGISTRY_SETTINGS_SET: &str = "RegistrySettingsSet";

    // -- Registry: installs -------------------------------------------------

    /// Lists the servers the user has installed.
    pub const INSTALLED_LIST: &str = "InstalledList";
    /// Installs a server from an upstream registry.
    pub const INSTALL: &str = "Install";
    /// Removes an installed server, its stored credentials, and its row.
    pub const UNINSTALL: &str = "Uninstall";
    /// Turns an installed server on or off without uninstalling it.
    pub const SET_ENABLED: &str = "SetEnabled";
    /// Replaces an installed server's stored environment values.
    pub const UPDATE_ENV: &str = "UpdateEnv";

    // -- Registry: connections ----------------------------------------------

    /// Connects an installed server and lists what it advertises.
    pub const CONNECT: &str = "Connect";
    /// Disconnects an installed server.
    pub const DISCONNECT: &str = "Disconnect";
    /// Reports the connection state of every installed server.
    pub const STATUS: &str = "Status";
    /// Discovers how to authenticate to a server that answered 401.
    pub const DETECT_AUTH: &str = "DetectAuth";
    /// Starts an OAuth authorization for a server that requires one.
    pub const OAUTH_BEGIN: &str = "OAuthBegin";

    // -- Registry: tools ----------------------------------------------------

    /// Lists the tools a connected server advertises.
    pub const LIST_TOOLS: &str = "ListTools";
    /// Calls a tool on a connected server.
    pub const TOOL_CALL: &str = "ToolCall";
    /// Prepares the context a host needs to help a user configure a server.
    ///
    /// The module gathers what it knows; running a model over it is host
    /// policy and stays with the host.
    pub const CONFIG_ASSIST: &str = "ConfigAssist";

    // -- Setup --------------------------------------------------------------

    /// Searches the registries from within the guided setup flow.
    pub const SETUP_SEARCH: &str = "SetupSearch";
    /// Fetches one server's detail from within the guided setup flow.
    pub const SETUP_GET: &str = "SetupGet";
    /// Asks the host to collect a secret from the user.
    pub const SETUP_REQUEST_SECRET: &str = "SetupRequestSecret";
    /// Accepts a secret the host collected.
    pub const SETUP_SUBMIT_SECRET: &str = "SetupSubmitSecret";
    /// Dials a server to check the supplied configuration works.
    pub const SETUP_TEST_CONNECTION: &str = "SetupTestConnection";
    /// Installs a configured server and connects it in one step.
    pub const SETUP_INSTALL_AND_CONNECT: &str = "SetupInstallAndConnect";

    // -- Static servers -----------------------------------------------------

    /// Lists the servers the host declared in its own configuration.
    pub const STATIC_LIST: &str = "StaticList";
    /// Lists the tools one statically declared server advertises.
    pub const STATIC_LIST_TOOLS: &str = "StaticListTools";
    /// Calls a tool on a statically declared server.
    pub const STATIC_CALL_TOOL: &str = "StaticCallTool";

    // -- Audit --------------------------------------------------------------

    /// Records one write in the audit log.
    pub const AUDIT_RECORD_WRITE: &str = "AuditRecordWrite";
    /// Lists recorded writes.
    pub const AUDIT_LIST_WRITES: &str = "AuditListWrites";
}

/// Every member of [`INTERFACE`], in the order the interface dispatches them.
///
/// `crates/tinymcp` asserts its declared manifest methods against this list, so
/// a member added to one and not the other fails that crate's tests rather than
/// surfacing as an unknown method in a host at runtime.
pub const METHODS: &[&str] = &[
    // Registry: browsing
    methods::REGISTRY_SEARCH,
    methods::REGISTRY_GET,
    methods::REGISTRY_SETTINGS_GET,
    methods::REGISTRY_SETTINGS_SET,
    // Registry: installs
    methods::INSTALLED_LIST,
    methods::INSTALL,
    methods::UNINSTALL,
    methods::SET_ENABLED,
    methods::UPDATE_ENV,
    // Registry: connections
    methods::CONNECT,
    methods::DISCONNECT,
    methods::STATUS,
    methods::DETECT_AUTH,
    methods::OAUTH_BEGIN,
    // Registry: tools
    methods::LIST_TOOLS,
    methods::TOOL_CALL,
    methods::CONFIG_ASSIST,
    // Setup
    methods::SETUP_SEARCH,
    methods::SETUP_GET,
    methods::SETUP_REQUEST_SECRET,
    methods::SETUP_SUBMIT_SECRET,
    methods::SETUP_TEST_CONNECTION,
    methods::SETUP_INSTALL_AND_CONNECT,
    // Static servers
    methods::STATIC_LIST,
    methods::STATIC_LIST_TOOLS,
    methods::STATIC_CALL_TOOL,
    // Audit
    methods::AUDIT_RECORD_WRITE,
    methods::AUDIT_LIST_WRITES,
];

#[cfg(test)]
mod test;
