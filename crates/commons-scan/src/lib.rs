//! Scanning: from a path on disk to a typed, segmented object.
//!
//! Two things live here, and the order matters. [`detect`] decides *what a
//! file is* from its bytes. [`segment`] decides *how many objects that file
//! backs* -- the answer to stash#3530, #2276, and #2511, which are three
//! requests for one primitive.

pub mod detect;
pub mod funscript;
pub mod segment;
pub mod types;

pub use detect::{detect, detect_from_bytes, Container, Detection, Evidence};
pub use funscript::{
    discover, Action, Axis, Funscript, FunscriptError, FunscriptSource, FunscriptWarning,
};
pub use segment::{split_file, CompilationRelation, ExistingMarker, Segment, Split, SplitError};
pub use types::*;
