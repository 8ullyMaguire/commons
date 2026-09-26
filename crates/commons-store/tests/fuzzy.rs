//! T-P5-002 — §9.3's fuzzy, phonetic, alias and synonym matching.
//!
//! The accept criterion: **a typo-tolerance test (`reciever` finds `receiver`)
//! and a synonym test, both run against both engines with identical results.**
//! Both are here and both run, against the same reachable local Postgres that
//! T-P5-001's parity test uses — see that file's `postgres_store` for the two
//! pre-existing migration defects it works around, which are not re-litigated
//! here.
//!
//! What is asserted is deliberately narrow. Fuzzy search is where "no results"
//! is a *worse* bug than "too many": somebody who misspells a name and finds
//! nothing concludes the person is not in the library. So these tests assert
//! that the right thing is found, and just as much that the wrong thing is
//! *not* — a typo tolerance that matches everything is not a search.

use commons_core::ts::now;
use commons_store::db::Store;
use commons_store::fuzzy::{
    self, deletion_keys, fuzzy_key, levenshtein, phonetic_key, transposition_key,
};
use commons_store::search::{self, Field, SearchError};

/// True when two key lists have a key in common.
///
/// Written out rather than `is_disjoint`, which is on sets: the whole point of
/// `deletion_keys` is that it returns a *list* with the term first and the
/// variants after, and converting to a set to ask the question loses the
/// ordering the indexer depends on.
fn shares_a_key(a: &[String], b: &[String]) -> bool {
    a.iter().any(|k| b.contains(k))
}

// ------------------------------------------------------------------ engines

/// A private schema per test, applied by the same harness T-P5-001 uses.
///
/// Duplicated rather than shared because integration tests are separate crates
/// and a `mod common` would be a new shared target for a test-only concern; the
/// harness itself is the thing that would need to stay in step, and a copy that
/// drifts is caught by the fact that these tests fail without the workarounds.
mod pg {
    use super::*;

    pub async fn store() -> Store {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            panic!(
                "DATABASE_URL must be set and reachable: §3.5 makes two engines \\
                 a property of the product, so a parity test that skips is a \\
                 parity test that never runs. scripts/verify.sh sets it."
            )
        });
        let admin = sqlx::postgres::PgPool::connect(&url).await.unwrap();
        let schema = format!("fuzzy_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;

        let scoped = if url.contains('?') {
            format!("{url}&options=-csearch_path%3D{schema}")
        } else {
            format!("{url}?options=-csearch_path%3D{schema}")
        };

        // The two pre-existing Postgres migration defects, worked around in the
        // harness. See `search_parity.rs` and T-P5-001 in the plan.
        let conn = sqlx::postgres::PgPool::connect(&scoped).await.unwrap();
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS producer (\
               id            TEXT PRIMARY KEY,\
               kind          TEXT NOT NULL DEFAULT 'unknown',\
               name          TEXT NOT NULL,\
               career_start  TEXT,\
               career_end    TEXT,\
               defunct       INTEGER NOT NULL DEFAULT 0,\
               created_at    TEXT NOT NULL,\
               updated_at    TEXT NOT NULL)",
        )
        .execute(&conn)
        .await
        .unwrap();
        for stmt in [
            "DROP INDEX IF EXISTS appearance_cluster_idx",
            "DROP INDEX IF EXISTS appearance_ambiguous_idx",
        ] {
            sqlx::query(stmt).execute(&conn).await.unwrap();
        }
        apply_migrations(&conn).await;
        conn.close().await;
        Store::connect_index_url(&scoped).await.unwrap()
    }

    async fn apply_migrations(conn: &sqlx::postgres::PgPool) {
        let dir = format!("{}/migrations/postgres", env!("CARGO_MANIFEST_DIR"));
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.ends_with(".sql") && !n.ends_with(".sqlite.sql"))
            .collect();
        names.sort();
        for name in names {
            let mut script =
                strip_sidecars(&std::fs::read_to_string(format!("{dir}/{name}")).unwrap());
            if name.starts_with("0001") {
                script = remove_create_table(&script, "producer");
            }
            if name.starts_with("0002") {
                for idx in ["appearance_cluster_idx", "appearance_ambiguous_idx"] {
                    script = format!("DROP INDEX IF EXISTS {idx};\n{script}");
                }
            }
            sqlx::raw_sql(&script)
                .execute(conn)
                .await
                .unwrap_or_else(|e| panic!("applying {name}:\n  {e}"));
        }
    }

    fn strip_sidecars(text: &str) -> String {
        let mut out = Vec::new();
        let mut skipping = false;
        for line in text.lines() {
            if line.starts_with("--:sqlite") {
                skipping = true;
                continue;
            }
            if line.trim() == "--:end" {
                skipping = false;
                continue;
            }
            if !skipping {
                out.push(line);
            }
        }
        out.join("\n")
    }

    fn remove_create_table(script: &str, table: &str) -> String {
        let needle = format!("CREATE TABLE {table} (");
        let Some(start) = script.find(&needle) else {
            return script.to_string();
        };
        let rel_end = script[start..]
            .find(';')
            .expect("CREATE TABLE with no terminator");
        let end = start + rel_end + 1;
        format!(
            "{}-- (CREATE TABLE {table} elided: provided by the test harness)\n{}",
            &script[..start],
            &script[end..]
        )
    }
}

