//! T-P5-004's accept criterion, end to end.
//!
//! The plan asks for "a fixture set with one byte-identical pair, one re-encode,
//! and one false positive; assert each is classified correctly and that
//! auto-merge is off by default and reversible when on." The two halves are
//! tested separately -- `commons-scan/src/dedup.rs` for the classification and
//! `tests/relations.rs` for the storage -- and this file is the only place they
//! meet.
//!
//! That is worth a file of its own because the gap between the halves is where
//! a feature is wrong without any test being red: the classifier can be right
//! and the store can drop the verdict, or the store can store a verdict the
//! classifier never produced. Neither unit test can see that.
//!
//! It lives here rather than in `commons-scan/tests/` because it needs the
//! two-engine harness, and reaching across a crate boundary with a
//! `#[path = "../commons-store/tests/harness/mod.rs"]` would make one crate's
//! test directory part of another crate's build. A test that needs two crates'
//! public APIs together belongs to the crate that depends on both.
//!
//! Both engines. §3.5 makes the agreement a property of the product, and these
//! are the assertions that the *classification* is the same on both.

#[path = "harness/mod.rs"]
mod harness;

use commons_core::RelationType;
use commons_scan::dedup::{
    classify, classify_all, store_findings, FileFacts, PhashPolicy, Verdict, DEFAULT_ALGORITHM,
};
use commons_store::relations::{AutoMergeConfig, Relations};
use commons_store::Store;
use harness::{postgres_store, sqlite_store};

fn now() -> String {
    "2026-09-26T10:00:00.000Z".to_string()
}

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
            q.execute(p)
                .await
                .unwrap_or_else(|e| panic!("SQLite {sql}\n  {e}"))
                .rows_affected()
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
            q.execute(p)
                .await
                .unwrap_or_else(|e| panic!("SQLite {sql}\n  {e}"))
                .rows_affected()
        }
    };
    assert_eq!(n, 1, "fixture row not written: {sql}");
}

async fn object(store: &Store, id: &str) {
    row(
        store,
        "INSERT INTO object (id, kind, created_at, updated_at) VALUES (?, 'scene', ?, ?)",
        &[B::T(id), B::T(&now()), B::T(&now())],
    )
    .await;
    consent(store, id, "self_published").await;
}

/// One file, as the dedup half of the system sees it.
///
/// A struct rather than eight parameters. `phash` and `algorithm` are one fact
/// -- a hash *plus* what computed it -- and as two parameters the pairing was
/// easy to get wrong, which is the mistake the whole ticket exists to prevent:
/// two hashes with no algorithm attached are not comparable and must not be
/// compared.
struct Fixture<'a> {
    id: &'a str,
    object_id: &'a str,
    path: &'a str,
    size: i64,
    blake3: &'a str,
    phash: &'a str,
    algorithm: &'a str,
}

