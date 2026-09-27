//! Undo against a real database, on both engines.
//!
//! T-P5-006 item 7. Spec §10.7, §8.6. Companion to `undo.rs`'s pure tests,
//! which cover the state comparison; this file covers the schema and the SQL.
//!
//! Three things only a real database can fail, and each has a test here:
//!
//! * **The record round-trips.** `undo_record` reconstructs `after.row_existed`
//!   rather than storing it, so a test has to confirm that reconstruction from
//!   a written row gives back the state that was recorded.
//! * **The staleness check fires against real rows.** The pure test can pass a
//!   state that the database would never produce; this one edits a row behind
//!   the record's back and requires the undo to be refused.
//! * **Absent and NULL are different in the database.** Not a claim about
//!   `Option` — a claim that `DELETE` leaves the row count at zero while a
//!   row-of-NULLs leaves it at one, and that the undo restores each correctly.

#[path = "harness/mod.rs"]
mod harness;
use harness::{postgres_store, sqlite_store};

use commons_core::ts::now;
use commons_store::db::Store;
use commons_store::undo::{
    self, PreparedUndo, TagState, UndoEntry, UndoError, Write, UNDO_WINDOW_SECS,
};
/// Run a block against both engines.
///
/// Same reason as `folders_db.rs`: the two result types share no variant a
/// caller can name, so a body whose value is `()` is what reconciles.
macro_rules! on_each_store {
    (|$store:ident| $body:block) => {{
        for $store in [sqlite_store().await, postgres_store().await] {
            $body
        }
    }};
}

// ---------------------------------------------------------------- fixtures

/// Every fixture write goes through this, because the two engines' result types
/// are unrelated and a `match` whose arms yield them does not typecheck. The
/// block's value is `()`, so there is nothing to reconcile — the same trick
/// `folders_db.rs` uses, and for the same reason.
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

/// A tag, and an object to hang it on.
///
/// `seed` names both ids, so a fixture never writes a row another test's
/// assertions depend on: the shared ambient row is how a test ends up asserting
/// a value some other suite can set.
async fn tag_and_object(store: &Store, seed: &str) -> (String, String) {
    let tag_id = format!("t-{seed}");
    let object_id = format!("o-{seed}");

    run_write!(
        store,
        "INSERT INTO tag (id, name) VALUES (?, ?)",
        tag_id.clone(),
        format!("{seed}-tag")
    );
    // `kind` is NOT NULL and there is no `name` column -- `title` is the human
    // label. Read from the migration rather than guessed: a fixture that
    // invents a column fails loudly here, which is why this is a panic and not
    // a skip.
    let t = now();
    run_write!(
        store,
        "INSERT INTO object (id, kind, title, created_at, updated_at) VALUES (?, ?, ?, ?, ?)",
        object_id.clone(),
        "clip",
        format!("{seed}-obj"),
        t.clone(),
        t
    );
    (tag_id, object_id)
}

/// A tag row written directly, with the given column values.
///
/// The conflict clause is what a real write uses, so the fixture uses it too: a
/// fixture that inserts a different way can set up a state the application can
/// never produce, and then a test passes for a reason that cannot recur.
async fn put_tag_row(
    store: &Store,
    object_id: &str,
    tag_id: &str,
    confidence: Option<f64>,
    source: Option<&str>,
) {
    run_write!(
        store,
        "INSERT INTO object_tag (object_id, tag_id, confidence, source, created_at) \
         VALUES (?, ?, ?, ?, NULL) \
         ON CONFLICT (object_id, tag_id) DO UPDATE SET \
           confidence = excluded.confidence, source = excluded.source",
        object_id.to_string(),
        tag_id.to_string(),
        confidence,
        source.map(str::to_string),
    );
}

/// How many tag rows an object has. Zero means absent, one means a row.
async fn tag_row_count(store: &Store, object_id: &str, tag_id: &str) -> i64 {
    let sql = "SELECT 1 FROM object_tag WHERE object_id = ? AND tag_id = ?";
    macro_rules! go {
        ($p:expr, $q:expr) => {{
            let mut qb = sqlx::query($q);
            qb = qb.bind(object_id.to_string()).bind(tag_id.to_string());
            qb.fetch_all($p)
                .await
                .map(|r| r.len() as i64)
                .unwrap_or_else(|e| panic!("{sql}: {e}"))
        }};
    }
    match store {
        Store::Sqlite(p) => go!(p, sql),
        Store::Postgres(p) => {
            let bound = Store::bind_sql(sql);
            go!(p, &bound)
        }
    }
}