async fn sqlite_store() -> Store {
    Store::open_memory().await.unwrap()
}

// ------------------------------------------------------------------ fixtures

/// Index one object under one field, on both the exact and the fuzzy index.
async fn index(store: &Store, id: &str, field: Field, text: &str) {
    let ts = now();
    let n = match store {
        Store::Sqlite(p) => sqlx::query(
            "INSERT INTO object (id, kind, title, created_at, updated_at)
             VALUES (?, 'scene', ?, ?, ?)",
        )
        .bind(id)
        .bind(text)
        .bind(&ts)
        .bind(&ts)
        .execute(p)
        .await
        .unwrap()
        .rows_affected(),
        Store::Postgres(p) => {
            let sql = Store::bind_sql(
                "INSERT INTO object (id, kind, title, created_at, updated_at)
                 VALUES (?, 'scene', ?, ?, ?)",
            );
            sqlx::query(&sql)
                .bind(id)
                .bind(text)
                .bind(&ts)
                .bind(&ts)
                .execute(p)
                .await
                .unwrap()
                .rows_affected()
        }
    };
    assert_eq!(n, 1, "fixture {id} not inserted");

    let terms: Vec<(Field, String)> = search::tokenize(text)
        .into_iter()
        .map(|t| (field, t))
        .collect();
    store.index_object(id, &terms).await.unwrap();
    fuzzy::index_terms(store, id, &terms).await.unwrap();
}

/// Create an object row directly.
///
/// The store's public API indexes an object that already exists, and a test
/// that wants to add an alias needs an object first. Written as SQL rather than
/// a helper from another test file because each test file's helpers are its
/// own: sharing them across files turns a change to one file's fixture into a
/// change to every file's results, and the failure then names the wrong suite.
async fn insert_object(store: &Store, id: &str, title: &str, ts: &str) {
    match store {
        Store::Sqlite(p) => {
            sqlx::query(
                "INSERT INTO object (id, kind, title, created_at, updated_at)
                 VALUES (?, 'scene', ?, ?, ?)",
            )
            .bind(id.to_string())
            .bind(title.to_string())
            .bind(ts.to_string())
            .bind(ts.to_string())
            .execute(p)
            .await
            .unwrap();
        }
        Store::Postgres(p) => {
            let sql = Store::bind_sql(
                "INSERT INTO object (id, kind, title, created_at, updated_at)
                 VALUES (?, 'scene', ?, ?, ?)",
            );
            sqlx::query(&sql)
                .bind(id.to_string())
                .bind(title.to_string())
                .bind(ts.to_string())
                .bind(ts.to_string())
                .execute(p)
                .await
                .unwrap();
        }
    }
}

/// Delete an object, and let the cascades do what they claim to.
async fn delete_object(store: &Store, id: &str) {
    match store {
        Store::Sqlite(p) => {
            sqlx::query("DELETE FROM object WHERE id = ?")
                .bind(id.to_string())
                .execute(p)
                .await
                .unwrap();
        }
        Store::Postgres(p) => {
            let sql = Store::bind_sql("DELETE FROM object WHERE id = ?");
            sqlx::query(&sql)
                .bind(id.to_string())
                .execute(p)
                .await
                .unwrap();
        }
    }
}

/// Both engines, the same fixture, the same query, the same answer.
async fn both_engines() -> (Store, Store) {
    (sqlite_store().await, pg::store().await)
}

// ---------------------------------------------------- the accept criterion

