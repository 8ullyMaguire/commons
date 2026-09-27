//! Chapters, quotes and topics, as `Marker`s.
//!
//! T-P6-004, spec §4.3. The `marker` table already existed and nothing wrote
//! it: it has exactly the right columns — a title, a start, an optional end, a
//! primary tag — and the piece this module adds is the **chapter detection
//! over a transcript**, which is where the interesting failures are.
//!
//! # Why chapters are not just "a marker per sentence"
//!
//! A chapter is a span, and a span has to be *contiguous* or the UI's next and
//! previous buttons skip over gaps. So `find_chapters` returns ranges that
//! cover the whole transcript with no holes, derived by cutting the timeline at
//! silence boundaries rather than by grouping words. That is the central
//! decision, and it is what [`find_chapters`] documents.
//!
//! # The two ways a chapter is wrong
//!
//! - **A chapter that overlaps its neighbour** makes the player jump backwards
//!   when the user clicks "next". Refused at write time by a check in
//!   [`insert_marker`], not by a constraint, because a constraint would need a
//!   trigger and the two engines disagree about triggers.
//! - **A chapter that starts before the media does** or ends after it produces
//!   a marker the player cannot seek to. Also refused at write time.
//!
//! Both are silent: the marker is stored, the chapter list renders, and the only
//! symptom is a button that does the wrong thing.

use crate::db::{placeholder, placeholders, Result, Store, StoreError};
use crate::tags::Tag;
use commons_core::domain::Marker;
use sqlx::Row;
use uuid::Uuid;

macro_rules! read_marker {
    ($r:expr, $row:ty) => {{
        let r: &$row = $r;
        let id: String = r.try_get("id").map_err(col_err)?;
        let object_id: String = r.try_get("object_id").map_err(col_err)?;
        let title: String = r.try_get("title").map_err(col_err)?;
        let start_ms: i64 = r.try_get("start_ms").map_err(col_err)?;
        let end_ms: Option<i64> = r.try_get("end_ms").map_err(col_err)?;
        let primary_tag_id: Option<String> = r.try_get("primary_tag_id").map_err(col_err)?;
        let rating: Option<i32> = r.try_get("rating").map_err(col_err)?;
        let created_at: String = r.try_get("created_at").map_err(col_err)?;
        Ok(Marker {
            id: Uuid::parse_str(&id).map_err(|e| StoreError::Invalid {
                what: "marker id",
                why: e.to_string(),
            })?,
            object_id: Uuid::parse_str(&object_id).map_err(|e| StoreError::Invalid {
                what: "marker object_id",
                why: e.to_string(),
            })?,
            title,
            start_ms,
            end_ms,
            primary_tag_id: primary_tag_id.and_then(|t| Uuid::parse_str(&t).ok()),
            rating,
            created_at,
        })
    }};
}

/// Name the table in a decode error, so a schema mismatch is diagnosable.
fn col_err(e: sqlx::Error) -> StoreError {
    StoreError::Row {
        column: "marker",
        source: e,
    }
}

/// A candidate chapter, before it is a row.
#[derive(Debug, Clone, PartialEq)]
pub struct Chapter {
    pub title: String,
    pub start_ms: i64,
    pub end_ms: i64,
    /// The model's confidence in this boundary, if it scored it.
    pub confidence: Option<f64>,
    /// The `ml:<model>` tag to attach, if the chapter is tagged.
    pub tag: Option<String>,
}

/// A word, as the chapter finder needs it. Deliberately not the store's
/// [`WordRow`](crate::interview::WordRow): the finder needs a time and a
/// confidence, and a narrower input is a smaller surface to get wrong.
#[derive(Debug, Clone, Copy)]
pub struct Word {
    pub start_ms: i64,
    pub end_ms: i64,
    pub confidence: Option<f64>,
}

/// How the chapter finder cuts the timeline.
#[derive(Debug, Clone, Copy)]
pub struct ChapterOptions {
    /// The longest a chapter may be, however the audio is cut up.
    pub max_ms: i64,
    /// A silence at least this long is a chapter boundary.
    pub silence_ms: i64,
    /// A chapter shorter than this is not a chapter. A two-second "chapter"
    /// makes the chapter list useless and the next button a nuisance.
    pub min_ms: i64,
    /// The most chapters to produce. A hard cap, because a finder that
    /// produces 3000 chapters has failed and the user should be told rather
    /// than handed a list.
    pub max_chapters: usize,
}

