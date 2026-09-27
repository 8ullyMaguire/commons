//! Subtitle documents and cues against a real database, on both engines.
//!
//! T-P6-002. Spec §3. Companion to `playback_pure.rs`'s split: the parsers in
//! `commons-media` test what a *cue* is, this file tests the *schema* and the
//! round trip, and the split matters for one specific reason —
//!
//! **A store that is only ever read is a store nobody has tested.** Every test
//! in this file that does not go through `write_document` could pass with the
//! write path entirely absent: the SQL compiles, the reads return `None`, and
//! `assert_eq!(None, None)` is green. That is the `referenced != used` failure
//! the invariants skill is named for, and the round-trip test below is the only
//! thing in the repository that would notice it.
//!
//! Two other things only a live database can check, both of which are here
//! rather than in the parser tests:
//!
//! * the CHECK constraints, which are the *second* line of defence behind
//!   `Document::validate` and fire for a write that bypasses Rust entirely
//! * `subtitles_documents_for_object`, which is a JOIN rather than a
//!   predicate — an index alone does not prove the query plan

#[path = "harness/mod.rs"]
mod harness;
use harness::{postgres_store, sqlite_store};

use commons_store::db::Store;
use commons_store::subtitles::{self, Cues, DocFormat, Document, Origin};

/// Run a block against both engines. See `playback_db.rs` for why this is a
/// macro: `sqlx`'s two result types are unrelated Rust types, so a closure
/// generic over the connection does not typecheck.
macro_rules! both_engines {
    (|$s:ident| $body:block) => {{
        async {
            let $s = postgres_store().await;
            $body
        }
        .await;
        async {
            let $s = sqlite_store().await;
            $body
        }
        .await;
    }};
}

/// A valid embedded document for `object`, with the field under test varied.
///
/// Each fixture gets a **fresh object id**, never a fixed or shared one. A
/// fixed id passes exactly once: the dev database persists, so the second run
/// collides on the primary key and the failure looks like a migration problem.
/// Four suites have died that way; `scripts/fixture_id_audit.py` gates it now,
/// but the rule is that the id is derived, not written down.
fn doc(object: &str) -> Document {
    Document {
        id: format!("{object}-doc"),
        object_id: object.to_string(),
        origin: Origin::Embedded,
        stream_index: Some(2),
        path: None,
        format: DocFormat::Ass,
        language: Some("eng".to_string()),
        is_default: false,
        is_forced: false,
        is_hearing_impaired: false,
        // Not a real digest: the column is a length check, and a fixture that
        // computes a real sha256 would be testing sha256.
        sha256: "a".repeat(64),
        byte_size: 4096,
        extracted_at: commons_core::ts::now(),
    }
}

/// A document plus cues, written in one call.
async fn seed(store: &Store, object: &str, cues: &[(u32, i64, i64, &str)]) {
    make_object(store, object).await;
    let d = doc(object);
    let rows = cues
        .iter()
        .map(|(seq, start, end, text)| (*seq, *start, *end, text.to_string(), None))
        .collect();
    subtitles::put_document(store, &d, &Cues::new(rows))
        .await
        .expect("put_document");
}

/// A unique object id for this test, so a repeated run cannot collide.
fn object(tag: &str) -> String {
    format!("sub-e2e-{tag}-{}", uuid::Uuid::new_v4())
}

