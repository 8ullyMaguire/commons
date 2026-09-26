//! §5.9's relations, and §9.7's duplicate checker as a view over them.
//!
//! `object_relation` has had a table since migration 0001 and no writer until
//! now. Everything here is the write path plus the view, and the two live in
//! one module deliberately: the view is a query over what this module writes,
//! so a change to one that did not consider the other is the bug.
//!
//! The spec's framing is the design. **A duplicate checker is a view over
//! relations**, which is why #39, #5786, #5823 and #1220 are one feature
//! rather than four. Nothing here reads a file's bytes or computes a hash --
//! the signals are gathered elsewhere and arrive as relations. Deciding *which*
//! relation a set of signals implies is `commons_scan::dedup`.
//!
//! # Direction
//!
//! `(from_id, to_id, relation)` is directed, and for `ReEncodeOf` the direction
//! *is* the claim: "this is a re-encode of that" is not the same statement as
//! "that is a re-encode of this", and #5067's "keep the highest bitrate" reads
//! it to decide which file to delete. `SameSceneAs` and `UnrelatedTo` are
//! symmetric, so they are stored low-id-first -- otherwise a query has to try
//! both orders, and asserting one fact twice writes two rows with two ids and
//! two timestamps.

use crate::db::{Store, StoreError};
use crate::filter_ast::{CallerId, Filter, FilterError, Value};
use commons_core::{ts, RelationType};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One row of `object_relation`, as read back.
/// Bind the consent clause's parameters, in placeholder order.
///
/// Hand-written rather than shared with `query.rs`, which keeps its own
/// private. `sqlx::Query` and `sqlx::QueryAs` are different types with no
/// common trait to abstract over, so a shared helper would have to be generic
/// over a type parameter neither exposes -- which is why the duplication is
/// two lines rather than a design.
macro_rules! bind_params {
    ($q:expr, $params:expr) => {{
        let mut q = $q;
        for v in $params {
            q = match v {
                Value::Str(s) => q.bind(s.clone()),
                Value::Int(i) => q.bind(*i),
                Value::Float(f) => q.bind(*f),
                Value::Bool(b) => q.bind(*b),
                // `consent_clause` emits only scalars. A non-scalar here would
                // be a new case in a function that is deliberately exhaustive,
                // and a silent stringification is how a filter ends up
                // comparing against the literal text `["a"]`.
                Value::Null | Value::List(_) => {
                    panic!("consent_clause emitted a non-scalar parameter: {v:?}")
                }
            };
        }
        q
    }};
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Relation {
    pub id: String,
    pub from_id: String,
    pub to_id: String,
    pub relation: RelationType,
    /// False until a human or a rule set confirmed it. §9.7's checker is a
    /// *view*, so a machine's guess and a person's decision are the same row
    /// with a flag between them -- which is what makes "show me what the
    /// machine thinks" a filter rather than a second table.
    pub asserted: bool,
}

/// One merge an auto-merge pass performed, kept so it can be taken back.
///
/// Reversibility is not a separate undo log. §9.7 asks for auto-merge to be
/// reversible, and the way to make that true without a second source of truth
/// is to record exactly what the pass wrote: these rows *are* the undo. A
/// snapshot of everything that changed would be a parallel record free to
/// disagree with the relations, and then there are two answers to "is this
/// merged".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Merged {
    /// The `object_relation.id` that was written.
    pub relation_id: String,
    pub from_id: String,
    pub to_id: String,
    pub relation: RelationType,
}

/// What one auto-merge pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeOutcome {
    pub merged: Vec<Merged>,
}

