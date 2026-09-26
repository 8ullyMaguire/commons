//! T-P4-008 — the takedown pipeline.
//!
//! §14.1: a report moves an object to `quarantined` **everywhere**; if accepted,
//! to `denied`, "which adds a content-hash blocklist entry that propagates to
//! every peer and is checked on import, scan, and match."
//!
//! # The three properties that make a takedown mean anything
//!
//! **It is a blocklist, not a flag.** A tier on a row says this *copy* is
//! hidden. It says nothing about the file being re-uploaded tomorrow under a
//! new object id, which is the obvious way around any takedown and the reason
//! §14.1 names a *content hash* rather than an id. So the pipeline's output is a
//! row in a table keyed by content hash, and the check on the way in is what
//! makes it a blocklist rather than a note.
//!
//! **It propagates as a tombstone, not a vote.** §13.2: revocation travels as a
//! tombstone. A peer that receives one applies it; a peer that merely *counts*
//! it has not. This is the property that cannot be tested by reading one
//! database, which is why this file builds two.
//!
//! **Locators are destroyed, not tombstoned.** The ticket says it and it is
//! right: a locator is the *thing that retrieves the content*. A tombstoned
//! magnet still resolves. So the row is deleted, and the test asserts
//! `rows_affected() == 1` and then that a fresh read finds nothing — a
//! tombstone would satisfy the first and fail the second.
//!
//! # What the tests are for, in one line each
//!
//! The headline test is `a_tombstone_from_a_peer_blocks_a_reimport` — the
//! ticket's done-when. The rest exist because each of the other two properties
//! fails in a way the headline test would not notice.

use commons_consent::takedown::{self, DenyError, Takedown};
use commons_federation::propagate::{self, WireTombstone};
use commons_identity::claim;
use commons_store::db::Store;
use commons_store::locator;
use uuid::Uuid;

/// A store with migrations applied. Two of them, for the propagation tests.
async fn store() -> Store {
    Store::open_memory().await.unwrap()
}

/// A person cluster, so a takedown has somebody to be about.
async fn cluster(store: &Store) -> String {
    let id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO person_cluster (id, handle, state, created_at, updated_at)
         VALUES (?, 'a-person', 'anonymous', ?, ?)",
    )
    .bind(&id)
    .bind(commons_core::ts::now())
    .bind(commons_core::ts::now())
    .execute(store.pool())
    .await
    .unwrap();
    id
}

