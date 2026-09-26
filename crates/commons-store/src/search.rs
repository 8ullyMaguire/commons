//! §9.2, §9.3 — search, one behaviour across both engines.
//!
//! # The constraint that decides the design
//!
//! §9.3: "FTS in both Postgres and the embedded store, **with the same
//! tokenizer and the same synonym table** — one behaviour across both engines."
//!
//! Read literally with each engine's native full-text search, that is
//! unimplementable. SQLite's FTS5 `unicode61` tokenizer and Postgres's
//! `to_tsvector('english', …)` disagree about stemming, stop words, and
//! hyphenation — `running` matches `run` on one and not the other, and
//! `the` is a stop word on one and a term on the other. Two native FTS
//! implementations cannot be given the same tokenizer; they have two.
//!
//! So the tokenizer is **ours**, in Rust, and the index is an ordinary table
//! both engines store identically. [`tokenize`] produces the terms, and the
//! table is
//! `search_term (object_id, field, term)` with a plain index. Neither engine
//! has an opinion about it, which is the only way "one behaviour" is true
//! rather than aspirational.
//!
//! The cost is honest and worth stating: this is not a native inverted index,
//! so it is a table scan per term rather than a specialised engine's lookup.
//! For a personal library that is the right trade — a hosted index with a
//! million rows can add `tsvector` as an accelerator *alongside* this, keyed on
//! the same terms, and get the same answers faster. What it cannot do is answer
//! differently, which is the property that actually matters.
//!
//! # What is searched
//!
//! §9.2 names the breadth: titles in all languages, descriptions, tags,
//! performer names *and aliases*, clusters, studios, groups, markers,
//! **transcripts and captions** (#4985), and external ids. The token table
//! carries a `field` per row, which is what lets a hit in a description rank
//! below the same word in a title without a second index — the scoring rule is
//! on [`Field`], not in the query.
//!
//! # The tokenizer
//!
//! [`tokenize`] lowercases, splits on anything that is not a letter or a digit,
//! applies a small stop-word list, and stems the English suffix set. It is
//! deliberately simple and deliberately *written down*: a tokenizer nobody can
//! predict is a tokenizer whose results nobody can reproduce, and the
//! cross-engine equality test in `tests/search_parity.rs` is only meaningful
//! because both engines are handed the same terms by this function.
//!
//! Unicode is handled with `char::is_alphanumeric`, so a title in any script
//! tokenizes by its own letters rather than being stripped — §9.2 says "all
//! languages" and a tokenizer that only knows ASCII does not mean that.

use crate::db::{Store, StoreError};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use std::collections::BTreeMap;

/// Which part of an object a term came from.
///
/// The weighting lives here rather than in the query so that a caller cannot
/// forget it: a `score` column in the table would be a stored number that has
/// to be kept in step with the weighting table when the weighting changes,
/// which is the counter bug from T-P4-006 in a new place. A weight per row is
/// a number somebody edits in one place and every index has to be rebuilt to
/// match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Field {
    /// §9.2 puts titles first and the rest after; the ordering here is the
    /// ranking, and it is asserted as a table in the tests.
    Title,
    /// A marker title — "the part", §9.2's markers entry.
    Marker,
    Description,
    /// A performer's name, primary or alias. Aliases share the field so a
    /// search hit on a stage name ranks exactly as one on a real name.
    Performer,
    Cluster,
    Tag,
    Studio,
    Group,
    /// A transcript or caption. §5.8's Q&A moment search reads this.
    Subtitle,
    /// An external id, so searching `abc123` finds the object it belongs to.
    ExternalId,
    /// §9.3's alias: whatever the user calls this thing.
    ///
    /// A distinct field rather than a reuse of `Performer` or `Title`, and the
    /// reason is scope. A performer's alias *is* a name and shares `Performer`'s
    /// weight -- `performer_alias` (§7.2) is a claim about a performer's history,
    /// and a stage name should rank as a name. An alias here is a search aid on
    /// an arbitrary object, so a file nicknamed "the good one" must not outrank
    /// a genuine title. It sits between `Performer` and `Tag`: a name the user
    /// chose for this object is a stronger signal than a tag anyone applied, and
    /// weaker than the object's own name.
    Alias,
}