/// The row's current `source`, or `None` if the row is absent.
///
/// The double `Option` is the distinction the module exists to keep: `None` is
/// no row, `Some(None)` is a row whose `source` is NULL, and collapsing them is
/// the bug this file keeps asserting against.
async fn tag_source(store: &Store, object_id: &str, tag_id: &str) -> Option<Option<String>> {
    use sqlx::Row;
    let sql = "SELECT source FROM object_tag WHERE object_id = ? AND tag_id = ?";
    macro_rules! go {
        ($p:expr, $q:expr) => {{
            let mut qb = sqlx::query($q);
            qb = qb.bind(object_id.to_string()).bind(tag_id.to_string());
            qb.fetch_optional($p)
                .await
                .map(|r| r.map(|r| r.try_get::<Option<String>, _>(0).ok().flatten()))
                .unwrap_or_else(|e| panic!("{sql}: {e}"))
        }};
    }
    match store {
        Store::Sqlite(p) => go!(p, sql),
        Store::Postgres(p) => {
            let bound = Store::bind_sql(sql);
            go!(p, &bound)
        }
    }
}

/// The common entry: an add that created the row, so undoing it deletes.
fn created_entry(object_id: &str) -> UndoEntry {
    UndoEntry {
        object_id: object_id.to_string(),
        before: TagState::absent(),
        after: TagState {
            row_existed: true,
            confidence: Some(0.9),
            source: Some("bulk".into()),
            created_at: None,
        },
    }
}

// ---------------------------------------------------------------- the tests

#[tokio::test]
async fn an_undo_record_round_trips_through_the_database() {
    on_each_store!(|store| {
        let (tag, obj) = tag_and_object(&store, "rt").await;
        let before = TagState {
            row_existed: true,
            confidence: Some(0.25),
            source: Some("import".into()),
            created_at: Some("2026-01-01T00:00:00.000Z".into()),
        };
        let after = TagState {
            row_existed: true,
            confidence: Some(0.9),
            source: Some("bulk".into()),
            created_at: None,
        };
        store
            .record_undo(&Write {
                id: "r-rt".into(),
                caller: "me".into(),
                action: "bulk.tag.add".into(),
                tag_id: tag.clone(),
                requested: 1,
                matched: 1,
                entries: vec![UndoEntry {
                    object_id: obj.clone(),
                    before: before.clone(),
                    after: after.clone(),
                }],
            })
            .await
            .expect("recording an undo must succeed");

        let rec = store
            .undo_record("r-rt")
            .await
            .expect("read")
            .expect("the record exists");
        assert_eq!(rec.tag_id, tag, "the record carries its own tag");
        assert_eq!(rec.caller, "me");
        assert_eq!(rec.action, "bulk.tag.add");
        assert_eq!(rec.undone_at, None, "a fresh record is offerable");
        assert_eq!(rec.entries.len(), 1);
        // The before state round-trips field for field.
        assert_eq!(rec.entries[0].before, before);
        // The after state round-trips too, *including* `row_existed` -- which
        // the read reconstructs rather than stores. If the reconstruction is
        // wrong this is where it shows, and the undo would then accept a record
        // whose own after-state it cannot reproduce.
        assert_eq!(rec.entries[0].after, after);
    });
}

#[tokio::test]
async fn an_absent_row_is_distinguished_from_a_row_of_nulls() {
    on_each_store!(|store| {
        let (tag, obj) = tag_and_object(&store, "nulls").await;

        // A row with every nullable column NULL -- the state of every
        // object_tag row written before migration 0016.
        put_tag_row(&store, &obj, &tag, None, None).await;
        let state = undo::tag_state(&store, &obj, &tag)
            .await
            .expect("read state");
        assert!(state.row_existed, "the row is there");
        assert_eq!(state.source, None, "and its source is NULL");
        assert_eq!(state.confidence, None);

        // An object with no row at all.
        let (tag2, obj2) = tag_and_object(&store, "nulls2").await;
        let absent = undo::tag_state(&store, &obj2, &tag2)
            .await
            .expect("read state");
        assert!(!absent.row_existed);

        // The two are not the same state, and `matches` must say so -- this is
        // the comparison the staleness check turns on.
        assert!(
            !state.matches(&absent),
            "a row of NULLs is not the same as no row"
        );
    });
}

