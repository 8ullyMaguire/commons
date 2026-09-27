//! Share grants against a real database, on both engines.
//!
//! T-P5-007 part 2B, spec §9.5 (#5612). Companion to the policy tests in
//! `commons-consent`, which test *what a valid grant is*. This file tests the
//! **schema**, and the split matters: a correct `resolve` cannot prove the table
//! exists, that the CHECK constraints are installed, or that a write bypassing
//! the policy layer is still stopped.
//!
//! A migration that silently fails to create its constraint passes every other
//! test in the repository — the Rust side would refuse the same values on its
//! own, so the suite would be green with a database that accepts anything. That
//! is the failure this file exists to catch, and the tests are written
//! deliberately to bypass the Rust layer so it can happen.
//!
//! # Per my own memory note: a fixed id passed once and died in the full suite
//!
//! Every row here is created per-test with a distinct id derived from a UUID,
//! never a name or a fixed literal. A fixed id passes exactly once — the first
//! run — and then dies on `share_grant_pkey` against a database that persists
//! between runs. A name-based id collides when two tests pick the same name.
//! Both of those have happened in this repository; see the fixture-id audit.

#[path = "harness/mod.rs"]
mod harness;
use harness::{postgres_store, sqlite_store};

use chrono::{DateTime, Duration, Utc};
use commons_store::share::{
    get_grant_by_id, insert_grant, list_access, list_grants, list_grants_for_target, record_access,
    record_granted, revoke_grant, ShareAccessRow, ShareGrantRow,
};
use uuid::Uuid;

/// Run a block against both engines. See `folders_db.rs` for why this is a
/// macro: `sqlx`'s two result types are unrelated Rust types.
macro_rules! both_engines {
    (|$s:ident| $body:block) => {{
        async {
            let $s = postgres_store().await;
            $body
        }
        .await;
        async {
            let $s = sqlite_store().await;
            $body
        }
        .await;
    }};
}

/// A grant with a UNIQUE hash.
///
/// The hash has to be unique per row because `token_hash` is UNIQUE and the
/// database persists between runs: a fixed `vec![7u8; 32]` passes once and then
/// dies on `share_grant_token_hash_key` against the second test that uses it.
/// This is the same class of bug as a fixed row id, and it is the reason
/// `token_hash` is derived from the id rather than fixed.
fn grant(id: &str) -> ShareGrantRow {
    // Hash the id so the value is unique per row AND deterministic, which keeps
    // the round-trip assertion able to compare against what it wrote.
    let mut hash = [0u8; 32];
    for (i, b) in id.as_bytes().iter().take(32).enumerate() {
        hash[i] = *b;
    }
    let mut g = ShareGrantRow::new(id, hash.to_vec(), Utc::now() + Duration::hours(1));
    g.created_at = Some(Utc::now());
    g
}

/// Compare at the resolution the schema stores.
///
/// Timestamps are ISO-8601 UTC TEXT with **millisecond** precision (plan §0.4),
/// so a `DateTime` carrying nanoseconds cannot survive a round trip exactly. The
/// test compares truncated to millis, because asserting equality at nanosecond
/// resolution would be asserting that the column stores more than it does.
fn same_instant(a: Option<DateTime<Utc>>, b: Option<DateTime<Utc>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => {
            let t = |d: DateTime<Utc>| d.timestamp_millis();
            t(a) == t(b)
        }
        (None, None) => true,
        _ => false,
    }
}

fn access(id: &str, grant_id: &str, granted: bool, reason: Option<&str>) -> ShareAccessRow {
    ShareAccessRow {
        id: id.to_string(),
        grant_id: grant_id.to_string(),
        at: Utc::now(),
        ip: Some("127.0.0.1".to_string()),
        user_agent: Some("test".to_string()),
        granted,
        denied_reason: reason.map(str::to_string),
    }
}

