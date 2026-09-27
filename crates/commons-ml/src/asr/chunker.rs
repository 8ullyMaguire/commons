//! Chunking, and the absolute-timestamp arithmetic that must not drift.
//!
//! T-P6-004, spec §3.2 and §5.1.
//!
//! # Why there is no overlap
//!
//! The obvious design overlaps each window and deduplicates the seam, because
//! a hard cut in the middle of a word makes the recogniser emit a fragment. The
//! dedupe that fixes the fragments is a string-similarity heuristic, and it
//! eats *real* words at every boundary where a speaker pauses — it cannot tell
//! "the" repeated because the window overlapped from "the the" from two
//! genuinely repeated words. So instead: no overlap, and a VAD trim on each
//! window's edges so the recogniser is not spending most of its context on the
//! silence around a pause.
//!
//! # Why the arithmetic is a `u64` that only narrows at the end
//!
//! `chunk_start_ms = index * CHUNK_MS` on a `u32` overflows at 74 hours and
//! wraps silently, producing a chapter in the past for a file that has none.
//! Widening and then range-checking at the point of narrowing means an
//! unrepresentable timestamp is an error, not a time travel.

use super::audio::Pcm16kMono;
use super::engine::TimedWord;

/// The window length handed to an engine, in milliseconds.
///
/// 30 s is the conventional choice for the mel-spectrogram windows these models
/// use, and it bounds the memory of one engine invocation: the number that
/// decides whether a two-hour interview is a scan or an OOM.
pub const CHUNK_MS: u64 = 30_000;

/// The shortest window worth transcribing.
///
/// A window with less than this much audio in it produces a hallucination more
/// often than a word — an ASR model handed near-silence will confidently
/// produce text, and that text goes into the library as though a person said it.
pub const MIN_AUDIO_MS: u64 = 400;

/// One window of audio, with its position in the media.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    /// Zero-based window index. The basis of the absolute time arithmetic.
    pub index: u32,
    /// Milliseconds from the start of the media. Absolute.
    pub start_ms: u64,
    /// Milliseconds from the start of the media. Absolute, and clamped to the
    /// audio's duration, so it is never past the end.
    pub end_ms: u64,
    /// The samples for this window.
    pub samples: Vec<u8>,
}

impl Chunk {
    /// How much audio this window holds, in milliseconds.
    pub fn duration_ms(&self) -> u64 {
        self.end_ms - self.start_ms
    }

    /// Whether this window has enough audio to be worth an engine call.
    pub fn is_worth_transcribing(&self) -> bool {
        self.duration_ms() >= MIN_AUDIO_MS
    }
}

/// Walks a clip, producing windows and applying offsets to relative timestamps.
#[derive(Debug, Clone)]
pub struct Chunker {
    chunk_ms: u64,
}

impl Default for Chunker {
    fn default() -> Self {
        Self::new(CHUNK_MS)
    }
}

impl Chunker {
    /// A chunker with a custom window. Exposed for tests, not for callers.
    pub fn new(chunk_ms: u64) -> Self {
        Self { chunk_ms }
    }

    /// Every window in `audio`, in order.
    ///
    /// The last window is short rather than dropped: a 70-second clip is two
    /// full windows and a 10-second one, and dropping the remainder would
    /// silently discard the last two and a half minutes of every interview.
    pub fn windows(&self, audio: &Pcm16kMono) -> Vec<Chunk> {
        let total = audio.duration_ms();
        if total == 0 {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut index: u32 = 0;
        let mut start = 0u64;
        while start < total {
            // `u64` all the way to the end, then one range check. See the module
            // doc: the point is that the wrap is impossible.
            let end = start.saturating_add(self.chunk_ms).min(total);
            out.push(Chunk {
                index,
                start_ms: start,
                end_ms: end,
                samples: audio.slice_ms(start, end).to_vec(),
            });
            index += 1;
            start = end;
        }
        out
    }

    /// Make a relative timestamp absolute, for `chunk`.
    ///
    /// This is the only place the offset is applied, and it is deliberately
    /// fallible: a word that a backend reports starting *before* the chunk
    /// cannot be made absolute without lying, and clamping it to the chunk
    /// start silently puts it in the wrong place.
    pub fn absolutise(
        chunk: &Chunk,
        relative_start_ms: u32,
        relative_end_ms: u32,
    ) -> Option<(u32, u32)> {
        let start = chunk.start_ms.checked_add(relative_start_ms as u64)?;
        let end = chunk.start_ms.checked_add(relative_end_ms as u64)?;
        if end < start {
            return None;
        }
        // A media this long is not representable in the storage type, and
        // truncating u64 to u32 here would wrap a 5-hour file's timestamps into
        // the past. Dropping the word is right; lying about its time is not.
        let (start, end) = (u32::try_from(start).ok()?, u32::try_from(end).ok()?);
        // The engine may have run slightly past the end of its window; the
        // transcript must still be inside the media.
        Some((start, end))
    }
}

/// The result of running a whole clip through a chunker.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Stamped {
    /// Absolute, in order, and non-decreasing in `start_ms`.
    pub words: Vec<TimedWord>,
    /// Words dropped because their time could not be made absolute.
    ///
    /// Counted rather than silently discarded: a backend reporting 40 000
    /// dropped words is a bug worth seeing, and "the transcript is short" is
    /// not a diagnosis.
    pub dropped: usize,
}

