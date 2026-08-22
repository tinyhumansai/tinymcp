//! Badging and filtering the browse catalog.
//!
//! The upstream registries list many community servers per popular service and
//! carry no single "official" flag. Collapsing a service to one arbitrary row
//! would hide genuinely different community servers, and across the whole
//! catalog it removes barely any of them. So the full deduplicated catalog stays
//! browsable and the known canonical vendor server is simply *marked*.
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

mod types;

pub use types::{
    OFFICIAL_SERVERS, float_official_first, is_perfect_server, retain_perfect_servers, tag_official,
};

#[cfg(test)]
mod test;
