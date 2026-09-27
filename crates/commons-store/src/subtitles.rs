//! Subtitle documents and their cues: the store layer.
//!
//! T-P6-002. Spec `docs/spec/t-p6-002-subtitles.md` §3. Migration
//! `0021_subtitles.sql`. The parsing lives in `commons-media::subtitles`; this
//! file owns validation and persistence and nothing else, so there is one place
//! where a rule lives.
//!
//! # The shape of the API, and why it is document-first
//!
//! A caller extracts a document, then `put_document` replaces the document and
//! its cues **in one transaction**. That is the whole API, and the shape is
//! deliberate:
//!
//! - **One call, one transaction.** A half-written document is a track list
//!   entry whose cues are missing, and a track list entry that does not work is
//!   worse than no entry. Re-extracting a file therefore cannot leave a stale
//!   cue behind, which a separate `put_cues` would.
//! - **Cues are replaced, not merged.** A file that went from 900 cues to 800
//!   must not keep 100 of the old ones. Upserting by `seq` would leave the tail
//!   forever, and the symptom is a caption appearing at a time it was removed
//!   from.
//! - **`sha256` is the idempotency key.** The caller passes the hash of the raw
//!   bytes; if the stored document for this object already has it, the write is
//!   a no-op. That is what makes a re-scan cheap, and it is checked here rather
//!   than trusted, because the UNIQUE index backs it up either way.

use std::fmt;

use commons_core::ts;
use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::db::{placeholder, placeholders, Store, StoreError};

/// Read a document out of a row of either engine's type.
///
/// A generic `fn doc_from_row<R: Row>` is the obvious thing to write and it does
/// not compile: `String: Decode<'_, R::Database>` is not implied by `R: Row`,
/// and no bound on `R` makes it so without naming the database. That is the same
/// wall sqlx puts up for `&mut PgConnection` vs `&mut SqliteConnection`, and the
/// crate's answer is the same both times: **name the concrete type and let a
/// macro carry the duplication**, so the SQL and the field list each appear once.
///
/// The alternative, `Any`, would unify the row types the same way. It is not
/// used anywhere in this crate and adding it for one module would be a second
/// way to talk to the database, which is how a crate ends up with two sets of
/// conventions and one set of bugs.
macro_rules! doc_from_row {
    ($r:expr, $row:ty) => {{
        let r: &$row = $r;
        Document::from(RowValues {
            id: r.get("id"),
            object_id: r.get("object_id"),
            origin: r.get("origin"),
            stream_index: r.get("stream_index"),
            path: r.get("path"),
            format: r.get("format"),
            language: r.get("language"),
            is_default: r.get::<i32, _>("is_default"),
            is_forced: r.get::<i32, _>("is_forced"),
            is_hearing_impaired: r.get::<i32, _>("is_hearing_impaired"),
            sha256: r.get("sha256"),
            byte_size: r.get("byte_size"),
            extracted_at: r.get("extracted_at"),
        })
    }};
}

/// Read a cue out of a row of either engine's type. Same reason as
/// [`doc_from_row`], and the same macro for the same reason.
macro_rules! cue_from_row {
    ($r:expr, $row:ty) => {{
        let r: &$row = $r;
        CueRow {
            seq: r.get::<i64, _>("seq") as u32,
            start_ms: r.get("start_ms"),
            end_ms: r.get("end_ms"),
            text: r.get("text"),
            style: r.get::<Option<String>, _>("style_json"),
        }
    }};
}

/// Where a subtitle document came from.
///
/// Not an `Option` and not a string. A sidecar and an embedded track have
/// different lifecycles — a sidecar can disappear from disk without the object
/// changing — and different ways of being re-read, so "unknown" is a third state
/// that no caller should have to handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// Muxed into the video file itself.
    Embedded,
    /// A separate file beside the video.
    Sidecar,
}

impl Origin {
    pub fn as_str(&self) -> &'static str {
        match self {
            Origin::Embedded => "embedded",
            Origin::Sidecar => "sidecar",
        }
    }
}

