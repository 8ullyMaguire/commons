//! §7.2's operator actions: merge, split, alias, disambiguate.
//!
//! # What all four of these have in common
//!
//! Each one exists because the automatic path was wrong, and each one is
//! performed by a person who can see that it was wrong. That has one
//! consequence which runs through the whole module: **afterwards, the automatic
//! path has to agree with what was asked.** A merge that re-points the
//! references but leaves the losing cluster's centroid reachable, or a split
//! that moves the appearances but leaves the original's centroid alone,
//! produces a system that re-creates the mistake the user just corrected on the
//! next assignment. The recomputation is the feature; the re-pointing is
//! bookkeeping.
//!
//! Which is also why these are free functions taking a `&Store` rather than
//! `Engine` methods. None of them reads a threshold, and a merge that was only
//! available when the assignment threshold was configured correctly would not be
//! available at exactly the moment it is needed.
//!
//! # Why the records are written here and not to §14
//!
//! Re-pointing every appearance from one cluster to another is not reversible
//! from the data that survives it. Once the loser is gone nothing says which
//! appearances used to be in it, so an operator who merges the wrong two
//! clusters has no way back. The records below are the local operator's
//! safety net; the federation's event log is a different concern and coupling
//! §7.2 to it would mean a merge could fail because a peer was unreachable.
//!
//! # The one place this module refuses to be clever
//!
//! A new alias that collides with an existing one is *added and flagged*,
//! never refused (§7.2, stash-box#726/#714). In an amateur corpus a shared name
//! is ordinary data, and a hard error here rejects real names — the people
//! whose data this exists to hold. The cost is that an alias can resolve to
//! several performers, so every read path that resolves an alias has to be
//! ready for more than one answer; [`performers_for_alias`] returns them all
//! rather than picking one.

use std::collections::HashSet;

use commons_store::{Store, StoreError};
use serde_json::Value;
use sqlx::Row;

use super::store;
use super::{now, refresh_centroid, ClusterError};

// Every sqlx failure below maps to `StoreError::Query` and then to
// `ClusterError::Store`. A `From<sqlx::Error> for ClusterError` impl would be a
// *public* promise that every sqlx failure in this crate is a store failure --
// true today, but a claim about the whole crate made from one file, and a reader
// of `ops.rs` should not have to check whether a query error is a store error.

/// A fresh row id. Generated here rather than taken from the store because the
/// store has no opinion about identity -- every table in this schema has a
/// caller-supplied `TEXT` id and the cluster code's ids are the same kind.
fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

// ---------------------------------------------------------------------------
// Merge
// ---------------------------------------------------------------------------

/// What a merge did, for the caller to log or show.
#[derive(Debug, Clone, PartialEq)]
pub struct MergeReport {
    pub winner: String,
    pub loser: String,
    pub moved_appearances: u64,
}

/// One merge, as recorded in `cluster_merge`.
#[derive(Debug, Clone, PartialEq)]
pub struct MergeRecord {
    pub id: String,
    pub winner: String,
    pub loser: String,
    pub moved_appearances: u64,
    /// Who did it. `None` for an automated consolidation, `Some` for a person.
    pub actor: Option<String>,
    pub at: String,
}

