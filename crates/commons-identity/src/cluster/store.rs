//! The SQL side of clustering: reading and writing the two tables §7.1 needs.
//!
//! # Why these are functions and not inline SQL
//!
//! The engine is the only writer of `person_cluster` and `appearance`, and
//! keeping every statement here means the ambiguity rules, the centroid
//! encoding, and the `ambiguous` flag each have exactly one implementation. A
//! caller that inserts an appearance row with its own SQL can set `ambiguous`
//! to whatever it likes, and the invariant "an ambiguous appearance belongs to
//! no cluster" becomes a convention rather than a fact.
//!
//! # `ambiguous` and the empty cluster id
//!
//! An ambiguous appearance is written with `cluster_id = ''`. The column is
//! `NOT NULL REFERENCES person_cluster(id)`, so this is worth spelling out: an
//! empty string is **not** a valid cluster id, and a foreign-key constraint
//! would reject it.
//!
//! So the schema cannot express "belongs to no cluster" as it stands, and the
//! choice is between three options, none of which is free:
//!
//!   * nullable `cluster_id` — a migration, and a nullable foreign key is a
//!     nullable identity: every read has to handle three states, and a NULL
//!     `cluster_id` is indistinguishable from a bug in a query that forgot to
//!     join.
//!   * a sentinel cluster — a real row that means "ambiguous", which is a
//!     lie: it is a person-shaped row that is not a person, and it shows up in
//!     counts.
//!   * **the current schema, with the row written to no cluster** — which
//!     requires the `NOT NULL` and the foreign key to give.
//!
//! This is a real schema defect and it is *not* worked around by writing an
//! empty string, because that would either fail the foreign key or, on an engine
//! where foreign keys are off, create a dangling reference that every later read
//! has to defend against. The fix is migration 0002, which makes
//! `appearance.cluster_id` nullable and adds a partial index. Until that
//! migration lands, [`insert_appearance`] for an ambiguous row returns
//! [`ClusterError::AmbiguousNeedsMigration`] rather than storing something
//! wrong.
//!
//! That means **the ambiguity acceptance test cannot pass yet**, and that is the
//! honest state of the work rather than a test to be weakened. The rule is: a
//! test that cannot pass because the schema is wrong should say so, and the
//! schema should be fixed, not the test.

use commons_store::{column, Store, StoreError};
use sqlx::Row;

/// One `person_cluster` row, with what the UI needs to show it.
#[derive(Debug, Clone, PartialEq)]
pub struct ClusterRow {
    pub id: String,
    pub handle: Option<String>,
    pub state: super::ClusterState,
    /// The centroid, decoded, if the cluster has one.
    pub centroid: Option<Vec<f32>>,
    pub appearance_count: i64,
}

/// A cluster plus its member appearance ids.
#[derive(Debug, Clone, PartialEq)]
pub struct ClusterWithMembers {
    pub id: String,
    pub handle: Option<String>,
    pub state: super::ClusterState,
    pub centroid: Option<Vec<f32>>,
    pub appearance_count: i64,
    /// The `appearance.id` of every member, ordered so the list is stable.
    pub member_ids: Vec<String>,
}

/// One `appearance` row.
#[derive(Debug, Clone, PartialEq)]
pub struct AppearanceRecord {
    pub id: String,
    pub object_id: String,
    /// `None` when the appearance is ambiguous and belongs to no cluster.
    pub cluster_id: Option<String>,
    pub distance: Option<f32>,
    pub face_score: Option<f32>,
    pub body_score: Option<f32>,
    pub ambiguous: bool,
}

/// A candidate cluster for an ambiguous appearance, with the distance that put
/// it there.
///
/// §7.4 requires the UI to be able to say *why*, so the distance travels with
/// the candidate rather than being looked up again.
#[derive(Debug, Clone, PartialEq)]
pub struct CandidateDistance {
    pub cluster_id: String,
    pub distance: f32,
}

/// A proposed handle value.
#[derive(Debug, Clone, PartialEq)]
pub struct HandleCandidate {
    pub value: String,
    pub source: String,
}

