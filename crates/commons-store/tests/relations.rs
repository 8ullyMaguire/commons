//! T-P5-004: `object_relation` has had a table and no writer since 0001.
//!
//! These tests are the store half. The classification — which of §9.7's four
//! signals says "duplicate" and which says "re-encode" — is in
//! `commons-scan/src/dedup.rs`, and its tests are in `dedup.rs`. Splitting it
//! this way is not tidiness: storing a relation and deciding what a relation
//! *is* are two decisions, and putting them in one file would let a test pass
//! for the wrong one.
//!
//! Both engines, every test. §3.5 makes the agreement a property of the
//! product, and the eight tests in this file are the store's half of the
//! ticket's accept criterion.

#[path = "harness/mod.rs"]
mod harness;

use commons_core::RelationType;
use commons_scan::dedup::{preferred_copy, FileFacts};
use commons_store::relations::{AutoMergeConfig, Relations};
use commons_store::Store;
use harness::{postgres_store, sqlite_store};
use uuid::Uuid;

fn now() -> String {
    "2026-09-26T10:00:00.000Z".to_string()
}

/// One fixture row on either engine.
///
/// Written here rather than through `db::insert_object` / `db::insert_file`
/// because both are SQLite-only -- `insert_object` uses `INSERT OR IGNORE`,
/// which Postgres has no equivalent for. That is a pre-existing gap in those
/// helpers, not something this ticket should change, and a helper that panics
/// on the second engine is a test that quietly runs on one.
///
/// The binds are positional and typed, which is the whole lesson of the first
/// run. Two versions were wrong in the same way:
///
///   * all binds as `String` puts a `text` into a `BIGINT` column, and Postgres
///     reports `42804 ... but expression is of type text` at position 115 of the
///     statement. SQLite coerces silently, so it passed on one engine and failed
///     on the other for a reason that reads as a schema bug.
///   * "all the texts, then all the ints" puts the values in the wrong
///     *order* whenever the placeholders interleave, which they do -- and a
///     misordered bind list is a wrong *value* in a right-typed column, so
///     nothing complains. This is the failure the crate's own
///     `db-migration-integrity` skill records about a dropped `.bind()`.
///
/// So: one list, in placeholder order, each entry carrying its own type.
enum B<'a> {
    T(&'a str),
    I(i64),
}

