//! Media handling: probing, artifacts, thumbnails, archives.
//!
//! Everything here shells out to ffmpeg-family tools rather than linking media
//! libraries. That is a deliberate trade (spec §3.2): the binaries are on every
//! system that has a video player, they stay current, and the alternative --
//! linking libavcodec -- puts a large C dependency in the build graph for a
//! program whose whole premise is a small idle footprint.

pub mod probe;

pub use probe::{probe, MediaInfo, ProbeError, Prober};
