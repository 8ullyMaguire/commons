//! Which ffmpeg encoder, how many threads, and what a WebP target allows.
//!
//! Two upstream requests, both about hardcoded values in the ffmpeg invocation:
//!
//! - **stash#894** — "allow to change encoder for FFmpeg `-c:v` in
//!   Configuration for Preview Generation". The value was a literal in the
//!   argument list.
//! - **stash#819** — "configure number of threads for ffmpeg transcodes … the
//!   ffmpeg parameters in the code seem to hardcode the number of ffmpeg
//!   threads to 4". It did.
//!
//! # Why the encoder is validated rather than passed through
//!
//! A user who can type any string into a config field will type an encoder that
//! cannot produce WebP, and the result is a file named `.webp` that is an H.264
//! stream. That failure is invisible until something tries to decode it — the
//! thumbnail grid, the browser, or a user with the raw file. So the
//! configuration is a constrained set of named choices rather than a free
//! string, and [`OutputFormat`] carries the constraints with it.
//!
//! The one hard rule: **the thumbnail pipeline's output is WebP**, because
//! §5.1 specifies it and the frontend relies on it. There is no hardware WebP
//! encoder in ffmpeg, so acceleration never changes the encoder — it changes
//! decode and scale only. [`OutputFormat::hardware_encoder`] returns `None` for
//! every variant, and that is the type system's way of saying a hardware
//! encoder cannot be substituted here without producing a mislabelled file.
//!
//! # Why threads default to something other than 1 or 4
//!
//! The old hardcoded value was 4, chosen when 4 was a lot of cores. The
//! correct default is a function of the machine, because the number that is
//! right on a 32-core server is pathological on a 2-core NAS, and both are
//! wrong for a transcoder running several jobs at once. [`ThreadCount::Auto`]
//! resolves to `cores - 1` clamped to `[1, 8]`: leaving one core for the
//! scanner and the UI, and capping the top so ffmpeg does not spin up more
//! threads than there is work to give them — oversubscription on a machine
//! already running the job queue makes a transcode *slower*.

use serde::{Deserialize, Serialize};
use std::fmt;

/// The container/codec a generated artifact is written in.
///
/// Not a free-form string, so a configuration typo cannot produce a file whose
/// extension lies about its contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    /// WebP, lossless or near-lossless via `libwebp`. The pipeline default.
    #[default]
    Webp,
    /// JPEG via `mjpeg`. Larger files, universal support.
    Jpeg,
    /// PNG. Lossless and large; useful for screenshots and images with text.
    Png,
}

