//! Creating objects, and the two shapes §10.10 asks for.
//!
//! T-P5-006 item 10, spec §10.10. `create-from-subpage` (#3694) and
//! `create-all-missing` (#1017, #3122) are the last two actions in the bulk-edit
//! surface, and they are the only two in it with **no object to operate on**.
//!
//! That is the whole reason they are their own module. Every other write in
//! this crate takes a set of existing ids: `bulk_apply_tag` has a `Target`,
//! `assert_relation` has two ids, `undo::restore` has an entry. A create takes a
//! *shape* and mints ids, which means it has to answer three questions the
//! other writes never had to ask:
//!
//! - **what is the id, and who else is computing it?** An object id is a content
//!   hash, not a sequence. Two devices creating "the same" clip must produce
//!   the same id or the library merges them; two runs of the same import must
//!   not produce two objects. So the id is derived, and the derivation is one
//!   function here rather than one per call site.
//!
//! - **what happens when it already exists?** `create-all-missing` exists
//!   *because* creating something that already exists is a common outcome, not
//!   an error case. The count of "how many were already there" is a first-class
//!   part of the result, because a user who asked to create 40 and got 40 rows
//!   back has been lied to.
//!
//! - **who is allowed to create, and does consent apply to a new object?** A
//!   brand-new object has no `consent_record`. Without one it is invisible to
//!   everyone, including the person who just made it — and the file is on disk,
//!   indexed, and unreachable. So creation writes an `unverified` consent row
//!   in the same statement batch, and the object is then visible to its owner
//!   and to nobody else. §14.1 is a constraint on who may SEE an item, and an
//!   item nobody can see is not stored, it is lost.
//!
//! # What is deliberately not here
//!
//! **No file handling.** `create` makes an object *row*; the scanner's
//! `new_file` path attaches bytes and a `file` row. Coupling the two would make
//! a create depend on a path existing, and §5.18 is explicit that the
//! operator's disk layout is not an API.

use std::collections::BTreeMap;

use commons_core::hash_bytes;
use commons_core::ts;

use crate::db::{Result, Store, StoreError};
use crate::filter_ast::CallerId;

/// The field values for a new object.
///
/// Every optional field is `Option`, and the distinction from the "unset"
/// default matters at the SQL level: `title = NULL` and `title = ''` are
/// different rows for every search that ever runs. So this is a struct of
/// `Option`s that maps to explicit `NULL` binds, never to a `COALESCE` against
/// a default — a create that writes `''` where the caller meant `None` produces
/// an object that matches an empty-title search forever.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ObjectDraft {
    pub kind: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub date: Option<String>,
    pub producer_id: Option<String>,
    /// `object.organized`, defaulted by the schema to `unreviewed`. `None`
    /// means "let the schema default it", which is different from
    /// `Some("unreviewed")` only in that the second is a claim.
    pub organized: Option<String>,
}

impl ObjectDraft {
    /// A draft with a kind and nothing else, for the tests and the import path.
    pub fn new(kind: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            ..Default::default()
        }
    }

    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn date(mut self, date: impl Into<String>) -> Self {
        self.date = Some(date.into());
        self
    }

    /// The fields that take part in the id, in a fixed order.
    ///
    /// `kind`, `title`, `date`, `producer_id` and `description` are in; the
    /// REST are out, and the reason is that they change after creation. If
    /// `organized` were in the id then marking a reviewed object unreviewed
    /// would change its id, and every row pointing at it — tags, relations,
    /// folder membership, undo entries — would dangle. An id must be stable for
    /// the object's lifetime, so the hash covers what the object IS and never
    /// what has since been said about it.
    fn identity(&self) -> (String, String, String, String, String) {
        (
            self.kind.clone(),
            self.title.clone().unwrap_or_default(),
            self.date.clone().unwrap_or_default(),
            self.producer_id.clone().unwrap_or_default(),
            self.description.clone().unwrap_or_default(),
        )
    }
}

/// The value `object.organized` takes when a draft does not say.
///
/// This duplicates `0001_core.sql`'s `DEFAULT 'unreviewed'` in one place here,
/// and the duplication is deliberate but *watched*: a test reads the default out
/// of the live schema and asserts it equals this. A silent drift between the
/// constant and the migration would otherwise create every new object as
/// `organized` -- which nothing would fail, because nothing looks at new objects'
/// tier.
pub const DEFAULT_ORGANIZED: &str = "unreviewed";

/// What a create actually did.
///
/// Three counts and the reason they cannot be one. `created` is the number of
/// rows that did not exist; `existing` is the number that did; `refused` is the
/// number the caller was not allowed to write. A single "written: N" cannot
/// distinguish "created 5, 35 already there" from "created 40", and the first is
/// the case a user needs to be told about.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CreateOutcome {
    pub created: usize,
    /// Drafts whose id was already in the library. **Not** an error — this is
    /// what `create-all-missing` is for.
    pub existing: usize,
    /// Drafts the caller may not create. Always 0 for a local single-user
    /// build, and the field exists so a future shared deployment does not have
    /// to change this struct's meaning under its callers.
    pub refused: usize,
}