#[tokio::test]
async fn undoing_an_add_deletes_the_row_it_created() {
    on_each_store!(|store| {
        let (tag, obj) = tag_and_object(&store, "add").await;
        assert_eq!(tag_row_count(&store, &obj, &tag).await, 0, "starts absent");

        // The write: add the tag.
        put_tag_row(&store, &obj, &tag, Some(0.9), Some("bulk")).await;
        assert_eq!(tag_row_count(&store, &obj, &tag).await, 1);

        store
            .record_undo(&Write {
                id: "r-add".into(),
                caller: "me".into(),
                action: "bulk.tag.add".into(),
                tag_id: tag.clone(),
                requested: 1,
                matched: 1,
                entries: vec![created_entry(&obj)],
            })
            .await
            .expect("record");

        let n = store.undo("r-add", "me").await.expect("the undo applies");
        assert_eq!(n, 1, "one object restored");
        assert_eq!(
            tag_row_count(&store, &obj, &tag).await,
            0,
            "the row the add created is gone, not blanked"
        );
    });
}

#[tokio::test]
async fn undoing_an_add_over_an_existing_row_puts_the_old_values_back() {
    on_each_store!(|store| {
        let (tag, obj) = tag_and_object(&store, "over").await;
        // The object already had the tag, with values the write will replace.
        put_tag_row(&store, &obj, &tag, Some(0.3), Some("manual")).await;

        // The write overwrites confidence and source.
        put_tag_row(&store, &obj, &tag, Some(0.9), Some("bulk")).await;
        assert_eq!(
            tag_source(&store, &obj, &tag).await,
            Some(Some("bulk".into()))
        );

        store
            .record_undo(&Write {
                id: "r-over".into(),
                caller: "me".into(),
                action: "bulk.tag.add".into(),
                tag_id: tag.clone(),
                requested: 1,
                matched: 1,
                entries: vec![UndoEntry {
                    object_id: obj.clone(),
                    before: TagState {
                        row_existed: true,
                        confidence: Some(0.3),
                        source: Some("manual".into()),
                        created_at: None,
                    },
                    after: TagState {
                        row_existed: true,
                        confidence: Some(0.9),
                        source: Some("bulk".into()),
                        created_at: None,
                    },
                }],
            })
            .await
            .expect("record");

        store.undo("r-over", "me").await.expect("the undo applies");
        assert_eq!(
            tag_source(&store, &obj, &tag).await,
            Some(Some("manual".into())),
            "the prior source is back, not the bulk one"
        );
    });
}

#[tokio::test]
async fn a_row_of_nulls_is_restored_as_a_row_of_nulls() {
    on_each_store!(|store| {
        let (tag, obj) = tag_and_object(&store, "restore-nulls").await;
        // Before: a row, all nullable columns NULL.
        put_tag_row(&store, &obj, &tag, None, None).await;
        // After: the write filled it in.
        put_tag_row(&store, &obj, &tag, Some(0.9), Some("bulk")).await;

        store
            .record_undo(&Write {
                id: "r-rn".into(),
                caller: "me".into(),
                action: "bulk.tag.add".into(),
                tag_id: tag.clone(),
                requested: 1,
                matched: 1,
                entries: vec![UndoEntry {
                    object_id: obj.clone(),
                    before: TagState {
                        row_existed: true,
                        confidence: None,
                        source: None,
                        created_at: None,
                    },
                    after: TagState {
                        row_existed: true,
                        confidence: Some(0.9),
                        source: Some("bulk".into()),
                        created_at: None,
                    },
                }],
            })
            .await
            .expect("record");

        store.undo("r-rn", "me").await.expect("the undo applies");
        assert_eq!(
            tag_row_count(&store, &obj, &tag).await,
            1,
            "the row must still be there: it was there before"
        );
        assert_eq!(
            tag_source(&store, &obj, &tag).await,
            Some(None),
            "and its source is NULL again, not 'bulk' and not absent"
        );
    });
}