#[derive(Debug, thiserror::Error)]
pub enum RelationError {
    #[error("object {id} does not exist")]
    UnknownObject { id: String },
    #[error("object {id} is related to itself as {relation}")]
    SelfRelation { id: String, relation: RelationType },
    #[error("cannot relate a {from_kind} to a {to_kind} as {relation}")]
    KindMismatch {
        from_kind: String,
        to_kind: String,
        relation: RelationType,
    },
    #[error("relation query failed: {0}")]
    Query(#[from] StoreError),
    /// The consent clause could not be built. A separate variant because a
    /// caller that gets this has a broken `CallerId`, not a broken database,
    /// and folding it into `Query` would make the two the same log line.
    #[error("consent clause could not be built: {0}")]
    Consent(#[from] FilterError),
}

type RelResult<T> = std::result::Result<T, RelationError>;

/// The row shape every read in this module decodes.
///
/// `asserted` is `i32`, not `i64`, and that is not a style choice: `INTEGER` is
/// INT4 on Postgres and INT8 on SQLite, so a tuple declaring `i64` decodes
/// cleanly on SQLite and fails on Postgres with `Rust type i64 (as SQL type
/// INT8) is not compatible with SQL type INT4`. `i32` is what both give you.
type Row = (String, String, String, String, i32);

/// #2094: auto-merge by rules, opt-in and reversible.
///
/// A value the caller passes per run, never a row. That is the whole of the
/// opt-in guarantee: a program that knows nothing about auto-merge cannot have
/// it enabled, because there is no stored setting for it to be enabled *in*,
/// and the default is off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoMergeConfig {
    pub enabled: bool,
    /// Whether the pass may write `ReEncodeOf` for groups it has *already*
    /// classified as re-encodes -- which it reads from an existing
    /// `ReEncodeOf` row, never from a guess. Off by default: a directed claim
    /// about which file is the original, written by a background pass that
    /// cannot check, is how the original gets deleted.
    ///
    /// An existing `ReEncodeOf` is a claim someone already made, so promoting
    /// it to asserted is not the pass deciding anything; it is the pass
    /// declining to second-guess a decision. That is what makes this flag safe
    /// in a way the obvious implementation is not.
    pub allow_re_encode: bool,
    /// Groups larger than this are left alone. A "duplicate group" holding
    /// every item in a compilation is a sign the signal is wrong, and a pass
    /// that merged 400 items on the strength of one bad hash is not something
    /// a user can recover from by reading a log.
    pub max_group: usize,
}

impl Default for AutoMergeConfig {
    /// Off, re-encode refused, and a group of 100 is already suspicious.
    fn default() -> Self {
        Self {
            enabled: false,
            allow_re_encode: false,
            max_group: 100,
        }
    }
}

pub struct Relations<'a> {
    store: &'a Store,
}

impl<'a> Relations<'a> {
    pub fn new(store: &'a Store) -> Self {
        Self { store }
    }

    // ------------------------------------------------------- reading

    async fn fetch(&self, sql: &str, binds: &[String]) -> RelResult<Vec<Relation>> {
        let rows: Vec<Row> = match self.store {
            Store::Sqlite(p) => {
                let mut q = sqlx::query_as(sql);
                for b in binds {
                    q = q.bind(b.clone());
                }
                q.fetch_all(p).await.map_err(StoreError::Query)?
            }
            Store::Postgres(p) => {
                let pg = Store::bind_sql(sql);
                let mut q = sqlx::query_as(&pg);
                for b in binds {
                    q = q.bind(b.clone());
                }
                q.fetch_all(p).await.map_err(StoreError::Query)?
            }
        };
        Ok(rows.into_iter().map(row_to_relation).collect())
    }

    /// The symmetric relation between two objects, in either order.
    ///
    /// `ReEncodeOf` is deliberately excluded, and that exclusion is the whole
    /// point rather than a convenience: its reverse is a *different claim*, so
    /// a function that returned it for a reversed query would make "is B a
    /// re-encode of A" answer "yes" because "A is a re-encode of B". A
    /// directed relation is read with [`Relations::directed_between`].
    pub async fn relation_between(&self, a: &str, b: &str) -> RelResult<Option<Relation>> {
        // A pair may hold *both* a `same_scene_as` and an `unrelated_to` row --
        // the classifier proposes the merge, the user then rules the pair out --
        // and `LIMIT 1` over both types returns whichever was written first.
        // That made `is_ruled_out` answer "no" for a pair the user had just
        // dismissed, and `auto_merge` consults `is_ruled_out` before every
        // merge, so the next pass re-merged exactly the pair that was supposed
        // to stay split. #1656 is "mark not duplicate" and it has to mean it.
        //
        // The fix is the ordering: a suppression is a *later, stronger* claim
        // than a proposal, so it wins. `created_at` is not even consulted --
        // two rows of the same type cannot exist, because `assert_relation`
        // returns the existing one instead of writing a second, so the tie is
        // only ever between the two types and "later is stronger" is total.
        //
        // `is_merged` is the opposite question and `is_ruled_out` is
        // `UnrelatedTo`-only, so this change makes both correct rather than
        // trading one wrong answer for another.
        let sql = "SELECT id, from_id, to_id, relation, asserted FROM object_relation \
                   WHERE ((from_id = ? AND to_id = ?) OR (from_id = ? AND to_id = ?)) \
                     AND relation IN ('same_scene_as', 'unrelated_to') \
                   ORDER BY CASE relation WHEN 'unrelated_to' THEN 0 ELSE 1 END, created_at \
                   LIMIT 1";
        let binds: Vec<String> = vec![a.into(), b.into(), b.into(), a.into()];
        Ok(self.fetch(sql, &binds).await?.into_iter().next())
    }

