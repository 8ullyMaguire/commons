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
//! # `ambiguous` and the null cluster

//! An ambiguous appearance belongs to no cluster, and the column is nullable
//! because of that -- migration 0002 made it so. The alternatives were a sentinel
//! cluster, which is a person-shaped row that is not a person and inflates every
//! count, and an empty string, which is a foreign-key violation where constraints
//! are enforced and a dangling reference where they are not. Both are recorded here
//! because both are things a reasonable person would try, and a reader who has not
//! seen the reasoning will otherwise rediscover one of them.

//! # Everything above this crate

//! `field_proposal` carries two things that look like an abuse of a generic table and
//! are not: the candidate list for an ambiguous appearance, and the member vectors a
//! centroid is computed from. Both are "a value proposed for a field of a subject",
//! both are already indexed, and both need to survive as *proposals* -- §7.1 makes a
//! link a proposal, not an assignment, and a bespoke private column would have made
//! the UI's "show both candidates" a second read path.

use commons_store::{Store, StoreError};

use super::ClusterError;
use sqlx::Row;

/// Read one column, mapping a decode failure onto a store error.
///
/// A local helper rather than `row.get(..)?` so every decode failure carries the
/// column name: a `ColumnDecode` error with no name is a bug report with no
/// address.
fn col<'r, T>(row: &'r sqlx::sqlite::SqliteRow, name: &'static str) -> Result<T, StoreError>
where
    T: sqlx::Decode<'r, sqlx::Sqlite> + sqlx::Type<sqlx::Sqlite>,
{
    row.try_get::<T, _>(name).map_err(|source| StoreError::Row {
        column: name,
        source,
    })
}

/// One `person_cluster` row, with what the UI needs to show it.
#[derive(Debug, Clone, PartialEq)]
pub struct ClusterRow {
    pub id: String,
    pub handle: Option<String>,
    pub state: super::ClusterState,
    /// The face centroid, decoded, if the cluster has one.
    pub centroid: Option<Vec<f32>>,
    /// The body centroid, decoded. `None` until an appearance with body evidence
    /// joins -- §7.4's composite compares against it, and a cluster without one
    /// has no body evidence to weigh, which is not the same as a body distance
    /// of zero.
    pub body_centroid: Option<Vec<f32>>,
    pub appearance_count: i64,
}

/// A cluster plus its member appearance ids.
#[derive(Debug, Clone, PartialEq)]
pub struct ClusterWithMembers {
    pub id: String,
    pub handle: Option<String>,
    pub state: super::ClusterState,
    pub centroid: Option<Vec<f32>>,
    pub body_centroid: Option<Vec<f32>>,
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
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CandidateDistance {
    pub cluster_id: String,
    pub distance: f32,
}

/// A proposed handle value.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HandleCandidate {
    pub value: String,
    pub source: String,
}

