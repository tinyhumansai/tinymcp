//! The vocabulary of installed servers: what is installed, how it is dialled,
//! where it stands, and what the upstream registries say about it.
//!
//! # The install record carries no credentials
//!
//! [`InstalledServer`] lists the *names* of the environment variables a server
//! needs and never their values. Install records are listed over RPC, rendered
//! in user interfaces, and written to logs; a credential that is not in the
//! struct cannot escape through any of those.
//!
//! # Two fields are write-only from the adapter's side
//!
//! [`RegistryServerSummary::website_url`] and
//! [`RegistryServerSummary::auth_kind`] are trust signals that decide whether a
//! server passes the strict catalog filter. They are set by the registry
//! adapter from metadata it has verified, and they are marked
//! `skip_deserializing` so an upstream that begins emitting those keys cannot
//! set them itself. That annotation is the whole control, so it is pinned by
//! test.
//!
//! # Naming
//!
//! The upstream DTOs are named `Registry*` rather than `Smithery*`. Two
//! registries produce them — Smithery and the official
//! `modelcontextprotocol/registry` — and only one of them is Smithery. The wire
//! form is unchanged; only the Rust name differs from the code this was
//! extracted from.
//!
//! | This crate | Extracted from |
//! | --- | --- |
//! | [`RegistryServerSummary`] | `SmitheryServerSummary` |
//! | [`RegistryServerDetail`] | `SmitheryServerDetail` |
//! | [`RegistryConnection`] | `SmitheryConnection` |
//! | [`RegistryPagination`] | `SmitheryPagination` |
//! | [`RegistryListResponse`] | `SmitheryListResponse` |

mod types;

pub use types::{
    ChatTurn, CommandKind, ConnStatus, ConnectedServerOverview, ExtraFields, InstalledServer,
    McpAuthHint, McpTool, RegistryConnection, RegistryListResponse, RegistryPagination,
    RegistryServerDetail, RegistryServerSummary, ServerStatus, Transport,
};

#[cfg(test)]
mod test;
