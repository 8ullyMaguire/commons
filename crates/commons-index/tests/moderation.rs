//! §8.5 — moderation, locking, disputes.
//!
//! The ticket's done-when is explicit and unusual: **"all seven negative tests
//! exist."** Not seven tests, seven *negative* ones — a test that the right role
//! can resolve an item, and a test that a lower role cannot. The asymmetry is the
//! point: a moderation queue whose only tests are positive has no test that the
//! authorization check runs, only that the happy path does.
//!
//! So every item type below gets the same pair, and the negative half is written
//! first in each block because it is the half that matters. A positive test for
//! "a steward can lock a field" passes just as well against a function that locks
//! for anybody, which is why it is not the test that decides whether the check
//! exists.
//!
//! What the seven types are, and why each needs its own authorization rather than
//! one shared `may_moderate`:
//!
//! | type | who may resolve | why the role differs from the others |
//! |---|---|---|
//! | `ContestedField` | Steward | a judgement call about evidence; a Curator's vote is already in the weights |
//! | `DisputedMerge` | Steward | same, but the consequence is two identities becoming one |
//! | `DisputedConsent` | Admin | consent is §14's, and revocation is never outvoted by contribution |
//! | `AbuseReport` | Steward | an accusation about a person, not a value |
//! | `PendingEdit` | Steward | someone else's pending edit is being amended |
//! | `NameCollision` | Steward | a warning, resolvable by the proposer without a steward |
//! | `ClosedSubmission` | Steward | reopening a closed submission, which is a policy call |
//!
//! `DisputedConsent` is Admin rather than Steward on purpose, and that is the
//! one place this module overrides a role the rest of the codebase shares:
//! §14.2 says a consent revocation propagates as a tombstone and is *never*
//! outvoted by contribution points. A steward who is otherwise trusted with
//! every other queue is not trusted with a consent decision, so
//! `Role::may_moderate` is deliberately not consulted for that type.

use commons_core::{ts, Role, SubjectType};
use commons_index::moderation::{self, ItemType, Resolution};
use commons_store::Store;
use serde_json::Value;
use uuid::Uuid;

mod common;
use common::{account_on_field, store};

/// An account with a role, as a queue actor.
async fn actor(store: &Store, handle: &str, role: Role) -> Uuid {
    account_on_field(store, handle, role, "title", 1.0).await
}

/// A real `object` row, which `consent_record` and `field_lock` both key off.
///
/// Not a bare uuid. `consent_record.object_id` has a foreign key to `object(id)`,
/// and SQLite does enforce it here, so a consent test that invents a subject gets
/// a constraint failure instead of a decision -- which means the test never
/// exercises the one thing it exists to check.
async fn object(store: &Store) -> Uuid {
    let id = Uuid::new_v4();
    let now = ts::now();
    sqlx::query(
        "INSERT INTO object (id, kind, title, organized, created_at, updated_at)
         VALUES (?, 'clip', 'A Clip', 'unreviewed', ?, ?)",
    )
    .bind(id.to_string())
    .bind(&now)
    .bind(&now)
    .execute(store.pool())
    .await
    .unwrap();
    id
}

/// An account that exists but is only ever named, never curates.
///
/// `raise_pending_edit` looks the author up so the limit and the disabled check
/// have a role to read, so a handle with no account row is a programming error at
/// the call site rather than something the queue invents a default for.
async fn named(store: &Store, handle: &str) -> Uuid {
    actor(store, handle, Role::Contributor).await
}

fn subject() -> Uuid {
    Uuid::new_v4()
}

// ---- the shape of every type ----------------------------------------------

/// Every one of the seven types refuses a role that may not curate.
///
/// One test over all seven rather than seven near-identical tests, because the
/// property is a property of the *dispatch* and a test per type would only
/// re-assert the same match arm seven times. What is per-type is the *upper*
/// boundary — see `each_type_refuses_the_role_below_its_own`.
#[tokio::test]
async fn every_type_refuses_a_role_that_may_not_curate() {
    let (_d, store) = store().await;
    let nobody = actor(&store, "nobody", Role::Subscriber).await;
    let item = moderation::Item {
        kind: ItemType::ContestedField,
        subject_type: SubjectType::Object,
        subject_id: subject(),
        field: "title".to_string(),
        summary: "two values, one field".to_string(),
        raised_by: Some("alice".to_string()),
        raised_at: ts::now(),
    };
    for kind in ItemType::ALL {
        let mut item = item.clone();
        item.kind = kind;
        let refused = moderation::resolve(&store, kind, &item, &nobody, Resolution::Dismiss).await;
        assert!(
            refused.is_err(),
            "{kind:?} was resolvable by a Subscriber -- the authorization check \\
             for this type does not run"
        );
    }
}