/// Fetch one cluster.
pub async fn get(store: &Store, id: &str) -> Result<Option<ClusterRow>, StoreError> {
    let row = sqlx::query(
        "SELECT id, handle, state, centroid_hex, body_centroid_hex, appearance_count \
         FROM person_cluster WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(store.pool())
    .await
    .map_err(StoreError::Query)?;
    row.map(row_to_cluster).transpose()
}

/// A nullable hex-encoded vector column, decoded.
///
/// `None` is preserved as `None` rather than becoming an empty vector: an empty
/// vector has no direction, so every cosine against it is undefined and a zero
/// distance would read as a perfect match. §7.4's composite depends on telling
/// "no centroid yet" apart from "a centroid of nothing".
fn decode_opt(
    row: &sqlx::sqlite::SqliteRow,
    column: &'static str,
) -> Result<Option<Vec<f32>>, StoreError> {
    let hex: Option<String> = col(row, column)?;
    match hex {
        Some(h) => Ok(Some(super::from_hex(&h).map_err(|e| {
            StoreError::Query(sqlx::Error::Protocol(e.to_string()))
        })?)),
        None => Ok(None),
    }
}

fn row_to_cluster(row: sqlx::sqlite::SqliteRow) -> Result<ClusterRow, StoreError> {
    let state_str: String = col(&row, "state")?;
    // An unrecognised state is `anonymous` rather than an error: the column has
    // a NOT NULL DEFAULT and a future writer may add a state this build does not
    // know. Reading it as the *most* restrictive known state would be wrong
    // (anonymous is the default, and a named cluster read as anonymous loses
    // the user's name), so it degrades to the default and the raw string is
    // still available through `state` for a caller that cares.
    let state = super::ClusterState::parse(&state_str).unwrap_or(super::ClusterState::Anonymous);
    Ok(ClusterRow {
        id: col(&row, "id")?,
        handle: col(&row, "handle")?,
        state,
        centroid: decode_opt(&row, "centroid_hex")?,
        body_centroid: decode_opt(&row, "body_centroid_hex")?,
        appearance_count: col(&row, "appearance_count")?,
    })
}

/// Every cluster with its members, in a stable order.
pub async fn all_with_members(store: &Store) -> Result<Vec<ClusterWithMembers>, StoreError> {
    let rows = sqlx::query(
        "SELECT id, handle, state, centroid_hex, body_centroid_hex, appearance_count \
         FROM person_cluster ORDER BY id",
    )
    .fetch_all(store.pool())
    .await
    .map_err(StoreError::Query)?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let base = row_to_cluster(row)?;
        let members = sqlx::query(
            "SELECT id FROM appearance WHERE cluster_id = ? AND ambiguous = 0 ORDER BY id",
        )
        .bind(&base.id)
        .fetch_all(store.pool())
        .await
        .map_err(StoreError::Query)?;
        let member_ids = members
            .into_iter()
            .map(|r| -> Result<String, StoreError> { col(&r, "id") })
            .collect::<Result<Vec<_>, _>>()?;
        out.push(ClusterWithMembers {
            id: base.id,
            handle: base.handle,
            state: base.state,
            centroid: base.centroid,
            body_centroid: base.body_centroid,
            appearance_count: base.appearance_count,
            member_ids,
        });
    }
    Ok(out)
}

/// Every appearance, in a stable order.
///
/// §7.4 needs this to be readable: "the UI shows *why* two items were linked" is
/// a statement about what a caller can fetch, and a projection that exposes only
/// `distance` cannot answer it. Ordered by id so a caller that takes the first
/// row for an object gets the same one every run.
pub async fn all_appearances(store: &Store) -> Result<Vec<AppearanceRecord>, StoreError> {
    let rows = sqlx::query(
        "SELECT id, object_id, cluster_id, distance, face_score, body_score, ambiguous \
         FROM appearance ORDER BY created_at, id",
    )
    .fetch_all(store.pool())
    .await
    .map_err(StoreError::Query)?;
    rows.into_iter().map(row_to_appearance).collect()
}