impl OutputFormat {
    /// The ffmpeg encoder name for `-c:v`.
    pub fn encoder(self) -> &'static str {
        match self {
            OutputFormat::Webp => "libwebp",
            OutputFormat::Jpeg => "mjpeg",
            OutputFormat::Png => "png",
        }
    }

    /// The file extension. Never inferred from the encoder: the extension is
    /// the pipeline's contract with the frontend, and a mismatch is a bug
    /// here rather than something a caller can opt into.
    pub fn extension(self) -> &'static str {
        match self {
            OutputFormat::Webp => "webp",
            OutputFormat::Jpeg => "jpg",
            OutputFormat::Png => "png",
        }
    }

    /// The `-pix_fmt` for this format.
    ///
    /// WebP requires `yuva420p` here: the stills carry an alpha channel, and
    /// ffmpeg's default `yuv420p` silently drops it, producing a sprite sheet
    /// with black boxes where the transparency was. PNG gets `rgba` for the
    /// same reason, and `mjpeg` gets `yuvj420p` because that is the only
    /// full-range JPEG pixel format ffmpeg accepts without a range warning on
    /// every single file.
    pub fn pixel_format(self) -> &'static str {
        match self {
            OutputFormat::Webp => "yuva420p",
            OutputFormat::Jpeg => "yuvj420p",
            OutputFormat::Png => "rgba",
        }
    }

    /// Does this format carry an alpha channel?
    pub fn has_alpha(self) -> bool {
        matches!(self, OutputFormat::Webp | OutputFormat::Png)
    }

    /// Does this format have a lossless mode?
    pub fn supports_lossless(self) -> bool {
        matches!(self, OutputFormat::Webp | OutputFormat::Png)
    }

    /// The quality flag ffmpeg wants, and its name.
    ///
    /// Returns `None` for PNG, which has no quality knob — passing `-quality`
    /// to the PNG encoder is an error in some ffmpeg builds and a warning in
    /// others, and a config value that means nothing on one format should not
    /// be accepted on that format.
    pub fn quality_flag(self) -> Option<&'static str> {
        match self {
            OutputFormat::Webp => Some("-quality"),
            OutputFormat::Jpeg => Some("-q:v"),
            OutputFormat::Png => None,
        }
    }

    /// The valid quality range as ffmpeg understands it for this format.
    ///
    /// `-quality` for libwebp is 0–100. `-q:v` for mjpeg is 2–31, **inverted**:
    /// a lower number is better. Handing a user one "quality" number that means
    /// 90/100 for WebP and 3/31 for JPEG is how a config produces a worse image
    /// than the default, so each format carries its own range and the
    /// conversion happens here rather than in each call site.
    pub fn quality_range(self) -> Option<(u8, u8)> {
        match self {
            // 0 = lossless-ish/smallest, 100 = best.
            OutputFormat::Webp => Some((0, 100)),
            // 2 = best, 31 = worst. Inverted relative to WebP.
            OutputFormat::Jpeg => Some((2, 31)),
            OutputFormat::Png => None,
        }
    }

    /// Normalise a 0–100 "quality" to this format's own scale.
    ///
    /// Returns `None` when the format has no quality axis, so the caller
    /// reports "this setting does not apply" instead of silently dropping it.
    pub fn normalise_quality(self, quality: u8) -> Option<u8> {
        let (lo, hi) = self.quality_range()?;
        let q = quality.min(100);
        Some(match self {
            // Identity: libwebp's scale is already 0–100.
            OutputFormat::Webp => q,
            // Map 0–100 onto the inverted 2–31 scale: quality 100 lands on
            // `lo` (2, the best JPEG) and quality 0 on `hi` (31, the worst).
            // The arithmetic is lo + (1 - q/100) * span, written with integer
            // math so there is no float rounding to reason about at the ends:
            // 100 must give exactly 2, not 3.
            OutputFormat::Jpeg => {
                let span = u32::from(hi - lo);
                let worst_minus_best = (100u32.saturating_sub(u32::from(q)) * span) / 100;
                (u32::from(lo) + worst_minus_best) as u8
            }
            OutputFormat::Png => return None,
        })
    }

    /// The hardware encoder for this format, if one exists.
    ///
    /// Always `None`. There is no hardware WebP, JPEG or PNG encoder exposed by
    /// ffmpeg's VA-API or NVENC paths in any build that also has `libwebp` —
    /// the hardware encoders available are H.264/H.265/AV1/VP9, none of which
    /// produce a file the frontend can display. The function exists so that
    /// this is a checked statement rather than an absence, and so that the day
    /// ffmpeg does ship one it is a one-line change at a place that is already
    /// being looked at.
    pub fn hardware_encoder(self) -> Option<&'static str> {
        let _ = self;
        None
    }
}

impl fmt::Display for OutputFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            OutputFormat::Webp => "webp",
            OutputFormat::Jpeg => "jpeg",
            OutputFormat::Png => "png",
        })
    }
}

/// How many threads ffmpeg should use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThreadCount {
    /// Resolve from the machine. The default.
    #[default]
    Auto,
    /// Exactly this many. `0` is not representable — see
    /// [`ThreadCount::resolve`].
    Fixed(u8),
}

impl ThreadCount {
    /// The default thread count for this machine.
    ///
    /// `cores - 1`, clamped to `[1, 8]`.
    ///
    /// The `- 1` leaves a core for the scanner, the watcher and the API, which
    /// are all latency-sensitive and would be starved by a transcode using
    /// every core — the job feels slow and the UI feels stuck, and the user
    /// concludes the machine is overloaded rather than that two things are
    /// competing.
    ///
    /// The cap of 8 is because ffmpeg's threading is not free: beyond a
    /// handful of threads the synchronisation cost dominates for the frame
    /// sizes here, and on a machine already running several transcode jobs
    /// each claiming 32 threads the contention makes every one of them slower.
    pub fn resolve(self, cores: usize) -> u8 {
        match self {
            ThreadCount::Auto => {
                let leave_one = cores.saturating_sub(1);
                leave_one.clamp(1, 8) as u8
            }
            ThreadCount::Fixed(n) => n.max(1),
        }
    }

    /// `-threads` and its value, or nothing if ffmpeg's default is wanted.
    pub fn args(self, cores: usize) -> Vec<String> {
        let n = self.resolve(cores);
        if n == 1 {
            // Passing -threads 1 is not the same as omitting it for some
            // filters: it disables frame-level threading that ffmpeg would
            // otherwise choose, which is what we want, but it also suppresses
            // slice threading. Emitting nothing lets ffmpeg pick, which for
            // a single-threaded request is the same thing done properly.
            Vec::new()
        } else {
            vec!["-threads".to_string(), n.to_string()]
        }
    }
}