#[tokio::test]
async fn a_superseded_write_is_refused_and_nothing_is_written() {
    on_each_store!(|store| {
        let (tag, a) = tag_and_object(&store, "sup-a").await;
        let (_, b) = tag_and_object(&store, "sup-b").await;
        for o in [&a, &b] {
            put_tag_row(&store, o, &tag, Some(0.9), Some("bulk")).await;
        }

        store
            .record_undo(&Write {
                id: "r-sup".into(),
                caller: "me".into(),
                action: "bulk.tag.add".into(),
                tag_id: tag.clone(),
                requested: 2,
                matched: 2,
                entries: vec![created_entry(&a), created_entry(&b)],
            })
            .await
            .expect("record");

        // Somebody edits one object behind the record's back.
        put_tag_row(&store, &b, &tag, Some(0.1), Some("hand")).await;

        let err = store
            .undo("r-sup", "me")
            .await
            .expect_err("the undo must be refused");
        assert_eq!(
            err,
            UndoError::Superseded {
                object_id: b.clone()
            },
            "the refusal names the object that moved, not just that one did"
        );

        // All-or-nothing: object `a` was untouched, so it must still carry the
        // tag. A partial undo here would leave the user unable to tell which
        // half happened.
        assert_eq!(
            tag_row_count(&store, &a, &tag).await,
            1,
            "the first object is untouched: the undo wrote nothing at all"
        );
    });
}

#[tokio::test]
async fn another_callers_record_is_not_theirs_to_undo() {
    on_each_store!(|store| {
        let (tag, obj) = tag_and_object(&store, "owner").await;
        put_tag_row(&store, &obj, &tag, Some(0.9), Some("bulk")).await;
        store
            .record_undo(&Write {
                id: "r-own".into(),
                caller: "alice".into(),
                action: "bulk.tag.add".into(),
                tag_id: tag.clone(),
                requested: 1,
                matched: 1,
                entries: vec![created_entry(&obj)],
            })
            .await
            .expect("record");

        let err = store
            .undo("r-own", "bob")
            .await
            .expect_err("bob must not undo alice's write");
        assert_eq!(err, UndoError::NotYourRecord);
        assert_eq!(
            tag_row_count(&store, &obj, &tag).await,
            1,
            "and the refusal wrote nothing"
        );

        // Alice can.
        store
            .undo("r-own", "alice")
            .await
            .expect("alice's own undo applies");
    });
}

#[tokio::test]
async fn a_record_can_be_undone_only_once() {
    on_each_store!(|store| {
        let (tag, obj) = tag_and_object(&store, "twice").await;
        put_tag_row(&store, &obj, &tag, Some(0.9), Some("bulk")).await;
        store
            .record_undo(&Write {
                id: "r-twice".into(),
                caller: "me".into(),
                action: "bulk.tag.add".into(),
                tag_id: tag.clone(),
                requested: 1,
                matched: 1,
                entries: vec![created_entry(&obj)],
            })
            .await
            .expect("record");

        store.undo("r-twice", "me").await.expect("first undo");
        let err = store
            .undo("r-twice", "me")
            .await
            .expect_err("a second undo must be refused");
        assert_eq!(err, UndoError::AlreadyUndone);

        // And it is no longer offered.
        let offerable = store.undoable("me").await.expect("list");
        assert!(
            !offerable.iter().any(|r| r.id == "r-twice"),
            "a consumed record is not offerable again"
        );
    });
}

