//! Scanning: from a path on disk to a typed object (T-P1-001+).
//!
//! The first thing a scanner does is decide *what a file is*, and the spec is
//! explicit that this comes from the bytes rather than the name (§5.2). That is
//! [`detect`].

pub mod detect;

pub use detect::{detect, detect_from_bytes, Container, Detection, Evidence};