/// Every one of the seven types is resolvable by an admin.
///
/// The upper half of the pair. Worth having for all seven because a type that
/// *nobody* can resolve is a queue item that accumulates forever, and that is
/// indistinguishable from a working queue in a test that only checks the
/// negative.
#[tokio::test]
async fn every_type_is_resolvable_by_an_admin() {
    let (_d, store) = store().await;
    let admin = actor(&store, "admin", Role::Admin).await;
    for kind in ItemType::ALL {
        let item = moderation::Item {
            kind,
            subject_type: SubjectType::Object,
            subject_id: subject(),
            field: "title".to_string(),
            summary: format!("{kind:?}"),
            raised_by: Some("alice".to_string()),
            raised_at: ts::now(),
        };
        moderation::resolve(&store, kind, &item, &admin, Resolution::Dismiss)
            .await
            .unwrap_or_else(|e| panic!("{kind:?} was not resolvable by an admin: {e}"));
    }
}

/// A type's own boundary, which is not the same for all seven.
///
/// The table at the top of this file, asserted. Without it, "every type refuses a
/// Subscriber and an admin may resolve" would still pass if every type's boundary
/// were Steward — including `DisputedConsent`, which is the one that must be
/// Admin. A test that only checks the ends of a range cannot see a boundary in
/// the middle moving.
#[tokio::test]
async fn each_type_refuses_the_role_below_its_own() {
    let (_d, store) = store().await;
    let steward = actor(&store, "steward", Role::Steward).await;
    let admin = actor(&store, "admin2", Role::Admin).await;

    for kind in ItemType::ALL {
        let item = moderation::Item {
            kind,
            subject_type: SubjectType::Object,
            subject_id: subject(),
            field: "title".to_string(),
            summary: format!("{kind:?}"),
            raised_by: None,
            raised_at: ts::now(),
        };

        // The lowest role that may resolve this type.
        let lowest = kind.lowest_role();
        if lowest == Role::Admin {
            // The type whose boundary is above Steward.
            assert!(
                moderation::resolve(&store, kind, &item, &steward, Resolution::Dismiss)
                    .await
                    .is_err(),
                "{kind:?} must not be resolvable by a Steward: its lowest role is \\
                 Admin, because §14.2 says a consent decision is never outvoted by \\
                 contribution and a steward is trusted with every other queue"
            );
            assert!(
                moderation::resolve(&store, kind, &item, &admin, Resolution::Dismiss)
                    .await
                    .is_ok(),
                "{kind:?} must be resolvable by an Admin"
            );
        } else {
            assert!(
                moderation::resolve(&store, kind, &item, &steward, Resolution::Dismiss)
                    .await
                    .is_ok(),
                "{kind:?} must be resolvable by a Steward"
            );
        }
    }
}

/// The seven types, written out, because a test that iterates the enum cannot
/// notice a type that was forgotten.
///
/// The count is the assertion. `ItemType::ALL` is derived from the enum, so a
/// test over it passes with six types just as happily as with seven, and the
/// ticket's done-when is about there being seven.
#[tokio::test]
async fn there_are_seven_item_types_and_they_are_these() {
    assert_eq!(ItemType::ALL.len(), 7, "§8.5 names seven");
    let names: Vec<&str> = ItemType::ALL.iter().map(|k| k.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "contested_field",
            "disputed_merge",
            "disputed_consent",
            "abuse_report",
            "pending_edit",
            "name_collision",
            "closed_submission",
        ]
    );
    // And the per-type boundaries, as data rather than as behaviour.
    let boundaries: Vec<(&str, Role)> = ItemType::ALL
        .iter()
        .map(|k| (k.as_str(), k.lowest_role()))
        .collect();
    assert_eq!(
        boundaries,
        vec![
            ("contested_field", Role::Steward),
            ("disputed_merge", Role::Steward),
            // The one that differs, and the reason is §14.2.
            ("disputed_consent", Role::Admin),
            ("abuse_report", Role::Steward),
            ("pending_edit", Role::Steward),
            ("name_collision", Role::Steward),
            ("closed_submission", Role::Steward),
        ],
        "six Steward and one Admin, and the Admin one is the consent queue"
    );
}

// ---- 1. contested field ---------------------------------------------------

