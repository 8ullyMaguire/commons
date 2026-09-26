//! T-P5-001 — §9.2's breadth, §9.3's "one behaviour across both engines".
//!
//! The accept criterion for this ticket is a **shared fixture corpus with known
//! expected hits, run against both engines, asserting identical result id
//! sets**. That equality is the test; the rest of this file is the fixtures it
//! needs.
//!
//! # Why the test can be real here
//!
//! An equality test that needs a Postgres nobody has is a comment about
//! equality. The local server is reachable, so the parity tests run against
//! both. When `DATABASE_URL` is unset they **fail loudly** rather than skip —
//! §3.5 makes two engines a property of the product, not a deployment choice,
//! and a silently-skipped parity test is how a divergence ships.
//!
//! Each engine gets a **fresh schema** rather than sharing one: the corpus is
//! written twice, and a shared table would make the second run's results depend
//! on the first's. `search_schema` isolates them, so a test that inserts an
//! object cannot see another test's.
//!
//! # What the fixtures are for
//!
//! [`known_hits`] is the expected result for each query, written by hand from
//! §9.2's list — not recorded from a run. A corpus whose expectations were
//! recorded from the implementation proves only that the implementation is
//! consistent with itself, which is the failure mode this whole ticket exists
//! to catch.

#[path = "harness/mod.rs"]
mod harness;
use harness::{postgres_store, sqlite_store};

use commons_core::ts::now;
use commons_store::db::Store;
use commons_store::search::{self, Field, SearchError};
use serde_json::json;

// ------------------------------------------------------------------ fixtures

/// One object in the corpus, with the fields §9.2 names.
struct Fixture {
    id: &'static str,
    title: &'static str,
    description: &'static str,
    tags: &'static [&'static str],
    performers: &'static [&'static str],
    transcript: &'static str,
}

/// A corpus chosen so that every §9.2 field is represented, and so that the
/// ranking is *checkable* rather than merely present.
///
/// The two title cases are the interesting ones. `o-alpine` has the word in its
/// title; `o-valley` has it only in a transcript. A search that ignored the
/// field weighting would return them in whatever order the database liked, so
/// their order is the assertion that the weighting is real.
const CORPUS: &[Fixture] = &[
    Fixture {
        id: "o-alpine",
        title: "Alpine Ascent",
        description: "a climb",
        tags: &["mountain"],
        performers: &["R. Alpine"],
        transcript: "we leave at dawn",
    },
    Fixture {
        id: "o-valley",
        title: "Valley Drive",
        description: "a road",
        tags: &["car"],
        performers: &["S. Valley"],
        transcript: "the ascent begins",
    },
    Fixture {
        id: "o-quiet",
        title: "Quiet Morning",
        description: "nothing happens",
        tags: &["calm"],
        performers: &[],
        transcript: "silence",
    },
];

/// Write the corpus into a store and index every object.
///
/// Everything is tokenized by [`search::tokenize`] — the same function a query
/// goes through — so the index cannot be built by a different rule than the one
/// it is searched with.
async fn load(store: &Store) {
    let ts = now();
    for f in CORPUS {
        insert_object(store, f.id, f.title, &ts).await;
        let mut terms: Vec<(Field, String)> = Vec::new();
        push_terms(&mut terms, Field::Title, f.title);
        push_terms(&mut terms, Field::Description, f.description);
        push_terms(&mut terms, Field::Subtitle, f.transcript);
        for t in f.tags {
            push_terms(&mut terms, Field::Tag, t);
        }
        for p in f.performers {
            push_terms(&mut terms, Field::Performer, p);
        }
        store
            .index_object(f.id, &terms)
            .await
            .unwrap_or_else(|e| panic!("index {}: {e}", f.id));
    }
}

fn push_terms(out: &mut Vec<(Field, String)>, field: Field, text: &str) {
    for t in search::tokenize(text) {
        out.push((field, t));
    }
}

async fn insert_object(store: &Store, id: &str, title: &str, ts: &str) {
    let n = exec(
        store,
        "INSERT INTO object (id, kind, title, created_at, updated_at)
                       VALUES (?, 'scene', ?, ?, ?)",
        &[id, title, ts, ts],
    )
    .await;
    assert_eq!(n, 1, "fixture {id} was not inserted");
}

