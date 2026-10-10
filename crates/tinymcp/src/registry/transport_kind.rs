//! Implementation extensions for shared wire vocabulary.

use tinymcp_bus::Transport;
/// Behavior implemented by the module rather than the contract crate.
pub trait TransportExt: Sized {
    /// The inverse of [`Transport::dispatch_kind`], for re-hydrating a stored row.
    ///
    /// An unknown or empty kind becomes [`Transport::Stdio`]. That is the
    /// migration-safety hatch: rows written before the column existed were all
    /// stdio installs, and a misconfigured row should stall on connect rather
    /// than get misrouted to a transport it was never meant for.
    ///
    /// # Examples
    ///
    /// ```
    /// # use tinymcp::{Transport, TransportExt};
    /// assert_eq!(Transport::parse("", None), Transport::Stdio);
    /// assert_eq!(
    ///     Transport::parse("http_remote", Some("https://x.test/mcp")),
    ///     Transport::HttpRemote { url: "https://x.test/mcp".into() },
    /// );
    /// ```
    #[must_use]
    fn parse(kind: &str, deployment_url: Option<&str>) -> Self;
}

impl TransportExt for Transport {
    /// The inverse of [`Transport::dispatch_kind`], for re-hydrating a stored row.
    ///
    /// An unknown or empty kind becomes [`Transport::Stdio`]. That is the
    /// migration-safety hatch: rows written before the column existed were all
    /// stdio installs, and a misconfigured row should stall on connect rather
    /// than get misrouted to a transport it was never meant for.
    ///
    /// # Examples
    ///
    /// ```
    /// # use tinymcp::{Transport, TransportExt};
    /// assert_eq!(Transport::parse("", None), Transport::Stdio);
    /// assert_eq!(
    ///     Transport::parse("http_remote", Some("https://x.test/mcp")),
    ///     Transport::HttpRemote { url: "https://x.test/mcp".into() },
    /// );
    /// ```
    fn parse(kind: &str, deployment_url: Option<&str>) -> Self {
        match kind {
            "http_remote" => Self::HttpRemote {
                url: deployment_url.unwrap_or_default().to_string(),
            },
            _ => Transport::Stdio,
        }
    }
}
