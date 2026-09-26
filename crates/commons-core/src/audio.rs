//! Audio objects (T-P1-007, spec §5.4).
//!
//! # Why audio is not "a video with no picture"
//!
//! stash#1258 is the most-discussed single issue in the corpus (45 comments)
//! and its complaint is not that stash cannot *hold* audio files. It can. The
//! complaint is that audio is a second-class object: the waveforms do not
//! exist, replay gain is not applied, and a track is navigated by timestamp
//! rather than by listening.
//!
//! So this module carries the three things that make audio an object rather
//! than a file with a duration:
//!
//! 1. **A peak envelope** ([`Waveform`]), which is what a waveform artifact is
//!    rendered from and what the player scrubs against.
//! 2. **Replay gain**, normalised rather than a raw tag, because the correct
//!    normalisation of a track depends on what you are normalising *against*
//!    and a bare number cannot say that.
//! 3. **Track and disc structure** from the container, so a multi-disc release
//!    is navigable and a compilation's track order is the file's, not the
//!    filesystem's.
//!
//! # The envelope is in dB, and that matters
//!
//! Peaks are stored as signed 16-bit values -- full resolution, no scaling, so
//! the artifact can be re-rendered at any zoom level without re-reading the
//! audio. The conversion to display heights happens at render time, and it is
//! done in **decibels**, not linearly, because a linear amplitude plot of music
//! is a nearly flat line with a spike at the start and nobody can see the
//! structure. A dB scale with a floor is what makes a waveform legible.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Buckets per second. The plan specifies 1024; this is the resolution at which
/// a waveform artifact is rendered, and it is also the resolution at which the
/// envelope is stored, so a zoomed view is a resample of stored data rather
/// than a re-read of the audio.
pub const BUCKETS_PER_SECOND: u32 = 1024;

/// The dB floor. Below this a peak is inaudible, and a linear plot would spend
/// most of its height on samples nobody can hear. -60 dB is the conventional
/// floor and maps 16-bit full scale to a 0..1 display height.
pub const DB_FLOOR: f32 = -60.0;

/// One audio object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct AudioObject {
    pub duration_ms: u64,
    /// Track number within its disc/album, when the container says so. `None`
    /// is meaningfully different from `Some(0)`: a track with no number is
    /// untagged, a track numbered 0 is the first of something.
    pub track_number: Option<u32>,
    /// Total tracks on the disc, when known.
    pub track_total: Option<u32>,
    pub disc_number: Option<u32>,
    pub disc_total: Option<u32>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    /// Normalised replay gain, or the raw tag when it cannot be parsed.
    pub replay_gain: ReplayGain,
    /// Sample rate and channels, for display and for the player's path choice.
    pub sample_rate: Option<u32>,
    pub channels: Option<u32>,
}

/// Replay gain, normalised.
///
/// The distinction that makes this a struct rather than a field: ReplayGain
/// tags come in two incompatible conventions, and the number means opposite
/// things in each. `radio` gain is a *negative* adjustment a player applies
/// (negative = attenuate, because radio assumes a loud master). `audiophile`
/// gain is positive for the same file (positive = amplify, because it assumes
/// an unmastered source). A single untyped f32 would be read correctly by
/// exactly half the players in the world.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct ReplayGain {
    /// Gain in dB, as written in the tag.
    pub gain_db: f32,
    /// Peak in dBFS, as written in the tag. A peak of 0.0 means the file has
    /// samples at or above full scale and cannot be amplified.
    pub peak_dbfs: f32,
    /// Which convention the tag used. `None` when there was no tag at all,
    /// which is different from a tag that parsed to zero.
    pub convention: Option<GainConvention>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GainConvention {
    /// ReplayGain 1.0 "radio": the value is an attenuation to apply. Files
    /// mastered for radio need this, and applying it as an amplification
    /// would clip.
    Radio,
    /// ReplayGain 2.0 "audiophile": the value is an amplification to apply.
    Audiophile,
}

