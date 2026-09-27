//! The on-demand proxy: cache key, ffmpeg arguments, and the encode itself.
//!
//! T-P6-001, §11.5. Spec §4 and §6. Companion to `playback.rs`'s
//! [`Rung`](commons_store::playback::Rung), which decides *which* rung; this
//! decides *how* to produce it and *where* the result lives.
//!
//! # Three things this has to get right, in order
//!
//! 1. **The cache key is the CONTENT, not the request.** §11.5 and §12.1.1 both
//!    make bandwidth the real constraint, and a proxy keyed by request re-transcodes
//!    on every seek. The key here is `(content hash, rung)`, so a second viewer of
//!    the same file at the same quality gets the bytes off disk.
//! 2. **A partial file is never served.** ffmpeg is killed, the disk fills, or the
//!    process dies mid-encode, and the output path exists holding half a video.
//!    A `<video>` fed a truncated MP4 shows a green bar and plays nothing. Every
//!    write therefore goes to a temporary name and is renamed into place only on
//!    success — the rename is the atomicity, and it is the same reason a crash
//!    must not leave a `.tmp` that a later request mistakes for a finished file.
//! 3. **A failure names the reason.** "proxy failed" is undiagnosable from a
//!    browser. The caller gets the rung, the ffmpeg exit status, and the last
//!    meaningful line of stderr, which is the difference between "no h264 encoder
//!    in this ffmpeg build" and "the source has no video stream".

use std::path::{Path, PathBuf};

use commons_store::playback::Rung;

/// Where one rung's encode of one file lives.
///
/// Split out from [`Transcoder`] so the key can be tested without a transcode,
/// and so a cache is a value somebody can reason about rather than a string
/// concatenation buried in a request handler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheKey {
    /// The source file's content hash, as `commons_core::hashing` computes it.
    pub content_hash: String,
    pub rung: Rung,
}

/// A filename-safe digest of a source file.
///
/// Deliberately NOT `ContentHash`'s own `Display`, which is
/// `blake3:<hex>xxh128:<hex>`. The colons are legal in a filename but are a
/// nuisance in a shell, an ffmpeg argument and a URL, and the whole point of
/// this string is that it appears in all three. So the two hex halves are
/// concatenated with a `-`, which is unique enough (the lengths are fixed) and
/// legal everywhere the name is.
///
/// The BLAKE3 half alone would be sufficient for a cache key, and using both
/// halves means a key cannot collide by accident between a file whose blake3
/// was truncated and another's that was not.
pub fn cache_digest(h: &commons_core::ContentHash) -> String {
    format!("{}-{}", h.blake3_hex(), h.xxh128_hex())
}

impl CacheKey {
    pub fn new(content_hash: impl Into<String>, rung: Rung) -> Self {
        CacheKey {
            content_hash: content_hash.into(),
            rung,
        }
    }

    /// The file name this key maps to, inside the cache directory.
    ///
    /// Both halves of the name are load-bearing and neither is decoration:
    ///
    /// * the **hash** means a replaced file is a different cache entry, so an
    ///   edit in place cannot serve yesterday's video;
    /// * the **rung** means a 480p proxy and a 1080p proxy of the same file
    ///   coexist, and asking for the low rung never returns the high one.
    ///
    /// The extension follows the rung's container, so a browser handed this path
    /// has a filename it can infer a MIME type from without a second request.
    pub fn file_name(&self) -> String {
        format!(
            "{}-{}.{}",
            self.content_hash,
            self.rung.height(),
            self.rung.container()
        )
    }

    /// The temporary name an encode writes to before it is finished.
    ///
    /// Distinct from the final name so a concurrent reader cannot open a
    /// half-written file, and suffixed rather than prefixed so a cleanup that
    /// globs `*.partial` cannot mistake a real entry for debris.
    ///
    /// **The container extension is kept, and the `.partial` goes BEFORE it.**
    /// This is not cosmetic. ffmpeg infers its output muxer from the output
    /// file's extension, so `foo.mp4.partial` makes it fail with `Error
    /// opening output files: Invalid argument` — the encode never starts, and
    /// the message points at the output file rather than at the extension.
    /// `foo.partial.mp4` is simultaneously a distinguishable name and a file
    /// ffmpeg can write, which is why the order is this way and not the other.
    pub fn partial_file_name(&self) -> String {
        let name = self.file_name();
        // Strip the one extension we just appended, put `.partial` in front of
        // it. `rsplit_once` rather than `strip_suffix` so a change to
        // `file_name` cannot silently make this a no-op returning the original.
        match name.rsplit_once('.') {
            Some((stem, ext)) => format!("{stem}.partial.{ext}"),
            None => format!("{name}.partial"),
        }
    }

    pub fn final_path(&self, cache_dir: &Path) -> PathBuf {
        cache_dir.join(self.file_name())
    }

