//! The canonical-server list and the catalog filters.

use tinymcp_bus::RegistryServerSummary;

/// Canonical first-party servers, by exact registry qualified name.
///
/// Each was confirmed present in the official registry export. These get the
/// badge; every other server is shown without one. Extend the list as vendors
/// publish official servers — and only ever with a name checked against the
/// registry, since an entry here is a claim made to the user.
pub const OFFICIAL_SERVERS: &[&str] = &[
    "io.github.github/github-mcp-server",
    "com.notion/mcp",
    "com.stripe/mcp",
    "com.atlassian/atlassian-mcp-server",
    "app.linear/linear",
    "com.gitlab/mcp",
    "com.paypal.mcp/mcp",
    "com.cloudflare.mcp/mcp",
    "com.airtable/mcp",
    "com.supabase/mcp",
    "com.vercel/vercel-mcp",
    "com.webflow/mcp",
    "com.wix/mcp",
];

/// Marks the canonical first-party server for each known service.
///
/// Sets the badge on an exact qualified-name match and clears it otherwise, so
/// a row arriving with the flag already set cannot keep it. See the module note
/// on why the match is never a substring.
///
/// # Examples
///
/// ```
/// # use tinymcp::registry::curation::tag_official;
/// # use tinymcp_bus::RegistryServerSummary;
/// let mut servers: Vec<RegistryServerSummary> = serde_json::from_value(serde_json::json!([
///     { "qualified_name": "com.notion/mcp", "display_name": "Notion" },
///     { "qualified_name": "ai.smithery/smithery-notion", "display_name": "Notion-ish" },
/// ]))?;
///
/// tag_official(&mut servers);
///
/// assert!(servers[0].official);
/// assert!(!servers[1].official, "merely containing 'notion' is not official");
/// # Ok::<(), serde_json::Error>(())
/// ```
pub fn tag_official(servers: &mut [RegistryServerSummary]) {
    for server in servers.iter_mut() {
        server.official = OFFICIAL_SERVERS.contains(&server.qualified_name.as_str());
    }
}

/// Whether a row says enough, from its metadata alone, to be installed and
/// connected without guessing.
///
/// That means a non-blank vendor website — the user's destination for getting a
/// key, and a signal somebody stands behind the server — and a declared static
/// credential.
#[must_use]
pub fn is_perfect_server(server: &RegistryServerSummary) -> bool {
    server
        .website_url
        .as_deref()
        .is_some_and(|url| !url.trim().is_empty())
        && server.auth_kind.as_deref() == Some("api_key")
}

/// Keeps only the rows [`is_perfect_server`] accepts, returning how many went.
///
/// This drops OAuth-only, open, and under-declared servers. It is a deliberate
/// quality-over-quantity trade: a user browsing the catalog only sees servers
/// that can be installed and connected with confidence, rather than a longer
/// list where some fraction will fail in ways they cannot diagnose.
///
/// The count is returned so a caller can log what was trimmed. A filter that
/// silently removes most of a catalog reads as "there is nothing here".
pub fn retain_perfect_servers(servers: &mut Vec<RegistryServerSummary>) -> usize {
    let before = servers.len();
    servers.retain(is_perfect_server);
    before - servers.len()
}

/// Floats the badged servers to the top, keeping relevance order below.
///
/// A stable sort, so everything that is not badged stays in the order the
/// upstream ranked it.
pub fn float_official_first(servers: &mut [RegistryServerSummary]) {
    servers.sort_by_key(|server| !server.official);
}