#[tokio::test]
async fn the_expiry_boundary_is_exact() {
    on_each_store!(|store| {
        let (tag, obj) = tag_and_object(&store, "exp").await;
        put_tag_row(&store, &obj, &tag, Some(0.9), Some("bulk")).await;

        let rec = store
            .record_undo(&Write {
                id: "r-exp".into(),
                caller: "me".into(),
                action: "bulk.tag.add".into(),
                tag_id: tag.clone(),
                requested: 1,
                matched: 1,
                entries: vec![created_entry(&obj)],
            })
            .await;
        assert!(rec.is_ok());

        let r = store
            .undo_record("r-exp")
            .await
            .expect("read")
            .expect("exists");

        // A second before the boundary, a second at it, a second after. The
        // comparison is a string compare on RFC 3339, which is order-preserving
        // only because every timestamp here is the same shape and UTC -- a fact
        // `expires_at` guarantees by writing the offset itself.
        let created = chrono::DateTime::parse_from_rfc3339(&r.created_at).expect("ts");
        let shift = |secs: i64| {
            (created + chrono::Duration::seconds(secs))
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        };

        assert!(r.is_undoable(&shift(UNDO_WINDOW_SECS - 1)), "just inside");
        assert!(
            !r.is_undoable(&r.expires_at),
            "at the boundary the record is expired -- `>` is strict"
        );
        assert!(!r.is_undoable(&shift(UNDO_WINDOW_SECS + 1)), "just outside");

        // The database agrees, not just the pure function: the listing filters
        // on the stored value.
        let offerable = store.undoable("me").await.expect("list");
        assert!(
            offerable.iter().any(|x| x.id == "r-exp"),
            "a fresh record is offerable in the database too"
        );
    });
}

#[tokio::test]
async fn undoable_lists_only_this_callers_fresh_records() {
    on_each_store!(|store| {
        let (tag, obj) = tag_and_object(&store, "list").await;
        put_tag_row(&store, &obj, &tag, Some(0.9), Some("bulk")).await;

        let entry = UndoEntry {
            object_id: obj.clone(),
            before: TagState::absent(),
            after: TagState {
                row_existed: true,
                confidence: Some(0.9),
                source: Some("bulk".into()),
                created_at: None,
            },
        };
        store
            .record_undo(&Write {
                id: "r-mine".into(),
                caller: "me".into(),
                action: "bulk.tag.add".into(),
                tag_id: tag.clone(),
                requested: 1,
                matched: 1,
                entries: vec![entry.clone()],
            })
            .await
            .expect("record");
        store
            .record_undo(&Write {
                id: "r-theirs".into(),
                caller: "you".into(),
                action: "bulk.tag.add".into(),
                tag_id: tag.clone(),
                requested: 1,
                matched: 1,
                entries: vec![entry],
            })
            .await
            .expect("record");

        let mine = store.undoable("me").await.expect("list");
        assert!(mine.iter().any(|r| r.id == "r-mine"));
        assert!(
            !mine.iter().any(|r| r.id == "r-theirs"),
            "another caller's record is not offered"
        );
    });
}

#[tokio::test]
async fn an_unknown_record_is_named_in_the_error() {
    on_each_store!(|store| {
        let err = store.undo("nope", "me").await.expect_err("no such record");
        assert_eq!(err, UndoError::NoSuchRecord("nope".into()));
    });
}

/// A row of NULLs is the case that makes `IS NOT DISTINCT FROM` load-bearing.
///
/// Every value column is nullable, and `=` never matches NULL — so a restore
/// guarded with `confidence = ?` would find zero rows for a row whose
/// confidence is NULL and report the undo superseded. That is a false refusal
/// on the single most common legacy state, and it would look like a bug in the
/// feature rather than in the comparison. This test fails with `=` and passes
/// with `IS NOT DISTINCT FROM`.
#[tokio::test]
async fn a_row_of_nulls_is_not_mistaken_for_a_superseded_one() {
    on_each_store!(|store| {
        let (tag, obj) = tag_and_object(&store, "nulls-not-stale").await;
        // The write fills in a row that was there with NULLs before.
        put_tag_row(&store, &obj, &tag, None, None).await;

        store
            .record_undo(&Write {
                id: "r-nns".into(),
                caller: "me".into(),
                action: "bulk.tag.add".into(),
                tag_id: tag.clone(),
                requested: 1,
                matched: 1,
                entries: vec![UndoEntry {
                    object_id: obj.clone(),
                    before: TagState {
                        row_existed: true,
                        confidence: None,
                        source: None,
                        created_at: None,
                    },
                    after: TagState {
                        row_existed: true,
                        confidence: None,
                        source: None,
                        created_at: None,
                    },
                }],
            })
            .await
            .expect("record");

        // Same values on both sides: the row is exactly as the record says, so
        // the undo must apply rather than refuse.
        let n = store
            .undo("r-nns", "me")
            .await
            .expect("a row of NULLs is not a superseded row");
        assert_eq!(n, 1);
        assert_eq!(
            tag_row_count(&store, &obj, &tag).await,
            1,
            "the row survives: it existed before and exists after"
        );
    });
}

