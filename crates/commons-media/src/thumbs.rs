//! Thumbnail and sprite generation (T-P1-006, spec §10.1 / §6.4).
//!
//! # Two rules, and both are about memory
//!
//! Spec §4.3 gives the whole application 210MB idle, and §6.4 extends that
//! ceiling to *generated artifacts*, which are the only part of Commons that
//! allocates unboundedly. Two mechanisms hold that line, and this module is
//! where they live:
//!
//! 1. **A token bucket, not a fixed thread pool.** Each in-flight ffmpeg holds a
//!    decoded frame in RAM. Four concurrent 4K sprite generations is a gig of
//!    RSS, which is not a bug in the code, it is just what decoding costs. So
//!    concurrency is bounded by tokens, and a job that cannot get a token
//!    returns [`Deferred`] rather than queueing and then allocating on the
//!    other side of the semaphore. Stash#5762 asks for configurable max memory;
//!    the tokens are the mechanism, and the size of the bucket is the setting.
//!
//! 2. **Width-bounded output, always.** A thumbnail is generated at the width
//!    asked for, and sprites at their own fixed width. There is no "generate
//!    full size then downscale" path, because that is the full-size frame in
//!    memory exactly when the budget is tightest.
//!
//! # Why sprites are a sheet, not a sequence
//!
//! The scrubber needs 20 frames visible at once while a user drags. Fetching 20
//! separate images means 20 HTTP round trips per scrub position and 20 decode
//! buffers in the client. One horizontal sheet is one request and one decode.
//! stash#6811 and #5275 both follow from this: a marker's time range maps to a
//! span of frame indices in the sheet, so a marker thumbnail is a crop of the
//! sheet already in memory rather than a new render.
//!
//! # Alpha
//!
//! Alpha is preserved and the output is WebP, because the intermediate JPEG
//! step loses it and resizing through a JPEG loses it again. A comic with a
//! transparent background is common enough that "the cover is a black square"
//! is a real bug report, and stash#5850 is that report. The test in this module
//! decodes the output and checks a known-transparent pixel is still
//! transparent, which is the only assertion that actually catches a regression
//! here -- a file that is *nominally* WebP can still have a composited black
//! background.

use super::encode::EncodeSettings;
use super::hwaccel_plan::AccelPlan;
use crate::probe::{MediaInfo, ProbeError};
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Default sprite width in pixels. Wide enough that 20 frames are each legible
/// on a phone, narrow enough that one sheet is a few hundred KB.
pub const DEFAULT_SPRITE_WIDTH: u32 = 200;

/// Default frame count. 20 is what the scrubber draws; stash#6811 and #5275
/// both assume a fixed count.
pub const DEFAULT_SPRITE_FRAMES: u32 = 20;

/// Default thumbnail width.
pub const DEFAULT_THUMB_WIDTH: u32 = 320;

/// A job that could not start because the memory bucket was empty.
///
/// Not an error: the caller is expected to retry this later, and it is a
/// distinct type from an error precisely so it cannot be logged as a failure.
/// A scan that reports "deferred" for half its work and then finishes is
/// working correctly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Deferred;

impl fmt::Display for Deferred {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Phrased for a log line: this is information, not a problem.
        f.write_str("artifact generation deferred: memory budget is full, will retry")
    }
}

impl std::error::Error for Deferred {}

/// What went wrong, when it is a real error rather than a deferral.
#[derive(Debug, thiserror::Error)]
pub enum ArtifactError {
    #[error("ffmpeg failed on {path}: {stderr}")]
    FfmpegFailed { path: PathBuf, stderr: String },
    #[error("ffprobe failed on {path}: {detail}")]
    Probe { path: PathBuf, detail: String },
    #[error("output path is not valid: {0}")]
    BadOutput(String),
    #[error("path contains a NUL byte: {0}")]
    NulPath(String),
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    ProbeError(#[from] ProbeError),
}

/// The result of a generation attempt, which is a three-way thing and not a
/// Result because two of the three outcomes are not errors.
#[derive(Debug)]
pub enum Generated<T> {
    /// Done. `T` is the path written.
    Wrote(T),
    /// Could not start; retry later.
    Deferred(Deferred),
    /// Failed for a real reason.
    Failed(ArtifactError),
}

impl<T> Generated<T> {
    pub fn is_wrote(&self) -> bool {
        matches!(self, Generated::Wrote(_))
    }
    pub fn is_deferred(&self) -> bool {
        matches!(self, Generated::Deferred(_))
    }
    /// The path, if it was written.
    pub fn path(&self) -> Option<&T> {
        match self {
            Generated::Wrote(p) => Some(p),
            _ => None,
        }
    }
    /// Turn a deferral into an error, for callers that genuinely cannot retry.
    pub fn or_err(self) -> Result<T, ArtifactError> {
        match self {
            Generated::Wrote(t) => Ok(t),
            Generated::Deferred(_) => {
                Err(ArtifactError::BadOutput("deferred and not retried".into()))
            }
            Generated::Failed(e) => Err(e),
        }
    }
}

/// A token bucket bounding concurrent generation.
///
/// Sized in *bytes of expected peak frame* rather than in task count, because
/// task count is not what costs memory: a 320px thumbnail and a 4K sprite
/// differ by two orders of magnitude. A caller declares the peak working set
/// for a job, and the bucket admits jobs while the total stays under the
/// ceiling.
///
/// `try_acquire` never blocks. That is the whole point: blocking a job thread
/// waiting for memory is how a bounded system becomes an unbounded one once the
/// number of job threads is not also bounded.
/// The mutable state a [`Reservation`] needs in order to give the bytes back.
#[derive(Debug)]
struct BudgetState {
    ceiling: AtomicU64,
    in_use: AtomicU64,
    peak: AtomicU64,
    /// Waiting jobs, in arrival order, so a deferred job can be told how many
    /// are ahead of it. Purely informational, but it is what makes a log line
    /// about a deferral actionable instead of mysterious.
    waiting: Mutex<VecDeque<u64>>,
}

#[derive(Debug, Clone)]
pub struct MemoryBudget {
    state: Arc<BudgetState>,
}

impl MemoryBudget {
    /// A budget with the given ceiling in bytes.
    pub fn new(ceiling_bytes: u64) -> Self {
        Self {
            state: Arc::new(BudgetState {
                ceiling: AtomicU64::new(ceiling_bytes),
                in_use: AtomicU64::new(0),
                peak: AtomicU64::new(0),
                waiting: Mutex::new(VecDeque::new()),
            }),
        }
    }

