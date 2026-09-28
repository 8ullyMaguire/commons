//! Interview transcript storage. Migration `0023_interviews.sql`.
//!
//! Spec §5.8, in `docs/spec/t-p6-004-interviews.md`.
//!
//! # Why this module has no policy in it
//!
//! The same split as `share.rs`: a store layer that decides things is a store
//! layer whose decisions cannot be tested without a database. Every function
//! here moves a row or counts one.
//!
//! # The one decision in this module: a correction is a PROPOSAL
//!
//! [`propose_correction`] is the only way to change what a transcript says, and
//! it writes a `FieldProposal` — it does not touch `interview_word`. There is
//! deliberately no `update_word`.
//!
//! The reason is §8.1, and it is worth stating in full because the alternative
//! is a two-line function that looks obviously fine: once the model's output is
//! overwritten, nobody can tell a misheard word from a typo, and a user who
//! corrects the same word a second time has nothing to compare against. The
//! evidence is the feature.
//!
//! # The two-engine rules, restated because they bite on every query here
//!
//! * Placeholders are `?` on SQLite and `$1, $2` on Postgres. Every query is
//!   written once as a `const SQL` with `{p}` markers and expanded through
//!   [`placeholders`].
//! * `word_count`, `duration_ms` are `BIGINT`, so `i64`. An `INTEGER` column
//!   read as `i64` is a *read-time* type error — invisible to the parity test,
//!   which compares column names.
//! * `confidence` is `DOUBLE PRECISION`, so `f64`. Declared `REAL` it is FLOAT4
//!   on Postgres and FLOAT8 on SQLite: decodes on one, refused by the other.
//! * `start_ms`/`end_ms` are `INTEGER` (INT4/INT8), so **`i32`**. Decoding them
//!   as `i64` passes on SQLite forever and fails on Postgres at runtime.

use chrono::{DateTime, Utc};
use sqlx::Row;

use crate::db::{placeholder, placeholders, Store, StoreError};

/// The stored spelling of a timestamp. Plan §0.4: ISO-8601 UTC, millis, `Z`.
fn to_ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn from_ts(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// One row of `interview_transcript`.
///
/// A mirror of the columns rather than a conversion into a domain type,
/// because the domain type for a transcript does not exist yet and inventing one
/// here would put the shape in the wrong crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptRow {
    pub id: String,
    pub object_id: String,
    pub engine: String,
    pub model_id: String,
    pub model_sha256: String,
    pub audio_command: String,
    pub sample_rate: i32,
    pub language: Option<String>,
    pub word_count: i64,
    pub duration_ms: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl TranscriptRow {
    /// A row with the provenance a caller must supply, and a placeholder
    /// object. Nothing else has a sensible default, which is the point: a
    /// transcript without a model digest is not reproducible, so the type will
    /// not let one be built without thinking about it.
    pub fn new(
        id: &str,
        object_id: &str,
        engine: &str,
        model_id: &str,
        model_sha256: &str,
        audio_command: &str,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: id.to_string(),
            object_id: object_id.to_string(),
            engine: engine.to_string(),
            model_id: model_id.to_string(),
            model_sha256: model_sha256.to_string(),
            audio_command: audio_command.to_string(),
            sample_rate: 16_000,
            language: None,
            word_count: 0,
            duration_ms: 0,
            created_at: now,
            updated_at: now,
        }
    }
}

/// One row of `interview_word`.
///
/// `confidence` and `speaker` are `Option` and the nullability is load-bearing:
/// "this engine does not score words" and "this engine scored every word 0.5"
/// are different facts, and code that averages over both is making a claim
/// about the audio that it cannot support.
#[derive(Debug, Clone, PartialEq)]
pub struct WordRow {
    pub ordinal: i32,
    pub text: String,
    pub start_ms: i32,
    pub end_ms: i32,
    pub confidence: Option<f64>,
    pub speaker: Option<String>,
}

impl WordRow {
    /// A word with no score and no speaker, for a non-scoring engine.
    pub fn new(ordinal: i32, text: &str, start_ms: i32, end_ms: i32) -> Self {
        Self {
            ordinal,
            text: text.to_string(),
            start_ms,
            end_ms,
            confidence: None,
            speaker: None,
        }
    }

    /// The duration of this word, in milliseconds.
    pub fn duration_ms(&self) -> i32 {
        self.end_ms - self.start_ms
    }
}

/// One window the engine was given, whether or not it worked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowRow {
    pub window_index: i32,
    pub start_ms: i32,
    pub end_ms: i32,
    pub ok: bool,
    pub failure: Option<String>,
}