/// Join two clusters, keeping `winner`.
///
/// `actor` is what distinguishes a person correcting a mistake from the
/// scheduled pass doing its job, and it is the only thing that can authorise
/// overriding a `not_same_as` assertion (§7.2's disambiguation). Passing `None`
/// here is a deliberate claim: "no human decided this".
///
/// The order is: check the assertions, write the record, move the references,
/// recompute the winner's centroid. The record goes first because it is the only
/// thing that makes the move reversible, and a merge that wrote it last would
/// have a window in which an interrupted run left no trace at all.
pub async fn merge(
    store: &Store,
    winner: &str,
    loser: &str,
    actor: Option<&str>,
) -> Result<MergeReport, ClusterError> {
    if winner == loser {
        return Err(ClusterError::SelfMerge);
    }
    for id in [winner, loser] {
        if store::get(store, id).await?.is_none() {
            return Err(ClusterError::NotFound("person_cluster"));
        }
    }

    // An assertion a bulk operation can overrule silently is not an assertion.
    // This is checked before anything is written, so a refused merge leaves no
    // trace -- a half-applied refusal would be worse than either outcome.
    if actor.is_none() && blocked_pair(store, winner, loser).await? {
        return Err(ClusterError::BlockedByAssertion {
            left: winner.to_string(),
            right: loser.to_string(),
        });
    }

    let at = now();
    let id = new_id();
    sqlx::query(
        "INSERT INTO cluster_merge (id, winner_id, loser_id, moved_appearances, actor, created_at)
         VALUES (?, ?, ?, 0, ?, ?)",
    )
    .bind(&id)
    .bind(winner)
    .bind(loser)
    .bind(actor)
    .bind(&at)
    .execute(store.pool())
    .await
    .map_err(StoreError::Query)?;

    let moved = store::merge(store, loser, winner).await?;
    sqlx::query("UPDATE cluster_merge SET moved_appearances = ? WHERE id = ?")
        .bind(moved as i64)
        .bind(&id)
        .execute(store.pool())
        .await
        .map_err(StoreError::Query)?;

    // The winner's centroid is recomputed from the union of members, not
    // inherited. The absorbed members are members now; a centroid that excludes
    // them is a mean of a subset, and the next assignment compares against it.
    refresh_centroid(store, winner).await?;

    Ok(MergeReport {
        winner: winner.to_string(),
        loser: loser.to_string(),
        moved_appearances: moved,
    })
}