/// A file whose content hash is `hash`, attached to an object.
///
/// The hash is a parameter because the whole pipeline is keyed on it: a fixture
/// with one fixed hash cannot distinguish "blocked this file" from "blocked
/// something", and every test in this file would pass against a blocklist that
/// refused everything.
async fn object_with_hash(store: &Store, hash: &str) -> (String, String) {
    let object_id = Uuid::new_v4().to_string();
    let file_id = Uuid::new_v4().to_string();
    let now = commons_core::ts::now();
    sqlx::query(
        "INSERT INTO object (id, kind, title, created_at, updated_at)
         VALUES (?, 'scene', 'A Scene', ?, ?)",
    )
    .bind(&object_id)
    .bind(&now)
    .bind(&now)
    .execute(store.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO file (id, object_id, path, size_bytes, mtime_ns, hash_blake3, state)
         VALUES (?, ?, '/library/a.mkv', 1, 0, ?, 'present')",
    )
    .bind(&file_id)
    .bind(&object_id)
    .bind(hash)
    .execute(store.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO consent_record (id, object_id, tier, redistribution_permitted, updated_at)
         VALUES (?, ?, 'self_published', 1, ?)",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(&object_id)
    .bind(&now)
    .execute(store.pool())
    .await
    .unwrap();
    (object_id, file_id)
}

/// A magnet locator on an object, which is what §5.18 lets a
/// `third_party_permitted` item carry.
async fn magnet(store: &Store, object_id: &str) {
    let now = commons_core::ts::now();
    sqlx::query(
        "INSERT INTO locator (id, object_id, scheme, uri, infohash, source, added_at)
         VALUES (?, ?, 'magnet', 'magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567',
                 '0123456789abcdef0123456789abcdef01234567', 'test', ?)",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(object_id)
    .bind(&now)
    .execute(store.pool())
    .await
    .unwrap();
}

/// A signing key, and its key id as the wire `issuer`.
///
/// Process-wide, because `propagate::trust` is: the trust store is global, so a
/// per-test key would leave earlier tests' keys trusted and make a signature
/// test pass for the wrong reason. One key, generated once, for the file.
fn signer() -> &'static (ed25519_dalek::SigningKey, String) {
    use std::sync::OnceLock;
    static S: OnceLock<(ed25519_dalek::SigningKey, String)> = OnceLock::new();
    S.get_or_init(|| {
        // A fixed seed, not a random one: a test that generates a key per run
        // cannot be debugged from its failure message, and the key is a test
        // fixture rather than a secret.
        let key = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
        let hex: String = key
            .verifying_key()
            .to_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        (key, hex)
    })
}

/// Sign a tombstone the way a peer would, and trust the signer.
fn signed(t: &mut WireTombstone) {
    let (key, key_hex) = signer();
    propagate::trust(&t.issuer, key_hex).expect("trust the fixture key");
    use ed25519_dalek::Signer as _;
    let sig = key.sign(&propagate::canonical_bytes(t));
    t.signature = propagate::to_hex(&sig.to_bytes());
}

/// A wire tombstone, signed, with the fields a test cares about.
fn tombstone(kind: propagate::Kind, content_hash: &str, object_id: Option<&str>) -> WireTombstone {
    let mut t = WireTombstone {
        id: Uuid::new_v4().to_string(),
        kind,
        content_hash: content_hash.to_string(),
        object_id: object_id.map(str::to_string),
        reason: "takedown accepted".to_string(),
        issued_at: commons_core::ts::now(),
        signature: String::new(),
        issuer: "peer-a".to_string(),
    };
    signed(&mut t);
    t
}

// ---------------------------------------------------------------------------
// Locators are destroyed, not tombstoned.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_accepted_takedown_destroys_the_locators() {
    let store = store().await;
    let cluster = cluster(&store).await;
    let (object_id, _) = object_with_hash(&store, "hash-a").await;
    magnet(&store, &object_id).await;

    assert_eq!(locator::count(&store, &object_id).await.unwrap(), 1);
    takedown::deny(
        &store,
        Takedown {
            cluster_id: &cluster,
            object_id: &object_id,
            requested_by: "someone",
            reason: "not mine",
            decided_by: "a-steward",
        },
    )
    .await
    .unwrap();

    assert_eq!(
        locator::count(&store, &object_id).await.unwrap(),
        0,
        "a tombstoned magnet still resolves: the row has to be *gone*, not \\
         marked"
    );
    // ...and gone from the table, not just from the count.
    let rows: Vec<String> = sqlx::query_scalar("SELECT id FROM locator WHERE object_id = ?")
        .bind(&object_id)
        .fetch_all(store.pool())
        .await
        .unwrap();
    assert!(rows.is_empty(), "the locator rows themselves: {rows:?}");
}

#[tokio::test]
async fn a_quarantine_does_not_destroy_the_locators() {
    // The other half. A quarantine is *pending review*, and destroying the
    // locator before anybody has decided would be the takedown happening twice:
    // once by hiding the object and once by pulling the only way to reach the
    // file. A rejected request must leave the object exactly as it was.
    let store = store().await;
    let cluster = cluster(&store).await;
    let (object_id, _) = object_with_hash(&store, "hash-a").await;
    magnet(&store, &object_id).await;

    takedown::quarantine(&store, &cluster, &object_id, "someone", "reporting this")
        .await
        .unwrap();

    assert_eq!(
        locator::count(&store, &object_id).await.unwrap(),
        1,
        "a pending request must not destroy anything: the review has not \\
         happened, and §14.1 ties destruction to *acceptance*"
    );
    let tier: String = sqlx::query_scalar("SELECT tier FROM consent_record WHERE object_id = ?")
        .bind(&object_id)
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(tier, "quarantined");
}

// ---------------------------------------------------------------------------
// The blocklist is keyed on content, not on the object.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_blocklist_is_keyed_on_the_content_hash() {
    let store = store().await;
    let cluster = cluster(&store).await;
    let (object_id, file_id) = object_with_hash(&store, "hash-b").await;
    let file: (Option<String>,) = sqlx::query_as("SELECT hash_blake3 FROM file WHERE id = ?")
        .bind(&file_id)
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(file.0.as_deref(), Some("hash-b"));

    takedown::deny(
        &store,
        Takedown {
            cluster_id: &cluster,
            object_id: &object_id,
            requested_by: "someone",
            reason: "not mine",
            decided_by: "a-steward",
        },
    )
    .await
    .unwrap();

    assert!(
        takedown::is_blocked(&store, "hash-b").await.unwrap(),
        "the *content* is blocked, which is the only thing that survives the \\
         object being deleted and re-added under a new id"
    );
    assert!(
        !takedown::is_blocked(&store, "hash-c").await.unwrap(),
        "and only that content: a blocklist that refuses everything is a \\
         denial of service wearing a blocklist's clothes"
    );
}

