//! Scanning: from a path on disk to a typed, segmented object.
//!
//! Three things live here, and the order matters. [`walker`] decides *which
//! files there are*. [`detect`] decides *what a file is* from its bytes.
//! [`segment`] decides *how many objects that file backs* -- the answer to
//! stash#3530, #2276, and #2511, which are three requests for one primitive.

pub mod detect;
pub mod funscript;
pub mod progress;
pub mod segment;
pub mod types;
pub mod walker;
pub mod watch;

pub use detect::{detect, detect_from_bytes, Container, Detection, Evidence};
pub use funscript::{
    discover, Action, Axis, Funscript, FunscriptError, FunscriptSource, FunscriptWarning,
};
pub use progress::{Confidence, Progress, Snapshot};
pub use segment::{split_file, CompilationRelation, ExistingMarker, Segment, Split, SplitError};
pub use types::*;
pub use walker::{
    fs_type, glob_match, policy_for, read_head, walk, walk_resume, Checkpoint, FoundFile, SkipRule,
    VolumeKind, VolumePolicy, WalkConfig, WalkReport, WalkStats, Walker,
};
pub use watch::{Change, ChangeKind, ChangeSink, Debouncer, VolumeWatcher, WatchMode};