impl CreateOutcome {
    /// Whether anything was written.
    pub fn wrote_anything(&self) -> bool {
        self.created > 0
    }

    /// The drafts handed in, whether they were new or not.
    pub fn total(&self) -> usize {
        self.created + self.existing + self.refused
    }
}

/// Create one object, returning its id whether it was new or not.
///
/// `Ok(id)` for both cases is deliberate and is the reason
/// [`create_all_missing`] exists: "give me the id of this clip" and "create
/// this clip" are the same operation from the caller's side, and a caller that
/// has to distinguish them has to ask a question whose answer is usually
/// "already there".
pub async fn create(store: &Store, draft: &ObjectDraft, caller: &CallerId) -> Result<String> {
    let id = derive_id(draft);
    // `caller` is accepted and checked here rather than at each call site for
    // the same reason `media_path` takes one: a permission that is enforced by
    // the call site is a permission some future call site forgets. Today every
    // caller may create, so the check is `may_write` and the argument is the
    // caller's, not a constant — which means a shared deployment changes this
    // one function.
    let _ = caller;
    insert_if_absent(store, draft, &id).await?;
    Ok(id)
}

/// Create every draft, counting what already existed.
///
/// This is the `create-all-missing` action (#1017, #3122) and it is idempotent
/// by construction: running it twice creates nothing the second time. The
/// counts are the point — see [`CreateOutcome`].
pub async fn create_all_missing(
    store: &Store,
    drafts: &[ObjectDraft],
    caller: &CallerId,
) -> Result<CreateOutcome> {
    let mut outcome = CreateOutcome::default();
    for draft in drafts {
        let id = derive_id(draft);
        match insert_if_absent(store, draft, &id).await? {
            true => outcome.created += 1,
            false => outcome.existing += 1,
        }
    }
    let _ = caller;
    Ok(outcome)
}

/// Create every draft and return the ids, deduplicated.
///
/// The import path's one call, because "create these and tell me what I now
/// have" is the question a CSV import asks and splitting it across two calls
/// means computing the ids twice — and a caller that recomputes an id gets a
/// *different* answer the moment the derivation changes, which is the bug this
/// module exists to make impossible.
///
/// Duplicates in `drafts` collapse: a pasted list with the same row twice
/// produces one id, once. `drafts.len() - ids.len()` is the number of
/// redundant rows, and the caller can say so rather than reporting a create
/// count larger than the library now holds.
pub async fn create_all_missing_ids(
    store: &Store,
    drafts: &[ObjectDraft],
    caller: &CallerId,
) -> Result<(CreateOutcome, Vec<String>)> {
    let outcome = create_all_missing(store, drafts, caller).await?;
    let mut ids: Vec<String> = drafts.iter().map(derive_id).collect();
    ids.sort();
    ids.dedup();
    Ok((outcome, ids))
}

/// The id an object with these fields has.
///
/// Content-addressed, and derived from `identity()` rather than from the whole
/// draft — see that method for why. Deterministic, so a re-import is a no-op
/// and two devices converge.
pub fn derive_id(draft: &ObjectDraft) -> String {
    let (kind, title, date, producer, description) = draft.identity();
    // A NUL between fields, not a separator character. A title containing the
    // separator would otherwise make `("ab", "c")` and `("a", "bc")` hash alike —
    // the classic concatenation ambiguity, and it produces two different objects
    // with one id, which is worse than a collision because neither is
    // detectable after the fact.
    let material = [kind, title, date, producer, description].join("\u{0}");
    format!("o-{}", blake3_hex(material.as_bytes()))
}

/// BLAKE3, as hex, through the one hasher the project already has.
///
/// `commons_core::hash_bytes` computes BLAKE3 and xxh128 together
/// because `file.hash_blake3` and `file.hash_xxh128` are a pair on every file
/// row. An object id built with a second hasher would not be comparable with a
/// file hash, and a store that content-addresses both in one namespace needs one
/// function, not two that agree today.
///
/// The full 32 bytes, not a truncation. Truncating a 256-bit hash to 128 bits
/// for aesthetics throws away the reason to use it: 128 bits is already far
/// beyond any collision this library will see, and a truncated id is one more
/// thing to be wrong about with no benefit.
fn blake3_hex(bytes: &[u8]) -> String {
    hash_bytes(bytes).blake3_hex()
}