/// The 12 binds of `SQL_INSERT_TRANSCRIPT`, as one macro body.
///
/// A macro and not a loop over a boxed executor: `&mut PgConnection` and
/// `&mut SqliteConnection` are unrelated types, and so are the transaction
/// types they come from. sqlx's `Any` would unify them and is not used anywhere
/// in this crate — a second way to talk to the database for one module is how a
/// crate ends up with two sets of conventions and one set of bugs.
macro_rules! write_all {
    ($conn:expr, $numbered:literal, $row:expr, $words:expr, $windows:expr) => {{
        let sql = SQL_INSERT_TRANSCRIPT.replace("{p}", &placeholders(12, $numbered));
        let row = $row;

        // The children go FIRST. A failure part-way then leaves the old
        // transcript with no words -- visibly broken, and a re-run fixes it --
        // rather than the new transcript carrying the old words, which reads as
        // complete and is not.
        // Keyed on BOTH the incoming transcript id AND the object, because
        // each covers a case the other misses:
        //
        //   * same id, first run   — the transcript row does not exist yet, so
        //     an object-keyed delete matches nothing and the re-run collides on
        //     `interview_word_pkey`.
        //   * new id, re-run       — the object-keyed arm is the only one that
        //     finds the previous run's words; an id-keyed delete matches
        //     nothing and leaves two runs side by side.
        //
        // A replacement that misses either leaves a transcript that reads as
        // complete and is not, so both are in the predicate.
        let del_words = "DELETE FROM interview_word WHERE transcript_id = {a} \
            OR transcript_id IN \
               (SELECT id FROM interview_transcript WHERE object_id = {b})"
            .replace("{a}", &placeholder(1, $numbered))
            .replace("{b}", &placeholder(2, $numbered));
        let del_windows = "DELETE FROM interview_window WHERE transcript_id = {a} \
            OR transcript_id IN \
               (SELECT id FROM interview_transcript WHERE object_id = {b})"
            .replace("{a}", &placeholder(1, $numbered))
            .replace("{b}", &placeholder(2, $numbered));
        let del_speakers = "DELETE FROM interview_speaker WHERE transcript_id = {a} \
            OR transcript_id IN \
               (SELECT id FROM interview_transcript WHERE object_id = {b})"
            .replace("{a}", &placeholder(1, $numbered))
            .replace("{b}", &placeholder(2, $numbered));

        sqlx::query(&del_words)
            .bind(&row.id)
            .bind(&row.object_id)
            .execute($conn)
            .await
            .map_err(StoreError::Query)?;
        sqlx::query(&del_windows)
            .bind(&row.id)
            .bind(&row.object_id)
            .execute($conn)
            .await
            .map_err(StoreError::Query)?;
        sqlx::query(&del_speakers)
            .bind(&row.id)
            .bind(&row.object_id)
            .execute($conn)
            .await
            .map_err(StoreError::Query)?;

        sqlx::query(&sql)
            .bind(&row.id)
            .bind(&row.object_id)
            .bind(&row.engine)
            .bind(&row.model_id)
            .bind(&row.model_sha256)
            .bind(&row.audio_command)
            .bind(row.sample_rate)
            .bind(&row.language)
            .bind(row.word_count)
            .bind(row.duration_ms)
            .bind(to_ts(row.created_at))
            .bind(to_ts(row.updated_at))
            .execute($conn)
            .await
            .map_err(StoreError::Query)?;

        let word_sql = "INSERT INTO interview_word
                (id, transcript_id, ordinal, text, start_ms, end_ms, confidence, speaker)
              VALUES ({p})"
            .replace("{p}", &placeholders(8, $numbered));
        for w in $words {
            sqlx::query(&word_sql)
                .bind(format!("{}-{}", row.id, w.ordinal))
                .bind(&row.id)
                .bind(w.ordinal)
                .bind(&w.text)
                .bind(w.start_ms)
                .bind(w.end_ms)
                .bind(w.confidence)
                .bind(&w.speaker)
                .execute($conn)
                .await
                .map_err(StoreError::Query)?;
        }

        let window_sql = "INSERT INTO interview_window
                (transcript_id, window_index, start_ms, end_ms, ok, failure)
              VALUES ({p})"
            .replace("{p}", &placeholders(6, $numbered));
        for win in $windows {
            sqlx::query(&window_sql)
                .bind(&row.id)
                .bind(win.window_index)
                .bind(win.start_ms)
                .bind(win.end_ms)
                .bind(if win.ok { 1i32 } else { 0i32 })
                .bind(&win.failure)
                .execute($conn)
                .await
                .map_err(StoreError::Query)?;
        }
    }};
}

/// Write a transcript and its words, replacing whatever was there.
///
/// A transaction, because the portable spelling of "delete children then insert
/// parent and children" is one: SQLite has no data-modifying CTE, so
/// `WITH … INSERT … SELECT` is not an option on this schema.
pub async fn replace_transcript(
    store: &Store,
    row: &TranscriptRow,
    words: &[WordRow],
    windows: &[WindowRow],
) -> Result<(), StoreError> {
    match store {
        Store::Sqlite(pool) => {
            let mut tx = pool.begin().await.map_err(StoreError::Query)?;
            write_all!(&mut *tx, false, row, words, windows);
            tx.commit().await.map_err(StoreError::Query)
        }
        Store::Postgres(pool) => {
            let mut tx = pool.begin().await.map_err(StoreError::Query)?;
            write_all!(&mut *tx, true, row, words, windows);
            tx.commit().await.map_err(StoreError::Query)
        }
    }
}

