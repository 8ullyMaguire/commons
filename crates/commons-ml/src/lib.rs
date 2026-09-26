//! commons-ml — local machine learning, and nothing else.
//!
//! §6.5 is the constraint that shapes this crate: all inference is local, and
//! what may ever leave the machine is a salted fingerprint, never a byte of
//! the content. There is no HTTP client in this crate's dependency tree, and
//! that is a deliberate property rather than an accident of what has been
//! needed so far — adding one would mean adding a `§6.5` exception, which is
//! a much larger decision than a dependency.
//!
//! Modules:
//!
//! * [`model`] — verified model loading. A model is executable content fetched
//!   over the network, so its SHA-256 is checked before any byte of it is
//!   parsed.
//! * [`face`] — detection and embedding (§7.1 steps 1 and 2).
//! * [`sidecar`] — the on-disk vector index, deliberately not a SQL table.

pub mod face;
pub mod model;
pub mod sidecar;
