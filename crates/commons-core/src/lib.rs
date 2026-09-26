//! Domain types for Commons.
//!
//! No I/O and no database types live here. This crate is the bottom of the
//! dependency graph: everything else may depend on it, it depends on nothing
//! local. `T-P0-007` enforces that with a layering test.

pub mod domain;
pub mod enums;
mod hashing;

pub use domain::*;
pub use enums::*;
pub use hashing::*;

/// Crate version, surfaced by `/healthz` (§12.6) and the updater (§12.4).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