/// A SINGLE `{p}`, not one per column.
///
/// `placeholders(n, …)` returns the whole list, so twelve markers expanded
/// twelve times produce 144 placeholders against 12 binds — and Postgres
/// answers `syntax error at end of input` at position 51, which names neither
/// the extra placeholders nor the real cause. The `share.rs` and
/// `subtitles.rs` convention is one marker, and it is the only spelling that
/// cannot be multiplied.
/// An UPSERT on the UNIQUE `object_id`, not a bare INSERT.
///
/// A re-transcription after a model update has to REPLACE (spec §4), and the
/// children have already been deleted above, so a plain INSERT would fail on
/// `interview_transcript_pkey` and a re-run could never succeed.
///
/// `created_at` is deliberately NOT in the update list: it is when the object
/// was first transcribed, and overwriting it on every re-run would make
/// "how long has this transcript been stale" unanswerable. `updated_at` is the
/// only timestamp that moves.
const SQL_INSERT_TRANSCRIPT: &str = "INSERT INTO interview_transcript
    (id, object_id, engine, model_id, model_sha256, audio_command,
     sample_rate, language, word_count, duration_ms, created_at, updated_at)
  VALUES ({p})
  ON CONFLICT (object_id) DO UPDATE SET
     id           = EXCLUDED.id,
     engine       = EXCLUDED.engine,
     model_id     = EXCLUDED.model_id,
     model_sha256 = EXCLUDED.model_sha256,
     audio_command = EXCLUDED.audio_command,
     sample_rate  = EXCLUDED.sample_rate,
     language     = EXCLUDED.language,
     word_count   = EXCLUDED.word_count,
     duration_ms  = EXCLUDED.duration_ms,
     updated_at   = EXCLUDED.updated_at";

/// Row decoders, as macros keyed on the CONCRETE row type.
///
/// Not `fn read<R: Row>(&R)`: `ColumnIndex<R>` is not implemented for `str`
/// without naming `R` concretely, so a generic decoder does not compile. The
/// macro instantiates per pool type, which is the same reason the query
/// execution below is a macro.
macro_rules! read_transcript {
    ($r:expr, $row:ty) => {{
        let r: &$row = $r;
        let created: String = r.try_get("created_at").map_err(col_err)?;
        let updated: String = r.try_get("updated_at").map_err(col_err)?;
        Ok::<TranscriptRow, StoreError>(TranscriptRow {
            id: r.try_get("id").map_err(col_err)?,
            object_id: r.try_get("object_id").map_err(col_err)?,
            engine: r.try_get("engine").map_err(col_err)?,
            model_id: r.try_get("model_id").map_err(col_err)?,
            model_sha256: r.try_get("model_sha256").map_err(col_err)?,
            audio_command: r.try_get("audio_command").map_err(col_err)?,
            sample_rate: r.try_get("sample_rate").map_err(col_err)?,
            language: r.try_get("language").map_err(col_err)?,
            word_count: r.try_get("word_count").map_err(col_err)?,
            duration_ms: r.try_get("duration_ms").map_err(col_err)?,
            // A corrupt timestamp is bad, but taking the process down on a read
            // is worse: a transcript that cannot be read is a missing
            // transcript, and a re-scan rebuilds it.
            created_at: from_ts(&created).unwrap_or_else(Utc::now),
            updated_at: from_ts(&updated).unwrap_or_else(Utc::now),
        })
    }};
}

macro_rules! read_word {
    ($r:expr, $row:ty) => {{
        let r: &$row = $r;
        Ok::<WordRow, StoreError>(WordRow {
            ordinal: r.try_get("ordinal").map_err(col_err)?,
            text: r.try_get("text").map_err(col_err)?,
            start_ms: r.try_get("start_ms").map_err(col_err)?,
            end_ms: r.try_get("end_ms").map_err(col_err)?,
            confidence: r.try_get("confidence").map_err(col_err)?,
            speaker: r.try_get("speaker").map_err(col_err)?,
        })
    }};
}

macro_rules! read_window {
    ($r:expr, $row:ty) => {{
        let r: &$row = $r;
        let ok: i32 = r.try_get("ok").map_err(col_err)?;
        Ok::<WindowRow, StoreError>(WindowRow {
            window_index: r.try_get("window_index").map_err(col_err)?,
            start_ms: r.try_get("start_ms").map_err(col_err)?,
            end_ms: r.try_get("end_ms").map_err(col_err)?,
            ok: ok != 0,
            failure: r.try_get("failure").map_err(col_err)?,
        })
    }};
}

fn col_err(e: sqlx::Error) -> StoreError {
    StoreError::Row {
        column: "interview",
        source: e,
    }
}

/// The transcript for an object, if it has one.
pub async fn transcript_for(
    store: &Store,
    object_id: &str,
) -> Result<Option<TranscriptRow>, StoreError> {
    const SQL: &str = "SELECT * FROM interview_transcript WHERE object_id = {p}";
    macro_rules! go {
        ($p:expr, $numbered:literal, $row:ty) => {{
            let sql = SQL.replace("{p}", &placeholders(1, $numbered));
            sqlx::query(&sql)
                .bind(object_id)
                .fetch_optional($p)
                .await
                .map_err(StoreError::Query)
                // `and_then` into a fallible decoder. The `None` arm has to
                // produce `Ok(None)` rather than use `?`, because the closure
                // returns a Result and `?` on an Option is a type error.
                .and_then(|row| match row {
                    Some(r) => {
                        let r: $row = r;
                        read_transcript!(&r, $row).map(Some)
                    }
                    None => Ok(None),
                })
        }};
    }
    match store {
        Store::Sqlite(p) => go!(p, false, sqlx::sqlite::SqliteRow),
        Store::Postgres(p) => go!(p, true, sqlx::postgres::PgRow),
    }
}

