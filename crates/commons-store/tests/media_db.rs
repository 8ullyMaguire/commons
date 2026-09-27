//! Serving an object's media, and the consent gate in front of it.
//!
//! T-P5-006 item 9, spec §10.6. Companion to `locator.rs`'s gate, and for the
//! same reason: **a check that lives at the call site is a check that some
//! future call site forgets.**
//!
//! # The things only a real database can fail
//!
//! * **The gate is a gate and not a filter.** A denied object must produce *no
//!   path*, not a path the caller is trusted not to read. `Option` in the return
//!   type is what makes "forgot to check" a compile error rather than a
//!   disclosure.
//! * **Absent and denied are the same answer, and that is deliberate.** A caller
//!   who may not see an object and a caller asking about an object that does
//!   not exist both get `None`. Differentiating them turns the route into an
//!   oracle for what is in the library, which is consent information itself.
//! * **A file can be missing while its object exists.** `file.state` says the
//!   row is there but the bytes are not, and a route that ignores it 500s for a
//!   library mid-rescan.
//! * **The two engines do not agree by default.** `ORDER BY` on a path is
//!   BINARY on SQLite and locale-dependent on Postgres, so the same object
//!   serves a different file depending on which engine is behind it.

#[path = "harness/mod.rs"]
mod harness;
use harness::{postgres_store, sqlite_store};

use commons_core::Role;
use commons_store::db::Store;
use commons_store::filter_ast::CallerId;

/// A seed name no other fixture in this binary can also be using.
///
/// The Postgres side of this harness is ONE database shared by every test in
/// the file — `postgres_store()` scopes the connection, not the database — so a
/// fixed id like `o-pairing` is an ambient row any other test can create, and
/// the first version of the size-pairing test did exactly that: it passed on the
/// first engine and failed on the second, because a run of itself had left rows
/// behind and `media_path` returns the lexically smallest of them.
///
/// This is the same trap as three earlier bugs in this repo: a fixture that
/// names a shared row is a test whose result another test decides.
fn seed() -> String {
    format!("m{}", uuid::Uuid::new_v4().simple())
}

macro_rules! on_each_store {
    (|$store:ident| $body:block) => {{
        for $store in [sqlite_store().await, postgres_store().await] {
            $body
        }
    }};
}

macro_rules! run_write {
    ($store:expr, $q:expr, $($b:expr),* $(,)?) => {{
        let sql: &str = $q;
        macro_rules! go {
            ($p:expr, $query:expr) => {{
                let mut qb = sqlx::query($query);
                $( qb = qb.bind($b); )*
                qb.execute($p).await.unwrap_or_else(|e| panic!("{sql}: {e}"));
            }};
        }
        match $store {
            Store::Sqlite(p) => go!(p, sql),
            Store::Postgres(p) => {
                let bound = Store::bind_sql(sql);
                go!(p, &bound)
            }
        }
    }};
}

/// An object with one file at `/library/{seed}.mp4`, consented at `tier`.
///
/// Size 0 on purpose: it is distinct from the 4096 the multi-file fixtures use,
/// so "which row did this come from" is answerable from the size alone.
async fn seed_object_with_file(store: &Store, seed: &str, tier: &str, state: &str) -> String {
    let object_id = format!("o-{seed}");
    let t = commons_core::ts::now();
    run_write!(
        store,
        "INSERT INTO object (id, kind, title, created_at, updated_at) VALUES (?, ?, ?, ?, ?)",
        object_id.clone(),
        "clip",
        format!("{seed}-obj"),
        t.clone(),
        t.clone()
    );
    run_write!(
        store,
        "INSERT INTO file (id, object_id, path, state) VALUES (?, ?, ?, ?)",
        format!("f-{seed}"),
        object_id.clone(),
        format!("/library/{seed}.mp4"),
        state
    );
    run_write!(
        store,
        "INSERT INTO consent_record (id, object_id, tier, redistribution_permitted, updated_at)
         VALUES (?, ?, ?, ?, ?)",
        format!("cr-{seed}"),
        object_id.clone(),
        tier,
        0i32,
        t
    );
    object_id
}

/// A caller who may see only the publishable tiers.
fn public() -> CallerId {
    CallerId::anonymous()
}