/// The second pass re-checks the same condition, so a write that is refused
/// there reports the object that moved rather than a bare failure.
///
/// This is the only observable difference between a read-only check and a
/// conditional write: both refuse a superseded record, and only the second
/// catches a commit that lands between them. Asserting the refusal NAMES the
/// object is what pins the second pass — a first-pass-only implementation
/// refuses the same record with the same variant, so the error's payload is the
/// only thing distinguishing them from the outside.
#[tokio::test]
async fn a_refusal_names_the_object_rather_than_failing_opaquely() {
    on_each_store!(|store| {
        let (tag, obj) = tag_and_object(&store, "names").await;
        put_tag_row(&store, &obj, &tag, Some(0.9), Some("bulk")).await;
        store
            .record_undo(&Write {
                id: "r-names".into(),
                caller: "me".into(),
                action: "bulk.tag.add".into(),
                tag_id: tag.clone(),
                requested: 1,
                matched: 1,
                entries: vec![created_entry(&obj)],
            })
            .await
            .expect("record");
        put_tag_row(&store, &obj, &tag, Some(0.4), Some("hand")).await;

        match store.undo("r-names", "me").await {
            Err(UndoError::Superseded { object_id }) => assert_eq!(object_id, obj),
            other => panic!("expected a named refusal, got {other:?}"),
        }
    });
}

/// The claim the set-based restore exists to make: a record spanning both
/// shapes restores entirely, or not at all.
///
/// Three objects, and the record mixes the two shapes a real write produces --
/// one object that already had the tag (an UPDATE) and two that did not (a
/// DELETE). Then the second object's row is changed behind the undo's back, so
/// the record goes stale. The undo must restore NOTHING: not the first object's
/// update, not the third object's delete.
///
/// This is the case a loop cannot get right. The loop restores what it has
/// already walked past before it reaches the stale object, leaving the user with
/// a half-undone write and a refusal -- and the refusal tells them to pick a
/// different action when the action is the one they just pressed.
#[tokio::test]
async fn a_stale_object_stops_every_object_in_the_record_not_just_its_own() {
    on_each_store!(|store| {
        let (tag, a) = tag_and_object(&store, "mix-a").await;
        let (_, b) = tag_and_object(&store, "mix-b").await;
        let (_, c) = tag_and_object(&store, "mix-c").await;

        // `a` already has the tag, so undoing it is an UPDATE.
        put_tag_row(&store, &a, &tag, Some(0.3), Some("manual")).await;

        // The write, which is the state every entry's `after` describes. `b` and
        // `c` had no row before it, so undoing them is a DELETE. Without this
        // step the record is stale the moment it is written, and the undo
        // refusing it would be the module working rather than failing.
        for o in [&a, &b, &c] {
            put_tag_row(&store, o, &tag, Some(0.9), Some("bulk")).await;
        }

        let over = UndoEntry {
            object_id: a.clone(),
            before: TagState {
                row_existed: true,
                confidence: Some(0.3),
                source: Some("manual".into()),
                created_at: None,
            },
            after: TagState {
                row_existed: true,
                confidence: Some(0.9),
                source: Some("bulk".into()),
                created_at: None,
            },
        };
        store
            .record_undo(&Write {
                id: "r-mix".into(),
                caller: "me".into(),
                action: "bulk.tag.add".into(),
                tag_id: tag.clone(),
                requested: 3,
                matched: 3,
                entries: vec![over, created_entry(&b), created_entry(&c)],
            })
            .await
            .expect("record");

        // Somebody edits `b` after the write. The record is stale, and `b` is the
        // object that moved.
        put_tag_row(&store, &b, &tag, Some(0.7), Some("other")).await;

        let err = store
            .undo("r-mix", "me")
            .await
            .expect_err("a stale object refuses the whole record");
        assert_eq!(
            err,
            UndoError::Superseded {
                object_id: b.clone()
            },
            "the refusal names the object that moved"
        );

        // The point. `a` still carries exactly what the write left, and `c`'s
        // row still exists. A loop would have restored both before reaching `b`.
        let a_now = undo::tag_state(&store, &a, &tag).await.expect("read a");
        assert_eq!(
            a_now.confidence,
            Some(0.9),
            "a is untouched: the update half of the record did not apply"
        );
        assert_eq!(a_now.source, Some("bulk".into()));
        assert_eq!(
            tag_row_count(&store, &c, &tag).await,
            1,
            "c is untouched: the delete half of the record did not apply"
        );
    });
}