/// Every word of a transcript, in time order.
///
/// `ORDER BY start_ms, ordinal` rather than `ordinal` alone: `ordinal` is the
/// order the words were *produced*, and a window that failed leaves a gap that
/// `start_ms` makes visible and `ordinal` does not.
pub async fn words_for(store: &Store, transcript_id: &str) -> Result<Vec<WordRow>, StoreError> {
    const SQL: &str = "SELECT ordinal, text, start_ms, end_ms, confidence, speaker \
        FROM interview_word WHERE transcript_id = {p} ORDER BY start_ms, ordinal";
    macro_rules! go {
        ($p:expr, $numbered:literal, $row:ty) => {{
            let sql = SQL.replace("{p}", &placeholders(1, $numbered));
            let rows = sqlx::query(&sql)
                .bind(transcript_id)
                .fetch_all($p)
                .await
                .map_err(StoreError::Query)?;
            rows.iter().map(|r| read_word!(r, $row)).collect()
        }};
    }
    match store {
        Store::Sqlite(p) => go!(p, false, sqlx::sqlite::SqliteRow),
        Store::Postgres(p) => go!(p, true, sqlx::postgres::PgRow),
    }
}

/// The words inside a time range — the question the `(transcript_id, start_ms)`
/// index exists for.
pub async fn words_between(
    store: &Store,
    transcript_id: &str,
    from_ms: i32,
    to_ms: i32,
) -> Result<Vec<WordRow>, StoreError> {
    /// The two range bounds are CAST, and that is not decoration.
    ///
    /// On Postgres an untyped `$n` in a comparison against an `INTEGER` column
    /// is inferred as `text`, so `start_ms >= $2` is `integer >= text` and the
    /// query is refused with `operator does not exist` — a *type* error raised
    /// at PLAN time on the arm that runs the comparison, which is why a
    /// test that only ever lists every word (never a range) would be green over
    /// a broken range query. SQLite widens everything and is silent.
    /// Three DISTINCT markers, because `.replace` is positional-blind: it
    /// always fills the FIRST remaining `{p}`, so three identical markers and
    /// a single `placeholders(3, …)` put `$1, $2, $3` in the wrong slots — the
    /// transcript id got an integer cast, and the query failed with
    /// `invalid input syntax for type integer: "tr-…"`. Naming them per
    /// parameter is the only spelling that cannot be permuted.
    /// `{a}` / `{b}` / `{c}`, not three `{p}`s.
    ///
    /// Two separate traps in one line. `placeholders(3, …)` returns the whole
    /// comma-joined list, so it cannot be spliced into three slots; and a
    /// marker that is a PREFIX of another (`{p}` inside `{p:i}`) is rewritten
    /// by the other marker's replacement, whichever runs first — which put
    /// `$2` in both range bounds and made the range query return nothing, with
    /// no error anywhere. Distinct, non-overlapping names make the order
    /// irrelevant.
    const SQL: &str = "SELECT ordinal, text, start_ms, end_ms, confidence, speaker \
        FROM interview_word \
        WHERE transcript_id = {a} \
          AND start_ms >= CAST({b} AS INTEGER) \
          AND start_ms <  CAST({c} AS INTEGER) \
        ORDER BY start_ms, ordinal";

    macro_rules! go {
        ($p:expr, $numbered:literal, $row:ty) => {{
            let sql = SQL
                .replace("{a}", &placeholder(1, $numbered))
                .replace("{b}", &placeholder(2, $numbered))
                .replace("{c}", &placeholder(3, $numbered));
            let rows = sqlx::query(&sql)
                .bind(transcript_id)
                .bind(from_ms)
                .bind(to_ms)
                .fetch_all($p)
                .await
                .map_err(StoreError::Query)?;
            rows.iter().map(|r| read_word!(r, $row)).collect()
        }};
    }
    match store {
        Store::Sqlite(p) => go!(p, false, sqlx::sqlite::SqliteRow),
        Store::Postgres(p) => go!(p, true, sqlx::postgres::PgRow),
    }
}

/// The windows that failed, so a short transcript can explain itself.
///
/// Without this a transcript that lost 20 minutes to engine errors is
/// indistinguishable from one of a 20-minute interview.
pub async fn failed_windows(
    store: &Store,
    transcript_id: &str,
) -> Result<Vec<WindowRow>, StoreError> {
    const SQL: &str = "SELECT window_index, start_ms, end_ms, ok, failure \
        FROM interview_window WHERE transcript_id = {p} AND ok = 0 ORDER BY window_index";
    macro_rules! go {
        ($p:expr, $numbered:literal, $row:ty) => {{
            let sql = SQL.replace("{p}", &placeholders(1, $numbered));
            let rows = sqlx::query(&sql)
                .bind(transcript_id)
                .fetch_all($p)
                .await
                .map_err(StoreError::Query)?;
            rows.iter().map(|r| read_window!(r, $row)).collect()
        }};
    }
    match store {
        Store::Sqlite(p) => go!(p, false, sqlx::sqlite::SqliteRow),
        Store::Postgres(p) => go!(p, true, sqlx::postgres::PgRow),
    }
}

// ---------------------------------------------------------------- correction

