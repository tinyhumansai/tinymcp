//! What each member of the interface returns.
//!
//! One type per operation whose reply is more than a single value already
//! modelled elsewhere. Operations that answer with a plain
//! [`Vec<InstalledServer>`](crate::InstalledServer) or a
//! [`RegistryServerDetail`](crate::RegistryServerDetail) use that type directly
//! rather than wrapping it in a struct with one field.
//!
//! # These are replies, not envelopes
//!
//! There is no shared success-or-error wrapper. A failure crosses the bus as a
//! failure; these types describe what a *successful* call produced. That keeps
//! a caller from having to unwrap two layers to find out whether anything went
//! wrong, and keeps the failure path typed rather than stringly.

mod types;

pub use types::{
    ConnectOutcome, InstallOutcome, RegistrySearchPage, RegistrySettings, ToolCallOutcome,
    UpdateEnvOutcome, UpdateEnvStatus,
};

#[cfg(test)]
mod test;