impl ReplayGain {
    /// Parse a `REPLAYGAIN_TRACK_GAIN` value such as `"-7.03 dB"`.
    ///
    /// Returns `None` for an unparseable value rather than defaulting to 0.0,
    /// because a silent zero gain and a failed parse are different facts and
    /// the player needs to know which it has. An empty or whitespace-only tag
    /// is `None`, not 0.
    pub fn parse(
        value: &str,
        peak: Option<f32>,
        convention: Option<GainConvention>,
    ) -> Option<Self> {
        let trimmed = value.trim();
        // Strip the unit. Tags are written "-7.03 dB" and sometimes "-7.03dB"
        // and sometimes just "-7.03"; all three occur in the wild.
        let numeric = trimmed
            .trim_end_matches("dB")
            .trim_end_matches("db")
            .trim_end_matches("DB")
            .trim();
        if numeric.is_empty() {
            return None;
        }
        let gain_db: f32 = numeric.parse().ok()?;
        if !gain_db.is_finite() {
            return None;
        }
        Some(Self {
            gain_db,
            peak_dbfs: peak.unwrap_or(0.0),
            convention,
        })
    }

    /// The adjustment a player should APPLY, in dB.
    ///
    /// This is the whole reason the convention is stored: a radio value of
    /// -7.03 means attenuate by 7.03, and an audiophile value of -7.03 means
    /// amplify by 7.03. The sign flips.
    pub fn adjustment_db(&self) -> f32 {
        match self.convention {
            Some(GainConvention::Radio) => -self.gain_db.abs(),
            Some(GainConvention::Audiophile) => self.gain_db.abs(),
            // No convention: the value is used as written, which is what a
            // player without convention support does, and is the only choice
            // that does not silently invert someone's intent.
            None => self.gain_db,
        }
    }

    /// How many dB this file can be raised before it reaches full scale.
    ///
    /// Always `>= 0` for a file that is not already clipping: a file peaking
    /// at -12 dBFS has 12 dB of headroom to 0 dBFS. This is the useful
    /// quantity, and unlike the clipped/boost formulation it is bounded on one
    /// side, so a caller cannot ask a question whose answer is always "yes it
    /// clips".
    pub fn headroom_db(&self) -> f32 {
        -self.peak_dbfs
    }

    /// The largest boost that keeps the peak at or below 0 dBFS.
    pub fn max_safe_boost_db(&self) -> f32 {
        self.headroom_db()
    }

    /// Whether applying `boost_db` of gain would push a sample past full scale.
    ///
    /// `boost_db` is a BOOST, not a target: boosting a -12 dBFS file by 3 dB
    /// gives a -9 dBFS peak, which is fine. The earlier formulation of this
    /// function took a target peak, under which every target above the file's
    /// own peak is trivially a clip -- the check always fired and told a player
    /// nothing. A boost is the question a gain stage actually asks.
    pub fn would_clip(&self, boost_db: f32) -> bool {
        self.peak_dbfs + boost_db > 0.0
    }

    /// The boost that should actually be applied for a `target_db` output
    /// peak, clamped to what the file can take.
    ///
    /// This is the function a player calls, and the clamp is the reason it
    /// exists: a file tagged with 6 dB of gain and a peak 2 dB below full
    /// scale can only take 2 dB, and applying 6 would clip. Returning the
    /// clamped value means the caller cannot skip the check by accident.
    pub fn safe_boost_for_target_db(&self, target_db: f32) -> f32 {
        (target_db - self.peak_dbfs).min(self.max_safe_boost_db())
    }

    /// Whether the file's own peak is at or above full scale, i.e. it clips as
    /// it is. A file with `peak_dbfs >= 0` cannot be amplified at all.
    pub fn peaks_at_full_scale(&self) -> bool {
        self.peak_dbfs >= 0.0
    }
}

impl fmt::Display for ReplayGain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.convention.is_none() {
            return write!(f, "none");
        }
        let c = match self.convention {
            Some(GainConvention::Radio) => "radio",
            Some(GainConvention::Audiophile) => "audiophile",
            None => unreachable!(),
        };
        write!(
            f,
            "{:+.2} dB {c} (peak {:.2} dBFS)",
            self.gain_db, self.peak_dbfs
        )
    }
}

/// A peak envelope: min and max per bucket, full 16-bit resolution.
///
/// Stored at full scale rather than normalised to 0..1 so a re-render at any
/// zoom or any dB floor is a resample of this data rather than a re-read of
/// the audio. The plan asks for 1024 buckets per second.
///
/// [`Deserialize`] is hand-written because two of the fields are load-bearing
/// invariants rather than merely data: `min` and `max` must be the same length
/// (otherwise every bucket read past the shorter one is a wrong answer), and
/// `buckets_per_second` must be non-zero (otherwise every timestamp maps to
/// bucket 0 and the envelope is a single column). Both are reachable from a
/// stored database or an imported stash database, so both are checked at the
/// boundary rather than assumed.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Waveform {
    buckets_per_second: u32,
    duration_ms: u64,
    min: Vec<i16>,
    max: Vec<i16>,
}