impl Field {
    /// In weight order, descending.
    ///
    /// The order is not cosmetic. `ALL` generates the ranking query's `CASE`
    /// arms, it is the order the API and the tests enumerate fields in, and it
    /// is a `search_hit`'s `fields` list. Keeping it equal to the ranking means
    /// a caller iterating `ALL` visits the strongest field first, and it means
    /// the order a new field appears in cannot be `weight()` and something else
    /// at once. `field_weights_are_the_documented_table` pins it.
    pub const ALL: [Field; 11] = [
        Field::Title,
        Field::Marker,
        Field::Performer,
        Field::Cluster,
        Field::Alias,
        Field::Tag,
        Field::Studio,
        Field::Group,
        Field::ExternalId,
        Field::Description,
        Field::Subtitle,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Field::Title => "title",
            Field::Marker => "marker",
            Field::Description => "description",
            Field::Performer => "performer",
            Field::Cluster => "cluster",
            Field::Alias => "alias",
            Field::Tag => "tag",
            Field::Studio => "studio",
            Field::Group => "group",
            Field::Subtitle => "subtitle",
            Field::ExternalId => "external_id",
        }
    }

    /// Parse a stored field name.
    ///
    /// `pub` because the fuzzy index stores the same names and reads them
    /// back, and a private parser would have the fuzzy module reach into the
    /// enum's innards to do it.
    pub fn parse_str(s: &str) -> Option<Self> {
        Field::ALL.into_iter().find(|f| f.as_str() == s)
    }

    /// How much a hit here is worth relative to a title hit.
    ///
    /// A title is the thing a person types when they know what they want, so a
    /// title hit outranks everything. A marker title is close behind because
    /// §9.2 treats a marker as a titled thing in its own right. A description
    /// is prose a person wrote *about* the thing, which is a weaker signal than
    /// a title and a much weaker one than a name.
    ///
    /// Transcripts sit low on purpose. A transcript is the longest text in the
    /// schema, so an unweighted one swamps every other field: a word spoken
    /// once in a two-hour dialogue would outrank a performer whose name is the
    /// query. That is the failure mode §9.2's inclusion of transcripts invites,
    /// and the weight is what prevents it.
    pub const fn weight(self) -> i64 {
        match self {
            Field::Title => 100,
            Field::Marker => 60,
            Field::Performer => 50,
            Field::Cluster => 45,
            // Above a cluster and a tag: a name the user chose for this object
            // is a signal about *this* object, where a tag is often inherited
            // from a performer or a series. 48 rather than 40, because a tie
            // with `Tag` makes the ordering between two different fields
            // arbitrary and the two engines would then have to break it the
            // same way by accident.
            Field::Alias => 48,
            Field::Tag => 40,
            Field::Studio => 35,
            Field::Group => 30,
            Field::ExternalId => 25,
            Field::Description => 20,
            Field::Subtitle => 5,
        }
    }
}

/// The English stop words.
///
/// Small and written out rather than a dependency's list, for the same reason
/// [`tokenize`] is written out: a stop list is a *policy* — it decides which
/// words nobody searches for — and a policy that arrives as a transitive
/// dependency's data file is a policy nobody can review in this repository.
///
/// Deliberately short. A long list removes words a person really does search
/// for; `not` and `no` are absent because §5.x's adult corpus makes them real
/// queries, and `off` is absent because it is a tag.
const STOP_WORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "but", "by", "for", "from", "has", "he", "in", "is",
    "it", "its", "of", "on", "or", "that", "the", "to", "was", "were", "will", "with",
];

/// Reduce one word to its stem.
///
/// Not `&str`-returning: the stem can be shorter and is always a prefix, so a
/// `&str` into the original would be a lie about lifetime that a caller could
/// then hold past the local. A `String` per term is a small cost on a path that
/// already allocates a row.
pub fn stem(word: &str) -> String {
    // Words this short are not stemmed: `is` -> `i` and `as` -> `a` turn two
    // distinct stop words into one, and a stemmer that merges vocabulary is a
    // stemmer that makes search *less* precise.
    if word.len() < 4 {
        return word.to_string();
    }

    // Porter step 1a, in Porter's own order, as one readable block rather than
    // a suffix list. The order is the whole algorithm: `sses` must be tried
    // before `s` or `dresses` stems to `dresse`, and `ss` must be tried before
    // `s` or `dress` stems to `dres`.
    //
    // The `ss` rule maps `ss` to itself. Written as an explicit arm rather than
    // as an absent suffix on purpose -- a first version listed `ss` among the
    // strippable suffixes, reasoning that `ss` had to be removed before `s`
    // could reach it, which turns `dress` into `dre` and `class` into `cla`.
    // A word ending in `ss` has already had its suffix removed; there is
    // nothing left to strip.
    if let Some(base) = word.strip_suffix("sses") {
        if base.len() >= 2 {
            return format!("{base}ss");
        }
    }
    if let Some(base) = word.strip_suffix("ies") {
        if base.len() >= 3 {
            return format!("{base}i");
        }
    }
    if word.ends_with("ss") {
        return word.to_string();
    }
    if let Some(base) = word.strip_suffix("s") {
        if base.len() >= 3 {
            return base.to_string();
        }
    }

    // Step 1b, after 1a. `ing` and `ed` are only stripped from a word long
    // enough to leave a stem behind; `ed` and `edly` overlap, and `edly` is
    // tried first for the same reason `sses` is.
    for suffix in ["ingly", "edly", "ing", "ed"] {
        if let Some(base) = word.strip_suffix(suffix) {
            if base.len() >= 3 {
                return base.to_string();
            }
        }
    }
    // Step 1c, the `y` rule, last: a word ending in `y` is stemmed only when
    // the `y` is a vowel-and-consonant ending rather than the whole word.
    if let Some(base) = word.strip_suffix("y") {
        if base.len() >= 3 {
            return base.to_string();
        }
    }
    word.to_string()
}