/// The subtitle formats this store accepts, as ffprobe and extensions report
/// them. Mirrors `commons_media::subtitles::Format`; kept as its own type so the
/// store does not depend on ffmpeg-facing code to name a format, and so a
/// spelling change is one edit in one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocFormat {
    Srt,
    Vtt,
    Ass,
    Ssa,
    MovText,
}

impl DocFormat {
    pub fn as_str(&self) -> &'static str {
        match self {
            DocFormat::Srt => "srt",
            DocFormat::Vtt => "vtt",
            DocFormat::Ass => "ass",
            DocFormat::Ssa => "ssa",
            DocFormat::MovText => "mov_text",
        }
    }

    /// The wire content type, for serving a sidecar to a client.
    pub fn content_type(&self) -> &'static str {
        match self {
            DocFormat::Srt => "application/x-subrip",
            DocFormat::Vtt => "text/vtt",
            DocFormat::Ass | DocFormat::Ssa => "text/x-ssa",
            DocFormat::MovText => "text/plain",
        }
    }
}

/// A subtitle document: one subtitle source for one object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    pub id: String,
    pub object_id: String,
    pub origin: Origin,
    /// ffprobe's stream index. `None` for a sidecar — inventing one would make a
    /// sidecar look like stream 0 of the video, which is a track that does not
    /// exist.
    pub stream_index: Option<i64>,
    /// The sidecar's path. `None` when embedded.
    pub path: Option<String>,
    pub format: DocFormat,
    /// BCP-47, normalised. `None` when the file declares none, which is
    /// different from the empty string, which means the file said it has none.
    pub language: Option<String>,
    pub is_default: bool,
    pub is_forced: bool,
    pub is_hearing_impaired: bool,
    /// Of the raw document. The idempotency key for a re-extract.
    pub sha256: String,
    pub byte_size: i64,
    pub extracted_at: String,
}

/// What identifies a subtitle track: WHERE it is, not what it contains.
///
/// The five columns of `subtitle_documents_by_track`, in one type, so the skip
/// in [`put_document`] and the unique index in the migration are visibly the
/// same tuple. It is a type rather than "pass the whole `Document`" so a caller
/// can ask about a track it has not extracted yet, which is the whole point of
/// asking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackKey<'a> {
    pub object_id: &'a str,
    pub origin: Origin,
    /// ffprobe's stream index for an embedded track. `None` for a sidecar,
    /// which the COALESCE maps to a key value of its own.
    pub stream_index: Option<i64>,
    /// BCP-47, or `None` when the file declares no language.
    pub language: Option<&'a str>,
    pub format: DocFormat,
}

impl Document {
    /// This document's track identity, for a [`stored_hash`] check.
    pub fn track_key(&self) -> TrackKey<'_> {
        TrackKey {
            object_id: &self.object_id,
            origin: self.origin,
            stream_index: self.stream_index,
            language: self.language.as_deref(),
            format: self.format,
        }
    }
}

impl Document {
    /// A new document, with the fields the caller cannot know left empty.
    pub fn new(
        id: impl Into<String>,
        object_id: impl Into<String>,
        origin: Origin,
        format: DocFormat,
        sha256: impl Into<String>,
    ) -> Self {
        Document {
            id: id.into(),
            object_id: object_id.into(),
            origin,
            stream_index: None,
            path: None,
            format,
            language: None,
            is_default: false,
            is_forced: false,
            is_hearing_impaired: false,
            sha256: sha256.into(),
            byte_size: 0,
            extracted_at: String::new(),
        }
    }