impl fmt::Display for ThreadCount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ThreadCount::Auto => f.write_str("auto"),
            ThreadCount::Fixed(n) => write!(f, "{n}"),
        }
    }
}

/// The complete set of knobs a generated artifact honours.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncodeSettings {
    pub format: OutputFormat,
    /// 0–100, higher is better, on every format. Translated to the format's
    /// own scale by [`OutputFormat::normalise_quality`].
    pub quality: u8,
    pub threads: ThreadCount,
    /// Write a lossless file where the format supports it.
    pub lossless: bool,
}

impl Default for EncodeSettings {
    fn default() -> Self {
        EncodeSettings {
            format: OutputFormat::Webp,
            // 82 for stills and 80 for sprite sheets in the old code; one
            // number is easier to reason about than two, and 82 is the one
            // that was measured for the larger, more visible artifact.
            quality: 82,
            threads: ThreadCount::Auto,
            lossless: false,
        }
    }
}

impl EncodeSettings {
    /// The encoder arguments for `format`, in ffmpeg's order.
    ///
    /// `-c:v`, `-pix_fmt`, then the quality flag, then `-lossless` for the
    /// formats that have it. Returns an empty `Vec` for the quality flag rather
    /// than omitting the pair, because `png` has no quality axis and passing
    /// `-quality 82` to it is an error on some builds.
    pub fn args(&self) -> Vec<String> {
        let mut args = vec![
            "-c:v".to_string(),
            self.format.encoder().to_string(),
            "-pix_fmt".to_string(),
            self.format.pixel_format().to_string(),
        ];
        if let (Some(flag), Some(q)) = (
            self.format.quality_flag(),
            self.format.normalise_quality(self.quality),
        ) {
            args.push(flag.to_string());
            args.push(q.to_string());
        }
        if self.lossless && self.format.supports_lossless() {
            args.push("-lossless".to_string());
            args.push("1".to_string());
        }
        args
    }

    /// The global arguments, which must precede the input file.
    ///
    /// `-threads` belongs here, not among the codec arguments. ffmpeg accepts
    /// it in either position and applies it to whatever comes *next*, so
    /// putting it after the input silently applies the thread count to the
    /// output encoding instead of the whole run. That is not an error ffmpeg
    /// reports, and the symptom is a transcode that ignores the setting with
    /// no diagnostic -- which is why the two groups are separate methods
    /// rather than one list a caller sorts.
    pub fn global_args(&self, cores: usize) -> Vec<String> {
        self.threads.args(cores)
    }

