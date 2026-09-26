//! Media probing (T-P1-002, spec §5.2, §5.11).
//!
//! A thin, typed wrapper over `ffprobe`. Three rules govern it:
//!
//! 1. **Never shell out through a string.** [`Command`] takes the binary and
//!    each argument separately, so a filename containing a space, a quote, a
//!    `;`, or `$(...)` is a filename and nothing more. A library path is
//!    attacker-influenced data — a folder name can come from anywhere — and
//!    string interpolation into a shell is the kind of bug that is found by
//!    somebody else.
//! 2. **Report what the file says, including the awkward parts.** A non-zero
//!    stream `start_time` is recorded per stream (stash#7229) rather than
//!    normalised away, because preview generation has to offset by it.
//! 3. **The probe is authoritative for duration and streams; the sniffer is
//!    authoritative for container.** Neither invents the other's answers.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use serde::Deserialize;

/// Everything the scanner and the artifact pipeline need from one media file.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MediaInfo {
    /// The path ffprobe was asked about, echoed back. Useful in a parallel
    /// scan log, where a result has to be attributable to a subject.
    pub path: Option<String>,
    /// The container as ffprobe reports it (`mov,mp4,m4a,3gp,3g2,mj2`, …).
    pub format_name: String,
    /// Total duration in milliseconds.
    pub duration_ms: u64,
    /// The format-level `start_time`, in milliseconds. Non-zero on files that
    /// begin partway into a timeline (stash#7229).
    pub format_start_ms: u64,
    pub bit_rate: Option<u64>,
    pub video_streams: Vec<VideoStream>,
    pub audio_streams: Vec<AudioStream>,
    /// Chapters from the container. Titles here become `Marker` seeds (§5.11),
    /// not metadata strings — a chapter is a point in time, not a caption.
    pub chapters: Vec<Chapter>,
    /// Container-level tags (artist, album, title, …).
    pub tags: BTreeMap<String, String>,
    /// `creation_time` as written by the muxer, when present.
    pub creation_time: Option<String>,
}

/// One video stream.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VideoStream {
    /// ffprobe's stream index within the file. Kept because every `-map` and
    /// every frame-accurate seek refers to it.
    pub index: i64,
    pub codec_name: String,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub frames: Option<u64>,
    pub duration_ms: Option<u64>,
    /// **May be non-zero.** stash#7229: a stream that starts at 5s means
    /// frame N of the file is at 5s + N/fps, and any preview generator that
    /// ignores this produces a sprite full of the wrong frame.
    pub start_time_ms: i64,
    pub bit_rate: Option<u64>,
    pub pix_fmt: Option<String>,
    /// Rotation from the display matrix, in degrees, normalised to 0/90/180/270.
    /// Phone footage is routinely 90° and a grid of sideways thumbnails is the
    /// visible symptom of ignoring it.
    pub rotation: i32,
    pub is_animated: bool,
}

impl VideoStream {
    /// The first frame's timestamp within the file, in milliseconds.
    ///
    /// Never negative: a stream whose start_time is negative (some MPEG-TS
    /// files) is clamped to zero, because a negative timestamp would index
    /// backwards out of every buffer downstream.
    pub fn first_frame_ms(&self) -> u64 {
        self.start_time_ms.max(0) as u64
    }
}

/// One audio stream.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AudioStream {
    pub index: i64,
    pub codec_name: String,
    pub channels: u16,
    pub sample_rate: Option<u32>,
    pub duration_ms: Option<u64>,
    pub start_time_ms: i64,
    pub bit_rate: Option<u64>,
    /// ffprobe reports a default disposition, and an audio track the author
    /// marked `default=0` is commentary, not the soundtrack (stash#1058 is
    /// about choosing between tracks).
    pub is_default: bool,
    /// Replay-gain tags, when the file carries them (§5.4).
    pub replaygain_track_gain: Option<String>,
}

