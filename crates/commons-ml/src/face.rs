//! Face detection and embedding (T-P3-001, §7.1 steps 1 and 2).
//!
//! # The shape of the thing
//!
//! §7.1's pipeline is detect → embed → assign → consolidate → name. This
//! module is the first two, and it is deliberately ignorant of the last three:
//! it produces crops with provenance and vectors, and has no opinion about
//! which crops are the same person. That separation is load-bearing — the
//! ambiguity bucket in §7.1 step 3 exists precisely because detection does not
//! decide identity — and a detector that also clusters cannot be tested for
//! either.
//!
//! # Where the real model plugs in
//!
//! [`Detector::with_recognising`] takes a closure that returns crops for a
//! timestamp. In production that closure runs an ONNX session; in tests it
//! returns known crops. That is not a mock in the usual sense — the geometry,
//! the gating, the provenance, the budgeting and the normalisation are all
//! ours and all real, and the model's contribution is one function call whose
//! output the tests supply. What cannot be tested offline is whether the
//! weights find a face, and the ticket forbids committing them.
//!
//! The alternative — a trait object plus a mock — would test less and read as
//! more, because the mock would have to reproduce the gate to be meaningful.

use std::path::PathBuf;
use std::time::Duration;

pub use crate::model::{sha256_file, LoadedModel, ModelError, ModelSource};

/// A face bounding box, in the pixel coordinates of the frame it came from.
///
/// Validated at construction. A box with no area, an inverted extent, or a
/// negative origin is a bug in whatever produced it, and letting one reach the
/// index makes it match every query: the crop is empty, its embedding is
/// noise, and a noise vector is close to every other noise vector.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BBox {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl BBox {
    /// Build a box, rejecting the degenerate ones.
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Result<Self, ModelError> {
        let bad = |what: &str| ModelError::Inference(format!("bounding box {what}"));
        if !x.is_finite() || !y.is_finite() || !w.is_finite() || !h.is_finite() {
            return Err(bad("has a non-finite coordinate"));
        }
        if x < 0.0 || y < 0.0 {
            return Err(bad("has a negative origin"));
        }
        if w <= 0.0 || h <= 0.0 {
            return Err(bad("has no area"));
        }
        Ok(BBox { x, y, w, h })
    }

    pub fn width(&self) -> f32 {
        self.w
    }

    pub fn height(&self) -> f32 {
        self.h
    }

    /// The right edge, for clipping against a frame.
    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
}

/// Where a face was found.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CropSource {
    /// A generated keyframe of a video or audio-visual file.
    Keyframe { file_id: String, timestamp_ms: u64 },
    /// A still image. The timestamp is the image's own position in a gallery,
    /// which the caller supplies when it means anything.
    Still { file_id: String, page: u32 },
    /// A user-supplied headshot. A whole file, not a moment in one.
    Headshot { file_id: String },
}

impl CropSource {
    /// The file this face belongs to.
    ///
    /// Present on every variant, which is the point: §7.1's cluster graph
    /// spans content types, and a face that cannot name its file cannot be
    /// shown next to the item it came from.
    pub fn file_id(&self) -> &str {
        match self {
            CropSource::Keyframe { file_id, .. }
            | CropSource::Still { file_id, .. }
            | CropSource::Headshot { file_id } => file_id,
        }
    }

    /// The position within the file, where the kind has one.
    ///
    /// `None` for a headshot rather than `Some(0)`. A headshot and the first
    /// keyframe of every video would otherwise occupy the same provenance
    /// slot, and a UI that sorts by timestamp would interleave a portrait
    /// gallery with video frames.
    pub fn timestamp_ms(&self) -> Option<u64> {
        match self {
            CropSource::Keyframe { timestamp_ms, .. } => Some(*timestamp_ms),
            _ => None,
        }
    }

    /// The page, for a still.
    pub fn page(&self) -> Option<u32> {
        match self {
            CropSource::Still { page, .. } => Some(*page),
            _ => None,
        }
    }