/// The library's own operator: a logged-in local user with no explicit
/// allowlist. Sees `unverified`, so a freshly scanned file is visible in the
/// app holding it.
fn owner() -> CallerId {
    CallerId {
        account_id: Some(uuid::Uuid::new_v4().to_string()),
        ..CallerId::anonymous()
    }
}

/// A steward. **Not** the owner: `may_see_restricted()` routes a Steward to
/// `ConsentTiers::MODERATION`, which is the moderation tiers and the
/// publishable ones — and deliberately NOT `unverified`. I wrote the first
/// version of this file using `Role::Steward` as "the operator", which is wrong
/// in two ways at once: it would have made the unverified test pass for the
/// wrong reason (a Steward cannot see `unverified` either) and the denied test
/// meaningless.
fn steward() -> CallerId {
    CallerId {
        account_id: Some(uuid::Uuid::new_v4().to_string()),
        role: Role::Steward,
        ..CallerId::anonymous()
    }
}

#[tokio::test]
async fn a_publishable_object_hands_back_its_path() {
    on_each_store!(|store| {
        let seed = seed();
        let object_id = seed_object_with_file(&store, &seed, "self_published", "present").await;
        let media = store
            .media_path(&object_id, &public())
            .await
            .expect("query")
            .expect("the caller may see it, so there is a path");
        assert_eq!(
            media.path,
            std::path::Path::new(&format!("/library/{seed}.mp4"))
        );
        assert_eq!(
            media.size_bytes, 0,
            "the real size, which the fixture left at 0"
        );
        assert!(media.is_present(), "and the state agrees with the filter");
    });
}

#[tokio::test]
async fn an_object_the_caller_may_not_see_hands_back_nothing_at_all() {
    on_each_store!(|store| {
        // THE test. `unverified` is deliberately absent from the public tiers:
        // consent not established means consent not established for anyone but
        // the holder. The assertion is on the Option, not on a boolean flag --
        // there is no path in the return value to ignore, so "forgot to check"
        // cannot compile.
        let seed = seed();
        let object_id = seed_object_with_file(&store, &seed, "unverified", "present").await;
        assert!(
            store
                .media_path(&object_id, &public())
                .await
                .expect("query")
                .is_none(),
            "no path, not a path the caller is trusted not to read"
        );
    });
}

#[tokio::test]
async fn the_holder_of_an_unverified_object_still_gets_the_path() {
    on_each_store!(|store| {
        // The other half. A gate that refuses the operator their own unverified
        // scans is the bug the `consent_filter.rs` header describes happening
        // once already: a licensed item the owner cannot find in their own
        // library. The gate is about *who*, not about *whether*.
        let seed = seed();
        let object_id = seed_object_with_file(&store, &seed, "unverified", "present").await;
        assert!(
            store
                .media_path(&object_id, &owner())
                .await
                .expect("query")
                .is_some(),
            "the operator can see their own unverified scan"
        );
    });
}

#[tokio::test]
async fn a_denied_object_is_invisible_to_everyone() {
    on_each_store!(|store| {
        // `denied` is the tombstone, and it is in `ConsentTiers::ALL` and in
        // nobody's set -- not the public set, not OWNER, not MODERATION. A route
        // that served it because the caller is the operator would be the
        // largest hole in the codebase, so this asserts all three callers.
        let seed = seed();
        let object_id = seed_object_with_file(&store, &seed, "denied", "present").await;
        for (label, caller) in [
            ("public", public()),
            ("owner", owner()),
            ("steward", steward()),
        ] {
            assert!(
                store
                    .media_path(&object_id, &caller)
                    .await
                    .expect("query")
                    .is_none(),
                "denied is denied for the {label} too"
            );
        }
    });
}

#[tokio::test]
async fn an_object_that_does_not_exist_and_one_that_is_denied_answer_the_same() {
    on_each_store!(|store| {
        // Indistinguishable on purpose. Differentiating them turns this function
        // into an oracle for what is in the library -- and on a consent-first
        // platform "does this id exist" is itself consent information.
        let seed = seed();
        let denied_id = seed_object_with_file(&store, &seed, "denied", "present").await;
        let denied = store.media_path(&denied_id, &owner()).await.expect("query");
        let absent = store
            .media_path(&format!("o-{seed}-absent"), &owner())
            .await
            .expect("query");
        assert_eq!(
            denied, absent,
            "the same answer, so the route leaks nothing"
        );
    });
}