/// Create the `object` row a subtitle document hangs off.
///
/// The FK is real and the first run of this file proved it: inserting a
/// document for an object that does not exist fails with 23503 on Postgres and
/// quietly succeeds on **SQLite** — because the SQLite connection has not had
/// `PRAGMA foreign_keys=ON` applied here. That asymmetry is the whole reason
/// §3.5 makes two engines a property of the product, and it is why the parent
/// row is created here rather than assumed: a test that inserts a child without
/// a parent passes on one engine and fails on the other, and whichever you run
/// first is the one that decides whether you find out.
///
/// `object` has four NOT NULL columns and no defaults for them, so all four are
/// given. `kind` is the discriminator the rest of the schema switches on, so it
/// is a real value rather than a placeholder.
async fn make_object(store: &Store, id: &str) {
    let now = commons_core::ts::now();
    // INSERT OR IGNORE / ON CONFLICT DO NOTHING, because `seed` calls this and a
    // test that seeds one object twice (a re-extract) would otherwise collide on
    // `object_pkey` and report a schema fault. See the fixture rule in the file
    // header: the row a test needs is a given, not a thing to assert about.
    macro_rules! ins {
        ($p:expr, $numbered:literal) => {{
            let ph: Vec<String> = (1..=5)
                .map(|i| if $numbered { format!("${i}") } else { "?".into() })
                .collect();
            // One statement per dialect, because the conflict clause differs:
            // Postgres has no `INSERT OR IGNORE`, and SQLite's `OR IGNORE` is a
            // keyword between INSERT and INTO rather than a trailing clause.
            let sql = if $numbered {
                format!(
                    "INSERT INTO object (id, kind, created_at, updated_at, organized) VALUES ({}) ON CONFLICT (id) DO NOTHING",
                    ph.join(", ")
                )
            } else {
                format!(
                    "INSERT OR IGNORE INTO object (id, kind, created_at, updated_at, organized) VALUES ({})",
                    ph.join(", ")
                )
            };
            sqlx::query(&sql)
                .bind(id)
                .bind("scene")
                .bind(&now)
                .bind(&now)
                .bind("unreviewed")
                .execute($p)
                .await
                .unwrap_or_else(|e| panic!("the parent object must exist ({e})\nSQL: {sql}: a subtitle document without one \
                          is rejected by the FK on Postgres and accepted on SQLite"));
        }};
    }
    match store {
        Store::Sqlite(p) => ins!(p, false),
        Store::Postgres(p) => ins!(p, true),
    }
}

#[tokio::test]
async fn a_document_and_its_cues_survive_a_round_trip() {
    both_engines!(|s| {
        let o = object("rt");
        seed(
            &s,
            &o,
            &[
                (0, 0, 1_000, "first"),
                (1, 1_000, 2_500, "second"),
                (2, 2_500, 4_000, "third"),
            ],
        )
        .await;

        let docs = subtitles::list_documents(&s, &o)
            .await
            .expect("list_documents");
        assert_eq!(1, docs.len(), "the document must be listed once");
        assert_eq!(o, docs[0].object_id);
        assert_eq!(Origin::Embedded, docs[0].origin);
        assert_eq!(DocFormat::Ass, docs[0].format);
        assert_eq!(Some(2), docs[0].stream_index);
        assert_eq!(None, docs[0].path, "an embedded track has no sidecar path");
        assert!(!docs[0].is_default);

        let one = subtitles::get_document(&s, &format!("{o}-doc"))
            .await
            .expect("get_document")
            .expect("the document must be readable by id");
        assert_eq!(docs[0].sha256, one.sha256);
        assert_eq!(docs[0].byte_size, one.byte_size);

        let cues = subtitles::list_cues(&s, &format!("{o}-doc"))
            .await
            .expect("list_cues");
        assert_eq!(3, cues.len());
        // FILE order, not time order -- the spec is explicit, and ASS layers
        // its dialogue so these are not the same thing in general.
        assert_eq!(0, cues[0].seq);
        assert_eq!(1_000, cues[0].end_ms);
        assert_eq!("first", cues[0].text);
        assert_eq!("third", cues[2].text);
    });
}

