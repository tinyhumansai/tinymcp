//! Making a server's argument schema safe to hand a model.

use crate::sanitize::sanitize_for_llm;
use serde_json::{Value, json};

use super::MAX_LLM_BLOCK_BYTES;

/// The longest a description or title inside a schema may be.
const MAX_SCHEMA_TEXT_BYTES: usize = 500;

/// A server's `inputSchema`, ready to declare to a model.
///
/// Every `description` and `title` inside it is untrusted remote text and is
/// sanitized, at any depth. A schema that is not an object becomes the empty
/// object schema, and an object schema missing its `type` gets one: providers
/// reject a function whose parameters are not declared as an object.
#[must_use]
pub fn tool_parameters(input_schema: &Value) -> Value {
    let mut schema = if input_schema.is_object() {
        input_schema.clone()
    } else {
        json!({ "type": "object", "properties": {} })
    };
    if let Value::Object(map) = &mut schema
        && map.get("type") != Some(&json!("object"))
    {
        map.insert("type".to_string(), json!("object"));
    }
    sanitize_schema_text(&mut schema);
    let exceeds_limit = serde_json::to_vec(&schema)
        .map_or(true, |serialized| serialized.len() > MAX_LLM_BLOCK_BYTES);
    if exceeds_limit {
        json!({ "type": "object", "properties": {} })
    } else {
        schema
    }
}

/// Sanitizes every string-valued `description` and `title`, recursively.
fn sanitize_schema_text(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                if (key == "description" || key == "title") && child.is_string() {
                    *child = Value::String(sanitize_for_llm(
                        child.as_str().unwrap_or_default(),
                        MAX_SCHEMA_TEXT_BYTES,
                    ));
                } else {
                    sanitize_schema_text(child);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(sanitize_schema_text),
        _ => {}
    }
}