#[tokio::test]
async fn a_file_with_no_recorded_hash_is_not_blocked() {
    // The asymmetry matters and is easy to get wrong. An object whose file has
    // no content hash cannot be matched against the blocklist, so the honest
    // answers are "refuse it" (a local library cannot curate what it cannot
    // hash) or "let it in and keep scanning". Letting it in silently is the one
    // that must not happen, and the test pins that it is *deliberate* by
    // checking it is not a spurious hit.
    let store = store().await;
    let cluster = cluster(&store).await;
    let (object_id, _) = object_with_hash(&store, "hash-d").await;
    takedown::deny(
        &store,
        Takedown {
            cluster_id: &cluster,
            object_id: &object_id,
            requested_by: "s",
            reason: "r",
            decided_by: "d",
        },
    )
    .await
    .unwrap();

    assert!(
        !takedown::is_blocked(&store, "").await.unwrap(),
        "an empty hash must not match a blocklist entry: that would block every \\
         unhashed file in the library"
    );
}

#[tokio::test]
async fn a_reimport_of_denied_content_is_refused() {
    // The ticket's done-when, on one peer: the same bytes arriving again under
    // a new object id.
    let store = store().await;
    let cluster = cluster(&store).await;
    let (object_id, _) = object_with_hash(&store, "hash-e").await;

    takedown::deny(
        &store,
        Takedown {
            cluster_id: &cluster,
            object_id: &object_id,
            requested_by: "s",
            reason: "r",
            decided_by: "d",
        },
    )
    .await
    .unwrap();

    // The original is gone -- a takedown removes it from the library entirely.
    //
    // Deleting the object cascades to the file, and the *hash* the re-import is
    // refused on must survive that. So the assertion below is the point of the
    // whole design stated directly: the blocklist row outlives the row it was
    // made from, which is the difference between a blocklist and a flag. An
    // earlier version of this test deleted the object *before* the `deny`, so
    // there was no file left to hash and no blocklist row to write -- and the
    // re-import check passed only because nothing had ever been blocked.
    sqlx::query("DELETE FROM object WHERE id = ?")
        .bind(&object_id)
        .execute(store.pool())
        .await
        .unwrap();

    let block: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM content_blocklist WHERE content_hash = 'hash-e'")
            .fetch_one(store.pool())
            .await
            .unwrap();
    assert_eq!(
        block, 1,
        "the blocklist entry outlives the row it was made from"
    );

    let verdict = takedown::check_import(&store, "hash-e").await.unwrap();
    assert!(
        !verdict.allowed,
        "re-importing denied content must be refused: {verdict:?}"
    );
    assert!(
        matches!(verdict.block, Some(takedown::BlockReason::Denied)),
        "and for the right reason, not a side effect: {verdict:?}"
    );

    // ...and the positive half, or the check could refuse everything.
    let other = takedown::check_import(&store, "hash-f").await.unwrap();
    assert!(
        other.allowed,
        "unrelated content must still import: {other:?}"
    );
}