/// Fetch one cluster.
pub async fn get(store: &Store, id: &str) -> Result<Option<ClusterRow>, StoreError> {
    let row = sqlx::query(
        "SELECT id, handle, state, centroid_hex, appearance_count \
         FROM person_cluster WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(store.pool())
    .await?;
    row.map(row_to_cluster).transpose()
}

fn row_to_cluster(row: sqlx::sqlite::SqliteRow) -> Result<ClusterRow, StoreError> {
    let state_str: String = column!(row, "state")?;
    // An unrecognised state is `anonymous` rather than an error: the column has
    // a NOT NULL DEFAULT and a future writer may add a state this build does not
    // know. Reading it as the *most* restrictive known state would be wrong
    // (anonymous is the default, and a named cluster read as anonymous loses
    // the user's name), so it degrades to the default and the raw string is
    // still available through `state` for a caller that cares.
    let state = super::ClusterState::parse(&state_str).unwrap_or(super::ClusterState::Anonymous);
    let centroid_hex: Option<String> = column!(row, "centroid_hex")?;
    let centroid = match centroid_hex {
        Some(h) => Some(super::from_hex(&h)?),
        None => None,
    };
    Ok(ClusterRow {
        id: column!(row, "id")?,
        handle: column!(row, "handle")?,
        state,
        centroid,
        appearance_count: column!(row, "appearance_count")?,
    })
}

/// Every cluster with its members, in a stable order.
pub async fn all_with_members(store: &Store) -> Result<Vec<ClusterWithMembers>, StoreError> {
    let rows = sqlx::query(
        "SELECT id, handle, state, centroid_hex, appearance_count \
         FROM person_cluster ORDER BY id",
    )
    .fetch_all(store.pool())
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let base = row_to_cluster(row)?;
        let members = sqlx::query(
            "SELECT id FROM appearance WHERE cluster_id = ? AND ambiguous = 0 ORDER BY id",
        )
        .bind(&base.id)
        .fetch_all(store.pool())
        .await?;
        let member_ids = members
            .into_iter()
            .map(|r| -> Result<String, StoreError> { column!(r, "id") })
            .collect::<Result<Vec<_>, _>>()?;
        out.push(ClusterWithMembers {
            id: base.id,
            handle: base.handle,
            state: base.state,
            centroid: base.centroid,
            appearance_count: base.appearance_count,
            member_ids,
        });
    }
    Ok(out)
}

/// Fetch one appearance.
pub async fn appearance(
    store: &Store,
    id: &str,
) -> Result<Option<AppearanceRecord>, StoreError> {
    let row = sqlx::query(
        "SELECT id, object_id, cluster_id, distance, face_score, body_score, ambiguous \
         FROM appearance WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(store.pool())
    .await?;
    row.map(row_to_appearance).transpose()
}

fn row_to_appearance(row: sqlx::sqlite::SqliteRow) -> Result<AppearanceRecord, StoreError> {
    Ok(AppearanceRecord {
        id: column!(row, "id")?,
        object_id: column!(row, "object_id")?,
        // Empty is the legacy marker for "belongs to no cluster" and is read as
        // `None`. See the module header: this is the state the schema cannot
        // express properly, and it is a read-side compatibility shim for rows
        // written before migration 0002.
        cluster_id: column!(row, "cluster_id")?.filter(|s: &String| !s.is_empty()),
        distance: column!(row, "distance")?,
        face_score: column!(row, "face_score")?,
        body_score: column!(row, "body_score")?,
        ambiguous: column!(row, "ambiguous")? != 0,
    })
}

/// The candidates for an ambiguous appearance: every cluster whose centroid is
/// within the threshold, with the distance that put it there.
pub async fn ambiguous_candidates(
    store: &Store,
    appearance_id: &str,
) -> Result<Vec<CandidateDistance>, StoreError> {
    let a = appearance(store, appearance_id)
        .await?
        .ok_or_else(|| StoreError::Query("no such appearance".into()))?;
    if !a.ambiguous {
        return Ok(Vec::new());
    }
    // The vector is not in SQL — it lives in the sidecar — so the candidates are
    // computed by the engine, which has it. This function is a thin read of what
    // the engine recorded at decision time, stored so the UI does not need the
    // sidecar to explain a decision it already made.
    store::read_candidates(store, appearance_id).await
}

mod inner {
    pub use super::*;
}

/// Read the candidate list recorded for an ambiguous appearance.
pub async fn read_candidates(
    store: &Store,
    appearance_id: &str,
) -> Result<Vec<CandidateDistance>, StoreError> {
    // Candidates are stored in the `field_proposal` table under a reserved
    // field, rather than in a bespoke table. That table is exactly "a value
    // proposed for a field of a subject", it is already indexed, and it means
    // the ambiguous-candidate list is a first-class proposal the UI can show,
    // vote on, or lock — which is what §7.1's "both candidates are shown" needs
    // and what a private column would not give.
    let rows = sqlx::query(
        "SELECT value_json FROM field_proposal \
         WHERE subject_type = 'appearance' AND subject_id = ? AND field = 'ambiguous_candidate' \
         ORDER BY created_at, id",
    )
    .bind(appearance_id)
    .fetch_all(store.pool())
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let json: String = column!(row, "value_json")?;
        // A hand-edited or truncated `value_json` must not take down the read of
        // every other candidate, so a bad row is skipped rather than propagated
        // — a candidate list missing one entry is recoverable, and a 500 is not.
        if let Ok(v) = serde_json::from_str::<CandidateDistance>(&json) {
            out.push(v);
        }
    }
    Ok(out)
}

