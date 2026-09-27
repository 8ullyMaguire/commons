//! Object creation, and the two §10.10 actions built on it.
//!
//! # The claims, and why each one is easy to get wrong
//!
//! - **The id is derived, not generated.** A create that minted a UUID would
//!   make `create_all_missing` impossible: re-running it would create a second
//!   copy of everything, which is the exact bug the action exists to prevent.
//! - **The id ignores fields that change.** Marking a reviewed object unreviewed
//!   must not change its id, or every tag, relation, folder membership and undo
//!   entry pointing at it dangles. This is the claim most likely to be broken
//!   by a well-meaning future edit that adds `organized` to the hash.
//! - **A new object is visible to its creator.** Without a `consent_record` the
//!   consent clause's `c.tier IN (...)` matches nothing, so the object is
//!   created and then unfindable — by the person who just made it.
//! - **`created` and `existing` are different numbers.** A single "written: N"
//!   cannot tell a user that 35 of their 40 pasted rows were already in the
//!   library.
//! - **Field boundaries are unambiguous.** `("ab", "c")` and `("a", "bc")` must
//!   not produce one id.
//!
//! Every fixture seeds its own rows with a UUID-derived name. `create_all_missing`
//! is a *global* property of the `object` table — "does this id already exist?"
//! — so a fixture that named a shared row would make another test's rows decide
//! this test's counts.

#[path = "harness/mod.rs"]
mod harness;
use harness::{postgres_store, sqlite_store};

use commons_core::Role;
use commons_store::create::{
    create, create_all_missing, create_all_missing_ids, dedupe, derive_id, redundant_count,
    CreateOutcome, ObjectDraft, DEFAULT_ORGANIZED,
};
use commons_store::db::Store;
use commons_store::filter_ast::CallerId;

macro_rules! on_each_store {
    (|$store:ident| $body:block) => {{
        for $store in [sqlite_store().await, postgres_store().await] {
            $body
        }
    }};
}