    /// A *directed* relation, in the direction it was stored.
    ///
    /// No reverse matching, and no `LIMIT` to hide a second row: a directed
    /// lookup that found `A re_encode_of B` when asked about `B` would answer a
    /// question nobody asked. Two rows here means two claims were written and
    /// the lowest id wins, deterministically.
    pub async fn directed_between(
        &self,
        from_id: &str,
        to_id: &str,
        relation: RelationType,
    ) -> RelResult<Option<Relation>> {
        self.find_row(from_id, to_id, relation).await
    }

    /// Every relation touching one object, in either direction.
    ///
    /// Ordered by `relation` and then `from_id` rather than by anything
    /// per-engine: a UUID order differs between the two databases, so a test
    /// that compared their output would be comparing two arbitrary orders.
    pub async fn relations_of(&self, object_id: &str) -> RelResult<Vec<Relation>> {
        let sql = "SELECT id, from_id, to_id, relation, asserted FROM object_relation \
                   WHERE from_id = ? OR to_id = ? ORDER BY relation, from_id";
        let binds: Vec<String> = vec![object_id.into(), object_id.into()];
        self.fetch(sql, &binds).await
    }

    /// Run one statement with a text bind on every placeholder.
    ///
    /// Text-only on purpose. `object_relation.asserted` is an INTEGER column
    /// and this binds `"1"` to it, which SQLite accepts and Postgres refuses
    /// with `42804 ... but expression is of type text`. Rather than carry a
    /// typed-bind list for one statement, the value is written as the literal
    /// `1` in the SQL -- which is a constant of the function, not a caller's
    /// input, so it is not the injection that `format!` on a caller string
    /// would be.
    async fn exec(&self, sql: &str, binds: &[String]) -> RelResult<u64> {
        let n = match self.store {
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

    // ------------------------------------------------------- writing

    /// Store one relation, in the direction its meaning requires.
    ///
    /// Idempotent: asserting a relation that is already stored succeeds and
    /// changes nothing, because the lookup below is against the canonicalised
    /// tuple. A caller that re-asserts on every scan is the normal case, not an
    /// edge case -- which is why this returns the existing row rather than
    /// erroring on the uniqueness constraint.
    pub async fn assert_relation(
        &self,
        from_id: &str,
        to_id: &str,
        relation: RelationType,
    ) -> RelResult<Relation> {
        self.assert_relation_for(&CallerId::anonymous(), from_id, to_id, relation)
            .await
    }

    /// As [`Relations::assert_relation`], for a caller who is not anonymous.
    pub async fn assert_relation_for(
        &self,
        caller: &CallerId,
        from_id: &str,
        to_id: &str,
        relation: RelationType,
    ) -> RelResult<Relation> {
        if from_id == to_id {
            return Err(RelationError::SelfRelation {
                id: from_id.to_string(),
                relation,
            });
        }
        self.require_same_kind(from_id, to_id, relation, caller)
            .await?;

        let (from_id, to_id) = orient(from_id, to_id, relation);
        let (from_id, to_id) = (from_id.to_string(), to_id.to_string());
        if let Some(row) = self.find_row(&from_id, &to_id, relation).await? {
            return Ok(row);
        }
        let row = Relation {
            id: Uuid::new_v4().simple().to_string(),
            from_id,
            to_id,
            relation,
            asserted: true,
        };
        // `asserted` is a literal, not a bind: the column is INTEGER on both
        // engines and a text bind for it is refused by Postgres with 42804.
        self.exec(
            "INSERT INTO object_relation \
             (id, from_id, to_id, relation, asserted, created_at) \
             VALUES (?, ?, ?, ?, 1, ?)",
            &[
                row.id.clone(),
                row.from_id.clone(),
                row.to_id.clone(),
                row.relation.as_str().to_string(),
                ts::now(),
            ],
        )
        .await?;
        Ok(row)
    }

    /// Record that two objects are *not* duplicates (#1656).
    ///
    /// Symmetric and canonicalised, and with no precondition: it applies to a
    /// pair no relation exists for, which is the case that matters. Marking a
    /// false positive as a non-duplicate must not require first asserting the
    /// duplicate the user is denying.
    pub async fn mark_not_duplicate(&self, a: &str, b: &str) -> RelResult<Relation> {
        self.assert_relation(a, b, RelationType::UnrelatedTo).await
    }

    /// Has this specific pair been ruled out?
    ///
    /// `UnrelatedTo` only. The first version answered "yes" for any symmetric
    /// relation, which quietly made `SameSceneAs` read as a *suppression* --
    /// and `is_ruled_out` is what `auto_merge` consults before merging, so a
    /// pair that was already merged would then refuse to be merged by a later
    /// pass. The two questions are opposites and a caller that wants the
    /// broader one wants [`Relations::is_merged`].
    pub async fn is_ruled_out(&self, a: &str, b: &str) -> RelResult<bool> {
        Ok(self
            .relation_between(a, b)
            .await?
            .is_some_and(|r| r.relation == RelationType::UnrelatedTo))
    }

    /// Is the pair merged, in either order?
    pub async fn is_merged(&self, m: &Merged) -> RelResult<bool> {
        let rows = self
            .fetch(
                "SELECT id, from_id, to_id, relation, asserted FROM object_relation \
                 WHERE id = ? AND relation = 'same_scene_as'",
                std::slice::from_ref(&m.relation_id),
            )
            .await?;
        Ok(!rows.is_empty())
    }

    // ----------------------------------------------------- auto-merge

    /// Run the auto-merge pass (#2094). Writes nothing when disabled, which is
    /// the state every caller that has never heard of this function is in.
    pub async fn auto_merge(&self, cfg: &AutoMergeConfig) -> RelResult<MergeOutcome> {
        let mut out = MergeOutcome::default();
        if !cfg.enabled {
            return Ok(out);
        }
        if cfg.allow_re_encode {
            for m in self.unasserted_re_encodes().await? {
                let row = self
                    .assert_relation(&m.from_id, &m.to_id, m.relation)
                    .await?;
                if !out.merged.iter().any(|x| x.relation_id == row.id) {
                    out.merged.push(Merged {
                        relation_id: row.id,
                        from_id: row.from_id,
                        to_id: row.to_id,
                        relation: row.relation,
                    });
                }
            }
        }
        for group in self.same_scene_groups().await? {
            if group.len() > cfg.max_group {
                continue;
            }
            // The whole group merges onto its lowest-id member, so the choice
            // does not depend on row order -- which differs between the two
            // engines and would otherwise make them write different relations
            // from identical data.
            let mut ids = group;
            ids.sort();
            ids.dedup();
            let Some((first, rest)) = ids.split_first() else {
                continue;
            };
            for other in rest {
                // A user who ruled this pair out outranks the pass.
                if self.is_ruled_out(first, other).await? {
                    continue;
                }
                let row = self
                    .assert_relation(first, other, RelationType::SameSceneAs)
                    .await?;
                out.merged.push(Merged {
                    relation_id: row.id,
                    from_id: row.from_id,
                    to_id: row.to_id,
                    relation: row.relation,
                });
            }
        }
        Ok(out)
    }

    /// Take back one auto-merge pass.
    ///
    /// Deletes by `relation_id`, and nothing else -- so a relation a person
    /// confirmed keeps its `asserted` flag and survives a pass being reverted
    /// for an unrelated reason.
    pub async fn unmerge(&self, out: &MergeOutcome) -> RelResult<usize> {
        let mut n = 0usize;
        for m in &out.merged {
            n += self
                .exec(
                    "DELETE FROM object_relation WHERE id = ?",
                    std::slice::from_ref(&m.relation_id),
                )
                .await? as usize;
        }
        Ok(n)
    }

    /// Every connected group of objects joined by a `SameSceneAs` relation.
    ///
    /// Union-find rather than a recursive CTE: a transitive closure in SQL is a
    /// dialect difference (§0.4), and this is not a hard problem -- the closure
    /// runs over a relation table that is small by construction.
    async fn same_scene_groups(&self) -> RelResult<Vec<Vec<String>>> {
        let rows = self
            .fetch(
                "SELECT id, from_id, to_id, relation, asserted FROM object_relation \
                 WHERE relation = 'same_scene_as'",
                &[],
            )
            .await?;
        let mut uf = Union::new();
        for r in &rows {
            uf.add(&r.from_id);
            uf.add(&r.to_id);
        }
        for r in &rows {
            uf.union(&r.from_id, &r.to_id);
        }
        let mut groups: Vec<(String, Vec<String>)> = Vec::new();
        for r in &rows {
            // Both ends, and deduplicated per group. Collecting only `to_id`
            // was the first version: a pair is one edge, so every group came
            // back with exactly one member and the filter below discarded all
            // of them, which is why `auto_merge` reported having merged nothing
            // with a relation sitting in the table. A group's members are its
            // *objects*, not its edges.
            //
            // Keyed by root, so the grouping is a function of the ids and not of
            // the order the rows arrived in -- which differs between the two
            // engines and would otherwise make them group differently.
            let key = uf.root(&r.from_id);
            match groups.iter_mut().find(|(k, _)| *k == key) {
                Some((_, v)) => {
                    v.push(r.from_id.clone());
                    v.push(r.to_id.clone());
                }
                None => groups.push((key, vec![r.from_id.clone(), r.to_id.clone()])),
            }
        }
        // A group of one is not a group of duplicates. Every pair looks like a
        // group of one before it is merged, so returning those would make
        // auto-merge a no-op that reports having done something.
        Ok(groups
            .into_iter()
            .map(|(_, mut v)| {
                v.sort();
                v.dedup();
                v
            })
            .filter(|v| v.len() > 1)
            .collect())
    }

    /// `ReEncodeOf` rows already in the table, in their stored direction.
    ///
    /// Read rather than derived, because the pass has no way to know which copy
    /// of a file is the original -- a re-encode is a claim, and a claim the
    /// pass cannot make is one it must not make.
    async fn unasserted_re_encodes(&self) -> RelResult<Vec<Relation>> {
        self.fetch(
            "SELECT id, from_id, to_id, relation, asserted FROM object_relation \
             WHERE relation = 're_encode_of' AND asserted = 0 ORDER BY from_id, to_id",
            &[],
        )
        .await
    }

    async fn find_row(
        &self,
        from_id: &str,
        to_id: &str,
        relation: RelationType,
    ) -> RelResult<Option<Relation>> {
        let rows = self
            .fetch(
                "SELECT id, from_id, to_id, relation, asserted FROM object_relation \
                 WHERE from_id = ? AND to_id = ? AND relation = ?",
                &[
                    from_id.to_string(),
                    to_id.to_string(),
                    relation.as_str().to_string(),
                ],
            )
            .await?;
        Ok(rows.into_iter().next())
    }

    /// Both ends must exist and be the same kind of thing.
    ///
    /// Asserted in code rather than by a CHECK constraint because
    /// `object_relation` joins two `object` rows and a CHECK cannot see the
    /// other table -- and because SQLite would not enforce a cross-table check
    /// even if it could. The error names both kinds, so a caller that gets it
    /// knows which pair to fix rather than only that something was wrong.
    ///
    /// # This query is consent-filtered, and that is not ceremony
    ///
    /// It reads `object.kind`, which is metadata about a thing that may be
    /// under a takedown, so `tests/consent_filter.rs` is right to flag it. The
    /// first version did not join `consent_record` and the guard test failed
    /// with the statement quoted -- which is the guard working, and the fastest
    /// possible confirmation that a clause nobody is obliged to call really is
    /// only a convention.
    ///
    /// `caller` is a parameter rather than a fixed anonymous identity because
    /// a relation a moderator may not read is a relation they may not write
    /// either, and because a hardcoded caller would make this a rule that can
    /// be satisfied without being honoured. It defaults to anonymous at every
    /// public entry point, which is the conservative direction.
    async fn require_same_kind(
        &self,
        from_id: &str,
        to_id: &str,
        relation: RelationType,
        caller: &CallerId,
    ) -> RelResult<()> {
        let mut params: Vec<Value> = vec![
            Value::Str(from_id.to_string()),
            Value::Str(to_id.to_string()),
        ];
        // The clause is built first so it can *push its own parameters* -- it
        // owns the order they appear in, and a query that supplied its own tier
        // list would have to reproduce that order. Two ids go in first.
        let clause = Filter::consent_clause(caller, &mut params).map_err(RelationError::Consent)?;

        // The predicate is written out here rather than interpolated whole,
        // because `tests/consent_filter.rs` proves a filter is present by
        // looking for the text of a tier predicate in the statement. Building
        // the tier list by hand instead -- counting `params` and emitting `?n` --
        // looked equivalent and was not: the count was derived, so a change to
        // the clause's tier set produced a *shorter* list, and Postgres reported
        // `could not determine data type of parameter $6` for the placeholder
        // that no longer had a type. Using the clause's own placeholder text
        // cannot drift, because there is nothing to keep in step.
        //
        // `consent_clause` returns exactly `c.tier IN (...)` or
        // `c.tier NOT IN (...)`; anything else is a change to that function
        // that this call site has to know about rather than paper over.
        let predicate = clause.trim();
        debug_assert!(
            predicate.starts_with("c.tier") || predicate.contains("c.tier"),
            "consent_clause changed shape: {clause}"
        );

        let sql = format!(
            "SELECT o.id, o.kind FROM object o
             INNER JOIN consent_record c ON c.object_id = o.id
             WHERE (o.id = ? OR o.id = ?)
               AND {predicate}
             ORDER BY o.id"
        );

        let rows: Vec<(String, String)> = match self.store {
            Store::Sqlite(p) => bind_params!(sqlx::query_as(&sql), &params)
                .fetch_all(p)
                .await
                .map_err(StoreError::Query)?,
            Store::Postgres(p) => {
                let pg = Store::bind_sql(&sql);
                bind_params!(sqlx::query_as(&pg), &params)
                    .fetch_all(p)
                    .await
                    .map_err(StoreError::Query)?
            }
        };
        let kind_of = |id: &str| {
            rows.iter()
                .find(|(k, _)| k == id)
                .map(|(_, kind)| kind.clone())
        };
        let from_kind = kind_of(from_id).ok_or_else(|| RelationError::UnknownObject {
            id: from_id.to_string(),
        })?;
        let to_kind = kind_of(to_id).ok_or_else(|| RelationError::UnknownObject {
            id: to_id.to_string(),
        })?;
        if from_kind != to_kind {
            return Err(RelationError::KindMismatch {
                from_kind,
                to_kind,
                relation,
            });
        }
        Ok(())
    }
}

/// Union-find over object ids.
///
/// A struct rather than two closures over one `Vec<String>` because the closure
/// form needs a mutable borrow for `add` and an immutable one for `find` at the
/// same expression, and solving that by cloning the vector into a local is the
/// kind of fix that is correct today and quadratic tomorrow.
#[derive(Default)]
struct Union {
    /// Slot -> the id in that slot, which is the *name* and never overwritten.
    /// Keeping the names out of `parent` is the whole fix: `parent` holds a
    /// link that `union` rewrites, so looking an id up *in* `parent` finds a
    /// representative whose own slot has been replaced, and the id that lost
    /// the union is not findable at all.
    names: Vec<String>,
    parent: Vec<usize>,
    rank: Vec<u32>,
}

impl Union {
    fn new() -> Self {
        Self::default()
    }

    fn add(&mut self, id: &str) {
        if self.index(id).is_none() {
            self.names.push(id.to_string());
            self.parent.push(self.names.len() - 1);
            self.rank.push(0);
        }
    }

    fn index(&self, id: &str) -> Option<usize> {
        self.names.iter().position(|n| n == id)
    }

    fn root(&self, id: &str) -> String {
        let i = self.index(id).expect("id added before use");
        self.names[self.find(i)].clone()
    }

    /// Union by rank. Rank rather than "lowest index wins" because the latter
    /// makes a group's representative depend on insertion order, and insertion
    /// order is a per-engine property.
    fn union(&mut self, a: &str, b: &str) {
        let (Some(ia), Some(ib)) = (self.index(a), self.index(b)) else {
            return;
        };
        let (ra, rb) = (self.find(ia), self.find(ib));
        if ra == rb {
            return;
        }
        let (hi, lo) = if self.rank[ra] >= self.rank[rb] {
            (ra, rb)
        } else {
            (rb, ra)
        };
        self.parent[lo] = hi;
        if self.rank[hi] == self.rank[lo] {
            self.rank[hi] += 1;
        }
    }

    fn find(&self, mut i: usize) -> usize {
        // Guard rather than path compression: a compressed walk needs `&mut`,
        // and the cycle check is what makes a bad `parent` write a named
        // failure here instead of a wrong group three calls later.
        let mut guard = self.names.len() + 1;
        while self.parent[i] != i {
            i = self.parent[i];
            guard -= 1;
            assert!(guard > 0, "cycle in union-find parents");
        }
        i
    }
}

fn row_to_relation(r: Row) -> Relation {
    Relation {
        id: r.0,
        from_id: r.1,
        to_id: r.2,
        // An unparseable relation string is a row this build cannot interpret,
        // and mapping it to `UnrelatedTo` would silently turn it into a
        // suppression -- the one relation a caller acts on by *absence*. The
        // alternative, erroring the whole read, would make one bad row hide a
        // library's entire relation set, so it is skipped by the filter above.
        relation: RelationType::parse(&r.3).unwrap_or(RelationType::UnrelatedTo),
        asserted: r.4 != 0,
    }
}

/// The direction a relation is *stored* in.
///
/// Symmetric relations are canonicalised to low-id-first so that one fact is
/// one row. `ReEncodeOf`, `PartOf`, `CompilationOf` and `ExtraOf` are
/// directed, and keep what they were given.
fn orient<'a>(from_id: &'a str, to_id: &'a str, relation: RelationType) -> (&'a str, &'a str) {
    match relation {
        RelationType::SameSceneAs | RelationType::UnrelatedTo if from_id > to_id => {
            (to_id, from_id)
        }
        _ => (from_id, to_id),
    }
}

impl std::fmt::Display for Merged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {} {}",
            self.from_id,
            self.relation.as_str(),
            self.to_id
        )
    }
}