/// The wire form, used only by `Deserialize`.
#[derive(Deserialize)]
struct WaveformWire {
    buckets_per_second: u32,
    duration_ms: u64,
    min: Vec<i16>,
    max: Vec<i16>,
}

impl<'de> Deserialize<'de> for Waveform {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        let w = WaveformWire::deserialize(d)?;
        if w.min.len() != w.max.len() {
            return Err(D::Error::custom(format!(
                "waveform has {} min and {} max buckets",
                w.min.len(),
                w.max.len()
            )));
        }
        if w.buckets_per_second == 0 {
            return Err(D::Error::custom(
                "waveform buckets_per_second must be non-zero",
            ));
        }
        Ok(Waveform {
            buckets_per_second: w.buckets_per_second,
            duration_ms: w.duration_ms,
            min: w.min,
            max: w.max,
        })
    }
}

impl Waveform {
    /// Build from interleaved-ish min/max sample pairs.
    ///
    /// `min` and `max` must be the same length; that is not checked here
    /// because this is a constructor for code that has just produced them in a
    /// loop, and a length mismatch there is a bug in that loop, not bad input
    /// from a user. The deserialiser is where hostile or corrupt data arrives,
    /// and it enforces the invariant.
    pub fn new(buckets_per_second: u32, duration_ms: u64, min: Vec<i16>, max: Vec<i16>) -> Self {
        debug_assert_eq!(min.len(), max.len());
        Self {
            buckets_per_second,
            duration_ms,
            min,
            max,
        }
    }

    pub fn buckets_per_second(&self) -> u32 {
        self.buckets_per_second
    }

    pub fn duration_ms(&self) -> u64 {
        self.duration_ms
    }

    pub fn len(&self) -> usize {
        self.min.len()
    }

    pub fn is_empty(&self) -> bool {
        self.min.is_empty()
    }

    /// The peak pair for a bucket, or `None` if out of range.
    pub fn bucket(&self, i: usize) -> Option<(i16, i16)> {
        Some((*self.min.get(i)?, *self.max.get(i)?))
    }

    /// Resample to a display height in 0..1, on a dB scale floored at
    /// [`DB_FLOOR`].
    ///
    /// This is the function that makes a waveform legible. A linear amplitude
    /// plot of music is a flat line: typical programme material peaks around
    /// -12 dBFS, so a linear plot uses 25% of its height and the difference
    /// between a quiet passage and a loud one is invisible. Mapping dB to
    /// height spreads that 48 dB across the full range.
    ///
    /// `None` in, `None` out: a bucket index past the end of the envelope is
    /// not an error, it is a gap, and a render loop asking one bucket past the
    /// end should get nothing rather than a panic or a silent zero that looks
    /// like silence.
    pub fn display_height(&self, i: usize) -> Option<f32> {
        let (lo, hi) = self.bucket(i)?;
        Some(amplitude_to_height(hi, lo))
    }

    /// The whole envelope as display heights, one per bucket, in 0..=1.
    pub fn display_heights(&self) -> Vec<f32> {
        self.min
            .iter()
            .zip(&self.max)
            .map(|(&lo, &hi)| amplitude_to_height(hi, lo))
            .collect()
    }

    /// The loudest bucket's display height, i.e. the top of the waveform.
    /// A render that scales to this is what a player does to normalise the
    /// view.
    pub fn peak_height(&self) -> f32 {
        self.display_heights()
            .iter()
            .copied()
            .fold(0.0f32, f32::max)
    }

