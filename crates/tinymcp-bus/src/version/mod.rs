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
/// vocabulary.
pub const CONTRACT_VERSION: (u32, u32) = (1, 6);

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
/// assert!(is_compatible((1, 7)));
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