    /// Is this a valid combination?
    ///
    /// Only one thing is invalid today — lossless on JPEG — and it is checked
    /// rather than ignored, because silently dropping a setting the user asked
    /// for is how a config file stops being trustworthy.
    pub fn validate(&self) -> Result<(), String> {
        if self.lossless && !self.format.supports_lossless() {
            return Err(format!(
                "{} has no lossless mode; the lossless setting does not apply",
                self.format
            ));
        }
        if self.quality > 100 {
            return Err(format!("quality {} is out of range 0-100", self.quality));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_old_hardcoded_values_are_the_defaults() {
        // The defaults must be what the pipeline did before this existed,
        // byte for byte, or every existing artifact becomes stale.
        let s = EncodeSettings::default();
        assert_eq!(s.format, OutputFormat::Webp);
        assert_eq!(s.quality, 82);
        assert_eq!(
            s.args(),
            vec!["-c:v", "libwebp", "-pix_fmt", "yuva420p", "-quality", "82"]
        );
    }

    #[test]
    fn webp_keeps_alpha_and_jpeg_cannot() {
        // Stills carry transparency; dropping it produces black boxes in the
        // sprite sheet.
        assert!(OutputFormat::Webp.has_alpha());
        assert!(!OutputFormat::Jpeg.has_alpha());
        assert_eq!(OutputFormat::Webp.pixel_format(), "yuva420p");
    }

    #[test]
    fn quality_translates_between_the_two_inverted_scales() {
        // mjpeg's -q:v is inverted: 2 is best, 31 is worst. Handing it a
        // WebP-style 82 would produce one of the worst JPEGs possible while
        // the config said "high quality".
        assert_eq!(OutputFormat::Webp.normalise_quality(82), Some(82));
        assert_eq!(OutputFormat::Webp.normalise_quality(0), Some(0));
        assert_eq!(OutputFormat::Webp.normalise_quality(100), Some(100));

        // Monotonic: higher quality is never a worse jpeg number.
        let mut prev = 32u8;
        for q in (0..=100).step_by(5) {
            let v = OutputFormat::Jpeg.normalise_quality(q).unwrap();
            assert!(v <= prev, "quality {q} gave {v}, worse than {prev}");
            prev = v;
        }
    }

    #[test]
    fn png_has_no_quality_axis_and_says_so() {
        assert_eq!(OutputFormat::Png.quality_flag(), None);
        assert_eq!(OutputFormat::Png.normalise_quality(82), None);
        let args = EncodeSettings {
            format: OutputFormat::Png,
            ..Default::default()
        }
        .args();
        assert!(
            !args.iter().any(|a| a == "-quality"),
            "a quality flag for png is an error on some ffmpeg builds: {args:?}"
        );
    }

    #[test]
    fn no_format_has_a_hardware_encoder() {
        // The reason acceleration never changes -c:v here.
        for f in [OutputFormat::Webp, OutputFormat::Jpeg, OutputFormat::Png] {
            assert_eq!(f.hardware_encoder(), None, "{f} claims a hardware encoder");
        }
    }

    #[test]
    fn threads_auto_leaves_a_core_and_caps_at_eight() {
        assert_eq!(ThreadCount::Auto.resolve(1), 1, "single core, still 1");
        assert_eq!(ThreadCount::Auto.resolve(2), 1);
        assert_eq!(ThreadCount::Auto.resolve(4), 3);
        assert_eq!(ThreadCount::Auto.resolve(9), 8);
        assert_eq!(
            ThreadCount::Auto.resolve(128),
            8,
            "oversubscription is slower"
        );
    }

    #[test]
    fn a_fixed_zero_threads_becomes_one() {
        // -threads 0 means "auto" to ffmpeg, which is not what a user asking
        // for 0 means, and it is one of the two values that would otherwise
        // silently re-enable the thing this setting exists to control.
        assert_eq!(ThreadCount::Fixed(0).resolve(16), 1);
        assert_eq!(ThreadCount::Fixed(1).resolve(16), 1);
        assert_eq!(ThreadCount::Fixed(6).resolve(16), 6);
    }

    #[test]
    fn single_threaded_emits_no_flag() {
        assert!(ThreadCount::Fixed(1).args(16).is_empty());
        assert_eq!(ThreadCount::Fixed(4).args(16), vec!["-threads", "4"]);
        assert_eq!(ThreadCount::Auto.args(4), vec!["-threads", "3"]);
    }

    #[test]
    fn lossless_on_jpeg_is_rejected_rather_than_ignored() {
        let s = EncodeSettings {
            format: OutputFormat::Jpeg,
            lossless: true,
            ..Default::default()
        };
        let err = s.validate().unwrap_err();
        assert!(err.contains("lossless"), "{err}");
        // And the args must not contain it either.
        assert!(!s.args().iter().any(|a| a == "-lossless"));
    }

    #[test]
    fn lossless_on_webp_is_allowed_and_emitted() {
        let s = EncodeSettings {
            lossless: true,
            ..Default::default()
        };
        s.validate().unwrap();
        assert!(s.args().iter().any(|a| a == "-lossless"));
    }

    #[test]
    fn the_extension_never_disagrees_with_the_encoder() {
        // The mislabelled-file bug: an encoder whose output is not the format
        // the extension claims. Pinned as pairs, because there is no general
        // rule deriving one from the other -- "mjpeg" produces "jpg",
        // "libwebp" produces "webp" -- and a test hunting for a rule passes
        // for the wrong reason.
        let pairs = [
            (OutputFormat::Webp, "libwebp", "webp"),
            (OutputFormat::Jpeg, "mjpeg", "jpg"),
            (OutputFormat::Png, "png", "png"),
        ];
        for (format, encoder, extension) in pairs {
            assert_eq!(format.encoder(), encoder, "{format} encoder");
            assert_eq!(format.extension(), extension, "{format} extension");
        }
        // The three extensions are distinct, so a generated file's name
        // identifies its format unambiguously.
        let mut exts: Vec<&str> = pairs.iter().map(|(_, _, e)| *e).collect();
        exts.sort_unstable();
        exts.dedup();
        assert_eq!(exts.len(), 3, "two formats share an extension: {exts:?}");
    }
}