    /// The label a track list should show for this document.
    ///
    /// Derived rather than stored, and the derivation is the reason it lives
    /// here: the *language* comes from the container's tag when there is one and
    /// from the filename when there is not, and a track list that shows
    /// `None`, or an empty string, or a raw path is a track list nobody can pick
    /// from. The `(none)` case is a fact worth showing — "no language" is
    /// different from "the request failed" — so it is a label, not an absence.
    pub fn label(&self) -> String {
        match self.language.as_deref() {
            Some(l) if !l.is_empty() => l.to_string(),
            // No declared language: the file's own name is the best signal left,
            // and it is a real one — `episode.eng.srt` says English in its name.
            _ => self
                .path
                .as_deref()
                .and_then(file_stem)
                .unwrap_or_else(|| self.format.as_str().to_string()),
        }
    }

    /// Whether two documents are the same track as far as a viewer is concerned.
    ///
    /// Used to pick a default when nothing is flagged: the language matters, the
    /// sidecar path does not, and the SHA does not. Two sidecars with identical
    /// cues and different names are the same track.
    pub fn same_track_as(&self, other: &Document) -> bool {
        self.language == other.language
            && self.origin == other.origin
            && self.stream_index == other.stream_index
    }
}

/// The file stem of a path: the last component without its extension.
fn file_stem(path: &str) -> Option<String> {
    let name = path.rsplit(['/', '\\']).next()?;
    let stem = match name.rfind('.') {
        Some(i) if i > 0 => &name[..i],
        _ => name,
    };
    (!stem.is_empty()).then(|| stem.to_string())
}

/// Why a subtitle document or its cues are not storable as given.
///
/// `Debug` only, not `Clone` or `PartialEq`: the `Query` variant wraps
/// `StoreError`, which wraps `sqlx::Error`, and neither is `Clone` or `Eq`. So
/// tests here assert on the `Invalid` variant's message and on `matches!` for
/// the rest, rather than on the error as a value — which is the right thing to
/// do anyway, since two different SQL failures are not interchangeable in a
/// test even when they are both "a query failed".
#[derive(Debug)]
pub enum SubtitleStoreError {
    /// The document or a cue is not one the database will accept. The
    /// validation lives here rather than in the migration's CHECKs so the
    /// message can name what is wrong; the CHECKs are the second line.
    Invalid(String),
    Query(StoreError),
}