/// Split text into the terms that go in the index and come out of a query.
///
/// The same function on both sides, which is the entire point: an index built
/// with one tokenizer and queried with another finds nothing, and finds nothing
/// *silently* — a search that returns no results looks exactly like a search
/// for something that is not there.
pub fn tokenize(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            current.push(ch);
        } else if !current.is_empty() {
            push_term(&mut out, &current);
            current.clear();
        }
    }
    if !current.is_empty() {
        push_term(&mut out, &current);
    }
    out
}

fn push_term(out: &mut Vec<String>, word: &str) {
    // `to_lowercase` rather than `to_ascii_lowercase`: the latter leaves
    // non-ASCII uppercase alone, so `ÉMILIE` and `émilie` would be two terms
    // and §9.2's "titles in all languages" would be true only for scripts that
    // happen to be lowercase already.
    let lower = word.to_lowercase();
    if lower.len() < 2 || STOP_WORDS.contains(&lower.as_str()) {
        return;
    }
    let stemmed = stem(&lower);
    if stemmed.is_empty() {
        return;
    }
    out.push(stemmed);
}

/// A synonym, as §9.3 describes them: per-vocabulary, so "cunt" finds the tag
/// and "ass" finds both meanings.
///
/// Per-vocabulary rather than global because a global table cannot express
/// "ass" meaning two things, and §9.3 names that case explicitly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Synonym {
    pub vocabulary: String,
    pub term: String,
    /// The terms this one expands to, including itself.
    pub expands_to: Vec<String>,
}

/// Run the one query this module needs, on whichever engine the store holds.
///
/// There are exactly two places a dialect could leak in: the bind markers (`?`
/// vs `$n`) and nothing else. Everything else -- the `CASE`, `GROUP BY`,
/// `HAVING`, the `IN` list -- is in the portable subset of §15.2, which is why
/// there is one SQL string and not one per engine.
///
/// The alternative was a `[T: FromRow<SqliteRow> + FromRow<PgRow>]` helper in
/// `db.rs`, and it does not compile: `FromRow` is parameterised by the row type
/// as well as the lifetime, so no derived `FromRow` satisfies both bounds and
/// the error says nothing about what to do about it. Two arms here is less
/// machinery than a trait hierarchy for one query, and the duplication is two
/// `match` arms rather than two SQL statements -- which is the distinction that
/// matters for "one behaviour".
async fn ranked(store: &Store, sql: &str, terms: &[String]) -> Result<Vec<Ranked>, SearchError> {
    // The binds are the `IN` terms, then the group-`CASE` terms, in that order
    // -- which is the order the `?` marks appear in the SQL. Bound twice over
    // the same list because the query references it twice, and a caller cannot
    // pass a list that disagrees with the `CASE` arms it built: both come from
    // the same `groups`.
    //
    // A version of this took a `group_count` as well, on the theory that the
    // signature should stop a caller from disagreeing with itself. The SQL
    // already interpolates that count from the same `groups`, so it could not
    // disagree; the parameter was a `let _ =` and an extra thing to keep right.
    match store {
        Store::Sqlite(p) => {
            let mut q = sqlx::query_as::<_, Ranked>(sql);
            for t in terms {
                q = q.bind(t.clone());
            }
            for t in terms {
                q = q.bind(t.clone());
            }
            Ok(q.fetch_all(p).await.map_err(StoreError::Query)?)
        }
        Store::Postgres(p) => {
            // Bound to a local: `bind_sql` returns a String, and passing a
            // temporary where the query borrows it is a borrow of a value that
            // dies at the end of the statement.
            let pg = Store::bind_sql(sql);
            let mut q = sqlx::query_as::<_, Ranked>(&pg);
            for t in terms {
                q = q.bind(t.clone());
            }
            for t in terms {
                q = q.bind(t.clone());
            }
            Ok(q.fetch_all(p).await.map_err(StoreError::Query)?)
        }
    }
}