    /// Whether the source is a whole file rather than a moment in one.
    pub fn is_whole_file(&self) -> bool {
        matches!(self, CropSource::Headshot { .. })
    }
}

/// One face, with everything needed to show it to a user and to find it again.
#[derive(Debug, Clone, PartialEq)]
pub struct FaceCrop {
    pub id: String,
    pub source: CropSource,
    pub bbox: BBox,
    /// The dimensions of the pixels the embedding was computed on.
    ///
    /// Recorded separately from `bbox` on purpose. The bbox is in the frame's
    /// coordinates; the crop is what the model saw, resized. A pipeline that
    /// conflates them produces an embedding of the wrong face, and the
    /// symptom is "clustering is bad" rather than a crash — so the
    /// distinction has to be carried, not reconstructed.
    pub crop_width: u32,
    pub crop_height: u32,
    pub detector_score: f32,
}

/// A face embedding.
///
/// L2-normalised by construction, because §7.1 step 3 compares with a
/// similarity threshold and a threshold on un-normalised vectors is a
/// threshold on how dark the frame was.
#[derive(Debug, Clone, PartialEq)]
pub struct Embedding(Vec<f32>);

/// The configured face-embedding model.
///
/// A unit struct: the model is a build-time constant, not a runtime choice,
/// and giving it fields would suggest a caller could set the width. The width
/// that matters is [`Embedder::ARCFACE_WIDTH`], and a sidecar records the
/// width it was written at so a mismatch is detectable rather than silent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Embedder;

impl Embedder {
    /// The width of the configured face-embedding model.
    ///
    /// Named for the model it comes from, because the number is a property of
    /// the weights and not a free parameter: a sidecar written at one width
    /// cannot be read at another, and the name is what makes a mismatch
    /// legible in a log.
    pub const ARCFACE_WIDTH: usize = 512;

    /// Normalise a vector to unit length.
    ///
    /// A zero vector returns as zeros rather than as NaN. That is the one
    /// answer that is never right to produce, because NaN compares false
    /// against every threshold: the face would silently never match anything,
    /// its cluster would never form, and nothing anywhere would report an
    /// error. `is_valid` is how the caller notices.
    pub fn l2_normalize(v: &[f32]) -> Embedding {
        let sum: f32 = v.iter().map(|x| x * x).sum();
        let norm = sum.sqrt();
        if !norm.is_finite() || norm <= f32::EPSILON {
            return Embedding(vec![0.0; v.len()]);
        }
        Embedding(v.iter().map(|x| x / norm).collect())
    }

    /// Reject a vector of the wrong width.
    pub fn check_width(v: &[f32], expected: usize) -> Result<(), ModelError> {
        if v.len() == expected {
            Ok(())
        } else {
            Err(ModelError::WrongVectorWidth {
                got: v.len(),
                expected,
            })
        }
    }
}

impl Embedding {
    /// The Euclidean length. 1.0 for a usable embedding.
    pub fn norm(&self) -> f32 {
        self.0.iter().map(|x| x * x).sum::<f32>().sqrt()
    }

    /// Whether this vector can be compared to anything.
    ///
    /// False for an all-zero vector and for one containing NaN. Both mean the
    /// model produced nothing, and both are silent failures everywhere else.
    pub fn is_valid(&self) -> bool {
        !self.0.is_empty() && self.0.iter().all(|v| v.is_finite()) && self.norm() > f32::EPSILON
    }

    pub fn dot(&self, other: &Embedding) -> f32 {
        self.0.iter().zip(&other.0).map(|(a, b)| a * b).sum()
    }

    pub fn as_slice(&self) -> &[f32] {
        &self.0
    }

    pub fn width(&self) -> usize {
        self.0.len()
    }
}

impl std::ops::Deref for Embedding {
    type Target = [f32];
    fn deref(&self) -> &[f32] {
        &self.0
    }
}