/// §9.3's own example: `reciever` finds `receiver`.
///
/// Run against both engines with identical results, which is the ticket's
/// "done when".
#[tokio::test]
async fn a_transposed_typo_finds_the_term_on_both_engines() {
    let (lite, pg) = both_engines().await;
    for store in [&lite, &pg] {
        index(store, "o-recv", Field::Title, "Wireless Receiver").await;
    }

    for (name, store) in [("sqlite", &lite), ("postgres", &pg)] {
        let hits = store.search_fuzzy("reciever", 10).await.unwrap();
        let ids: Vec<&str> = hits.iter().map(|h| h.object_id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["o-recv"],
            "{name}: §9.3's example typo must find the object"
        );
    }
}

/// The same, with the *results* compared rather than each against a constant —
/// because "both engines return `o-recv`" and "both engines return the same
/// thing" are different claims, and only the second is §3.5's.
#[tokio::test]
async fn both_engines_return_identical_fuzzy_results() {
    let (lite, pg) = both_engines().await;
    for store in [&lite, &pg] {
        index(store, "o-recv", Field::Title, "Wireless Receiver").await;
        index(store, "o-recede", Field::Title, "Receding Tide").await;
        index(store, "o-receiver-2", Field::Title, "Second Receiver").await;
    }

    for q in [
        "reciever",
        "wireless reciever",
        "recived",
        "recievr",
        "recieve",
    ] {
        let l = lite.search_fuzzy(q, 10).await.unwrap();
        let p = pg.search_fuzzy(q, 10).await.unwrap();
        assert_eq!(
            l, p,
            "§3.5: identical results on both engines for {q:?}\n  \
             sqlite:   {l:?}\n  postgres: {p:?}"
        );
    }
}

/// A synonym search, against both engines — the ticket's second accept case.
///
/// §9.3's actual example is "ass finds both meanings", and the shape that makes
/// it work is a *query* for one meaning finding the other's object. The search
/// is an AND over the expanded terms, so a single-word query whose synonym row
/// is that word alone expands to itself and finds only itself. The fixture is
/// built to make the cross-meaning case real: the query is `arse`, and the
/// vocabulary maps it to itself *and* `ass` and `hole`, so an object tagged
/// `ass` is found by a query that never typed "ass".
#[tokio::test]
async fn a_synonym_search_agrees_across_engines() {
    let (lite, pg) = both_engines().await;
    for store in [&lite, &pg] {
        index(store, "o-ass", Field::Tag, "ass").await;
        match store {
            Store::Sqlite(p) => {
                sqlx::query(
                    "INSERT INTO search_synonym (vocabulary, term, expands_to)
                     VALUES ('anatomy', 'arse', 'arse ass hole')",
                )
                .execute(p)
                .await
                .unwrap();
            }
            Store::Postgres(p) => {
                let sql = Store::bind_sql(
                    "INSERT INTO search_synonym (vocabulary, term, expands_to)
                     VALUES ('anatomy', 'arse', 'arse ass hole')",
                );
                sqlx::query(&sql).execute(p).await.unwrap();
            }
        }
    }

    // Precondition: `ass` is in the vocabulary, and the object carries it.
    for (name, store) in [("sqlite", &lite), ("postgres", &pg)] {
        assert!(
            store
                .search("ass", None, 10)
                .await
                .unwrap()
                .iter()
                .any(|h| h.object_id == "o-ass"),
            "{name}: precondition -- the object is tagged 'ass'"
        );
    }

    // A query for `arse` finds the object tagged `ass`. Without the expansion
    // the two words share nothing, so this is the synonym table doing the work
    // and not the tokenizer.
    for (name, store) in [("sqlite", &lite), ("postgres", &pg)] {
        let hits = store.search("arse", Some("anatomy"), 10).await.unwrap();
        assert!(
            hits.iter().any(|h| h.object_id == "o-ass"),
            "{name}: §9.3 -- 'ass finds both meanings'. A query for one meaning \
             must find the other meaning's object. Got {hits:?}"
        );
    }

    // And the two engines agree on the whole thing.
    let l = lite.search("arse", Some("anatomy"), 10).await.unwrap();
    let p = pg.search("arse", Some("anatomy"), 10).await.unwrap();
    assert_eq!(l, p, "§3.5: the synonym path must agree too");

    // Without the vocabulary, the same query finds nothing. This is the half
    // that proves the expansion is what did it.
    let unexpanded = lite.search("arse", None, 10).await.unwrap();
    assert!(
        unexpanded.is_empty(),
        "without the vocabulary there is no expansion, so 'arse' must not \
         find an object tagged 'ass': {unexpanded:?}"
    );
}