/// A human's correction to one word, as a `FieldProposal`.
///
/// The ticket's second accept criterion, and the whole reason this function
/// exists: **a corrected word becomes a proposal, not a silent overwrite**
/// (§8.1, spec §1).
///
/// There is deliberately no `update_word` in this module, and no function
/// anywhere that writes `interview_word` outside `replace_transcript`. Once the
/// model's output is overwritten, nobody can tell a misheard word from a typo,
/// and a user who corrects the same word a second time has nothing to compare
/// against. The evidence is the feature.
///
/// # What the `field` string carries, and why it is not a bare word index
///
/// `transcript_word[<ordinal>]` — the transcript, the word, in that order. The
/// bare ordinal would be ambiguous across the library: ordinal 12 of a
/// two-hour interview and ordinal 12 of a four-minute one are unrelated words,
/// and a field string that cannot say which is not a field string, it is a
/// coincidence.
///
/// # Why the ORIGINAL text is in the value
///
/// The proposal's value is what the user believes the word was. What the engine
/// heard is `interview_word.text`, and it is left alone. Putting the engine's
/// guess in the proposal would make an accepted correction look like it agreed
/// with the model, which is the one thing a correction says it does not.
///
/// # Idempotence
///
/// The unique index on `(subject_type, subject_id, field, value_json, source,
/// COALESCE(proposer_id, ''))` means the same user correcting the same word to
/// the same value twice is one proposal. A user who fixes a typo, undoes it,
/// and fixes it again must not leave two rows for the reviewer to reconcile.
pub async fn propose_correction(
    store: &Store,
    object_id: &str,
    word_ordinal: i32,
    corrected_text: &str,
    corrected_by: Option<&str>,
) -> Result<String, StoreError> {
    use commons_core::{ProposalSource, ProposerKind, SubjectType};
    use uuid::Uuid;

    // A negative ordinal cannot address a word, and writing a proposal for it
    // would put a correction on file that refers to nothing.
    if word_ordinal < 0 {
        return Err(StoreError::Invalid {
            what: "word ordinal",
            why: format!("{word_ordinal} is not a word position"),
        });
    }
    if corrected_text.trim().is_empty() {
        return Err(StoreError::Invalid {
            what: "corrected text",
            why: "an empty correction is a deletion, not a correction".to_string(),
        });
    }

    let id = Uuid::new_v4();
    let field = format!("transcript_word[{word_ordinal}]");
    // JSON, because a FieldProposal's value is always JSON and a bare string
    // that happens to contain a quote would produce a row nothing can read back.
    let value_json = serde_json::to_string(corrected_text).map_err(|e| StoreError::Invalid {
        what: "corrected text",
        why: e.to_string(),
    })?;
    let created_at = to_ts(Utc::now());
    let proposer_kind = match corrected_by {
        Some(_) => ProposerKind::User.as_str(),
        // No proposer is an AUTO correction -- a plugin, or a later run of the
        // pipeline. Never silently recorded as a human, because §8.2 has the UI
        // show "proposed by 4 users" beside it.
        None => ProposerKind::Auto.as_str(),
    };

    const SQL: &str = "INSERT INTO field_proposal
        (id, subject_type, subject_id, field, value_json, source,
         proposer_kind, proposer_id, confidence, created_at, justification)
      VALUES ({p})";
    macro_rules! go {
        ($p:expr, $numbered:literal) => {{
            let sql = SQL.replace("{p}", &placeholders(11, $numbered));
            sqlx::query(&sql)
                .bind(id.to_string())
                .bind(SubjectType::Object.as_str())
                .bind(object_id)
                .bind(&field)
                .bind(&value_json)
                .bind(ProposalSource::Transcript.as_str())
                .bind(proposer_kind)
                .bind(corrected_by)
                .bind(None::<f64>)
                .bind(&created_at)
                // The justification is §8.2's requirement, and here it is the
                // provenance string: which engine, which model, which digest,
                // which word position. A reviewer deciding whether to accept a
                // correction needs to know the claim being corrected.
                .bind(Some(format!(
                    "word {word_ordinal} of the transcript, corrected from the model output"
                )))
                .execute($p)
                .await
                .map_err(StoreError::Query)
        }};
    }
    // The two `QueryResult` types are unrelated, so the arms cannot be
    // expressions of a `match` that yields one. Each arm discards the result
    // and returns the id, which is the only thing the caller wanted.
    match store {
        Store::Sqlite(p) => {
            go!(p, false)?;
        }
        Store::Postgres(p) => {
            go!(p, true)?;
        }
    }
    Ok(id.to_string())
}

/// The corrections on file for an object.
///
/// Read by the UI beside the transcript, so the reader can see BOTH what the
/// model said and what a person believes it said — which is the entire reason
/// the correction is a proposal.
pub async fn corrections_for(
    store: &Store,
    object_id: &str,
) -> Result<Vec<(String, String, String)>, StoreError> {
    use commons_core::{ProposalSource, SubjectType};

    // `field LIKE 'transcript_word[%]'` rather than an exact name, because the
    // ordinal is part of the field and there is no way to enumerate the
    // ordinals without reading every word.
    // The third column is ALIASED. Without one, Postgres returns it under the
    // expression text (`coalesce`) and SQLite under `proposer_id`, so reading
    // it by name is engine-dependent and one arm fails at decode. An alias is
    // the same on both.
    const SQL: &str = "SELECT field, value_json, COALESCE(proposer_id, '') AS proposer
        FROM field_proposal
        WHERE subject_type = {a} AND subject_id = {b}
          AND source = {c} AND field LIKE {d}
        ORDER BY created_at";
    macro_rules! go {
        ($p:expr, $numbered:literal, $row:ty) => {{
            let sql = SQL
                .replace("{a}", &placeholder(1, $numbered))
                .replace("{b}", &placeholder(2, $numbered))
                .replace("{c}", &placeholder(3, $numbered))
                .replace("{d}", &placeholder(4, $numbered));
            let rows = sqlx::query(&sql)
                .bind(SubjectType::Object.as_str())
                .bind(object_id)
                .bind(ProposalSource::Transcript.as_str())
                // Escaped, because `%` and `_` in a LIKE pattern are wildcards
                // and this one is a literal prefix followed by a wildcard. A
                // bare `'transcript_word[%'` is right here, but only because
                // nothing upstream can inject into it -- the value is a const.
                .bind("transcript_word[%")
                .fetch_all($p)
                .await
                .map_err(StoreError::Query)?;
            rows.iter()
                .map(|r| {
                    let r: &$row = r;
                    Ok((
                        r.try_get("field").map_err(col_err)?,
                        r.try_get("value_json").map_err(col_err)?,
                        r.try_get("proposer").map_err(col_err)?,
                    ))
                })
                .collect()
        }};
    }
    match store {
        Store::Sqlite(p) => go!(p, false, sqlx::sqlite::SqliteRow),
        Store::Postgres(p) => go!(p, true, sqlx::postgres::PgRow),
    }
}