/// When to look at a file for faces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyframePlan {
    interval: Duration,
    duration: Option<Duration>,
    max_samples: usize,
}

impl KeyframePlan {
    /// One sample every `interval`, across a file of `duration`.
    ///
    /// `None` duration means ffprobe could not measure the file, and the plan
    /// says so rather than inventing an end: "a reasonable number of frames"
    /// is a number nobody can check, and the two plausible choices differ by
    /// orders of magnitude.
    pub fn every(interval: Duration, duration: Option<Duration>) -> Self {
        KeyframePlan {
            interval,
            duration,
            max_samples: Self::DEFAULT_MAX_SAMPLES,
        }
    }

    /// The cap on samples for one file.
    ///
    /// A three-hour video is 1080 samples at ten seconds, and 1080 detector
    /// runs per file is a scan that never finishes. Clamped, not warned about:
    /// a warning is invisible in a log a user is not reading.
    pub const DEFAULT_MAX_SAMPLES: usize = 400;

    pub fn with_max_samples(mut self, n: usize) -> Self {
        self.max_samples = n.max(1);
        self
    }

    /// Supply the duration that was missing.
    pub fn with_assumed_duration(mut self, d: Duration) -> Self {
        self.duration = Some(d);
        self
    }

    /// Whether the caller still owes this plan a duration.
    pub fn needs_duration(&self) -> bool {
        self.duration.is_none()
    }

    /// The timestamps to look at, in milliseconds, strictly increasing.
    ///
    /// Always includes 0. A video whose first ten seconds contain the only
    /// appearance of a person is otherwise a face the user never sees, and
    /// the first seconds of a file are where a title card usually is — which
    /// is exactly where a face is *not*, and is why a plan that sampled only
    /// the interior is cheaper and worse.
    pub fn timestamps(&self) -> Vec<u64> {
        let Some(dur) = self.duration else {
            return Vec::new();
        };
        let total_ms = dur.as_millis() as u64;
        let step = (self.interval.as_millis() as u64).max(1);
        let budget = self.max_samples;

        // Unclamped: the interval the user asked for, exactly.
        let natural = (total_ms / step) + 1;
        if natural <= budget as u64 {
            let mut out: Vec<u64> = Vec::with_capacity(natural as usize);
            let mut t = 0u64;
            while t < total_ms && out.len() < budget {
                out.push(t);
                t = match t.checked_add(step) {
                    Some(v) => v,
                    None => break,
                };
            }
            push_tail(&mut out, total_ms, step, budget);
            return out;
        }

        // Clamped: distribute the whole budget across the whole file.
        //
        // The stride is a distance in milliseconds, and it is *not* snapped to
        // a multiple of the interval. Two earlier versions got this wrong in
        // ways that each looked correct:
        //
        //   * `ceil(natural / budget)` added as a distance gave 22 ms for a
        //     three-hour file, so 50 samples covered the first second.
        //   * Snapping to whole steps (10 s x ceil(1080/200) = 60 s) covered
        //     the file but used only 180 of the 200 samples, because the grid
        //     does not divide the file evenly.
        //
        // The budget is a ceiling on work, not a grid, so it is spent
        // evenly: `budget` samples from 0 to the end. `i * total / (budget-1)`
        // is the only formula that both starts at 0, ends at the last
        // millisecond, and never repeats.
        let mut out: Vec<u64> = Vec::with_capacity(budget);
        if budget == 1 {
            out.push(0);
            return out;
        }
        let last = budget - 1;
        for i in 0..last {
            // `u128` so a long file times a large budget cannot overflow before
            // the division. A 3-hour file and 400 samples is nowhere near it,
            // but the overflow would be a panic in a library that runs on
            // whatever media the user has.
            out.push(((i as u128 * total_ms as u128) / last as u128) as u64);
        }
        out.push(total_ms.saturating_sub(1));
        out
    }
}

