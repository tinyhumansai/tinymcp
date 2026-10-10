//! Implementation extensions for shared wire vocabulary.

use tinymcp_bus::{DEFAULT_LIST_LIMIT, MAX_LIST_LIMIT, McpWriteListQuery};
/// Behavior implemented by the module rather than the contract crate.
pub trait McpWriteListQueryExt: Sized {
    /// The page size this query resolves to, clamped to [`MAX_LIST_LIMIT`].
    ///
    /// # Examples
    ///
    /// ```
    /// # use tinymcp::{McpWriteListQuery, McpWriteListQueryExt, MAX_LIST_LIMIT, DEFAULT_LIST_LIMIT};
    /// assert_eq!(McpWriteListQuery::default().resolved_limit(), DEFAULT_LIST_LIMIT);
    ///
    /// let mut query = McpWriteListQuery::default();
    /// query.limit = Some(10_000);
    /// assert_eq!(query.resolved_limit(), MAX_LIST_LIMIT);
    /// ```
    #[must_use]
    fn resolved_limit(&self) -> u64;

    /// The offset this query resolves to.
    #[must_use]
    fn resolved_offset(&self) -> u64;

    /// The client filter, trimmed, or `None` when it is absent or blank.
    ///
    /// A whitespace-only filter is treated as no filter rather than as a filter
    /// that matches nothing — a caller sending an empty text box wants
    /// everything, not silence.
    ///
    /// # Examples
    ///
    /// ```
    /// # use tinymcp::{McpWriteListQuery, McpWriteListQueryExt};
    /// let mut query = McpWriteListQuery::default();
    /// query.client_filter = Some("  claude  ".into());
    /// assert_eq!(query.resolved_client_filter(), Some("claude"));
    ///
    /// query.client_filter = Some("   ".into());
    /// assert_eq!(query.resolved_client_filter(), None);
    /// ```
    #[must_use]
    fn resolved_client_filter(&self) -> Option<&str>;

    /// The tool filter, trimmed, or `None` when it is absent or blank.
    ///
    /// The same rule as [`Self::resolved_client_filter`].
    #[must_use]
    fn resolved_tool_filter(&self) -> Option<&str>;

    /// Whether to return successful writes only.
    ///
    /// Both `None` and `Some(false)` mean no filter.
    #[must_use]
    fn resolved_success_only(&self) -> bool;
}

impl McpWriteListQueryExt for McpWriteListQuery {
    /// The page size this query resolves to, clamped to [`MAX_LIST_LIMIT`].
    ///
    /// # Examples
    ///
    /// ```
    /// # use tinymcp::{McpWriteListQuery, McpWriteListQueryExt, MAX_LIST_LIMIT, DEFAULT_LIST_LIMIT};
    /// assert_eq!(McpWriteListQuery::default().resolved_limit(), DEFAULT_LIST_LIMIT);
    ///
    /// let mut query = McpWriteListQuery::default();
    /// query.limit = Some(10_000);
    /// assert_eq!(query.resolved_limit(), MAX_LIST_LIMIT);
    /// ```
    fn resolved_limit(&self) -> u64 {
        self.limit.unwrap_or(DEFAULT_LIST_LIMIT).min(MAX_LIST_LIMIT)
    }

    /// The offset this query resolves to.
    fn resolved_offset(&self) -> u64 {
        self.offset.unwrap_or(0)
    }

    /// The client filter, trimmed, or `None` when it is absent or blank.
    ///
    /// A whitespace-only filter is treated as no filter rather than as a filter
    /// that matches nothing — a caller sending an empty text box wants
    /// everything, not silence.
    ///
    /// # Examples
    ///
    /// ```
    /// # use tinymcp::{McpWriteListQuery, McpWriteListQueryExt};
    /// let mut query = McpWriteListQuery::default();
    /// query.client_filter = Some("  claude  ".into());
    /// assert_eq!(query.resolved_client_filter(), Some("claude"));
    ///
    /// query.client_filter = Some("   ".into());
    /// assert_eq!(query.resolved_client_filter(), None);
    /// ```
    fn resolved_client_filter(&self) -> Option<&str> {
        normalized_filter(self.client_filter.as_deref())
    }

    /// The tool filter, trimmed, or `None` when it is absent or blank.
    ///
    /// The same rule as [`Self::resolved_client_filter`].
    fn resolved_tool_filter(&self) -> Option<&str> {
        normalized_filter(self.tool_filter.as_deref())
    }

    /// Whether to return successful writes only.
    ///
    /// Both `None` and `Some(false)` mean no filter.
    fn resolved_success_only(&self) -> bool {
        self.success_only.unwrap_or(false)
    }
}

/// Trims a filter and treats a blank one as absent.
fn normalized_filter(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}