/// A portable `INSERT`, because `pool()` is SQLite-only and half this file runs
/// on Postgres.
async fn exec(store: &Store, sql: &str, binds: &[&str]) -> u64 {
    match store {
        Store::Sqlite(p) => {
            let mut q = sqlx::query(sql);
            for b in binds {
                q = q.bind(b.to_string());
            }
            q.execute(p).await.unwrap().rows_affected()
        }
        Store::Postgres(p) => {
            let pg = Store::bind_sql(sql);
            let mut q = sqlx::query(&pg);
            for b in binds {
                q = q.bind(b.to_string());
            }
            q.execute(p).await.unwrap().rows_affected()
        }
    }
}

// --------------------------------------------------------- expected results

/// The expected result for each query, by hand, from §9.2's field list.
///
/// Not recorded from a run. These are the claims the ticket makes, and a test
/// whose expectations come from the implementation cannot fail in the way that
/// matters.
fn known_hits() -> Vec<(&'static str, Vec<&'static str>)> {
    vec![
        // "ascent" appears in o-alpine's *title* and o-valley's *transcript*.
        // The title must win: §9.2 puts titles first and the weighting table
        // says a title is worth 100 against a transcript's 5.
        ("ascent", vec!["o-alpine", "o-valley"]),
        // "valley" is in o-valley's title and in S. Valley's name.
        ("valley", vec!["o-valley"]),
        // A tag, and §9.2's "search includes performers and tags in keyword
        // search" (#2976) -- without the tag row this returns nothing at all.
        ("mountain", vec!["o-alpine"]),
        // A performer name.
        ("alpine", vec!["o-alpine"]),
        // A word in one object only.
        ("silence", vec!["o-quiet"]),
        // Two words, both required. An OR-only search would return both o-alpine
        // and o-valley for "alpine dawn", and only o-alpine has both.
        ("alpine dawn", vec!["o-alpine"]),
        // Nothing matches: an AND query with a term no object holds is empty,
        // not "everything".
        ("alpine nonexistentword", vec![]),
    ]
}

// ----------------------------------------------------------------- the tests

/// §9.3's requirement, stated as the ticket states it: the same corpus, the
/// same queries, the same results, on both engines.
#[tokio::test]
async fn both_engines_return_identical_results() {
    let lite = sqlite_store().await;
    let pg = postgres_store().await;
    load(&lite).await;
    load(&pg).await;

    for (query, expected) in known_hits() {
        let l = lite
            .search(query, None, 10)
            .await
            .unwrap_or_else(|e| panic!("sqlite search {query:?}: {e}"));
        let p = pg
            .search(query, None, 10)
            .await
            .unwrap_or_else(|e| panic!("postgres search {query:?}: {e}"));

        let l_ids: Vec<&str> = l.iter().map(|h| h.object_id.as_str()).collect();
        let p_ids: Vec<&str> = p.iter().map(|h| h.object_id.as_str()).collect();

        assert_eq!(
            l_ids, p_ids,
            "§9.3: one behaviour across both engines. \
             query {query:?} returned different id sets:\n  \
             sqlite:   {l_ids:?}\n  postgres: {p_ids:?}"
        );
        assert_eq!(
            l_ids, expected,
            "query {query:?}: expected {expected:?} by hand from §9.2's field list, \
             sqlite returned {l_ids:?}"
        );
    }
}

/// The scores have to agree too, not just the ids.
///
/// Two engines returning the same *set* in the same order by different scores
/// is a search whose ranking depends on which database it runs in — and it is
/// exactly what an id-set-only equality test cannot see, which is why this
/// test exists next to that one.
#[tokio::test]
async fn both_engines_return_identical_scores() {
    let lite = sqlite_store().await;
    let pg = postgres_store().await;
    load(&lite).await;
    load(&pg).await;

    for (query, _) in known_hits() {
        let l = lite.search(query, None, 10).await.unwrap();
        let p = pg.search(query, None, 10).await.unwrap();
        let l_pairs: Vec<(&str, i64)> = l.iter().map(|h| (h.object_id.as_str(), h.score)).collect();
        let p_pairs: Vec<(&str, i64)> = p.iter().map(|h| (h.object_id.as_str(), h.score)).collect();
        assert_eq!(
            l_pairs, p_pairs,
            "§9.3: the same ranking, not just the same hits. query {query:?}"
        );
    }
}