// ---------- quotes (T-P6-004b step 2) ----------
//
// Migration `0024_interview_quotes.sql`.
//
// # The one decision here: a quote hangs off the OBJECT, not the transcript
//
// `replace_transcript` deletes `interview_word` and `interview_window` on a
// re-transcription, keyed on both the incoming transcript id and the object —
// see the long comment on the delete inside it, which explains why each arm
// covers a case the other misses. This module does NOT delete quotes, and the
// reason is the whole reason the table is keyed on `object_id`:
//
// a re-transcription replaces the WORDS. The thing a person found worth
// quoting is not a property of those words. If a new model hears the same
// sentence differently and the quote silently re-renders itself from the new
// transcript, then the quote is no longer the one the user chose — and there
// is nothing to compare against, because the original is gone. A drift between
// a quote and the words it was cut from is VISIBLE. A quote that silently
// updates itself is not.
//
// So the deletion is a non-action, deliberately. A future reader will see
// `interview_quote` in the schema, see that `replace_transcript` deletes two
// of the three interview child tables and not the third, and assume the third
// was an oversight. It is not, and this comment is the reason why.

/// Who decided a quote is worth surfacing, and therefore what scale its
/// `weight` is on.
///
/// A human marks a quote on a 1-5 scale; a model emits a salience in
/// 0.0-1.0. The two are not comparable as numbers, and a ranking that mixes
/// them without normalising is wrong in a way nothing reports. See
/// [`comparable_weight`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WeightSource {
    /// A person marked it. 1-5, the scale the UI offers.
    Human,
    /// A model scored it. 0.0-1.0, the scale a salience model emits.
    Model,
}

impl WeightSource {
    /// The stored spelling, matching the CHECK constraint in migration 0024.
    pub fn as_str(self) -> &'static str {
        match self {
            WeightSource::Human => "human",
            WeightSource::Model => "model",
        }
    }
}

/// Put a human's weight and a model's weight on one scale.
///
/// **This is the only function in the crate that knows they are different
/// units**, which is why it exists as a pure function with no I/O: the mistake
/// it prevents is a sort that reads one column and silently compares two
/// scales, and that mistake produces a *wrong order* rather than an error.
///
/// A human's mark is taken at face value — 1-5 is the band. A model's salience
/// is mapped into that same band, so a model at full confidence can outrank a
/// human's minimum (it is the only evidence anyone has) and a model at zero
/// outranks nothing (it is the model declining to say).
///
/// `MODEL_BAND` is the human band the model range is scaled into. Making it a
/// named constant rather than a literal is the point: if someone later wants a
/// different policy, this is the one line to change, and every test that
/// depends on the ordering is testing *this* value rather than a number
/// repeated in a test.
const MODEL_BAND: f64 = 5.0;

/// Map a weight onto the shared human band. See [`WeightSource`].
pub fn comparable_weight(weight: f64, source: WeightSource) -> f64 {
    match source {
        // A human chose on this scale already.
        WeightSource::Human => weight,
        // A model chose on 0.0-1.0, and the band is 1-5. Clamped at both ends
        // because a salience outside 0-1 means the model is broken, and a
        // broken model should not be able to outrank a confident human.
        WeightSource::Model => (weight.clamp(0.0, 1.0)) * MODEL_BAND,
    }
}

/// A stored quote.
#[derive(Debug, Clone, PartialEq)]
pub struct Quote {
    pub id: String,
    pub object_id: String,
    pub start_ms: i32,
    pub end_ms: i32,
    /// A snapshot, not a live projection of `interview_word`. See the module
    /// comment on why that direction is correct.
    pub text: String,
    pub weight: f64,
    pub weight_source: WeightSource,
}

