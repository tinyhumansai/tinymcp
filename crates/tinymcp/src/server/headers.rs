//! Implementation extensions for shared wire vocabulary.

use tinymcp_bus::RequestHeaders;
/// Behavior implemented by the module rather than the contract crate.
pub trait RequestHeadersExt: Sized {
    /// Records a header, unless one of that name is already present.
    fn insert(&mut self, name: &str, value: impl Into<String>);

    /// The value of the named header, if the request carried it.
    #[must_use]
    fn get(&self, name: &str) -> Option<&str>;
}

impl RequestHeadersExt for RequestHeaders {
    /// Records a header, unless one of that name is already present.
    fn insert(&mut self, name: &str, value: impl Into<String>) {
        self.entries
            .entry(name.to_ascii_lowercase())
            .or_insert_with(|| value.into());
    }

    /// The value of the named header, if the request carried it.
    fn get(&self, name: &str) -> Option<&str> {
        self.entries
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }
}