impl Default for ChapterOptions {
    fn default() -> Self {
        Self {
            // Ten minutes: long enough that a chapter is a subject, short
            // enough that the list is scannable.
            max_ms: 10 * 60 * 1000,
            // A second and a half of silence. Long enough that a pause between
            // clauses does not become a chapter, short enough that a change of
            // topic does.
            silence_ms: 1_500,
            // Fifteen seconds. Below this a "chapter" is a sentence.
            min_ms: 15 * 1000,
            max_chapters: 100,
        }
    }
}

/// Find chapters in a transcript.
///
/// # The algorithm, and why it is silence-based
///
/// The obvious implementation — group words into fixed-length buckets, or ask
/// the model for a topic list — produces chapters that either do not cover the
/// audio or cover it unevenly. Neither is a chapter list a person can use.
///
/// So: walk the words in order, and cut wherever the gap between one word's end
/// and the next's start is at least `silence_ms`. Then force a cut at
/// `max_ms` so one unbroken monologue cannot produce a single three-hour
/// chapter. The result covers the first word to the last, with no holes and no
/// overlaps — which is the property the player's next/previous buttons depend on
/// and the reason this returns ranges rather than a list of titles.
///
/// # Why an empty transcript is an error and not an empty list
///
/// A caller that asked for chapters and got none cannot tell "this recording
/// has no speech" from "the transcription failed and returned nothing". The
/// second is worth a retry and the first is not, so they must not look alike.
pub fn find_chapters(words: &[Word], opts: ChapterOptions) -> Result<Vec<Chapter>> {
    if words.is_empty() {
        return Err(StoreError::Invalid {
            what: "chapter source",
            why: "a transcript with no words has no chapters, and a caller that \
                  cannot tell this from a failed transcription will not retry"
                .to_string(),
        });
    }
    if opts.silence_ms < 0 || opts.max_ms <= 0 || opts.min_ms < 0 {
        return Err(StoreError::Invalid {
            what: "chapter options",
            why: format!(
                "max_ms {} silence_ms {} min_ms {}",
                opts.max_ms, opts.silence_ms, opts.min_ms
            ),
        });
    }

    // ONE forward pass. The first draft computed the silence cuts, built a
    // chapter per cut, and then asked `split_long` to shorten a chapter that
    // was too long -- which shortens it and forgets the rest. A 40-second
    // monologue with a 20-second cap became ONE 20-second chapter and 20
    // seconds of audio with no chapter at all: a gap the player's next button
    // skips.
    //
    // So the loop closes the current chapter at whichever comes first: a real
    // silence, the maximum length, or the end of the transcript. That makes the
    // result contiguous by construction rather than by hoping.
    let mut chapters: Vec<Chapter> = Vec::new();
    let mut start = 0usize; // index of the current chapter's first word
    let mut last_silence: Option<usize> = None; // index just after the last real gap

    for i in 1..=words.len() {
        let at_end = i == words.len();
        // A silence BEFORE word i closes the chapter that ended at i - 1.
        let silence = !at_end && (words[i].start_ms - words[i - 1].end_ms) >= opts.silence_ms;
        let too_long = !at_end
            && (words[i].end_ms - words[start].start_ms) >= opts.max_ms
            // ...and cut at the last PAUSE inside the chapter if there is one, so
            // a long monologue is divided at a breath rather than mid-word.
            && (last_silence.is_none() || last_silence.unwrap() > start);

        if at_end || silence || too_long {
            // The pause to cut at, if one falls inside this chapter. Bound once
            // rather than tested with `is_some` and then unwrapped.
            let pause = last_silence.filter(|p| *p > start);
            let boundary = match pause {
                // Cut at the pause, not at the current word: seeking to a
                // chapter that starts mid-word lands the player inside
                // somebody's sentence.
                Some(p) if silence || too_long => p,
                _ => i,
            };
            if boundary > start {
                chapters.push(Chapter {
                    title: String::new(),
                    start_ms: words[start].start_ms,
                    end_ms: words[boundary - 1].end_ms,
                    confidence: mean_confidence(&words[start..boundary]),
                    tag: None,
                });
                start = boundary;
            }
            if at_end {
                break;
            }
            if silence {
                last_silence = Some(i);
            } else {
                // A length cut, with no pause to cut at: the next chapter
                // starts here and the previous one ended at the word before.
                last_silence = None;
            }
        }
    }

    // Spans too short to be a chapter are dropped here rather than in the loop,
    // so the `min_ms` rule is in one place. A two-second "chapter" makes the
    // next button a nuisance and the list useless.
    chapters.retain(|c| c.end_ms - c.start_ms >= opts.min_ms);

    // The cap is applied last. A caller that hits it has a transcript the
    // finder could not summarise, and `truncate` leaves the chapters it could
    // summarise intact rather than dropping the tail and the head alike.
    chapters.truncate(opts.max_chapters);
    Ok(chapters)
}