/// The matching fields, so a caller can say *why* something matched.
#[tokio::test]
async fn a_hit_reports_which_fields_matched() {
    let store = sqlite_store().await;
    load(&store).await;

    let hits = store.search("ascent", None, 10).await.unwrap();
    let alpine = hits.iter().find(|h| h.object_id == "o-alpine").unwrap();
    assert_eq!(
        alpine.fields,
        vec!["title".to_string()],
        "a title hit should say title, not a list containing everything"
    );

    let valley = hits.iter().find(|h| h.object_id == "o-valley").unwrap();
    assert_eq!(
        valley.fields,
        vec!["subtitle".to_string()],
        "the transcript hit should report the transcript"
    );
}

/// The weighting table, asserted as a table.
///
/// A test that only checked "a title outranks a transcript" would pass if the
/// weights were 2 and 1. Pinning the numbers means a change to what a field is
/// worth is a *deliberate* edit to this test rather than a silent reordering
/// somebody notices in a screenshot.
#[test]
fn field_weights_are_the_documented_table() {
    let documented: Vec<(&str, i64)> = vec![
        ("title", 100),
        ("marker", 60),
        ("performer", 50),
        ("cluster", 45),
        ("alias", 48),
        ("tag", 40),
        ("studio", 35),
        ("group", 30),
        ("external_id", 25),
        ("description", 20),
        ("subtitle", 5),
    ];
    let actual: Vec<(&str, i64)> = Field::ALL
        .iter()
        .map(|f| (f.as_str(), f.weight()))
        .collect();
    assert_eq!(actual, documented, "the weighting table changed");
}

// -------------------------------------------------------------- the tokenizer

/// The tokenizer, as §9.3's "same tokenizer" depends on it being written down
/// rather than inherited from a dependency.
#[test]
fn the_tokenizer_is_the_documented_one() {
    // §9.2 says "titles in all languages", so a non-ASCII title has to survive.
    // `to_ascii_lowercase` would leave `ÉMILIE` alone and produce two terms.
    assert_eq!(
        search::tokenize("ÉMILIE"),
        vec!["émilie".to_string()],
        "a title in a non-ASCII script must tokenize by its own letters"
    );

    // Stop words go; content words stay.
    assert_eq!(
        search::tokenize("the quick brown fox"),
        vec!["quick".to_string(), "brown".to_string(), "fox".to_string()]
    );

    // Stemming: the `ing` suffix, and `running` finding `run`.
    assert_eq!(search::tokenize("running"), vec!["runn".to_string()]);
    assert_eq!(
        search::tokenize("runs"),
        vec!["run".to_string()],
        "'runs' should reach the same stem as 'run'"
    );

    // Punctuation is a separator, not a deletion: `foo-bar` is two terms, and
    // the hyphen is what §9.2's "titles in all languages" runs into.
    assert_eq!(
        search::tokenize("foo-bar"),
        vec!["foo".to_string(), "bar".to_string()]
    );

    // An empty query produces no terms, and `search` turns that into an error
    // rather than matching everything.
    assert!(search::tokenize("   ").is_empty());
    assert!(search::tokenize("the a of").is_empty());
}

/// The stemmer's edge cases, which are where a naive one quietly loses words.
#[test]
fn the_stemmer_keeps_short_and_doubled_words() {
    // Too short to stem: `is` -> `i` would merge two stop words.
    assert_eq!(search::stem("is"), "is");
    assert_eq!(search::stem("as"), "as");
    // `ss` must not be stripped to nothing.
    assert_eq!(search::stem("dress"), "dress");
    assert_eq!(search::stem("class"), "class");
    // The suffix set is ordered longest-first, or `dresses` -> `dresse`.
    assert_eq!(search::stem("dresses"), "dress");
    // `ies` before `s`.
    assert_eq!(search::stem("ponies"), "poni");
}

// ------------------------------------------------------------------ synonyms