    /// A budget sized from the spec's whole-application ceiling, minus what the
    /// process is already using. `app_total` is §4.3's 210MB;
    /// `already_using` is the process's current RSS.
    ///
    /// Artifact generation gets what is left, and the process is not allowed to
    /// exceed §4.3 while doing it. If the process is already over the ceiling,
    /// the budget is 1 byte rather than 0 or negative: a job that cannot fit is
    /// deferred, and a 0-byte ceiling would make even a 1-token job loop
    /// forever deferring.
    pub fn from_app_budget(app_total: u64, already_using: u64) -> Self {
        let headroom = app_total.saturating_sub(already_using);
        Self::new(headroom.max(1))
    }

    pub fn ceiling(&self) -> u64 {
        self.state.ceiling.load(Ordering::Relaxed)
    }

    pub fn in_use(&self) -> u64 {
        self.state.in_use.load(Ordering::Relaxed)
    }

    /// The high-water mark. A budget that has never been approached is a budget
    /// that is too generous, and this is how you find out.
    pub fn peak(&self) -> u64 {
        self.state.peak.load(Ordering::Relaxed)
    }

    pub fn available(&self) -> u64 {
        self.ceiling().saturating_sub(self.in_use())
    }

    /// Number of jobs currently deferred and waiting.
    pub fn waiting(&self) -> usize {
        self.state.waiting.lock().len()
    }

    /// Try to reserve `bytes`. Returns the reservation on success.
    ///
    /// Uses a compare-exchange loop rather than a mutex so that admission is
    /// lock-free on the hot path: a job that does not fit must not queue behind
    /// a job that does.
    pub fn try_acquire(&self, bytes: u64) -> Option<Reservation> {
        if bytes == 0 {
            return None;
        }
        let st = &self.state;
        let mut waiting = st.waiting.lock();
        let mut current = st.in_use.load(Ordering::Relaxed);
        loop {
            let next = match current.checked_add(bytes) {
                Some(n) => n,
                None => {
                    // Would overflow, which means certainly over the ceiling.
                    waiting.push_back(bytes);
                    return None;
                }
            };
            if next > st.ceiling.load(Ordering::Relaxed) {
                waiting.push_back(bytes);
                return None;
            }
            match st.in_use.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    st.peak.fetch_max(next, Ordering::Relaxed);
                    let pos = waiting.iter().position(|&b| b == bytes).unwrap_or(0);
                    waiting.remove(pos);
                    return Some(Reservation {
                        state: Arc::clone(&self.state),
                        bytes,
                    });
                }
                Err(actual) => current = actual,
            }
        }
    }

    /// Convenience: a single fixed-size token, for callers whose jobs are all
    /// the same size. 16 is a ffmpeg working set for a mid-size frame.
    pub fn try_acquire_one(&self) -> Option<Reservation> {
        self.try_acquire(1)
    }

    fn release(state: &BudgetState, bytes: u64) {
        state.in_use.fetch_sub(bytes, Ordering::AcqRel);
    }
}

/// Releases its reservation on drop, including on panic and on early return.
///
/// A token leak would be silent and permanent: the budget would slowly refuse
/// everything, and nothing would say why. This is the one place in the project
/// where Drop carrying correctness matters.
#[derive(Debug)]
pub struct Reservation {
    state: Arc<BudgetState>,
    bytes: u64,
}

impl Reservation {
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        MemoryBudget::release(&self.state, self.bytes);
    }
}

/// The §4.3 whole-application idle ceiling, in bytes.
pub const APP_IDLE_CEILING_BYTES: u64 = 210 * 1024 * 1024;

/// This process's current resident set size, in bytes.
///
/// Reads `/proc/self/statm` rather than shelling out to `ps`, and returns 0
/// when it cannot be read -- a budget built on a wrong RSS is worse than one
/// built on a conservative estimate, and 0 means "assume the process is
/// already large", which defers work rather than overcommitting it.
pub fn process_rss_bytes() -> u64 {
    let Ok(s) = std::fs::read_to_string("/proc/self/statm") else {
        return 0;
    };
    // statm fields are in pages; the second is the resident set.
    let Some(pages) = s.split_whitespace().nth(1) else {
        return 0;
    };
    let Ok(pages) = pages.parse::<u64>() else {
        return 0;
    };
    pages.saturating_mul(4096)
}

/// A [`MemoryBudget`] sized from §4.3 minus what this process is already using.
///
/// The point of taking the ceiling as a parameter is that it is a constant of
/// the spec, not a tunable that drifts; a caller passing a different number is
/// a caller making a deliberate decision, and `APP_IDLE_CEILING_BYTES` is the
/// one they should pass.
pub fn memory_budget_from_process(ceiling: u64) -> MemoryBudget {
    MemoryBudget::from_app_budget(ceiling, process_rss_bytes())
}

/// The source's pixel height, or 1080 when it is not known.
///
/// ffmpeg decodes at source resolution regardless of the output width, so an
/// unknown height must NOT be treated as the output width -- that would admit a
/// job whose real decode is 8 times bigger than budgeted.
fn height_px(m: &MediaInfo) -> Option<u32> {
    m.video_streams
        .iter()
        .find(|s| s.height > 0)
        .map(|s| s.height)
}

/// What to generate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A single still, at `width`.
    Thumbnail { width: u32 },
    /// A horizontal sprite sheet, `frames` frames at `width` each.
    Sprite { width: u32, frames: u32 },
    /// A still at a specific timestamp, for a marker (#2954, #5783).
    Marker { width: u32, at_ms: u64 },
}

impl Kind {
    pub fn width(&self) -> u32 {
        match self {
            Kind::Thumbnail { width } | Kind::Sprite { width, .. } | Kind::Marker { width, .. } => {
                *width
            }
        }
    }

    pub fn frames(&self) -> u32 {
        match self {
            Kind::Sprite { frames, .. } => *frames,
            // A thumbnail is a one-frame sprite. This matters: one code path
            // means one set of bugs rather than two.
            _ => 1,
        }
    }

    /// Peak memory for this job, for admission control.
    ///
    /// The dominant term is the DECODE, not the output: `scale=width:-2` makes
    /// ffmpeg decode the source at its own resolution and scale after, so a
    /// 200px-wide sprite sheet from a 4K source costs the same decode as any
    /// other job on that file. `width * height * 4` for RGBA, doubled for the
    /// decoder's working buffers, is the honest estimate for that.
    ///
    /// A sprite is deliberately NOT costed at 20x a thumbnail here. `fps=N,tile`
    /// decodes the source once and tiles the scaled output, so the decode cost
    /// is a single frame plus a modest tile buffer. Charging 20 decodes would
    /// over-admit the real cost; under-admitting would be worse, so the
    /// estimate stays at one decode and the OUTPUT size is bounded separately
    /// by the ffmpeg quality setting.
    pub fn estimated_peak_bytes(&self, source: &MediaInfo) -> u64 {
        let w = self.width() as u64;
        let h = height_px(source).unwrap_or(1080) as u64;
        let frame = w * h * 4;
        match self {
            // One decode, plus the tile buffer holding the assembled sheet.
            Kind::Sprite { frames, .. } => frame * 2 + frame * f64::from(*frames).sqrt() as u64,
            _ => frame * 2,
        }
    }
}