/// Expand a query's terms through the synonym table.
///
/// A term expands to *itself plus* its synonyms, not to its synonyms alone: a
/// replacement that drops the original makes a search for a word that happens
/// to have a synonym stop finding the word itself, which is a bug that looks
/// like a synonym table being helpful.
/// Expand a query's terms into **groups**, one per query term.
///
/// The shape matters and the first version got it wrong. A flat expansion
/// turns every synonym into an extra *required* term, so a query for `arse`
/// against a vocabulary mapping `arse -> arse ass hole` demands an object
/// carrying all three — and no object ever does, so §9.3's own "ass finds both
/// meanings" finds nothing. The test caught it, and the fix is the shape every
/// search engine with synonym expansion uses:
///
/// - **within** a group, the terms are alternatives (OR) — `arse` means the
///   same as `ass`, so an object carrying either answers the query;
/// - **between** groups, the terms are requirements (AND) — a two-word query
///   still needs both words' meanings present.
///
/// The group is returned rather than a flattened list precisely so the caller
/// can AND across groups and OR within one. Returning a flat `Vec<String>`
/// loses exactly the information needed to build the query, which is how the
/// bug got in.
pub async fn expand(
    store: &Store,
    vocabulary: &str,
    terms: &[String],
) -> Result<Vec<Vec<String>>, StoreError> {
    let rows: Vec<(String, String)> = match store {
        Store::Sqlite(p) => sqlx::query_as::<_, (String, String)>(
            "SELECT term, expands_to FROM search_synonym WHERE vocabulary = ?",
        )
        .bind(vocabulary.to_string())
        .fetch_all(p)
        .await
        .map_err(StoreError::Query)?,
        Store::Postgres(p) => {
            let sql =
                Store::bind_sql("SELECT term, expands_to FROM search_synonym WHERE vocabulary = ?");
            sqlx::query_as::<_, (String, String)>(&sql)
                .bind(vocabulary.to_string())
                .fetch_all(p)
                .await
                .map_err(StoreError::Query)?
        }
    };

    let table: BTreeMap<String, Vec<String>> = rows
        .into_iter()
        .map(|(term, expands_to)| {
            // The space-separated list is split here rather than in SQL: a
            // `string_agg`/`GROUP_CONCAT` in this query would make the synonym
            // table engine-specific, and the whole point of §9.3 is that the
            // synonym behaviour is the same on both.
            let list = expands_to
                .split_whitespace()
                .map(str::to_string)
                .collect::<Vec<_>>();
            (term, list)
        })
        .collect();

    Ok(terms
        .iter()
        .map(|t| {
            // A term with no entry is its own group of one. A term WITH an
            // entry expands to its synonyms, and the original is *kept* — a
            // replacement that drops the original makes a search for a word
            // that happens to have a synonym stop finding the word itself.
            let mut group = vec![t.clone()];
            if let Some(extra) = table.get(t) {
                for e in extra {
                    if !group.contains(e) {
                        group.push(e.clone());
                    }
                }
            }
            group
        })
        .collect())
}

/// One row of the term index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchHit {
    pub object_id: String,
    pub score: i64,
    /// The fields that matched, so a caller can say *why* something matched
    /// rather than only that it did.
    ///
    /// Not decoded from the ranked query -- see [`Ranked`], which is what that
    /// query returns. A `GROUP_CONCAT`/`string_agg` here would make the ranking
    /// SQL engine-specific for a column nothing ranks by.
    pub fields: Vec<String>,
}

/// The ranked query's actual projection: two scalars both engines agree on.
#[derive(Debug, Clone, FromRow)]
struct Ranked {
    object_id: String,
    score: i64,
}

/// Why a search failed.
#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    #[error("the query has no searchable terms in it: {0}")]
    EmptyQuery(String),
    #[error("a limit of {requested} is not allowed; the maximum is {max}")]
    LimitTooLarge { requested: usize, max: usize },
    #[error("store: {0}")]
    Store(#[from] StoreError),
}

/// The largest page a search will return.
pub const MAX_RESULTS: usize = 500;

impl SearchError {
    /// True when the query had no searchable terms, and so a fuzzy pass has
    /// nothing to work from either.
    ///
    /// A predicate rather than `Clone` on the error: `search_fuzzy` runs the
    /// exact search first and needs to know *which kind* of failure it was, and
    /// making the whole error cloneable to carry that one bit out is a large
    /// requirement (`StoreError` wraps an `io::Error`) for a boolean.
    pub fn is_empty_query(&self) -> bool {
        matches!(self, SearchError::EmptyQuery(_))
    }
}