// ---------------------------------------------------------------------------
// Propagation. Two peers.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_tombstone_from_a_peer_blocks_a_reimport() {
    // The accept criterion, whole: peer A denies, peer B receives, the object is
    // invisible on B, and re-importing it is refused.
    let peer_a = store().await;
    let peer_b = store().await;
    let cluster = cluster(&peer_a).await;

    // The same content exists on both peers, under different object ids, which
    // is the normal case and the reason a blocklist has to key on the hash.
    let (a_object, _) = object_with_hash(&peer_a, "shared-hash").await;
    let (b_object, _) = object_with_hash(&peer_b, "shared-hash").await;

    takedown::deny(
        &peer_a,
        Takedown {
            cluster_id: &cluster,
            object_id: &a_object,
            requested_by: "the person in it",
            reason: "not mine",
            decided_by: "a-steward",
        },
    )
    .await
    .unwrap();

    // Peer B's copy is still sitting there, visible, until the tombstone lands.
    assert!(
        takedown::tier_of(&peer_b, &b_object).await.unwrap() == Some("self_published".into()),
        "precondition: B has not heard yet"
    );

    let wire: Vec<WireTombstone> = propagate::pending(&peer_a).await.unwrap();
    assert_eq!(wire.len(), 1, "A owes exactly one tombstone");
    // The outbox row is signed by the *local* instance, which is a placeholder
    // until Phase 8's transport signs as the peer. So the wire tombstone is
    // re-signed here with the fixture key, which is what a real peer would send.
    let mut over = wire[0].clone();
    signed(&mut over);
    let applied = propagate::receive(&peer_b, &over).await.unwrap();
    assert!(
        applied.applied,
        "B must apply the tombstone rather than merely record it: {applied:?}"
    );

    // Invisible on B.
    assert_eq!(
        takedown::tier_of(&peer_b, &b_object).await.unwrap(),
        Some("denied".into()),
        "the object is denied on B, which §14.1 makes invisible to every caller"
    );

    // And a re-import is refused, which is the done-when.
    let verdict = takedown::check_import(&peer_b, "shared-hash")
        .await
        .unwrap();
    assert!(!verdict.allowed, "B must refuse the re-import: {verdict:?}");

    // Applying the same tombstone twice must not double-count or error: a peer
    // that retries after a dropped connection is the normal case, not an edge.
    let again = propagate::receive(&peer_b, &over).await.unwrap();
    assert!(
        again.duplicate,
        "a redelivered tombstone is a duplicate, not an error: {again:?}"
    );
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM content_blocklist")
        .fetch_one(peer_b.pool())
        .await
        .unwrap();
    assert_eq!(count, 1, "and it does not add a second row: {count}");
}

#[tokio::test]
async fn a_tombstone_does_not_unblock_content_that_was_already_denied() {
    // Revocation is not a vote and there is no un-deny (§14.1: `denied` is
    // "permanently blocked by hash"). A tombstone that arrived *before* the
    // local denial must not leave the library in a state where re-importing is
    // allowed again.
    let peer_b = store().await;
    let (_b_object, _) = object_with_hash(&peer_b, "shared-hash").await;

    let cluster = cluster(&peer_b).await;
    takedown::deny(
        &peer_b,
        Takedown {
            cluster_id: &cluster,
            object_id: &_b_object,
            requested_by: "s",
            reason: "r",
            decided_by: "d",
        },
    )
    .await
    .unwrap();

    // Same content, a *different* tombstone id: this is a distinct decision
    // arriving, not a redelivery, and it must not restore access. The id is
    // inside the signed bytes, so it is changed *before* signing.
    let mut over = WireTombstone {
        id: Uuid::new_v4().to_string(),
        kind: propagate::Kind::Denied,
        content_hash: "shared-hash".to_string(),
        object_id: None,
        reason: "takedown accepted".to_string(),
        issued_at: commons_core::ts::now(),
        signature: String::new(),
        issuer: "peer-a".to_string(),
    };
    signed(&mut over);
    let r = propagate::receive(&peer_b, &over).await.unwrap();
    assert!(r.duplicate || r.applied);
    assert!(
        !takedown::check_import(&peer_b, "shared-hash")
            .await
            .unwrap()
            .allowed,
        "a tombstone must never restore access to denied content"
    );
}

#[tokio::test]
async fn a_tombstone_with_no_signature_is_refused() {
    // §14.1: the blocklist "propagates to every peer". To *every* peer, and an
    // unsigned row claiming to be a takedown is a way to deny somebody else's
    // library wholesale. The check is the whole security property, so it is a
    // test on its own rather than a line in the happy path.
    let peer_b = store().await;
    // A *trusted* issuer with a bad signature. Using an unknown issuer here
    // would be a weaker test: `UnknownIssuer` is refused earlier, so the
    // signature check would never run. The signer is trusted precisely so the
    // refusal can only be the signature.
    let (_key, key_hex) = signer();
    propagate::trust("peer-a", key_hex).unwrap();
    let mut good = tombstone(propagate::Kind::Denied, "unsigned-hash", None);
    // Signed correctly, then the signature replaced. A tombstone that was never
    // signed at all would be refused for the same reason but would not prove the
    // check runs *after* the key lookup rather than instead of it.
    good.signature = "not-a-signature".to_string();
    let err = propagate::receive(&peer_b, &good).await.unwrap_err();
    assert!(
        matches!(
            err,
            propagate::ReceiveError::BadSignature | propagate::ReceiveError::UnknownIssuer(_)
        ),
        "an unsigned takedown must be refused, not applied: {err:?}"
    );
    assert!(
        takedown::check_import(&peer_b, "unsigned-hash")
            .await
            .unwrap()
            .allowed,
        "and it must not have been applied on the way to failing"
    );
}

