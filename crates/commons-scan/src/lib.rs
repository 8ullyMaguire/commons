//! Scanning: from a path on disk to a typed, segmented object.
//!
//! Three things live here, and the order matters. [`walker`] decides *which
//! files there are*. [`detect`] decides *what a file is* from its bytes.
//! [`segment`] decides *how many objects that file backs* -- the answer to
//! stash#3530, #2276, and #2511, which are three requests for one primitive.

pub mod budget;
pub mod dedup;
pub mod detect;
pub mod funscript;
// T-P6-003: the sampling timeline a player reads, beside the parser it
// samples rather than above it. See the module doc for why this is here
// rather than in `commons-media`.
pub mod funscript_timeline;
pub mod hashing;
pub mod pipeline;
pub mod progress;
pub mod reconcile;
pub mod segment;
pub mod size;
pub mod state;
pub mod types;
pub mod walker;
pub mod watch;

pub use detect::{detect, detect_from_bytes, Container, Detection, Evidence};
pub use funscript::{
    discover, Action, Axis, Funscript, FunscriptError, FunscriptSource, FunscriptWarning,
};
pub use hashing::{
    hash_file, hash_files, hash_reader, FileDisposition, FileHashes, Rehash, BUFFER_BYTES,
};
pub use progress::{Confidence, Progress, Snapshot};
pub use reconcile::{apply, plan, Move, Reconciled, Reconciliation, ScanInput};
pub use segment::{split_file, CompilationRelation, ExistingMarker, Segment, Split, SplitError};
pub use state::{
    actionable_by_default, parse_file_state, partition_for_bulk, BulkOutcome, Lease, LeaseGuard,
    LeaseRegistry, Volume, VolumeProbe, VolumeState, VolumeTracker,
};
pub use types::*;
pub use walker::{
    fs_type, glob_match, policy_for, read_head, walk, walk_resume, Checkpoint, FoundFile, SkipRule,
    VolumeKind, VolumePolicy, WalkConfig, WalkReport, WalkStats, Walker,
};
pub use watch::{Change, ChangeKind, ChangeSink, Debouncer, VolumeWatcher, WatchMode};
