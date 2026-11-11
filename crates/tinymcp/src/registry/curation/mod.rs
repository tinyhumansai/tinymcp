//! Badging and filtering the browse catalog.
//!
//! The upstream registries list many community servers per popular service and
//! carry no single "official" flag. Collapsing a service to one arbitrary row
//! would hide genuinely different community servers, and across the whole
//! catalog it removes barely any of them. So the full deduplicated catalog stays
//! browsable and the known canonical vendor server is simply *marked*.
//!
//! # Curated entries say how to reach the server
//!
//! Each entry in [`CURATED_SERVERS`] carries the vendor-hosted endpoint, its
//! transport and how it authenticates, not just a name. That is what lets a
//! local search show a curated server the registry does not list, and an
//! install reach it when the registry cannot be asked. [`OFFICIAL_SERVERS`] is
//! the same list as bare names.
//!
//! # A host can add curated servers at runtime
//!
//! [`CuratedCatalog`] merges the host's
//! [`tinymcp_bus::McpRegistryAuthConfig::curated_servers`] over the built-in
//! list. Runtime entries are listed, searched and described, but never badged
//! or promoted; see the `catalog` module.
//!
//! # Matching is exact, never a substring
//!
//! A term like `stripe` or `github` appears in the name of plenty of unrelated
//! community servers — an Obsidian-GitHub plugin, a forked checkout server. A
//! substring match would put a "verified" badge on servers nobody has vetted,
//! which is worse than no badge at all: it is a claim, made by us, about
//! software we have not looked at.
//!
//! # The trust signals come from the adapter, not the wire
//!
//! [`retain_perfect_servers`] filters on `website_url` and `auth_kind`, and both
//! are `skip_deserializing` on the payload type. An upstream that started
//! emitting those keys could otherwise filter itself into a catalog that
//! promises the user a server is safely installable.

mod catalog;
mod servers;
mod types;

pub use catalog::{CuratedCatalog, entry_detail, entry_summary, validated_runtime_entries};
pub use types::{
    CURATED_SERVERS, CuratedAuth, CuratedServer, CuratedTransport, OFFICIAL_SERVERS,
    curated_server, float_official_first, is_perfect_server, retain_perfect_servers, tag_official,
};

#[cfg(test)]
#[path = "mod_tests.rs"]
mod test;