/// Propose a quote: a span of the interview worth resurfacing.
///
/// Returns the new quote's id, or the id of the existing one when the
/// identical quote is proposed twice — the `UNIQUE (object_id, start_ms,
/// end_ms, text)` constraint is what makes a re-run idempotent rather than a
/// way to fill a list with copies of one sentence.
///
/// The text is stored TRIMMED and validated after trimming, because
/// `"   "` is the empty string wearing a costume: it passes `is_empty()` on
/// some inputs, renders as a blank row in a quote list, and is the shape a
/// test asserts against when it thinks it is testing emptiness.
pub async fn propose_quote(
    store: &Store,
    object_id: &str,
    start_ms: i32,
    end_ms: i32,
    text: &str,
    weight: f64,
    source: WeightSource,
) -> Result<String, StoreError> {
    use uuid::Uuid;

    // A zero-length or inverted span is a quote that exists, renders, and
    // carries no information. Refused here as well as by the schema CHECK, so
    // the caller gets a `StoreError::Invalid` naming the field rather than a
    // constraint violation surfacing as `StoreError::Query`.
    if start_ms < 0 {
        return Err(StoreError::Invalid {
            what: "quote start",
            why: format!("{start_ms}ms is before the start of the media"),
        });
    }
    if end_ms <= start_ms {
        return Err(StoreError::Invalid {
            what: "quote span",
            why: format!("end {end_ms}ms is not after start {start_ms}ms"),
        });
    }
    let text = text.trim();
    if text.is_empty() {
        return Err(StoreError::Invalid {
            what: "quote text",
            why: "a blank quote is not a quote".to_string(),
        });
    }
    // A non-finite weight sorts to the top of a DESC and to the bottom of an
    // ASC, depending on the database, which is not a property anyone can
    // explain to a user. Rejected instead.
    if !weight.is_finite() {
        return Err(StoreError::Invalid {
            what: "quote weight",
            why: format!("{weight} is not a finite weight"),
        });
    }

    let id = Uuid::new_v4();
    let created_at = to_ts(Utc::now());

    const SQL: &str = "INSERT INTO interview_quote
        (id, object_id, start_ms, end_ms, text, weight, weight_source, created_at)
      VALUES ({p})";
    macro_rules! go {
        ($p:expr, $numbered:literal) => {{
            let sql = SQL.replace("{p}", &placeholders(8, $numbered));
            sqlx::query(&sql)
                .bind(id.to_string())
                .bind(object_id)
                .bind(start_ms)
                .bind(end_ms)
                .bind(text)
                // The weight stored is the RAW weight, not `comparable_weight`.
                // Normalising on write would destroy the source's scale and
                // make `weight_source` decorative — the reader would have no
                // way to tell a human's 3 from a model's 0.6 that had already
                // been scaled. The comparison happens at sort time, in
                // `comparable_weight`, where the policy lives.
                .bind(weight)
                .bind(source.as_str())
                .bind(&created_at)
                .execute($p)
                .await
                // The result is discarded INSIDE the macro. `SqliteQueryResult`
                // and `PgQueryResult` are different types, so a macro whose
                // tail is the execute would make the two match arms
                // incompatible -- and the error names `execute`, not the
                // match, which is a confusing way to be told a query returns
                // two things. Nothing here needs the row count: the UNIQUE
                // constraint makes a duplicate an error, not a no-op.
                .map(|_| ())
        }};
    }

    match store {
        Store::Sqlite(conn) => go!(conn, false),
        Store::Postgres(conn) => go!(conn, true),
    }
    .map_err(StoreError::Query)?;

    Ok(id.to_string())
}

/// Every quote for an object, in time order.
///
/// Time order is the only order a quote list can be read in: a list sorted by
/// id or by weight has a "next" that goes sideways.
pub async fn quotes_for(store: &Store, object_id: &str) -> Result<Vec<Quote>, StoreError> {
    const SQL: &str = "SELECT id, object_id, start_ms, end_ms, text, weight, weight_source
        FROM interview_quote
        WHERE object_id = {a}
        ORDER BY start_ms, id";
    macro_rules! go {
        ($p:expr, $numbered:literal, $row:ty) => {{
            let sql = SQL.replace("{a}", &placeholder(1, $numbered));
            let rows = sqlx::query(&sql)
                .bind(object_id)
                .fetch_all($p)
                .await
                .map_err(StoreError::Query)?;
            rows.iter()
                .map(|r| {
                    let r: &$row = r;
                    Ok(Quote {
                        id: r.get("id"),
                        object_id: r.get("object_id"),
                        start_ms: r.get("start_ms"),
                        end_ms: r.get("end_ms"),
                        text: r.get("text"),
                        weight: r.get("weight"),
                        weight_source: if r.get::<String, _>("weight_source") == "model" {
                            WeightSource::Model
                        } else {
                            WeightSource::Human
                        },
                    })
                })
                .collect::<Result<Vec<Quote>, StoreError>>()
        }};
    }

    match store {
        Store::Sqlite(conn) => go!(conn, false, sqlx::sqlite::SqliteRow),
        Store::Postgres(conn) => go!(conn, true, sqlx::postgres::PgRow),
    }
}

/// How many quotes an object has. Cheaper than `quotes_for(..).len()` when the
/// caller only wants a count for a summary — which is what the server route
/// does.
pub async fn quote_count(store: &Store, object_id: &str) -> Result<i64, StoreError> {
    const SQL: &str = "SELECT count(*) FROM interview_quote WHERE object_id = {a}";
    macro_rules! go {
        ($p:expr, $numbered:literal) => {{
            let sql = SQL.replace("{a}", &placeholder(1, $numbered));
            sqlx::query_scalar::<_, i64>(&sql)
                .bind(object_id)
                .fetch_one($p)
                .await
                .map_err(StoreError::Query)
        }};
    }

    match store {
        Store::Sqlite(conn) => go!(conn, false),
        Store::Postgres(conn) => go!(conn, true),
    }
}