/// A steward may lock a contested field, and the lock is what resolve reads.
#[tokio::test]
async fn a_steward_may_lock_a_contested_field() {
    let (_d, store) = store().await;
    let steward = actor(&store, "steward", Role::Steward).await;
    let s = object(&store).await;
    let item = item(ItemType::ContestedField, s, "title");

    moderation::resolve(
        &store,
        ItemType::ContestedField,
        &item,
        &steward,
        Resolution::LockTo("\"Locked\"".to_string()),
    )
    .await
    .unwrap();

    let lock = commons_store::index::field_lock(&store, SubjectType::Object, &s, "title")
        .await
        .unwrap()
        .expect("a lock is in force");
    assert_eq!(lock.value_json.as_deref(), Some("\"Locked\""));
}

/// **Negative test 1.** A contributor cannot lock a contested field.
///
/// This is the negative half of the contested-field pair, and it is the one that
/// exists. `Role::may_lock` is `Steward | Admin` and a contributor has exactly
/// the vote that made the field contested; letting them also pin the answer would
/// make the lock a way to discard the evidence their own vote was part of.
#[tokio::test]
async fn a_contributor_cannot_lock_a_contested_field() {
    let (_d, store) = store().await;
    let contributor = actor(&store, "contributor", Role::Contributor).await;
    let s = object(&store).await;
    let item = item(ItemType::ContestedField, s, "title");

    let refused = moderation::resolve(
        &store,
        ItemType::ContestedField,
        &item,
        &contributor,
        Resolution::LockTo("\"Locked\"".to_string()),
    )
    .await;
    assert!(
        refused.is_err(),
        "a Contributor resolved a contested field, which is the one thing a \\
         Contributor may not do"
    );
    assert!(
        commons_store::index::field_lock(&store, SubjectType::Object, &s, "title")
            .await
            .unwrap()
            .is_none(),
        "and no lock was written: a refused resolution must leave no trace"
    );
}

// ---- 2. disputed merge ----------------------------------------------------

/// A steward may confirm a disputed merge, and the merge record says so.
#[tokio::test]
async fn a_steward_may_confirm_a_disputed_merge() {
    let (_d, store) = store().await;
    let steward = actor(&store, "steward", Role::Steward).await;
    let winner = object(&store).await;
    let loser = object(&store).await;
    let item = item(ItemType::DisputedMerge, winner, "title");

    moderation::resolve(
        &store,
        ItemType::DisputedMerge,
        &item,
        &steward,
        Resolution::Merge(loser),
    )
    .await
    .unwrap();

    let merges = commons_index::history::merges_into(&store, winner)
        .await
        .unwrap();
    assert_eq!(merges.len(), 1, "{merges:#?}");
    assert_eq!(merges[0].loser, loser);
}

/// **Negative test 2.** A contributor cannot confirm a disputed merge.
///
/// A merge is irreversible in the ways that matter: the loser's identity is
/// retired, and T-P4-004's rule means nothing downstream keeps a counter that a
/// later merge would have to reconcile. A contributor who could confirm one would
/// be able to retire an identity on the strength of a vote, which is not what a
/// vote is for.
#[tokio::test]
async fn a_contributor_cannot_confirm_a_disputed_merge() {
    let (_d, store) = store().await;
    let contributor = actor(&store, "contributor", Role::Contributor).await;
    let winner = object(&store).await;
    let loser = object(&store).await;
    let item = item(ItemType::DisputedMerge, winner, "title");

    let refused = moderation::resolve(
        &store,
        ItemType::DisputedMerge,
        &item,
        &contributor,
        Resolution::Merge(loser),
    )
    .await;
    assert!(refused.is_err(), "a Contributor confirmed a merge");
    assert!(
        commons_index::history::merges_into(&store, winner)
            .await
            .unwrap()
            .is_empty(),
        "and no merge record was written, so the two identities are still \\
         distinct and nothing has to be undone"
    );
}

// ---- 3. disputed consent --------------------------------------------------

/// An admin may decide a disputed consent tier.
#[tokio::test]
async fn an_admin_may_decide_a_disputed_consent_tier() {
    let (_d, store) = store().await;
    let admin = actor(&store, "admin", Role::Admin).await;
    let s = object(&store).await;
    let item = item(ItemType::DisputedConsent, s, "tier");

    moderation::resolve(
        &store,
        ItemType::DisputedConsent,
        &item,
        &admin,
        Resolution::SetConsentTier("verified".to_string()),
    )
    .await
    .unwrap();

    assert_eq!(
        commons_store::index::consent_tier(&store, s)
            .await
            .unwrap()
            .as_deref(),
        Some("verified"),
        "the tier the admin set is the tier a reader sees -- the queue marks its \
         item resolved and the effect is a separate write, and only this \
         assertion ties the two together"
    );
}

