//! Implementation extensions for shared wire vocabulary.

use serde_json::{Map, Value, json};
use tinymcp_bus::ServerToolSpec;
/// Behavior implemented by the module rather than the contract crate.
pub trait ServerToolSpecExt: Sized {
    /// The entry as it appears in a `tools/list` result.
    #[must_use]
    fn to_json(&self) -> Value;
}

impl ServerToolSpecExt for ServerToolSpec {
    /// The entry as it appears in a `tools/list` result.
    fn to_json(&self) -> Value {
        let mut entry = Map::new();
        entry.insert("name".to_string(), json!(self.name));
        if let Some(title) = &self.title {
            entry.insert("title".to_string(), json!(title));
        }
        entry.insert("description".to_string(), json!(self.description));
        entry.insert("inputSchema".to_string(), self.input_schema.clone());
        if let Some(annotations) = &self.annotations {
            entry.insert("annotations".to_string(), annotations.clone());
        }
        Value::Object(entry)
    }
}