#[cfg(test)]
mod quote_weight_tests {
    // T-P6-004b step 3, and deliberately placed in the SOURCE rather than in
    // the db suite: `comparable_weight` has no I/O, so a test that needs a
    // database to check an arithmetic function is a slower test of a simpler
    // thing. It lives next to the function for the ordinary reason, and
    // because the two are meant to be read together.
    use super::{comparable_weight, WeightSource, MODEL_BAND};

    /// A human's mark is on the 1-5 scale and passes through untouched.
    ///
    /// The identity assertion is the point: if this ever stops being identity,
    /// every human mark in the library is being silently rescaled.
    #[test]
    fn a_humans_weight_is_taken_at_face_value() {
        for w in [1.0, 2.0, 3.0, 4.0, 5.0] {
            assert_eq!(
                comparable_weight(w, WeightSource::Human),
                w,
                "a human's {w} must not be moved"
            );
        }
    }

    /// A model's salience is mapped into the human band.
    #[test]
    fn a_models_weight_is_scaled_into_the_human_band() {
        assert_eq!(comparable_weight(0.0, WeightSource::Model), 0.0);
        assert_eq!(comparable_weight(1.0, WeightSource::Model), MODEL_BAND);
        assert_eq!(comparable_weight(0.6, WeightSource::Model), 0.6 * MODEL_BAND);
    }

    /// The cross-source case, and the reason this function exists.
    ///
    /// A model at full confidence outranks a human's minimum: it is the only
    /// evidence anyone has about a quote nobody marked. This is the assertion
    /// that fails if someone "simplifies" the mapping to a plain comparison of
    /// raw numbers, which is the bug in its most likely form.
    #[test]
    fn a_confident_model_outranks_a_humans_minimum() {
        assert!(
            comparable_weight(1.0, WeightSource::Model) > comparable_weight(1.0, WeightSource::Human),
            "a model at full confidence ({}) beats a human's 1 ({})",
            comparable_weight(1.0, WeightSource::Model),
            comparable_weight(1.0, WeightSource::Human)
        );
    }

    /// ...and a model that declines to say outranks nothing at all.
    ///
    /// Zero maps to zero, so a silent model loses to a human's 1.0 and to
    /// everything above it. Worth being explicit that zero is not rounded UP to
    /// the bottom of the human band: with a mapping that floored at 1, a model
    /// that declined to say would tie a human's weakest possible mark, and a
    /// "best quotes" list that leads with "the model had nothing to say about
    /// this" is a list nobody believes.
    #[test]
    fn a_silent_model_outranks_nothing() {
        assert_eq!(
            comparable_weight(0.0, WeightSource::Model),
            0.0,
            "a model at zero is zero, not a human's minimum"
        );
        assert!(
            comparable_weight(0.0, WeightSource::Model) < comparable_weight(1.0, WeightSource::Human),
            "and it does not beat a human's minimum"
        );
        assert!(
            comparable_weight(0.0, WeightSource::Model) < comparable_weight(5.0, WeightSource::Human),
            "nor a human's maximum"
        );
    }

    /// A salience outside 0-1 means the model is broken, and a broken model must
    /// not be able to outrank a confident human.
    #[test]
    fn a_model_outside_its_range_is_clamped_rather_than_trusted() {
        assert_eq!(
            comparable_weight(2.0, WeightSource::Model),
            MODEL_BAND,
            "a salience of 2.0 is nonsense and is clamped"
        );
        assert_eq!(comparable_weight(-1.0, WeightSource::Model), 0.0, "and so is a negative");
    }

    /// The test the plan asks for: an INTERLEAVED list must come out in
    /// separate bands.
    ///
    /// Sorting by the raw `weight` column — the obvious implementation, and one
    /// a query can do for free — would put the human's 2 and 3 below the
    /// model's 0.9, because 0.9 < 2. Nothing errors here. The list is simply
    /// ordered wrongly, which is spec §4.2 in full.
    #[test]
    fn the_two_sources_come_out_in_separate_bands_not_interleaved() {
        let items: Vec<(&str, f64, WeightSource)> = vec![
            ("human 2", 2.0, WeightSource::Human),
            ("model 0.9", 0.9, WeightSource::Model),
            ("human 5", 5.0, WeightSource::Human),
            ("model 0.1", 0.1, WeightSource::Model),
        ];

        // Sorted through the mapping, DESC — what a "best quotes first" list
        // should do.
        let mut sorted = items.clone();
        sorted.sort_by(|a, b| {
            comparable_weight(b.1, b.2)
                .partial_cmp(&comparable_weight(a.1, a.2))
                .expect("comparable_weight is always finite")
        });
        let order: Vec<&str> = sorted.iter().map(|i| i.0).collect();
        assert_eq!(
            order,
            ["human 5", "model 0.9", "human 2", "model 0.1"],
            "sorted through the mapping: {order:?}"
        );

        // And the failure this exists to prevent: sorting the raw column puts
        // both human marks above both model marks, which is the OPPOSITE
        // interleaving and looks superficially reasonable.
        let mut raw = items.clone();
        raw.sort_by(|a, b| b.1.partial_cmp(&a.1).expect("finite"));
        let raw_order: Vec<&str> = raw.iter().map(|i| i.0).collect();
        assert_ne!(
            raw_order, order,
            "sorting the raw column gives a DIFFERENT list -- which is why the \
             mapping has to be in the sort and not in the column"
        );
    }
}