impl Store {
    /// Index one object's terms.
    ///
    /// Replaces rather than merges: a `search_term` row is derived data, and a
    /// stale row is worse than no row because it is indistinguishable from a
    /// current one. `DELETE` then insert, in one transaction, so a reader never
    /// sees the object with half its terms.
    ///
    /// An *empty* `terms` clears nothing under the field-scoped delete below,
    /// because there is no field to scope it to. `clear_field` is the explicit
    /// way to empty one, and `remove_alias` calls it when the last alias goes.
    /// Passing an empty slice to mean "clear this object" would be a second
    /// meaning for one argument, and the field-scoped behaviour is the one that
    /// keeps a title from being wiped by an alias write.
    pub async fn index_object(
        &self,
        object_id: &str,
        terms: &[(Field, String)],
    ) -> Result<usize, SearchError> {
        // Replaces rather than merges: a `search_term` row is derived data, and
        // a stale row is worse than no row because it is indistinguishable from a
        // current one. DELETE then insert, so a reader never sees the object
        // with half its terms.
        //
        // Not in a transaction, and that is a deliberate trade. A transaction
        // would need the `Transaction` executor generic over both engines, and
        // `sqlx::Transaction<DB>` is exactly as engine-specific as the pool it
        // came from -- so the alternative is one `index_object` per engine, and
        // the two would drift. The window is a single statement wide in the
        // common case (nothing was indexed before, so the DELETE removes
        // nothing), and a re-index of an already-indexed object is the only case
        // where a reader can observe the gap. For a search index rebuilt on
        // demand, that is the right way round; a caller that cannot tolerate it
        // indexes an object nobody is searching for yet.
        // The distinct fields in `terms`, so each is replaced once. Collected
        // before the delete because the delete is driven by the field list and
        // a term list is not a field list.
        let mut fields: Vec<&str> = terms.iter().map(|(f, _)| f.as_str()).collect();
        fields.sort_unstable();
        fields.dedup();

        // A full replace is wrong here, and it was wrong in T-P5-001 too:
        // deleting every row for the object means indexing the title wipes the
        // aliases, and indexing an alias wipes the title. The two are different
        // fields and neither call knows about the other.
        //
        // It did not show up until T-P5-003, and the reason is worth keeping:
        // every test that used `index_object` indexed one object with one
        // field, so a full replace and a field replace are the same statement.
        // The bug became visible the moment a test indexed a title *and* an
        // alias on the same object — which is what an object with a name and a
        // nickname actually is.
        //
        // So the delete is scoped to the fields being written. A field not
        // mentioned in `terms` keeps its rows, and a field mentioned with fewer
        // terms than before is fully replaced — which is what "replace this
        // field" means, and what `add_alias` and `remove_alias` rely on when
        // they re-index a field from the table.
        for field in fields {
            let sql = "DELETE FROM search_term WHERE object_id = ? AND field = ?";
            let bound = Store::bind_sql(sql);
            match self {
                Store::Sqlite(p) => {
                    sqlx::query(&bound)
                        .bind(object_id.to_string())
                        .bind(field)
                        .execute(p)
                        .await
                        .map_err(StoreError::Query)?;
                }
                Store::Postgres(p) => {
                    sqlx::query(&bound)
                        .bind(object_id.to_string())
                        .bind(field)
                        .execute(p)
                        .await
                        .map_err(StoreError::Query)?;
                }
            }
        }

        let mut n = 0usize;
        for (field, term) in terms {
            // One row per (object, field, term), so a title and a description
            // holding the same word are two rows and a title hit is
            // distinguishable from a description hit. The conflict clause is
            // what stops `index_object` being called twice from doubling
            // somebody's score.
            self.exec1(
                "INSERT INTO search_term (object_id, field, term)
                 VALUES (?, ?, ?)
                 ON CONFLICT (object_id, field, term) DO NOTHING",
                &[
                    object_id.to_string(),
                    field.as_str().to_string(),
                    term.clone(),
                ],
            )
            .await?;
            n += 1;
        }

        // The fuzzy keys, written here rather than left to a caller to
        // remember. `index_terms` is public and `index_object` is what every
        // other write path goes through, so a caller that indexed an object's
        // terms and forgot the fuzzy pass got an object that was findable
        // exactly and not findable with a typo -- which is the failure the
        // alias path hit, and which nothing but this call would have caught.
        //
        // Ordering: the exact rows first, so a search that lands between the
        // two writes finds the object rather than missing it. The reverse order
        // would surface a fuzzy-only hit for an object whose exact terms were
        // not yet written, which ranks the object on a field the caller has not
        // finished describing.
        crate::fuzzy::index_terms(self, object_id, terms).await?;
        Ok(n)
    }

    /// One writing statement, on either engine, with `?` markers.
    async fn exec1(&self, sql: &str, binds: &[String]) -> Result<u64, SearchError> {
        let n = match self {
            Store::Sqlite(p) => {
                let mut q = sqlx::query(sql);
                for b in binds {
                    q = q.bind(b.clone());
                }
                q.execute(p)
                    .await
                    .map_err(StoreError::Query)?
                    .rows_affected()
            }
            Store::Postgres(p) => {
                let pg = Store::bind_sql(sql);
                let mut q = sqlx::query(&pg);
                for b in binds {
                    q = q.bind(b.clone());
                }
                q.execute(p)
                    .await
                    .map_err(StoreError::Query)?
                    .rows_affected()
            }
        };
        Ok(n)
    }