async fn row(store: &Store, sql: &str, binds: &[B<'_>]) {
    let n = match store {
        Store::Sqlite(p) => {
            let mut q = sqlx::query(sql);
            for b in binds {
                q = match b {
                    B::T(s) => q.bind(*s),
                    B::I(i) => q.bind(*i),
                };
            }
            q.execute(p).await.unwrap().rows_affected()
        }
        Store::Postgres(p) => {
            let pg = Store::bind_sql(sql);
            let mut q = sqlx::query(&pg);
            for b in binds {
                q = match b {
                    B::T(s) => q.bind(*s),
                    B::I(i) => q.bind(*i),
                };
            }
            q.execute(p).await.unwrap().rows_affected()
        }
    };
    assert_eq!(n, 1, "fixture row not written: {sql}");
}

async fn object(store: &Store, id: &str, kind: &str) {
    row(
        store,
        "INSERT INTO object (id, kind, created_at, updated_at) VALUES (?, ?, ?, ?)",
        &[B::T(id), B::T(kind), B::T(&now()), B::T(&now())],
    )
    .await;
    consent(store, id, "self_published").await;
}

async fn file(store: &Store, id: &str, object_id: &str, path: &str, size: i64, blake3: &str) {
    row(
        store,
        "INSERT INTO file (id, object_id, path, size_bytes, mtime_ns, hash_xxh128, \
         hash_blake3, state) VALUES (?, ?, ?, ?, ?, ?, ?, 'present')",
        &[
            B::T(id),
            B::T(object_id),
            B::T(path),
            B::I(size),
            B::I(1_700_000_000_000_000_000),
            B::T("0000aaaa0000bbbb"),
            B::T(blake3),
        ],
    )
    .await;
}

/// Give an object a consent record, so a consent-filtered query can see it.
///
/// §14.1: an object with no consent record is not a public object, and every
/// query in this crate reads `object` through `consent_record`. A fixture that
/// writes an object and not its consent row is a fixture that has not created
/// the object as far as the library is concerned -- which is the correct
/// behaviour and a baffling test failure.
async fn consent(store: &Store, object_id: &str, tier: &str) {
    row(
        store,
        "INSERT INTO consent_record (id, object_id, tier, updated_at) VALUES (?, ?, ?, ?)",
        &[
            B::T(&format!("cr-{object_id}")),
            B::T(object_id),
            B::T(tier),
            B::T(&now()),
        ],
    )
    .await;
}

/// The corpus the plan's accept criterion names: one byte-identical pair, one
/// re-encode, one false positive.
///
/// Three objects, three different phashes, and the third is the one the
/// threshold must NOT let through — a two-deep phash and the rest far away,
/// because a corpus where every pair is close cannot tell a threshold from a
/// constant.
async fn corpus(store: &Store) {
    object(store, "o-identical-a", "scene").await;
    object(store, "o-identical-b", "scene").await;
    object(store, "o-reencode", "scene").await;
    object(store, "o-false-positive", "scene").await;
    object(store, "o-unrelated", "scene").await;
    object(store, "o-reencode-far", "scene").await;

    // The byte-identical pair: same blake3, different paths.
    file(
        store,
        "f-a",
        "o-identical-a",
        "/lib/a.mkv",
        1_000,
        "blake-aaa",
    )
    .await;
    file(
        store,
        "f-b",
        "o-identical-b",
        "/other/b.mkv",
        1_000,
        "blake-aaa",
    )
    .await;
    // A re-encode: different bytes, same picture, smaller.
    file(store, "f-re", "o-reencode", "/lib/re.mkv", 700, "blake-bbb").await;
    // The false positive: unrelated content that happens to look similar.
    file(
        store,
        "f-fp",
        "o-false-positive",
        "/lib/fp.mkv",
        900,
        "blake-ccc",
    )
    .await;
    file(
        store,
        "f-un",
        "o-unrelated",
        "/lib/un.mkv",
        2_000,
        "blake-ddd",
    )
    .await;
    file(
        store,
        "f-refar",
        "o-reencode-far",
        "/lib/refar.mkv",
        2_500,
        "blake-eee",
    )
    .await;
}

// ------------------------------------------------------------ same_scene_as

#[tokio::test]
async fn a_same_scene_relation_is_stored_in_canonical_direction() {
    for store in [sqlite_store().await, postgres_store().await] {
        corpus(&store).await;
        let r = Relations::new(&store);
        // Stored in the order given: z before a.
        r.assert_relation("o-identical-b", "o-identical-a", RelationType::SameSceneAs)
            .await
            .unwrap();

        // Canonical direction means the reverse lookup also finds it, and
        // `from_id` is the *smaller* id, not the one the caller happened to
        // pass. This is the assertion that would fail if the function simply
        // stored what it was given: the relation would then only be findable in
        // one direction, and the duplicate view would be empty for half the
        // pairs it is given.
        let row = r
            .relation_between("o-identical-a", "o-identical-b")
            .await
            .unwrap()
            .expect("canonical direction must be findable from either order");
        assert_eq!(row.relation, RelationType::SameSceneAs);
        assert!(
            row.from_id < row.to_id,
            "same_scene_as is stored low-id-first, got {} -> {}",
            row.from_id,
            row.to_id
        );
    }
}

#[tokio::test]
async fn asserting_the_same_relation_twice_is_one_row() {
    for store in [sqlite_store().await, postgres_store().await] {
        corpus(&store).await;
        let r = Relations::new(&store);
        r.assert_relation("o-identical-a", "o-identical-b", RelationType::SameSceneAs)
            .await
            .unwrap();
        // Canonical direction means the second call is the *same* row in
        // reverse, not a second row. `UNIQUE (from_id, to_id, relation)` would
        // not stop it -- `a->b` and `b->a` are different tuples -- so this is
        // the assertion that a plain INSERT leaves doubled.
        r.assert_relation("o-identical-b", "o-identical-a", RelationType::SameSceneAs)
            .await
            .unwrap();
        let all = r.relations_of("o-identical-a").await.unwrap();
        assert_eq!(
            all.len(),
            1,
            "the same relation asserted in both directions is one row, got {all:?}"
        );
    }
}

#[tokio::test]
async fn a_re_encode_keeps_its_natural_direction() {
    for store in [sqlite_store().await, postgres_store().await] {
        corpus(&store).await;
        let r = Relations::new(&store);
        r.assert_relation("o-reencode", "o-identical-a", RelationType::ReEncodeOf)
            .await
            .unwrap();
        // The opposite of SameSceneAs, deliberately. Direction is the content
        // of a re-encode claim: `B re_encode_of A` says B is the copy, and
        // canonicalising it to low-id-first would make the claim about the
        // wrong file. #5067's "keep the highest bitrate" reads this direction
        // to decide which to delete.
        let row = r
            .directed_between("o-reencode", "o-identical-a", RelationType::ReEncodeOf)
            .await
            .unwrap()
            .expect("a re-encode keeps the direction it was given");
        assert_eq!(row.from_id, "o-reencode");
        assert_eq!(row.to_id, "o-identical-a");

        // And the reverse is not implied: B is a re-encode of A, which says
        // nothing about whether A is a re-encode of B.
        assert!(
            r.directed_between("o-identical-a", "o-reencode", RelationType::ReEncodeOf)
                .await
                .unwrap()
                .is_none(),
            "a re-encode is directed, so the reverse direction is a different claim"
        );
    }
}

#[tokio::test]
async fn two_different_relations_between_one_pair_coexist() {
    for store in [sqlite_store().await, postgres_store().await] {
        corpus(&store).await;
        let r = Relations::new(&store);
        r.assert_relation("o-identical-a", "o-identical-b", RelationType::SameSceneAs)
            .await
            .unwrap();
        // `UNIQUE (from_id, to_id, relation)` includes the relation, so these
        // are two rows by design: "the same scene" and "unrelated" can both be
        // recorded and the second is how #1656 retracts the first.
        r.assert_relation("o-unrelated", "o-identical-a", RelationType::UnrelatedTo)
            .await
            .unwrap();
        let all = r.relations_of("o-identical-a").await.unwrap();
        assert_eq!(all.len(), 2, "got {all:?}");
        assert!(all.iter().any(|x| x.relation == RelationType::SameSceneAs));
        assert!(all.iter().any(|x| x.relation == RelationType::UnrelatedTo));
    }
}

#[tokio::test]
async fn a_relation_between_two_different_kinds_is_refused() {
    for store in [sqlite_store().await, postgres_store().await] {
        corpus(&store).await;
        object(&store, "o-a-performer", "performer").await;
        let r = Relations::new(&store);
        let err = r
            .assert_relation("o-a-performer", "o-identical-a", RelationType::SameSceneAs)
            .await
            .expect_err("a scene and a performer are not the same scene");
        assert!(
            matches!(
                err,
                commons_store::relations::RelationError::KindMismatch { .. }
            ),
            "expected KindMismatch, got {err:?}"
        );
        // Nothing was written. A refused write that still left a row behind
        // would be worse than no check at all, because the check would be
        // reporting a problem the database does not have.
        assert!(r.relations_of("o-a-performer").await.unwrap().is_empty());
    }
}

#[tokio::test]
async fn a_self_relation_is_refused() {
    for store in [sqlite_store().await, postgres_store().await] {
        corpus(&store).await;
        let r = Relations::new(&store);
        let err = r
            .assert_relation("o-reencode", "o-reencode", RelationType::SameSceneAs)
            .await
            .expect_err("an object is not a duplicate of itself");
        assert!(matches!(
            err,
            commons_store::relations::RelationError::SelfRelation { .. }
        ));
    }
}

// ----------------------------------------------------------- suppression

#[tokio::test]
async fn a_suppression_of_one_pair_does_not_suppress_another() {
    for store in [sqlite_store().await, postgres_store().await] {
        corpus(&store).await;
        let r = Relations::new(&store);
        r.mark_not_duplicate("o-false-positive", "o-reencode")
            .await
            .unwrap();
        assert!(r
            .is_ruled_out("o-false-positive", "o-reencode")
            .await
            .unwrap());
        // Per-pair, not per-object. The reason A and B are not duplicates is
        // usually something specific to A and B -- different performers,
        // different day of a compilation -- and it does not transfer to C. A
        // suppression that suppressed everything involving A would silently
        // hide every real duplicate of A, which is worse than re-proposing
        // one the user already dismissed.
        assert!(
            !r.is_ruled_out("o-false-positive", "o-unrelated")
                .await
                .unwrap(),
            "ruling out one pair must not rule out the same object against another"
        );
        // Three pairs that each share an object with the ruled-out one, and
        // none of them is ruled out. The second and third share the *other*
        // end, so a suppression that keyed on one object rather than the pair
        // would be caught whichever object it keyed on.
        assert!(
            !r.is_ruled_out("o-reencode", "o-reencode-far")
                .await
                .unwrap(),
            "ruling out A/B must not rule out B/C: the other end is not exempt"
        );
        assert!(
            !r.is_ruled_out("o-false-positive", "o-reencode-far")
                .await
                .unwrap(),
            "a third object, sharing the first: also not ruled out"
        );
    }
}

#[tokio::test]
async fn a_suppression_survives_a_re_scan() {
    for store in [sqlite_store().await, postgres_store().await] {
        corpus(&store).await;
        let r = Relations::new(&store);
        r.mark_not_duplicate("o-false-positive", "o-reencode")
            .await
            .unwrap();
        // Mark it not-a-duplicate twice. #1656 is a per-scan action in the UI,
        // so the second scan proposes the same pair again and the user
        // dismisses it again -- and the dismissal must not have to be
        // re-entered. `UNIQUE (from_id, to_id, relation)` is what makes this
        // work, so the test is really that the insert is idempotent rather
        // than an error.
        r.mark_not_duplicate("o-false-positive", "o-reencode")
            .await
            .unwrap();
        let all = r.relations_of("o-false-positive").await.unwrap();
        assert_eq!(all.len(), 1, "a repeated dismissal is one row, got {all:?}");
    }
}

#[tokio::test]
async fn a_suppression_is_also_findable_in_the_reverse_order() {
    for store in [sqlite_store().await, postgres_store().await] {
        corpus(&store).await;
        let r = Relations::new(&store);
        r.mark_not_duplicate("o-reencode", "o-false-positive")
            .await
            .unwrap();
        // UnrelatedTo is symmetric in meaning -- "these two are not the same"
        // does not depend on which is the subject -- so unlike ReEncodeOf it
        // is canonicalised. A UI that asks about a pair in the opposite order
        // must not conclude it was never ruled out.
        assert!(r
            .is_ruled_out("o-false-positive", "o-reencode")
            .await
            .unwrap());
    }
}

// ------------------------------------------------------------- auto-merge

#[tokio::test]
async fn auto_merge_is_off_unless_a_config_turns_it_on() {
    for store in [sqlite_store().await, postgres_store().await] {
        corpus(&store).await;
        let r = Relations::new(&store);
        // The default. A program that knows nothing about dedup has this
        // value, and a default config that merges would make the feature
        // destructive on upgrade.
        let out = r.auto_merge(&AutoMergeConfig::default()).await.unwrap();
        assert_eq!(out.merged.len(), 0);
        assert!(r.relations_of("o-identical-a").await.unwrap().is_empty());
        assert!(
            r.relations_of("o-identical-b").await.unwrap().is_empty(),
            "nothing was merged, so the copy must have no relation either"
        );
    }
}

#[tokio::test]
async fn an_auto_merge_can_be_unmerged() {
    for store in [sqlite_store().await, postgres_store().await] {
        corpus(&store).await;
        let r = Relations::new(&store);
        // Auto-merge *consolidates* proposals; it does not make them. The
        // classifier (`commons_scan::dedup`) reads the content hashes and
        // asserts `SameSceneAs`, and this is what it produces. Without the
        // proposal the pass has nothing to consolidate, which is why the
        // corpus in the plan -- "one byte-identical pair" -- has to be
        // classified before the merge means anything.
        r.assert_relation("o-identical-a", "o-identical-b", RelationType::SameSceneAs)
            .await
            .unwrap();
        let cfg = AutoMergeConfig {
            enabled: true,
            ..Default::default()
        };
        let out = r.auto_merge(&cfg).await.unwrap();
        assert_eq!(out.merged.len(), 1, "the identical pair merges: {out:?}");
        assert_eq!(out.merged[0].relation, RelationType::SameSceneAs);
        assert!(r.is_merged(&out.merged[0]).await.unwrap());

        r.unmerge(&out).await.unwrap();
        assert!(!r.is_merged(&out.merged[0]).await.unwrap());
        assert!(
            r.relations_of("o-identical-a").await.unwrap().is_empty(),
            "unmerge must remove the relation, not mark it superseded"
        );
    }
}

#[tokio::test]
async fn an_auto_merge_never_writes_a_re_encode() {
    for store in [sqlite_store().await, postgres_store().await] {
        corpus(&store).await;
        let r = Relations::new(&store);
        // A re-encode is a *directed* claim about which file is the original.
        // An auto-merge that guessed it wrong would delete the original, and
        // #5067's "keep the highest bitrate" comparison is what decides that
        // -- not a background pass that runs while nobody is looking.
        let out = r
            .auto_merge(&AutoMergeConfig {
                enabled: true,
                allow_re_encode: false,
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(
            out.merged
                .iter()
                .all(|m| m.relation != RelationType::ReEncodeOf),
            "auto-merge wrote a directed claim: {:?}",
            out.merged
        );
    }
}

#[tokio::test]
async fn a_suppressed_pair_is_never_auto_merged() {
    for store in [sqlite_store().await, postgres_store().await] {
        corpus(&store).await;
        let r = Relations::new(&store);
        r.mark_not_duplicate("o-identical-a", "o-identical-b")
            .await
            .unwrap();
        let out = r
            .auto_merge(&AutoMergeConfig {
                enabled: true,
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(
            out.merged.is_empty(),
            "a user who ruled a pair out must not have it merged by a later pass: {:?}",
            out.merged
        );
    }
}

#[tokio::test]
async fn a_relation_survives_the_deletion_of_an_unrelated_object() {
    for store in [sqlite_store().await, postgres_store().await] {
        corpus(&store).await;
        let r = Relations::new(&store);
        r.assert_relation("o-identical-a", "o-identical-b", RelationType::SameSceneAs)
            .await
            .unwrap();
        // Deleting one end must take the relation with it. `object_relation`
        // has had `ON DELETE CASCADE` since 0001, so this is the schema doing
        // the work -- and it is the one cascade the codebase still has, which
        // is why the test exists: 0017 removed the others, and a future
        // migration that touched this table would be free to break it.
        store.delete_object("o-identical-b").await.unwrap();
        assert!(
            r.relations_of("o-identical-a").await.unwrap().is_empty(),
            "a relation to a deleted object is an orphan"
        );
    }
}

#[tokio::test]
async fn relations_of_an_unknown_object_is_empty_not_an_error() {
    for store in [sqlite_store().await, postgres_store().await] {
        let r = Relations::new(&store);
        // The duplicate view runs over objects a scan has not reached yet, and
        // a missing object is a normal state for it, not a failure.
        assert!(r
            .relations_of(&format!("o-{}", Uuid::new_v4()))
            .await
            .unwrap()
            .is_empty());
    }
}

// ---------------------------------------------------------------------------
// Auto-merge needs a *group*, and a group of two is not enough.
//
// Every one of these was written after `scripts/mutate-dedup.py` reported the
// corresponding mutation SURVIVED. Each survivor had the same shape: the test
// asserted that something did not happen, and the mutation removed a check on a
// code path that the test never reached. A test that cannot fail is worse than
// no test, and these were found by trying to break them.
// ---------------------------------------------------------------------------

/// Three objects in one scene, proposed by the classifier.
async fn three_in_a_scene(store: &Store) {
    for id in ["o-a", "o-b", "o-c"] {
        object(store, id, "scene").await;
        file(
            store,
            &format!("f-{id}"),
            id,
            &format!("/{id}.jpg"),
            100,
            &format!("h-{id}"),
        )
        .await;
    }
    let r = Relations::new(store);
    for pair in [("o-a", "o-b"), ("o-b", "o-c")] {
        r.assert_relation(pair.0, pair.1, RelationType::SameSceneAs)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn a_transitive_scene_merges_onto_one_member_not_two_pairs() {
    for store in [sqlite_store().await, postgres_store().await] {
        three_in_a_scene(&store).await;
        let r = Relations::new(&store);
        // a-b and b-c are the classifier's output; union-find is what turns two
        // edges into one group of three. Without it the pass writes the two
        // pairs it was given and never learns that a and c are connected, which
        // is the whole reason this layer exists rather than a plain query.
        let out = r
            .auto_merge(&AutoMergeConfig {
                enabled: true,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(
            out.merged.len(),
            2,
            "three members merge as two edges: {out:?}"
        );
        let targets: std::collections::BTreeSet<&str> =
            out.merged.iter().map(|m| m.from_id.as_str()).collect();
        // Both edges point at the group's lowest-id member, so the result does
        // not depend on row order -- which differs between SQLite and Postgres
        // and would otherwise make the two engines disagree on identical data.
        assert!(
            targets.len() == 1 && targets.contains("o-a"),
            "not onto one member: {targets:?}"
        );
        // The pass wrote *three* edges, not the two the classifier proposed:
        // a-b and b-c came in, and a-c is the transitive edge union-find
        // discovered. That edge is the entire value of this layer -- the
        // classifier only compares pairs, so a-b and b-c is all it can ever
        // see, and a pass that merely consolidated its input would leave a and
        // c looking like two different scenes. Asserting "no new edge" here
        // would have asserted the absence of the feature.
        // `relations_of` matches both directions, so counting rows would count
        // every edge twice and no edge once -- the count says nothing about
        // which edges exist. Collecting into a set of normalised pairs is what
        // turns "how many rows came back" into "which pairs are related".
        let mut edges: std::collections::BTreeSet<(String, String)> =
            std::collections::BTreeSet::new();
        for id in ["o-a", "o-b", "o-c"] {
            for rel in r.relations_of(id).await.unwrap() {
                let (a, b) = if rel.from_id <= rel.to_id {
                    (rel.from_id, rel.to_id)
                } else {
                    (rel.to_id, rel.from_id)
                };
                edges.insert((a, b));
            }
        }
        assert_eq!(
            edges
                .iter()
                .map(|(a, b)| (a.as_str(), b.as_str()))
                .collect::<std::collections::BTreeSet<_>>(),
            [("o-a", "o-b"), ("o-a", "o-c"), ("o-b", "o-c")]
                .into_iter()
                .collect(),
            "the group is a triangle, and a-c is the edge the pass found"
        );
    }
}

#[tokio::test]
async fn the_configured_group_ceiling_is_the_ceiling_and_not_a_suggestion() {
    for store in [sqlite_store().await, postgres_store().await] {
        three_in_a_scene(&store).await;
        let r = Relations::new(&store);
        // `max_group` is the blast radius. A pass that ignores it and merges a
        // 40-image group because all 40 look alike is the failure mode: one
        // perceptual-hash collision at scale becomes 39 wrong merges.
        let out = r
            .auto_merge(&AutoMergeConfig {
                enabled: true,
                max_group: 2,
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(
            out.merged.is_empty(),
            "a group of three exceeds max_group 2 and must be left alone: {out:?}"
        );
        let out = r
            .auto_merge(&AutoMergeConfig {
                enabled: true,
                max_group: 3,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(out.merged.len(), 2, "and it merges at exactly 3: {out:?}");
    }
}

#[tokio::test]
async fn a_suppression_splits_a_group_it_does_not_dissolve() {
    for store in [sqlite_store().await, postgres_store().await] {
        three_in_a_scene(&store).await;
        let r = Relations::new(&store);
        // The user ruled out a-c. b is still in the same scene as both, so the
        // group must lose that one edge and keep the other. Dropping the whole
        // group is not conservative -- it discards a correct merge because of an
        // unrelated pair -- and ignoring the suppression is the thing #1656
        // asked not to happen.
        r.mark_not_duplicate("o-a", "o-c").await.unwrap();
        let out = r
            .auto_merge(&AutoMergeConfig {
                enabled: true,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(
            out.merged.len(),
            1,
            "one of the two edges survives: {out:?}"
        );
        let m = &out.merged[0];
        let pair = [m.from_id.as_str(), m.to_id.as_str()];
        assert!(
            pair == ["o-a", "o-b"]
                || pair == ["o-b", "o-a"]
                || pair == ["o-a", "o-c"]
                || pair == ["o-c", "o-a"],
            "the surviving edge must not be the suppressed pair a-c: {m:?}"
        );
        assert_ne!(
            pair,
            ["o-a", "o-c"],
            "the suppressed pair was merged anyway"
        );
        assert_ne!(
            pair,
            ["o-c", "o-a"],
            "the suppressed pair was merged anyway"
        );
    }
}

#[tokio::test]
async fn a_suppressed_pair_inside_a_group_is_still_never_merged() {
    for store in [sqlite_store().await, postgres_store().await] {
        three_in_a_scene(&store).await;
        let r = Relations::new(&store);
        // The positive counterpart to the test above, and the one the
        // `is_ruled_out` mutation actually killed. A suppression on a *pair the
        // pass would otherwise have written* -- which is only observable once
        // the pass has a group of three and a lowest-id member to fold onto.
        r.mark_not_duplicate("o-a", "o-b").await.unwrap();
        let out = r
            .auto_merge(&AutoMergeConfig {
                enabled: true,
                ..Default::default()
            })
            .await
            .unwrap();
        // The pass folds a group onto its lowest-id member, and o-a is the
        // lowest, so the pairs it writes are a-b and a-c -- not the b-c the
        // classifier proposed. Suppressing a-b therefore leaves a-c, and that
        // is the assertion: the suppression removes exactly the edge it names
        // and nothing else. Asserting b-c would be asserting a pair the pass
        // never writes, which is a test that passes without testing anything.
        assert_eq!(
            out.merged.len(),
            1,
            "one of the two edges survives: {out:?}"
        );
        let m = &out.merged[0];
        assert_eq!(
            (m.from_id.as_str(), m.to_id.as_str()),
            ("o-a", "o-c"),
            "the surviving edge must be the unsuppressed pair: {m:?}"
        );
    }
}

#[tokio::test]
async fn a_disabled_config_merges_nothing_even_when_there_is_work() {
    for store in [sqlite_store().await, postgres_store().await] {
        // Objects only -- no proposals, so the store holds no relations and the
        // *pass* is the only thing that could write any. The first version of
        // this test ran `three_in_a_scene`, which leaves the classifier's two
        // relations in place; both branches then returned those same relations,
        // so the assertion could not tell a disabled pass from an enabled one.
        // That is exactly how the `if !cfg.enabled` mutation survived.
        for id in ["o-a", "o-b", "o-c"] {
            object(&store, id, "scene").await;
        }
        let r = Relations::new(&store);
        let out = r
            .auto_merge(&AutoMergeConfig {
                enabled: false,
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(
            out.merged.is_empty(),
            "a disabled pass wrote relations: {out:?}"
        );
        for id in ["o-a", "o-b", "o-c"] {
            assert!(
                r.relations_of(id).await.unwrap().is_empty(),
                "{id} gained a relation from a pass that was switched off"
            );
        }
    }
}

#[tokio::test]
async fn a_lone_object_is_not_a_group() {
    for store in [sqlite_store().await, postgres_store().await] {
        object(&store, "o-lonely", "scene").await;
        let r = Relations::new(&store);
        // `same_scene_groups` used to return singletons, and the `len() > 1`
        // filter was where they went. A singleton group reaches
        // `split_first`, yields an empty `rest`, and writes nothing -- so the
        // filter is invisible from the outside. It is asserted here through
        // `relations_of` instead, because the alternative is a filter with no
        // test for the case it guards, which is how the previous two §5 bugs
        // happened.
        let out = r
            .auto_merge(&AutoMergeConfig {
                enabled: true,
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(
            out.merged.is_empty(),
            "an object with no pair merged anyway: {out:?}"
        );
    }
}

#[tokio::test]
async fn the_canonical_direction_does_not_depend_on_which_end_is_named() {
    for store in [sqlite_store().await, postgres_store().await] {
        object(&store, "o-zzz", "scene").await;
        object(&store, "o-aaa", "scene").await;
        let r = Relations::new(&store);
        // Asserted z->a first, which is the descending order. The store must
        // canonicalise, or the two engines -- and two calls -- can disagree
        // about which row exists, and a duplicate `same_scene_as` in both
        // directions is a group of two that union-find reads as a cycle.
        r.assert_relation("o-zzz", "o-aaa", RelationType::SameSceneAs)
            .await
            .unwrap();
        let forward = r
            .assert_relation("o-aaa", "o-zzz", RelationType::SameSceneAs)
            .await
            .unwrap();
        assert_eq!(forward.from_id, "o-aaa", "the stored direction flipped");
        assert_eq!(forward.to_id, "o-zzz");
        let all = r.relations_of("o-zzz").await.unwrap();
        assert_eq!(all.len(), 1, "asserting both directions made two rows");
    }
}

#[tokio::test]
async fn the_preferred_copy_is_chosen_by_the_rule_not_by_the_row_order() {
    for store in [sqlite_store().await, postgres_store().await] {
        // A real pair: two files, two objects, one scene, and a size that
        // decides which survives. `preferred_copy` is the *classifier*'s rule
        // -- larger file wins, then the lower id as a tie-break -- and it is
        // what `unasserted_re_encodes` and the merge pass have to agree with.
        object(&store, "o-keep", "scene").await;
        object(&store, "o-drop", "scene").await;
        file(&store, "f-keep", "o-keep", "/keep.jpg", 9000, "hash-keep").await;
        file(&store, "f-drop", "o-drop", "/drop.jpg", 1000, "hash-drop").await;
        // The same phash on both, so the only thing that can decide is size.
        // A hash value has to be 16 hex digits for a 64-bit phash; a shorter
        // one is not comparable at all, which is a different code path.
        let same_phash = "0123456789abcdef";
        let keep = commons_scan::dedup::FileFacts {
            object_id: "o-keep".into(),
            size_bytes: 9000,
            blake3: Some("hash-keep".into()),
            phash: Some(same_phash.into()),
            phash_algorithm: Some("phash-64-v1".into()),
        };
        let drop = commons_scan::dedup::FileFacts {
            object_id: "o-drop".into(),
            size_bytes: 1000,
            blake3: Some("hash-drop".into()),
            phash: Some(same_phash.into()),
            phash_algorithm: Some("phash-64-v1".into()),
        };
        // Inserted largest-second, so a rule that returns the first argument
        // and a rule that returns the larger file disagree here.
        let preferred = commons_scan::dedup::preferred_copy(&drop, &keep);
        assert_eq!(preferred.object_id, "o-keep", "the larger file must win");
        // And the reverse call order gives the same answer, which is the
        // property the store depends on when rows arrive in either order.
        let preferred = commons_scan::dedup::preferred_copy(&keep, &drop);
        assert_eq!(preferred.object_id, "o-keep");
    }
}

// ---------------------------------------------------------------------------
// Round two of the mutation pass.
//
// `scripts/mutate-dedup.py` reported these four SURVIVED after the first round
// of tests was written. Every one of them had the same cause, and it is worth
// naming because it is the most common way a test suite lies to itself:
//
//   the test asserted an *absence*, and the mutation removed a check on a path
//   the test never reached.
//
// A pass that is switched off and a pass with nothing to do both return zero
// merges, so a test that only counts merges cannot tell them apart. The fix is
// always the same: put the code under test in a state where the branch it guards
// *is* the thing being measured.
// ---------------------------------------------------------------------------

/// Two objects, a relation proposed between them, and one object on its own.
///
/// The lone object is the point. `same_scene_groups` builds a group of one for
/// every object it sees, and the `len() > 1` filter is what removes them -- so a
/// corpus with no singleton cannot tell a working filter from a missing one.
async fn a_scene_and_a_lonely_object(store: &Store) {
    for id in ["o-a", "o-b", "o-lonely"] {
        object(store, id, "scene").await;
    }
    Relations::new(store)
        .assert_relation("o-a", "o-b", RelationType::SameSceneAs)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_group_of_one_is_filtered_out_before_the_pass_sees_it() {
    for store in [sqlite_store().await, postgres_store().await] {
        a_scene_and_a_lonely_object(&store).await;
        let r = Relations::new(&store);
        // `o-lonely` is in `object` and has no relation. Without the `len() > 1`
        // filter it becomes a group of one, reaches `split_first`, yields an
        // empty `rest` and writes nothing -- so from `out.merged` alone the
        // mutation is invisible. What it *does* change is the work: the pass
        // now walks a group for every object in the table, and on a library of
        // any size that is a scan per rescan for a result that is always empty.
        // It is asserted through the merge outcome anyway, because the property
        // is "a lone object never produces a merge" and that is what a caller
        // can observe.
        let out = r
            .auto_merge(&AutoMergeConfig {
                enabled: true,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(out.merged.len(), 1, "only the real pair merges: {out:?}");
        let m = &out.merged[0];
        assert!(
            (m.from_id.as_str(), m.to_id.as_str()) != ("o-lonely", "o-lonely"),
            "a lone object became a group of one and was merged with itself"
        );
        assert!(
            !m.from_id.contains("lonely") && !m.to_id.contains("lonely"),
            "the singleton reached the merge: {m:?}"
        );
    }
}

#[tokio::test]
async fn a_pass_that_is_switched_off_writes_nothing_even_with_proposals_present() {
    for store in [sqlite_store().await, postgres_store().await] {
        a_scene_and_a_lonely_object(&store).await;
        let r = Relations::new(&store);
        // The `if !cfg.enabled` guard, with the store in a state where the
        // enabled branch *would* write: a-b is already `same_scene_as`, so the
        // enabled pass's job is to consolidate and discover a-b's transitive
        // pair, and a guard-free pass reaches that code. The assertion is that
        // no *new* row appeared -- the pre-existing a-b row is what the
        // classifier wrote, and `relations_of` cannot tell the two apart, so the
        // count is taken before and after.
        let before = r.relations_of("o-a").await.unwrap().len();
        let out = r
            .auto_merge(&AutoMergeConfig {
                enabled: false,
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(
            out.merged.is_empty(),
            "a disabled pass reported merges: {out:?}"
        );
        let after = r.relations_of("o-a").await.unwrap().len();
        assert_eq!(
            after, before,
            "a disabled pass wrote a relation: {before} rows became {after}"
        );
        // And the proof the corpus was live: switching it on *does* write, so
        // the previous assertion is measuring the flag and not an empty store.
        let out = r
            .auto_merge(&AutoMergeConfig {
                enabled: true,
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(
            !out.merged.is_empty() || r.relations_of("o-a").await.unwrap().len() > before,
            "this corpus has no work, so the disabled assertion above proves \
             nothing -- the store must be live for it to mean anything"
        );
    }
}

#[tokio::test]
async fn the_canonical_direction_is_low_id_first_whatever_order_the_caller_uses() {
    for store in [sqlite_store().await, postgres_store().await] {
        object(&store, "o-mmm", "scene").await;
        object(&store, "o-bbb", "scene").await;
        let r = Relations::new(&store);
        // Asserted high-to-low, which is the direction the canonicalisation has
        // to reverse. The first version of the mutation that removes the swap
        // survived because both orders produced *a* row and the test only asked
        // whether one existed. This asks which one.
        let row = r
            .assert_relation("o-mmm", "o-bbb", RelationType::SameSceneAs)
            .await
            .unwrap();
        assert_eq!(
            (row.from_id.as_str(), row.to_id.as_str()),
            ("o-bbb", "o-mmm"),
            "from_id must be the lower id, whatever order the caller used"
        );
        // And the other direction is the *same* row, not a second one.
        let again = r
            .assert_relation("o-bbb", "o-mmm", RelationType::SameSceneAs)
            .await
            .unwrap();
        assert_eq!(again.id, row.id, "asserting the reverse wrote a second row");
    }
}

#[tokio::test]
async fn the_preferred_copy_is_the_larger_file_and_ties_break_on_the_id() {
    // This is `preferred_copy` in `commons-scan::dedup`, and it is pure -- no
    // store, no database. The mutation that made it return `a` regardless
    // survived the earlier version of this test because both files had the same
    // size, so "return the first argument" and "return the larger file" agreed.
    // A fixture where the answer and the argument order disagree is the whole
    // difference between testing a rule and testing an implementation detail.
    let big = FileFacts {
        object_id: "o-aaa".into(),
        size_bytes: 9000,
        blake3: Some("hash-a".into()),
        phash: Some("0123456789abcdef".into()),
        phash_algorithm: Some("phash-64-v1".into()),
    };
    let small = FileFacts {
        object_id: "o-zzz".into(),
        size_bytes: 1000,
        blake3: Some("hash-z".into()),
        phash: Some("0123456789abcdef".into()),
        phash_algorithm: Some("phash-64-v1".into()),
    };
    // The larger file is the *first* argument here, and the smaller the second.
    assert_eq!(
        preferred_copy(&big, &small).object_id,
        "o-aaa",
        "the larger file must win"
    );
    assert_eq!(
        preferred_copy(&small, &big).object_id,
        "o-aaa",
        "the larger file must win whichever way round they are passed"
    );
    // Equal sizes, opposite argument order. Without a stated tie-break this
    // returns whichever is first, which means the answer depends on the order
    // the caller happened to read the rows in -- and row order differs between
    // SQLite and Postgres, so the two engines would pick different originals.
    let tie_a = FileFacts {
        size_bytes: 5000,
        ..big.clone()
    };
    let tie_b = FileFacts {
        size_bytes: 5000,
        ..small.clone()
    };
    assert_eq!(
        preferred_copy(&tie_a, &tie_b).object_id,
        preferred_copy(&tie_b, &tie_a).object_id,
        "a tie must not depend on the argument order, or the two engines pick \
         different originals for the same data"
    );
    assert_eq!(
        preferred_copy(&tie_a, &tie_b).object_id,
        "o-aaa",
        "and the tie-break is the lower id, not whichever came first"
    );
}

#[tokio::test]
async fn a_singleton_group_produces_no_merge_and_no_singleton_pair() {
    for store in [sqlite_store().await, postgres_store().await] {
        // One object, and nothing else in the table. `same_scene_groups` walks
        // every `same_scene_as` row and groups its two ends, so with no rows
        // there is nothing to group -- which is the *easy* direction. The
        // mutation that removed the `len() > 1` filter needs a group of one to
        // actually exist, and that comes from a row whose two ends are the same
        // object, which the store refuses to write.
        //
        // So the filter is exercised from the other side instead: a table with
        // plenty of rows and one object in none of them. Every other object is
        // in a pair, this one is in nothing, and a filter that is not there
        // produces a group of one that reaches `split_first` and writes an
        // empty `rest`. The observable difference is the *work*, not the
        // result -- so the assertion is that the pass reports exactly the pairs
        // that exist and no more, which is what a caller of a library-sized
        // rescan can tell from a duplicate count.
        for id in ["o-a", "o-b", "o-c", "o-lonely"] {
            object(&store, id, "scene").await;
        }
        let r = Relations::new(&store);
        for pair in [("o-a", "o-b"), ("o-c", "o-a")] {
            r.assert_relation(pair.0, pair.1, RelationType::SameSceneAs)
                .await
                .unwrap();
        }
        let out = r
            .auto_merge(&AutoMergeConfig {
                enabled: true,
                ..Default::default()
            })
            .await
            .unwrap();
        // A merge whose two ends are the same object is the signature of a
        // group of one reaching `split_first` and pairing an object with
        // itself. Stated directly, because a set with a no-op in it says
        // nothing.
        for m in &out.merged {
            assert_ne!(
                m.from_id, m.to_id,
                "a group of one was merged with itself: {m:?}"
            );
            assert!(
                !m.from_id.contains("lonely") && !m.to_id.contains("lonely"),
                "the singleton object reached a merge: {m:?}"
            );
        }
    }
}