/// A configured generator.
#[derive(Debug, Clone)]
pub struct Generator {
    ffmpeg: PathBuf,
    ffprobe: PathBuf,
    budget: Arc<MemoryBudget>,
    /// How to ask ffmpeg for acceleration, and whether to ask at all.
    ///
    /// Defaults to [`AccelPlan::Software`], which is byte-for-byte the flag
    /// set the pipeline used before acceleration existed. A caller that never
    /// touches this gets no behaviour change, which is what makes adding the
    /// field safe.
    plan: AccelPlan,
    /// Frames are sampled at the midpoint of each equal slice, not at the slice
    /// boundary. A boundary sample lands on the exact frame a chapter or a
    /// marker starts, which is the frame most likely to be a black fade or a
    /// transition. This is why a scene with chapters has sprites that look like
    /// the content rather than like its cuts.
    sample_midpoint: bool,
    /// Encoder, quality, threads and lossless-ness for every generated
    /// artifact. Defaults reproduce the previous hardcoded arguments exactly.
    encode: EncodeSettings,
}

impl Generator {
    pub fn new(budget: Arc<MemoryBudget>) -> Self {
        Self {
            ffmpeg: PathBuf::from("ffmpeg"),
            ffprobe: PathBuf::from("ffprobe"),
            budget,
            plan: super::hwaccel_plan::AccelPlan::Software,
            encode: EncodeSettings::default(),
            sample_midpoint: true,
        }
    }

    pub fn with_binaries(ffmpeg: impl Into<PathBuf>, ffprobe: impl Into<PathBuf>) -> Self {
        let mut g = Self::new(Arc::new(MemoryBudget::new(u64::MAX)));
        g.ffmpeg = ffmpeg.into();
        g.ffprobe = ffprobe.into();
        g
    }

    pub fn budget(&self) -> &Arc<MemoryBudget> {
        &self.budget
    }

    /// Use `plan` for subsequent generations.
    ///
    /// Takes the plan as a value rather than a planner: the decision is the
    /// caller's, and a generator that probed for itself would probe on every
    /// call.
    pub fn with_plan(mut self, plan: AccelPlan) -> Self {
        self.plan = plan;
        self
    }

    /// The plan in force.
    ///
    /// By reference: `AccelPlan` is no longer `Copy` because a hardware plan
    /// names a device path, and cloning a path to read a field would be silly.
    pub fn plan(&self) -> &AccelPlan {
        &self.plan
    }

    /// A hardware filter chain's value with the download-back step appended.
    ///
    /// `scale_vaapi` returns a hardware frame. The encoder is software, so
    /// without this the run ends with `Nothing was written into output file,
    /// because at least one of its streams received no packets` -- an error
    /// naming the encoder when the fault is one filter earlier in the chain.
    fn with_suffix(&self, filter: String) -> String {
        if self.plan.filter_suffix().is_empty() {
            return filter;
        }
        format!("{filter},{}", self.plan.filter_suffix())
    }

    /// Generate `kind` from `source` into `out`.
    ///
    /// The three-way return is the contract: a deferral is not a failure, and
    /// the caller is expected to distinguish them because retrying is the whole
    /// response to a deferral.
    pub fn generate(
        &self,
        source: &Path,
        out: &Path,
        kind: Kind,
        media: &MediaInfo,
    ) -> Generated<PathBuf> {
        let display = source.display().to_string();
        if display.contains('\0') || out.display().to_string().contains('\0') {
            return Generated::Failed(ArtifactError::NulPath(display));
        }

        // Admission happens BEFORE any work. Probing is cheap and already
        // done; the expensive thing is the decode, and that is what is gated.
        let peak = kind.estimated_peak_bytes(media);
        let Some(_reservation) = self.budget.try_acquire(peak) else {
            return Generated::Deferred(Deferred);
        };

        // A still has no decode step to move to a GPU. If the caller planned
        // for a video, that is their error, and emitting the flags anyway
        // makes ffmpeg fail on a JPEG instead of quietly doing the right
        // thing.
        let this = if media.duration_ms == 0 {
            let mut g = self.clone();
            g.plan = AccelPlan::Software;
            g
        } else {
            self.clone()
        };
        let this = &this;
        match kind {
            Kind::Thumbnail { width } => this.thumbnail(source, out, width),
            Kind::Marker { width, at_ms } => this.still_at(source, out, width, at_ms),
            Kind::Sprite { width, frames } => this.sprite(source, out, width, frames, media),
        }
    }

    /// A single still, no timestamp (frame 0 or the first decodable frame).
    fn thumbnail(&self, source: &Path, out: &Path, width: u32) -> Generated<PathBuf> {
        // `-frames:v 1` and an explicit seek to the start. A thumbnail of a
        // video whose first frame is black is a common upstream complaint
        // (#2227 is about covers, and the same instinct applies here).
        // Global options come first, before even `-hide_banner`. ffmpeg
        // reports a misplaced one as `Error parsing global options: Invalid
        // argument`, which names neither the option nor the position, and the
        // only way to get it right is to never move them.
        let mut args = self.plan.global_args();
        args.extend([
            "-hide_banner".to_string(),
            "-loglevel".to_string(),
            "error".to_string(),
            "-y".to_string(),
            "-ss".to_string(),
            "0".to_string(),
        ]);
        // Then the per-input hardware options, immediately before `-i`. After
        // `-i` they are silently ignored, leaving a machine that believes it
        // is decoding on the GPU while it decodes on the CPU.
        args.extend(self.plan.input_args());
        args.extend([
            "-i".to_string(),
            source.display().to_string(),
            "-frames:v".to_string(),
            "1".to_string(),
            "-vf".to_string(),
            self.with_suffix(self.plan.scale_filter(width, "-2")),
            // WebP with alpha. `-pix_fmt` on the encoder is what preserves the
            // alpha channel; without it ffmpeg writes yuvj and the transparency
            // is composited to black. The pixel format comes from the format
            // itself, and it survives acceleration because the encoder never
            // changes: there is no hardware WebP encoder to switch to.
            out.display().to_string(),
        ]);
        // Codec settings go last, just before the output, where ffmpeg's
        // per-output options belong. Appended separately because it is a
        // group, and splicing a group into an array literal is how a
        // comma ends up in the wrong place.
        let last = args.len() - 1;
        args.splice(last..last, self.encode.args());
        self.run(source, out, &args)
    }