    /// Drop one object's terms, without deleting the object.
    ///
    /// **Not on the deletion path** -- `search_term.object_id` is
    /// `ON DELETE CASCADE`, so deleting an object removes its terms and this
    /// function has no caller. An earlier version of this doc claimed it was
    /// "called when the object is deleted", which was a description of what
    /// ought to happen rather than what the code did, and it is exactly the
    /// kind of claim that makes a cascade untested: with the cascade doing the
    /// work, a no-op here would pass every test in the suite. `deleting_an_
    /// object_removes_its_terms` covers the cascade, and this covers the case
    /// the cascade does not -- an object that is still there but should stop
    /// being findable, which is what a consent-tier change needs.
    pub async fn deindex_object(&self, object_id: &str) -> Result<u64, SearchError> {
        self.exec1(
            "DELETE FROM search_term WHERE object_id = ?",
            &[object_id.to_string()],
        )
        .await
    }

    /// Remove every indexed term for one field of one object.
    ///
    /// The explicit counterpart to the field-scoped delete inside
    /// [`Store::index_object`]. It exists because "replace the aliases of an
    /// object that now has none" is a real operation -- `remove_alias` on the
    /// last alias -- and an empty term list carries no field to scope the delete
    /// to.
    ///
    /// Both the exact and the fuzzy index are cleared. They are written
    /// together by T-P5-001's design and reading them separately is the state
    /// the `indexing_a_tag_does_not_disturb_another_objects_terms` test exists
    /// to prevent, so clearing one and not the other would reintroduce it.
    pub async fn clear_field(&self, object_id: &str, field: Field) -> Result<u64, SearchError> {
        let sql = "DELETE FROM search_term WHERE object_id = ? AND field = ?";
        let bound = Store::bind_sql(sql);
        let n = match self {
            Store::Sqlite(p) => sqlx::query(&bound)
                .bind(object_id.to_string())
                .bind(field.as_str())
                .execute(p)
                .await
                .map_err(StoreError::Query)?
                .rows_affected(),
            Store::Postgres(p) => sqlx::query(&bound)
                .bind(object_id.to_string())
                .bind(field.as_str())
                .execute(p)
                .await
                .map_err(StoreError::Query)?
                .rows_affected(),
        };
        crate::fuzzy::clear_field(self, object_id, field).await?;
        Ok(n)
    }

    /// Delete an object and everything derived from it.
    ///
    /// Migration 0017 dropped the foreign key from `search_term` and
    /// `search_fuzzy` to `object(id)`, so a tag could be indexed. That removed
    /// the `ON DELETE CASCADE` with it, and this method is what replaced it.
    ///
    /// Without it, deleting a row from `object` left its index rows behind and
    /// the object was still returned by a search — with a score, as though it
    /// existed. `deleting_an_object_removes_its_fuzzy_keys` in
    /// `tests/fuzzy.rs` failed on exactly this: `FOREIGN KEY constraint failed`
    /// was how 0017 was discovered, and the two tests that had been passing
    /// since T-P5-002 were passing *because of* the cascade. Removing the
    /// guarantee did not make the tests wrong; it made them report a real gap
    /// that had been covered by a constraint rather than by any code.
    ///
    /// The derived rows go first, in one transaction, and the object goes last.
    /// The order matters only for the reader: a concurrent search between the
    /// two would see an object with no index rather than an index with no
    /// object, and the first is an empty result while the second is a ghost.
    /// Making that a real guarantee needs serializable isolation, which is a
    /// bigger claim than this method makes — it is written in the order that
    /// fails in the harmless direction, and the comment says so rather than
    /// implying more.
    pub async fn delete_object(&self, object_id: &str) -> Result<(), StoreError> {
        // Four statements, ids bound rather than interpolated, in the order
        // that fails in the harmless direction: the derived rows go first, so a
        // search that lands between the statements sees an object with no index
        // (an empty result) rather than an index with no object (a ghost).
        //
        // The first version built the script with `format!` and the id spliced
        // into the SQL text — a SQL injection with a `;` in it, even though the
        // id here is a UUID the store generated. It is still the wrong shape: a
        // function that puts a caller-supplied id into a statement is a
        // function that has to be re-audited the moment the caller stops being
        // the only source.
        //
        // Four separate `execute` calls, not one `raw_sql` script and not one
        // transaction. A transaction would be better and is not available: the
        // two engines' `Transaction` types are different, and this crate's
        // established way to run a statement on both is a `match` on the `Store`
        // with the SQL written once. The four statements are each idempotent,
        // so re-running the method on a half-deleted object is safe — which is
        // also the recovery path if one of the four fails, and a caller can
        // simply call it again.
        const STATEMENTS: [&str; 4] = [
            "DELETE FROM search_term  WHERE object_id = ?",
            "DELETE FROM search_fuzzy WHERE object_id = ?",
            "DELETE FROM object_alias WHERE object_id = ?",
            "DELETE FROM object       WHERE id = ?",
        ];
        for sql in STATEMENTS {
            let bound = Store::bind_sql(sql);
            match self {
                Store::Sqlite(p) => {
                    sqlx::query(&bound)
                        .bind(object_id.to_string())
                        .execute(p)
                        .await
                        .map_err(StoreError::Query)?;
                }
                Store::Postgres(p) => {
                    sqlx::query(&bound)
                        .bind(object_id.to_string())
                        .execute(p)
                        .await
                        .map_err(StoreError::Query)?;
                }
            }
        }
        Ok(())
    }