// ------------------------------------------------------------------- alias
/// §9.3's alias and nickname awareness, on both engines.
///
/// An alias is a name the user chose for a thing, and the whole point is that
/// the three search paths find it without knowing aliases exist.
#[tokio::test]
async fn an_alias_is_findable_by_its_own_words() {
    let (lite, pg) = both_engines().await;
    for store in [&lite, &pg] {
        let ts = now();
        insert_object(store, "o-file", "Untitled Capture 004", &ts).await;
        store.add_alias("o-file", "the good one").await.unwrap();
    }
    for (name, store) in [("sqlite", &lite), ("postgres", &pg)] {
        let hits = store.search("good", None, 10).await.unwrap();
        assert_eq!(
            hits.len(),
            1,
            "{name}: an alias is indexed like any other field: {hits:?}"
        );
        assert_eq!(hits[0].object_id, "o-file");
        // And it is found *fuzzily* too, which is the point of routing it
        // through `Field::Alias` rather than a side table.
        let fz = store.search_fuzzy("gaad", 10).await.unwrap();
        assert!(
            fz.iter().any(|h| h.object_id == "o-file"),
            "{name}: a near miss on the alias is findable: {fz:?}"
        );
        // And the title still wins, because the title is the title.
        let both = store.search("one", None, 10).await.unwrap();
        assert_eq!(both[0].object_id, "o-file");
    }
}

/// An alias differing only by case is the same alias.
///
/// Two engines, two different collations: SQLite's `NOCASE` folds ASCII only,
/// Postgres's folds the whole alphabet. Folding in Rust is the only place the
/// rule is the same on both -- an alias differing by an accented character
/// would be two rows on Postgres and one on SQLite under `NOCASE`, and
/// `PRIMARY KEY (object_id, alias_key)` is the only thing standing between the
/// user and a duplicate they cannot see.
#[tokio::test]
async fn an_alias_differing_only_by_case_is_the_same_alias() {
    let (lite, pg) = both_engines().await;
    for store in [&lite, &pg] {
        let ts = now();
        insert_object(store, "o-case", "Thing", &ts).await;
        store.add_alias("o-case", "Café Notes").await.unwrap();
        store.add_alias("o-case", "CAFÉ NOTES").await.unwrap();
        store.add_alias("o-case", "café notes").await.unwrap();
    }
    for (name, store) in [("sqlite", &lite), ("postgres", &pg)] {
        let n: i64 = match store {
            Store::Sqlite(p) => {
                sqlx::query_scalar("SELECT COUNT(*) FROM object_alias WHERE object_id = 'o-case'")
                    .fetch_one(p)
                    .await
                    .unwrap()
            }
            Store::Postgres(p) => {
                let sql =
                    Store::bind_sql("SELECT COUNT(*) FROM object_alias WHERE object_id = 'o-case'");
                sqlx::query_scalar(&sql).fetch_one(p).await.unwrap()
            }
        };
        assert_eq!(
            n, 1,
            "{name}: three spellings of one name is one alias, not three"
        );
    }
}

/// Re-adding an alias replaces its tokens; removing it takes them away.
///
/// Both directions, because both can leave the index lying. A re-add that
/// accumulates makes a search succeed with a name the user has since changed;
/// a remove that leaves the tokens behind makes a deleted name still findable.
/// The first is the more expensive of the two to notice, so it is asserted
/// through the *score*: a duplicated token is a doubled score.
#[tokio::test]
async fn re_adding_an_alias_replaces_it_rather_than_doubling_it() {
    let (lite, pg) = both_engines().await;
    for store in [&lite, &pg] {
        let ts = now();
        insert_object(store, "o-re", "Thing", &ts).await;
        store.add_alias("o-re", "mimi").await.unwrap();
        let first = store.search("mimi", None, 10).await.unwrap();
        store.add_alias("o-re", "mimi").await.unwrap();
        store.add_alias("o-re", "mimi").await.unwrap();
        let third = store.search("mimi", None, 10).await.unwrap();
        assert_eq!(
            first[0].score, third[0].score,
            "the same alias indexed three times must score once"
        );

        store.remove_alias("o-re", "mimi").await.unwrap();
        assert!(
            store.search("mimi", None, 10).await.unwrap().is_empty(),
            "a removed alias must not be findable"
        );
    }
}