/// Record one candidate for an ambiguous appearance.
pub async fn insert_candidate(
    store: &Store,
    appearance_id: &str,
    candidate: &CandidateDistance,
) -> Result<(), StoreError> {
    let json = serde_json::to_string(candidate)
        .map_err(|e| StoreError::Query(format!("candidate is not serialisable: {e}")))?;
    sqlx::query(
        "INSERT INTO field_proposal \
         (id, subject_type, subject_id, field, value_json, source, proposer_kind, confidence, created_at) \
         VALUES (?, 'appearance', ?, 'ambiguous_candidate', ?, 'clustering', 'system', 1.0, ?)",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(appearance_id)
    .bind(json)
    .bind(super::now())
    .execute(store.pool())
    .await?;
    Ok(())
}

/// Insert a `person_cluster` row.
pub async fn insert_cluster(
    store: &Store,
    id: &str,
    state: super::ClusterState,
    centroid_hex: Option<&str>,
    now: &str,
) -> Result<(), StoreError> {
    sqlx::query(
        "INSERT INTO person_cluster (id, handle, state, centroid_hex, appearance_count, created_at, updated_at) \
         VALUES (?, NULL, ?, ?, 0, ?, ?)",
    )
    .bind(id)
    .bind(state.as_str())
    .bind(centroid_hex)
    .bind(now)
    .bind(now)
    .execute(store.pool())
    .await?;
    Ok(())
}

/// Set a cluster's centroid.
pub async fn set_centroid(
    store: &Store,
    id: &str,
    centroid_hex: &str,
    now: &str,
) -> Result<(), StoreError> {
    sqlx::query("UPDATE person_cluster SET centroid_hex = ?, updated_at = ? WHERE id = ?")
        .bind(centroid_hex)
        .bind(now)
        .bind(id)
        .execute(store.pool())
        .await?;
    Ok(())
}

/// Increment a cluster's appearance count.
pub async fn bump_count(store: &Store, id: &str, now: &str) -> Result<(), StoreError> {
    sqlx::query(
        "UPDATE person_cluster \
         SET appearance_count = appearance_count + 1, updated_at = ? WHERE id = ?",
    )
    .bind(now)
    .bind(id)
    .execute(store.pool())
    .await?;
    Ok(())
}

/// Insert an `appearance` row.
///
/// `cluster_id` is `None` for an ambiguous appearance, which the current schema
/// cannot represent — see the module header. This returns
/// [`AmbiguousNeedsMigration`] rather than writing an empty string, because an
/// empty string is a dangling foreign key on any engine with constraints off and
/// an outright failure on any engine with them on.
pub async fn insert_appearance(
    store: &Store,
    id: &str,
    object_id: &str,
    cluster_id: &str,
    scores: &super::ScoreComponents,
    distance: Option<f32>,
    ambiguous: bool,
    now: &str,
) -> Result<(), StoreError> {
    if ambiguous {
        return Err(StoreError::Query(
            "an ambiguous appearance has no cluster, which schema 0001 cannot store; \
             migration 0002 is required"
                .into(),
        ));
    }
    sqlx::query(
        "INSERT INTO appearance \
         (id, object_id, cluster_id, appearance_type, face_id, distance, face_score, body_score, ambiguous, source, created_at) \
         VALUES (?, ?, ?, 'primary', NULL, ?, ?, ?, 0, 'clustering', ?)",
    )
    .bind(id)
    .bind(object_id)
    .bind(cluster_id)
    .bind(distance)
    .bind(scores.face)
    .bind(scores.body)
    .bind(now)
    .execute(store.pool())
    .await?;
    bump_count(store, cluster_id, now).await?;
    Ok(())
}

/// Propose a handle value for a cluster.
///
/// Recorded as a `field_proposal` rather than written to `person_cluster.handle`
/// directly, because §7.1's whole framing is that a link is a *proposal*: the
/// system does not get to name a person, and two proposals for the same cluster
/// with different values is the conflicting-evidence case rather than a last
/// write wins.
pub async fn propose_handle(
    store: &Store,
    cluster_id: &str,
    value: &str,
    source: &str,
) -> Result<(), StoreError> {
    let json = serde_json::to_string(&value)
        .map_err(|e| StoreError::Query(format!("handle is not serialisable: {e}")))?;
    sqlx::query(
        "INSERT INTO field_proposal \
         (id, subject_type, subject_id, field, value_json, source, proposer_kind, proposer_id, confidence, created_at) \
         VALUES (?, 'person_cluster', ?, 'handle', ?, ?, 'annotator', NULL, 1.0, ?)",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(cluster_id)
    .bind(json)
    .bind(source)
    .bind(super::now())
    .execute(store.pool())
    .await?;
    Ok(())
}

/// Every handle proposed for a cluster.
pub async fn handle_candidates(
    store: &Store,
    cluster_id: &str,
) -> Result<Vec<HandleCandidate>, StoreError> {
    let rows = sqlx::query(
        "SELECT value_json, source FROM field_proposal \
         WHERE subject_type = 'person_cluster' AND subject_id = ? AND field = 'handle' \
         ORDER BY created_at, id",
    )
    .bind(cluster_id)
    .fetch_all(store.pool())
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let json: String = column!(row, "value_json")?;
        let source: String = column!(row, "source")?;
        if let Ok(value) = serde_json::from_str::<String>(&json) {
            out.push(HandleCandidate { value, source });
        }
    }
    Ok(out)
}

/// Recompute a cluster's state from its proposals, and set its handle if exactly
/// one distinct value was proposed.
///
/// The rule: zero or one candidate leaves the state alone; two or more
/// *distinct* values make the cluster `ambiguous` and clear the handle. Two
/// proposals of the same value are agreement, not conflict.
pub async fn reconcile_state(
    store: &Store,
    cluster_id: &str,
) -> Result<super::ClusterState, StoreError> {
    let candidates = handle_candidates(store, cluster_id).await?;
    let mut distinct: Vec<&str> = candidates.iter().map(|c| c.value.as_str()).collect();
    distinct.sort_unstable();
    distinct.dedup();
    let now = super::now();
    let (state, handle) = match distinct.len() {
        0 => (super::ClusterState::Anonymous, None),
        1 => (super::ClusterState::Named, Some(distinct[0].to_string())),
        _ => (super::ClusterState::Ambiguous, None),
    };
    sqlx::query("UPDATE person_cluster SET state = ?, handle = ?, updated_at = ? WHERE id = ?")
        .bind(state.as_str())
        .bind(handle)
        .bind(now)
        .bind(cluster_id)
        .execute(store.pool())
        .await?;
    Ok(state)
}

/// The stored face vector for every non-ambiguous member of a cluster.
///
/// The vectors are not in SQL — they are in the sidecar — so this reads the
/// centroid-bearing membership the engine needs from `appearance.object_id`, and
/// the caller supplies the vectors it already has. Here it reads from
/// `field_proposal` rows the engine wrote, which keeps the centroid computation
/// honest: it averages *recorded* vectors, not a copy that could drift.
pub async fn member_vectors(
    store: &Store,
    cluster_id: &str,
) -> Result<Vec<Vec<f32>>, StoreError> {
    let rows = sqlx::query(
        "SELECT value_json FROM field_proposal \
         WHERE subject_type = 'person_cluster' AND subject_id = ? AND field = 'member_vector' \
         ORDER BY created_at, id",
    )
    .bind(cluster_id)
    .fetch_all(store.pool())
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let json: String = column!(row, "value_json")?;
        match serde_json::from_str::<Vec<f32>>(&json) {
            Ok(v) => out.push(v),
            // Same reasoning as the candidates: one bad row is skipped, not
            // propagated. A cluster with one unreadable member still gets a
            // centroid from the rest.
            Err(_) => continue,
        }
    }
    Ok(out)
}

