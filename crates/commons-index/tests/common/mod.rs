//! Fixtures for the curation tests.
//!
//! Two things here earn their existence rather than being a convenience.
//!
//! **The account fixture takes a reputation.** Weight is the product of
//! reputation and decay, so a fixture that hardcodes weight 1.0 could only ever
//! test the decay half. The ticket's own acceptance criterion is a 5/3/1 split,
//! and that split has to come from somewhere.
//!
//! **The account fixture is one row, not one account.** §8.3's sybil damping
//! and per-field independence are both about what happens when there are
//! *several* accounts, and a fixture with a single account makes "this
//! account's votes" and "every vote" the same result — so a filter that ignores
//! which account is voting passes. Every test that asserts a filter builds at
//! least two.

#![allow(dead_code)] // Each test binary uses a subset.

use commons_core::Role;
use commons_store::Store;
use uuid::Uuid;

/// An isolated store, migrated.
pub async fn store() -> (tempfile::TempDir, Store) {
    let d = tempfile::tempdir().unwrap();
    let store = Store::open_library(d.path()).await.unwrap();
    (d, store)
}

/// An account with a given reputation, as a subject of a proposal.
///
/// The subject is deliberately a *fresh* uuid each call and is not registered
/// as an `object` row: `field_proposal.subject_id` is polymorphic and carries no
/// foreign key (a studio name votes by the same machinery as a scene title),
/// so resolve never joins to the subject. That is worth knowing when reading the
/// resolve tests -- nothing about them depends on the subject existing.
pub async fn account(store: &Store, handle: &str, role: Role, reputation: f64) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO account (id, handle, role, reputation, disabled, field_reputation, created_at)
         VALUES (?, ?, ?, ?, 0, NULL, ?)",
    )
    .bind(id.to_string())
    .bind(handle)
    .bind(role.as_str())
    .bind(reputation)
    .bind(commons_core::ts::now())
    .execute(store.pool())
    .await
    .unwrap();
    id
}

/// An account whose reputation on `field` is `reputation`.
///
/// **This is the fixture the voting tests need**, and the distinction from
/// `account` above is not cosmetic. `account.reputation` is the account's
/// standing overall; `account.field_reputation` is its standing on one field,
/// and §8.3 is explicit that a vote weighs what the voter is worth *on the field
/// being voted on*. A fixture that set only the first would produce a resolve
/// that ignores per-field reputation while every test still passed, because a
/// single-field library never notices the difference.
pub async fn account_on_field(
    store: &Store,
    handle: &str,
    role: Role,
    field: &str,
    reputation: f64,
) -> Uuid {
    let id = account(store, handle, role, reputation).await;
    set_field_reputation(store, &id, field, reputation).await;
    id
}

/// Give an account a reputation on one field, by writing reputation *events*.
///
/// This used to write the `field_reputation` column, and that stopped being
/// true the moment `cast_vote` began recomputing from the event log: the column
/// is a cache of the log, so anything a fixture puts in it is overwritten by
/// the account's next vote. Every `resolve` test that set a weight here kept
/// passing its own assertions and failed on weight — a test that cannot fail on
/// the thing it is about.
///
/// So a value inside the curve's range is written as events, and the count is
/// derived by *inverting* `weight_from`:
///   agreements:  net = n,                    n = e^((v-base)/gain) - 1
///   disputes:     pen = 1 - e^((v-base)/gain), and pen = P·d/(1+F·d), so
///                 d = pen / (P - F·pen)
/// Inverting rather than searching, because a search round-trips per candidate
/// and the first version looped to 400 — four hundred round-trips inside a
/// fixture, which made the suite time out rather than fail. A fixture that can
/// hang is a fixture that will.
///
/// A value *outside* the range is not a reputation at all. The curve is bounded
/// by `min_weight` and `max_weight`, so no number of agreements reaches 5.0;
/// asking for one and looping is what hung the suite. Such a value is a raw vote
/// weight — a number frozen at cast time, which is what a test that wants a
/// heavy voter is actually testing — so it goes into the cache and no events
/// are written, and it is recorded in `raw_weight_fixtures` so a reader can see
/// it was deliberate rather than a value that quietly got clamped.
pub async fn set_field_reputation(store: &Store, account: &Uuid, field: &str, value: f64) {
    let config = commons_index::reputation::ReputationConfig::default();
    let in_range = (config.min_weight..=config.max_weight).contains(&value);
    if !in_range {
        RAW_WEIGHT_FIXTURES
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((*account, field.to_string(), value));
        write_cache(store, account, field, value).await;
        return;
    }

    let agreements = if value > config.base_weight {
        (((value - config.base_weight) / config.agreement_gain).exp() - 1.0)
            .round()
            .max(1.0) as i64
    } else {
        0
    };
    let disputes = if value <= config.base_weight {
        let pen = 1.0 - ((value - config.base_weight) / config.agreement_gain).exp();
        let denom = config.dispute_penalty - config.dispute_forgiveness * pen;
        if denom <= 0.0 {
            0
        } else {
            (pen / denom).round().max(0.0) as i64
        }
    } else {
        0
    };

    for (kind, n) in [
        (commons_index::reputation::EventKind::Agreed, agreements),
        (commons_index::reputation::EventKind::Disputed, disputes),
    ] {
        for _ in 0..n {
            commons_index::reputation::record_event(
                store,
                account,
                field,
                kind,
                Some(&Uuid::new_v4()),
                if kind == commons_index::reputation::EventKind::Agreed {
                    config.agreement_gain
                } else {
                    -config.dispute_penalty
                },
                1.0,
            )
            .await
            .expect("record reputation event");
        }
    }

    // And the cache, so a read that does not go through the event log still sees
    // it. Written through one function so the two cannot disagree about format.
    write_cache(store, account, field, value).await;

    let got = commons_index::reputation::weight(store, account, field)
        .await
        .expect("read back the weight");
    assert!(
        (got - value).abs() < 0.1,
        "set_field_reputation asked for {value} on {field} and the event log \
         gives {got}: the fixture and the curve disagree"
    );
}