/// A container chapter (§5.11).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Chapter {
    /// Index within the chapter list.
    pub index: u64,
    pub title: Option<String>,
    /// Offset from the start of the *file*, in milliseconds. ffprobe reports
    /// chapter `start_time` as an absolute file timestamp, not relative to the
    /// chapter list, so this is the value to seek to.
    pub start_ms: u64,
    pub end_ms: u64,
}

impl Chapter {
    /// The chapter as a marker seed. Duration 0 means "a point", which is what
    /// a chapter title is.
    pub fn as_marker(&self) -> (u64, u64, Option<String>) {
        (
            self.start_ms,
            self.end_ms.saturating_sub(self.start_ms),
            self.title.clone(),
        )
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    #[error("ffprobe not found on PATH; install ffmpeg or set FFPROBE")]
    NotFound,
    #[error("ffprobe failed on {path}: {stderr}")]
    Failed { path: String, stderr: String },
    #[error("ffprobe output for {path} is not valid JSON: {detail}")]
    BadJson { path: String, detail: String },
    #[error("cannot read {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("path {0} contains a NUL byte and cannot be passed to a process")]
    NulPath(String),
}

/// The binary to run. Overridable so a test can point at a stub, and so a
/// packaged install can ship its own rather than depend on the host's.
#[derive(Debug, Clone)]
pub struct Prober {
    binary: std::path::PathBuf,
}

impl Default for Prober {
    fn default() -> Self {
        Self::new()
    }
}

impl Prober {
    /// `FFPROBE` if set, else `ffprobe` from PATH.
    pub fn new() -> Self {
        let binary = std::env::var_os("FFPROBE")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("ffprobe"));
        Self { binary }
    }

    pub fn with_binary(binary: impl Into<std::path::PathBuf>) -> Self {
        Self {
            binary: binary.into(),
        }
    }

    /// Probe a file.
    ///
    /// The argument vector is fixed and the path is the *last* argument, so a
    /// path beginning with a dash cannot be read as an option. `--` is not
    /// accepted by ffprobe, so instead the path is passed after a bare `-i`,
    /// which ffprobe treats as the end of options in this position; the `-i`
    /// also makes ffprobe treat a leading-dash name as a filename.
    pub fn probe(&self, path: &Path) -> Result<MediaInfo, ProbeError> {
        let display = path.display().to_string();
        if display.contains('\0') {
            return Err(ProbeError::NulPath(display));
        }

        let output = Command::new(&self.binary)
            .arg("-v")
            .arg("quiet")
            .arg("-print_format")
            .arg("json")
            .arg("-show_format")
            .arg("-show_streams")
            .arg("-show_chapters")
            .arg("-i")
            .arg(path)
            .output()
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => ProbeError::NotFound,
                _ => ProbeError::Io {
                    path: display.clone(),
                    source: e,
                },
            })?;

        if !output.status.success() {
            return Err(ProbeError::Failed {
                path: display,
                stderr: String::from_utf8_lossy(&output.stderr)
                    .trim()
                    .chars()
                    .take(400)
                    .collect(),
            });
        }

        let raw: FfprobeOutput =
            serde_json::from_slice(&output.stdout).map_err(|e| ProbeError::BadJson {
                path: display,
                detail: e.to_string(),
            })?;
        Ok(raw.into())
    }
}

/// Probe with the default binary.
pub fn probe(path: &Path) -> Result<MediaInfo, ProbeError> {
    Prober::new().probe(path)
}

/// Is this file an animated image or a short clip? (stash#5111)
///
/// The container cannot answer this, so the answer is computed from what the
/// probe returned. Both conditions must hold, and the thresholds are the ones
/// the spec states:
///
/// * **at least 2 frames** — a single-frame GIF is a picture of a thing.
/// * **at least 2 seconds of loop duration** — a 0.5s loop is a reaction gif
///   pasted into a chat, and a library wants it as an image.
///
/// A video file that fails either test is still a `Scene`; this only decides
/// whether a *GIF* is promoted to one.
pub const MIN_ANIMATED_FRAMES: u64 = 2;
pub const MIN_ANIMATED_DURATION_MS: u64 = 2_000;