    /// Resample to a different bucket rate, by taking the extreme of each
    /// output bucket over the input buckets that fall inside it.
    ///
    /// Extreme, not average: averaging a peak envelope loses exactly the
    /// transients it exists to show, and a downsampled waveform with no
    /// transients is a flat line again.
    pub fn resample(&self, to_buckets_per_second: u32) -> Waveform {
        if to_buckets_per_second == self.buckets_per_second || self.is_empty() {
            return self.clone();
        }
        if to_buckets_per_second == 0 {
            return self.clone();
        }
        let out_len =
            ((self.duration_ms * u64::from(to_buckets_per_second)) / 1000).max(1) as usize;
        let mut min = Vec::with_capacity(out_len);
        let mut max = Vec::with_capacity(out_len);
        let in_len = self.min.len();

        for oi in 0..out_len {
            // Map the output bucket to an input range. Integer arithmetic
            // throughout so there is no floating-point drift over a long file.
            let start = oi * in_len / out_len;
            let end = (((oi + 1) * in_len) / out_len).max(start + 1).min(in_len);
            let (mut lo, mut hi) = (i16::MAX, i16::MIN);
            for i in start..end {
                lo = lo.min(self.min[i]);
                hi = hi.max(self.max[i]);
            }
            min.push(lo);
            max.push(hi);
        }
        Waveform::new(to_buckets_per_second, self.duration_ms, min, max)
    }

    /// The bucket index for a timestamp, clamped to the envelope.
    pub fn bucket_for_ms(&self, at_ms: u64) -> usize {
        if self.buckets_per_second == 0 {
            return 0;
        }
        let idx = (at_ms * u64::from(self.buckets_per_second)) / 1000;
        (idx as usize).min(self.len().saturating_sub(1))
    }
}