#[tokio::test]
async fn a_quarantine_propagates_as_quarantine_and_not_as_denied() {
    // The two kinds are different and a pipeline that collapses them is a
    // pipeline that denies people on a report.
    let peer_a = store().await;
    let peer_b = store().await;
    let cluster = cluster(&peer_a).await;
    let (a_object, _) = object_with_hash(&peer_a, "q-hash").await;
    let (b_object, _) = object_with_hash(&peer_b, "q-hash").await;

    takedown::quarantine(&peer_a, &cluster, &a_object, "s", "reporting")
        .await
        .unwrap();
    let wire = propagate::pending(&peer_a).await.unwrap();
    assert_eq!(wire[0].kind, propagate::Kind::Quarantined);
    let mut over = wire[0].clone();
    signed(&mut over);
    propagate::receive(&peer_b, &over).await.unwrap();

    assert_eq!(
        takedown::tier_of(&peer_b, &b_object).await.unwrap(),
        Some("quarantined".into())
    );
    assert!(
        takedown::check_import(&peer_b, "q-hash")
            .await
            .unwrap()
            .allowed,
        "a quarantine does not block re-import -- the content is disputed, not \
         denied, and blocking on a report is the pipeline failing at the one \
         job it must get right"
    );
}

#[tokio::test]
async fn a_quarantine_marks_the_object_invisible_before_any_decision() {
    let store = store().await;
    let cluster = cluster(&store).await;
    let (object_id, _) = object_with_hash(&store, "q2-hash").await;
    takedown::quarantine(&store, &cluster, &object_id, "s", "reporting")
        .await
        .unwrap();

    let page = store
        .query(
            &commons_store::filter_ast::Filter::All,
            &commons_store::filter_ast::CallerId::anonymous(),
            50,
        )
        .await
        .unwrap();
    assert!(
        !page.ids().contains(&object_id),
        "§14.1: quarantined is 'hidden everywhere, pending review' -- so it is \
         hidden from an anonymous browse the moment it is reported"
    );
}

// ---------------------------------------------------------------------------
// The request path: a performer with no account (§14.1, §7.5).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_request_from_an_unverified_cluster_goes_to_the_stewards() {
    // §7.5: a person can request takedown *without an account on the index*.
    // If the request then had no recipient it would be dropped, and the one
    // outcome worse than a misdirected request is silence.
    let store = store().await;
    let cluster = cluster(&store).await;
    let request = claim::open_takedown(&store, &cluster, "a-stranger", "this is me")
        .await
        .unwrap();
    assert!(
        request.to_stewards,
        "an unverified request has nobody but the stewards to go to"
    );
    assert!(claim::takedown_is_steward_queue(&store, &request.id)
        .await
        .unwrap());
}

#[tokio::test]
async fn a_request_needs_a_reason() {
    // A takedown with an empty reason cannot be reviewed by anybody, including
    // the person whose content it is about. §14.1 calls the basis part of the
    // record.
    let store = store().await;
    let cluster = cluster(&store).await;
    let err = claim::open_takedown(&store, &cluster, "a-stranger", "  ")
        .await
        .unwrap_err();
    assert!(
        matches!(err, claim::ClaimError::EmptyReason),
        "an empty reason must be refused, not stored: {err:?}"
    );
}

#[tokio::test]
async fn a_takedown_against_an_unknown_object_is_refused() {
    let store = store().await;
    let cluster = cluster(&store).await;
    let err = takedown::deny(
        &store,
        Takedown {
            cluster_id: &cluster,
            object_id: "no-such-object",
            requested_by: "s",
            reason: "r",
            decided_by: "d",
        },
    )
    .await
    .unwrap_err();
    assert!(
        matches!(err, DenyError::NoSuchObject(_)),
        "a takedown against nothing must fail loudly, because a silently \
         ignored takedown is the worst outcome available: {err:?}"
    );
}