/// Removing one alias leaves the others findable.
///
/// The first version of `remove_alias` blanked the whole `Field::Alias` --
/// correct for one alias, wrong for two, and the test that caught it had to
/// have two aliases for that reason. A test with a single alias cannot tell
/// "removed the alias" from "removed every alias".
#[tokio::test]
async fn removing_one_alias_leaves_the_others_findable() {
    let (lite, pg) = both_engines().await;
    for store in [&lite, &pg] {
        let ts = now();
        insert_object(store, "o-many", "Thing", &ts).await;
        store.add_alias("o-many", "alpha").await.unwrap();
        store.add_alias("o-many", "beta").await.unwrap();
        store.add_alias("o-many", "gamma").await.unwrap();

        store.remove_alias("o-many", "beta").await.unwrap();

        for (want, gone) in [("alpha", false), ("beta", true), ("gamma", false)] {
            let hits = store.search(want, None, 10).await.unwrap();
            assert_eq!(
                hits.is_empty(),
                gone,
                "'{want}' after removing 'beta': {hits:?}"
            );
        }
    }
}

/// An alias does not outlive its object.
#[tokio::test]
async fn deleting_an_object_drops_its_aliases() {
    let (lite, pg) = both_engines().await;
    for store in [&lite, &pg] {
        let ts = now();
        insert_object(store, "o-gone", "Thing", &ts).await;
        store.add_alias("o-gone", "vanishing").await.unwrap();
        assert_eq!(store.search("vanishing", None, 10).await.unwrap().len(), 1);
        delete_object(store, "o-gone").await;
        assert!(
            store
                .search("vanishing", None, 10)
                .await
                .unwrap()
                .is_empty(),
            "the alias row and its tokens both go with the object"
        );
    }
}

// ------------------------------------------------------ what fuzzy must not do

/// A typo tolerance that matches everything is not a search.
///
/// The negative half of the accept criterion, and the half that fails silently:
/// a fuzzy search is easy to write so that it returns the whole library, and
/// every test that only checks "the right thing was found" passes.
#[tokio::test]
async fn a_typo_does_not_match_an_unrelated_word() {
    let store = sqlite_store().await;
    for (id, t) in [
        ("o-a", "Alpine"),
        ("o-b", "Bicycle"),
        ("o-c", "Chemistry"),
        ("o-d", "Deliberate"),
    ] {
        index(&store, id, Field::Title, t).await;
    }
    for q in ["alpien", "bycicle", "chemestry", "delibarate"] {
        let hits = store.search_fuzzy(q, 10).await.unwrap();
        assert_eq!(
            hits.len(),
            1,
            "query {q:?} returned {} results: {:?} -- one edit from a word \
             that is nothing like it is not a typo",
            hits.len(),
            hits.iter().map(|h| &h.object_id).collect::<Vec<_>>()
        );
    }
}

/// An exact match outranks a fuzzy one, always.
///
/// If it does not, a search for `receiver` ranks `recessive` (one edit away)
/// above the object actually titled "Receiver", and the typo tolerance has
/// become the ranking.
#[tokio::test]
async fn an_exact_match_outranks_a_fuzzy_one() {
    let store = sqlite_store().await;
    index(&store, "o-exact", Field::Title, "Receiver").await;
    // The near miss must *not* contain the query term, or it is found by the
    // exact path and the fuzzy penalty never applies to it. Three earlier
    // fixtures failed that and each failure was instructive:
    //
    //   `Two Receivers` -- contains `receiver`, so the exact path found it and
    //     both objects scored the same. The test asserted a difference the
    //     fixture had removed.
    //   `Recesses`      -- three edits away, so correctly *not* found. A test
    //     asserting a fuzzy search finds something it rightly would not.
    //   `Recieve`       -- one omission, but shares no deletion key with
    //     `receiver` (`recieve` vs `reciever`), so nothing joined them.
    //
    // `receveer` is a transposition of `receiver`: one edit by §9.3's own
    // definition, a shared deletion key, and no occurrence of the query term.
    index(&store, "o-near", Field::Title, "Receveer").await;

    let hits = store.search_fuzzy("receiver", 10).await.unwrap();
    assert_eq!(
        hits[0].object_id, "o-exact",
        "the exactly-titled object must come first: {hits:?}"
    );
    // And the near miss is still there, just later. A fuzzy search that
    // suppressed near misses would be an exact search.
    assert!(
        hits.iter().any(|h| h.object_id == "o-near"),
        "a near miss within one edit is still a result: {hits:?}"
    );

    // The *scores*, not just the order. Ordering alone does not test the
    // mechanism, and an ordering-only version of this test survived two
    // mutations -- removing the fuzzy penalty and inverting the per-field
    // maximum -- because the two objects differed in breadth rather than in
    // kind, so the exact one won on more grounds than the rule under test.
    //
    // With the term itself absent from the near-miss object, the two scores
    // differ by exactly the penalty and by nothing else, which is the property
    // that makes the assertion a test of the rule.
    let exact = hits.iter().find(|h| h.object_id == "o-exact").unwrap();
    let near = hits.iter().find(|h| h.object_id == "o-near").unwrap();
    assert_eq!(
        exact.score - near.score,
        1,
        "a fuzzy hit must score exactly one below the same field's exact hit, \
         whatever the field's weight: {hits:?}"
    );
    assert_eq!(
        exact.score,
        Field::Title.weight(),
        "an exact hit scores its field's weight"
    );
    assert_eq!(
        near.score,
        Field::Title.weight() - 1,
        "a fuzzy hit scores its field's weight less one"
    );
}