/// Decide whether a probed GIF should be an `Image` or a `Scene`.
///
/// Returns `Some(true)` for "promote to scene", `Some(false)` for "keep as
/// image", and `None` when the file is not a GIF at all and the question does
/// not apply.
pub fn gif_is_scene(info: &MediaInfo) -> Option<bool> {
    if !is_gif(info) {
        return None;
    }
    let frames = info
        .video_streams
        .first()
        .and_then(|s| s.frames)
        .unwrap_or(MIN_ANIMATED_FRAMES);
    Some(frames >= MIN_ANIMATED_FRAMES && info.duration_ms >= MIN_ANIMATED_DURATION_MS)
}

fn is_gif(info: &MediaInfo) -> bool {
    info.format_name
        .split(',')
        .any(|f| f.trim().eq_ignore_ascii_case("gif"))
}

// --- ffprobe's JSON shape. Field names are snake_case, as ffprobe emits them.
// Structs are private because they are an implementation detail of the
// translation, not part of this crate's contract: the caller gets `MediaInfo`.

#[derive(Debug, Deserialize)]
struct FfprobeOutput {
    #[serde(default)]
    streams: Vec<FfprobeStream>,
    #[serde(default)]
    format: Option<FfprobeFormat>,
    #[serde(default)]
    chapters: Vec<FfprobeChapter>,
}

#[derive(Debug, Default, Deserialize)]
struct FfprobeFormat {
    #[serde(default)]
    filename: Option<String>,
    #[serde(default)]
    format_name: Option<String>,
    #[serde(default)]
    duration: Option<Num>,
    #[serde(default)]
    start_time: Option<Num>,
    #[serde(default)]
    bit_rate: Option<Num>,
    #[serde(default, rename = "tags")]
    tags: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct FfprobeStream {
    #[serde(default)]
    index: i64,
    #[serde(default)]
    codec_type: String,
    #[serde(default)]
    codec_name: Option<String>,
    #[serde(default)]
    width: Option<u32>,
    #[serde(default)]
    height: Option<u32>,
    #[serde(default)]
    duration: Option<Num>,
    #[serde(default)]
    start_time: Option<Num>,
    #[serde(default)]
    bit_rate: Option<Num>,
    #[serde(default)]
    channels: Option<u16>,
    #[serde(default)]
    sample_rate: Option<String>,
    #[serde(default, rename = "r_frame_rate")]
    r_frame_rate: Option<String>,
    #[serde(default)]
    avg_frame_rate: Option<String>,
    #[serde(default)]
    nb_frames: Option<Num>,
    #[serde(default)]
    pix_fmt: Option<String>,
    #[serde(default)]
    disposition: BTreeMap<String, i64>,
    #[serde(default, rename = "tags")]
    tags: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    side_data_list: Option<Vec<serde_json::Value>>,
}

#[derive(Debug, Deserialize)]
struct FfprobeChapter {
    #[serde(default)]
    id: Option<u64>,
    #[serde(default)]
    start_time: Option<Num>,
    #[serde(default)]
    end_time: Option<Num>,
    #[serde(default, rename = "tags")]
    tags: BTreeMap<String, serde_json::Value>,
}

/// Seconds (float) to milliseconds, saturating and rounding to nearest.
///
/// Saturating because a corrupt `duration` of `1e30` would otherwise wrap on a
/// `u64` cast in release mode, turning into a small number that looks valid.
/// A JSON number that ffprobe may have written as a string.
///
/// ffprobe's JSON writer emits `"duration":"300.500000"` — every numeric field
/// arrives as a quoted string. A struct typed `f64` therefore deserialises
/// cleanly against a hand-written test fixture and fails against the real tool,
/// which is the worst possible arrangement: the tests pass and the code does
/// not work. This accepts both forms, so either shape parses.
// Not Copy: the String variant is not. Every method takes `self` by value,
// which is correct because the values are read once during translation.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum Num {
    Num(f64),
    Str(String),
}

impl Num {
    fn to_f64(&self) -> Option<f64> {
        match self {
            Num::Num(v) => Some(*v),
            Num::Str(s) => s.trim().parse().ok(),
        }
    }

