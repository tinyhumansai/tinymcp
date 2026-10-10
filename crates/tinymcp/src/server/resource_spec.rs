//! Implementation extensions for shared wire vocabulary.

use serde_json::{Map, Value, json};
use tinymcp_bus::ResourceSpec;
/// Behavior implemented by the module rather than the contract crate.
pub trait ResourceSpecExt: Sized {
    /// The entry as it appears in a `resources/list` result.
    #[must_use]
    fn to_json(&self) -> Value;
}

impl ResourceSpecExt for ResourceSpec {
    /// The entry as it appears in a `resources/list` result.
    fn to_json(&self) -> Value {
        let mut entry = Map::new();
        entry.insert("uri".to_string(), json!(self.uri));
        entry.insert("name".to_string(), json!(self.name));
        if let Some(description) = &self.description {
            entry.insert("description".to_string(), json!(description));
        }
        if let Some(mime_type) = &self.mime_type {
            entry.insert("mimeType".to_string(), json!(mime_type));
        }
        Value::Object(entry)
    }
}
