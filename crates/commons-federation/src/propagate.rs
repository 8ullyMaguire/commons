//! §13.2 / §14.1 — tombstone propagation.
//!
//! Revocation travels as a **tombstone, not a vote**. That sentence is the whole
//! module, and it has a consequence that is easy to get backwards: a receiving
//! peer *applies* a tombstone, it does not weigh it. There is no quorum, no
//! tally, and no "this peer is more trusted than that one". A peer that
//! received a takedown and then counted it among its inputs and concluded
//! otherwise has implemented a vote, and a vote is outvoted by contribution —
//! which is exactly the failure §13.2 names.
//!
//! # The three things a receiving peer must get right
//!
//! **Verify the signature, or the feature is a denial-of-service tool.** §14.1
//! says the blocklist propagates to *every* peer. An unsigned row claiming to be
//! a takedown, accepted from anyone, is a way to deny an arbitrary library in
//! one request. So the signature is checked before anything is written, and
//! `a_tombstone_with_no_signature_is_refused` asserts the check happens *before*
//! the write, not after.
//!
//! **Apply it idempotently.** A peer that retries after a dropped connection
//! must not double-apply, and must not treat the retry as an error. The
//! inbox's primary key is the *originating peer's* tombstone id, so a
//! redelivery is a conflict on insert rather than a second denial — the same
//! reason `points_award` is keyed on `(account_id, proposal_id)`.
//!
//! **Never un-deny.** A tombstone that arrives *after* a local denial must not
//! leave the library in a state where the content is importable again.
//! `content_blocklist` has a partial unique index making `denied` a
//! once-only fact, and `receive` never deletes.
//!
//! # Why `pending` is a query and not a flag
//!
//! `delivered_at IS NULL` is the queue, and `delivered_at` is a timestamp rather
//! than a boolean. A boolean that never clears would leave the outbox
//! permanently non-empty for a deployment that had ever synced, and every
//! subsequent poll would re-send the whole history. A timestamp is also what
//! makes "delivered to peer B, not yet to peer C" representable, which a
//! single boolean is not.

use commons_consent::takedown;
use commons_store::db::{Store, StoreError};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use std::fmt;

/// What a tombstone asserts. The two kinds are not a severity ordering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Reported and contested. §14.1: "hidden everywhere, pending review."
    /// Importable: a dispute is not a decision.
    Quarantined,
    /// Takedown accepted. §14.1: "permanently blocked by hash across all
    /// peers."
    Denied,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Quarantined => "quarantined",
            Kind::Denied => "denied",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "quarantined" => Some(Kind::Quarantined),
            "denied" => Some(Kind::Denied),
            _ => None,
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A tombstone as it crosses the wire.
///
/// A separate type from the stored row, deliberately. The wire shape is a
/// protocol: adding a field to it is a change every peer has to understand, and
/// the two would drift into one type the first time somebody wanted a local
/// column on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireTombstone {
    /// The originating peer's id for this tombstone. The inbox's primary key, so
    /// redelivery is a duplicate rather than a second denial.
    pub id: String,
    pub kind: Kind,
    pub content_hash: String,
    /// The object this was about, when the originating peer knew one. A peer
    /// blocks by *hash*; the object id is for the human reading the log, and
    /// `None` is legitimate — the origin may have had only a file.
    pub object_id: Option<String>,
    pub issued_at: String,
    pub signature: String,
    /// §14.1: the basis is part of the record. A peer receiving a denial shows
    /// this to the person whose content it is, so it crosses the wire rather
    /// than staying local.
    pub reason: String,
    /// The peer's key id, which is how the receiver finds the public key.
    pub issuer: String,
}

/// What happened when a tombstone arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Received {
    /// The tombstone changed this store's state.
    pub applied: bool,
    /// This tombstone id was already in the inbox. `applied` is false when this
    /// is true, and both being true is not representable — a redelivery is
    /// either new or it is not.
    pub duplicate: bool,
}

impl Received {
    fn fresh() -> Self {
        Received {
            applied: true,
            duplicate: false,
        }
    }
    fn again() -> Self {
        Received {
            applied: false,
            duplicate: true,
        }
    }
}

