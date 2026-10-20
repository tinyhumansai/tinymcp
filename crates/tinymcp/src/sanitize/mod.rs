//! Shared lexical preparation is owned by `TinyTools`; MCP retains its presentation caps.
pub use tinymcp_bus::{MAX_DESCRIPTION_BYTES, MAX_TITLE_BYTES};
pub use tinytools::sanitize::{
    sanitize_for_llm, strip_control_chars, strip_instruction_fences, truncate_utf8_safe,
};