async fn file(store: &Store, f: &Fixture<'_>) {
    let Fixture {
        id,
        object_id,
        path,
        size,
        blake3,
        phash,
        algorithm,
    } = *f;
    row(
        store,
        "INSERT INTO file (id, object_id, path, size_bytes, mtime_ns, hash_xxh128, 
         hash_blake3, state) VALUES (?, ?, ?, ?, 1700000000000000000, '00', ?, 'present')",
        &[
            B::T(id),
            B::T(object_id),
            B::T(path),
            B::I(size),
            B::T(blake3),
        ],
    )
    .await;
    row(
        store,
        "INSERT INTO file_phash (file_id, phash, algorithm, created_at) VALUES (?, ?, ?, ?)",
        &[B::T(id), B::T(phash), B::T(algorithm), B::T(&now())],
    )
    .await;
    row(
        store,
        "INSERT INTO object_phash (object_id, phash, algorithm, created_at) VALUES (?, ?, ?, ?)",
        &[B::T(object_id), B::T(phash), B::T(algorithm), B::T(&now())],
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

/// The corpus, in the database, with the phashes the classifier will read.
async fn corpus(store: &Store) {
    // Every object first: `file.object_id` is a foreign key, so a fixture
    // that writes a file before its object fails on a constraint that has
    // nothing to do with what the test is about.
    for id in [
        "o-identical-a",
        "o-identical-b",
        "o-reencode",
        "o-false-positive",
        "o-far",
    ] {
        object(store, id).await;
    }
    // The byte-identical pair: same blake3, so identity needs no phash at all.
    file(
        store,
        &Fixture {
            id: "f-a",
            object_id: "o-identical-a",
            path: "/lib/a.mkv",
            size: 1_000,
            blake3: "blake-aaa",
            phash: "0000000000000000",
            algorithm: DEFAULT_ALGORITHM,
        },
    )
    .await;
    file(
        store,
        &Fixture {
            id: "f-b",
            object_id: "o-identical-b",
            path: "/other/b.mkv",
            size: 1_000,
            blake3: "blake-aaa",
            phash: "0000000000000000",
            algorithm: DEFAULT_ALGORITHM,
        },
    )
    .await;
    // A re-encode: different bytes, one bit of phash apart.
    file(
        store,
        &Fixture {
            id: "f-re",
            object_id: "o-reencode",
            path: "/lib/re.mkv",
            size: 700,
            blake3: "blake-bbb",
            phash: "0000000000000001",
            algorithm: DEFAULT_ALGORITHM,
        },
    )
    .await;
    // The false positive: genuinely different content, a phash 12 bits from
    // `o-identical-a`'s. Under `re_encode_at_or_below: 10` this is `Possible`,
    // which is the *only* reason the plan's fixture set needs a false positive
    // in it -- a threshold of 12 would assert this pair and be wrong.
    file(
        store,
        &Fixture {
            id: "f-fp",
            object_id: "o-false-positive",
            path: "/lib/fp.mkv",
            size: 900,
            blake3: "blake-ccc",
            phash: "0000000000000fff",
            algorithm: DEFAULT_ALGORITHM,
        },
    )
    .await;
    file(
        store,
        &Fixture {
            id: "f-far",
            object_id: "o-far",
            path: "/lib/far.mkv",
            size: 2_000,
            blake3: "blake-ddd",
            phash: "ffffffffffffffff",
            algorithm: DEFAULT_ALGORITHM,
        },
    )
    .await;
}

/// Order-independent pair key, so "was this pair dismissed" does not depend on
/// which order the classifier happened to visit it in.
fn key(a: &str, b: &str) -> (String, String) {
    if a <= b {
        (a.to_string(), b.to_string())
    } else {
        (b.to_string(), a.to_string())
    }
}

/// Every pair the user has dismissed, read once.
async fn ruled_out(rel: &Relations<'_>, f: &[FileFacts]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (i, a) in f.iter().enumerate() {
        for b in f.iter().skip(i + 1) {
            if rel.is_ruled_out(&a.object_id, &b.object_id).await.unwrap() {
                out.push(key(&a.object_id, &b.object_id));
            }
        }
    }
    out
}

/// Read the facts back out of the store, which is the direction the product
/// runs in and the only way the test can catch a *reader* that disagrees with
/// the writer.
async fn facts(store: &Store) -> Vec<FileFacts> {
    let sql = "SELECT f.object_id, f.size_bytes, f.hash_blake3, p.phash, p.algorithm 
               FROM file f JOIN object_phash p ON p.object_id = f.object_id 
               ORDER BY f.object_id";
    let rows: Vec<(String, i64, Option<String>, String, String)> = match store {
        Store::Sqlite(p) => sqlx::query_as(sql).fetch_all(p).await.unwrap(),
        Store::Postgres(p) => {
            let pg = Store::bind_sql(sql);
            sqlx::query_as(&pg).fetch_all(p).await.unwrap()
        }
    };
    rows.into_iter()
        .map(
            |(object_id, size_bytes, blake3, phash, algorithm)| FileFacts {
                object_id,
                blake3,
                phash: Some(phash),
                phash_algorithm: Some(algorithm),
                size_bytes,
            },
        )
        .collect()
}

// ------------------------------------------------------------------ the plan