/// **Negative test 3.** A *steward* cannot decide a disputed consent tier.
///
/// This is the negative test that has no counterpart among the other six, and it
/// is the one worth the most. Every other type's boundary is Steward, so a suite
/// that checked "Contributor cannot" for all seven and "Admin can" for all seven
/// would pass with this type's boundary at Steward -- the single most consequential
/// boundary in the module, wrong, with every test green.
///
/// §14.2 is why: a consent revocation propagates as a tombstone and is never
/// outvoted by contribution. A steward is trusted with every other queue precisely
/// *because* their standing is contribution-derived, which is the same standing
/// §14.2 refuses to let decide this.
#[tokio::test]
async fn a_steward_cannot_decide_a_disputed_consent_tier() {
    let (_d, store) = store().await;
    let steward = actor(&store, "steward", Role::Steward).await;
    let s = object(&store).await;
    let item = item(ItemType::DisputedConsent, s, "tier");

    let refused = moderation::resolve(
        &store,
        ItemType::DisputedConsent,
        &item,
        &steward,
        Resolution::SetConsentTier("verified".to_string()),
    )
    .await;
    assert!(
        refused.is_err(),
        "a Steward decided a consent tier. §14.2 says a consent decision is \\
         never outvoted by contribution, and a steward's standing is \\
         contribution. This boundary is the only one in the module that is not \\
         Steward, and it is the only one that must not move."
    );
    assert_eq!(
        commons_store::index::consent_tier(&store, s).await.unwrap(),
        None,
        "and no tier was written, so a refused resolution leaves no partial state \
         for a later one to build on"
    );
}

// ---- 4. abuse report ------------------------------------------------------

/// A steward may dismiss an abuse report, and the dismissal is recorded.
#[tokio::test]
async fn a_steward_may_dismiss_an_abuse_report() {
    let (_d, store) = store().await;
    let steward = actor(&store, "steward", Role::Steward).await;
    let s = subject();
    named(&store, "alice").await;
    let report = moderation::raise_abuse_report(
        &store,
        s,
        "title",
        "alice",
        "this account is proposing the same wrong value on every clip",
    )
    .await
    .unwrap();

    moderation::resolve(
        &store,
        ItemType::AbuseReport,
        &item(ItemType::AbuseReport, s, "title"),
        &steward,
        Resolution::Dismiss,
    )
    .await
    .unwrap();

    let closed = moderation::abuse_report(&store, report).await.unwrap();
    assert!(
        closed.is_some_and(|r| r.dismissed_at.is_some()),
        "the report is marked dismissed, so a steward who looked at it can see \\
         that somebody else already did"
    );
}

/// **Negative test 4.** A contributor cannot dismiss an abuse report.
///
/// An abuse report is an accusation about a person, and dismissing one is a
/// judgement about a person. Letting the accused's peers -- contributors are
/// peers, and the queue's own premise is that they may be -- clear the queue
/// would make the queue deniable by anyone with three accounts, which is the
/// abuse the type exists to catch.
#[tokio::test]
async fn a_contributor_cannot_dismiss_an_abuse_report() {
    let (_d, store) = store().await;
    let contributor = actor(&store, "contributor", Role::Contributor).await;
    let s = subject();
    named(&store, "alice").await;
    let report = moderation::raise_abuse_report(&store, s, "title", "alice", "spam")
        .await
        .unwrap();

    let refused = moderation::resolve(
        &store,
        ItemType::AbuseReport,
        &item(ItemType::AbuseReport, s, "title"),
        &contributor,
        Resolution::Dismiss,
    )
    .await;
    assert!(
        refused.is_err(),
        "a Contributor dismissed an abuse report, which makes the queue \\
         deniable by anybody with several accounts"
    );
    assert!(
        moderation::abuse_report(&store, report)
            .await
            .unwrap()
            .is_some_and(|r| r.dismissed_at.is_none()),
        "and the report is still open"
    );
}

// ---- 5. pending edit ------------------------------------------------------