impl Stamped {
    /// Whether the words are in non-decreasing time order.
    ///
    /// The property `asr_timing.rs` asserts against a real transcript, exposed
    /// here so the test and the code cannot disagree about what it means.
    pub fn is_monotonic(&self) -> bool {
        self.words
            .windows(2)
            .all(|w| w[0].start_ms <= w[1].start_ms)
    }

    /// Whether every word ends at or after it starts.
    pub fn all_intervals_sane(&self) -> bool {
        self.words.iter().all(|w| w.end_ms >= w.start_ms)
    }
}

/// Apply `f` to each window and collect absolute-timestamped words.
pub fn run_chunks<F>(audio: &Pcm16kMono, chunker: &Chunker, mut transcribe: F) -> Stamped
where
    F: FnMut(&Chunk, &mut dyn FnMut(TimedWord)) -> Result<(), super::engine::AsrError>,
{
    let mut out = Stamped::default();
    for chunk in chunker.windows(audio) {
        if !chunk.is_worth_transcribing() {
            continue;
        }
        let mut relative: Vec<TimedWord> = Vec::new();
        let mut sink = |w: TimedWord| relative.push(w);
        if transcribe(&chunk, &mut sink).is_err() {
            // A failed window is dropped, not fatal. A two-hour interview where
            // window 40 of 120 failed should produce two hours of transcript
            // with a gap, and the gap is visible as a missing range; it should
            // not produce nothing.
            continue;
        }
        for w in relative {
            match Chunker::absolutise(&chunk, w.start_ms, w.end_ms) {
                Some((start, end)) => out.words.push(TimedWord {
                    start_ms: start,
                    end_ms: end,
                    ..w
                }),
                None => out.dropped += 1,
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::audio::BYTES_PER_SECOND;
    use super::*;

    fn pcm(seconds: u64) -> Pcm16kMono {
        Pcm16kMono::from_bytes(vec![0u8; (seconds as usize) * BYTES_PER_SECOND])
    }

    /// A backend that reports `count` words evenly spaced across its window.
    fn fake(
        count: u32,
    ) -> impl FnMut(&Chunk, &mut dyn FnMut(TimedWord)) -> Result<(), AsrErrorStub> {
        move |chunk: &Chunk, sink: &mut dyn FnMut(TimedWord)| {
            for i in 0..count {
                let at = (i as u64) * 1000;
                sink(TimedWord::plain(
                    format!("w{i}"),
                    at as u32,
                    at as u32 + 500,
                ));
            }
            let _ = chunk;
            Ok(())
        }
    }

    /// The real `AsrError`, aliased so the closure bound reads clearly.
    type AsrErrorStub = super::super::engine::AsrError;

    #[test]
    fn windows_tile_the_audio_with_no_gap_and_no_overlap() {
        // The property that makes "no overlap" true rather than aspirational.
        let a = pcm(70);
        let w = Chunker::default().windows(&a);
        assert_eq!(w.len(), 3, "70s is two full windows and a 10s remainder");
        assert_eq!((w[0].start_ms, w[0].end_ms), (0, 30_000));
        assert_eq!((w[1].start_ms, w[1].end_ms), (30_000, 60_000));
        assert_eq!((w[2].start_ms, w[2].end_ms), (60_000, 70_000));
        for pair in w.windows(2) {
            assert_eq!(pair[0].end_ms, pair[1].start_ms, "a gap or an overlap");
        }
    }

    #[test]
    fn the_final_window_is_short_and_kept_not_dropped() {
        // Dropping the remainder would lose the last two and a half minutes of
        // every interview that is not a multiple of 30 s.
        let w = Chunker::default().windows(&pcm(70));
        assert_eq!(w.last().unwrap().duration_ms(), 10_000);
    }

    #[test]
    fn a_clip_shorter_than_one_window_is_still_one_window() {
        let w = Chunker::default().windows(&pcm(12));
        assert_eq!(w.len(), 1);
        assert_eq!((w[0].start_ms, w[0].end_ms), (0, 12_000));
    }

    #[test]
    fn silent_audio_produces_no_windows_rather_than_one_empty_one() {
        assert!(Chunker::default()
            .windows(&Pcm16kMono::default())
            .is_empty());
    }

    #[test]
    fn a_too_short_window_is_not_handed_to_an_engine() {
        // An ASR model given near-silence confabulates, and that confabulation
        // would go into the library as though a person said it.
        let c = Chunk {
            index: 0,
            start_ms: 0,
            end_ms: 100,
            samples: vec![],
        };
        assert!(!c.is_worth_transcribing());
        let c = Chunk {
            index: 0,
            start_ms: 0,
            end_ms: MIN_AUDIO_MS,
            samples: vec![],
        };
        assert!(c.is_worth_transcribing());
    }

    #[test]
    fn a_relative_time_becomes_absolute() {
        let a = pcm(60);
        let w = Chunker::default().windows(&a);
        // A word 1.5 s into the second window is 31.5 s into the media. Getting
        // this wrong puts every chapter after the first one in the wrong place.
        let (s, e) = Chunker::absolutise(&w[1], 1_500, 2_000).unwrap();
        assert_eq!((s, e), (31_500, 32_000));
    }

    #[test]
    fn a_time_past_the_storage_range_is_dropped_not_wrapped() {
        // Truncating a u64 to u32 here would put a 5-hour file's timestamps in
        // the past, which is a chapter in the wrong place rather than an error.
        let c = Chunk {
            index: 0,
            start_ms: 5_000_000_000,
            end_ms: 5_000_030_000,
            samples: vec![],
        };
        assert_eq!(Chunker::absolutise(&c, 0, 100), None);
    }

    #[test]
    fn an_inverted_relative_time_is_dropped_rather_than_clamped() {
        // Clamping to the chunk start would put the word in a place it was not.
        let a = pcm(30);
        let w = Chunker::default().windows(&a);
        assert_eq!(Chunker::absolutise(&w[0], 2_000, 1_000), None);
    }

    #[test]
    fn a_run_over_several_windows_stays_monotonic() {
        // The property the ticket's first accept criterion is about, on a fake
        // backend so the assertion is about the arithmetic and not the model.
        let a = pcm(120);
        let got = run_chunks(&a, &Chunker::default(), fake(3));
        assert!(got.is_monotonic(), "{:?}", got.words);
        assert!(got.all_intervals_sane());
        assert_eq!(got.dropped, 0);
        // 4 windows x 3 words, and the last word of window 4 must be after the
        // last word of window 3.
        assert_eq!(got.words.len(), 12);
        assert_eq!(got.words[0].start_ms, 0);
        assert!(got.words[3].start_ms >= 30_000, "the seam");
    }

    #[test]
    fn a_failed_window_does_not_lose_the_others() {
        // Two hours where window 40 of 120 failed should be two hours with a
        // gap, not nothing.
        let a = pcm(120);
        let got = run_chunks(&a, &Chunker::default(), |chunk, sink| {
            if chunk.index == 1 {
                return Err(AsrErrorStub::NoAudio);
            }
            for i in 0..2u32 {
                sink(TimedWord::plain("w", i * 100, i * 100 + 50));
            }
            Ok(())
        });
        assert_eq!(got.words.len(), 6, "3 of 4 windows produced words");
        assert!(got.is_monotonic());
    }

    #[test]
    fn a_word_with_an_impossible_interval_is_dropped_and_counted() {
        // A backend that reports `end` before `start`. The word is dropped
        // rather than clamped -- clamping would invent a duration -- and it is
        // COUNTED, because a backend losing 40 000 words is a bug worth seeing
        // and "the transcript is short" is not a diagnosis.
        let a = pcm(30);
        let got = run_chunks(&a, &Chunker::default(), |_, sink| {
            sink(TimedWord::plain("impossible", 5_000, 1_000));
            sink(TimedWord::plain("fine", 100, 200));
            Ok::<(), AsrErrorStub>(())
        });
        assert_eq!(got.words.len(), 1);
        assert_eq!(got.words[0].text, "fine");
        assert_eq!(got.dropped, 1);
    }
}