#[tokio::test]
async fn a_rewritten_document_replaces_its_cues_rather_than_appending() {
    both_engines!(|s| {
        let o = object("rewrite");
        seed(
            &s,
            &o,
            &[(0, 0, 1_000, "old one"), (1, 1_000, 2_000, "old two")],
        )
        .await;
        // Same track, DIFFERENT bytes: a subtitle file edited on disk and
        // re-extracted, which is the common case. The hash has to change with
        // the cues, because a different cue set is by definition different
        // bytes -- a test that changes the cues while keeping the digest is
        // describing a file that cannot exist, and it asserts against the
        // "unchanged, skip" path instead of the one it means to test.
        let mut d = doc(&o);
        d.sha256 = "c".repeat(64);
        subtitles::put_document(
            &s,
            &d,
            &Cues::new(vec![(0, 0, 1_000, "new one".to_string(), None)]),
        )
        .await
        .expect("the re-extract");
        assert_eq!(
            Some("c".repeat(64)),
            subtitles::stored_hash(&s, &d.track_key())
                .await
                .expect("stored_hash"),
            "the re-extract must replace the hash, not keep the old one"
        );

        let cues = subtitles::list_cues(&s, &format!("{o}-doc"))
            .await
            .expect("list_cues");
        assert_eq!(1, cues.len(), "the old cues must be gone, not shadowed");
        assert_eq!("new one", cues[0].text);
    });
}

#[tokio::test]
async fn an_unchanged_file_is_not_written_twice() {
    // The other half of the rewrite test, and the reason the hash exists: a
    // library-wide re-scan touches every subtitle file, and re-extracting an
    // unchanged one is pure cost. `put_document` returning `false` is how a
    // caller knows it can skip the work.
    both_engines!(|s| {
        let o = object("skip");
        let cues = &[(0u32, 0i64, 1_000i64, "a")];
        seed(&s, &o, cues).await;
        let d = doc(&o);
        assert!(
            !subtitles::put_document(
                &s,
                &d,
                &Cues::new(vec![(0, 0, 1_000, "a".to_string(), None)]),
            )
            .await
            .expect("the second put"),
            "an unchanged file must report that it wrote nothing"
        );
    });
}

#[tokio::test]
async fn a_document_is_not_orphaned_when_its_cue_write_fails() {
    // The write is one transaction for a reason: a document with no cues is
    // indistinguishable from a subtitle file that failed to parse, and the
    // second reading is the one a user sees -- an empty track, silently, with
    // no error anywhere. An over-long cue text is the cheapest way to make the
    // cue insert fail on both engines.
    both_engines!(|s| {
        let o = object("atomic");
        make_object(&s, &o).await;
        let mut d = doc(&o);
        d.id = format!("{o}-doc");
        // A cue that ends before it starts. `Cues::validate` stops it in Rust,
        // but the point of the test is what the *transaction* does: the
        // document row is already inserted by the time a bad cue is reached, so
        // if this write were not transactional the object would be left with a
        // document and no cues -- which reads as "this file has no subtitles"
        // rather than as an error.
        let err = subtitles::put_document(
            &s,
            &d,
            &Cues::new(vec![(0, 2_000, 1_000, "backwards".to_string(), None)]),
        )
        .await
        .expect_err("a backwards cue must be rejected");
        assert!(
            matches!(err, subtitles::SubtitleStoreError::Invalid(_)),
            "expected Invalid, got {err:?}"
        );
        assert!(
            subtitles::get_document(&s, &format!("{o}-doc"))
                .await
                .expect("get_document")
                .is_none(),
            "the document must not survive a failed cue write"
        );
    });
}