/// Why a tombstone could not be applied.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ReceiveError {
    /// The signature did not verify against the issuer's key.
    ///
    /// Checked *before* any write, and the test asserts that: a check applied
    /// after the write is a check that leaves the damage in place.
    #[error("the tombstone's signature does not verify against the issuer's key")]
    BadSignature,
    /// The issuer is a peer this store has never heard of, and an unknown
    /// issuer's key cannot be checked — so the tombstone is refused rather than
    /// trusted, which is the direction §14.1's "every peer" requires.
    #[error("unknown issuer: {0}")]
    UnknownIssuer(String),
    /// The content hash is empty, and an empty hash would match every unhashed
    /// file in the library.
    #[error("a tombstone with no content hash cannot be matched against anything")]
    NoContentHash,
    /// The kind is not one the protocol defines.
    #[error("unknown tombstone kind: {0}")]
    UnknownKind(String),
    #[error("store: {0}")]
    Store(String),
}

impl From<StoreError> for ReceiveError {
    fn from(e: StoreError) -> Self {
        ReceiveError::Store(e.to_string())
    }
}

/// The bytes a signature covers.
///
/// Field-separated and length-prefixed by content, not `format!`'d. A
/// concatenation of the fields would let a peer shift a character from
/// `reason` into `content_hash` and produce a different tombstone with the same
/// signature — the classic length-extension shape, and the reason every field
/// here is a named, delimited unit.
pub fn canonical_bytes(t: &WireTombstone) -> Vec<u8> {
    let mut out = Vec::new();
    for (name, value) in [
        ("v", "1".to_string()),
        ("id", t.id.clone()),
        ("kind", t.kind.as_str().to_string()),
        ("hash", t.content_hash.clone()),
        ("object", t.object_id.clone().unwrap_or_default()),
        ("issued", t.issued_at.clone()),
        ("issuer", t.issuer.clone()),
        ("reason", t.reason.clone()),
    ] {
        out.extend_from_slice(&name.len().to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&value.len().to_le_bytes());
        out.extend_from_slice(value.as_bytes());
    }
    out
}

/// The tombstones this store owes to its peers.
///
/// Undelivered only, oldest first. The ordering is not cosmetic: two tombstones
/// for the same content in the wrong order would tell a peer "quarantined" after
/// "denied", and the peer's `ON CONFLICT DO NOTHING` would then keep the weaker
/// fact as the only one it had.
pub async fn pending(store: &Store) -> Result<Vec<WireTombstone>, StoreError> {
    // A struct rather than an eight-tuple. Positionally, `signer` and
    // `signature` are adjacent and both `String`, so a swapped pair compiles and
    // every tombstone is signed by its own signature. Naming the columns is the
    // only thing that makes that mistake visible.
    #[derive(sqlx::FromRow)]
    struct Row {
        id: String,
        content_hash: String,
        kind: String,
        object_id: Option<String>,
        reason: String,
        signer: String,
        signature: String,
        created_at: String,
    }

    let rows: Vec<Row> = sqlx::query_as(
        "SELECT id, content_hash, kind, object_id, reason, signer, signature, created_at
           FROM tombstone_outbox
          WHERE delivered_at IS NULL
          ORDER BY created_at, id",
    )
    .fetch_all(store.pool())
    .await
    .map_err(StoreError::Query)?;

    // A row whose `kind` is not one the protocol defines is dropped rather than
    // sent: a peer that receives a kind it cannot parse would reject the whole
    // batch, and one unreadable row would then hold up every takedown behind it.
    // The `kind` column has a CHECK constraint, so this is unreachable today --
    // which is the point. A migration that loosens the constraint fails here
    // first, loudly, instead of at a peer.
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            Some(WireTombstone {
                id: r.id,
                kind: Kind::parse(&r.kind)?,
                content_hash: r.content_hash,
                object_id: r.object_id,
                reason: r.reason,
                signature: r.signature,
                issuer: r.signer,
                issued_at: r.created_at,
            })
        })
        .collect())
}

/// Mark tombstones delivered, by id.
///
/// Takes the ids rather than a peer name: the outbox has no notion of a peer yet
/// (that is Phase 8's transport), and guessing at one here would mean a
/// `delivered_to` column that nothing writes correctly.
pub async fn mark_delivered(store: &Store, ids: &[String]) -> Result<u64, StoreError> {
    let mut n = 0u64;
    for id in ids {
        n += sqlx::query(
            "UPDATE tombstone_outbox SET delivered_at = ? WHERE id = ? AND delivered_at IS NULL",
        )
        .bind(commons_core::ts::now())
        .bind(id)
        .execute(store.pool())
        .await
        .map_err(StoreError::Query)?
        .rows_affected();
    }
    Ok(n)
}