/// Map a min/max sample pair to a 0..1 display height on a dB scale.
///
/// The louder of the two drives the height, because a waveform column's
/// magnitude is its loudest point. Signed zeros and the asymmetry of i16::MIN
/// are handled by taking the absolute value in f32, where `-(-32768.0)` is
/// fine -- doing it in i16 would overflow.
fn amplitude_to_height(hi: i16, lo: i16) -> f32 {
    let magnitude = (hi as f32).abs().max((lo as f32).abs());
    if magnitude <= 0.0 {
        return 0.0;
    }
    // 16-bit full scale is 32768.
    let db = 20.0 * (magnitude / 32768.0).log10();
    // Map DB_FLOOR..0dB onto 0..1.
    ((db - DB_FLOOR) / -DB_FLOOR).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- replay gain: the two conventions ----

    /// The bug this type exists to prevent: a radio-tagged -7.03 dB read as an
    /// amplification when it is an attenuation. Applying it as written makes
    /// the track seven decibels LOUDER than the artist intended, which is the
    /// exact opposite of what the tag says.
    #[test]
    fn a_radio_gain_is_an_attenuation_not_an_amplification() {
        let g = ReplayGain::parse("-7.03 dB", Some(-0.5), Some(GainConvention::Radio)).unwrap();
        assert_eq!(g.gain_db, -7.03);
        assert_eq!(g.adjustment_db(), -7.03, "radio: apply as written");
    }

    /// The same number under the other convention means the opposite thing.
    #[test]
    fn the_same_number_under_the_other_convention_flips_the_sign() {
        let radio = ReplayGain::parse("-7.03 dB", None, Some(GainConvention::Radio)).unwrap();
        let audio = ReplayGain::parse("-7.03 dB", None, Some(GainConvention::Audiophile)).unwrap();
        assert_eq!(radio.gain_db, audio.gain_db, "the tag value is the same");
        assert_ne!(
            radio.adjustment_db().signum(),
            audio.adjustment_db().signum(),
            "the two conventions must not produce the same adjustment"
        );
    }

    /// A positive radio value is still an attenuation. The convention decides
    /// the direction, not the sign of the number, because a badly-tagged file
    /// with a positive radio value exists and applying it as written would be
    /// worse than the consistent choice.
    #[test]
    fn a_positive_radio_value_is_still_attenuated() {
        let g = ReplayGain::parse("+3.0 dB", None, Some(GainConvention::Radio)).unwrap();
        assert_eq!(g.gain_db, 3.0);
        assert_eq!(g.adjustment_db(), -3.0, "convention decides direction");
    }

    #[test]
    fn a_tag_with_no_convention_is_used_as_written() {
        let g = ReplayGain::parse("-2.0 dB", None, None).unwrap();
        assert_eq!(g.adjustment_db(), -2.0);
    }

    /// The three spellings that occur in the wild, all meaning -7.03.
    #[test]
    fn gain_parses_in_every_spelling_found_in_the_wild() {
        for s in ["-7.03 dB", "-7.03dB", "-7.03DB", "-7.03", "  -7.03 dB  "] {
            let g = ReplayGain::parse(s, None, Some(GainConvention::Radio)).unwrap();
            assert!(
                (g.gain_db + 7.03).abs() < 0.001,
                "{s:?} parsed as {}",
                g.gain_db
            );
        }
    }

    /// A missing tag and a tag that parsed to zero are different facts. A
    /// player that treats "no tag" as "0 dB gain" is making an assumption it
    /// has no basis for, and the object has to be able to say which it has.
    #[test]
    fn an_unparseable_tag_is_none_not_zero() {
        assert!(ReplayGain::parse("", None, None).is_none());
        assert!(ReplayGain::parse("   ", None, None).is_none());
        assert!(
            ReplayGain::parse("dB", None, None).is_none(),
            "unit with no number"
        );
        assert!(ReplayGain::parse("loud", None, None).is_none());
        assert!(ReplayGain::parse("NaN", None, None).is_none(), "not finite");
        assert!(ReplayGain::parse("-7.03 dB", None, None).is_some());
    }

    #[test]
    fn a_parsed_zero_gain_is_a_real_value() {
        let g = ReplayGain::parse("0.00 dB", Some(-1.0), Some(GainConvention::Radio)).unwrap();
        assert_eq!(g.gain_db, 0.0);
        assert_eq!(g.convention, Some(GainConvention::Radio));
        assert_ne!(
            ReplayGain::default().convention,
            Some(GainConvention::Radio)
        );
    }

    // ---- clipping: the question a naive implementation gets wrong ----

    /// A file peaking at 0 dBFS can be attenuated and nothing else. This is
    /// the case a naive implementation gets wrong: it applies the file's own
    /// tagged gain and lets a limiter absorb the overs, so a file already at
    /// full scale is amplified straight into clipping.
    #[test]
    fn a_file_at_full_scale_can_be_attenuated_and_nothing_else() {
        let g = ReplayGain {
            gain_db: 3.0,
            peak_dbfs: 0.0,
            convention: Some(GainConvention::Audiophile),
        };
        assert!(g.peaks_at_full_scale());
        assert_eq!(g.headroom_db(), 0.0);
        assert_eq!(g.max_safe_boost_db(), 0.0, "there is nowhere to go");
        assert!(g.would_clip(0.1), "even a tenth of a dB clips");
        assert!(!g.would_clip(0.0), "exactly full scale is not over it");
        assert!(!g.would_clip(-3.0), "attenuating is always safe");
    }

    #[test]
    fn a_quiet_file_has_headroom_equal_to_its_peak_below_full_scale() {
        let g = ReplayGain {
            gain_db: 0.0,
            peak_dbfs: -12.0,
            convention: Some(GainConvention::Audiophile),
        };
        assert!(!g.peaks_at_full_scale());
        assert_eq!(g.headroom_db(), 12.0, "-12 peak is 12 dB of room");
        assert_eq!(g.max_safe_boost_db(), 12.0);
        assert!(!g.would_clip(6.0), "6 dB into 12 dB of room");
        assert!(!g.would_clip(12.0), "exactly the headroom is not over it");
        assert!(g.would_clip(12.1), "a tenth past it is");
    }

    /// The function a player actually calls, and the reason it exists: a file
    /// tagged with more gain than it can take must be clamped, not applied.
    #[test]
    fn a_safe_boost_for_a_target_is_clamped_to_what_the_file_can_take() {
        // Wants +6 dB to reach a -6 dBFS target. The file peaks at -8, so it
        // needs 2 dB to reach -6 -- comfortably inside its 8 dB of headroom.
        let g = ReplayGain {
            gain_db: 0.0,
            peak_dbfs: -8.0,
            convention: Some(GainConvention::Audiophile),
        };
        assert_eq!(g.safe_boost_for_target_db(-6.0), 2.0, "exactly enough");
        assert!(!g.would_clip(g.safe_boost_for_target_db(-6.0)));

        // Wants +10 dB on a file with only 3 dB of headroom. The clamp is the
        // point: applying 10 would peak at +2 and clip.
        let tight = ReplayGain {
            gain_db: 0.0,
            peak_dbfs: -3.0,
            convention: Some(GainConvention::Audiophile),
        };
        assert_eq!(tight.safe_boost_for_target_db(7.0), 3.0, "clamped to 3");
        assert!(!tight.would_clip(tight.safe_boost_for_target_db(7.0)));
        assert!(tight.would_clip(10.0), "the unclamped value would clip");
    }

    /// A file that already peaks at full scale gets zero boost regardless of
    /// what the target asks for.
    #[test]
    fn a_full_scale_file_gets_no_boost_for_any_target() {
        let g = ReplayGain {
            gain_db: 0.0,
            peak_dbfs: 0.0,
            convention: Some(GainConvention::Audiophile),
        };
        for target in [-12.0, -1.0, 0.0, 6.0] {
            let boost = g.safe_boost_for_target_db(target);
            assert!(
                !g.would_clip(boost),
                "target {target} produced boost {boost}"
            );
            assert!(boost <= 0.0, "a full-scale file is never boosted");
        }
    }

    /// A missing peak tag means the peak is unknown, which is not the same as
    /// "peaks at 0". Defaulting to 0 would refuse to boost anything;
    /// `headroom_db` reports the unknown peak honestly as 0 dB, and a caller
    /// that cares can check `convention`.
    #[test]
    fn an_absent_peak_is_reported_as_zero_headroom_not_as_a_known_peak() {
        let g = ReplayGain::parse("-3.0 dB", None, Some(GainConvention::Radio)).unwrap();
        assert_eq!(g.peak_dbfs, 0.0, "the default, which is conservative");
        assert_eq!(g.headroom_db(), 0.0);
        assert!(
            g.would_clip(0.1),
            "conservative: no boost until the peak is known"
        );
    }

    #[test]
    fn display_says_what_convention_was_read() {
        let g = ReplayGain::parse("-7.03 dB", Some(-1.2), Some(GainConvention::Radio)).unwrap();
        let s = g.to_string();
        assert!(s.contains("radio"), "{s}");
        assert!(s.contains("-7.03"), "{s}");
        assert!(s.contains("-1.20"), "the peak should be shown: {s}");
        assert_eq!(ReplayGain::default().to_string(), "none");
    }

    // ---- the envelope ----

    /// A helper: a waveform of `n` buckets, all at the same amplitude.
    fn flat(n: usize, value: i16) -> Waveform {
        Waveform::new(BUCKETS_PER_SECOND, 1000, vec![-value; n], vec![value; n])
    }

    #[test]
    fn an_envelope_round_trips_its_construction() {
        let w = flat(1024, 1000);
        assert_eq!(w.len(), 1024);
        assert!(!w.is_empty());
        assert_eq!(w.buckets_per_second(), BUCKETS_PER_SECOND);
        assert_eq!(w.duration_ms(), 1000);
        assert_eq!(w.bucket(0), Some((-1000, 1000)));
    }

    #[test]
    fn an_index_past_the_end_is_none_not_a_panic() {
        let w = flat(4, 100);
        assert_eq!(w.bucket(4), None);
        assert_eq!(w.bucket(9999), None);
        assert_eq!(w.display_height(4), None, "a gap, not a silent zero");
    }

    /// The dB scale is the point of the module. A linear plot of typical
    /// programme material at -12 dBFS would use 25% of the height; on the dB
    /// scale it fills most of it, which is the difference between a legible
    /// waveform and a flat line.
    #[test]
    fn the_display_scale_is_decibel_not_linear() {
        let full = amplitude_to_height(32767, -32768);
        let half_db = amplitude_to_height((32768.0 * 0.5012) as i16, 0);
        let quarter_db = amplitude_to_height((32768.0 * 0.2512) as i16, 0);

        assert!(
            (full - 1.0).abs() < 0.01,
            "full scale is full height: {full}"
        );
        // -6 dB is halfway on a dB scale. On a linear scale it would be 50% of
        // the height too -- so this alone does not distinguish them. The
        // distinguishing case is the quiet end:
        assert!(
            quarter_db > 0.0,
            "-12 dBFS must be visible, not zero: {quarter_db}"
        );
        // The -60 dB floor is 32768 * 10^(-60/20) = 32.7. A sample at 32 is
        // below it and renders as zero; 200 is -44 dB and is NOT below it,
        // which is the distinction an earlier version of this test got wrong.
        assert_eq!(amplitude_to_height(32, 0), 0.0, "at the floor is zero");
        assert_eq!(amplitude_to_height(16, 0), 0.0, "below the floor is zero");
        assert!(
            amplitude_to_height(33, 0) > 0.0,
            "one above the floor must be visible, or the floor is off by one"
        );
        assert!(
            amplitude_to_height(200, 0) > 0.0,
            "-44 dBFS is above the -60 floor and must be visible"
        );
        // A linear plot of -24 dBFS would be 6% of the height; the dB scale
        // gives it (60-24)/60 = 60%.
        let minus24 = amplitude_to_height((32768.0 * 0.0631) as i16, 0);
        assert!(
            minus24 > 0.5,
            "-24 dBFS should be over half height on a dB scale, got {minus24}"
        );
        let _ = half_db;
    }

    #[test]
    fn silence_is_zero_height() {
        assert_eq!(amplitude_to_height(0, 0), 0.0);
        assert_eq!(flat(4, 0).display_heights(), vec![0.0; 4]);
    }

    /// i16::MIN is the one value whose absolute value does not fit in i16.
    /// Doing the negation in i16 would panic or wrap.
    #[test]
    fn the_most_negative_sample_does_not_overflow() {
        let h = amplitude_to_height(0, i16::MIN);
        assert!(h > 0.99, "full scale from the negative end: {h}");
    }

    #[test]
    fn height_is_always_in_range() {
        for v in [1i16, 100, 1000, 16000, 32767, i16::MIN, i16::MAX] {
            // `neg` in i32, not `-v` in i16: negating i16::MIN does not fit and
            // this test is about the height function, not about i16 arithmetic.
            let neg = (v as i32).wrapping_neg() as i16;
            for (a, b) in [(v, 0), (0, v), (v, v), (v, neg), (neg, v)] {
                let h = amplitude_to_height(a, b);
                assert!(
                    (0.0..=1.0).contains(&h),
                    "height {h} out of range for {a}/{b}"
                );
            }
        }
    }

    #[test]
    fn the_louder_side_drives_the_height() {
        // An asymmetric bucket: the positive peak is the magnitude.
        assert_eq!(
            amplitude_to_height(20000, -100),
            amplitude_to_height(20000, 0)
        );
        assert!(
            amplitude_to_height(100, -20000) > amplitude_to_height(100, -100),
            "the negative side must count too"
        );
    }

    // ---- resampling ----

    /// A downsampled envelope must keep its peaks. Averaging here would lose
    /// every transient, which is the only thing a peak envelope is for.
    #[test]
    fn downsampling_keeps_peaks_rather_than_averaging_them() {
        // 4 buckets, only the middle one loud.
        let w = Waveform::new(
            BUCKETS_PER_SECOND,
            1000,
            vec![0, 0, -30000, 0],
            vec![0, 0, 30000, 0],
        );
        let r = w.resample(BUCKETS_PER_SECOND / 2);
        assert_eq!(r.buckets_per_second(), BUCKETS_PER_SECOND / 2);
        // Through the public accessor, not the private field: a test that
        // reaches past the API is testing the representation.
        let peak = (0..r.len())
            .max_by_key(|i| r.bucket(*i).map(|(_, hi)| hi).unwrap_or(i16::MIN))
            .unwrap();
        let loud_bucket = r.bucket(peak).unwrap();
        assert!(
            loud_bucket.1 > 29000,
            "the transient was averaged away: {loud_bucket:?}"
        );
    }

    #[test]
    fn resampling_to_the_same_rate_is_the_identity() {
        let w = flat(16, 500);
        assert_eq!(w.resample(BUCKETS_PER_SECOND), w);
    }

    #[test]
    fn a_zero_target_rate_is_ignored_rather_than_dividing_by_zero() {
        let w = flat(16, 500);
        assert_eq!(
            w.resample(0),
            w,
            "a 0 rate must not panic or empty the envelope"
        );
    }

    #[test]
    fn upsampling_produces_more_buckets_and_keeps_the_peak() {
        let w = flat(100, 20000);
        let r = w.resample(BUCKETS_PER_SECOND * 2);
        assert!(r.len() > w.len());
        assert!(
            r.peak_height() > 0.5,
            "the peak survived: {}",
            r.peak_height()
        );
    }

    #[test]
    fn resampling_a_zero_duration_envelope_is_the_identity() {
        let w = flat(0, 500);
        assert!(w.resample(100).is_empty());
    }

    #[test]
    fn a_very_short_file_still_gets_at_least_one_bucket() {
        // 1ms of audio at 1024/s is 1.024 buckets; integer arithmetic must not
        // produce zero and then divide by it.
        let w = Waveform::new(BUCKETS_PER_SECOND, 1, vec![-100], vec![100]);
        let r = w.resample(1);
        assert_eq!(r.len(), 1);
    }

    // ---- bucket lookup ----

    /// 1024 buckets per second means bucket `i` covers `[i, i+1)` ms. The
    /// last millisecond of a 1s file is bucket 1023 -- but the file's duration
    /// boundary, 1000ms, is past the end of the envelope and clamps there.
    #[test]
    fn a_timestamp_maps_to_its_bucket() {
        let w = flat(BUCKETS_PER_SECOND as usize, 100); // 1024 buckets = 1s
        assert_eq!(w.bucket_for_ms(0), 0);
        assert_eq!(w.bucket_for_ms(500), 512, "half a second is halfway");
        assert_eq!(
            w.bucket_for_ms(999),
            1022,
            "999ms is the 1023rd millisecond"
        );
        assert_eq!(w.bucket_for_ms(1000), 1023, "the end clamps to the last");
    }

    /// A marker past the end of the audio is real (a bad duration, a bad
    /// chapter list) and must not index off the end.
    #[test]
    fn a_timestamp_past_the_end_clamps_to_the_last_bucket() {
        let w = flat(BUCKETS_PER_SECOND as usize, 100);
        assert_eq!(w.bucket_for_ms(99_999), 1023);
    }

    #[test]
    fn a_zero_rate_envelope_does_not_divide_by_zero() {
        let w = Waveform::new(0, 1000, vec![-1], vec![1]);
        assert_eq!(w.bucket_for_ms(500), 0);
    }

    #[test]
    fn an_empty_envelope_clamps_to_nothing_rather_than_underflowing() {
        let w = Waveform::new(BUCKETS_PER_SECOND, 1000, vec![], vec![]);
        assert_eq!(w.bucket_for_ms(500), 0);
        assert_eq!(w.peak_height(), 0.0);
    }

    // ---- the object ----

    #[test]
    fn a_default_object_is_empty_not_claiming_a_track_zero() {
        let a = AudioObject::default();
        assert_eq!(a.track_number, None, "no number is not track zero");
        assert_eq!(a.duration_ms, 0);
        assert_eq!(a.replay_gain, ReplayGain::default());
    }

    /// An untagged track and a track numbered 0 are different facts, and the
    /// player navigates differently for them.
    #[test]
    fn track_zero_is_distinguishable_from_no_track_number() {
        let mut a = AudioObject::default();
        assert!(a.track_number.is_none());
        a.track_number = Some(0);
        assert_eq!(a.track_number, Some(0));
        assert_ne!(a.track_number, Some(1));
    }

    #[test]
    fn the_object_round_trips_through_json() {
        let a = AudioObject {
            duration_ms: 225_000,
            track_number: Some(3),
            track_total: Some(12),
            disc_number: Some(1),
            disc_total: Some(2),
            title: Some("Track".into()),
            artist: Some("Artist".into()),
            album: Some("Album".into()),
            replay_gain: ReplayGain::parse("-7.03 dB", Some(-1.0), Some(GainConvention::Radio))
                .unwrap(),
            sample_rate: Some(44100),
            channels: Some(2),
        };
        let json = serde_json::to_string(&a).unwrap();
        let back: AudioObject = serde_json::from_str(&json).unwrap();
        assert_eq!(a, back);
    }

    #[test]
    fn the_envelope_round_trips_through_json() {
        let w = Waveform::new(1024, 1000, vec![-100, -200, -300], vec![100, 200, 300]);
        let json = serde_json::to_string(&w).unwrap();
        let back: Waveform = serde_json::from_str(&json).unwrap();
        assert_eq!(w, back);
    }

    /// The deserialiser is where corrupt data arrives, so the min/max length
    /// invariant is enforced HERE rather than trusted from a constructor.
    #[test]
    fn an_envelope_with_mismatched_min_and_max_is_rejected() {
        let bad = r#"{"buckets_per_second":1024,"duration_ms":1000,"min":[1,2,3],"max":[1,2]}"#;
        let r: Result<Waveform, _> = serde_json::from_str(bad);
        assert!(r.is_err(), "a mismatched envelope must not deserialise");
    }

    #[test]
    fn an_envelope_with_a_zero_rate_is_rejected() {
        // A zero rate would make every bucket lookup return 0, which is a
        // silently wrong envelope rather than an error.
        let bad = r#"{"buckets_per_second":0,"duration_ms":1000,"min":[1],"max":[1]}"#;
        let r: Result<Waveform, _> = serde_json::from_str(bad);
        assert!(r.is_err(), "a zero buckets_per_second must not deserialise");
    }
}
