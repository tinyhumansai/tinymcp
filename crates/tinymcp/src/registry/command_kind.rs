//! Implementation extensions for shared wire vocabulary.

use tinymcp_bus::CommandKind;
/// Behavior implemented by the module rather than the contract crate.
pub trait CommandKindExt: Sized {
    /// Parses a persisted string, falling back to [`CommandKind::Node`].
    ///
    /// The fallback is deliberate: `npx` is what the overwhelming majority of
    /// registry listings use, so an unrecognised value is far more likely to be
    /// a stale row than a new ecosystem.
    ///
    /// # Examples
    ///
    /// ```
    /// # use tinymcp::{CommandKind, CommandKindExt};
    /// assert_eq!(CommandKind::parse("python"), CommandKind::Python);
    /// assert_eq!(CommandKind::parse("nonsense"), CommandKind::Node);
    /// ```
    #[must_use]
    fn parse(raw: &str) -> Self;
}

impl CommandKindExt for CommandKind {
    /// Parses a persisted string, falling back to [`CommandKind::Node`].
    ///
    /// The fallback is deliberate: `npx` is what the overwhelming majority of
    /// registry listings use, so an unrecognised value is far more likely to be
    /// a stale row than a new ecosystem.
    ///
    /// # Examples
    ///
    /// ```
    /// # use tinymcp::{CommandKind, CommandKindExt};
    /// assert_eq!(CommandKind::parse("python"), CommandKind::Python);
    /// assert_eq!(CommandKind::parse("nonsense"), CommandKind::Node);
    /// ```
    fn parse(raw: &str) -> Self {
        match raw {
            "python" => Self::Python,
            "binary" => Self::Binary,
            _ => CommandKind::Node,
        }
    }
}