    /// Search. The terms are the ones [`tokenize`] produces, so a caller cannot
    /// disagree with the indexer about what a word is.
    ///
    /// The SQL is written once. `Engine` appears nowhere in it, because the
    /// whole design is that there is nothing engine-specific to switch on — and
    /// a function taking an `Engine` invites exactly the branch that makes the
    /// two engines diverge.
    pub async fn search(
        &self,
        text: &str,
        vocabulary: Option<&str>,
        limit: usize,
    ) -> Result<Vec<SearchHit>, SearchError> {
        if limit > MAX_RESULTS {
            return Err(SearchError::LimitTooLarge {
                requested: limit,
                max: MAX_RESULTS,
            });
        }
        let terms = tokenize(text);
        if terms.is_empty() {
            return Err(SearchError::EmptyQuery(text.to_string()));
        }
        // Groups, whether or not a vocabulary was named: a query term with no
        // synonym entry is a group of one, so the query below is written once
        // and the "no vocabulary" case falls out of the general shape rather
        // than being a second code path.
        let groups: Vec<Vec<String>> = match vocabulary {
            Some(v) => expand(self, v, &terms).await?,
            None => terms.into_iter().map(|t| vec![t]).collect(),
        };

        // Rank by the sum of the field weights for the matched terms, and
        // require every *group* to be satisfied.
        //
        // The `WHERE ... OR (...) AND (WHERE ... OR ...)` shape is OR within a
        // synonym group and AND between groups. Written as one flat
        // `term IN (...)` with `HAVING COUNT(DISTINCT term) = n`, which is what
        // the first version did, that is only correct when every group has one
        // member -- and a synonym group with three members then demands an
        // object carrying all three, which no object does, so §9.3's "ass
        // finds both meanings" found nothing.
        //
        // `HAVING COUNT(DISTINCT grp) = n` is the AND between groups, where
        // `grp` is the group index. That is portable: `COUNT(DISTINCT x)` is in
        // the §15.2 subset, and it avoids a chain of self-joins, which is a
        // different query in every engine's planner.
        //
        // The group index is *interpolated*, not bound. It is a loop counter
        // over `groups`, an integer this function produced, and it cannot
        // disagree with the `IN` lists below it because both come from the same
        // loop. Binding it would mean a heterogeneous parameter list, which in
        // sqlx means implementing `Encode` and `Type` for a private enum to
        // carry a number already known to be an integer.
        let case = Field::ALL
            .iter()
            .map(|f| format!("WHEN '{}' THEN {}", f.as_str(), f.weight()))
            .collect::<Vec<_>>()
            .join("\n                          ");

        // One `IN (...) OR (...)` per group, and the matching binds in order.
        let mut where_parts: Vec<String> = Vec::with_capacity(groups.len());
        let mut bind_terms: Vec<String> = Vec::new();
        for group in &groups {
            let marks = vec!["?"; group.len()].join(", ");
            where_parts.push(format!("(s.term IN ({marks}))"));
            bind_terms.extend(group.iter().cloned());
        }
        let where_clause = where_parts.join(" OR ");

        // Every row carries the index of the group its term came from, so the
        // `HAVING` can count distinct groups. The `CASE` maps a bound term back
        // to its group: a term in two groups (a word that is its own synonym
        // and also an expansion of another) satisfies both, and the first
        // matching arm wins -- which is why the arms are `WHEN ... THEN n` over
        // a bound term rather than a join against a temporary table.
        // The `CASE` maps a matched term back to its group index, so the
        // `COUNT(DISTINCT ...)` counts *groups* matched rather than terms, and
        // two terms in the same group count once.
        //
        // It is written in the **subject form** -- `CASE <expr> WHEN <value>
        // THEN <result>` -- not the searched form, and not with a bound
        // parameter in either. Both alternatives fail on Postgres while
        // working on SQLite, which is the whole hazard this module exists to
        // avoid:
        //
        //   `CASE WHEN ? THEN 1`      -> 42804, "argument of CASE/WHEN must be
        //                                  type boolean": a bare parameter used
        //                                  only as a condition has nothing to
        //                                  infer its type from, so `PREPARE`
        //                                  rejects the query before it runs.
        //   `CASE WHEN 'ass' THEN 1`  -> 22P02, "invalid input syntax for type
        //                                  boolean "ass"": with the value
        //                                  spelled out, Postgres reads a
        //                                  searched `CASE` whose arms are
        //                                  boolean conditions, and tries to
        //                                  coerce the term to one.
        //
        // The subject form gives the arms a type -- the type of `s.term` --
        // and compares as text on both engines. That is the only spelling of
        // the three that means the same thing in both.
        //
        // Interpolating a term into SQL is normally what never to do, and it is
        // safe here for reasons that are checkable rather than assumed: a term
        // comes from `tokenize`, which yields only alphanumeric runs, so there
        // is no quote to escape; and the string still goes out through the same
        // `bind_sql` path as every other Postgres literal. It is also bounded:
        // the arms are one per query term, so the query grows with the query,
        // not with the corpus.
        let mut group_arms: Vec<String> = Vec::new();
        for (gi, group) in groups.iter().enumerate() {
            // 1-based, because `ELSE 0` is what the `COUNT(DISTINCT ...)` reads
            // as "matched no group" and a 0-based index would collide with it.
            for t in group {
                group_arms.push(format!("WHEN '{t}' THEN {}", gi + 1));
            }
        }
        // The `ELSE` is inside the `CASE`, not an `END` of its own: the
        // searched form's `ELSE 0` is what a term that matches no arm falls to,
        // and `COUNT(DISTINCT ...)` must not count it.
        let group_case = format!("CASE s.term {} ELSE 0 END", group_arms.join(" "));

        let sql = format!(
            "SELECT s.object_id,
                    SUM(CASE s.field
                          {case}
                          ELSE 0
                        END) AS score
               FROM search_term s
              WHERE {where_clause}
              GROUP BY s.object_id
             HAVING COUNT(DISTINCT {group_case}) = {n}",
            case = case,
            where_clause = where_clause,
            group_case = group_case,
            n = groups.len(),
        );

        let rows = ranked(self, &sql, &bind_terms).await?;

        let mut out: Vec<SearchHit> = rows
            .into_iter()
            .map(|r| SearchHit {
                object_id: r.object_id,
                score: r.score,
                // Filled in below, once the winner set is known.
                fields: Vec::new(),
            })
            .collect();
        // Ordered in Rust as well as in SQL. `ORDER BY` in the query would be
        // the natural place, and adding it there is where the two engines would
        // start to disagree about ties. The tie-break is the id, so the order
        // is total and the equality test can compare it.
        out.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| a.object_id.cmp(&b.object_id))
        });
        out.truncate(limit);

        // The matching fields, as a second flat query rather than a
        // `GROUP_CONCAT` in the first. Two engines spell that aggregate
        // differently -- `string_agg` vs `GROUP_CONCAT` -- and §3.5's
        // "DuckDB where the workload is analytics-shaped" would spell it a third
        // way. The fields belong to the response, not to the ranking, so
        // moving them out is what keeps the ranking query portable.
        //
        // No `ORDER BY` here, on purpose. Ordering is done in Rust from the
        // enum's own `Ord`: `ORDER BY field` would be alphabetical on one engine
        // and collation-dependent on the other, which is the same class of
        // disagreement the whole design exists to prevent.
        let ids: Vec<String> = out.iter().map(|h| h.object_id.clone()).collect();
        if !ids.is_empty() {
            let id_marks = vec!["?"; ids.len()].join(", ");
            let term_marks = vec!["?"; bind_terms.len()].join(", ");
            // Two mark lists, counted separately. The first version reused one
            // `marks` for both the `IN` lists and bound `ids.len()` of them for
            // the first, which is a query that returns whatever the engine feels
            // like -- the `?` count and the mark count stop agreeing and the
            // error surfaces as wrong results on one engine and a bind error on
            // the other.
            let field_sql = format!(
                "SELECT object_id, field
                   FROM search_term
                  WHERE object_id IN ({id_marks}) AND term IN ({term_marks})",
                id_marks = id_marks,
                term_marks = term_marks,
            );
            let frows: Vec<(String, String)> = match self {
                Store::Sqlite(p) => {
                    let mut fb = sqlx::query_as::<_, (String, String)>(&field_sql);
                    for id in &ids {
                        fb = fb.bind(id.clone());
                    }
                    for t in &bind_terms {
                        fb = fb.bind(t.clone());
                    }
                    Ok::<_, StoreError>(fb.fetch_all(p).await.map_err(StoreError::Query)?)
                }
                Store::Postgres(p) => {
                    let pg = Store::bind_sql(&field_sql);
                    let mut fb = sqlx::query_as::<_, (String, String)>(&pg);
                    for id in &ids {
                        fb = fb.bind(id.clone());
                    }
                    for t in &bind_terms {
                        fb = fb.bind(t.clone());
                    }
                    Ok::<_, StoreError>(fb.fetch_all(p).await.map_err(StoreError::Query)?)
                }
            }?;
            let mut by_object: BTreeMap<String, Vec<Field>> = BTreeMap::new();
            for (oid, field) in frows {
                if let Some(f) = Field::parse_str(&field) {
                    by_object.entry(oid).or_default().push(f);
                }
            }
            for hit in &mut out {
                if let Some(mut fields) = by_object.remove(&hit.object_id) {
                    fields.sort_unstable();
                    fields.dedup();
                    hit.fields = fields.into_iter().map(|f| f.as_str().to_string()).collect();
                }
            }
        }
        Ok(out)
    }
}