impl fmt::Display for SubtitleStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SubtitleStoreError::Invalid(why) => write!(f, "{why}"),
            SubtitleStoreError::Query(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for SubtitleStoreError {}

impl From<sqlx::Error> for SubtitleStoreError {
    fn from(e: sqlx::Error) -> Self {
        SubtitleStoreError::Query(StoreError::Query(e))
    }
}

impl From<StoreError> for SubtitleStoreError {
    fn from(e: StoreError) -> Self {
        SubtitleStoreError::Query(e)
    }
}

type Result<T> = std::result::Result<T, SubtitleStoreError>;

/// What a document's cues say about the document.
pub struct Cues {
    /// `(seq, start_ms, end_ms, text, style)`, in file order.
    ///
    /// A tuple rather than a struct because the caller has them as
    /// `commons_media::subtitles::Cue` values and a conversion here would be a
    /// second mapping of the same five fields in a second file.
    pub rows: Vec<(u32, i64, i64, String, Option<String>)>,
}

impl Cues {
    /// Build a cue set from rows in file order.
    ///
    /// The field is public, so this is not the only way in — it exists because
    /// `rows: vec![…]` at a call site has to spell the five-tuple out inline
    /// with a bare `None` for the style, and a bare `None` there means "no
    /// styling" to a reader who does not know the type. Naming the tuple once
    /// here is the difference between a call site that reads as intent and one
    /// that reads as a column list.
    pub fn new(rows: Vec<(u32, i64, i64, String, Option<String>)>) -> Self {
        Cues { rows }
    }
}

impl Document {
    /// Check the document against every rule the table enforces, and against the
    /// ones it cannot.
    fn validate(&self) -> Result<()> {
        if self.id.trim().is_empty() {
            return Err(SubtitleStoreError::Invalid("a document needs an id".into()));
        }
        if self.object_id.trim().is_empty() {
            return Err(SubtitleStoreError::Invalid(
                "a document needs an object_id: subtitles belong to something".into(),
            ));
        }
        if self.sha256.trim().is_empty() {
            // The hash is the idempotency key AND the uniqueness constraint's
            // subject. A document with no hash cannot be de-duplicated, so it
            // would appear twice in the track list after one re-scan.
            return Err(SubtitleStoreError::Invalid(
                "a document needs a sha256 of its raw bytes: without it a re-scan \
                 cannot tell an unchanged file from a rewritten one"
                    .into(),
            ));
        }
        // The origin/path pair. Redundant with the table's CHECK, and kept
        // because the CHECK's message is a constraint name and this one says
        // which half is wrong.
        match (self.origin, self.path.is_some()) {
            (Origin::Sidecar, false) => {
                return Err(SubtitleStoreError::Invalid(
                    "a sidecar document needs the path it was read from".into(),
                ))
            }
            (Origin::Embedded, true) => {
                return Err(SubtitleStoreError::Invalid(
                    "an embedded document has no path: its bytes are in the video file".into(),
                ))
            }
            (Origin::Embedded, false) => {
                if self.stream_index.is_none() {
                    return Err(SubtitleStoreError::Invalid(
                        "an embedded document needs its stream index, or there is no \
                         stream to read it from"
                            .into(),
                    ));
                }
            }
            (Origin::Sidecar, true) => {}
        }
        if self.byte_size < 0 {
            return Err(SubtitleStoreError::Invalid(
                "byte_size cannot be negative".into(),
            ));
        }
        Ok(())
    }
}

impl Cues {
    /// Check every cue, and check them as a *set* rather than one at a time.
    ///
    /// The set-level rules are the ones a per-cue check cannot express, and they
    /// are the ones that matter:
    ///
    /// - `seq` must be unique within the document, because it is the file's own
    ///   order and the UNIQUE index enforces it. A duplicate is a parser that
    ///   numbered two cues the same, and it would make `ORDER BY seq` ambiguous.
    /// - `end_ms > start_ms`, because a browser **discards** a WebVTT cue with
    ///   `end <= start` silently — and a silently discarded cue is
    ///   indistinguishable from having no subtitles. `to_webvtt` drops such a
    ///   cue for the same reason, so the two agree rather than one of them
    ///   quietly disagreeing with the other.
    /// - text must be non-empty, because an empty cue is a blank line in the
    ///   output rather than a caption, and a blank line in a WebVTT document
    ///   *separates cues* — so one empty cue swallows the next one's timing.
    fn validate(&self, document_id: &str) -> Result<()> {
        let mut seen: Vec<u32> = Vec::with_capacity(self.rows.len());
        for (i, (seq, start_ms, end_ms, text, _)) in self.rows.iter().enumerate() {
            if end_ms <= start_ms {
                return Err(SubtitleStoreError::Invalid(format!(
                    "cue {i} (seq {seq}) ends at {end_ms} ms, which is not after it starts at \
                     {start_ms} ms: a zero-length cue is dropped silently by every browser, and \
                     the viewer would see no subtitles at all"
                )));
            }
            if *start_ms < 0 {
                return Err(SubtitleStoreError::Invalid(format!(
                    "cue {i} (seq {seq}) starts at {start_ms} ms, before the file"
                )));
            }
            if text.trim().is_empty() {
                return Err(SubtitleStoreError::Invalid(format!(
                    "cue {i} (seq {seq}) has no text: an empty cue is a blank line in the output, \
                     and a blank line separates cues, so it swallows the next one's timing"
                )));
            }
            if seen.contains(seq) {
                return Err(SubtitleStoreError::Invalid(format!(
                    "cue seq {seq} appears more than once in document {document_id}: seq is the \
                     file's own order, and a duplicate makes it ambiguous"
                )));
            }
            seen.push(*seq);
        }
        Ok(())
    }
}

/// Every subtitle document for an object, in a stable order.
///
/// One query, ordered by language then format, so the client groups by language
/// without sorting. `language IS NULL` first on both engines, so a track with no
/// declared language leads the list: the tracks that know what they are come
/// before the one that does not, and a user picks a language far more often than
/// they pick "the file with no language".
pub async fn list_documents(store: &Store, object_id: &str) -> Result<Vec<Document>> {
    const SQL: &str = "SELECT id, object_id, origin, stream_index, path, format, language, \
         is_default, is_forced, is_hearing_impaired, sha256, byte_size, extracted_at \
         FROM subtitle_documents WHERE object_id = {p} \
         ORDER BY language IS NULL, language, format, id";
    macro_rules! go {
        ($p:expr, $numbered:literal, $row:ty) => {{
            let sql = SQL.replace("{p}", &placeholders(1, $numbered));
            sqlx::query(&sql)
                .bind(object_id)
                .fetch_all($p)
                .await
                .map_err(|e| SubtitleStoreError::Query(StoreError::Query(e)))
                .map(|rows| rows.iter().map(|r| doc_from_row!(r, $row)).collect())
        }};
    }
    match store {
        Store::Sqlite(p) => go!(p, false, sqlx::sqlite::SqliteRow),
        Store::Postgres(p) => go!(p, true, sqlx::postgres::PgRow),
    }
}

/// One document by id.
pub async fn get_document(store: &Store, id: &str) -> Result<Option<Document>> {
    const SQL: &str = "SELECT id, object_id, origin, stream_index, path, format, language, \
         is_default, is_forced, is_hearing_impaired, sha256, byte_size, extracted_at \
         FROM subtitle_documents WHERE id = {p}";
    macro_rules! go {
        ($p:expr, $numbered:literal, $row:ty) => {{
            let sql = SQL.replace("{p}", &placeholders(1, $numbered));
            sqlx::query(&sql)
                .bind(id)
                .fetch_optional($p)
                .await
                .map_err(|e| SubtitleStoreError::Query(StoreError::Query(e)))
                .map(|row| row.as_ref().map(|r| doc_from_row!(r, $row)))
        }};
    }
    match store {
        Store::Sqlite(p) => go!(p, false, sqlx::sqlite::SqliteRow),
        Store::Postgres(p) => go!(p, true, sqlx::postgres::PgRow),
    }
}

/// The hash already stored for **this track**, if any.
///
/// The cheap half of "has this already been extracted", and the reason a
/// re-scan does not re-decode every subtitle in the library. Returns the hash
/// rather than a bool so a caller that *does* want to force a re-extract can
/// compare and decide.
///
/// Scoped to the track, not to the object. Keyed on `object_id` this answers
/// "has ANY subtitle been extracted for this object", which is true from the
/// first track onwards -- so every track after the first is skipped as
/// "unchanged" and a multi-language file ends up with one language. That is not
/// hypothetical; the test `two_languages_of_one_object_are_both_kept` is the
/// bug, and it exists because this was written the obvious way first.
///
/// The predicate is the same tuple as `subtitle_documents_by_track`, so the
/// skip and the constraint cannot disagree about what a track is.
pub async fn stored_hash<'a>(store: &Store, track: &TrackKey<'a>) -> Result<Option<String>> {
    const SQL: &str = "SELECT sha256 FROM subtitle_documents WHERE object_id = {p1} AND origin = {p2} AND COALESCE(stream_index, -1) = {p3} AND COALESCE(language, '') = {p4} AND format = {p5} LIMIT 1";
    // The decode is inside each arm: the two row types are unrelated Rust
    // types, so one `row.map(|r| r.get(…))` after the match does not
    // typecheck. The macro keeps the SQL in one place; the arms differ only in
    // the placeholder dialect.
    //
    // `{p1}`..`{p5}`, built with `placeholder` (singular), not `placeholders`.
    //
    // `placeholders(n)` returns the LIST 1..=n, so a slot spelled `{p2}` wanted
    // "$1, $2" and the statement ended up with eleven placeholders for five
    // bound values. The database rejected it at a comma. The two names are one
    // character apart and mean opposite things, which is the whole hazard.
    //
    // The names are numbered and distinct for a second reason: "{p}" is a
    // PREFIX of "{p2}", so substituting the suffix-less slot last would rewrite
    // text the earlier passes had already produced.
    macro_rules! go {
        ($p:expr, $numbered:literal) => {{
            let sql = SQL
                .replace("{p1}", &placeholder(1, $numbered))
                .replace("{p2}", &placeholder(2, $numbered))
                .replace("{p3}", &placeholder(3, $numbered))
                .replace("{p4}", &placeholder(4, $numbered))
                .replace("{p5}", &placeholder(5, $numbered));
            sqlx::query(&sql)
                .bind(&track.object_id)
                .bind(track.origin.as_str())
                .bind(track.stream_index.unwrap_or(-1))
                .bind(track.language.as_deref().unwrap_or(""))
                .bind(track.format.as_str())
                .fetch_optional($p)
                .await
                .map_err(|e| SubtitleStoreError::Query(StoreError::Query(e)))
                .map(|row| row.map(|r| r.get::<String, _>("sha256")))
        }};
    }

    match store {
        Store::Sqlite(p) => go!(p, false),
        Store::Postgres(p) => go!(p, true),
    }
}