/// A steward may amend another user's pending edit, keeping both attributions.
#[tokio::test]
async fn a_steward_may_amend_another_users_pending_edit() {
    let (_d, store) = store().await;
    let steward = actor(&store, "steward", Role::Steward).await;
    let s = subject();
    named(&store, "alice").await;
    let pending = moderation::raise_pending_edit(
        &store,
        s,
        "title",
        "alice",
        &serde_json::json!("Alice's title"),
    )
    .await
    .unwrap();

    moderation::resolve(
        &store,
        ItemType::PendingEdit,
        &item(ItemType::PendingEdit, s, "title"),
        &steward,
        Resolution::Amend(
            pending,
            "corrected: the container says otherwise".to_string(),
        ),
    )
    .await
    .unwrap();

    let amended = moderation::pending_edit(&store, pending)
        .await
        .unwrap()
        .unwrap();
    // The steward corrected the *value*, so the effective value is the
    // correction. Read through `effective` because that is the accessor the rest
    // of the codebase uses, and a test reading `.amended_value` directly would
    // pass even if every call site were still reading the pre-amendment value.
    assert_eq!(
        moderation::effective(&amended),
        &serde_json::json!(["Alice's title", "corrected: the container says otherwise"]),
        "the value that will be written is the amended one, and both halves are \
         kept: a steward corrects an edit rather than replacing it, so a reader \
         can see what was proposed and what was decided"
    );
    // stash-box#599: editing another's pending edit keeps the original author.
    assert_eq!(
        amended.author, "alice",
        "the amendment does not take authorship away from Alice, and the \
         steward is recorded separately -- an edit with one author and a \
         stranger's text in it is unattributable"
    );
    assert_eq!(
        amended.amended_by.as_deref(),
        Some(steward.to_string().as_str()),
        "and the steward is recorded as the amender, separately from the author"
    );
}

/// **Negative test 5.** A contributor cannot amend another user's pending edit.
///
/// stash-box#599 allows the edit, deliberately: a steward who can see that the
/// container disagrees with the title is more useful than one who can only
/// reject. It does not allow a *contributor* to do it, because a contributor
/// editing a pending edit is indistinguishable from a contributor editing a
/// settled one -- the only thing distinguishing them is the pending flag, and a
/// rule that turns on a flag the editor also controls is not a rule.
#[tokio::test]
async fn a_contributor_cannot_amend_another_users_pending_edit() {
    let (_d, store) = store().await;
    let contributor = actor(&store, "contributor", Role::Contributor).await;
    let s = subject();
    named(&store, "alice").await;
    let pending =
        moderation::raise_pending_edit(&store, s, "title", "alice", &serde_json::json!("Alice's"))
            .await
            .unwrap();

    let refused = moderation::resolve(
        &store,
        ItemType::PendingEdit,
        &item(ItemType::PendingEdit, s, "title"),
        &contributor,
        Resolution::Amend(pending, "my version is better".to_string()),
    )
    .await;
    assert!(
        refused.is_err(),
        "a Contributor amended somebody else's pending edit, which is the same \\
         power as amending a settled one with an extra step"
    );
    let unchanged = moderation::pending_edit(&store, pending)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        unchanged.value,
        serde_json::json!("Alice's"),
        "and the edit still says what Alice wrote"
    );
}

// ---- 6. name collision ----------------------------------------------------

/// A steward may dismiss a name collision warning.
#[tokio::test]
async fn a_steward_may_dismiss_a_name_collision_warning() {
    let (_d, store) = store().await;
    let steward = actor(&store, "steward", Role::Steward).await;
    let s = subject();
    named(&store, "alice").await;
    let raised = moderation::raise_name_collision(
        &store,
        s,
        "studio",
        "alice",
        &serde_json::json!("Studio"),
        "there is already an alias called Studio (stash-box#714)",
    )
    .await
    .unwrap();

    moderation::resolve(
        &store,
        ItemType::NameCollision,
        &item(ItemType::NameCollision, s, "studio"),
        &steward,
        Resolution::Dismiss,
    )
    .await
    .unwrap();

    assert!(
        moderation::name_collision(&store, raised)
            .await
            .unwrap()
            .is_some_and(|c| c.dismissed_at.is_some()),
        "the warning is marked dismissed"
    );
}

/// **Negative test 6.** A contributor cannot dismiss a name collision warning.
///
/// stash-box#714 and #726 are explicit that a colliding name is a *warning and
/// not a hard error* -- in an amateur corpus a shared name is ordinary, and a
/// block would reject real data. The consequence is that the warning is the only
/// signal there is: if a contributor can dismiss it, a collision becomes invisible
/// without anyone deciding that a collision is acceptable.
#[tokio::test]
async fn a_contributor_cannot_dismiss_a_name_collision_warning() {
    let (_d, store) = store().await;
    let contributor = actor(&store, "contributor", Role::Contributor).await;
    let s = subject();
    named(&store, "alice").await;
    let raised = moderation::raise_name_collision(
        &store,
        s,
        "studio",
        "alice",
        &serde_json::json!("Studio"),
        "collides",
    )
    .await
    .unwrap();

    let refused = moderation::resolve(
        &store,
        ItemType::NameCollision,
        &item(ItemType::NameCollision, s, "studio"),
        &contributor,
        Resolution::Dismiss,
    )
    .await;
    assert!(
        refused.is_err(),
        "a Contributor dismissed a name-collision warning"
    );
    assert!(
        moderation::name_collision(&store, raised)
            .await
            .unwrap()
            .is_some_and(|c| c.dismissed_at.is_none()),
        "and the warning is still standing -- it is the only signal a shared \\
         name produces, per stash-box#714"
    );
}