/// The other half of the claim: with nothing stale, a record spanning both
/// shapes restores all of them, and the two shapes do not interfere.
///
/// An UPDATE and a DELETE over a shared `VALUES` block is where a bind-order or
/// column-order slip shows up: the wrong column written, or one entry's values
/// landing on another's row. The two updating objects therefore carry DIFFERENT
/// prior values, so a slip is visible rather than merely possible.
#[tokio::test]
async fn a_record_mixing_both_shapes_restores_every_object() {
    on_each_store!(|store| {
        let (tag, a) = tag_and_object(&store, "both-a").await;
        let (_, b) = tag_and_object(&store, "both-b").await;
        let (_, c) = tag_and_object(&store, "both-c").await;
        let (_, d) = tag_and_object(&store, "both-d").await;

        put_tag_row(&store, &a, &tag, Some(0.11), Some("import")).await;
        put_tag_row(&store, &d, &tag, Some(0.77), Some("import")).await;
        for o in [&a, &b, &c, &d] {
            put_tag_row(&store, o, &tag, Some(0.9), Some("bulk")).await;
        }

        let over = |object_id: &str, before_conf: f64| UndoEntry {
            object_id: object_id.to_string(),
            before: TagState {
                row_existed: true,
                confidence: Some(before_conf),
                source: Some("import".into()),
                created_at: None,
            },
            after: TagState {
                row_existed: true,
                confidence: Some(0.9),
                source: Some("bulk".into()),
                created_at: None,
            },
        };
        store
            .record_undo(&Write {
                id: "r-both".into(),
                caller: "me".into(),
                action: "bulk.tag.add".into(),
                tag_id: tag.clone(),
                requested: 4,
                matched: 4,
                entries: vec![
                    over(&a, 0.11),
                    over(&d, 0.77),
                    created_entry(&b),
                    created_entry(&c),
                ],
            })
            .await
            .expect("record");

        let n = store.undo("r-both", "me").await.expect("the undo applies");
        assert_eq!(n, 4, "every entry in the record is restored");

        // Each updated object comes back carrying ITS OWN prior values.
        let a_now = undo::tag_state(&store, &a, &tag).await.expect("read a");
        assert_eq!(a_now.confidence, Some(0.11), "a's own value, not d's");
        assert_eq!(a_now.source, Some("import".into()));
        let d_now = undo::tag_state(&store, &d, &tag).await.expect("read d");
        assert_eq!(d_now.confidence, Some(0.77), "d's own value, not a's");
        assert_eq!(d_now.source, Some("import".into()));

        // And both deleted rows are gone rather than blanked.
        assert_eq!(tag_row_count(&store, &b, &tag).await, 0, "b's row is gone");
        assert_eq!(tag_row_count(&store, &c, &tag).await, 0, "c's row is gone");
    });
}