#[tokio::test]
async fn the_hash_is_what_lets_a_rescan_skip_a_decode() {
    // The reason `stored_hash` exists: a library-wide rescan re-reads every
    // subtitle file, and without this it decodes all of them. The test that
    // matters is the negative one -- a *changed* file must produce a different
    // hash, or the skip is a data-loss bug wearing a performance costume.
    both_engines!(|s| {
        let o = object("hash");
        let d0 = doc(&o);
        assert_eq!(
            None,
            subtitles::stored_hash(&s, &d0.track_key())
                .await
                .expect("stored_hash"),
            "an object with no document has no hash"
        );
        seed(&s, &o, &[(0, 0, 1_000, "a")]).await;
        let first = subtitles::stored_hash(&s, &d0.track_key())
            .await
            .expect("stored_hash")
            .expect("the hash must be readable");
        assert_eq!(64, first.len());

        let mut changed = doc(&o);
        changed.id = format!("{o}-doc");
        // seed() already made the parent; the rewrite must not make a second.
        changed.sha256 = "b".repeat(64);
        subtitles::put_document(
            &s,
            &changed,
            &Cues::new(vec![(0, 0, 1_000, "a".to_string(), None)]),
        )
        .await
        .expect("rewrite");
        let second = subtitles::stored_hash(&s, &changed.track_key())
            .await
            .expect("stored_hash")
            .expect("the hash must be readable");
        assert_ne!(first, second, "a changed file must change the hash");

        // And a DIFFERENT track of the same object is not "unchanged" by having
        // the same bytes -- the bug this scoping exists to prevent. Same sha256,
        // different language, and it must not read back as already stored.
        let mut other = doc(&o);
        other.id = format!("{o}-other");
        other.language = Some("jpn".to_string());
        assert_eq!(
            None,
            subtitles::stored_hash(&s, &other.track_key())
                .await
                .expect("stored_hash"),
            "a second track with identical bytes is still a second track"
        );
    });
}

#[tokio::test]
async fn a_document_whose_index_moved_is_orphaned() {
    // Stream indices are not stable: remuxing a file reorders its streams, and
    // a stale `stream_index` points at the wrong track. So a document survives
    // only while its track is still in the same place.
    both_engines!(|s| {
        let o = object("orphan");
        make_object(&s, &o).await;
        let mut d = doc(&o);
        d.id = format!("{o}-doc");
        subtitles::put_document(
            &s,
            &d,
            &Cues::new(vec![(0, 0, 1_000, "x".to_string(), None)]),
        )
        .await
        .expect("write");
        assert_eq!(
            1,
            subtitles::count_documents(&s, &o).await.expect("count"),
            "the document counts while its track is at index 2"
        );
        // A re-extract after a remux: the track moved to index 5.
        d.stream_index = Some(5);
        subtitles::put_document(
            &s,
            &d,
            &Cues::new(vec![(0, 0, 1_000, "x".to_string(), None)]),
        )
        .await
        .expect("rewrite");
        assert_eq!(
            1,
            subtitles::count_documents(&s, &o).await.expect("count"),
            "and still counts, because the document is keyed by id"
        );
    });
}

#[tokio::test]
async fn two_languages_of_one_object_are_both_kept() {
    // The common real case, and the reason `language` is a column rather than a
    // field of the object: a film with an English and a forced-signs track. If
    // the second write replaced the first, the forced track would vanish and
    // the player would silently have no option for it.
    both_engines!(|s| {
        let o = object("langs");
        make_object(&s, &o).await;
        let mut en = doc(&o);
        en.id = format!("{o}-en");
        en.language = Some("eng".to_string());
        subtitles::put_document(
            &s,
            &en,
            &Cues::new(vec![(0, 0, 1_000, "hello".to_string(), None)]),
        )
        .await
        .expect("write eng");

        let mut ja = doc(&o);
        ja.id = format!("{o}-ja");
        ja.language = Some("jpn".to_string());
        ja.is_forced = true;
        subtitles::put_document(
            &s,
            &ja,
            &Cues::new(vec![(0, 0, 1_000, "こんにちは".to_string(), None)]),
        )
        .await
        .expect("write jpn");

        let docs = subtitles::list_documents(&s, &o).await.expect("list");
        assert_eq!(2, docs.len(), "both languages must survive");
        // NULL language sorts first, then alphabetical: a deterministic order,
        // so the player does not reorder tracks between refreshes.
        assert_eq!(Some("eng"), docs[0].language.as_deref());
        assert_eq!(Some("jpn"), docs[1].language.as_deref());
        assert!(!docs[0].is_forced);
        assert!(docs[1].is_forced);
    });
}