    pub fn partial_path(&self, cache_dir: &Path) -> PathBuf {
        cache_dir.join(self.partial_file_name())
    }
}

/// Why a proxy encode did not produce a file.
#[derive(Debug)]
pub enum TranscodeError {
    /// ffmpeg is not installed or not on `PATH`.
    FfmpegMissing,
    /// ffmpeg ran and failed. `status` is its exit code; `detail` is the last
    /// meaningful stderr line, which is almost always the actual reason.
    FfmpegFailed { status: Option<i32>, detail: String },
    /// The source path does not exist.
    SourceMissing(PathBuf),
    /// The cache directory could not be created.
    NoCacheDir(PathBuf),
    /// The encode succeeded but the file is not there, or is empty.
    ///
    /// Worth its own variant because it means ffmpeg exited 0 and lied — which
    /// happens when the arguments select no output stream — and a caller that
    /// trusts the exit code would serve a 404 on the next request instead of
    /// reporting the real problem.
    NothingProduced(PathBuf),
}

impl std::fmt::Display for TranscodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TranscodeError::FfmpegMissing => {
                write!(f, "ffmpeg is not installed or not on PATH")
            }
            TranscodeError::FfmpegFailed { status, detail } => match status {
                Some(c) => write!(f, "ffmpeg exited {c}: {detail}"),
                None => write!(f, "ffmpeg was killed by a signal: {detail}"),
            },
            TranscodeError::SourceMissing(p) => {
                write!(f, "source file is missing: {}", p.display())
            }
            TranscodeError::NoCacheDir(p) => {
                write!(f, "could not create the cache directory {}", p.display())
            }
            TranscodeError::NothingProduced(p) => write!(
                f,
                "ffmpeg reported success but wrote no usable file at {}",
                p.display()
            ),
        }
    }
}

impl std::error::Error for TranscodeError {}

/// Produces one rung of one file, on demand.
#[derive(Debug, Clone)]
pub struct Transcoder {
    ffmpeg: PathBuf,
    cache_dir: PathBuf,
    cores: usize,
}

impl Transcoder {
    pub fn new(cache_dir: impl Into<PathBuf>) -> Self {
        Transcoder {
            ffmpeg: PathBuf::from("ffmpeg"),
            cache_dir: cache_dir.into(),
            cores: 1,
        }
    }

    pub fn with_binary(mut self, binary: impl Into<PathBuf>) -> Self {
        self.ffmpeg = binary.into();
        self
    }

    pub fn with_cores(mut self, cores: usize) -> Self {
        self.cores = cores.max(1);
        self
    }

    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    /// Where `key`'s finished encode lives.
    pub fn path_for(&self, key: &CacheKey) -> PathBuf {
        key.final_path(&self.cache_dir)
    }

    /// Is this rung already on disk?
    ///
    /// Checks **size**, not just existence. A zero-byte file is what a killed
    /// encode leaves behind, and treating it as a cache hit serves an empty
    /// response with a 200 — the client waits for bytes that are not coming.
    pub fn is_cached(&self, key: &CacheKey) -> bool {
        std::fs::metadata(self.path_for(key))
            .map(|m| m.is_file() && m.len() > 0)
            .unwrap_or(false)
    }