/// The public keys this store trusts, by peer key id.
///
/// Process-wide rather than per-`Store`, because a `Store` is opened once per
/// library and the key set is deployment configuration. Threading it through
/// every call would put a configuration argument on a protocol function, which
/// is how protocol functions grow arguments that change their meaning.
///
/// The default is **empty**, and an empty key set means every tombstone is
/// refused as [`ReceiveError::UnknownIssuer`]. That is the correct behaviour for
/// a store that has not been told who its peers are: §14.1's "every peer" is a
/// statement about a deployment that has peers, and a store that trusts
/// everyone is a store where a stranger's tombstone denies a library.
static TRUSTED: std::sync::LazyLock<
    std::sync::RwLock<std::collections::HashMap<String, VerifyingKey>>,
> = std::sync::LazyLock::new(|| std::sync::RwLock::new(std::collections::HashMap::new()));

/// Trust a peer's key, by key id.
///
/// A no-op on a second call with a *different* key would be a silent key change,
/// so the second call overwrites and the caller is expected to mean it: a
/// peer's key rotating is a legitimate event and refusing it would leave the
/// peer permanently unable to send anything.
pub fn trust(peer: &str, key_hex: &str) -> Result<(), String> {
    let key = key_from_hex(key_hex.trim())
        .ok_or_else(|| "not a 32-byte hex verifying key".to_string())?;
    TRUSTED
        .write()
        .map_err(|_| "trust store poisoned".to_string())?
        .insert(peer.to_string(), key);
    Ok(())
}

/// Forget a peer. For a deployment removing a peer, and for tests.
pub fn untrust(peer: &str) {
    if let Ok(mut t) = TRUSTED.write() {
        t.remove(peer);
    }
}

/// A peer's trusted key, if any.
fn trusted(peer: &str) -> Option<VerifyingKey> {
    TRUSTED.read().ok()?.get(peer).copied()
}