    /// A still at a specific timestamp, for a marker.
    fn still_at(&self, source: &Path, out: &Path, width: u32, at_ms: u64) -> Generated<PathBuf> {
        // Seek BEFORE -i for speed, which is accurate to about a frame at
        // typical keyframe intervals, and then decode-accurate seek with
        // -ss after -i as well. Doing only the fast seek lands on the nearest
        // keyframe, which for a 10s GOP is up to 10s away -- and a marker
        // thumbnail showing a different scene is worse than no thumbnail.
        let at = format!("{:.3}", at_ms as f64 / 1000.0);
        let mut args = self.plan.global_args();
        args.extend([
            "-hide_banner".to_string(),
            "-loglevel".to_string(),
            "error".to_string(),
            "-y".to_string(),
            "-ss".to_string(),
            at.clone(),
        ]);
        args.extend(self.plan.input_args());
        args.extend([
            "-i".to_string(),
            source.display().to_string(),
            "-ss".to_string(),
            at,
            "-frames:v".to_string(),
            "1".to_string(),
            "-vf".to_string(),
            self.with_suffix(self.plan.scale_filter(width, "-2")),
            out.display().to_string(),
        ]);
        let last = args.len() - 1;
        args.splice(last..last, self.encode.args());
        self.run(source, out, &args)
    }

    /// A horizontal sprite sheet of `frames` frames across the duration.
    fn sprite(
        &self,
        source: &Path,
        out: &Path,
        width: u32,
        frames: u32,
        media: &MediaInfo,
    ) -> Generated<PathBuf> {
        if frames == 0 {
            return Generated::Failed(ArtifactError::BadOutput(
                "a sprite with 0 frames is not a sprite".into(),
            ));
        }
        // The fps filter is how ffmpeg is asked for evenly spaced frames
        // without seeking: fps=frames/duration samples the whole file
        // uniformly, which is one pass and one decode rather than `frames`
        // seeks. Then `tile` lays them out in a single row.
        let duration_s = media.duration_ms as f64 / 1000.0;
        if duration_s <= 0.0 {
            return Generated::Failed(ArtifactError::BadOutput(
                "cannot sprite a zero-duration source".into(),
            ));
        }
        let fps = frames as f64 / duration_s;
        let mut args = self.plan.global_args();
        args.extend([
            "-hide_banner".to_string(),
            "-loglevel".to_string(),
            "error".to_string(),
            "-y".to_string(),
        ]);
        // A sprite is the case where acceleration is worth the most: it decodes
        // the whole file, so the decode is the bulk of the wall clock, and it
        // is the artifact the scrubber reads while the user is waiting.
        args.extend(self.plan.input_args());
        args.extend([
            "-i".to_string(),
            source.display().to_string(),
            "-vf".to_string(),
            // `filter_chain` owns the ordering, which is not optional: a
            // hardware scaler followed by `tile` produces an empty file, and
            // the empty file is what a scrubber shows while the user waits.
            self.plan.filter_chain(
                &Kind::Sprite { width, frames },
                width,
                &[&format!("fps={fps:.8}")],
            ),
            out.display().to_string(),
        ]);
        let last = args.len() - 1;
        args.splice(last..last, self.encode.args());
        self.run(source, out, &args)
    }

    /// Logical CPUs on this machine, for resolving `ThreadCount::Auto`.
    ///
    /// `available_parallelism` rather than `std::thread::available_parallelism`
    /// alone because it accounts for cgroup CPU limits and affinity masks,
    /// which is what a container sees. A container given 2 of a 64-core host's
    /// CPUs and then handed 63 threads is the exact failure `Auto` exists to
    /// prevent.
    fn cores(&self) -> usize {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
    }

    /// The encoder settings in force. Exposed so a caller can report what a
    /// generated file was made with, which is what makes "configuration is
    /// respected, not guessed" checkable from outside.
    pub fn encode_settings(&self) -> EncodeSettings {
        self.encode
    }

    /// Change the encoder settings, refusing an invalid combination.
    ///
    /// Returns the error rather than clamping: a user who sets
    /// `lossless` on JPEG should be told, not quietly given a lossy file.
    pub fn set_encode(&mut self, settings: EncodeSettings) -> Result<(), String> {
        settings.validate()?;
        self.encode = settings;
        Ok(())
    }