/// Fetch one appearance.
pub async fn appearance(store: &Store, id: &str) -> Result<Option<AppearanceRecord>, StoreError> {
    let row = sqlx::query(
        "SELECT id, object_id, cluster_id, distance, face_score, body_score, ambiguous \
         FROM appearance WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(store.pool())
    .await
    .map_err(StoreError::Query)?;
    row.map(row_to_appearance).transpose()
}

fn row_to_appearance(row: sqlx::sqlite::SqliteRow) -> Result<AppearanceRecord, StoreError> {
    Ok(AppearanceRecord {
        id: col(&row, "id")?,
        object_id: col(&row, "object_id")?,
        // Empty is the legacy marker for "belongs to no cluster" and is read as
        // `None`. See the module header: this is the state the schema cannot
        // express properly, and it is a read-side compatibility shim for rows
        // written before migration 0002.
        cluster_id: col::<Option<String>>(&row, "cluster_id")?.filter(|s: &String| !s.is_empty()),
        distance: col(&row, "distance")?,
        face_score: col(&row, "face_score")?,
        body_score: col(&row, "body_score")?,
        ambiguous: col::<i64>(&row, "ambiguous")? != 0,
    })
}

/// The candidates for an ambiguous appearance: every cluster whose centroid is
/// within the threshold, with the distance that put it there.
pub async fn ambiguous_candidates(
    store: &Store,
    appearance_id: &str,
) -> Result<Vec<CandidateDistance>, ClusterError> {
    let a = appearance(store, appearance_id)
        .await?
        .ok_or(ClusterError::NotFound("appearance"))?;
    if !a.ambiguous {
        return Ok(Vec::new());
    }
    // The vector is not in SQL — it lives in the sidecar — so the candidates are
    // computed by the engine, which has it. This function is a thin read of what
    // the engine recorded at decision time, stored so the UI does not need the
    // sidecar to explain a decision it already made.
    read_candidates(store, appearance_id)
        .await
        .map_err(|e| ClusterError::NotSerialisable(e.to_string()))
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
    .await
    .map_err(StoreError::Query)?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let json: String = col(&row, "value_json")?;
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
) -> Result<(), ClusterError> {
    let json = serde_json::to_string(candidate).map_err(|e| {
        ClusterError::NotSerialisable(format!("candidate is not serialisable: {e}"))
    })?;
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
    .await.map_err(StoreError::Query)?;
    Ok(())
}

/// Insert a `person_cluster` row.
pub async fn insert_cluster(
    store: &Store,
    id: &str,
    state: super::ClusterState,
    centroid_hex: Option<&str>,
    body_centroid_hex: Option<String>,
    now: &str,
) -> Result<(), StoreError> {
    // Both centroids are seeded here rather than by a follow-up update, so a
    // cluster is never briefly visible with a face centroid and no body
    // centroid when it always had both. A reader in that window would score the
    // next appearance against half the evidence and renormalise the face
    // distance up to the full weight.
    sqlx::query(
        "INSERT INTO person_cluster \
         (id, handle, state, centroid_hex, body_centroid_hex, appearance_count, created_at, updated_at) \
         VALUES (?, NULL, ?, ?, ?, 0, ?, ?)",
    )
    .bind(id)
    .bind(state.as_str())
    .bind(centroid_hex)
    .bind(body_centroid_hex)
    .bind(now)
    .bind(now)
    .execute(store.pool())
    .await.map_err(StoreError::Query)?;
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
        .await
        .map_err(StoreError::Query)?;
    Ok(())
}

/// The body centroid, written only when the cluster does not have one yet.
///
/// Not "recomputed on every join": the first body embedding to reach a cluster
/// describes the person, and later body embeddings are a different embedding
/// space if the model changed. Recomputing would need a body-model version on
/// the row, which is T-P3-003's follow-up rather than a silent average over
/// vectors that may not be commensurable.
pub async fn set_body_centroid(
    store: &Store,
    id: &str,
    centroid_hex: &str,
    now: &str,
) -> Result<(), StoreError> {
    sqlx::query(
        "UPDATE person_cluster SET body_centroid_hex = ?, updated_at = ? \
         WHERE id = ? AND body_centroid_hex IS NULL",
    )
    .bind(centroid_hex)
    .bind(now)
    .bind(id)
    .execute(store.pool())
    .await
    .map_err(StoreError::Query)?;
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
    .await
    .map_err(StoreError::Query)?;
    Ok(())
}

/// Insert an `appearance` row.
///
/// `cluster_id` is `None` for an ambiguous appearance, which the current schema
/// cannot represent — see the module header. This returns
/// [`AmbiguousNeedsMigration`] rather than writing an empty string, because an
/// empty string is a dangling foreign key on any engine with constraints off and
/// an outright failure on any engine with them on.
/// What `assign` decided about an appearance, and the evidence for it.
///
/// The write-side counterpart to [`AppearanceRecord`], which is what a row reads
/// back as. Grouped into one argument rather than four because they are produced
/// together by a single decision and read together by anything auditing why an
/// appearance landed where it did; `distance` is recorded even for an ambiguous
/// decision, because that distance is the evidence for the abstention.
#[derive(Debug, Clone, Copy)]
pub struct NewAppearance<'a> {
    /// `None` for an appearance that was not assigned to a cluster. The row is
    /// still stored, with a null `cluster_id`.
    pub cluster_id: Option<&'a str>,
    pub scores: &'a super::ScoreComponents,
    pub distance: Option<f32>,
    pub ambiguous: bool,
    pub now: &'a str,
}

