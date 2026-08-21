//! Persistence for the write-audit log.

mod types;

pub use types::AuditStore;

#[cfg(test)]
mod test;