/// Every merge on record, newest last.
pub async fn merge_records(store: &Store) -> Result<Vec<MergeRecord>, ClusterError> {
    let rows = sqlx::query(
        "SELECT id, winner_id, loser_id, moved_appearances, actor, created_at
         FROM cluster_merge ORDER BY created_at, id",
    )
    .fetch_all(store.pool())
    .await
    .map_err(StoreError::Query)?;
    rows.iter()
        .map(|r| {
            Ok(MergeRecord {
                id: r.get("id"),
                winner: r.get("winner_id"),
                loser: r.get("loser_id"),
                moved_appearances: r.get::<i64, _>("moved_appearances") as u64,
                actor: r.get("actor"),
                at: r.get("created_at"),
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Split
// ---------------------------------------------------------------------------

/// What a split did.
///
/// A struct rather than an enum: there is exactly one thing a split can do, and
/// an enum with one variant makes every caller write a match that cannot fail.
/// If a later spec revision adds a "dry run" outcome this becomes an enum, and
/// the change will be visible in the diff.
#[derive(Debug, Clone, PartialEq)]
pub struct SplitOutcome {
    pub original: String,
    pub new_cluster: String,
    pub moved: Vec<String>,
}

/// Move the named appearances into a new cluster, leaving the original with the
/// rest.
///
/// The original's centroid is recomputed, and that is the ticket's point: a
/// split that moved the references but kept the old mean leaves the cluster
/// centred on the appearance that was wrong, so the next assignment joins the
/// error straight back. The new cluster's centroid is computed from what moved,
/// for the same reason.
pub async fn split(
    store: &Store,
    cluster_id: &str,
    appearance_ids: &[String],
) -> Result<SplitOutcome, ClusterError> {
    if store::get(store, cluster_id).await?.is_none() {
        return Err(ClusterError::NotFound("person_cluster"));
    }

    // Every named appearance must be in this cluster. A partial move would
    // silently split a different cluster than the one asked for, which is the
    // kind of mistake that is invisible until someone notices a face is missing.
    let owned: HashSet<String> = store::all_appearances(store)
        .await?
        .into_iter()
        .filter(|a| a.cluster_id.as_deref() == Some(cluster_id))
        .map(|a| a.id)
        .collect();
    for id in appearance_ids {
        if !owned.contains(id) {
            return Err(ClusterError::NoSuchAppearance(id.clone()));
        }
    }
    // Both ways of leaving a cluster with no members are refused, here, before
    // anything is written: naming nothing (which would move nothing and leave
    // the new cluster empty) and naming everything.
    //
    // The naming-nothing half was missing for a while. The check read
    // `appearance_ids.len() == owned.len()`, which is false for an empty list
    // against a populated cluster -- so an empty split sailed past it, inserted
    // a new cluster, moved nothing, and was caught only by the `moved == 0`
    // check *after* the insert. The error was right and the state was not: a
    // refused split had left an empty person-shaped row behind. A test that
    // asserts on the error alone passed throughout; the one that counts clusters
    // after the refusal is what found it.
    if appearance_ids.is_empty() || appearance_ids.len() == owned.len() {
        return Err(ClusterError::EmptySplit);
    }

    let at = now();
    let split_id = new_id();

    // The new cluster's vectors are its own members, which are a subset of the
    // original's, so the existing vectors are moved rather than recomputed from
    // data that does not exist here.
    store::insert_cluster(
        store,
        &split_id,
        super::ClusterState::Anonymous,
        None,
        None,
        &at,
    )
    .await?;
    // `move_appearances` moves the *named* appearances to the new cluster, which
    // is what a split wants: the appearances that go are the ones the operator
    // named. (Consolidation calls it the other way round, moving everything
    // except a keep-list into a temporary cluster -- hence the explicit name
    // here rather than a `split_or_keep` boolean.)
    let moved = store::move_appearances(store, cluster_id, &split_id, appearance_ids).await?;
    if moved == 0 {
        // Unreachable given the checks above: every named appearance was
        // verified to be in this cluster. It stays because `move_appearances`
        // reports a count rather than a guarantee, and a split that reported
        // success having moved nothing would be a lie in the returned report.
        return Err(ClusterError::EmptySplit);
    }
    move_member_vectors(store, cluster_id, &split_id, appearance_ids).await?;

    refresh_centroid(store, cluster_id).await?;
    refresh_centroid(store, &split_id).await?;
    store::recompute_count(store, cluster_id).await?;
    store::recompute_count(store, &split_id).await?;

    sqlx::query(
        "INSERT INTO cluster_split (id, original_id, new_id, moved_appearances, actor, created_at)
         VALUES (?, ?, ?, ?, NULL, ?)",
    )
    .bind(new_id())
    .bind(cluster_id)
    .bind(&split_id)
    .bind(moved as i64)
    .bind(&at)
    .execute(store.pool())
    .await
    .map_err(StoreError::Query)?;

    Ok(SplitOutcome {
        original: cluster_id.to_string(),
        new_cluster: split_id,
        moved: appearance_ids.to_vec(),
    })
}

/// Move the member vectors belonging to the named appearances.
///
/// # The data model had to change for this
///
/// `member_vector` proposals are a bare list per cluster: nothing said which
/// appearance each vector came from. That is enough to compute a centroid and
/// not enough to divide one, and a split that cannot divide its members
/// recomputes the original's centroid over *all* of them -- the ones that stayed
/// plus the ones that left -- which is the mean the ticket exists to correct. So
/// each member vector now records the appearance it came from, in
/// `proposer_id`, which was already a nullable "who proposed this" column with
/// no other use on this path.
///
/// The proposal is *copied* into the new cluster rather than re-pointed, and the
/// original is then deleted rather than left behind: a leftover would be counted
/// in the original's mean, which is the same error the split is correcting.
async fn move_member_vectors(
    store: &Store,
    from: &str,
    into: &str,
    appearance_ids: &[String],
) -> Result<(), ClusterError> {
    for appearance in appearance_ids {
        let rows = sqlx::query(
            "SELECT value_json FROM field_proposal
             WHERE subject_type = 'person_cluster' AND subject_id = ?
               AND field = 'member_vector' AND proposer_id = ?",
        )
        .bind(from)
        .bind(appearance)
        .fetch_all(store.pool())
        .await
        .map_err(StoreError::Query)?;

        for row in rows {
            let raw: String = row.get("value_json");
            sqlx::query(
                "INSERT INTO field_proposal
                     (id, subject_type, subject_id, field, value_json, source,
                      proposer_kind, proposer_id, confidence, created_at)
                 VALUES (?, 'person_cluster', ?, 'member_vector', ?, 'split',
                         'system', ?, 1.0, ?)",
            )
            .bind(new_id())
            .bind(into)
            .bind(&raw)
            .bind(appearance)
            .bind(now())
            .execute(store.pool())
            .await
            .map_err(StoreError::Query)?;
        }

        sqlx::query(
            "DELETE FROM field_proposal
             WHERE subject_type = 'person_cluster' AND subject_id = ?
               AND field = 'member_vector' AND proposer_id = ?",
        )
        .bind(from)
        .bind(appearance)
        .execute(store.pool())
        .await
        .map_err(StoreError::Query)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Disambiguation
// ---------------------------------------------------------------------------

/// A §7.2 assertion between two clusters.
#[derive(Debug, Clone, PartialEq)]
pub enum Disambiguation {
    SameAs {
        left: String,
        right: String,
        note: Option<String>,
    },
    NotSameAs {
        left: String,
        right: String,
        note: Option<String>,
    },
}

/// Record an assertion between two clusters.
///
/// Stored twice, once per direction, so "is this pair blocked" is one indexed
/// lookup rather than a scan with a lexicographic comparison. The cost is one
/// row and the benefit is that the merge path -- the reader that matters, and
/// the one that would otherwise forget -- does not have to know which side of
/// the pair it was handed.
///
/// A `SameAs` is a statement, not an action: it is recorded and it is evidence
/// for a later consolidation, which proposes rather than performs (§7.2).
pub async fn disambiguate(store: &Store, assertion: Disambiguation) -> Result<(), ClusterError> {
    let (left, right, same, note) = match &assertion {
        Disambiguation::SameAs { left, right, note } => (left, right, true, note),
        Disambiguation::NotSameAs { left, right, note } => (left, right, false, note),
    };
    if left == right {
        return Err(ClusterError::SelfMerge);
    }
    for id in [left, right] {
        if store::get(store, id).await?.is_none() {
            return Err(ClusterError::NotFound("person_cluster"));
        }
    }

    let at = now();
    let actor: Option<&str> = None;
    for (l, r) in [(left, right), (right, left)] {
        sqlx::query(
            "INSERT INTO cluster_assertion (id, left_id, right_id, same, note, actor, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(new_id())
        .bind(l)
        .bind(r)
        .bind(same as i32)
        .bind(note)
        .bind(actor)
        .bind(&at)
        .execute(store.pool())
        .await
        .map_err(StoreError::Query)?;
    }
    Ok(())
}

/// Has this pair been asserted *not* to be the same person?
///
/// One direction is enough, because [`disambiguate`] writes both. An earlier
/// version checked both directions in the SQL as well, on the theory that the
/// symmetric read was the safe one; a mutation pass showed the second
/// disjunct can never be the one that matches, because the mirrored row is
/// always present. Two rows and one read, rather than two rows and a read that
/// pays for a case it cannot hit.
///
/// The `a == b` early return is not about the query: a cluster is never
/// blocked from being itself, and a self-referential assertion is refused at
/// write time, so a row for `(a, a)` cannot exist. The check is here so a
/// caller that reaches this with `a == b` gets `false` rather than a count
/// that happened to be zero.
pub async fn blocked_pair(store: &Store, a: &str, b: &str) -> Result<bool, ClusterError> {
    if a == b {
        return Ok(false);
    }
    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM cluster_assertion
         WHERE same = 0 AND left_id = ? AND right_id = ?",
    )
    .bind(a)
    .bind(b)
    .fetch_one(store.pool())
    .await
    .map_err(StoreError::Query)?;
    Ok(n > 0)
}

// ---------------------------------------------------------------------------
// Performers, aliases
// ---------------------------------------------------------------------------

/// What scope an alias is unique within.
///
/// The scope is what makes a shared name a non-issue where it can be scoped:
/// "Sam" as a studio's stage name and "Sam" as the person's legal name are two
/// different aliases that happen to share a spelling.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AliasScope {
    Unscoped,
    /// Scoped to a producer/studio (#422, stash-box#818).
    Producer(String),
    /// Scoped to an era, for a name that is right for one period and wrong for
    /// another.
    Era(String),
}

/// One alias row.
#[derive(Debug, Clone, PartialEq)]
pub struct Alias {
    pub name: String,
    pub is_primary: bool,
    pub scope: AliasScope,
}

/// The result of adding an alias.
#[derive(Debug, Clone, PartialEq)]
pub struct AddAliasOutcome {
    pub added: bool,
    /// The other performers this name already belongs to, if any. §7.2's
    /// warning: the platform allows the collision and flags it.
    pub collision: Option<Vec<String>>,
}

pub async fn create_performer(store: &Store, name: &str) -> Result<String, ClusterError> {
    let id = new_id();
    let at = now();
    sqlx::query("INSERT INTO performer (id, name, created_at, updated_at) VALUES (?, ?, ?, ?)")
        .bind(&id)
        .bind(name)
        .bind(&at)
        .bind(&at)
        .execute(store.pool())
        .await
        .map_err(StoreError::Query)?;
    Ok(id)
}

pub async fn create_producer(store: &Store, name: &str) -> Result<String, ClusterError> {
    let id = new_id();
    let at = now();
    sqlx::query("INSERT INTO producer (id, name, created_at, updated_at) VALUES (?, ?, ?, ?)")
        .bind(&id)
        .bind(name)
        .bind(&at)
        .bind(&at)
        .execute(store.pool())
        .await
        .map_err(StoreError::Query)?;
    Ok(id)
}

/// Add an alias, reporting a collision instead of refusing it.
///
/// The name is stored exactly as given. No trimming, no case folding, no
/// comma splitting: "Doe, Jane" is one alias and the two upstream bugs this
/// guards (#778, stash#5033) were both a well-meaning normalisation turning a
/// name into two rows. Normalisation belongs at display time, where a person can
/// see what was done.
pub async fn add_alias(
    store: &Store,
    performer_id: &str,
    name: &str,
    scope: AliasScope,
) -> Result<AddAliasOutcome, ClusterError> {
    let (producer_id, era) = match &scope {
        AliasScope::Unscoped => (None, None),
        AliasScope::Producer(p) => (Some(p.clone()), None),
        AliasScope::Era(e) => (None, Some(e.clone())),
    };

    // A collision is only a collision within the same scope. Comparing against
    // every alias of the same string regardless of scope would flag the ordinary
    // case of a studio using a name the performer also uses.
    let others: Vec<String> = sqlx::query_scalar(
        "SELECT performer_id FROM performer_alias
         WHERE name = ? AND performer_id != ?
           AND ((producer_id IS NULL AND ? IS NULL) OR producer_id = ?)
           AND ((era IS NULL AND ? IS NULL) OR era = ?)",
    )
    .bind(name)
    .bind(performer_id)
    .bind(&producer_id)
    .bind(&producer_id)
    .bind(&era)
    .bind(&era)
    .fetch_all(store.pool())
    .await
    .map_err(StoreError::Query)?;

    // The first alias a performer has is the primary. §7.2 makes the primary
    // *selectable*, and a performer with no primary would have nothing for the
    // UI to show as their name.
    let existing: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM performer_alias WHERE performer_id = ?")
            .bind(performer_id)
            .fetch_one(store.pool())
            .await
            .map_err(StoreError::Query)?;
    let is_primary = existing == 0;

    let id = new_id();
    sqlx::query(
        "INSERT INTO performer_alias
             (id, performer_id, name, is_primary, producer_id, era)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(performer_id)
    .bind(name)
    .bind(is_primary as i32)
    .bind(&producer_id)
    .bind(&era)
    .execute(store.pool())
    .await
    .map_err(StoreError::Query)?;

    Ok(AddAliasOutcome {
        added: true,
        collision: (!others.is_empty()).then_some(others),
    })
}

/// Every alias of a performer, primary first.
///
/// The sort is only a display order; [`set_primary`] is what establishes which
/// one is primary, and this ordering must not be mistaken for that.
pub async fn aliases(store: &Store, performer_id: &str) -> Result<Vec<Alias>, ClusterError> {
    let rows = sqlx::query(
        "SELECT name, is_primary, producer_id, era FROM performer_alias
         WHERE performer_id = ? ORDER BY is_primary DESC, name",
    )
    .bind(performer_id)
    .fetch_all(store.pool())
    .await
    .map_err(StoreError::Query)?;
    rows.iter()
        .map(|r| {
            let producer: Option<String> = r.get("producer_id");
            let era: Option<String> = r.get("era");
            Ok(Alias {
                name: r.get("name"),
                is_primary: r.get::<i32, _>("is_primary") != 0,
                scope: match (producer, era) {
                    (Some(p), _) => AliasScope::Producer(p),
                    (_, Some(e)) => AliasScope::Era(e),
                    _ => AliasScope::Unscoped,
                },
            })
        })
        .collect()
}

/// Choose the primary alias (#610).
///
/// Demotion and promotion happen in one transaction. Two primaries is a state
/// the UI cannot render — it has to pick one to show — and a demotion that
/// succeeded while its promotion failed would leave exactly that with nothing to
/// recover from.
pub async fn set_primary(
    store: &Store,
    performer_id: &str,
    name: &str,
) -> Result<(), ClusterError> {
    let mut tx = store.pool().begin().await.map_err(StoreError::Query)?;
    sqlx::query("UPDATE performer_alias SET is_primary = 0 WHERE performer_id = ?")
        .bind(performer_id)
        .execute(&mut *tx)
        .await
        .map_err(StoreError::Query)?;
    let updated = sqlx::query(
        "UPDATE performer_alias SET is_primary = 1 WHERE performer_id = ? AND name = ?",
    )
    .bind(performer_id)
    .bind(name)
    .execute(&mut *tx)
    .await
    .map_err(StoreError::Query)?;
    if updated.rows_affected() == 0 {
        return Err(ClusterError::NotFound("performer_alias"));
    }
    tx.commit().await.map_err(StoreError::Query)?;
    Ok(())
}

/// Every performer this alias name resolves to.
///
/// More than one is the normal case for an unscoped name in an amateur corpus,
/// which is why [`add_alias`] reports a collision instead of refusing it. A
/// read path that picked one would be silently wrong for the second.
pub async fn performers_for_alias(store: &Store, name: &str) -> Result<Vec<String>, ClusterError> {
    Ok(sqlx::query_scalar(
        "SELECT performer_id FROM performer_alias WHERE name = ? ORDER BY performer_id",
    )
    .bind(name)
    .fetch_all(store.pool())
    .await
    .map_err(StoreError::Query)?)
}

/// Aliases as CSV, one per line.
///
/// RFC 4180 quoting, and the quoting is the whole point: "Doe, Jane" contains a
/// comma, and a naive join is the bug (#778, stash#5033). A name containing a
/// quote is escaped by doubling, which is the same rule.
pub async fn aliases_to_csv(store: &Store, performer_id: &str) -> Result<String, ClusterError> {
    let all = aliases(store, performer_id).await?;
    let mut out = String::new();
    for a in all {
        out.push_str(&csv_field(&a.name));
        out.push('\n');
    }
    Ok(out)
}

/// Parse the CSV form back, one alias per record.
///
/// Takes no performer: parsing is a property of the text, and a function that
/// wanted an id it never used would be implying the parse depended on one.
pub fn parse_aliases_csv(csv: &str) -> Vec<Alias> {
    let mut out: Vec<Alias> = Vec::new();
    for record in parse_csv_records(csv) {
        if record.is_empty() {
            continue;
        }
        out.push(Alias {
            name: record,
            is_primary: false,
            scope: AliasScope::Unscoped,
        });
    }
    out
}

/// Quote a CSV field if it needs it.
fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// Split CSV text into records, honouring quoted fields.
///
/// A hand-rolled reader rather than a dependency: the format is one field per
/// line, the only rule that matters is that a quoted field may contain the
/// delimiter, and a CSV crate would be a hundred kilobytes of dependency for
/// eleven lines of logic that a bug in the import path would hide.
fn parse_csv_records(text: &str) -> Vec<String> {
    let mut records = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut chars = text.chars().peekable();
    let mut quoted_field = false;
    while let Some(c) = chars.next() {
        if in_quotes {
            match c {
                '"' => {
                    if chars.peek() == Some(&'"') {
                        chars.next();
                        field.push('"');
                    } else {
                        in_quotes = false;
                    }
                }
                other => field.push(other),
            }
            continue;
        }
        match c {
            '"' if field.is_empty() && !quoted_field => {
                in_quotes = true;
                quoted_field = true;
            }
            ',' => {
                records.push(std::mem::take(&mut field));
                quoted_field = false;
            }
            '\n' => {
                records.push(std::mem::take(&mut field));
                quoted_field = false;
            }
            '\r' => {}
            other => field.push(other),
        }
    }
    if !field.is_empty() || !records.is_empty() {
        records.push(field);
    }
    records
}

/// The tag a performer auto-derives from their name.
///
/// Stash#2293: a non-ASCII name failed auto-tag outright, so the person was
/// created and then invisible to the tag path. The fix is that the tag is a
/// *normalisation* — lowercased and trimmed — and nothing else. There is no
/// ASCII filter here, and there must not be one: a filter is the bug.
///
/// The name is kept whole rather than transliterated, because a transliteration
/// is a guess and two different names can collapse onto one tag. `Björk` and
/// `Bjork` are different spellings of possibly the same person; the tag for the
/// second does not exist, which is a missed tag rather than a wrong merge.
pub fn auto_tag_for(name: &str) -> String {
    format!("performer:{}", name.trim().to_lowercase())
}

/// Whether the auto-tag for a performer has been written.
///
/// The tag is written by the caller's import path, not here, because a tag is a
/// relation to an object and this module is about the performer. What this
/// answers is whether the write happened, which is what the non-ASCII test needs
/// to distinguish "tagged" from "silently skipped".
pub async fn tag_exists(store: &Store, tag: &str) -> Result<bool, ClusterError> {
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tag WHERE name = ?")
        .bind(tag)
        .fetch_one(store.pool())
        .await
        .map_err(StoreError::Query)?;
    Ok(n > 0)
}

/// Write a tag. Separate from [`auto_tag_for`] so the derivation and the write
/// are separately testable, and so a caller that already has a tag row does not
/// have to reach into the tag module to add another.
pub async fn ensure_tag(store: &Store, name: &str) -> Result<String, ClusterError> {
    let id = new_id();
    // `tag` carries no timestamp column. It is a shared vocabulary rather than a
    // record of an event, so "when was this tag first written" has no answer in
    // the schema and inventing one here would make the first writer's clock the
    // tag's history.
    sqlx::query("INSERT INTO tag (id, name) VALUES (?, ?)")
        .bind(&id)
        .bind(name)
        .execute(store.pool())
        .await
        .map_err(StoreError::Query)?;
    Ok(id)
}

// ---------------------------------------------------------------------------
// The accepted-edit set
// ---------------------------------------------------------------------------
//
// stash-box#943: a merge silently lost edits. The fix is structural and is
// T-P4-006's subject: **nothing here maintains a counter or a denormalised
// total.** Every accepted value is derived by reading the proposals and the
// votes each time, so there is no stored quantity that a merge can fail to
// carry across.
//
// The operations here are therefore about *re-pointing* proposals, never about
// recomputing a total, because there is no total to recompute.

/// Propose a value for a field of a subject.
pub async fn propose(
    store: &Store,
    subject_type: &str,
    subject_id: &str,
    field: &str,
    value: &Value,
    source: &str,
    proposer_id: Option<&str>,
) -> Result<String, ClusterError> {
    let id = new_id();
    sqlx::query(
        "INSERT INTO field_proposal
             (id, subject_type, subject_id, field, value_json, source,
              proposer_kind, proposer_id, confidence, created_at)
         VALUES (?, ?, ?, ?, ?, ?, 'user', ?, NULL, ?)",
    )
    .bind(&id)
    .bind(subject_type)
    .bind(subject_id)
    .bind(field)
    .bind(serde_json::to_string(value).map_err(|e| ClusterError::NotSerialisable(e.to_string()))?)
    .bind(source)
    .bind(proposer_id)
    .bind(now())
    .execute(store.pool())
    .await
    .map_err(StoreError::Query)?;
    Ok(id)
}

/// Accept a proposal.
pub async fn vote(
    store: &Store,
    proposal_id: &str,
    account_id: &str,
    weight: f64,
) -> Result<(), ClusterError> {
    sqlx::query(
        "INSERT INTO vote (id, proposal_id, account_id, field, weight, retracted, created_at)
         VALUES (?, ?, ?, '', ?, 0, ?)",
    )
    .bind(new_id())
    .bind(proposal_id)
    .bind(account_id)
    .bind(weight)
    .bind(now())
    .execute(store.pool())
    .await
    .map_err(StoreError::Query)?;
    Ok(())
}

/// The accepted value for a field, derived from the proposals and their votes.
///
/// Derived on every read, never cached. That is the whole of the #943 fix: a
/// value that is recomputed cannot be lost by an operation that moves rows
/// around, because there is nothing stored to lose.
///
/// A proposal with no un-retracted vote does not count. One accepted vote does:
/// this is a single-user local library, and waiting for a quorum that will never
/// arrive would make every value permanently unaccepted.
pub async fn accepted_value(
    store: &Store,
    subject_type: &str,
    subject_id: &str,
    field: &str,
) -> Result<Option<Value>, ClusterError> {
    let row: Option<(String, f64)> = sqlx::query_as(
        "SELECT p.value_json, COALESCE(SUM(v.weight), 0) AS weight
         FROM field_proposal p
         JOIN vote v ON v.proposal_id = p.id AND v.retracted = 0
         WHERE p.subject_type = ? AND p.subject_id = ? AND p.field = ?
         GROUP BY p.id
         ORDER BY weight DESC, p.created_at, p.id
         LIMIT 1",
    )
    .bind(subject_type)
    .bind(subject_id)
    .bind(field)
    .fetch_optional(store.pool())
    .await
    .map_err(StoreError::Query)?;

    let Some((raw, weight)) = row else {
        return Ok(None);
    };
    if weight <= 0.0 {
        return Ok(None);
    }
    serde_json::from_str(&raw)
        .map(Some)
        .map_err(|e| ClusterError::NotSerialisable(e.to_string()))
}

/// Every proposal for a field, however voted.
///
/// Used by the merge path to show the operator what is on the losing side
/// before they act, and by the test to prove a proposal was re-pointed rather
/// than deleted.
pub async fn proposals_for(
    store: &Store,
    subject_type: &str,
    subject_id: &str,
    field: &str,
) -> Result<Vec<(String, Value)>, ClusterError> {
    let rows = sqlx::query(
        "SELECT id, value_json FROM field_proposal
         WHERE subject_type = ? AND subject_id = ? AND field = ?
         ORDER BY created_at, id",
    )
    .bind(subject_type)
    .bind(subject_id)
    .bind(field)
    .fetch_all(store.pool())
    .await
    .map_err(StoreError::Query)?;
    rows.iter()
        .map(|r| {
            let raw: String = r.get("value_json");
            let value =
                serde_json::from_str(&raw).map_err(|_| ClusterError::NotSerialisable(raw))?;
            Ok((r.get("id"), value))
        })
        .collect()
}