#[tokio::test]
async fn a_file_that_is_not_present_yields_no_path() {
    on_each_store!(|store| {
        // Mid-rescan. `file.state` exists to say the row is there but the bytes
        // are not, and a route that ignores it serves a 500 for a library that
        // is merely being reindexed. The filter is in the `WHERE` rather than in
        // Rust for that reason: absent rows are the *common* case while
        // indexing, not an edge case.
        let seed = seed();
        let object_id = seed_object_with_file(&store, &seed, "self_published", "absent").await;
        assert!(
            store
                .media_path(&object_id, &public())
                .await
                .expect("query")
                .is_none(),
            "the row is there, the bytes are not"
        );
    });
}

#[tokio::test]
async fn an_object_with_no_file_yields_no_path() {
    on_each_store!(|store| {
        // A scene with a gallery, or a metadata-only object. Distinct from the
        // previous case: there is no `file` row at all. That is why this is an
        // INNER JOIN and not the LEFT JOIN `all_tags_with_counts` needs -- one
        // file per object is the *precondition* here, not a filter, and listing
        // an object with nothing to serve produces a tile that 404s.
        let seed = seed();
        let object_id = format!("o-{seed}");
        let t = commons_core::ts::now();
        run_write!(
            &store,
            "INSERT INTO object (id, kind, title, created_at, updated_at) VALUES (?, ?, ?, ?, ?)",
            object_id.clone(),
            "scene",
            "no file",
            t.clone(),
            t.clone()
        );
        run_write!(
            &store,
            "INSERT INTO consent_record (id, object_id, tier, redistribution_permitted, updated_at)
             VALUES (?, ?, ?, ?, ?)",
            format!("{seed}-cr"),
            object_id.clone(),
            "self_published",
            0i32,
            t
        );
        assert!(store
            .media_path(&object_id, &public())
            .await
            .expect("query")
            .is_none());
    });
}

#[tokio::test]
async fn the_gate_is_a_where_clause_and_not_a_filter_after_the_fact() {
    on_each_store!(|store| {
        // The structural claim, measured rather than read: the consent clause
        // is a SQL predicate over data, so a row the caller may not see is never
        // loaded and then discarded. The difference is invisible in the result
        // -- both paths return `None` -- and visible only in what a future edit
        // to this function could do with a row it has already read.
        let mut params = Vec::new();
        let clause = commons_store::filter_ast::Filter::consent_clause(&public(), &mut params)
            .expect("clause");
        assert!(
            clause.to_lowercase().contains("tier"),
            "the consent clause names the tier column, so it filters in SQL: {clause}"
        );
        assert!(
            !params.is_empty(),
            "and it binds the tiers, so it is a predicate over data and not a constant"
        );

        // And the behaviour that structure produces: an unverified row is not
        // loaded for a public caller.
        let seed = seed();
        let object_id = seed_object_with_file(&store, &seed, "unverified", "present").await;
        assert!(store
            .media_path(&object_id, &public())
            .await
            .expect("query")
            .is_none());
    });
}

#[tokio::test]
async fn a_negative_recorded_size_comes_back_as_zero() {
    on_each_store!(|store| {
        // `size_bytes` is BIGINT NOT NULL, so a negative value can only arrive
        // by hand or by a bad migration. The route puts this number straight
        // into `Content-Length` and into the range arithmetic, and a negative
        // there is not a 4xx -- it is a response that never terminates or a
        // browser that blames the feed. Clamped here, once, where the row is
        // read, so no route can get it wrong independently.
        let seed = seed();
        let object_id = format!("o-{seed}");
        let t = commons_core::ts::now();
        run_write!(
            &store,
            "INSERT INTO object (id, kind, title, created_at, updated_at) VALUES (?, ?, ?, ?, ?)",
            object_id.clone(),
            "clip",
            "neg",
            t.clone(),
            t.clone()
        );
        run_write!(
            &store,
            "INSERT INTO file (id, object_id, path, size_bytes, state) VALUES (?, ?, ?, ?, ?)",
            format!("{seed}-f"),
            object_id.clone(),
            format!("/library/{seed}.mp4"),
            -1i64,
            "present"
        );
        run_write!(
            &store,
            "INSERT INTO consent_record (id, object_id, tier, redistribution_permitted, updated_at)
             VALUES (?, ?, ?, ?, ?)",
            format!("{seed}-cr"),
            object_id.clone(),
            "self_published",
            0i32,
            t
        );

        let media = store
            .media_path(&object_id, &public())
            .await
            .expect("query")
            .expect("present file");
        assert_eq!(media.size_bytes, 0, "not -1 cast to a huge u64");
    });
}

