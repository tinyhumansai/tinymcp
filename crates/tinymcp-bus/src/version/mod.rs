//! The contract version, and the rule a host uses to decide whether it can bind
//! to a module that reports one.
//!
//! The version describes *this vocabulary*, not the crate: bump the major
//! component when a payload's wire form changes incompatibly or a member is
//! removed or renamed, and the minor component when a member or an optional
//! field is added. It is deliberately independent of the package version the
//! release workflow bumps, which tracks the shipped artifact.

/// The wire contract version this crate defines.
///
/// 1.1 added agent tools; 1.2 added registry and directory members; 1.3 added
/// the structured call outcome and the credential-store error name; 1.4 added
/// the search page's freshness, including answers from the local catalog index,
/// and the registry-timeout error name; 1.5 added tool `_meta`, client
/// capabilities, resource payloads, the result envelope, and the
/// resource-too-large error name; 1.6 added the tool UI presentation
/// vocabulary; 1.7 added serialized supervisor observations and their drain member;
/// 1.8 adds module-owned server sessions and host callback operations;
/// 1.9 adds shared server DTOs and bounded metadata processing operations;
/// 1.10 adds per-directory client configuration through `OpenConfigured`;
/// 1.11 adds explicit directory close and observable lifecycle transitions;
/// 1.12 adds no-dial dynamic and static tool-cache reads; 1.13 adds
/// module-owned HTTP admission and server response metadata; 1.14 adds
/// installed configuration-document read and apply operations; 1.15 adds
/// no-create state lookup and module-prepared tool declarations. The released
/// 1.16 vocabulary is the baseline for this addition; 1.17 adds module-owned
/// scrubbing of credentials from static remote-tool metadata; 1.18 adds safe
/// static-server summaries, 1.19 adds one-shot configured server calls, and
/// 1.20 adds module-owned generic server argument validation. 1.21 adds
/// module-owned MCP identifier normalization to `TransformText`. The former
/// 1.22 Rust-only client lifecycle helpers moved to `tinymcp-client`; this
/// changes no wire vocabulary. 1.23 adds pure registry-answer helpers
/// (`require_identifier`, `ServerDetail::server_json`, `explain_not_connected`) that
/// hosts previously copied; no member was added or removed. 1.24 adds tool
/// `_meta` on `McpTool` and the `ReadResource` and `StaticReadResource`
/// members. 1.25 moves the pure tool-UI rules into `ui`; no member was added
/// or removed. Behaviour change (fail-closed): `http(s)` links with userinfo,
/// IDN/punycode or %-encoded hosts, IPv6, non-canonical IPv4, bad ports or dot
/// segments are now `Blocked` (previously `Web`). 1.26 adds
/// `McpClientConfig::stdio_sandbox` and `McpServerConfig::operator_trusted`:
/// a stdio server runs in the host's jail, or unconfined only when the operator
/// trusts it; with neither it is refused. 1.27 adds
/// `StdioSandboxConfig::enforcement` (`StdioSandboxEnforcement`), letting the
/// host choose `Declared` for a backend with partial enforcement; the default
/// stays `Enforced`.
pub const CONTRACT_VERSION: (u32, u32) = (1, 27);

/// Returns whether a host holding [`CONTRACT_VERSION`] can bind to a module
/// reporting `module`.
///
/// Compatibility is the ordinary semantic-version rule for a pre-release-free
/// contract: the majors must match, and the module must be at least as new as
/// the host, because a host cannot call a member a module does not serve.
///
/// # Examples
///
/// ```
/// # use tinymcp_bus::{is_compatible, CONTRACT_VERSION};
/// assert!(is_compatible(CONTRACT_VERSION));
/// assert!(is_compatible((CONTRACT_VERSION.0, CONTRACT_VERSION.1 + 1)));
/// assert!(!is_compatible((1, 17)));
/// assert!(!is_compatible((1, 14)));
/// assert!(!is_compatible((2, 0)));
/// ```
#[must_use]
pub fn is_compatible(module: (u32, u32)) -> bool {
    binds(CONTRACT_VERSION, module)
}

/// The bind rule with the host version supplied explicitly.
///
/// [`is_compatible`] is this function applied to [`CONTRACT_VERSION`]. It is
/// split out so the unit tests can exercise both directions of the comparison
/// without pinning them to whatever the shipped version happens to be.
fn binds(host: (u32, u32), module: (u32, u32)) -> bool {
    let (host_major, host_minor) = host;
    let (module_major, module_minor) = module;

    module_major == host_major && module_minor >= host_minor
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod test;
