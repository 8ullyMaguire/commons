//! Local speech recognition. T-P6-004, spec §3.1–3.2.
//!
//! # Why an engine trait and not an engine
//!
//! Two engines are wanted — whisper.cpp for accuracy-per-watt, an ONNX model
//! for in-process speed — and they disagree about nearly everything: units,
//! whether they score words, whether they diarise, whether they can be killed.
//! Picking one in the type system would put that disagreement in every caller.
//! The trait keeps the disagreements in the adapters and leaves the callers
//! with `TimedWord`.
//!
//! # What this module is allowed to assume about time
//!
//! Every `TimedWord` carries times **absolute from the start of the media**,
//! never relative to the chunk that produced it. This is the single most
//! important property in the module, because a relative timestamp that a caller
//! corrects with an offset table is an offset table that will be wrong
//! somewhere, and the failure is invisible: the transcript looks fine and every
//! chapter is in the wrong place. [`chunker`] exists to make the absolute times
//! impossible to get wrong, and `asr_timing.rs` exists to prove it.

pub mod audio;
pub mod chunker;
pub mod engine;

pub use audio::{AudioError, Pcm16kMono, SAMPLE_RATE};
pub use chunker::{Chunk, Chunker, CHUNK_MS};
pub use engine::{AsrEngine, AsrError, SpeakerId, TimedWord, Transcript};