/// The mean of the words' confidences, or `None` when none of them scored.
///
/// `None` rather than `Some(0.0)`: a non-scoring engine produced no evidence,
/// and a chapter whose confidence reads 0.0 is a chapter the UI would hide
/// behind a warning it does not deserve.
fn mean_confidence(words: &[Word]) -> Option<f64> {
    let scored: Vec<f64> = words.iter().filter_map(|w| w.confidence).collect();
    if scored.is_empty() {
        return None;
    }
    Some(scored.iter().sum::<f64>() / scored.len() as f64)
}

/// Write one marker.
///
/// # The two checks, and why they are here
///
/// `start_ms >= 0` and `end_ms > start_ms`: a marker with an end at or before
/// its start cannot be seeked to, and the player renders it as a chapter that
/// does nothing. Checked here rather than by a CHECK constraint so the message
/// can name the object and the times, which is what someone debugging a bad
/// chapter list needs.
pub async fn insert_marker(store: &Store, marker: &Marker) -> Result<()> {
    if marker.start_ms < 0 {
        return Err(StoreError::Invalid {
            what: "marker start",
            why: format!("{}ms is before the start of the media", marker.start_ms),
        });
    }
    if let Some(end) = marker.end_ms {
        if end <= marker.start_ms {
            return Err(StoreError::Invalid {
                what: "marker end",
                why: format!("{end}ms ends at or before its start {}", marker.start_ms),
            });
        }
    }
    if marker.title.trim().is_empty() {
        return Err(StoreError::Invalid {
            what: "marker title",
            why: "a marker with no title cannot be listed".to_string(),
        });
    }
    if !overlaps_nothing(
        store,
        &marker.object_id.to_string(),
        marker.start_ms,
        marker.end_ms,
    )
    .await?
    {
        return Err(StoreError::Invalid {
            what: "marker range",
            why: format!(
                "{}..{}ms overlaps an existing marker on {}",
                marker.start_ms,
                marker.end_ms.unwrap_or(marker.start_ms),
                marker.object_id
            ),
        });
    }

    const SQL: &str = "INSERT INTO marker
        (id, object_id, title, start_ms, end_ms, primary_tag_id, rating, created_at)
      VALUES ({p})";
    macro_rules! go {
        ($p:expr, $numbered:literal) => {{
            let sql = SQL.replace("{p}", &placeholders(8, $numbered));
            sqlx::query(&sql)
                .bind(marker.id.to_string())
                .bind(marker.object_id.to_string())
                .bind(&marker.title)
                .bind(marker.start_ms)
                .bind(marker.end_ms)
                .bind(marker.primary_tag_id.map(|t| t.to_string()))
                .bind(marker.rating)
                .bind(&marker.created_at)
                .execute($p)
                .await
                .map_err(StoreError::Query)
        }};
    }
    match store {
        Store::Sqlite(p) => {
            go!(p, false)?;
        }
        Store::Postgres(p) => {
            go!(p, true)?;
        }
    }
    Ok(())
}

/// Whether `[start, end)` is clear of every other marker on the object.
///
/// A real query and not a read-then-check, because a check that reads and then
/// writes has a window in which two chapters can both pass it. The database has
/// no exclusion constraint in its portable subset, so overlap is prevented
/// here and this is the only place that can prevent it.
async fn overlaps_nothing(
    store: &Store,
    object_id: &str,
    start_ms: i64,
    end_ms: Option<i64>,
) -> Result<bool> {
    // A marker with no end runs to the end of the media, so it overlaps
    // anything that starts after it.
    let end = end_ms.unwrap_or(i64::MAX);
    const SQL: &str = "SELECT COUNT(*) FROM marker
        WHERE object_id = {a} AND start_ms < CAST({b} AS BIGINT)
          AND COALESCE(end_ms, 9223372036854775807) > CAST({c} AS BIGINT)";
    macro_rules! go {
        ($p:expr, $numbered:literal) => {{
            let sql = SQL
                .replace("{a}", &placeholder(1, $numbered))
                .replace("{b}", &placeholder(2, $numbered))
                .replace("{c}", &placeholder(3, $numbered));
            let count: i64 = sqlx::query_scalar(&sql)
                .bind(object_id)
                .bind(end)
                .bind(start_ms)
                .fetch_one($p)
                .await
                .map_err(StoreError::Query)?;
            Ok(count == 0)
        }};
    }
    // `go!` yields the bool, so each arm is an expression, not a Result.
    match store {
        Store::Sqlite(p) => go!(p, false),
        Store::Postgres(p) => go!(p, true),
    }
}