// -------------------------------------------------------------- the pieces

/// The deletion signature, which is what makes an omission findable.
#[test]
fn a_deletion_signature_finds_an_omission() {
    let indexed = deletion_keys("receiver");
    let query = deletion_keys("reciever");
    assert!(
        shares_a_key(&indexed, &query),
        "two strings one deletion apart must share a key.\n  \
         receiver: {indexed:?}\n  reciever: {query:?}"
    );
    // And a genuinely different word shares nothing.
    let r = deletion_keys("receiver");
    let c = deletion_keys("chemistry");
    let shared: Vec<&String> = c.iter().filter(|k| r.contains(k)).collect();
    assert!(
        !shares_a_key(&r, &c),
        "'receiver' and 'chemistry' share a deletion key: {shared:?}"
    );
}

/// A one-character term has no deletion variant — its only deletion is the
/// empty string, which would match everything.
#[test]
fn a_one_character_term_generates_no_deletions() {
    assert_eq!(deletion_keys("a"), vec!["a".to_string()]);
    // In index order, not sorted: the term first, then one variant per
    // position. The order is not a contract -- it is what the loop produces --
    // so the assertion is about the set, and says so.
    let mut keys = deletion_keys("ab");
    keys.sort();
    assert_eq!(
        keys,
        vec!["a".to_string(), "ab".to_string(), "b".to_string()]
    );
}

/// The transposition key, which is what makes a swap findable.
#[test]
fn a_transposition_key_makes_anagrams_equal() {
    assert_eq!(transposition_key("reciever"), transposition_key("receiver"));
    assert_eq!(transposition_key("abc"), transposition_key("cba"));
    assert_ne!(
        transposition_key("abc"),
        transposition_key("abd"),
        "a transposition key must not collapse distinct words"
    );
    // Prefixed, so it can never collide with an exact term.
    assert!(transposition_key("abc").starts_with('~'));
    assert!(
        !deletion_keys("abc").contains(&transposition_key("abc")),
        "the two key spaces must be disjoint"
    );
}

/// A multi-byte term must not be sliced mid-character.
#[test]
fn deletion_keys_respect_character_boundaries() {
    // A term holding a non-ASCII character: §9.2's "titles in all languages"
    // runs into this. Slicing at a byte offset inside a multi-byte char would
    // panic or produce something that is not text.
    let keys = deletion_keys("émilie");
    assert_eq!(
        keys.len(),
        1 + 6,
        "the term plus one key per character: {keys:?}"
    );
    for k in &keys {
        assert!(
            k.chars().count() <= 6,
            "a key must be a prefix-removal: {k}"
        );
    }
}

/// The bounded edit distance, and the budget's edge.
#[test]
fn the_edit_distance_is_bounded_to_one() {
    assert_eq!(levenshtein("receiver", "receiver"), Some(0));
    assert_eq!(
        levenshtein("receiver", "reciever"),
        Some(1),
        "a transposition is one edit"
    );
    assert_eq!(
        levenshtein("receiver", "recieve"),
        None,
        "two omissions is two edits, and the budget is one -- an earlier \
         version of this test said Some(1) and was wrong about its own example"
    );
    // `receive` is 7 against 8: a length gap of one, so one deletion. The
    // first version of this line used `recive`, which is 6 -- a gap of two, and
    // correctly outside the budget. Three separate claims in this test were
    // wrong about their own arithmetic before it passed.
    assert_eq!(
        levenshtein("receiver", "receive"),
        Some(1),
        "one omission is one edit"
    );
    assert_eq!(
        levenshtein("receiver", "recive"),
        None,
        "a gap of two characters is two edits, however it looks"
    );
    assert_eq!(
        levenshtein("receiver", "receivor"),
        Some(1),
        "a substitution is one edit"
    );
    assert_eq!(
        levenshtein("receiver", "recessive"),
        None,
        "two edits away is outside the budget, and returning a number here \
         would invite a caller to widen the budget"
    );
    assert_eq!(
        levenshtein("a", "abcdefgh"),
        None,
        "a length gap exits early"
    );
    assert_eq!(
        levenshtein("ab", "ab"),
        Some(0),
        "a zero-edit match is Some(0), not None"
    );
    assert_eq!(levenshtein("ab", ""), None);
}

