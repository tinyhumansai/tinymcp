//! Implementation extensions for shared wire vocabulary.

use super::RequestHeadersExt;
use tinymcp_bus::RequestContext;
/// Behavior implemented by the module rather than the contract crate.
pub trait RequestContextExt: Sized {
    /// The value of the named transport header, if the request carried it.
    #[must_use]
    fn header(&self, name: &str) -> Option<&str>;
}

impl RequestContextExt for RequestContext {
    /// The value of the named transport header, if the request carried it.
    fn header(&self, name: &str) -> Option<&str> {
        self.headers().get(name)
    }
}
