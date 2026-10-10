//! Module-owned argument normalization and stable registry tool declarations.
mod arguments;
pub use arguments::normalize_tool_arguments;
pub use tinymcp_bus::agent_tools::{
    AgentToolEffect, AgentToolSpec, ArgsError, McpCallError, McpCallOutcome, RegistryTool,
    registry_tool_specs,
};
#[cfg(test)]
#[path = "mod_tests.rs"]
mod test;