/// A stored cue: the read shape of a `subtitle_cues` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CueRow {
    /// The cue's position in the FILE's order, which is not its time order —
    /// ASS layers its dialogue, so cues overlap and sorting by time would
    /// renumber the file.
    pub seq: u32,
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
    pub style: Option<String>,
}

/// The cues of a document, in file order.
pub async fn list_cues(store: &Store, document_id: &str) -> Result<Vec<CueRow>> {
    const SQL: &str = "SELECT seq, start_ms, end_ms, text, style_json FROM subtitle_cues \
         WHERE document_id = {p} ORDER BY seq";
    macro_rules! go {
        ($p:expr, $numbered:literal, $row:ty) => {{
            let sql = SQL.replace("{p}", &placeholders(1, $numbered));
            sqlx::query(&sql)
                .bind(document_id)
                .fetch_all($p)
                .await
                .map_err(|e| SubtitleStoreError::Query(StoreError::Query(e)))
                .map(|rows| rows.iter().map(|r| cue_from_row!(r, $row)).collect())
        }};
    }
    match store {
        Store::Sqlite(p) => go!(p, false, sqlx::sqlite::SqliteRow),
        Store::Postgres(p) => go!(p, true, sqlx::postgres::PgRow),
    }
}

/// Store a document and its cues, replacing both, in one transaction.
///
/// One transaction, and the replacement is the point: a file that went from 900
/// cues to 800 must not keep 100 of the old ones, because the symptom is a
/// caption appearing at a time it was removed from. Cues are deleted and
/// re-inserted rather than upserted, so the tail cannot survive.
///
/// Idempotent on the SHA: if this object already has a document with these
/// bytes, nothing is written and `Ok(false)` is returned. That is the caller's
/// signal that a re-scan did no work, and it is the reason a library can be
/// re-scanned without re-decoding every subtitle in it.
///
/// ## Why this is a macro and not a loop over a boxed executor
///
/// `&mut PgConnection` and `&mut SqliteConnection` are unrelated Rust types,
/// and the transaction types they come from are too. sqlx's `Any` would unify
/// them, and it is not used anywhere in this crate: introducing a second way to
/// talk to the database for one module is how a crate ends up with two sets of
/// conventions and one set of bugs. The macro is the convention the rest of the
/// crate already follows, and the alternative is a `Box<dyn Executor>` that
/// cannot own a transaction anyway.
macro_rules! put {
    ($store:expr, $doc:expr, $cues:expr, $extracted_at:expr, $numbered:literal, $conn:expr) => {{
        let doc = $doc;
        let p = placeholders(1, $numbered);

        // This TRACK's previous row goes first, and its cues with it via
        // ON DELETE CASCADE.
        //
        // By **id**, not by `object_id`. Deleting by object was the bug the
        // `two_languages_of_one_object_are_both_kept` test exists for: writing
        // the second language deleted the first, so a multi-language file ended
        // up with exactly one language and no error. The delete has to be as
        // narrow as the unique index, and the index is on the track.
        //
        // The cascade matters here and is why the delete is not a separate
        // statement for the cues: a re-extract that dropped the old cues
        // silently would double every line in the player.
        // `p` is substituted by the `format!`, not by a later `.replace`: the
        // earlier version wrote `{p}` and then replaced it inside a `format!`
        // ARGUMENT, where `{{p}}` is an escaped brace -- so the statement that
        // reached the database had a literal `{` in it and failed with
        // "syntax error at or near '{'", which names the brace and not the
        // mistake.
        sqlx::query(&format!(
            "DELETE FROM subtitle_documents WHERE id = {p}"
        ))
        .bind(&doc.id)
        .execute($conn)
        .await?;

        sqlx::query(&format!(
            // One line, and that is not a style preference: a `\` at the end of
            // a Rust string line is a LINE CONTINUATION, so it does not reach the
            // database -- but a `\` inside a `format!` ARGUMENT does, and the
            // result is a statement with a stray backslash in it that fails with
            // "syntax error at or near ','" and nothing resembling the cause.
            "INSERT INTO subtitle_documents (id, object_id, origin, stream_index, path, format, language, is_default, is_forced, is_hearing_impaired, sha256, byte_size, extracted_at) VALUES ({})",
            placeholders(13, $numbered)
        ))
        .bind(&doc.id)
        .bind(&doc.object_id)
        .bind(doc.origin.as_str())
        .bind(doc.stream_index)
        .bind(doc.path.as_deref())
        .bind(doc.format.as_str())
        .bind(doc.language.as_deref())
        .bind(doc.is_default as i32)
        .bind(doc.is_forced as i32)
        .bind(doc.is_hearing_impaired as i32)
        .bind(&doc.sha256)
        .bind(doc.byte_size)
        .bind($extracted_at.as_str())
        .execute($conn)
        .await?;

        let cq = placeholders(7, $numbered);
        for (seq, start_ms, end_ms, text, style) in &$cues.rows {
            sqlx::query(&format!(
                "INSERT INTO subtitle_cues (id, document_id, seq, start_ms, end_ms, text, style_json) VALUES ({cq})"
            ))
            // A deterministic id, so re-inserting the same cue is the same row
            // rather than a second one. The UNIQUE index on (document_id, seq)
            // is what actually enforces that; this makes the row's identity
            // legible in a dump without a lookup.
            .bind(format!("{}:{seq}", doc.id))
            .bind(&doc.id)
            .bind(*seq as i64)
            .bind(*start_ms)
            .bind(*end_ms)
            .bind(text)
            .bind(style.as_deref())
            .execute($conn)
            .await?;
        }
    }};
}