/// §9.3: "per-vocabulary synonyms (so 'cunt' finds the tag, 'ass' finds both
/// meanings)".
#[tokio::test]
async fn synonyms_are_per_vocabulary() {
    let store = sqlite_store().await;
    let ts = now();
    for (vocab, term, expands) in [
        ("tags", "cunt", "cunt pussy"),
        ("anatomy", "ass", "ass arse"),
    ] {
        exec(
            &store,
            "INSERT INTO search_synonym (vocabulary, term, expands_to) VALUES (?, ?, ?)",
            &[vocab, term, expands],
        )
        .await;
    }
    // A vocabulary with no entry for a term leaves it alone.
    exec(
        &store,
        "INSERT INTO object (id, kind, title, created_at, updated_at)
                  VALUES ('o-syn', 'scene', 'syn', ?, ?)",
        &[&ts, &ts],
    )
    .await;

    // Within its vocabulary, a synonym expands. The result is *groups* -- one
    // per query term, each holding that term's alternatives -- because a
    // synonym means the same thing as its neighbours, not "and also". A flat
    // list is only correct when every group has one member.
    let groups = search::expand(&store, "tags", &["cunt".to_string()])
        .await
        .unwrap();
    assert_eq!(groups.len(), 1, "one query term, one group: {groups:?}");
    assert!(
        groups[0].contains(&"pussy".to_string()),
        "§9.3's per-vocabulary expansion: {groups:?}"
    );
    assert!(
        groups[0].contains(&"cunt".to_string()),
        "a term expands to itself *plus* its synonyms, or a word with a \
         synonym stops finding itself: {groups:?}"
    );

    // And the wrong vocabulary does not expand it: a term with no entry there
    // is a group of one.
    let wrong = search::expand(&store, "anatomy", &["cunt".to_string()])
        .await
        .unwrap();
    assert_eq!(
        wrong,
        vec![vec!["cunt".to_string()]],
        "a term with no entry in the named vocabulary is unchanged"
    );
}

/// Two query terms are two groups, and the two are AND'd together.
///
/// The shape that makes a multi-word query mean what it says: each word's
/// meanings are alternatives, and the words themselves are both required.
#[tokio::test]
async fn two_query_terms_make_two_groups() {
    let store = sqlite_store().await;
    let groups = search::expand(
        &store,
        "anatomy",
        &["arse".to_string(), "mountain".to_string()],
    )
    .await
    .unwrap();
    assert_eq!(groups.len(), 2, "one group per query term: {groups:?}");
    assert_eq!(
        groups[1],
        vec!["mountain".to_string()],
        "no entry, so a group of one"
    );
}

/// A synonym that maps two ways — §9.3's "ass" case.
#[tokio::test]
async fn a_term_can_expand_to_several_meanings() {
    let store = sqlite_store().await;
    // One row, two expansions, both meanings.
    exec(
        &store,
        "INSERT INTO search_synonym (vocabulary, term, expands_to) VALUES ('anatomy', 'ass', 'ass arse hole')",
        &[],
    )
    .await;
    let groups = search::expand(&store, "anatomy", &["ass".to_string()])
        .await
        .unwrap();
    assert_eq!(
        groups,
        vec![vec![
            "ass".to_string(),
            "arse".to_string(),
            "hole".to_string(),
        ]],
        "§9.3: 'ass' finds both meanings -- one group, three alternatives"
    );
}

// --------------------------------------------------------------------- index
/// Re-indexing replaces rather than accumulating.
#[tokio::test]
async fn reindexing_replaces_rather_than_doubling() {
    let store = sqlite_store().await;
    let ts = now();
    insert_object(&store, "o-r", "First Title", &ts).await;

    store
        .index_object("o-r", &[(Field::Title, "first".to_string())])
        .await
        .unwrap();
    store
        .index_object("o-r", &[(Field::Title, "first".to_string())])
        .await
        .unwrap();

    let hits = store.search("first", None, 10).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(
        hits[0].score,
        Field::Title.weight(),
        "indexing the same term twice must not double the score: {}",
        hits[0].score
    );

    // And a *changed* title drops the old term: a stale row is worse than no
    // row, because it is indistinguishable from a current one.
    store
        .index_object("o-r", &[(Field::Title, "second".to_string())])
        .await
        .unwrap();
    assert!(
        store.search("first", None, 10).await.unwrap().is_empty(),
        "re-indexing with different terms must drop the old ones"
    );
    assert_eq!(store.search("second", None, 10).await.unwrap().len(), 1);
}