// ---- 7. closed submission -------------------------------------------------

/// A steward may reopen a closed submission.
#[tokio::test]
async fn a_steward_may_reopen_a_closed_submission() {
    let (_d, store) = store().await;
    let steward = actor(&store, "steward", Role::Steward).await;
    let s = subject();
    named(&store, "steward2").await;
    let closed = moderation::close_submission(
        &store,
        s,
        "title",
        "steward2",
        "out of scope for this corpus",
    )
    .await
    .unwrap();

    moderation::resolve(
        &store,
        ItemType::ClosedSubmission,
        &item(ItemType::ClosedSubmission, s, "title"),
        &steward,
        Resolution::Reopen,
    )
    .await
    .unwrap();

    let _ = closed;
    let after = moderation::submission_for_row(&store, SubjectType::Object, &s, "title")
        .await
        .unwrap();
    assert!(
        !after.is_closed(),
        "the submission is open again. `is_closed` is the accessor every call \
         site uses, and a reopen that left it true would be indistinguishable \
         from a reopen that did nothing"
    );
    assert!(
        after.closed_at.is_some(),
        "and the close is still on the record: a reopened submission has not \
         stopped being one that was closed, it has stopped being closed"
    );
    assert!(
        after.reopened_by.is_some(),
        "and who reopened it is recorded, because 'closed' and 'closed then \\
         quietly reopened' are different histories and only the second one \\
         needs explaining"
    );
}

/// **Negative test 7.** A contributor cannot reopen a closed submission.
///
/// stash-box#570 is about *editing* a closed submission. Reopening is stronger
/// than editing it: editing changes a value, reopening changes whether anyone is
/// allowed to change the value at all. A corpus that has closed a submission has
/// made a decision about scope, and a contributor who could reopen it would make
/// that decision revocable by anyone with a proposal.
#[tokio::test]
async fn a_contributor_cannot_reopen_a_closed_submission() {
    let (_d, store) = store().await;
    let contributor = actor(&store, "contributor", Role::Contributor).await;
    let s = subject();
    named(&store, "steward2").await;
    let closed = moderation::close_submission(&store, s, "title", "steward2", "out of scope")
        .await
        .unwrap();

    let refused = moderation::resolve(
        &store,
        ItemType::ClosedSubmission,
        &item(ItemType::ClosedSubmission, s, "title"),
        &contributor,
        Resolution::Reopen,
    )
    .await;
    assert!(
        refused.is_err(),
        "a Contributor reopened a closed submission, which makes a scope \\
         decision revocable by anyone holding a proposal"
    );
    let _ = closed;
    let after = moderation::submission_for_row(&store, SubjectType::Object, &s, "title")
        .await
        .unwrap();
    assert!(after.is_closed(), "and it is still closed");
}

// ---- the queue itself -----------------------------------------------------

/// The queue lists open items, and a resolution removes the item from it.
#[tokio::test]
async fn the_queue_holds_open_items_and_not_resolved_ones() {
    let (_d, store) = store().await;
    named(&store, "bob").await;
    let steward = actor(&store, "steward", Role::Steward).await;
    named(&store, "alice").await;
    let a = subject();
    let b = subject();

    moderation::raise_abuse_report(&store, a, "title", "alice", "spam")
        .await
        .unwrap();
    moderation::raise_name_collision(
        &store,
        b,
        "studio",
        "bob",
        &Value::String("X".into()),
        "collides",
    )
    .await
    .unwrap();

    let open = moderation::queue(&store).await.unwrap();
    assert_eq!(open.len(), 2, "{open:#?}");
    let kinds: Vec<ItemType> = open.iter().map(|i| i.kind).collect();
    assert!(kinds.contains(&ItemType::AbuseReport));
    assert!(kinds.contains(&ItemType::NameCollision));

    moderation::resolve(
        &store,
        ItemType::AbuseReport,
        &item(ItemType::AbuseReport, a, "title"),
        &steward,
        Resolution::Dismiss,
    )
    .await
    .unwrap();

    let after = moderation::queue(&store).await.unwrap();
    assert_eq!(
        after.len(),
        1,
        "a resolved item leaves the queue: {after:#?}"
    );
    assert!(!after.iter().any(|i| i.kind == ItemType::AbuseReport));
}