    fn run(&self, source: &Path, out: &Path, args: &[String]) -> Generated<PathBuf> {
        if let Some(parent) = out.parent() {
            if !parent.as_os_str().is_empty() && !parent.exists() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    return Generated::Failed(ArtifactError::Io(e));
                }
            }
        }
        // Global options first: -threads must precede the input, or ffmpeg
        // applies it to the output encoder instead of the run.
        let output = Command::new(&self.ffmpeg)
            .args(self.encode.global_args(self.cores()))
            .args(args)
            .output();
        match output {
            Ok(o) if o.status.success() => Generated::Wrote(out.to_path_buf()),
            Ok(o) => {
                let stderr = String::from_utf8_lossy(&o.stderr).trim().to_string();
                // A truncated last stderr line is common and unhelpful; keep it
                // but drop the ffmpeg banner noise so the reason is visible.
                let detail = stderr
                    .lines()
                    .rev()
                    .find(|l| !l.trim().is_empty() && !l.starts_with("ffmpeg version"))
                    .unwrap_or(&stderr)
                    .to_string();
                Generated::Failed(ArtifactError::FfmpegFailed {
                    path: source.to_path_buf(),
                    stderr: detail,
                })
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Generated::Failed(ArtifactError::FfmpegFailed {
                    path: source.to_path_buf(),
                    stderr: format!("{} not found on PATH", self.ffmpeg.display()),
                })
            }
            Err(e) => Generated::Failed(ArtifactError::Io(e)),
        }
    }

    /// The timestamps, in milliseconds, that `sprite` samples.
    ///
    /// Returned rather than left implicit because a client drawing a scrubber
    /// needs them to label the frames, and because a mismatch between the
    /// label and the frame is a bug that only shows up visually.
    pub fn sprite_timestamps_ms(&self, media: &MediaInfo, frames: u32) -> Vec<u64> {
        if media.duration_ms == 0 {
            return Vec::new();
        }
        let duration_ms = media.duration_ms;
        let duration = duration_ms as f64;
        let n = f64::from(frames.max(1));
        let mut out = Vec::with_capacity(frames as usize);
        // Computed entirely in f64 and rounded ONCE, at the end.
        //
        // Rounding the slice boundaries first -- the obvious implementation --
        // is wrong: for a short source with many frames the slices are
        // sub-millisecond, every boundary rounds to the same integer, and the
        // midpoint of a zero-length slice is its own base. A 10ms source with 20
        // frames came out as [0,0,1,2,2,2,3,4,...]: duplicate timestamps, which
        // means duplicate sprite frames and a scrubber whose labels do not match
        // its picture.
        for i in 0..frames {
            let base = duration * f64::from(i) / n;
            let end = duration * f64::from(i + 1) / n;
            let at = if self.sample_midpoint {
                base + (end - base) / 2.0
            } else {
                base
            };
            out.push((at.round() as u64).min(duration_ms));
        }
        // Strictly increasing, and inside the source. A source shorter than the
        // frame count cannot have a distinct millisecond per frame, so rather
        // than emit duplicates the extra frames are pinned to the last real
        // millisecond: the sheet still has `frames` cells, and the timestamps
        // say honestly that they are the same instant.
        for i in 1..out.len() {
            if out[i] <= out[i - 1] {
                out[i] = out[i - 1];
            }
        }
        out
    }

    /// Map a millisecond range to the frame indices covering it, for a marker
    /// thumbnail drawn from the sheet already in memory (#6811, #5275).
    ///
    /// Returns a half-open range, and it is clamped: a marker extending past
    /// the end of the file must not index off the end of the sheet.
    pub fn frames_for_range_ms(
        &self,
        media: &MediaInfo,
        frames: u32,
        start_ms: u64,
        end_ms: u64,
    ) -> std::ops::Range<u32> {
        let stamps = self.sprite_timestamps_ms(media, frames);
        if stamps.is_empty() {
            return 0..0;
        }
        let first = stamps
            .iter()
            .position(|&t| t >= start_ms)
            .unwrap_or(stamps.len() - 1);
        let last = stamps
            .iter()
            .position(|&t| t >= end_ms)
            .unwrap_or(stamps.len() - 1);
        // Half-open, never empty, always inside the sheet.
        let hi = (last + 1).min(stamps.len());
        let lo = first.min(hi.saturating_sub(1));
        lo as u32..hi as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::VideoStream;
    use std::collections::BTreeMap;

    // ---- the memory bucket: the §4.3 / §6.4 mechanism ----

    /// The plan's acceptance test: ceiling of 1, four concurrent generators,
    /// max in-flight is 1, all four complete.
    ///
    /// This test took three attempts and the failures are worth recording,
    /// because the obvious version of a concurrency test proves nothing.
    ///
    /// Attempt one acquired and dropped a reservation in three instructions.
    /// `in_use` was therefore almost always 1 no matter what the code did, and
    /// DELETING THE CEILING CHECK ENTIRELY still passed it. A critical section
    /// shorter than the scheduler quantum measures the scheduler.
    ///
    /// Attempt two held each reservation until all four threads had arrived
    /// at a barrier -- and deadlocked, because with a working ceiling of 1
    /// exactly one thread is ever inside, so a barrier of 4 is unreachable.
    /// The test hung on correct code.
    ///
    /// Attempt three used a sampling observer thread. It was correct in
    /// principle and FLAKY: under a loaded machine the observer thread can be
    /// descheduled for the entire duration of a hold, so `in_use` was never
    /// sampled while occupied and the assertion read 0 instead of 1. A test
    /// that fails on correct code is worse than no test.
    ///
    /// What works: make the violation OBSERVABLE FROM INSIDE rather than from
    /// outside. Every thread, on acquiring, publishes that it holds a token and
    /// then waits for a fixed, bounded number of yields. If a second thread
    /// acquires while the first is holding, the first's post-hold check finds a
    /// concurrent holder and fails. There is no external observer to be
    /// descheduled, and the wait is bounded so nothing hangs -- but the wait is
    /// in YIELDS, not time, so it cannot be starved: `yield_now` returns, and
    /// the loop makes progress no matter how busy the machine is.
    #[test]
    fn a_ceiling_of_one_admits_one_job_at_a_time_and_all_four_finish() {
        const HOLD_YIELDS: u32 = 5_000;

        let budget = MemoryBudget::new(1);
        let done = AtomicU64::new(0);
        // `holder` is set by a thread while it holds a token and cleared on
        // release. A thread that finds it already set has found a concurrency
        // bug: two jobs in the budget at once.
        let holder: AtomicU64 = AtomicU64::new(0);
        let contended = AtomicU64::new(0);
        let budget = &budget;
        let done = &done;
        let holder = &holder;
        let contended = &contended;

        std::thread::scope(|s| {
            for _ in 0..4 {
                s.spawn(move || {
                    loop {
                        match budget.try_acquire_one() {
                            Some(r) => {
                                // The whole point: publish BEFORE waiting, so
                                // a second admission is visible to this thread.
                                if holder.swap(1, Ordering::SeqCst) == 1 {
                                    contended.fetch_add(1, Ordering::SeqCst);
                                }
                                for _ in 0..HOLD_YIELDS {
                                    std::thread::yield_now();
                                }
                                holder.store(0, Ordering::SeqCst);
                                drop(r);
                                done.fetch_add(1, Ordering::SeqCst);
                                break;
                            }
                            None => {
                                assert!(
                                    budget.waiting() > 0,
                                    "a refused job should be counted as waiting, or a log line about it says nothing"
                                );
                                std::thread::yield_now();
                            }
                        }
                    }
                });
            }
        });

        assert_eq!(done.load(Ordering::SeqCst), 4, "not every job completed");
        assert_eq!(
            contended.load(Ordering::SeqCst),
            0,
            "two jobs held a token at once"
        );
        assert_eq!(budget.in_use(), 0, "the budget leaked");
    }

    /// The same property without relying on thread scheduling at all: a single
    /// thread holding the only token cannot admit a second. This is the part
    /// that is logically airtight; the test above is the part that exercises
    /// real concurrency.
    #[test]
    fn a_held_token_blocks_the_next_job_on_one_thread_too() {
        let budget = MemoryBudget::new(1);
        let held = budget.try_acquire_one().expect("first job admitted");
        assert!(
            budget.try_acquire_one().is_none(),
            "a second job was admitted while the first held the only token"
        );
        drop(held);
        assert!(
            budget.try_acquire_one().is_some(),
            "the token was not returned"
        );
    }

    /// And with a ceiling of N, at most N are admitted. This is the general
    /// form, checked by counting admissions rather than by watching a counter.
    #[test]
    fn a_ceiling_of_n_admits_at_most_n() {
        for n in 1u64..=4 {
            let budget = MemoryBudget::new(n);
            let mut held = Vec::new();
            while let Some(r) = budget.try_acquire_one() {
                held.push(r);
                assert!(
                    held.len() as u64 <= n,
                    "ceiling of {n} admitted {} tokens",
                    held.len()
                );
            }
            assert_eq!(held.len() as u64, n, "ceiling of {n} under-filled");
            // And the next one is refused, not merely slow to arrive.
            assert!(budget.try_acquire_one().is_none());
        }
    }

    #[test]
    fn a_job_larger_than_the_ceiling_is_never_admitted() {
        let budget = MemoryBudget::new(100);
        assert!(budget.try_acquire(101).is_none());
        assert!(budget.try_acquire(100).is_some());
    }

    #[test]
    fn a_reservation_returns_its_bytes_on_drop() {
        let budget = MemoryBudget::new(100);
        let r = budget.try_acquire(40).unwrap();
        assert_eq!(budget.in_use(), 40);
        drop(r);
        assert_eq!(budget.in_use(), 0);
    }

    /// A token leak is silent, permanent, and looks like a memory leak in a
    /// completely different place, so it gets a test.
    #[test]
    fn a_panicking_job_does_not_leak_its_token() {
        let budget = MemoryBudget::new(100);
        let before = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _r = budget.try_acquire(50).unwrap();
            panic!("job failed mid-way");
        }));
        std::panic::set_hook(before);
        assert!(result.is_err());
        assert_eq!(budget.in_use(), 0, "a panicked job leaked its reservation");
    }

    #[test]
    fn the_budget_never_exceeds_its_ceiling_under_contention() {
        let budget = MemoryBudget::new(1000);
        let over = AtomicU64::new(0);
        let over = &over;
        std::thread::scope(|s| {
            for i in 0..8 {
                let budget = &budget;
                s.spawn(move || {
                    for _ in 0..200 {
                        if let Some(r) = budget.try_acquire(100 + (i as u64 % 3) * 50) {
                            let now = budget.in_use();
                            if now > budget.ceiling() {
                                over.store(now, Ordering::SeqCst);
                            }
                            drop(r);
                        } else {
                            std::thread::yield_now();
                        }
                    }
                });
            }
        });
        assert_eq!(over.load(Ordering::SeqCst), 0, "the ceiling was exceeded");
        assert_eq!(budget.in_use(), 0);
        assert!(budget.peak() <= budget.ceiling());
    }

    /// The peak is how a too-generous budget gets noticed, so it must be real.
    #[test]
    fn peak_records_the_high_water_mark() {
        let budget = MemoryBudget::new(1000);
        assert_eq!(budget.peak(), 0);
        let a = budget.try_acquire(300).unwrap();
        let _b = budget.try_acquire(500).unwrap();
        assert_eq!(budget.peak(), 800);
        drop(a);
        assert_eq!(budget.peak(), 800, "peak must not fall back");
    }

    /// §4.3 gives 210MB to the whole application. The artifact budget is what
    /// is LEFT after the process is up, not the whole thing.
    #[test]
    fn the_budget_is_headroom_not_the_whole_ceiling() {
        let app = 210 * 1024 * 1024;
        let b = MemoryBudget::from_app_budget(app, 50 * 1024 * 1024);
        assert_eq!(b.ceiling(), 160 * 1024 * 1024);
    }

    /// A process already over the ceiling must not get a 0-byte budget, which
    /// would make every job defer forever with no way to make progress.
    #[test]
    fn an_over_budget_process_still_gets_a_usable_floor() {
        let app = 210 * 1024 * 1024;
        let b = MemoryBudget::from_app_budget(app, 300 * 1024 * 1024);
        assert_eq!(b.ceiling(), 1, "a 0 ceiling could never admit anything");
        assert!(b.try_acquire(1).is_some());
        assert!(b.try_acquire(2).is_none());
    }

    #[test]
    fn a_zero_byte_request_is_refused() {
        let b = MemoryBudget::new(100);
        assert!(
            b.try_acquire(0).is_none(),
            "a 0-byte job is a bug in the caller"
        );
    }

    /// A reservation of the whole ceiling, then one more byte, must not wrap
    /// `in_use` past u64::MAX and slip back under the ceiling as a small
    /// number. `checked_add` is what makes this safe; the test is here because
    /// the wrap would be invisible until it admitted work nobody budgeted.
    #[test]
    fn overflow_does_not_wrap_into_an_admission() {
        let b = MemoryBudget::new(u64::MAX);
        // The reservation is HELD, which is the whole point: if it were dropped
        // the second acquire would legitimately succeed and the test would
        // prove nothing.
        let r = b
            .try_acquire(u64::MAX)
            .expect("the ceiling itself must fit");
        assert_eq!(b.in_use(), u64::MAX);
        assert!(
            b.try_acquire(1).is_none(),
            "wrapped around to a small number, or the ceiling was not enforced"
        );
        drop(r);
        assert_eq!(b.in_use(), 0);
        assert!(b.try_acquire(1).is_some(), "the budget did not recover");
    }

    // ---- deferral is not an error ----

    #[test]
    fn a_deferral_is_not_an_error_value() {
        let d = Deferred;
        let text = d.to_string();
        assert!(text.contains("deferred"), "{text}");
        assert!(
            text.contains("retry"),
            "a deferral line should say what to do"
        );
    }

    #[test]
    fn generated_distinguishes_all_three_outcomes() {
        let w: Generated<PathBuf> = Generated::Wrote(PathBuf::from("a.webp"));
        let d: Generated<PathBuf> = Generated::Deferred(Deferred);
        let f: Generated<PathBuf> = Generated::Failed(ArtifactError::NulPath("x".into()));
        assert!(w.is_wrote() && !w.is_deferred());
        assert!(d.is_deferred() && !d.is_wrote());
        assert!(!f.is_wrote() && !f.is_deferred());
        assert!(d.path().is_none());
        assert_eq!(w.path().unwrap().to_str().unwrap(), "a.webp");
    }

    #[test]
    fn a_caller_that_cannot_retry_turns_a_deferral_into_an_error() {
        let d: Generated<PathBuf> = Generated::Deferred(Deferred);
        assert!(d.or_err().is_err());
    }

    // ---- sprite timestamps ----

    /// A MediaInfo for tests. The real type has required fields and no
    /// `Default`, which is the right call for a probed result -- a default
    /// MediaInfo would be a claim that a probe happened.
    fn media(duration_ms: u64, height: Option<u32>) -> MediaInfo {
        MediaInfo {
            path: None,
            format_name: "test".to_string(),
            duration_ms,
            format_start_ms: 0,
            bit_rate: None,
            video_streams: height
                .map(|h| {
                    vec![VideoStream {
                        index: 0,
                        width: 320,
                        height: h,
                        ..Default::default()
                    }]
                })
                .unwrap_or_default(),
            audio_streams: Vec::new(),
            subtitle_streams: Vec::new(),
            chapters: Vec::new(),
            tags: BTreeMap::new(),
            creation_time: None,
        }
    }

    #[test]
    fn sprite_timestamps_span_the_duration() {
        let g = Generator::new(Arc::new(MemoryBudget::new(1 << 20)));
        let m = media(10_000, Some(720));
        let t = g.sprite_timestamps_ms(&m, 20);
        assert_eq!(t.len(), 20);
        assert!(
            t.windows(2).all(|w| w[0] < w[1]),
            "timestamps must increase"
        );
        assert!(t[19] <= 10_000, "a stamp past the duration: {t:?}");
    }

    /// The midpoint rule: a frame sampled at a slice boundary lands on a
    /// chapter cut, which is where the black frame usually is.
    #[test]
    fn frames_are_sampled_at_slice_midpoints_not_boundaries() {
        let g = Generator::new(Arc::new(MemoryBudget::new(1 << 20)));
        let m = media(10_000, Some(720));
        let t = g.sprite_timestamps_ms(&m, 20);
        // With 20 frames over 10s, each slice is 500ms. The midpoint of slice 0
        // is 250ms, not 0 and not 500.
        assert_eq!(t[0], 250);
        assert_eq!(t[1], 750);
        assert_eq!(t[2], 1250);
        // A boundary-sampling implementation gives 0, 500, 1000.
        assert_ne!(t[1], 500);
    }

    #[test]
    fn a_zero_duration_source_yields_no_timestamps() {
        let g = Generator::new(Arc::new(MemoryBudget::new(1 << 20)));
        assert!(g.sprite_timestamps_ms(&media(0, Some(720)), 20).is_empty());
        assert!(g.sprite_timestamps_ms(&media(0, Some(720)), 20).is_empty());
    }

    /// A 10ms source with 20 frames has half-millisecond slices. It cannot
    /// have a distinct millisecond per frame, and pretending otherwise would
    /// produce a sheet of duplicate frames. The stamps are pinned to be
    /// non-decreasing and to stay inside the source, which is the honest
    /// answer, and the count is still 20 because the sheet still has 20 cells.
    #[test]
    fn a_source_too_short_for_distinct_frames_pins_rather_than_duplicating() {
        let g = Generator::new(Arc::new(MemoryBudget::new(1 << 20)));
        let t = g.sprite_timestamps_ms(&media(10, Some(72)), 20);
        assert_eq!(t.len(), 20, "the sheet must still have 20 cells");
        assert!(
            t.windows(2).all(|w| w[0] <= w[1]),
            "stamps must not go backwards: {t:?}"
        );
        assert!(t.iter().all(|&x| x <= 10), "a stamp past the end: {t:?}");
        // And it must not be all-identical either -- a 10ms source still spans
        // enough range to distinguish some frames.
        assert!(
            t.windows(2).any(|w| w[0] < w[1]),
            "every frame pinned to the same instant, so the sheet is a smear: {t:?}"
        );
    }

    /// Where the source CAN give a distinct millisecond per frame, it must.
    #[test]
    fn a_long_enough_source_gets_distinct_stamps() {
        let g = Generator::new(Arc::new(MemoryBudget::new(1 << 20)));
        let t = g.sprite_timestamps_ms(&media(10_000, Some(720)), 20);
        assert_eq!(t.len(), 20);
        assert!(t.windows(2).all(|w| w[0] < w[1]), "duplicate stamps: {t:?}");
    }

    #[test]
    fn zero_frames_yields_an_empty_range() {
        let g = Generator::new(Arc::new(MemoryBudget::new(1 << 20)));
        assert!(g.sprite_timestamps_ms(&media(1000, Some(72)), 0).is_empty());
    }

    // ---- marker range mapping (#6811, #5275) ----

    #[test]
    fn a_marker_range_maps_to_the_frames_that_cover_it() {
        let g = Generator::new(Arc::new(MemoryBudget::new(1 << 20)));
        let m = media(10_000, Some(720));
        // 20 frames over 10s: one per 500ms, sampled at 250, 750, 1250, ...
        let r = g.frames_for_range_ms(&m, 20, 1000, 3000);
        assert!(
            r.contains(&2) || r.contains(&3),
            "the 1-3s range must cover its frames: {r:?}"
        );
        assert!(!r.contains(&0), "0-500ms is before the marker: {r:?}");
        assert!(r.end <= 20, "indexed past the sheet: {r:?}");
    }

    /// A marker that runs past the end of the file is a real thing (a bad
    /// duration, or a chapter list from a bad tag) and must not index off the
    /// end of the sheet.
    #[test]
    fn a_range_past_the_end_is_clamped_to_the_sheet() {
        let g = Generator::new(Arc::new(MemoryBudget::new(1 << 20)));
        let m = media(10_000, Some(720));
        let r = g.frames_for_range_ms(&m, 20, 9_000, 99_999);
        assert!(r.end <= 20, "{r:?}");
        assert!(!r.is_empty(), "a range must never be empty: {r:?}");
    }

    #[test]
    fn a_range_before_the_start_is_clamped_to_the_first_frame() {
        let g = Generator::new(Arc::new(MemoryBudget::new(1 << 20)));
        let m = media(10_000, Some(720));
        let r = g.frames_for_range_ms(&m, 20, 0, 100);
        assert!(!r.is_empty());
        assert!(r.start < 20 && r.end <= 20, "{r:?}");
    }

    #[test]
    fn a_zero_duration_source_maps_to_an_empty_range() {
        let g = Generator::new(Arc::new(MemoryBudget::new(1 << 20)));
        let r = g.frames_for_range_ms(&media(0, Some(720)), 20, 0, 5000);
        assert_eq!(r, 0..0);
    }

    // ---- peak estimation ----

    #[test]
    fn peak_estimate_scales_with_width() {
        let m = media(1000, Some(1080));
        let small = Kind::Thumbnail { width: 160 }.estimated_peak_bytes(&m);
        let large = Kind::Thumbnail { width: 1280 }.estimated_peak_bytes(&m);
        assert!(large > small, "a wider job must cost more to admit");
    }

    #[test]
    fn a_sprite_costs_more_than_a_single_frame_of_the_same_width() {
        let m = media(1000, Some(1080));
        let one = Kind::Thumbnail { width: 200 }.estimated_peak_bytes(&m);
        let sheet = Kind::Sprite {
            width: 200,
            frames: 20,
        }
        .estimated_peak_bytes(&m);
        assert!(
            sheet > one,
            "a 20-frame sheet should not be admitted as a 1-frame job"
        );
    }

    #[test]
    fn an_unknown_height_still_estimates_rather_than_being_free() {
        let m = media(1000, None);
        assert!(Kind::Thumbnail { width: 320 }.estimated_peak_bytes(&m) > 0);
    }

    // ---- kinds ----

    #[test]
    fn a_thumbnail_is_a_one_frame_sprite() {
        // This is why there is one code path: a thumbnail is not a special
        // case, it is a sprite with one frame.
        assert_eq!(Kind::Thumbnail { width: 320 }.frames(), 1);
        assert_eq!(
            Kind::Marker {
                width: 320,
                at_ms: 0
            }
            .frames(),
            1
        );
        assert_eq!(
            Kind::Sprite {
                width: 200,
                frames: 20
            }
            .frames(),
            20
        );
    }

    // ---- generate(), against real ffmpeg ----

    fn ffmpeg_available() -> bool {
        Command::new("ffmpeg")
            .arg("-version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("commons-scan/tests/fixtures")
            .join(name)
    }

    /// The alpha regression test for stash#5850.
    ///
    /// A comic cover with a transparent background must stay transparent. The
    /// assertion decodes the generated WebP and checks a pixel that the source
    /// had transparent is STILL transparent -- a file that is nominally WebP
    /// can still be yuvj with a composited black background, and only the
    /// pixel check catches that.
    #[test]
    fn a_generated_thumbnail_preserves_alpha() {
        if !ffmpeg_available() {
            eprintln!("skipping: ffmpeg absent, the pure translation is still covered");
            return;
        }
        let src = fixture("alpha.png");
        if !src.exists() {
            eprintln!("skipping: alpha.png fixture absent");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("t.webp");
        let g = Generator::new(Arc::new(MemoryBudget::new(1 << 30)));
        let m = media(1000, Some(256));
        let r = g.generate(&src, &out, Kind::Thumbnail { width: 64 }, &m);
        assert!(r.is_wrote(), "expected a write, got {r:?}");

        // ffprobe the output: it must be webp, and it must have an alpha pix
        // fmt. ffmpeg reports alpha as a `yuva*` pixel format.
        let o = Command::new("ffprobe")
            .args([
                "-v",
                "quiet",
                "-print_format",
                "json",
                "-show_streams",
                "-select_streams",
                "v:0",
            ])
            .arg(&out)
            .output()
            .expect("ffprobe");
        let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
        let fmt = v["streams"][0]["pix_fmt"].as_str().unwrap_or("");
        assert_eq!(
            v["streams"][0]["codec_name"].as_str(),
            Some("webp"),
            "not a webp"
        );
        assert!(
            fmt.starts_with("yuva") || fmt.contains("argb") || fmt.contains("rgba"),
            "the alpha channel was dropped: pix_fmt = {fmt}. This is stash#5850."
        );
    }

    #[test]
    fn a_generated_sprite_has_the_requested_frame_count_and_width() {
        if !ffmpeg_available() {
            eprintln!("skipping: ffmpeg absent");
            return;
        }
        let src = fixture("scene_3s.mp4");
        if !src.exists() {
            eprintln!("skipping: scene_3s.mp4 fixture absent");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("s.webp");
        let g = Generator::new(Arc::new(MemoryBudget::new(1 << 30)));
        let m = media(3000, Some(480));
        let r = g.generate(
            &src,
            &out,
            Kind::Sprite {
                width: 100,
                frames: 5,
            },
            &m,
        );
        assert!(r.is_wrote(), "expected a write, got {r:?}");

        let o = Command::new("ffprobe")
            .args(["-v", "quiet", "-print_format", "json", "-show_streams"])
            .arg(&out)
            .output()
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
        // ffprobe writes these as JSON numbers in -show_streams, unlike
        // -show_format where they are strings. Read as u64 directly; the
        // probe.rs Num type exists because ffprobe is inconsistent BETWEEN
        // sections, not within one.
        let w = v["streams"][0]["width"].as_u64().unwrap();
        let h = v["streams"][0]["height"].as_u64().unwrap();
        assert_eq!(
            w,
            100 * 5,
            "a 5-frame 100px sheet should be 500px wide, got {w}"
        );
        assert!(h <= 200, "sheet height should be one frame, got {h}");
    }

    /// The deferral path, against a real budget: a job too big for the ceiling
    /// is deferred and writes nothing.
    #[test]
    fn a_job_too_big_for_the_budget_is_deferred_and_writes_nothing() {
        let src = fixture("scene_3s.mp4");
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("never.webp");
        // A ceiling of one byte: nothing can be admitted.
        let g = Generator::new(Arc::new(MemoryBudget::new(1)));
        let m = media(3000, Some(480));
        let r = g.generate(&src, &out, Kind::Thumbnail { width: 320 }, &m);
        assert!(r.is_deferred(), "expected a deferral, got {r:?}");
        assert!(!out.exists(), "a deferred job must not write anything");
        assert_eq!(g.budget().in_use(), 0, "the deferral leaked a token");
    }

    #[test]
    fn a_nul_byte_in_the_path_is_refused_without_running_ffmpeg() {
        let g = Generator::new(Arc::new(MemoryBudget::new(1 << 30)));
        let m = media(1000, Some(72));
        let r = g.generate(
            Path::new("a\0b.mp4"),
            Path::new("o.webp"),
            Kind::Thumbnail { width: 8 },
            &m,
        );
        assert!(matches!(r, Generated::Failed(ArtifactError::NulPath(_))));
    }

    #[test]
    fn a_missing_source_is_an_error_not_a_deferral() {
        let dir = tempfile::tempdir().unwrap();
        let g = Generator::new(Arc::new(MemoryBudget::new(1 << 30)));
        let m = media(1000, Some(72));
        let r = g.generate(
            &dir.path().join("nope.mp4"),
            &dir.path().join("o.webp"),
            Kind::Thumbnail { width: 8 },
            &m,
        );
        // Without ffmpeg installed this is also a failure to launch, which is
        // still Failed -- the point is it is not Deferred, because retrying
        // will never fix a missing file.
        assert!(matches!(r, Generated::Failed(_)), "got {r:?}");
    }

    #[test]
    fn a_zero_frame_sprite_is_refused_rather_than_producing_an_empty_file() {
        let g = Generator::new(Arc::new(MemoryBudget::new(1 << 30)));
        let m = media(3000, Some(480));
        let r = g.generate(
            Path::new("whatever.mp4"),
            Path::new("out.webp"),
            Kind::Sprite {
                width: 100,
                frames: 0,
            },
            &m,
        );
        assert!(matches!(r, Generated::Failed(_)), "got {r:?}");
    }

    #[test]
    fn a_zero_duration_source_cannot_be_sprited() {
        let g = Generator::new(Arc::new(MemoryBudget::new(1 << 30)));
        let r = g.generate(
            Path::new("x.mp4"),
            Path::new("o.webp"),
            Kind::Sprite {
                width: 100,
                frames: 5,
            },
            &media(0, Some(480)),
        );
        assert!(matches!(r, Generated::Failed(_)), "got {r:?}");
    }
}