/// The served file, from both engines, for one object holding two files whose
/// *byte* order and *collation* order disagree.
///
/// The pair is `a.mp4` and `a-preview.mp4`. SQLite's BINARY order puts the
/// preview LAST ('-' 0x2D > '.' 0x2E); Postgres's en_US.UTF-8 puts it FIRST,
/// because that collation drops punctuation at the primary comparison level. A
/// query that does `ORDER BY path LIMIT 1` therefore serves a different file
/// depending on which engine is behind it, and **only a test that runs both
/// engines sees it** — which is why this returns both answers rather than
/// asserting inside the loop.
async fn two_files_both_engines() -> (Option<String>, Option<String>) {
    // One seed for the whole call, so both engines are asked about the SAME two
    // files. A per-engine seed would compare two different libraries and pass
    // for the wrong reason.
    let seed = seed();
    let mut answers = Vec::new();
    for store in [sqlite_store().await, postgres_store().await] {
        let object_id = format!("o-{seed}");
        let t = commons_core::ts::now();
        run_write!(
            &store,
            "INSERT INTO object (id, kind, title, created_at, updated_at) VALUES (?, ?, ?, ?, ?)",
            object_id.clone(),
            "clip",
            format!("{seed}-obj"),
            t.clone(),
            t.clone()
        );
        for (n, (name, size)) in [("a.mp4", 0i64), ("a-preview.mp4", 4096i64)]
            .into_iter()
            .enumerate()
        {
            run_write!(
                &store,
                "INSERT INTO file (id, object_id, path, size_bytes, state) VALUES (?, ?, ?, ?, ?)",
                format!("{seed}-f{n}"),
                object_id.clone(),
                format!("/library/{name}"),
                size,
                "present"
            );
        }
        run_write!(
            &store,
            "INSERT INTO consent_record (id, object_id, tier, redistribution_permitted, updated_at)
             VALUES (?, ?, ?, ?, ?)",
            format!("{seed}-cr"),
            object_id.clone(),
            "self_published",
            0i32,
            t
        );
        answers.push(
            store
                .media_path(&object_id, &public())
                .await
                .expect("query")
                .map(|m| m.path.to_string_lossy().into_owned()),
        );
    }
    (answers[0].clone(), answers[1].clone())
}

#[tokio::test]
async fn the_served_file_is_the_same_one_on_both_engines() {
    // THE test for this function, and the one whose failure mode is invisible
    // on a single engine: parity. My first version of this asserted a literal
    // path and it failed with a DIFFERENT wrong answer on each run, because
    // each engine is right and they disagree. Asserting a literal can only ever
    // pin one of them.
    let (sqlite_answer, postgres_answer) = two_files_both_engines().await;

    assert_eq!(
        sqlite_answer, postgres_answer,
        "the two engines must serve the same file for the same object"
    );
    // And it must be a definite choice, not "whichever the engine felt like".
    assert_eq!(
        sqlite_answer.as_deref(),
        Some("/library/a-preview.mp4"),
        "Unicode code-point order at the first differing byte: '-' is 0x2D and \
         '.' is 0x2E, so a-preview.mp4 < a.mp4 and the preview is the answer on \
         BOTH engines. I had this backwards in the first draft, which is worth \
         writing down: the intuition that a bare name sorts before a '-suffix' of \
         it is exactly what the two engines' collations are built to do, and \
         neither of them does it here."
    );
}