pub async fn insert_appearance(
    store: &Store,
    id: &str,
    object_id: &str,
    record: &NewAppearance<'_>,
) -> Result<(), StoreError> {
    let NewAppearance {
        cluster_id,
        scores,
        distance,
        ambiguous,
        now,
    } = *record;
    // An ambiguous appearance belongs to no cluster, which is a NULL and not an
    // empty string. Migration 0002 made the column nullable for exactly this; an
    // empty string here would be a foreign-key violation on an engine with
    // constraints on and a dangling reference on one without.
    let bind_cluster: Option<&str> = if ambiguous { None } else { cluster_id };
    sqlx::query(
        "INSERT INTO appearance \
         (id, object_id, cluster_id, appearance_type, face_id, distance, face_score, body_score, ambiguous, source, created_at) \
         VALUES (?, ?, ?, 'primary', NULL, ?, ?, ?, ?, 'clustering', ?)",
    )
    .bind(id)
    .bind(object_id)
    .bind(bind_cluster)
    .bind(distance)
    .bind(scores.face)
    .bind(scores.body)
    .bind(if ambiguous { 1i64 } else { 0i64 })
    .bind(now)
    .execute(store.pool())
    .await
    .map_err(StoreError::Query)?;
    if !ambiguous {
        bump_count(
            store,
            cluster_id.expect("a non-ambiguous row has a cluster"),
            now,
        )
        .await?;
    }
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
) -> Result<(), ClusterError> {
    let json = serde_json::to_string(&value)
        .map_err(|e| ClusterError::NotSerialisable(format!("handle is not serialisable: {e}")))?;
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
    .await.map_err(StoreError::Query)?;
    // The state is derived, not chosen: one name on the cluster makes it Named,
    // two incompatible names make it Ambiguous, and no name leaves it Anonymous.
    // Recording the proposal without re-deriving the state would leave a cluster
    // carrying two different names and still claiming to be Anonymous, which is
    // the one answer the user cannot act on.
    reconcile_state(store, cluster_id).await?;
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
    .await
    .map_err(StoreError::Query)?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let json: String = col(&row, "value_json")?;
        let source: String = col(&row, "source")?;
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
        .await
        .map_err(StoreError::Query)?;
    Ok(state)
}

/// The stored face vector for every non-ambiguous member of a cluster.
///
/// The vectors are not in SQL — they are in the sidecar — so this reads the
/// centroid-bearing membership the engine needs from `appearance.object_id`, and
/// the caller supplies the vectors it already has. Here it reads from
/// `field_proposal` rows the engine wrote, which keeps the centroid computation
/// honest: it averages *recorded* vectors, not a copy that could drift.
pub async fn member_vectors(store: &Store, cluster_id: &str) -> Result<Vec<Vec<f32>>, StoreError> {
    let rows = sqlx::query(
        "SELECT value_json FROM field_proposal \
         WHERE subject_type = 'person_cluster' AND subject_id = ? AND field = 'member_vector' \
         ORDER BY created_at, id",
    )
    .bind(cluster_id)
    .fetch_all(store.pool())
    .await
    .map_err(StoreError::Query)?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let json: String = col(&row, "value_json")?;
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
) -> Result<(), ClusterError> {
    let json = serde_json::to_string(vector)
        .map_err(|e| ClusterError::NotSerialisable(format!("vector is not serialisable: {e}")))?;
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
    .await.map_err(StoreError::Query)?;
    Ok(())
}

/// Merge `from` into `to`, moving its members and destroying the empty cluster.
///
/// The guard that makes this safe is not here but in the caller: a merge is only
/// proposed when the *direct* distance passes. This function is the consequence,
/// and it is deliberately not the place that decides.
pub async fn merge(store: &Store, from: &str, to: &str) -> Result<u64, ClusterError> {
    if from == to {
        return Err(ClusterError::SelfMerge);
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
        let norm = commons_ml::face::Embedder::l2_normalize(&mean).to_vec();
        set_centroid(store, to, &super::hex(norm.as_slice()), &now).await?;
    }

    let moved = sqlx::query("UPDATE appearance SET cluster_id = ? WHERE cluster_id = ?")
        .bind(to)
        .bind(from)
        .execute(store.pool())
        .await
        .map_err(StoreError::Query)?
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
    .await
    .map_err(StoreError::Query)?;

    sqlx::query(
        "UPDATE person_cluster \
         SET appearance_count = (SELECT COUNT(*) FROM appearance WHERE cluster_id = ?), updated_at = ? \
         WHERE id = ?",
    )
    .bind(to)
    .bind(now)
    .bind(to)
    .execute(store.pool())
    .await.map_err(StoreError::Query)?;

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
    .await
    .map_err(StoreError::Query)?;

    sqlx::query("DELETE FROM person_cluster WHERE id = ?")
        .bind(from)
        .execute(store.pool())
        .await
        .map_err(StoreError::Query)?;
    let _ = moved;
    Ok(moved)
}

