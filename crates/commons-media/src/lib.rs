//! Media handling: probing, archives, artifacts, thumbnails.
//!
//! Everything here shells out to ffmpeg-family tools rather than linking media
//! libraries. That is a deliberate trade (spec §3.2): the binaries are on every
//! system that has a video player, they stay current, and the alternative --
//! linking libavcodec -- puts a large C dependency in the build graph for a
//! program whose whole premise is a small idle footprint.

pub mod archive;
pub mod archive_io;
pub mod probe;

pub use archive::{validate_member, Listing, Member, MemberKind, Rejection, Validated};
pub use archive_io::{list_zip, ExtractionPlan, Format};
pub use probe::{probe, MediaInfo, ProbeError, Prober};