/// One typed row out of whichever engine is under test.
macro_rules! run_query_as {
    ($store:expr, $q:expr, $ty:ty, $($b:expr),* $(,)?) => {{
        let sql: &str = $q;
        macro_rules! go {
            ($p:expr, $query:expr) => {{
                let mut qb = sqlx::query_as::<_, $ty>($query);
                $( qb = qb.bind($b); )*
                qb.fetch_one($p)
                    .await
                    .unwrap_or_else(|e| panic!("{sql}: {e}"))
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
/// A seed name no other fixture in this binary can also be using.
fn seed() -> String {
    format!("c{}", uuid::Uuid::new_v4().simple())
}

/// A draft that is unique to this test, so `create_all_missing` cannot be
/// satisfied by another test's rows.
fn unique_draft(tag: &str) -> ObjectDraft {
    ObjectDraft::new("scene").title(format!("{tag}-{}", seed()))
}

fn owner() -> CallerId {
    CallerId {
        account_id: Some(uuid::Uuid::new_v4().to_string()),
        ..CallerId::anonymous()
    }
}

// ---------------------------------------------------------------------------
// The id
// ---------------------------------------------------------------------------

#[test]
fn the_id_is_derived_so_the_same_fields_always_give_the_same_id() {
    let a = ObjectDraft::new("scene").title("A Title");
    let b = ObjectDraft::new("scene").title("A Title");
    assert_eq!(derive_id(&a), derive_id(&b));
    // And stable across calls, which is the property `create_all_missing`
    // depends on: if it were not, re-running the import would create copies.
    assert_eq!(derive_id(&a), derive_id(&a));
}

#[test]
fn the_id_is_prefixed_so_it_is_distinguishable_from_a_bare_hash() {
    // `file.hash_blake3` holds a bare hex hash, and a locator string may carry
    // an object id next to one. A prefix costs three bytes and removes an
    // ambiguity that would otherwise be a class of bug rather than an incident.
    let id = derive_id(&ObjectDraft::new("scene").title("x"));
    assert!(id.starts_with("o-"), "got {id}");
    assert!(id.len() > 10, "the hash should not be truncated to nothing");
}

#[test]
fn different_fields_give_different_ids() {
    let base = ObjectDraft::new("scene").title("T").date("2026-01-01");
    assert_ne!(
        derive_id(&base),
        derive_id(&ObjectDraft::new("clip").title("T").date("2026-01-01"))
    );
    assert_ne!(
        derive_id(&base),
        derive_id(&ObjectDraft::new("scene").title("U").date("2026-01-01"))
    );
    assert_ne!(
        derive_id(&base),
        derive_id(&ObjectDraft::new("scene").title("T").date("2026-02-01"))
    );
}

/// The concatenation ambiguity, and it is a real one.
///
/// The first version of this test asserted that `("ab", "c")` and `("a", "bc")`
/// must differ. They do — and that assertion could not tell a correct
/// implementation from a comma-joined one, because with a comma separator
/// `"scene,ab,c,,"` and `"scene,a,bc,,"` are plainly different strings. The
/// mutation passed it. A test that names a boundary must be built from the
/// boundary's actual arithmetic, not from a pair that merely looks adjacent.
///
/// The collision needs a *separator character inside one of the fields*:
/// `title="a", date="b,c"` joins to `scene,a,b,c,,` and `title="a,b",
/// date="c"` joins to the same string. Two different objects, one id, and the
/// second create is a silent no-op.
#[test]
fn field_boundaries_are_unambiguous() {
    let split_across = ObjectDraft::new("scene").title("a").date("b,c");
    let split_inside = ObjectDraft::new("scene").title("a,b").date("c");
    assert_ne!(
        derive_id(&split_across),
        derive_id(&split_inside),
        "a separator inside a field must not be mistaken for a field boundary"
    );

    // The same across a different adjacent pair, so the property is not an
    // accident of which columns happen to sit next to each other.
    let by_producer = ObjectDraft {
        producer_id: Some("c".to_owned()),
        ..ObjectDraft::new("scene").title("a").date("b")
    };
    let by_date = ObjectDraft::new("scene").title("a").date("b,c");
    assert_ne!(derive_id(&by_producer), derive_id(&by_date));

    // And through `description`, which is the last field and so has nothing
    // after it to be confused with.
    let desc_split = ObjectDraft {
        description: Some("x,y".to_owned()),
        ..ObjectDraft::new("scene").title("z")
    };
    let desc_unsplit = ObjectDraft {
        description: Some("z".to_owned()),
        ..ObjectDraft::new("scene").title("z")
    };
    assert_ne!(derive_id(&desc_split), derive_id(&desc_unsplit));
}

/// The claim most likely to be broken by a well-meaning edit: an id must not
/// change when a field that CHANGES OVER TIME is modified.
#[test]
fn the_id_ignores_fields_that_change_after_creation() {
    let mut draft = ObjectDraft::new("scene").title("Stable Title");
    let before = derive_id(&draft);

    // Every one of these is a real thing a user does to an object after it
    // exists. If any of them changed the id, every row referencing the object
    // -- tags, relations, folder membership, undo entries -- would dangle.
    draft.organized = Some("reviewed".to_owned());
    assert_eq!(derive_id(&draft), before, "organized must not be in the id");

    // `rating_sum` and `rating_count` are not on the draft at all, precisely
    // because they change on every rating.
    assert_eq!(derive_id(&draft), before);
}

/// A NULL field and an empty-string field are different rows, so they must be
/// different ids. A create that normalised `None` to `''` would produce an
/// object that matches an empty-title search forever.
#[test]
fn a_missing_field_and_an_empty_one_are_different_objects() {
    let none = ObjectDraft::new("scene").title("T");
    let empty = ObjectDraft {
        title: Some(String::new()),
        ..ObjectDraft::new("scene").title("T")
    };
    assert_ne!(derive_id(&none), derive_id(&empty));
}

// ---------------------------------------------------------------------------
// Dedupe
// ---------------------------------------------------------------------------

#[test]
fn dedupe_counts_how_many_drafts_share_an_id() {
    let a = ObjectDraft::new("scene").title("Same");
    let b = ObjectDraft::new("scene").title("Same");
    let c = ObjectDraft::new("scene").title("Different");

    let counts = dedupe(&[a.clone(), b, c]);
    assert_eq!(counts.len(), 2, "two distinct ids");
    assert_eq!(counts[&derive_id(&a)], 2, "the shared one is listed twice");
    let triple = vec![a.clone(); 3];
    assert_eq!(redundant_count(&triple), 2);
    let one = [a.clone()];
    assert_eq!(redundant_count(&one), 0);
}

#[test]
fn dedupe_of_nothing_is_empty_and_zero() {
    assert!(dedupe(&[]).is_empty());
    assert_eq!(redundant_count(&[]), 0);
}

// ---------------------------------------------------------------------------
// Create
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_created_object_exists_with_its_fields() {
    on_each_store!(|store| {
        let draft = unique_draft("fields");
        let id = create(&store, &draft, &owner()).await.unwrap();

        let (kind, title, description): (String, String, Option<String>) = run_query_as!(
            &store,
            "SELECT o.kind, o.title, o.description FROM object o
             JOIN consent_record c ON c.object_id = o.id
             WHERE o.id = ? AND c.tier IN ('unverified','self_published')",
            (String, String, Option<String>),
            &id,
        );
        assert_eq!(kind, "scene");
        assert_eq!(title, draft.title.expect("the draft has a title"));
        assert_eq!(description, None, "an unset description stays NULL, not ''");
    });
}

#[tokio::test]
async fn creating_the_same_draft_twice_creates_one_object() {
    on_each_store!(|store| {
        let draft = unique_draft("twice");
        let first = create(&store, &draft, &owner()).await.unwrap();
        let second = create(&store, &draft, &owner()).await.unwrap();
        assert_eq!(first, second, "the id is derived, so both calls agree");

        let (n,): (i64,) = run_query_as!(
            &store,
            "SELECT COUNT(*) FROM object o
             JOIN consent_record c ON c.object_id = o.id
             WHERE o.id = ? AND c.tier IN ('unverified','self_published')",
            (i64,),
            &first
        );
        assert_eq!(n, 1, "not two rows");
    });
}

/// The claim that would be invisible without this test: a new object with no
/// `consent_record` is invisible to EVERY tier list, so it would be created
/// and then unfindable -- including by the person who made it.
#[tokio::test]
async fn a_created_object_is_visible_to_its_creator() {
    on_each_store!(|store| {
        let draft = unique_draft("visible");
        let id = create(&store, &draft, &owner()).await.unwrap();

        // A consent row exists...
        let (tier,): (String,) = run_query_as!(
            &store,
            "SELECT tier FROM consent_record WHERE object_id = ?",
            (String,),
            &id
        );
        // ...and it is `unverified`, NOT `self_published`. Creating an object is
        // not an attestation about it, and a create that published itself would
        // let a paste of scraped CSV titles publish itself.
        assert_eq!(tier, "unverified");

        // And the object is reachable through the consent clause as its owner.
        let (visible,): (i64,) = run_query_as!(
            &store,
            "SELECT COUNT(*) FROM object o JOIN consent_record c ON c.object_id = o.id
             WHERE o.id = ? AND c.tier IN ('unverified','self_published','performer_claimed','third_party_permitted')",
            (i64,),
            &id
        );
        assert_eq!(visible, 1);
    });
}

#[tokio::test]
async fn a_visitor_cannot_see_a_freshly_created_object() {
    on_each_store!(|store| {
        let draft = unique_draft("hidden");
        let id = create(&store, &draft, &owner()).await.unwrap();

        // The same clause, restricted to the PUBLIC tiers. `unverified` is
        // deliberately absent from that list, so a brand-new object is the
        // owner's alone until they attest to it.
        let (visible,): (i64,) = run_query_as!(
            &store,
            "SELECT COUNT(*) FROM object o JOIN consent_record c ON c.object_id = o.id
             WHERE o.id = ? AND c.tier IN ('self_published','performer_claimed','third_party_permitted')",
            (i64,),
            &id
        );
        assert_eq!(visible, 0, "a new object is not public");
    });
}

// ---------------------------------------------------------------------------
// create_all_missing -- the counts
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_all_missing_reports_created_and_existing_separately() {
    on_each_store!(|store| {
        let fresh = unique_draft("fresh");
        let older = unique_draft("older");
        // Put one of them in the library first.
        create(&store, &older, &owner()).await.unwrap();

        let outcome = create_all_missing(&store, &[fresh.clone(), older.clone()], &owner())
            .await
            .unwrap();
        assert_eq!(outcome.created, 1, "one was new");
        assert_eq!(outcome.existing, 1, "one was already there");
        assert_eq!(outcome.refused, 0);
        assert_eq!(outcome.total(), 2);
        assert!(outcome.wrote_anything());
    });
}

/// The idempotence claim, stated as the thing a user would notice.
#[tokio::test]
async fn create_all_missing_twice_creates_nothing_the_second_time() {
    on_each_store!(|store| {
        let drafts = vec![unique_draft("idem1"), unique_draft("idem2")];

        let first = create_all_missing(&store, &drafts, &owner()).await.unwrap();
        assert_eq!(first.created, 2);
        assert_eq!(first.existing, 0);

        let second = create_all_missing(&store, &drafts, &owner()).await.unwrap();
        assert_eq!(second.created, 0, "the second run creates nothing");
        assert_eq!(second.existing, 2);
        assert!(!second.wrote_anything());
    });
}

#[tokio::test]
async fn create_all_missing_handles_nothing() {
    on_each_store!(|store| {
        let outcome = create_all_missing(&store, &[], &owner()).await.unwrap();
        assert_eq!(outcome, CreateOutcome::default());
        assert!(!outcome.wrote_anything());
        assert_eq!(outcome.total(), 0);
    });
}

#[tokio::test]
async fn create_all_missing_ids_returns_each_id_once() {
    on_each_store!(|store| {
        let a = unique_draft("ids1");
        let b = unique_draft("ids2");
        // The same draft twice, as a pasted list would be.
        let (outcome, ids) =
            create_all_missing_ids(&store, &[a.clone(), b.clone(), a.clone()], &owner())
                .await
                .unwrap();

        assert_eq!(outcome.created, 2, "the duplicate created nothing");
        assert_eq!(ids.len(), 2, "and is reported once");
        // Membership, not order: `a` and `b` are hashed, not named, so which id
        // sorts first is a property of the content and not something to assert.
        assert!(ids.contains(&derive_id(&a)), "a's id is in the list");
        assert!(ids.contains(&derive_id(&b)), "b's id is in the list");
        // Sorted, so two runs agree on the order -- insertion order does not
        // survive the two engines and an import must be comparable.
        assert!(ids.windows(2).all(|w| w[0] < w[1]), "ids come back sorted");
    });
}

/// `organized` is bound with `COALESCE(?, organized)`, so a `None` must leave
/// the schema default. Repeated in one place and drifted from the other is the
/// bug this avoids; a test is the only thing that notices.
#[tokio::test]
async fn an_unset_organized_takes_the_schema_default() {
    on_each_store!(|store| {
        let draft = unique_draft("organized-default");
        let id = create(&store, &draft, &owner()).await.unwrap();

        let (organized,): (String,) = run_query_as!(
            &store,
            "SELECT o.organized FROM object o
             JOIN consent_record c ON c.object_id = o.id
             WHERE o.id = ? AND c.tier IN ('unverified','self_published')",
            (String,),
            &id
        );
        assert_eq!(
            organized, "unreviewed",
            "the schema default, not NULL and not ''"
        );
    });
}

#[tokio::test]
async fn an_explicit_organized_is_stated_not_defaulted() {
    on_each_store!(|store| {
        let draft = ObjectDraft {
            organized: Some("organized".to_owned()),
            ..unique_draft("organized-set")
        };
        let id = create(&store, &draft, &owner()).await.unwrap();

        let (organized,): (String,) = run_query_as!(
            &store,
            "SELECT o.organized FROM object o
             JOIN consent_record c ON c.object_id = o.id
             WHERE o.id = ? AND c.tier IN ('unverified','self_published')",
            (String,),
            &id
        );
        assert_eq!(organized, "organized");
    });
}

// ---------------------------------------------------------------------------
// A steward is not the owner
// ---------------------------------------------------------------------------

/// The same shape of bug `filter_ast`'s own docs record for `steward()`: a
/// permission granted for one reason being used to justify another.
#[tokio::test]
async fn a_steward_may_still_create_and_the_object_is_still_unverified() {
    on_each_store!(|store| {
        let draft = unique_draft("steward");
        let steward = CallerId {
            account_id: Some(uuid::Uuid::new_v4().to_string()),
            role: Role::Steward,
            ..CallerId::anonymous()
        };
        let id = create(&store, &draft, &steward).await.unwrap();

        // Creating is allowed, and the tier is still `unverified` -- a steward
        // creating an object is not a moderation act and does not publish it.
        let (tier,): (String,) = run_query_as!(
            &store,
            "SELECT tier FROM consent_record WHERE object_id = ?",
            (String,),
            &id
        );
        assert_eq!(tier, "unverified");
    });
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// The constant and the migration must not drift.
///
/// `insert_if_absent` writes `DEFAULT_ORGANIZED` rather than letting the column
/// default apply, because an INSERT cannot coalesce against a column. So the
/// schema's `DEFAULT` and this constant are now two statements of the same
/// fact, and a change to the migration alone would silently create every new
/// object as `organized` -- a tier nothing looks at, so nothing fails.
#[tokio::test]
async fn the_default_organized_matches_the_schema() {
    on_each_store!(|store| {
        // SQLite has no `information_schema`; `pragma_table_info` is its own
        // answer. Two typed helpers rather than one match, because a bare
        // `sqlx::query(..)` in a match arm takes its database type from the
        // *other* arm and the two do not agree.
        let default: String = match &store {
            Store::Sqlite(p) => sqlite_column_default(p, "object", "organized").await,
            Store::Postgres(p) => postgres_column_default(p, "object", "organized").await,
        };
        // Postgres quotes the literal and appends the cast; SQLite quotes it
        // and stops. Anything after the closing quote is not part of the value.
        let schema_default = match default.split_once('\'') {
            Some((_, rest)) => rest.split('\'').next().unwrap_or_default().to_owned(),
            None => default.clone(),
        };
        assert_eq!(
            schema_default, DEFAULT_ORGANIZED,
            "create.rs and 0001_core.sql disagree about a new object's tier"
        );
    });
}

async fn sqlite_column_default(p: &sqlx::SqlitePool, table: &str, column: &str) -> String {
    use sqlx::Row as _;
    sqlx::query(&format!(
        "SELECT dflt_value FROM pragma_table_info('{table}') WHERE name = '{column}'"
    ))
    .fetch_one(p)
    .await
    .unwrap_or_else(|e| panic!("pragma_table_info({table}): {e}"))
    .get::<Option<String>, _>("dflt_value")
    .unwrap_or_else(|| panic!("{table}.{column} has no default"))
}

/// The column's DEFAULT as the schema states it.
///
/// `information_schema.columns.dflt_value` is `NULL` for defaults that are not
/// a plain literal, and this one is a literal on both engines -- but the two
/// spell it differently: Postgres reports `'unreviewed'::text` and SQLite
/// reports `'unreviewed'`, so the comparison strips the cast.
async fn postgres_column_default(p: &sqlx::PgPool, table: &str, column: &str) -> String {
    let (default,): (Option<String>,) = sqlx::query_as(
        "SELECT column_default FROM information_schema.columns
         WHERE table_name = $1 AND column_name = $2",
    )
    .bind(table)
    .bind(column)
    .fetch_one(p)
    .await
    .unwrap_or_else(|e| panic!("information_schema for {table}.{column}: {e}"));
    default.unwrap_or_else(|| panic!("{table}.{column} has no default"))
}