/// See [`put_document`]. Validates, then writes, and is a no-op when the bytes
/// are already stored.
pub async fn put_document(store: &Store, doc: &Document, cues: &Cues) -> Result<bool> {
    doc.validate()?;
    cues.validate(&doc.id)?;

    // Keyed on the TRACK, so a second language is extracted rather than skipped
    // as "unchanged". See `stored_hash`.
    if stored_hash(store, &doc.track_key()).await? == Some(doc.sha256.clone()) {
        return Ok(false);
    }

    let extracted_at = if doc.extracted_at.is_empty() {
        ts::now()
    } else {
        doc.extracted_at.clone()
    };

    match store {
        Store::Sqlite(pool) => {
            let mut tx = pool.begin().await?;
            put!(store, doc, cues, extracted_at, false, &mut *tx);
            tx.commit().await?;
        }
        Store::Postgres(pool) => {
            let mut tx = pool.begin().await?;
            put!(store, doc, cues, extracted_at, true, &mut *tx);
            tx.commit().await?;
        }
    }
    Ok(true)
}

/// Remove every subtitle document for an object.
///
/// A re-scan that finds the sidecars gone calls this; a delete does not need to,
/// because the FK's `ON DELETE CASCADE` covers it. The count is returned
/// because a caller re-scanning wants to know whether anything was there — a
/// silent zero and a silent one are the same number, but "I deleted three
/// tracks" is a log line worth having.
pub async fn delete_documents(store: &Store, object_id: &str) -> Result<usize> {
    let sql_del = format!(
        "DELETE FROM subtitle_documents WHERE object_id = {}",
        placeholders(1, true)
    );
    let n = match store {
        Store::Sqlite(p) => {
            let q = sql_del.replace("$1", "?");
            sqlx::query(&q)
                .bind(object_id)
                .execute(p)
                .await
                .map_err(StoreError::Query)?
                .rows_affected()
        }
        Store::Postgres(p) => sqlx::query(&sql_del)
            .bind(object_id)
            .execute(p)
            .await
            .map_err(StoreError::Query)?
            .rows_affected(),
    };
    Ok(n as usize)
}