/// Record a member's vector for centroid computation.
pub async fn insert_member_vector(
    store: &Store,
    cluster_id: &str,
    vector: &[f32],
) -> Result<(), StoreError> {
    let json = serde_json::to_string(vector)
        .map_err(|e| StoreError::Query(format!("vector is not serialisable: {e}")))?;
    sqlx::query(
        "INSERT INTO field_proposal \
         (id, subject_type, subject_id, field, value_json, source, proposer_kind, confidence, created_at) \
         VALUES (?, 'person_cluster', ?, 'member_vector', ?, 'clustering', 'system', 1.0, ?)",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(cluster_id)
    .bind(json)
    .bind(super::now())
    .execute(store.pool())
    .await?;
    Ok(())
}

/// Merge `from` into `to`, moving its members and destroying the empty cluster.
///
/// The guard that makes this safe is not here but in the caller: a merge is only
/// proposed when the *direct* distance passes. This function is the consequence,
/// and it is deliberately not the place that decides.
pub async fn merge(
    store: &Store,
    from: &str,
    to: &str,
) -> Result<u64, StoreError> {
    if from == to {
        return Err(StoreError::Query("a cluster cannot merge into itself".into()));
    }
    let now = super::now();
    // Recompute the survivor's centroid from the union of both membership sets
    // *before* moving anything, so there is no window in which the centroid
    // describes fewer members than the cluster has.
    let mut vectors = member_vectors(store, to).await?;
    vectors.extend(member_vectors(store, from).await?);
    if !vectors.is_empty() {
        let width = vectors[0].len();
        let mut mean = vec![0f32; width];
        for v in &vectors {
            for (m, x) in mean.iter_mut().zip(v) {
                *m += *x;
            }
        }
        let n = vectors.len() as f32;
        for m in mean.iter_mut() {
            *m /= n;
        }
        let norm = commons_ml::face::Embedder::l2_normalize(&mean);
        set_centroid(store, to, &super::hex(norm), &now).await?;
    }

    let moved = sqlx::query("UPDATE appearance SET cluster_id = ? WHERE cluster_id = ?")
        .bind(to)
        .bind(from)
        .execute(store.pool())
        .await?
        .rows_affected();

    // Member vectors follow the appearances, or the survivor's centroid is
    // recomputed from the loser's set forever after.
    sqlx::query(
        "UPDATE field_proposal SET subject_id = ? \
         WHERE subject_type = 'person_cluster' AND subject_id = ? AND field = 'member_vector'",
    )
    .bind(to)
    .bind(from)
    .execute(store.pool())
    .await?;

    sqlx::query(
        "UPDATE person_cluster \
         SET appearance_count = (SELECT COUNT(*) FROM appearance WHERE cluster_id = ?), updated_at = ? \
         WHERE id = ?",
    )
    .bind(to)
    .bind(now)
    .bind(to)
    .execute(store.pool())
    .await?;

    // The loser's proposals move too — including any *handle* proposals, which
    // is the point: two clusters that merge and each had a name are exactly the
    // conflicting-evidence case, and the merged cluster must be ambiguous.
    sqlx::query(
        "UPDATE field_proposal SET subject_id = ? \
         WHERE subject_type = 'person_cluster' AND subject_id = ? AND field = 'handle'",
    )
    .bind(to)
    .bind(from)
    .execute(store.pool())
    .await?;

    sqlx::query("DELETE FROM person_cluster WHERE id = ?")
        .bind(from)
        .execute(store.pool())
        .await?;
    let _ = moved;
    Ok(moved)
}

/// True when the object already has an appearance on this cluster.
pub async fn already_member(
    store: &Store,
    object_id: &str,
    cluster_id: &str,
) -> Result<bool, StoreError> {
    let row = sqlx::query(
        "SELECT 1 FROM appearance WHERE object_id = ? AND cluster_id = ? LIMIT 1",
    )
    .bind(object_id)
    .bind(cluster_id)
    .fetch_optional(store.pool())
    .await?;
    Ok(row.is_some())
}