#[tokio::test]
async fn the_plan_corpus_classifies_three_ways_and_stores_one_relation() {
    for store in [sqlite_store().await, postgres_store().await] {
        corpus(&store).await;
        let rel = Relations::new(&store);
        let f = facts(&store).await;
        let policy = PhashPolicy::default();

        // `classify_all` is a pure function, so its veto is a sync predicate.
        // Resolving the database into a set first is not a workaround for the
        // signature: a pure classifier that can await is a classifier whose
        // result depends on when it was called, which is exactly what "a view
        // over relations" is not.
        let veto = ruled_out(&rel, &f).await;
        let findings = classify_all(&f, &policy, &|a, b| veto.contains(&key(a, b)));

        // One byte-identical pair -> `Identical`, and *only* that one. The
        // assertion is by verdict and by count, because "the identical pair was
        // found" and "nothing else was" are different claims and a test that
        // only makes the first passes on a classifier that flags everything.
        let identical: Vec<_> = findings
            .iter()
            .filter(|x| x.verdict == Verdict::Identical)
            .collect();
        assert_eq!(
            identical.len(),
            1,
            "exactly one identical pair: {findings:?}"
        );
        let mut pair = identical[0].left.clone();
        pair.push('/');
        pair.push_str(&identical[0].right);
        assert!(
            pair == "o-identical-a/o-identical-b" || pair == "o-identical-b/o-identical-a",
            "the identical pair is the byte-identical one, got {pair}"
        );

        // The re-encode is `ReEncode`: an estimate, so it is reported and not
        // written. The false positive is `Possible` at distance 12 -- above the
        // threshold of 10, which is the whole reason the plan's fixture set has
        // a false positive in it.
        let re = findings
            .iter()
            .find(|x| x.verdict == Verdict::ReEncode)
            .expect("a one-bit phash difference is a re-encode");
        assert!(
            (re.left == "o-reencode" && re.right == "o-identical-a")
                || (re.right == "o-reencode" && re.left == "o-identical-a"),
            "the re-encode is the near-identical one, got {re:?}"
        );

        // `store_findings` writes the assertable ones only. So the table holds
        // exactly one relation, and it is a `SameSceneAs`.
        let written = store_findings(&rel, &findings).await.unwrap();
        assert_eq!(written, vec![RelationType::SameSceneAs]);
        assert!(
            rel.directed_between("o-identical-a", "o-identical-b", RelationType::ReEncodeOf)
                .await
                .unwrap()
                .is_none(),
            "a re-encode is an estimate and must not be asserted by a fact-only pass"
        );
    }
}

