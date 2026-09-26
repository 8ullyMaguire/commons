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

/// Set one field's reputation for an account, leaving the rest alone.
///
/// Per-field reputation is a JSON map on the account row rather than a table
/// because it is read on every vote of every field and written rarely. The map
/// is read-merge-write, so calling this for a second field keeps the first.
pub async fn set_field_reputation(store: &Store, account: &Uuid, field: &str, value: f64) {
    let existing: Option<String> =
        sqlx::query_scalar("SELECT field_reputation FROM account WHERE id = ?")
            .bind(account.to_string())
            .fetch_one(store.pool())
            .await
            .unwrap();
    let mut map: serde_json::Map<String, serde_json::Value> = existing
        .and_then(|j| serde_json::from_str(&j).ok())
        .unwrap_or_default();
    map.insert(field.to_string(), serde_json::json!(value));
    let n = sqlx::query("UPDATE account SET field_reputation = ? WHERE id = ?")
        .bind(serde_json::Value::Object(map).to_string())
        .bind(account.to_string())
        .execute(store.pool())
        .await
        .unwrap()
        .rows_affected();
    assert_eq!(n, 1, "set_field_reputation updated {n} rows, expected 1");
}