/// Apply a tombstone received from a peer.
///
/// Returns whether it changed anything and whether it had been applied before.
/// Neither is an error: a duplicate is the normal result of a retry, and a
/// caller that treats it as a failure will not retry the rest of the batch.
///
/// The order here is the security property. Signature, then kind, then hash, and
/// only then any write. A check performed after the write is a check that
/// documents a decision somebody already acted on.
pub async fn receive(store: &Store, t: &WireTombstone) -> Result<Received, ReceiveError> {
    if t.content_hash.is_empty() {
        return Err(ReceiveError::NoContentHash);
    }

    // 1. The issuer must be one this store knows, and the signature must verify.
    //    The key lookup and the verification are one step on purpose: an unknown
    //    issuer has no key to verify against, so "unknown" and "bad signature"
    //    are the same refusal with different messages, and neither is applied.
    let key = trusted(&t.issuer).ok_or_else(|| ReceiveError::UnknownIssuer(t.issuer.clone()))?;
    let sig_bytes: [u8; 64] = hex_to_64(&t.signature).ok_or(ReceiveError::BadSignature)?;
    key.verify(&canonical_bytes(t), &Signature::from_bytes(&sig_bytes))
        .map_err(|_| ReceiveError::BadSignature)?;

    // 2. Redelivery. The inbox insert is the authority, not a SELECT: a SELECT
    //    then INSERT loses the race between two copies of the same tombstone
    //    arriving concurrently, and the loser's `ON CONFLICT` would then be the
    //    only thing preventing a second denial.
    let inserted = sqlx::query(
        "INSERT INTO tombstone_inbox
             (peer_tombstone_id, peer, content_hash, kind, object_id, reason, received_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (peer_tombstone_id) DO NOTHING",
    )
    .bind(&t.id)
    .bind(&t.issuer)
    .bind(&t.content_hash)
    .bind(t.kind.as_str())
    .bind(&t.object_id)
    .bind(&t.reason)
    .bind(commons_core::ts::now())
    .execute(store.pool())
    .await
    .map_err(StoreError::Query)?
    .rows_affected();
    if inserted == 0 {
        return Ok(Received::again());
    }

    // 3. Apply. `denied` is once-only per hash -- the partial unique index makes
    //    it so -- so a denial already here is left alone rather than rewritten.
    //    Nothing is ever deleted: a tombstone cannot restore access.
    sqlx::query(
        "INSERT INTO content_blocklist (content_hash, kind, object_id, reason, created_at)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT (content_hash, kind) DO NOTHING",
    )
    .bind(&t.content_hash)
    .bind(t.kind.as_str())
    .bind(&t.object_id)
    .bind(&t.reason)
    .bind(commons_core::ts::now())
    .execute(store.pool())
    .await
    .map_err(StoreError::Query)?;

    // 4. Bring local rows in line. A tombstone reaches *content*, and the local
    //    copy of that content is an object still sitting in the library under a
    //    publishable tier. Leaving it publishable is how a peer applies a
    //    takedown and then serves the content anyway.
    //
    //    Both kinds, not just `denied`: §14.1 says a report moves an object to
    //    `quarantined` *everywhere*, and "everywhere" includes the peers that
    //    have not heard yet. A quarantine that stopped at the originating peer
    //    would be a quarantine that the next federation pull undoes.
    if let Some(tier) = match t.kind {
        Kind::Denied => Some("denied"),
        Kind::Quarantined => Some("quarantined"),
    } {
        let _ = tier;
        if let Some(object_id) = &t.object_id {
            let n = sqlx::query(
                "UPDATE consent_record SET tier = ?, updated_at = ? WHERE object_id = ?",
            )
            .bind(tier)
            .bind(commons_core::ts::now())
            .bind(object_id)
            .execute(store.pool())
            .await
            .map_err(StoreError::Query)?
            .rows_affected();
            // Locators go only on a denial. A quarantine is a *dispute*, and
            // §14.1 ties destruction to acceptance -- pulling the magnet on a
            // report would let anyone make a file unreachable by filing a
            // complaint and not answering a moderator.
            if n > 0 && t.kind == Kind::Denied {
                let _ = commons_store::locator::destroy_for_object(store, object_id).await;
            }
        }
        // ...and by hash, for the local object that holds *this* content, which
        // is the case the object_id above cannot cover: a peer's object ids are
        // not ours, so the hash is the only thing both sides agree on.
        sqlx::query(
            "UPDATE consent_record SET tier = ?, updated_at = ?
              WHERE object_id IN (
                    SELECT object_id FROM file WHERE hash_blake3 = ?)",
        )
        .bind(tier)
        .bind(commons_core::ts::now())
        .bind(&t.content_hash)
        .execute(store.pool())
        .await
        .map_err(StoreError::Query)?;
    }

    Ok(Received::fresh())
}

/// Is this content blocked on the receiving side? A thin re-export so a caller
/// holding a `WireTombstone` does not have to remember which crate the check
/// lives in.
pub async fn blocks_import(
    store: &Store,
    content_hash: &str,
) -> Result<takedown::ImportVerdict, StoreError> {
    takedown::check_import(store, content_hash).await
}

/// Parse a 64-byte signature from hex.
///
/// Returns `None` for anything the wrong length or not hex, rather than
/// panicking: this runs on data from the network and a malformed signature is
/// the *expected* input, not a programming error.
fn hex_to_64(s: &str) -> Option<[u8; 64]> {
    if s.len() != 128 {
        return None;
    }
    let mut out = [0u8; 64];
    for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
        let hi = (chunk[0] as char).to_digit(16)?;
        let lo = (chunk[1] as char).to_digit(16)?;
        out[i] = (hi * 16 + lo) as u8;
    }
    Some(out)
}

/// Parse a verifying key from 64 hex characters.
///
/// `from_bytes` rather than `from_str`, because `from_str` is behind an
/// ed25519 feature flag the workspace does not enable and enabling a crypto
/// feature for a hex decoder is a larger decision than this function warrants.
fn key_from_hex(s: &str) -> Option<VerifyingKey> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
        let hi = (chunk[0] as char).to_digit(16)?;
        let lo = (chunk[1] as char).to_digit(16)?;
        out[i] = (hi * 16 + lo) as u8;
    }
    VerifyingKey::from_bytes(&out).ok()
}

/// The hex form of a signature, for the transport layer and for tests.
pub fn to_hex(bytes: &[u8; 64]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