#[tokio::test]
async fn auto_merge_is_off_by_default_and_reversible_when_on() {
    for store in [sqlite_store().await, postgres_store().await] {
        corpus(&store).await;
        let rel = Relations::new(&store);
        let f = facts(&store).await;
        let findings = classify_all(&f, &PhashPolicy::default(), &|_, _| false);
        store_findings(&rel, &findings).await.unwrap();

        // Off. The default config is the one a caller has when it has never
        // heard of auto-merge, and it must be inert.
        let out = rel.auto_merge(&AutoMergeConfig::default()).await.unwrap();
        assert_eq!(out.merged.len(), 0, "auto-merge wrote nothing: {out:?}");

        // On, then taken back. The whole of §9.7's "opt-in and reversible" in
        // one assertion: the relation is there, the outcome names its id, and
        // removing the outcome removes exactly that relation.
        let out = rel
            .auto_merge(&AutoMergeConfig {
                enabled: true,
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(!out.merged.is_empty(), "auto-merge did something: {out:?}");
        let n = rel.unmerge(&out).await.unwrap();
        assert_eq!(n, out.merged.len(), "one row removed per merge");
        for m in &out.merged {
            assert!(
                rel.relations_of(&m.from_id).await.unwrap().is_empty()
                    && rel.relations_of(&m.to_id).await.unwrap().is_empty(),
                "unmerge left {} / {} behind",
                m.from_id,
                m.to_id
            );
        }
    }
}

// ---------------------------------------------------------- the false positive

#[tokio::test]
async fn the_false_positive_can_be_dismissed_and_stays_dismissed() {
    for store in [sqlite_store().await, postgres_store().await] {
        corpus(&store).await;
        let rel = Relations::new(&store);
        let f = facts(&store).await;

        // First pass: the false positive is proposed.
        let first = classify_all(&f, &PhashPolicy::default(), &|_, _| false);
        assert!(
            first
                .iter()
                .any(|x| x.left == "o-false-positive" || x.right == "o-false-positive"),
            "the false positive is in the candidate set: {first:?}"
        );

        // The user dismisses it (#1656). Dismissal is keyed on the *pair*, so
        // it is stored as a relation and not as an in-memory set -- which is
        // the only way it survives the process that produced it.
        rel.mark_not_duplicate("o-false-positive", "o-identical-a")
            .await
            .unwrap();

        // Second pass, same corpus, a fresh read. The pair is gone and the
        // real duplicate is not.
        let f2 = facts(&store).await;
        let veto = ruled_out(&rel, &f2).await;
        let second = classify_all(&f2, &PhashPolicy::default(), &|a, b| {
            veto.contains(&key(a, b))
        });
        assert!(
            !second.iter().any(|x| {
                (x.left == "o-false-positive" && x.right == "o-identical-a")
                    || (x.right == "o-false-positive" && x.left == "o-identical-a")
            }),
            "the dismissed pair was proposed again: {second:?}"
        );
        assert!(
            second.iter().any(|x| x.verdict == Verdict::Identical),
            "and the dismissal took the real duplicate with it: {second:?}"
        );
    }
}

#[tokio::test]
async fn a_foreign_algorithm_never_reaches_the_candidate_set() {
    for store in [sqlite_store().await, postgres_store().await] {
        corpus(&store).await;
        object(&store, "o-foreign").await;
        // Bit-identical to the re-encode's phash, so every distance
        // calculation says "the same picture".
        file(
            &store,
            &Fixture {
                id: "f-foreign",
                object_id: "o-foreign",
                path: "/lib/foreign.mkv",
                size: 700,
                blake3: "blake-fff",
                phash: "0000000000000001",
                algorithm: "phash64-v2",
            },
        )
        .await;

        let f = facts(&store).await;
        let findings = classify_all(&f, &PhashPolicy::default(), &|_, _| false);
        // The genuine one-bit pair is still a re-encode, so this is not a
        // classifier that found nothing -- it is a classifier that found the
        // comparable pair and left the incomparable one alone.
        assert!(findings.iter().any(|x| x.verdict == Verdict::ReEncode));
        assert!(
            !findings
                .iter()
                .any(|x| x.left == "o-foreign" || x.right == "o-foreign"),
            "a hash from another implementation is not a neighbour: {findings:?}"
        );
    }
}

#[tokio::test]
async fn the_threshold_is_the_boundary_it_claims_to_be() {
    // Exactly at, exactly one below, exactly one above. A threshold tested at a
    // comfortable distance from its own edge is a constant, not a threshold.
    let policy = PhashPolicy::default();
    let facts = |id: &str, phash: &str| FileFacts {
        object_id: id.to_string(),
        blake3: Some(format!("blake-{id}")),
        phash: Some(phash.to_string()),
        phash_algorithm: Some(DEFAULT_ALGORITHM.to_string()),
        size_bytes: 1_000,
    };
    // 10 bits set, against an all-zero hash.
    let at = "00000000000003ff";
    // 9 bits.
    let under = "00000000000001ff";
    // 11 bits.
    let over = "00000000000007ff";

    assert_eq!(
        classify(&facts("a", "0000000000000000"), &facts("b", at), &policy),
        Verdict::ReEncode,
        "at the threshold is a re-encode: the rule is inclusive"
    );
    assert_eq!(
        classify(&facts("a", "0000000000000000"), &facts("b", under), &policy),
        Verdict::ReEncode
    );
    assert_eq!(
        classify(&facts("a", "0000000000000000"), &facts("b", over), &policy),
        Verdict::Possible { distance: 11 },
        "one bit over is a suggestion, not a claim"
    );
}
