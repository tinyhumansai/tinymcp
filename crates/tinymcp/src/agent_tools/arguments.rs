//! Reading a tool call's `arguments` as the object MCP requires.
//!
//! MCP's `tools/call` carries `arguments` as a JSON object. Models do not
//! always send one: some providers JSON-encode a nested object-typed field, so
//! `{"arguments": {}}` arrives as `{"arguments": "{}"}`, sometimes wrapped in a
//! markdown code fence. Rejecting that fails a call whose intent is
//! unambiguous, and a model that retries the same shape can burn a whole turn
//! on it. [`normalize_tool_arguments`] accepts every spelling that carries an
//! object and refuses the rest with a message naming what actually arrived.
//!
//! This is tolerance at execution, not a schema change: the advertised schema
//! still says `object`, so a well-behaved model keeps sending one.

use serde_json::{Map, Value};

use tinymcp_bus::ArgsError;

/// The opening of a markdown code fence.
const FENCE: &str = "```";

/// Reads tool-call arguments as a JSON object.
///
/// - absent or `null` → an empty object, because a tool with no parameters is
///   legitimately called with nothing;
/// - an object → itself;
/// - a string that decodes to an object, optionally inside a markdown code
///   fence (` ```json … ``` `) → the decoded object.
///
/// Takes `impl Into<Option<Value>>` so a caller holding a `Value` and one
/// holding `Option<Value>` (a map lookup) can both call it directly.
///
/// # Errors
///
/// Returns [`ArgsError::NotAnObject`] for a boolean, number, or array, and
/// [`ArgsError::StringNotAnObject`] for a string that is not JSON or that
/// decodes to something other than an object. Both name what arrived, so the
/// message a model reads tells it what to change.
///
/// # Examples
///
/// ```
/// # use serde_json::json;
/// # use tinymcp::agent_tools::normalize_tool_arguments;
/// let decoded = normalize_tool_arguments(json!("{\"city\":\"Paris\"}")).unwrap();
/// assert_eq!(decoded["city"], "Paris");
/// assert!(normalize_tool_arguments(None).unwrap().is_empty());
/// assert!(normalize_tool_arguments(json!([1])).is_err());
/// ```
pub fn normalize_tool_arguments(
    arguments: impl Into<Option<Value>>,
) -> Result<Map<String, Value>, ArgsError> {
    match arguments.into() {
        None | Some(Value::Null) => Ok(Map::new()),
        Some(Value::Object(map)) => Ok(map),
        Some(Value::String(text)) => decode_object(&text),
        Some(other) => Err(ArgsError::NotAnObject {
            actual: json_type_name(&other),
        }),
    }
}

/// Decodes a string that should hold a JSON object.
fn decode_object(text: &str) -> Result<Map<String, Value>, ArgsError> {
    match serde_json::from_str::<Value>(strip_fence(text)) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(other) => Err(ArgsError::StringNotAnObject {
            decoded: Some(json_type_name(&other)),
        }),
        Err(_) => Err(ArgsError::StringNotAnObject { decoded: None }),
    }
}

/// Removes surrounding whitespace and one markdown code fence, if present.
///
/// The fence's language tag is dropped only when it is `json`, in any case;
/// anything else after the opening fence is left for the decoder to refuse.
fn strip_fence(text: &str) -> &str {
    let trimmed = text.trim();
    let Some(inner) = trimmed.strip_prefix(FENCE) else {
        return trimmed;
    };
    let inner = inner.strip_suffix(FENCE).unwrap_or(inner);
    let inner = match inner.get(..4) {
        Some(tag) if tag.eq_ignore_ascii_case("json") => inner.get(4..).unwrap_or_default(),
        _ => inner,
    };
    inner.trim()
}

/// The JSON type of `value`, with its article, as a message reads it.
fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}