#[tokio::test]
async fn a_grant_round_trips_on_both_engines() {
    both_engines!(|s| {
        let id = format!("g-{}", Uuid::new_v4());
        let mut g = grant(&id);
        g.target_id = format!("obj-{}", Uuid::new_v4());
        insert_grant(&s, &g).await.expect("insert");
        let back = get_grant_by_id(&s, &id)
            .await
            .expect("get")
            .expect("present");
        assert_eq!(back.id, id);
        assert_eq!(back.token_hash, g.token_hash, "token_hash must round trip");
        assert_eq!(back.scope, "view");
        assert_eq!(back.access_count, 0);
        // The timestamps survived the TEXT round trip to the millisecond, on
        // both engines. This is the assertion that catches a
        // chrono-vs-TEXT mismatch: it fails on the read, never on the write.
        assert!(
            same_instant(back.created_at, g.created_at),
            "created_at must round trip to the millisecond"
        );
        assert!(
            same_instant(back.expires_at, g.expires_at),
            "expires_at must round trip to the millisecond"
        );
        assert_eq!(back.revoked_at, None);
    });
}

#[tokio::test]
async fn a_missing_grant_is_none_not_an_error() {
    both_engines!(|s| {
        let got = get_grant_by_id(&s, &format!("nope-{}", Uuid::new_v4()))
            .await
            .expect("query");
        assert_eq!(got, None);
    });
}

#[tokio::test]
async fn the_scope_check_fires_without_rust_validation() {
    // The load-bearing test in this file. `scope` has a CHECK; the Rust `Scope`
    // enum has no string that could fail it. A write that bypasses the policy
    // layer -- a future admin route, a migration, a bug -- must be stopped by
    // the DATABASE, or the enum is the only thing standing between a caller and
    // a link that can download.
    both_engines!(|s| {
        let id = format!("g-{}", Uuid::new_v4());
        let mut g = grant(&id);
        g.scope = "download".to_string(); // not a Scope
        let err = insert_grant(&s, &g).await;
        assert!(err.is_err(), "the CHECK must refuse an invalid scope");
    });
}

#[tokio::test]
async fn the_target_kind_check_fires() {
    both_engines!(|s| {
        let mut g = grant(&format!("g-{}", Uuid::new_v4()));
        g.target_kind = "everything".to_string();
        assert!(insert_grant(&s, &g).await.is_err());
    });
}

#[tokio::test]
async fn the_expiry_check_fires() {
    // `expires_at > created_at`. A grant that expires before it was created is
    // a bug, and the CHECK catches it once rather than at every read site.
    both_engines!(|s| {
        let mut g = grant(&format!("g-{}", Uuid::new_v4()));
        g.expires_at = Some(Utc::now() - Duration::hours(1));
        g.created_at = Some(Utc::now());
        assert!(insert_grant(&s, &g).await.is_err());
    });
}

#[tokio::test]
async fn a_denied_access_without_a_reason_is_refused() {
    // A log of "denied" with no reason is a count, not a log. The reason is the
    // entire value of recording a failure.
    both_engines!(|s| {
        let id = format!("g-{}", Uuid::new_v4());
        insert_grant(&s, &grant(&id)).await.expect("insert");
        let bad = access(&format!("a-{}", Uuid::new_v4()), &id, false, None);
        assert!(
            record_access(&s, &bad).await.is_err(),
            "a denied row with no reason must be refused"
        );
    });
}

#[tokio::test]
async fn a_granted_access_may_have_no_reason() {
    both_engines!(|s| {
        let id = format!("g-{}", Uuid::new_v4());
        insert_grant(&s, &grant(&id)).await.expect("insert");
        let ok = access(&format!("a-{}", Uuid::new_v4()), &id, true, None);
        record_access(&s, &ok)
            .await
            .expect("a granted row needs no reason");
        let log = list_access(&s, &id).await.expect("log");
        assert_eq!(log.len(), 1);
        assert!(log[0].granted);
        assert_eq!(log[0].denied_reason, None);
    });
}

#[tokio::test]
async fn revoke_is_idempotent_and_keeps_the_first_timestamp() {
    // The timestamp is the only record of WHEN the owner killed the link, so a
    // second revoke must not move it. An impatient double-click rewriting it to
    // a time the owner did not act at is a small lie in the audit trail.
    both_engines!(|s| {
        let id = format!("g-{}", Uuid::new_v4());
        insert_grant(&s, &grant(&id)).await.expect("insert");
        let first = Utc::now();
        assert!(revoke_grant(&s, &id, first).await.expect("revoke"));
        let later = Utc::now() + Duration::hours(1);
        assert!(
            !revoke_grant(&s, &id, later).await.expect("second revoke"),
            "a second revoke must report that it changed nothing"
        );
        let row = get_grant_by_id(&s, &id)
            .await
            .expect("get")
            .expect("present");
        assert!(
            same_instant(row.revoked_at, Some(first)),
            "the first timestamp stands"
        );
    });
}

