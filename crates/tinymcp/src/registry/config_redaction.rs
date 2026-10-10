//! Implementation extensions for shared wire vocabulary.

use tinymcp_bus::McpRegistryAuthConfig;
/// Behavior implemented by the module rather than the contract crate.
pub trait McpRegistryAuthConfigExt: Sized {
    /// Returns a copy with every secret replaced by whether it was set.
    ///
    /// Use this on any path that reports configuration back to a caller. The
    /// non-secret [`McpRegistryAuthConfig::mcp_official_base`] is preserved, because a user who
    /// cannot see which registry they are pointed at cannot debug it.
    ///
    /// # Examples
    ///
    /// ```
    /// # use tinymcp::{McpRegistryAuthConfig, McpRegistryAuthConfigExt};
    /// let mut auth = McpRegistryAuthConfig::default();
    /// auth.smithery_api_key = Some("secret".into());
    ///
    /// let (redacted, smithery_set, _) = auth.redacted();
    /// assert!(smithery_set);
    /// assert_eq!(redacted.smithery_api_key, None);
    /// ```
    #[must_use]
    fn redacted(&self) -> (Self, bool, bool);
}

impl McpRegistryAuthConfigExt for McpRegistryAuthConfig {
    /// Returns a copy with every secret replaced by whether it was set.
    ///
    /// Use this on any path that reports configuration back to a caller. The
    /// non-secret [`McpRegistryAuthConfig::mcp_official_base`] is preserved, because a user who
    /// cannot see which registry they are pointed at cannot debug it.
    ///
    /// # Examples
    ///
    /// ```
    /// # use tinymcp::{McpRegistryAuthConfig, McpRegistryAuthConfigExt};
    /// let mut auth = McpRegistryAuthConfig::default();
    /// auth.smithery_api_key = Some("secret".into());
    ///
    /// let (redacted, smithery_set, _) = auth.redacted();
    /// assert!(smithery_set);
    /// assert_eq!(redacted.smithery_api_key, None);
    /// ```
    fn redacted(&self) -> (Self, bool, bool) {
        let smithery_set = self.smithery_api_key.is_some();
        let official_set = self.mcp_official_token.is_some();
        (
            Self {
                smithery_api_key: None,
                mcp_official_base: self.mcp_official_base.clone(),
                mcp_official_token: None,
            },
            smithery_set,
            official_set,
        )
    }
}