#[tokio::test]
async fn the_size_that_comes_back_belongs_to_the_file_that_was_chosen() {
    // Guards the pairing. A path from one row and a size from another is the
    // bug a joined-row `SELECT` invites, and it is invisible until the two
    // files differ in size -- which is why this fixture's two files do.
    //
    // The expected size is derived from the path rather than written as a
    // literal, so the assertion is about the *pairing* and survives a change to
    // the ordering rule. A literal here would only test that I can do
    // arithmetic.
    //
    // `Path::ends_with` is component-wise, not a string suffix: `p.ends_with(
    // "-preview.mp4")` asks whether the last *component* equals that string,
    // which for `/library/m<uuid>-preview.mp4` is never true. I wrote it that
    // way first and the test failed on every run with a path that visibly ended
    // in `-preview.mp4` printed right there in the message -- which is the tell.
    // When the failure message contradicts the predicate, the predicate is not
    // doing what its name says.
    let seed = seed();
    for store in [sqlite_store().await, postgres_store().await] {
        seed_object_with_file(&store, &seed, "self_published", "present").await;
        let object_id = format!("o-{seed}");
        run_write!(
            &store,
            "INSERT INTO file (id, object_id, path, size_bytes, state) VALUES (?, ?, ?, ?, ?)",
            format!("{seed}-f2"),
            object_id.clone(),
            format!("/library/{seed}-preview.mp4"),
            4096i64,
            "present"
        );
        let media = store
            .media_path(&object_id, &public())
            .await
            .expect("query")
            .expect("present file");

        let path = media.path.to_string_lossy().into_owned();
        let expected_size: u64 = if path.ends_with("-preview.mp4") {
            4096
        } else {
            0
        };
        assert_eq!(
            media.size_bytes, expected_size,
            "{path} came with the size of the other file: the path and the size \
             are not from one row"
        );
    }
}

#[tokio::test]
async fn an_object_with_a_file_and_no_consent_row_at_all_yields_no_path() {
    // NOTE ON WHAT THIS DOES AND DOES NOT PIN. This test cannot kill the
    // INNER->LEFT join mutation, and the reason is worth more than the test:
    // `consent_clause` emits `c.tier IN (...)`, and a LEFT JOIN with no match
    // leaves `c.tier` NULL, and `NULL IN (...)` is NULL, which is not TRUE, so
    // the row is filtered out anyway. The mutation is a no-op by construction.
    //
    // So the INNER join here is correct for a reason the tests cannot observe:
    // it is there to make the query say what it means, and the WHERE clause
    // happens to enforce the same thing. A test that claimed to pin it would be
    // claiming coverage it does not have. What this test DOES pin is the
    // behaviour, which is the thing that would actually be wrong.
    //
    on_each_store!(|store| {
        // Found by mutation, not by reading. Changing the consent join from
        // INNER to LEFT leaves every other test green, because every other
        // fixture writes a consent row -- so the mutants that survive are the
        // ones for which no fixture in the file constructs the input.
        //
        // An object with no `consent_record` is not a hypothetical: it is what
        // a scan interrupted between inserting the file and classifying it
        // leaves behind, and it is what a migration that added the table to an
        // existing library leaves for every row it has not backfilled. Consent
        // is not established by the ABSENCE of a denial. The default is "not
        // visible", and the only thing that makes an object visible is a row
        // that says so.
        let seed = seed();
        let object_id = format!("o-{seed}");
        let t = commons_core::ts::now();
        run_write!(
            &store,
            "INSERT INTO object (id, kind, title, created_at, updated_at) VALUES (?, ?, ?, ?, ?)",
            object_id.clone(),
            "clip",
            "never classified",
            t.clone(),
            t.clone()
        );
        run_write!(
            &store,
            "INSERT INTO file (id, object_id, path, state) VALUES (?, ?, ?, ?)",
            format!("{seed}-f"),
            object_id.clone(),
            format!("/library/{seed}.mp4"),
            "present"
        );
        // Deliberately NO consent_record. `consent_record` has a FK to object,
        // not the other way round, so this state is representable.

        for (label, caller) in [
            ("public", public()),
            ("owner", owner()),
            ("steward", steward()),
        ] {
            assert!(
                store
                    .media_path(&object_id, &caller)
                    .await
                    .expect("query")
                    .is_none(),
                "no consent record means not visible -- to the {label} too, since \
                 consent is not established by the absence of a denial"
            );
        }
    });
}