    /// The ffmpeg arguments for one rung, in order.
    ///
    /// Written out rather than composed from [`Rung`]'s accessors so a test can
    /// assert the whole vector, and so a change to the ladder shows up here as a
    /// failing test rather than as a different file on disk.
    ///
    /// The parts that are not obvious, and each is there for a reason:
    ///
    /// * `-movflags +faststart` — without it the `moov` atom lands at the END
    ///   of an MP4, and a browser must download the whole file before it can
    ///   play anything. A proxied video that appears to hang for 40 seconds and
    ///   then plays is this flag's absence.
    /// * `-vf scale=...:force_original_aspect_ratio=decrease` — never
    ///   `scale=width:height` alone, which stretches a 4:3 source into 16:9
    ///   black bars baked into the pixels. A proxy that distorts is worse than no
    ///   proxy, because the user cannot tell the proxy did it.
    /// * `-map 0:v:0 -map 0:a:0?` — the first video stream and the first audio
    ///   stream *if there is one*. The `?` is load-bearing: a file with no audio
    ///   makes plain `-map 0:a:0` a hard error, so a silent video would fail to
    ///   proxy at all.
    /// * `-sn` — no subtitles. T-P6-002 owns them, and muxing them here would
    ///   put a feature in this ticket that the next one cannot change.
    pub fn args_for(&self, source: &Path, out: &Path, rung: Rung) -> Vec<String> {
        // `{ih}` is an ffmpeg expression variable (input height), NOT a Rust
        // format placeholder -- writing it inside a `format!` template is a
        // compile error, and escaping it as `{{ih}}` would emit a LITERAL "{ih}"
        // into the filter graph, which ffmpeg rejects at run time. So the rung's
        // height is interpolated and the variable is concatenated alongside it.
        // The rung is a HEIGHT, so the HEIGHT is what gets bounded. The
        // earlier `scale='min(H,ih)':-2` bounded the WIDTH instead, which turned
        // a 160x120 fixture into 120x90 and a 1080p rung into a portrait-shaped
        // video nobody asked for.
        //
        // `force_divisible_by=2` is load-bearing and the least obvious of the
        // three. `-2` makes the derived dimension even, but the aspect-ratio fit
        // rounds *after* it, so a 16:9 source at 480 comes out 853x480 and
        // libx264 refuses with "width not divisible by 2 (853x480)" -- an error
        // that names the encoder rather than the filter, and that appears only
        // for some aspect ratios.
        let scale = format!(
            "scale=-2:'min({h},ih)':force_original_aspect_ratio=decrease:force_divisible_by=2",
            h = rung.height()
        );
        let mut v = vec![
            "-y".to_string(),
            "-i".to_string(),
            source.display().to_string(),
            "-map".to_string(),
            "0:v:0".to_string(),
            "-map".to_string(),
            "0:a:0?".to_string(),
            "-sn".to_string(),
            "-vf".to_string(),
            scale,
            "-c:v".to_string(),
            rung.video_codec().to_string(),
            // yuv420p is what makes the result playable in a browser at all: an
            // h264 encode in yuv444p or yuv422p plays nothing in Chrome or
            // Firefox while looking perfectly valid to ffprobe.
            "-pix_fmt".to_string(),
            "yuv420p".to_string(),
            // CRF 23 is x264's default and the point where the encode is not
            // visibly worse than the source. Height is the quality knob here,
            // not a bitrate, because a fixed bitrate re-encodes a dark scene and
            // a bright one to the same size and one of them is wrong.
            "-crf".to_string(),
            "23".to_string(),
            "-c:a".to_string(),
            rung.audio_codec().to_string(),
            "-b:a".to_string(),
            "128k".to_string(),
            "-movflags".to_string(),
            "+faststart".to_string(),
        ];
        if self.cores > 1 {
            v.push("-threads".to_string());
            v.push(self.cores.to_string());
        }
        v.push(out.display().to_string());
        v
    }

    /// Produce `key`'s rung for `source`, reusing an existing encode if there
    /// is one.
    ///
    /// Returns the path to a file that is **complete**. The contract is
    /// everything: on `Ok`, the file at that path is a finished video, whatever
    /// happened to the previous attempt.
    pub fn transcode(
        &self,
        source: &Path,
        content_hash: &str,
        rung: Rung,
    ) -> Result<PathBuf, TranscodeError> {
        let key = CacheKey::new(content_hash, rung);
        let final_path = self.path_for(&key);

        if self.is_cached(&key) {
            return Ok(final_path);
        }
        if !source.exists() {
            return Err(TranscodeError::SourceMissing(source.to_path_buf()));
        }
        std::fs::create_dir_all(&self.cache_dir)
            .map_err(|_| TranscodeError::NoCacheDir(self.cache_dir.clone()))?;

        // Write beside the target, finish under the target's name. The rename is
        // the only thing that publishes, so a reader never sees a partial file.
        let partial = key.partial_path(&self.cache_dir);
        // A leftover partial from a killed run would make ffmpeg refuse to
        // overwrite it with `-n`, and would make `is_cached` lie later.
        let _ = std::fs::remove_file(&partial);

        let args = self.args_for(source, &partial, rung);
        let output = std::process::Command::new(&self.ffmpeg)
            .args(&args)
            .output()
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => TranscodeError::FfmpegMissing,
                _ => TranscodeError::FfmpegFailed {
                    status: None,
                    detail: e.to_string(),
                },
            })?;

        if !output.status.success() {
            // Never leave the partial behind: it is not a cache entry, and a
            // later request would try to reuse it.
            let _ = std::fs::remove_file(&partial);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let detail = stderr
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty() && !l.starts_with("ffmpeg version"))
                .unwrap_or("no output")
                .trim()
                .to_string();
            return Err(TranscodeError::FfmpegFailed {
                status: output.status.code(),
                detail,
            });
        }

        // ffmpeg exited 0 and there is still no file: the arguments selected no
        // output stream. Trusting the exit code here would turn a real error into
        // a 404 on the next request.
        let produced = std::fs::metadata(&partial)
            .map(|m| m.is_file() && m.len() > 0)
            .unwrap_or(false);
        if !produced {
            let _ = std::fs::remove_file(&partial);
            return Err(TranscodeError::NothingProduced(partial));
        }

        std::fs::rename(&partial, &final_path)
            .map_err(|_| TranscodeError::NothingProduced(final_path.clone()))?;
        Ok(final_path)
    }
}