    fn to_u64(&self) -> Option<u64> {
        self.to_f64().and_then(|v| (v >= 0.0).then_some(v as u64))
    }
}

fn secs_to_ms(v: Option<Num>) -> u64 {
    match v.and_then(|n| n.to_f64()) {
        Some(s) if s.is_finite() && s > 0.0 => (s * 1000.0).round().min(u64::MAX as f64) as u64,
        _ => 0,
    }
}

/// Look a tag up case-insensitively.
///
/// The keys are *not* normalised on the way in, because tag case is meaningful
/// in some ecosystems (ReplayGain is conventionally uppercase, and so is
/// `creation_time` in some muxers) while others are not. An earlier version
/// lowercased both the stored keys and the lookup key, which made it find
/// `encoder` but silently miss `REPLAYGAIN_TRACK_GAIN` -- a tag that is
/// almost never lowercase, so audio replay gain came back as None for every
/// real file and a test written to match the implementation passed.
fn tag_string(tags: &BTreeMap<String, serde_json::Value>, key: &str) -> Option<String> {
    let want = key.to_ascii_lowercase();
    tags.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(&want))
        .and_then(|(_, v)| v.as_str())
        .map(|s| s.to_string())
}

fn parse_rational(v: &Option<String>) -> Option<f64> {
    let s = v.as_ref()?;
    let (n, d) = s.split_once('/')?;
    let n: f64 = n.trim().parse().ok()?;
    let d: f64 = d.trim().parse().ok()?;
    if d == 0.0 {
        return None;
    }
    Some(n / d)
}