/// The ordering claim, and it is a claim about a failure that cannot be
/// triggered from a test.
///
/// [`Store::prepare_undo`] writes the record BEFORE the write it reverses, so a
/// crash between them leaves a record describing a write that never happened.
/// This is safe only because undoing that record is refused — the row is not in
/// the `after` state the record claims, so the staleness check stops it. If that
/// check ever accepted it, the residue would restore `before` onto a row nobody
/// edited, and the "safe direction" would be the unsafe one after all.
///
/// So the residue is built directly here: a record, a write that never ran, and
/// the press.
#[tokio::test]
async fn a_record_whose_write_never_happened_is_refused_rather_than_replayed() {
    on_each_store!(|store| {
        let (tag, obj) = tag_and_object(&store, "residue").await;

        // The row as it was BEFORE, and no write ever touches it.
        put_tag_row(&store, &obj, &tag, Some(0.4), Some("manual")).await;

        store
            .record_undo(&Write {
                id: "r-residue".into(),
                caller: "me".into(),
                action: "bulk.tag.add".into(),
                tag_id: tag.clone(),
                requested: 1,
                matched: 1,
                // The record claims the write left 0.9/bulk. It did not.
                entries: vec![UndoEntry {
                    object_id: obj.clone(),
                    before: TagState {
                        row_existed: true,
                        confidence: Some(0.4),
                        source: Some("manual".into()),
                        created_at: None,
                    },
                    after: TagState {
                        row_existed: true,
                        confidence: Some(0.9),
                        source: Some("bulk".into()),
                        created_at: None,
                    },
                }],
            })
            .await
            .expect("record");

        // The record is OFFERABLE — expiry and ownership both pass, because both
        // are properties of the record and the write never happened. Only the
        // staleness check can catch this, which is why it is the one that has to.
        let offered = store
            .undoable("me")
            .await
            .expect("read")
            .iter()
            .any(|r| r.id == "r-residue");
        assert!(offered, "nothing about the record itself is wrong");

        // And pressing it is refused, with the row untouched.
        let err = store
            .undo("r-residue", "me")
            .await
            .expect_err("a record whose write never happened must not replay");
        assert_eq!(
            err,
            UndoError::Superseded {
                object_id: obj.clone()
            }
        );

        let now = undo::tag_state(&store, &obj, &tag).await.expect("read");
        assert_eq!(now.confidence, Some(0.4), "the row is exactly as it was");
        assert_eq!(now.source, Some("manual".into()));
    });
}

/// `prepare_undo` writes no record for a write that reached nothing.
///
/// A record with zero entries is offerable and undoes nothing, so the user
/// presses a button whose only effect is to disappear. The `Nothing` variant is
/// what stops that, and it has to be a distinct outcome rather than an empty
/// list, because "reached nothing" and "recorded" are different facts.
#[tokio::test]
async fn preparing_undo_for_a_write_that_reached_nothing_records_nothing() {
    on_each_store!(|store| {
        let (tag, _obj) = tag_and_object(&store, "empty").await;
        let w = Write {
            id: "r-empty".into(),
            caller: "me".into(),
            action: "bulk.tag.add".into(),
            tag_id: tag.clone(),
            requested: 0,
            matched: 0,
            entries: vec![],
        };
        let prepared = store.prepare_undo(&w, vec![]).await.expect("prepare");
        assert_eq!(prepared, PreparedUndo::Nothing, "no record is written");

        assert!(
            store.undo_record("r-empty").await.expect("read").is_none(),
            "and nothing is left in the table for the user to find"
        );
    });
}

/// And a write that DID reach objects records them, before the write runs.
#[tokio::test]
async fn preparing_undo_records_the_entries_before_the_write() {
    on_each_store!(|store| {
        let (tag, obj) = tag_and_object(&store, "prep").await;
        put_tag_row(&store, &obj, &tag, Some(0.3), Some("manual")).await;
        let entry = UndoEntry {
            object_id: obj.clone(),
            before: TagState {
                row_existed: true,
                confidence: Some(0.3),
                source: Some("manual".into()),
                created_at: None,
            },
            after: TagState {
                row_existed: true,
                confidence: Some(0.9),
                source: Some("bulk".into()),
                created_at: None,
            },
        };
        let w = Write {
            id: "r-prep".into(),
            caller: "me".into(),
            action: "bulk.tag.add".into(),
            tag_id: tag.clone(),
            requested: 1,
            matched: 1,
            entries: vec![entry.clone()],
        };
        let prepared = store.prepare_undo(&w, vec![entry]).await.expect("prepare");
        assert!(
            matches!(prepared, PreparedUndo::Recorded { .. }),
            "the record is on disk before the write runs"
        );
        // Readable at once, which is what "before the write" has to mean: the
        // record is not deferred or batched.
        let rec = store
            .undo_record("r-prep")
            .await
            .expect("read")
            .expect("the record exists");
        assert_eq!(rec.entries.len(), 1);
        assert_eq!(rec.entries[0].object_id, obj);
    });
}