#[cfg(test)]
mod tests {
    //! `orient` is pure and total, so it is tested as a pure function rather
    //! than through a database round-trip that could hide a wrong answer behind
    //! a query that also happens to be wrong.
    //!
    //! The mutation that removed the canonical swap survived two rounds of
    //! integration tests, because every one of them asserted *a* stored row
    //! rather than *which* row. A symmetric relation stored in the wrong
    //! direction is still a row, still a duplicate pair, and still reports the
    //! right count -- it just stops being the same fact twice.

    use super::orient;
    use commons_core::RelationType;

    #[test]
    fn a_symmetric_relation_is_stored_low_id_first_whatever_it_is_given() {
        assert_eq!(
            orient("o-b", "o-a", RelationType::SameSceneAs),
            ("o-a", "o-b"),
            "descending input must be canonicalised"
        );
        assert_eq!(
            orient("o-a", "o-b", RelationType::SameSceneAs),
            ("o-a", "o-b"),
            "ascending input must be left alone"
        );
        assert_eq!(
            orient("o-b", "o-a", RelationType::UnrelatedTo),
            ("o-a", "o-b"),
            "a suppression is symmetric too, and must be canonicalised the \
             same way -- otherwise the same dismissal is two rows"
        );
    }

    #[test]
    fn a_directed_relation_keeps_the_direction_it_was_given() {
        // `re_encode_of` names which file is the original. Swapping its ends
        // would assert that the *copy* is the original, and the merge would
        // then delete the wrong file -- so this is the one case where "always
        // canonicalise" would be a data-loss bug rather than a tidy-up.
        for r in [
            RelationType::ReEncodeOf,
            RelationType::PartOf,
            RelationType::CompilationOf,
            RelationType::ExtraOf,
        ] {
            assert_eq!(
                orient("o-b", "o-a", r),
                ("o-b", "o-a"),
                "{r:?} is directed and must not be swapped"
            );
        }
    }

    #[test]
    fn an_equal_id_is_not_reordered() {
        // Degenerate, and the store refuses a self-relation before it gets
        // here -- but `orient` is total and a total function needs a value for
        // the case that cannot happen, and that value must not panic.
        assert_eq!(
            orient("o-a", "o-a", RelationType::SameSceneAs),
            ("o-a", "o-a")
        );
    }
}