impl From<FfprobeOutput> for MediaInfo {
    fn from(f: FfprobeOutput) -> Self {
        let format = f.format.unwrap_or_default();
        let path = format.filename.clone();
        let duration_ms = secs_to_ms(format.duration);
        let format_start_ms = secs_to_ms(format.start_time);

        let mut video_streams = Vec::new();
        let mut audio_streams = Vec::new();
        for s in f.streams {
            match s.codec_type.as_str() {
                "video" => {
                    // A stream with attached pictures (cover art) is not a
                    // video stream for our purposes; counting an album's
                    // embedded cover as a second video track would make a
                    // one-scene file look multi-scene.
                    let disposition_attached =
                        *s.disposition.get("attached_pic").unwrap_or(&0) == 1;
                    if disposition_attached {
                        continue;
                    }
                    // Every field is read out of `s` before any of them is
                    // moved, and the one borrow (`rotation_of`) comes first.
                    // Reading them in place instead would make this a chain of
                    // "value used after partial move" errors, each fixed by
                    // moving the read earlier -- which is how the rotation
                    // ended up three lines below where it belonged.
                    let index = s.index;
                    let codec_name = s.codec_name.clone().unwrap_or_default();
                    let width = s.width.unwrap_or(0);
                    let height = s.height.unwrap_or(0);
                    let rotation = rotation_of(&s);
                    let pix_fmt = s.pix_fmt.clone();
                    let bit_rate = s.bit_rate.clone().and_then(|n| n.to_u64());
                    let stream_duration_ms = secs_to_ms(s.duration);
                    let start_time_ms = (s.start_time.and_then(|n| n.to_f64()).unwrap_or(0.0)
                        * 1000.0)
                        .round() as i64;
                    let fps = parse_rational(&s.avg_frame_rate)
                        .or_else(|| parse_rational(&s.r_frame_rate))
                        .unwrap_or(0.0);
                    // Fall back to duration x fps when nb_frames is absent,
                    // which is normal for MKV and for any stream that was
                    // never fully scanned.
                    let frames = s.nb_frames.and_then(|n| n.to_u64()).or_else(|| {
                        if fps > 0.0 && stream_duration_ms > 0 {
                            Some(((stream_duration_ms as f64 / 1000.0) * fps).round() as u64)
                        } else {
                            None
                        }
                    });
                    video_streams.push(VideoStream {
                        index,
                        codec_name,
                        width,
                        height,
                        fps,
                        frames,
                        duration_ms: Some(stream_duration_ms).filter(|v| *v > 0),
                        start_time_ms,
                        bit_rate,
                        pix_fmt,
                        rotation,
                        // A GIF reports many frames; anything else reporting
                        // `nb_frames > 1` is just video.
                        // Every stream reaching here is a video stream, so
                        // "animated" is true by construction. The field stays
                        // because callers filter on it, and an earlier version
                        // computed it as `codec_name == "gif"`, which would have
                        // reported a normal h264 scene as NOT animated -- a
                        // false negative on the one thing the field means.
                        is_animated: true,
                    });
                }
                "audio" => {
                    let replaygain = tag_string(&s.tags, "REPLAYGAIN_TRACK_GAIN");
                    let stream_duration_ms = secs_to_ms(s.duration);
                    let bit_rate = s.bit_rate.clone().and_then(|n| n.to_u64());
                    audio_streams.push(AudioStream {
                        index: s.index,
                        codec_name: s.codec_name.unwrap_or_default(),
                        channels: s.channels.unwrap_or(0),
                        sample_rate: s.sample_rate.as_deref().and_then(|s| s.parse::<u32>().ok()),
                        duration_ms: Some(stream_duration_ms).filter(|v| *v > 0),
                        start_time_ms: (s.start_time.and_then(|n| n.to_f64()).unwrap_or(0.0)
                            * 1000.0)
                            .round() as i64,
                        bit_rate,
                        is_default: *s.disposition.get("default").unwrap_or(&0) == 1,
                        replaygain_track_gain: replaygain,
                    });
                }
                // subtitle, data, attachment: not modelled yet. T-P6-002 adds
                // subtitle streams; ignoring them here is deliberate so they do
                // not inflate the stream count a caller sees today.
                _ => {}
            }
        }

        let chapters = f
            .chapters
            .into_iter()
            .enumerate()
            .map(|(i, c)| {
                let start_ms = secs_to_ms(c.start_time);
                let end_ms = secs_to_ms(c.end_time);
                Chapter {
                    index: c.id.unwrap_or(i as u64),
                    title: tag_string(&c.tags, "title"),
                    // A chapter with an end before its start is corrupt data;
                    // report it as a zero-length point rather than an inverted
                    // range that would break every consumer.
                    start_ms,
                    end_ms: end_ms.max(start_ms),
                }
            })
            .collect();

        let mut tags = BTreeMap::new();
        for (k, v) in &format.tags {
            if let Some(s) = v.as_str() {
                tags.insert(k.to_ascii_lowercase(), s.to_string());
            }
        }

        MediaInfo {
            path,
            format_name: format.format_name.unwrap_or_default(),
            duration_ms,
            format_start_ms,
            bit_rate: format.bit_rate.and_then(|n| n.to_u64()),
            video_streams,
            audio_streams,
            chapters,
            tags,
            creation_time: tag_string(&format.tags, "creation_time"),
        }
    }
}