#[tokio::test]
async fn a_revoked_grant_is_still_readable() {
    // get_grant_by_id does NOT filter. The policy layer needs the revoked row to
    // report `Revoked` rather than `UnknownToken`, and the owner's page needs to
    // show a link they killed. A filtered query makes all three worse.
    both_engines!(|s| {
        let id = format!("g-{}", Uuid::new_v4());
        insert_grant(&s, &grant(&id)).await.expect("insert");
        revoke_grant(&s, &id, Utc::now()).await.expect("revoke");
        let row = get_grant_by_id(&s, &id)
            .await
            .expect("get")
            .expect("still present");
        assert!(row.revoked_at.is_some());
    });
}

#[tokio::test]
async fn the_counter_increments_and_the_timestamp_lands() {
    both_engines!(|s| {
        let id = format!("g-{}", Uuid::new_v4());
        insert_grant(&s, &grant(&id)).await.expect("insert");
        let mut row = get_grant_by_id(&s, &id)
            .await
            .expect("get")
            .expect("present");
        for expected in 1..=3i64 {
            record_granted(&s, &row, Utc::now()).await.expect("bump");
            row = get_grant_by_id(&s, &id)
                .await
                .expect("get")
                .expect("present");
            assert_eq!(row.access_count, expected);
            assert!(row.last_accessed_at.is_some());
        }
    });
}

#[tokio::test]
async fn the_access_log_records_denials_with_their_reason() {
    // The property the whole log exists for: a question about who tried a link
    // lives entirely in the failures.
    both_engines!(|s| {
        let id = format!("g-{}", Uuid::new_v4());
        insert_grant(&s, &grant(&id)).await.expect("insert");
        record_access(
            &s,
            &access(&format!("a1-{}", Uuid::new_v4()), &id, true, None),
        )
        .await
        .expect("granted");
        for reason in ["expired", "revoked", "wrong_password"] {
            record_access(
                &s,
                &access(&format!("a-{}", Uuid::new_v4()), &id, false, Some(reason)),
            )
            .await
            .expect("denied");
        }
        let log = list_access(&s, &id).await.expect("log");
        assert_eq!(log.len(), 4);
        let reasons: Vec<Option<&str>> = log.iter().map(|r| r.denied_reason.as_deref()).collect();
        for want in ["expired", "revoked", "wrong_password"] {
            assert!(
                reasons.contains(&Some(want)),
                "missing {want} in {reasons:?}"
            );
        }
    });
}

#[tokio::test]
async fn the_owner_can_list_their_links_including_dead_ones() {
    both_engines!(|s| {
        // A shared target so the list is not empty and the filter is exercised.
        let target = format!("obj-{}", Uuid::new_v4());
        let mut live = grant(&format!("g-{}", Uuid::new_v4()));
        live.target_id = target.clone();
        let mut dead = grant(&format!("g-{}", Uuid::new_v4()));
        dead.target_id = target.clone();
        insert_grant(&s, &live).await.expect("insert");
        insert_grant(&s, &dead).await.expect("insert");
        revoke_grant(&s, &dead.id, Utc::now())
            .await
            .expect("revoke");

        let for_target = list_grants_for_target(&s, "object", &target)
            .await
            .expect("list");
        assert_eq!(for_target.len(), 2, "a dead link is still the owner's");
        assert!(for_target.iter().any(|g| g.revoked_at.is_some()));

        let all = list_grants(&s).await.expect("list all");
        assert!(all.len() >= 2);
        // Newest first.
        let times: Vec<_> = all.iter().filter_map(|g| g.created_at).collect();
        let mut sorted = times.clone();
        sorted.sort_by(|a, b| b.cmp(a));
        assert_eq!(times, sorted, "list_grants must be newest first");
    });
}

#[tokio::test]
async fn grants_for_another_target_are_not_returned() {
    both_engines!(|s| {
        let mine = format!("obj-{}", Uuid::new_v4());
        let theirs = format!("obj-{}", Uuid::new_v4());
        let mut g = grant(&format!("g-{}", Uuid::new_v4()));
        g.target_id = theirs.clone();
        insert_grant(&s, &g).await.expect("insert");
        let got = list_grants_for_target(&s, "object", &mine)
            .await
            .expect("list");
        assert!(got.is_empty(), "another target's links must not leak");
    });
}