/// An index row must not outlive its object.
#[tokio::test]
async fn deleting_an_object_removes_its_terms() {
    let store = sqlite_store().await;
    let ts = now();
    insert_object(&store, "o-del", "Ephemeral", &ts).await;
    store
        .index_object("o-del", &[(Field::Title, "ephemeral".to_string())])
        .await
        .unwrap();
    assert_eq!(store.search("ephemeral", None, 10).await.unwrap().len(), 1);

    // `Store::delete_object`, not a bare `DELETE FROM object`. This test used the
    // bare delete and it worked only because `search_term.object_id` was a
    // foreign key with `ON DELETE CASCADE`; migration 0017 dropped that
    // constraint so a tag could be indexed, and the cascade went with it. The
    // guarantee moved from the schema into this method, and the test is what
    // says the method is doing its half.
    store.delete_object("o-del").await.unwrap();
    let hits = store.search("ephemeral", None, 10).await.unwrap();
    assert!(
        hits.is_empty(),
        "a search result that opens onto a deleted object: {hits:?}"
    );
}

/// [`Store::deindex_object`] on an object that still exists.
///
/// The cascade test above cannot see this function at all -- `ON DELETE
/// CASCADE` removes the terms whether or not it works -- so a no-op
/// implementation would pass the whole suite. The case it *is* for is an
/// object that must stop being findable without being deleted, which is what
/// §14.1's consent change needs: the object is still in the library, it is just
/// no longer something a search should surface.
#[tokio::test]
async fn deindexing_removes_terms_without_deleting_the_object() {
    let store = sqlite_store().await;
    let ts = now();
    insert_object(&store, "o-hide", "Hideable", &ts).await;
    store
        .index_object("o-hide", &[(Field::Title, "hideable".to_string())])
        .await
        .unwrap();
    assert_eq!(store.search("hideable", None, 10).await.unwrap().len(), 1);

    let removed = store.deindex_object("o-hide").await.unwrap();
    assert_eq!(removed, 1, "deindexing should report what it removed");
    assert!(
        store.search("hideable", None, 10).await.unwrap().is_empty(),
        "the object must stop being findable"
    );
    // And the object itself is untouched: this is not a delete in disguise.
    // A scalar fetch, not `exec` -- see the note in the injection test about
    // `rows_affected` meaning different things on the two engines.
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM object")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(n, 1, "deindex must not remove the object");
}

// ----------------------------------------------------------------- guardrails

/// A query with no searchable terms is an error, not an empty result and
/// certainly not "everything".
#[tokio::test]
async fn an_empty_query_is_refused() {
    let store = sqlite_store().await;
    load(&store).await;
    for q in ["", "   ", "the", "the a of"] {
        match store.search(q, None, 10).await {
            Err(SearchError::EmptyQuery(_)) => {}
            other => panic!("query {q:?} should be refused, got {other:?}"),
        }
    }
}

/// The limit is bounded, so a caller cannot ask for the whole library.
#[tokio::test]
async fn the_result_limit_is_bounded() {
    let store = sqlite_store().await;
    load(&store).await;
    assert!(matches!(
        store.search("ascent", None, search::MAX_RESULTS + 1).await,
        Err(SearchError::LimitTooLarge { .. })
    ));
    // And the bound is honoured, not just rejected.
    assert!(store.search("ascent", None, 1).await.unwrap().len() <= 1);
}

