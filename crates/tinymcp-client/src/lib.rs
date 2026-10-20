//! Host-side helpers for driving `TinyMCP` clients over `TinyBus`.
//!
//! This crate contains client lifecycle state and admission bookkeeping. It
//! depends on the contract crate for message types and does not link the MCP
//! module implementation. The public lifecycle helpers are available at
//! [`client_lifecycle`].

pub mod client_lifecycle;

pub use tinymcp_bus::{McpClientConfig, OpenConfiguredResponse, TransitionStatus};