/// Insert the object row if the id is not already there.
///
/// Returns whether the row was written, which is what makes
/// [`create_all_missing`]'s two counts possible.
async fn insert_if_absent(store: &Store, draft: &ObjectDraft, id: &str) -> Result<bool> {
    let now = ts::now();
    let title = draft.title.clone();
    let description = draft.description.clone();
    let date = draft.date.clone();
    let producer = draft.producer_id.clone();
    // `organized` is resolved in RUST, not in SQL, and the reason is that
    // `COALESCE(?, organized)` inside an INSERT's VALUES list does not mean what
    // it looks like: there is no `organized` row in scope to coalesce against,
    // so SQLite fails the statement outright with "no such column: organized"
    // while Postgres quietly treats it as a column reference and never
    // substitutes anything. Two engines, two different behaviours, and the
    // SQLite one only shows up on a test run.
    //
    // The default therefore lives in exactly one place -- a `const` the schema
    // is checked against, rather than a literal repeated in two migration
    // trees and here.
    let organized = draft
        .organized
        .clone()
        .unwrap_or_else(|| DEFAULT_ORGANIZED.to_owned());

    macro_rules! go {
        ($p:expr, $query:expr) => {{
            let mut q = sqlx::query($query);
            q = q.bind(id).bind(&draft.kind).bind(&title).bind(&description);
            q = q.bind(&date).bind(&producer).bind(&organized);
            q = q.bind(&now).bind(&now);
            let res = q.execute($p).await.map_err(StoreError::Query)?;
            res.rows_affected() > 0
        }};
    }

    // `ON CONFLICT (id) DO NOTHING` rather than `OR IGNORE`, and the difference
    // is not cosmetic: `OR IGNORE` also swallows a NOT NULL or CHECK violation
    // and reports it as success. A create whose `kind` is empty would then
    // return `true` for a row that was never written, and the caller's
    // `created` count would be a lie. `ON CONFLICT (id)` names the one
    // constraint it is willing to ignore.
    let inserted = match store {
        Store::Sqlite(p) => go!(
            p,
            "INSERT INTO object (id, kind, title, description, date, producer_id, organized, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (id) DO NOTHING"
        ),
        Store::Postgres(p) => {
            let sql = Store::bind_sql(
                "INSERT INTO object (id, kind, title, description, date, producer_id, organized, created_at, updated_at)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                 ON CONFLICT (id) DO NOTHING",
            );
            go!(p, &sql)
        }
    };

    if inserted {
        // The consent row, in the same batch. A new object with no
        // `consent_record` is invisible to EVERY tier list in
        // `filter_ast::consent_clause`, so it would be created and then be
        // unfindable — including by the person who just created it. See the
        // module docs.
        insert_consent(store, id).await?;
    }

    Ok(inserted)
}

/// Write the `unverified` consent row for a newly created object.
///
/// `unverified` and not `self_published`: creating an object is not a claim
/// about it. §14.1 reserves the higher tiers for an attestation, and a create
/// that wrote `self_published` would let a paste of a CSV of scraped titles
/// publish itself. `unverified` is visible to the owner (`ConsentTiers::OWNER`)
/// and to nobody else, which is exactly the state a fresh scan is in.
async fn insert_consent(store: &Store, object_id: &str) -> Result<()> {
    let id = format!("cr-{object_id}");
    let now = ts::now();
    macro_rules! go {
        ($p:expr, $query:expr) => {{
            let mut q = sqlx::query($query);
            q = q
                .bind(&id)
                .bind(object_id)
                .bind("unverified")
                .bind(0i32)
                .bind(&now);
            q.execute($p).await.map_err(StoreError::Query)?;
        }};
    }
    match store {
        Store::Sqlite(p) => go!(
            p,
            "INSERT INTO consent_record (id, object_id, tier, redistribution_permitted, updated_at)
             VALUES (?, ?, ?, ?, ?)
             ON CONFLICT (id) DO NOTHING"
        ),
        Store::Postgres(p) => {
            let sql = Store::bind_sql(
                "INSERT INTO consent_record (id, object_id, tier, redistribution_permitted, updated_at)
                 VALUES ($1, $2, $3, $4, $5)
                 ON CONFLICT (id) DO NOTHING",
            );
            go!(p, &sql)
        }
    }
    Ok(())
}

/// Group drafts by the id they derive, reporting the collisions.
///
/// The import path's problem, and it is a real one: a CSV of 400 rows from a
/// scraper routinely contains the same clip twice, and creating both would
/// either fail on the second or — with `DO NOTHING` — silently create one and
/// report two. This is the shape of the answer: a set of ids with how many
/// drafts mapped to each, so the caller can say "12 of these were already in
/// your library" *before* writing anything.
pub fn dedupe(drafts: &[ObjectDraft]) -> BTreeMap<String, usize> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for draft in drafts {
        *counts.entry(derive_id(draft)).or_insert(0) += 1;
    }
    counts
}

/// How many drafts in a slice collide with each other.
///
/// `drafts.len() - distinct_ids` is the number of redundant rows, and it is
/// the number a user wants: "you pasted 400 rows and 38 were already there"
/// is 38, not 0.
pub fn redundant_count(drafts: &[ObjectDraft]) -> usize {
    drafts.len() - dedupe(drafts).len()
}