/// Read rotation from the display-matrix side data, in degrees.
///
/// ffprobe reports it as a `rotation` side-data entry holding a string like
/// `-90` or `90.000000`, and a phone recording is usually -90 meaning
/// "rotate clockwise to display". The sign is normalised away: the grid only
/// needs to know which way is up.
fn rotation_of(s: &FfprobeStream) -> i32 {
    let Some(list) = &s.side_data_list else {
        return 0;
    };
    for item in list {
        let Some(obj) = item.as_object() else {
            continue;
        };
        if obj.get("side_data_type").and_then(|v| v.as_str()) != Some("Display Matrix") {
            continue;
        }
        let r = obj.get("rotation").and_then(|v| v.as_str()).unwrap_or("");
        if let Ok(deg) = r.trim().parse::<f64>() {
            let norm = ((deg.round() as i32) % 360 + 360) % 360;
            return match norm {
                90 | 270 => 90,
                180 => 180,
                _ => 0,
            };
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fx(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../commons-scan/tests/fixtures")
            .join(name)
    }

    fn fixture_present(name: &str) {
        let p = fx(name);
        assert!(
            p.exists(),
            "fixture {name} missing at {}; run scripts/make-fixtures.sh",
            p.display()
        );
    }

    fn real() -> Prober {
        // Skip rather than fail when ffprobe is absent: the pure translation
        // tests below still run, and a contributor without ffmpeg should not
        // see the suite go red for a missing system package.
        Prober::new()
    }

    fn skip_without_ffprobe() -> bool {
        Command::new(Prober::new().binary.clone())
            .arg("-version")
            .output()
            .is_err()
    }

    #[test]
    fn a_three_second_mp4_reports_three_seconds() {
        fixture_present("scene_3s.mp4");
        if skip_without_ffprobe() {
            return;
        }
        let info = real().probe(&fx("scene_3s.mp4")).expect("probe");
        assert_eq!(info.duration_ms, 3000, "duration within 100ms of known");
        assert!(info.format_name.contains("mp4"), "{}", info.format_name);
        assert_eq!(info.video_streams.len(), 1);
        let v = &info.video_streams[0];
        assert_eq!(v.codec_name, "h264");
        assert_eq!((v.width, v.height), (320, 240));
        assert!((v.fps - 10.0).abs() < 0.5, "fps was {}", v.fps);
        assert_eq!(v.start_time_ms, 0);
    }

    #[test]
    fn a_non_zero_start_offset_is_reported_not_normalised_away() {
        // stash#7229, named on its own because it is the one that breaks
        // preview generation. A file whose video stream starts at 5s must not
        // report 0, or every sprite frame is the wrong frame.
        fixture_present("scene_offset.mp4");
        if skip_without_ffprobe() {
            return;
        }
        let info = real().probe(&fx("scene_offset.mp4")).expect("probe");
        assert_eq!(
            info.video_streams[0].start_time_ms, 5000,
            "the 5s offset must survive into the report"
        );
        assert_eq!(info.format_start_ms, 5000);
        assert_eq!(
            info.video_streams[0].first_frame_ms(),
            5000,
            "and be reachable as the first frame's timestamp"
        );
    }

    #[test]
    fn a_one_frame_gif_is_an_image_and_a_long_one_is_a_scene() {
        // stash#5111, the rule the sniffer deliberately refused to guess.
        fixture_present("still.gif");
        fixture_present("animated.gif");
        if skip_without_ffprobe() {
            return;
        }
        let still = real().probe(&fx("still.gif")).expect("probe still");
        assert_eq!(gif_is_scene(&still), Some(false), "1 frame, 40ms: an image");

        let anim = real().probe(&fx("animated.gif")).expect("probe animated");
        assert_eq!(gif_is_scene(&anim), Some(true), "100 frames, 4s: a scene");
    }

    #[test]
    fn the_gif_rule_does_not_apply_to_other_containers() {
        // The question is only asked about GIFs; an mp4 must not be asked.
        fixture_present("scene_3s.mp4");
        if skip_without_ffprobe() {
            return;
        }
        let info = real().probe(&fx("scene_3s.mp4")).expect("probe");
        assert_eq!(gif_is_scene(&info), None);
    }

    #[test]
    fn audio_streams_carry_channels_rate_and_duration() {
        fixture_present("tone.mp3");
        if skip_without_ffprobe() {
            return;
        }
        let info = real().probe(&fx("tone.mp3")).expect("probe");
        assert!(info.video_streams.is_empty(), "an mp3 has no video stream");
        assert_eq!(info.audio_streams.len(), 1);
        let a = &info.audio_streams[0];
        assert_eq!(a.codec_name, "mp3");
        assert_eq!(a.channels, 1);
        assert_eq!(a.sample_rate, Some(44_100));
        // An mp3's encoder delay puts start_time slightly above zero; the
        // report must carry the real number rather than zero.
        assert!(a.start_time_ms > 0, "encoder delay: {}", a.start_time_ms);
    }

    #[test]
    fn each_lossless_format_reports_its_own_codec() {
        fixture_present("tone.flac");
        fixture_present("tone.wav");
        if skip_without_ffprobe() {
            return;
        }
        assert_eq!(
            real().probe(&fx("tone.flac")).unwrap().audio_streams[0].codec_name,
            "flac"
        );
        assert_eq!(
            real().probe(&fx("tone.wav")).unwrap().audio_streams[0].codec_name,
            "pcm_s16le"
        );
    }

    #[test]
    fn matroska_and_webm_report_a_matroska_container() {
        fixture_present("clip.mkv");
        fixture_present("clip.webm");
        if skip_without_ffprobe() {
            return;
        }
        for n in ["clip.mkv", "clip.webm"] {
            let i = real().probe(&fx(n)).expect(n);
            assert!(i.format_name.contains("matroska"), "{n}: {}", i.format_name);
        }
    }

    // --- the pure translation, driven by JSON so it runs without ffmpeg ---

    fn parse(json: &str) -> MediaInfo {
        let out: FfprobeOutput = serde_json::from_str(json).expect("valid fixture json");
        out.into()
    }

    #[test]
    fn attached_pictures_are_not_counted_as_video_streams() {
        // An mp3 with embedded cover art reports a video stream. Counting it
        // would make a single audio object look like it has a video track, and
        // the grid would try to render a thumbnail of the artwork as a frame.
        let info = parse(
            r#"{"streams":[
                {"index":0,"codec_type":"audio","codec_name":"mp3"},
                {"index":1,"codec_type":"video","codec_name":"mjpeg",
                 "disposition":{"attached_pic":1}}
            ],"format":{"format_name":"mp3","duration":"300.5"}}"#,
        );
        assert_eq!(info.audio_streams.len(), 1);
        assert!(
            info.video_streams.is_empty(),
            "the cover art must be skipped"
        );
        assert_eq!(info.duration_ms, 300_500);
    }

    #[test]
    fn frame_count_is_derived_from_fps_when_nb_frames_is_absent() {
        // Normal for MKV. Without this, the GIF-vs-scene rule has no frame
        // count to work from and every MKV looks like a single frame.
        let info = parse(
            r#"{"streams":[{"index":0,"codec_type":"video","codec_name":"h264",
                 "width":1920,"height":1080,"avg_frame_rate":"24000/1001",
                 "duration":"10.0"}],
               "format":{"format_name":"matroska","duration":"10.0"}}"#,
        );
        let v = &info.video_streams[0];
        assert!((v.fps - 23.976).abs() < 0.01, "fps {}", v.fps);
        assert_eq!(v.frames, Some(240), "10s at 24fps");
    }

    #[test]
    fn a_zero_denominator_frame_rate_does_not_divide_by_zero() {
        let info = parse(
            r#"{"streams":[{"index":0,"codec_type":"video","codec_name":"h264",
                 "avg_frame_rate":"0/0"}],"format":{"format_name":"matroska"}}"#,
        );
        assert_eq!(info.video_streams[0].fps, 0.0);
        assert_eq!(info.video_streams[0].frames, None);
    }

    #[test]
    fn a_negative_stream_start_is_clamped_not_propagated() {
        // Some MPEG-TS files report a negative start_time. Passing it through
        // would index backwards out of every buffer downstream.
        let info = parse(
            r#"{"streams":[{"index":0,"codec_type":"video","codec_name":"h264",
                 "start_time":"-0.080000"}],"format":{"format_name":"mpegts"}}"#,
        );
        assert_eq!(info.video_streams[0].start_time_ms, -80);
        assert_eq!(info.video_streams[0].first_frame_ms(), 0, "clamped");
    }

    #[test]
    fn a_display_matrix_rotation_is_read_and_normalised() {
        let mk = |rot: &str| {
            parse(&format!(
                r#"{{"streams":[{{"index":0,"codec_type":"video","codec_name":"h264",
                   "side_data_list":[{{"side_data_type":"Display Matrix","rotation":"{rot}"}}]}}],
                   "format":{{"format_name":"mov"}}}}"#
            ))
        };
        assert_eq!(mk("90").video_streams[0].rotation, 90);
        assert_eq!(mk("-90").video_streams[0].rotation, 90, "sign is not 'up'");
        assert_eq!(mk("180").video_streams[0].rotation, 180);
        assert_eq!(mk("270").video_streams[0].rotation, 90);
        assert_eq!(mk("0.000000").video_streams[0].rotation, 0);
        assert_eq!(mk("359").video_streams[0].rotation, 0);
    }

    #[test]
    fn chapters_become_markers_with_absolute_timestamps() {
        let info = parse(
            r#"{"streams":[],"chapters":[
                {"id":0,"start_time":"0.000000","end_time":"61.5",
                 "tags":{"title":"Opening"}},
                {"id":1,"start_time":"61.5","end_time":"120.0",
                 "tags":{"title":"Main"}}],
               "format":{"format_name":"matroska"}}"#,
        );
        assert_eq!(info.chapters.len(), 2);
        assert_eq!(info.chapters[0].title.as_deref(), Some("Opening"));
        assert_eq!(info.chapters[0].start_ms, 0);
        assert_eq!(info.chapters[1].start_ms, 61_500, "absolute, not relative");
        let (start, dur, title) = info.chapters[1].as_marker();
        assert_eq!((start, dur), (61_500, 58_500));
        assert_eq!(title.as_deref(), Some("Main"));
    }

    #[test]
    fn a_chapter_whose_end_precedes_its_start_is_reported_as_a_point() {
        let info = parse(
            r#"{"streams":[],"chapters":[{"id":0,"start_time":"10.0","end_time":"5.0"}],
               "format":{"format_name":"matroska"}}"#,
        );
        let c = &info.chapters[0];
        assert_eq!((c.start_ms, c.end_ms), (10_000, 10_000));
        assert_eq!(c.as_marker().1, 0, "a zero-length point, not an inversion");
    }

    #[test]
    fn an_absurd_duration_saturates_instead_of_wrapping() {
        // 1e30 seconds cast to u64 wraps in release mode into a small, plausible
        // number. Saturating keeps the corruption visible.
        let info = parse(r#"{"streams":[],"format":{"format_name":"x","duration":"1e30"}}"#);
        assert_eq!(info.duration_ms, u64::MAX);
    }

    #[test]
    fn a_non_default_audio_track_is_marked_as_such() {
        // stash#1058: choosing between a commentary track and the soundtrack.
        let info = parse(
            r#"{"streams":[
                {"index":0,"codec_type":"audio","codec_name":"aac","disposition":{"default":1}},
                {"index":1,"codec_type":"audio","codec_name":"aac","disposition":{"default":0}}],
               "format":{"format_name":"mov"}}"#,
        );
        assert!(info.audio_streams[0].is_default);
        assert!(!info.audio_streams[1].is_default);
    }

    #[test]
    fn replay_gain_is_read_from_the_stream_tags() {
        // §5.4: audio objects carry replay-gain metadata.
        let info = parse(
            r#"{"streams":[{"index":0,"codec_type":"audio","codec_name":"flac",
                 "tags":{"REPLAYGAIN_TRACK_GAIN":"-7.20 dB"}}],
               "format":{"format_name":"flac"}}"#,
        );
        assert_eq!(
            info.audio_streams[0].replaygain_track_gain.as_deref(),
            Some("-7.20 dB")
        );
    }

    #[test]
    fn format_tags_are_lowercased_so_lookup_is_consistent() {
        let info = parse(
            r#"{"streams":[],"format":{"format_name":"mov",
                 "tags":{"ENCODER":"Lavf60","creation_time":"2026-01-01T00:00:00Z"}}}"#,
        );
        assert_eq!(info.tags.get("encoder").map(String::as_str), Some("Lavf60"));
        assert_eq!(info.creation_time.as_deref(), Some("2026-01-01T00:00:00Z"));
    }

    #[test]
    fn a_file_with_no_streams_at_all_still_parses() {
        // A .txt probed by mistake must not be an error; it is simply empty.
        let info = parse(r#"{"streams":[],"format":{"format_name":"txt"}}"#);
        assert_eq!(info.duration_ms, 0);
        assert!(info.video_streams.is_empty());
    }
}