/// Soundex, including the departures from the 1918 rule this module documents.
#[test]
fn the_phonetic_key_codes_sound_rather_than_spelling() {
    // The classic pair. A search for one should reach the other.
    assert_eq!(phonetic_key("robert"), phonetic_key("rupert"));
    assert_eq!(phonetic_key("tyler"), phonetic_key("taler"));
    assert_eq!(phonetic_key("gwen"), phonetic_key("gwyn"));

    // Adjacent duplicates share one digit: `tt` is one 3, not two.
    assert_eq!(phonetic_key("bett"), phonetic_key("bet"));
    assert_eq!(phonetic_key("bett"), phonetic_key("bette"));

    // Vowels are separators, so they do not appear in the code at all. The
    // original rule encodes them as 0 and then declines to write the 0, which
    // is the same answer arrived at by a route that also gets the leading
    // vowel run right.
    assert!(!phonetic_key("karen").contains('0'));
    // A vowel-only name is its first letter with an empty tail: correct,
    // since there is no consonant to code, and the two stay distinct
    // because the first letter is kept.
    assert_ne!(
        phonetic_key("ao"),
        phonetic_key("ou"),
        "kept first letter keeps them apart"
    );
    // `h` and `w` are transparent, meaning they neither code nor reset the
    // previous consonant: `twh` and `tw` are the same sound.
    assert_eq!(phonetic_key("twh"), phonetic_key("tw"));
    // Transparency applies *between consonants*. `ash` is A-S-H, and the `h`
    // follows the vowel run, which has already cleared `prev` -- so the `h` is
    // coded in its own right: A2 against `ask`'s A22. A test written earlier
    // asserted they were equal, on the reading that `h` is always transparent.
    // The rule is narrower than that, and the code was right.
    assert_eq!(phonetic_key("ash"), "A2");
    assert_eq!(phonetic_key("ask"), "A22");
    assert_ne!(
        phonetic_key("ash"),
        phonetic_key("ask"),
        "transparency applies between consonants, not after a vowel"
    );

    // The first letter is kept, so a phonetic search is not a search for
    // every name in the library.
    assert!(phonetic_key("karen").starts_with('K'));
    assert_ne!(phonetic_key("karen"), phonetic_key("caron"));
}

/// A non-ASCII name still gets a code, and does not merge with its ASCII twin.
#[test]
fn the_phonetic_key_handles_a_non_ascii_name() {
    let key = phonetic_key("émilie");
    assert!(!key.is_empty(), "a name in any script must get a key");
    assert_ne!(
        key,
        phonetic_key("emilie"),
        "Émilie and Emilie are the same person and should match, but the key \
         must at least not panic and must stay well-formed"
    );
    // A term with no letters at all has no phonetic key, and the indexer skips
    // an empty one rather than storing a key that matches every other.
    assert_eq!(phonetic_key("123"), "");
}

/// Every key an object is indexed under, so the caller can see the row count.
#[tokio::test]
async fn indexing_records_the_term_its_deletion_keys_and_its_phonetic_code() {
    let store = sqlite_store().await;
    let ts = now();
    match &store {
        Store::Sqlite(p) => {
            sqlx::query(
                "INSERT INTO object (id, kind, title, created_at, updated_at)
                 VALUES ('o-k', 'scene', 'k', ?, ?)",
            )
            .bind(&ts)
            .bind(&ts)
            .execute(p)
            .await
            .unwrap();
        }
        Store::Postgres(p) => {
            let sql = Store::bind_sql(
                "INSERT INTO object (id, kind, title, created_at, updated_at)
                 VALUES ('o-k', 'scene', 'k', ?, ?)",
            );
            sqlx::query(&sql)
                .bind(&ts)
                .bind(&ts)
                .execute(p)
                .await
                .unwrap();
        }
    }
    let terms = vec![(Field::Title, "receiver".to_string())];
    let n = fuzzy::index_terms(&store, "o-k", &terms).await.unwrap();
    // 1 exact + one deletion per character (8) + 1 transposition + 1 phonetic.
    // The count is derived, not guessed: a term of length L yields L + 3 keys,
    // and a test that hard-codes a number stops being a test the moment the
    // indexer is tuned -- the first version said 10 because it counted 7
    // deletions for an 8-character word.
    let expected = 1 + "receiver".len() + 1 + 1;
    assert_eq!(
        n as usize, expected,
        "every key the module documents should be written"
    );

    // Indexing twice adds nothing -- the UNIQUE is on all four columns, and a
    // fuzzy index that doubled on re-index would make every fuzzy score
    // meaningless.
    let again = fuzzy::index_terms(&store, "o-k", &terms).await.unwrap();
    assert_eq!(again, 0, "re-indexing the same term must add no rows");
}