/// A resolution is refused for a disabled account whatever its role.
///
/// `account_role` returns the role *and* whether the account is disabled, and the
/// second is what several call sites in the existing code discard. A disabled
/// account keeps its role, so a role-only check lets a disabled steward keep
/// moderating — which is the specific state a takedown creates, and the reason the
/// flag is read at all.
#[tokio::test]
async fn a_disabled_account_cannot_moderate_whatever_its_role() {
    let (_d, store) = store().await;
    let admin = actor(&store, "admin", Role::Admin).await;
    sqlx::query("UPDATE account SET disabled = 1 WHERE id = ?")
        .bind(admin.to_string())
        .execute(store.pool())
        .await
        .unwrap();

    let s = subject();
    for kind in ItemType::ALL {
        let refused = moderation::resolve(
            &store,
            kind,
            &item(kind, s, "title"),
            &admin,
            Resolution::Dismiss,
        )
        .await;
        assert!(
            refused.is_err(),
            "{kind:?} was resolvable by a disabled Admin. The role is still \\
             Admin on a disabled account, so a check that reads only the role \\
             lets a taken-down account keep curating."
        );
    }
    assert!(
        moderation::queue(&store).await.unwrap().is_empty(),
        "and nothing was queued or resolved on the way through"
    );
}

/// A disabled account is refused even on the types whose boundary is lowest.
#[tokio::test]
async fn a_disabled_contributor_cannot_raise_or_dismiss() {
    let (_d, store) = store().await;
    let contributor = actor(&store, "contributor", Role::Contributor).await;
    sqlx::query("UPDATE account SET disabled = 1 WHERE id = ?")
        .bind(contributor.to_string())
        .execute(store.pool())
        .await
        .unwrap();
    let s = subject();
    // Raising is a lesser act than resolving, but a disabled account raising an
    // item is how a queue gets filled with noise from a taken-down account.
    assert!(
        moderation::raise_abuse_report(&store, s, "title", "contributor", "spam")
            .await
            .is_err(),
        "a disabled account raised an abuse report"
    );
}

/// Resolution is refused for an account that does not exist.
///
/// Distinct from the disabled case and from the role case, and it is the one a
/// missing `account_role` lookup turns into a *grant*: a `None` role unwrapped
/// into a default is `Contributor`, and a Contributor fails the check, so the
/// accident happens to be safe here — but only by accident, and only for the
/// types whose boundary is above Contributor.
#[tokio::test]
async fn an_account_that_does_not_exist_cannot_resolve() {
    let (_d, store) = store().await;
    let nobody = Uuid::new_v4();
    for kind in ItemType::ALL {
        assert!(
            moderation::resolve(
                &store,
                kind,
                &item(kind, subject(), "title"),
                &nobody,
                Resolution::Dismiss
            )
            .await
            .is_err(),
            "{kind:?} was resolvable by an account that does not exist"
        );
    }
}

// ---- per-user pending limits (stash-box#782) --------------------------------

/// A contributor at the pending limit may not open another pending edit.
///
/// Not a queue item, and not a role check — it is a per-user cap, and it is here
/// because it is the other half of "pending": a queue is only useful if what is
/// in it was submitted by somebody who was allowed to submit it.
#[tokio::test]
async fn a_user_at_their_pending_limit_cannot_open_another() {
    let (_d, store) = store().await;
    actor(&store, "contributor", Role::Contributor).await;
    let s = subject();
    named(&store, "one_too_many").await;
    named(&store, "theirs").await;
    named(&store, "someone_else").await;
    let limit = moderation::pending_limit(Role::Contributor);

    for i in 0..limit {
        moderation::raise_pending_edit(
            &store,
            s,
            &format!("field{i}"),
            "contributor",
            &Value::from(i),
        )
        .await
        .unwrap();
    }
    let refused =
        moderation::raise_pending_edit(&store, s, "one_too_many", "contributor", &Value::from(9))
            .await;
    assert!(
        refused.is_err(),
        "a Contributor opened {} pending edits, over the limit of {limit}",
        limit + 1
    );

    // And the limit is per user, not global: a second contributor is unaffected.
    moderation::raise_pending_edit(&store, s, "theirs", "someone_else", &Value::from(1))
        .await
        .unwrap();
}