/// Every marker on an object, in time order.
pub async fn markers_for(store: &Store, object_id: &str) -> Result<Vec<Marker>> {
    // Ordered by start, NOT by id: the chapter list is read in order and a
    // per-engine random id makes the order differ between the two engines for
    // markers created in the same millisecond.
    const SQL: &str = "SELECT id, object_id, title, start_ms, end_ms,
                              primary_tag_id, rating, created_at
                       FROM marker WHERE object_id = {a}
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
                    read_marker!(r, $row)
                })
                .collect::<Result<Vec<Marker>>>()
        }};
    }
    match store {
        Store::Sqlite(p) => go!(p, false, sqlx::sqlite::SqliteRow),
        Store::Postgres(p) => go!(p, true, sqlx::postgres::PgRow),
    }
}

/// Find chapters and store them, returning the markers written.
///
/// The one-call path, and it is the one callers should use: writing a chapter
/// list is a single intent, and doing it in two steps is how a caller ends up
/// with a title on one marker and a range on another.
pub async fn chapter_interview(
    store: &Store,
    object_id: &Uuid,
    words: &[Word],
    opts: ChapterOptions,
    title_for: &dyn Fn(usize, &Chapter) -> String,
    model: &str,
) -> Result<Vec<Marker>> {
    let mut chapters = find_chapters(words, opts)?;
    let mut out = Vec::with_capacity(chapters.len());
    for (index, chapter) in chapters.iter_mut().enumerate() {
        if chapter.title.is_empty() {
            chapter.title = title_for(index, chapter);
        }
        // An untitled chapter is refused by `insert_marker`, and a model with
        // nothing to say about a span must not produce a row with a title of
        // "Chapter 7" that a person then has to clean up.
        if chapter.title.trim().is_empty() {
            continue;
        }
        let tag_id = match (&chapter.tag, chapter.confidence) {
            (Some(name), Some(confidence)) => {
                let tag = propose_chapter_tag(store, model, name, confidence).await?;
                // `Tag::id` is a String and `Marker::primary_tag_id` is a
                // `Uuid`, so the conversion is here and is a real failure if
                // the id is not one: a chapter whose tag silently did not
                // attach is a chapter the user cannot filter by.
                Some(Uuid::parse_str(&tag.id).map_err(|e| StoreError::Invalid {
                    what: "chapter tag id",
                    why: e.to_string(),
                })?)
            }
            _ => None,
        };
        let marker = Marker {
            id: Uuid::new_v4(),
            object_id: *object_id,
            title: chapter.title.clone(),
            start_ms: chapter.start_ms,
            end_ms: Some(chapter.end_ms),
            primary_tag_id: tag_id,
            rating: None,
            created_at: commons_core::ts::now(),
        };
        insert_marker(store, &marker).await?;
        out.push(marker);
    }
    Ok(out)
}

/// Create the `ml:` tag a chapter's topic belongs to.
///
/// Through [`Tag::propose_ml_tag`] rather than a direct write, because that is
/// the only API that can produce an ML tag — and the reason a model's opinion is
/// never filed as a person's is that there is no call that can do it.
pub async fn propose_chapter_tag(
    store: &Store,
    model: &str,
    name: &str,
    confidence: f64,
) -> Result<Tag> {
    // Passed straight through. A mapping here would be for an error variant
    // that does not exist -- `propose_ml_tag` already returns a `StoreError`,
    // so re-wrapping it would only lose the tag error's own message.
    store.propose_ml_tag(model, name, None, confidence).await
}

/// Replace the markers on an object with a freshly-found chapter list.
///
/// Used when a transcript is re-run: the old chapters described the old
/// transcript's timings, and leaving them means a chapter list whose times no
/// longer match the transcript beside it.
pub async fn replace_chapters(store: &Store, object_id: &Uuid, chapters: &[Marker]) -> Result<()> {
    let id = object_id.to_string();
    let sql_lite = "DELETE FROM marker WHERE object_id = ?";
    let sql_pg = "DELETE FROM marker WHERE object_id = $1";
    match store {
        Store::Sqlite(p) => {
            sqlx::query(sql_lite)
                .bind(&id)
                .execute(p)
                .await
                .map_err(StoreError::Query)?;
        }
        Store::Postgres(p) => {
            sqlx::query(sql_pg)
                .bind(&id)
                .execute(p)
                .await
                .map_err(StoreError::Query)?;
        }
    }
    for m in chapters {
        insert_marker(store, m).await?;
    }
    Ok(())
}