/// How many documents an object has, for a caller deciding whether to extract.
pub async fn count_documents(store: &Store, object_id: &str) -> Result<i64> {
    const SQL: &str = "SELECT count(*) AS n FROM subtitle_documents WHERE object_id = {p}";
    macro_rules! go {
        ($p:expr, $numbered:literal) => {{
            let sql = SQL.replace("{p}", &placeholders(1, $numbered));
            sqlx::query(&sql)
                .bind(object_id)
                .fetch_one($p)
                .await
                .map_err(|e| SubtitleStoreError::Query(StoreError::Query(e)))
                .map(|r| r.get::<i64, _>("n"))
        }};
    }
    match store {
        Store::Sqlite(p) => go!(p, false),
        Store::Postgres(p) => go!(p, true),
    }
}

/// A row's raw values, before the two enums are interpreted.
///
/// Thirteen positional arguments is the point at which a struct stops being
/// optional: the call site is a column list in one order and a parameter list in
/// another, and nothing checks they agree. Postgres rejects a mismatch loudly
/// and **SQLite does not** — a swapped pair of `TEXT` columns writes the wrong
/// value silently, on exactly one engine, which is the failure this crate's
/// two-engine discipline exists to prevent.
///
/// So the row is a struct with named fields, and the two enums (`Origin`,
/// `DocFormat`) are interpreted in one named place. What the database cannot
/// check is checked there, once, and the CHECKs in the migration are the second
/// line.
struct RowValues {
    id: String,
    object_id: String,
    origin: String,
    stream_index: Option<i64>,
    path: Option<String>,
    format: String,
    language: Option<String>,
    is_default: i32,
    is_forced: i32,
    is_hearing_impaired: i32,
    sha256: String,
    byte_size: i64,
    extracted_at: String,
}

impl From<RowValues> for Document {
    fn from(v: RowValues) -> Self {
        Document {
            id: v.id,
            object_id: v.object_id,
            origin: match v.origin.as_str() {
                "sidecar" => Origin::Sidecar,
                // The CHECK makes this unreachable, so the fallback is
                // `Embedded` rather than a panic: a row written by a future
                // migration with a third origin should not take the process down
                // on a read.
                _ => Origin::Embedded,
            },
            stream_index: v.stream_index,
            path: v.path,
            format: match v.format.as_str() {
                "srt" => DocFormat::Srt,
                "vtt" => DocFormat::Vtt,
                "ssa" => DocFormat::Ssa,
                "mov_text" => DocFormat::MovText,
                _ => DocFormat::Ass,
            },
            language: v.language,
            is_default: v.is_default != 0,
            is_forced: v.is_forced != 0,
            is_hearing_impaired: v.is_hearing_impaired != 0,
            sha256: v.sha256,
            byte_size: v.byte_size,
            extracted_at: v.extracted_at,
        }
    }
}