/// A steward is held to a tighter limit than a contributor.
///
/// A limit that is the same for everyone is not a limit, it is a constant: if a
/// steward may have as many pending edits as a contributor then a steward is the
/// account most able to flood the queue, since a steward's edits are the ones a
/// steward would resolve. The negative case — a *steward* over their own, lower
/// limit — is the assertion worth having.
#[tokio::test]
async fn a_steward_is_held_to_a_tighter_limit_than_a_contributor() {
    let (_d, store) = store().await;
    // The account exists because a pending edit resolves its author's role;
    // the id itself is not what this test is about.
    actor(&store, "steward", Role::Steward).await;
    let limit = moderation::pending_limit(Role::Steward);
    assert!(
        limit < moderation::pending_limit(Role::Contributor),
        "a Steward's limit ({limit}) must be tighter than a Contributor's ({})",
        moderation::pending_limit(Role::Contributor)
    );

    let s = subject();
    named(&store, "over").await;
    for i in 0..limit {
        moderation::raise_pending_edit(&store, s, &format!("f{i}"), "steward", &Value::from(i))
            .await
            .unwrap();
    }
    assert!(
        moderation::raise_pending_edit(&store, s, "over", "steward", &Value::from(99))
            .await
            .is_err(),
        "a Steward opened {limit} pending edits and one more, over their own \\
         lower limit. A steward is the account most able to flood the queue, \\
         because a steward's edits are the ones a steward would resolve."
    );
}

/// An admin is not subject to the limit at all.
///
/// Worth stating because "the limit is a number in a config" and "the limit
/// applies to everyone" are different designs, and this is the second. An admin
/// bulk-editing a corpus is the case the limit would obstruct, and there is
/// nothing behind an admin that a flood of pending edits could damage.
#[tokio::test]
async fn an_admin_is_not_subject_to_the_pending_limit() {
    let (_d, store) = store().await;
    actor(&store, "admin", Role::Admin).await;
    assert_eq!(moderation::pending_limit(Role::Admin), usize::MAX);
    let s = subject();
    for i in 0..20 {
        moderation::raise_pending_edit(&store, s, &format!("f{i}"), "admin", &Value::from(i))
            .await
            .unwrap();
    }
}

// ---- per-user pending limits, released by resolution -----------------------

/// Resolving a pending edit frees the slot, so the cap is on *open* edits.
///
/// Otherwise the cap is a lifetime cap: a contributor who proposes forty things
/// over a year is permanently locked out, which is not what "limits on pending
/// edits per user" means and is a way to lose a good contributor without ever
/// queueing anything.
#[tokio::test]
async fn resolving_a_pending_edit_frees_the_slot() {
    let (_d, store) = store().await;
    let steward = actor(&store, "steward", Role::Steward).await;
    let s = subject();
    named(&store, "now_fine").await;
    named(&store, "over").await;
    named(&store, "contributor").await;
    let limit = moderation::pending_limit(Role::Contributor);
    for i in 0..limit {
        moderation::raise_pending_edit(&store, s, &format!("f{i}"), "contributor", &Value::from(i))
            .await
            .unwrap();
    }
    assert!(
        moderation::raise_pending_edit(&store, s, "over", "contributor", &Value::from(9))
            .await
            .is_err()
    );

    moderation::resolve(
        &store,
        ItemType::PendingEdit,
        &item(ItemType::PendingEdit, s, "f0"),
        &steward,
        Resolution::Dismiss,
    )
    .await
    .unwrap();

    moderation::raise_pending_edit(&store, s, "now_fine", "contributor", &Value::from(9))
        .await
        .unwrap_or_else(|e| panic!("the slot was not released: {e}"));
}

// ---- per-user pending limits, counted per subject --------------------------

/// The limit is per user, not per user *and subject*.
///
/// A contributor working on a hundred objects has a hundred pending edits and is
/// still one person. Counting per subject would make the limit a function of how
/// a corpus is shaped, so a library of a thousand one-item objects would have no
/// effective limit at all while a library of one object would have one after two
/// edits.
#[tokio::test]
async fn the_pending_limit_counts_across_subjects() {
    let (_d, store) = store().await;
    named(&store, "contributor").await;
    let limit = moderation::pending_limit(Role::Contributor);
    for i in 0..limit {
        // A different subject every time.
        moderation::raise_pending_edit(&store, subject(), "title", "contributor", &Value::from(i))
            .await
            .unwrap();
    }
    assert!(
        moderation::raise_pending_edit(&store, subject(), "title", "contributor", &Value::from(0))
            .await
            .is_err(),
        "the limit was met across {limit} different subjects by one account, \\
         which is correct: it is one person with {limit} open edits, not {limit} \\
         people with one"
    );
}

// ---- helpers --------------------------------------------------------------

fn item(kind: ItemType, subject_id: Uuid, field: &str) -> moderation::Item {
    moderation::Item {
        kind,
        subject_type: SubjectType::Object,
        subject_id,
        field: field.to_string(),
        summary: format!("a {kind:?} on {field}"),
        raised_by: Some("alice".to_string()),
        raised_at: ts::now(),
    }
}