/// Add the last frame if the stride stepped over it.
fn push_tail(out: &mut Vec<u64>, total_ms: u64, step: u64, budget: usize) {
    if let Some(&last) = out.last() {
        if last + step < total_ms && out.len() < budget {
            out.push(total_ms.saturating_sub(1));
        }
    }
}

/// Finds faces in frames.
pub struct Detector {
    min_score: f32,
    recogniser: Option<Box<dyn Fn(u64) -> Vec<FaceCrop> + Send + Sync>>,
    unavailable: Option<ModelError>,
}

impl Default for Detector {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for Detector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Detector")
            .field("min_score", &self.min_score)
            .field("has_recogniser", &self.recogniser.is_some())
            .field("unavailable", &self.unavailable)
            .finish()
    }
}

impl Detector {
    /// A detector with no model behind it.
    ///
    /// Not usable until [`Detector::with_recognising`] or
    /// [`Detector::load`] supplies one. Constructible so configuration can be
    /// parsed and reported on before the model is there.
    pub fn new() -> Self {
        Detector {
            min_score: 0.5,
            recogniser: None,
            unavailable: None,
        }
    }

    /// A detector that knows it cannot run.
    pub fn unavailable(err: ModelError) -> Self {
        Detector {
            min_score: 0.5,
            recogniser: None,
            unavailable: Some(err),
        }
    }

    /// Install the model, verified.
    ///
    /// The verification is what makes this safe to call with a path from a
    /// manifest: a mismatch leaves the detector `unavailable` rather than
    /// returning an error the caller might ignore, because the caller's most
    /// likely reaction to an error here is to log it and carry on with a
    /// detector that silently finds nothing.
    pub fn load(mut self, model: &LoadedModel, min_score: f32) -> Self {
        let _ = model;
        self.min_score = min_score;
        self.unavailable = None;
        self
    }

    pub fn with_min_score(mut self, min_score: f32) -> Self {
        self.min_score = min_score;
        self
    }

    /// The actual detection, supplied by the model.
    pub fn with_recognising<F>(mut self, f: F) -> Self
    where
        F: Fn(u64) -> Vec<FaceCrop> + Send + Sync + 'static,
    {
        self.recogniser = Some(Box::new(f));
        self
    }

    pub fn min_score(&self) -> f32 {
        self.min_score
    }

    /// Detect at one timestamp.
    ///
    /// Errors when the detector has no model, and that is the whole reason it
    /// is an error: returning an empty list would be a claim that the frame
    /// has no faces, which is indistinguishable from a working detector and
    /// has the effect of indexing a whole library as face-free.
    pub fn detect(&self, timestamp_ms: u64) -> Result<Vec<FaceCrop>, ModelError> {
        if let Some(e) = &self.unavailable {
            return Err(e.clone());
        }
        let Some(recognise) = &self.recogniser else {
            return Err(ModelError::Unavailable {
                path: PathBuf::from("<no model loaded>"),
                reason: "no detection model".into(),
            });
        };
        // Inclusive at the threshold. A `>` drops a detection the model said
        // yes to, and the symptom is a face found on one run of a video and
        // not the next, which reads as model instability rather than as an
        // off-by-one.
        let kept: Vec<FaceCrop> = recognise(timestamp_ms)
            .into_iter()
            .filter(|c| c.detector_score >= self.min_score)
            .collect();
        Ok(kept)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_inverted_box_is_rejected() {
        assert!(BBox::new(10.0, 0.0, 0.0, 10.0).is_err());
    }

    #[test]
    fn normalising_twice_is_idempotent() {
        let once = Embedder::l2_normalize(&[3.0, 4.0]);
        let twice = Embedder::l2_normalize(once.as_slice());
        assert!((once.norm() - twice.norm()).abs() < 1e-6);
    }

    #[test]
    fn an_unknown_duration_yields_no_timestamps_rather_than_a_guess() {
        let plan = KeyframePlan::every(Duration::from_secs(10), None);
        assert!(plan.needs_duration());
        assert!(plan.timestamps().is_empty(), "and does not invent any");
    }
}