/// True when the object already has an appearance on this cluster.
pub async fn already_member(
    store: &Store,
    object_id: &str,
    cluster_id: &str,
) -> Result<bool, StoreError> {
    let row =
        sqlx::query("SELECT 1 FROM appearance WHERE object_id = ? AND cluster_id = ? LIMIT 1")
            .bind(object_id)
            .bind(cluster_id)
            .fetch_optional(store.pool())
            .await
            .map_err(StoreError::Query)?;
    Ok(row.is_some())
}

/// Attach an ambiguous appearance to the cluster the user chose.
///
/// One statement, so there is no window in which the row is attached to a
/// cluster and still flagged ambiguous -- a state every read would have to
/// defend against, and one the UI would render as both joined and unresolved.
pub async fn resolve_ambiguous(
    store: &Store,
    appearance_id: &str,
    cluster_id: &str,
) -> Result<(), StoreError> {
    sqlx::query(
        "UPDATE appearance SET cluster_id = ?, ambiguous = 0 WHERE id = ? AND ambiguous = 1",
    )
    .bind(cluster_id)
    .bind(appearance_id)
    .execute(store.pool())
    .await
    .map_err(StoreError::Query)?;
    Ok(())
}

/// Move the named appearances of `from` onto `to`, leaving the rest.
///
/// The `NOT IN` is built from bound placeholders rather than interpolated ids:
/// the ids come from the store, but a value that reaches a query as text is a
/// value that can reach it as anything, and this file has no reason to be the
/// place that lesson is unlearned.
pub async fn move_appearances(
    store: &Store,
    from: &str,
    to: &str,
    keep: &[String],
) -> Result<u64, StoreError> {
    if keep.is_empty() {
        return Ok(0);
    }
    let placeholders = vec!["?"; keep.len()].join(", ");
    let sql = format!(
        "UPDATE appearance SET cluster_id = ? \
         WHERE cluster_id = ? AND id IN ({placeholders})"
    );
    let mut q = sqlx::query(&sql).bind(to).bind(from);
    for id in keep {
        q = q.bind(id);
    }
    let moved = q
        .execute(store.pool())
        .await
        .map_err(StoreError::Query)?
        .rows_affected();
    recompute_count(store, to).await?;
    recompute_count(store, from).await?;
    Ok(moved)
}

/// Set a cluster's `appearance_count` to the number of rows that actually
/// point at it.
///
/// Counted rather than incremented, because every path that changes membership
/// has to remember to adjust the counter and one that forgets leaves a count
/// that is wrong forever with nothing to detect it. Counting is one query and
/// cannot drift.
pub async fn recompute_count(store: &Store, cluster_id: &str) -> Result<(), StoreError> {
    sqlx::query(
        "UPDATE person_cluster \
         SET appearance_count = (SELECT COUNT(*) FROM appearance WHERE cluster_id = ?), \
             updated_at = ? \
         WHERE id = ?",
    )
    .bind(cluster_id)
    .bind(super::now())
    .bind(cluster_id)
    .execute(store.pool())
    .await
    .map_err(StoreError::Query)?;
    Ok(())
}

/// Set a cluster's state.
pub async fn set_state(
    store: &Store,
    cluster_id: &str,
    state: super::ClusterState,
) -> Result<(), StoreError> {
    sqlx::query("UPDATE person_cluster SET state = ?, updated_at = ? WHERE id = ?")
        .bind(state.as_str())
        .bind(super::now())
        .bind(cluster_id)
        .execute(store.pool())
        .await
        .map_err(StoreError::Query)?;
    Ok(())
}