/// A query full of SQL metacharacters finds nothing rather than doing anything
/// interesting.
///
/// The tokenizer consumes them as separators, so they cannot reach the
/// statement -- this asserts that end to end rather than asserting the
/// tokenizer in isolation, because "the tokenizer is safe" and "the query is
/// safe" are different claims and only one of them is about the database.
#[tokio::test]
async fn a_query_carrying_sql_cannot_reach_the_statement() {
    let store = sqlite_store().await;
    load(&store).await;
    for q in [
        "'; DROP TABLE object; --",
        "\" OR 1=1 --",
        "ascent') OR ('1'='1",
        "ascent%_\\",
    ] {
        // Either answer is fine -- an empty query is refused, and a query with
        // real terms in it searches them. What is not fine is a result from
        // outside the corpus, which is what an injection would produce.
        match store.search(q, None, 10).await {
            Err(SearchError::EmptyQuery(_)) => {}
            Ok(hits) => assert!(
                hits.iter()
                    .all(|h| CORPUS.iter().any(|f| f.id == h.object_id)),
                "query {q:?} returned something outside the corpus: {hits:?}"
            ),
            Err(e) => panic!("query {q:?} failed oddly: {e}"),
        }
    }
    // The table survived, with the corpus still in it. Counting the rows is
    // the assertion: a `DROP TABLE` that had got through would leave zero, and
    // a `1=1` that had got through would leave the same three. The count is
    // `load`'s, which is why this test loads the corpus first.
    // A count, not `rows_affected`. `exec` returns the number of rows a
    // statement *changed*, which for a `SELECT` is 0 on SQLite and 1 on
    // Postgres -- so the first version of this assertion passed or failed
    // depending on the engine, which is a good illustration of why the
    // two-engine rule is worth enforcing everywhere and not only in search.
    let n: i64 = match &store {
        Store::Sqlite(p) => sqlx::query_scalar("SELECT COUNT(*) FROM object")
            .fetch_one(p)
            .await
            .unwrap(),
        Store::Postgres(p) => sqlx::query_scalar("SELECT COUNT(*) FROM object")
            .fetch_one(p)
            .await
            .unwrap(),
    };
    assert_eq!(
        n,
        CORPUS.len() as i64,
        "the object table is intact after the injection attempts"
    );
}

/// The full §9.2 field list is representable, and every field can be searched.
///
/// A test that only indexes titles would pass everything above while §9.2's
/// breadth — tags, performers, markers, transcripts, external ids — is
/// unimplemented. This asserts the *enum* covers §9.2 and that each field name
/// is one the SQL's `CASE` resolves, which is where a renamed field would
/// silently score zero.
#[tokio::test]
async fn every_field_in_the_spec_is_searchable() {
    let store = sqlite_store().await;
    let ts = now();
    insert_object(&store, "o-all", "Findable", &ts).await;

    // One field at a time, re-indexing between them. `index_object` *replaces*
    // -- which is the behaviour `reindexing_replaces_rather_than_doubling`
    // asserts -- so indexing all ten in a loop would leave only the last one
    // and every other field would look unimplemented. That is the test being
    // wrong about the API, not the API being wrong.
    for f in Field::ALL {
        // Index the *tokenized* form, because that is what a real indexer does
        // -- `load` does the same, and an index built from raw field names
        // would be searchable by a query that has been through the tokenizer,
        // which is a fixture that proves nothing about the pipeline.
        let terms: Vec<(Field, String)> = search::tokenize(f.as_str())
            .into_iter()
            .map(|t| (f, t))
            .collect();
        store.index_object("o-all", &terms).await.unwrap();
        let hits = store.search(f.as_str(), None, 10).await.unwrap();
        assert_eq!(
            hits.len(),
            1,
            "field {} is in §9.2's list but a search for its own name finds \
             nothing -- the CASE in the ranking query has no arm for it",
            f.as_str()
        );
        // `external_id` is two terms, so its score is the weight of each
        // matched term, not the weight of the field once. Everything else is a
        // single term and scores exactly its weight, which is the assertion
        // that the CASE resolves each field to its own number.
        let matched = terms.len() as i64;
        assert_eq!(
            hits[0].score,
            f.weight() * matched,
            "field {} scored {} rather than {} -- a field the CASE does not \
             resolve scores zero, and one it double-counts does not score \
             its weight per term",
            f.as_str(),
            hits[0].score,
            f.weight() * matched
        );
        assert_eq!(
            hits[0].fields,
            vec![f.as_str().to_string()],
            "the hit should report the one field that was indexed"
        );
    }
}

/// The JSON shape, so the API layer has something to assert against.
#[test]
fn a_hit_serialises_with_its_fields() {
    let hit = search::SearchHit {
        object_id: "o-1".to_string(),
        score: 100,
        fields: vec!["title".to_string()],
    };
    let v: serde_json::Value = serde_json::to_value(&hit).unwrap();
    assert_eq!(
        v,
        json!({"object_id": "o-1", "score": 100, "fields": ["title"]})
    );
}
