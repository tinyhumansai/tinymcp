//! The servers a host declares in its own configuration.
//!
//! These are the *static* half of MCP client support: a fixed set the host
//! pins, built once from an [`McpClientConfig`] and never persisted. The
//! *dynamic* half — servers a user installs at runtime, with a store and a
//! supervisor behind them — is the sibling registry.
//!
//! Both halves share the same transports underneath. The difference is who
//! chose the server and whether anything remembers the choice.
//!
//! # Tool permission is fail-closed and pre-transport
//!
//! [`McpServerDefinition::is_tool_allowed`] rejects an empty name, rejects
//! anything on the deny list, and — when an allow list is present — rejects
//! anything not on it. The deny list wins. [`McpServerRegistry::call_tool`]
//! checks before it dials, so a blocked call never reaches the network or a
//! subprocess: it costs nothing, tells the remote nothing, and cannot be
//! half-executed.
//!
//! # What is deliberately not here
//!
//! **No prompt-injection scanning.** A host that runs a detector over tool
//! descriptions applies it to what [`McpServerRegistry::list_tools`] returns.
//! That is host policy: the detector, its rules, and what a hit means all
//! belong to the host's threat model, and a module that silently dropped tools
//! according to its own would be making a decision it cannot explain to anyone.
//! The lexical half — control characters, prompt-template fences, length —
//! *is* applied here, through the display accessors on
//! [`McpRemoteTool`](tinymcp_bus::McpRemoteTool).

mod types;

pub use types::{McpRegistrySource, McpServerDefinition, McpServerRegistry, McpTransportClient};

#[cfg(test)]
mod test;