async fn write_cache(store: &Store, account: &Uuid, field: &str, value: f64) {
    let existing: Option<String> =
        sqlx::query_scalar("SELECT field_reputation FROM account WHERE id = ?")
            .bind(account.to_string())
            .fetch_optional(store.pool())
            .await
            .unwrap();
    let mut map: serde_json::Map<String, serde_json::Value> = existing
        .and_then(|j| serde_json::from_str(&j).ok())
        .unwrap_or_default();
    map.insert(field.to_string(), serde_json::json!(value));
    let n = sqlx::query("UPDATE account SET reputation = ?, field_reputation = ? WHERE id = ?")
        .bind(value)
        .bind(serde_json::Value::Object(map).to_string())
        .bind(account.to_string())
        .execute(store.pool())
        .await
        .unwrap()
        .rows_affected();
    assert_eq!(n, 1, "write_cache updated {n} rows, expected 1");
}

static RAW_WEIGHT_FIXTURES: std::sync::Mutex<Vec<(Uuid, String, f64)>> =
    std::sync::Mutex::new(Vec::new());

/// Every out-of-range weight a fixture has been asked for, so a test can assert
/// it meant what it did rather than what it got.
pub fn raw_weight_fixtures() -> Vec<(Uuid, String, f64)> {
    RAW_WEIGHT_FIXTURES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// An account with **no** reputation anywhere: `reputation` 1.0 and
/// `field_reputation` NULL.
///
/// The two fixtures above exist to make a *vote's* frozen weight a chosen
/// number, which is what `resolve` needs. This one exists for the other
/// direction: a test that is about reputation itself needs an account whose
/// reputation it did not set, or "a new account starts at the base" cannot be
/// tested — the fixture would have put it somewhere already. Note what it does
/// *not* do: it does not write a `field_reputation` key, because an empty map
/// and a NULL map are the same thing to every reader here and a fixture that
/// invented one would be asserting the thing under test.
pub async fn bare_account(store: &Store, handle: &str, role: Role) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO account (id, handle, role, reputation, disabled, created_at)
         VALUES (?, ?, ?, 1.0, 0, ?)",
    )
    .bind(id.to_string())
    .bind(handle)
    .bind(role.as_str())
    .bind(commons_core::ts::now())
    .execute(store.pool())
    .await
    .expect("insert bare account");
    id
}

/// Cast a vote and then freeze a specific weight onto it.
///
/// `vote.weight` is *the weight at cast time* — that is what the column is
/// documented to mean, and it is why a reputation change does not rewrite what
/// somebody said they believed when they said it. A test that wants a vote
/// weighing 6.0 when the cap is 4.0 is not describing an account's reputation;
/// it is describing a ballot. Writing it after the cast says exactly that, and
/// says it in the one place where a frozen weight can still be set.
///
/// The alternative — an account fixture whose reputation is out of range —
/// cannot work, because `cast_vote` recomputes the weight from the event log and
/// the log's curve is bounded. The three `resolve` tests that wanted this hung
/// for four minutes before they failed.
pub async fn vote_with_weight(store: &Store, by: &Uuid, proposal: &Uuid, field: &str, weight: f64) {
    commons_index::resolve::cast_vote(store, by, proposal, field)
        .await
        .unwrap();
    let n = sqlx::query("UPDATE vote SET weight = ? WHERE account_id = ? AND proposal_id = ?")
        .bind(weight)
        .bind(by.to_string())
        .bind(proposal.to_string())
        .execute(store.pool())
        .await
        .unwrap()
        .rows_affected();
    assert_eq!(n, 1, "vote_with_weight froze {n} votes, expected 1");
}
