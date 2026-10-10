//! MCP-specific presentation limits; lexical preparation executes outside this contract.

/// Maximum UTF-8 byte length of a model-facing remote tool description.
pub const MAX_DESCRIPTION_BYTES: usize = 1024;
/// Maximum UTF-8 byte length of a model-facing remote tool title.
pub const MAX_TITLE_BYTES: usize = 128;