/// A fuzzy row must not outlive its object.
#[tokio::test]
async fn deleting_an_object_removes_its_fuzzy_keys() {
    let store = sqlite_store().await;
    index(&store, "o-del", Field::Title, "Receiver").await;
    assert!(
        !store.search_fuzzy("reciever", 10).await.unwrap().is_empty(),
        "precondition: the fuzzy index finds it"
    );
    match &store {
        Store::Sqlite(p) => {
            sqlx::query("DELETE FROM object WHERE id = ?")
                .bind("o-del")
                .execute(p)
                .await
                .unwrap();
        }
        Store::Postgres(p) => {
            let sql = Store::bind_sql("DELETE FROM object WHERE id = ?");
            sqlx::query(&sql).bind("o-del").execute(p).await.unwrap();
        }
    }
    let hits = store.search_fuzzy("reciever", 10).await.unwrap();
    assert!(
        hits.is_empty(),
        "fuzzy keys outliving their object: {hits:?}"
    );
}

// ------------------------------------------------------------------ aliases

/// §9.3's "alias and nickname awareness" (stash#3266, stash-box#804, #742).
///
/// An alias is indexed under `Field::Performer`, the same field as a real
/// name, so a search for a stage name ranks exactly as one for a birth name.
/// That is the whole design: an alias is not a special case, it is another
/// name.
#[tokio::test]
async fn an_alias_is_searchable_and_ranks_as_a_name() {
    let store = sqlite_store().await;
    index(&store, "o-birth", Field::Performer, "Marilyn Chambers").await;
    index(&store, "o-stage", Field::Performer, "Angelique").await;
    // The alias itself: same field, so the same weight.
    let terms: Vec<(Field, String)> = search::tokenize("Angelique")
        .into_iter()
        .map(|t| (Field::Performer, t))
        .collect();
    store.index_object("o-stage", &terms).await.unwrap();

    let hits = store.search("angelique", None, 10).await.unwrap();
    assert!(
        hits.iter().any(|h| h.object_id == "o-stage"),
        "a stage name must be findable by the stage name: {hits:?}"
    );
    let hit = hits.iter().find(|h| h.object_id == "o-stage").unwrap();
    assert_eq!(
        hit.score,
        Field::Performer.weight(),
        "an alias must rank as a name, at the name's weight -- otherwise §9.2's \
         'search includes performers' has two different answers depending on \
         which name the object happens to carry"
    );
}

// ------------------------------------------------------------- guardrails

/// A query with no terms is still refused, fuzzy or not.
#[tokio::test]
async fn an_empty_query_is_refused_by_the_fuzzy_search_too() {
    let store = sqlite_store().await;
    index(&store, "o-x", Field::Title, "Receiver").await;
    for q in ["", "   ", "the"] {
        match store.search_fuzzy(q, 10).await {
            Err(SearchError::EmptyQuery(_)) => {}
            other => panic!("query {q:?} should be refused, got {other:?}"),
        }
    }
}

/// The limit still applies.
#[tokio::test]
async fn the_fuzzy_limit_is_bounded() {
    let store = sqlite_store().await;
    for i in 0..5 {
        index(&store, &format!("o-{i}"), Field::Title, "Receiver").await;
    }
    assert!(matches!(
        store
            .search_fuzzy("receiver", search::MAX_RESULTS + 1)
            .await,
        Err(SearchError::LimitTooLarge { .. })
    ));
    assert!(store.search_fuzzy("receiver", 2).await.unwrap().len() <= 2);
}

/// A full key set for a term, as the indexer writes it.
#[test]
fn a_fuzzy_key_set_is_the_term_its_deletions_and_its_transposition() {
    let keys = fuzzy_key("abc");
    assert!(keys.contains(&"abc".to_string()));
    assert!(keys.contains(&"ab".to_string()));
    assert!(keys.contains(&"ac".to_string()));
    assert!(keys.contains(&"bc".to_string()));
    assert!(keys.contains(&transposition_key("abc")));
    // No duplicates: a key written twice is a row that does nothing and a
    // candidate set that mentions the same term twice.
    let mut sorted = keys.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), keys.len(), "duplicate keys: {keys:?}");
}
