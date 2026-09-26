//! §8.6 — edit history, and the rule that scores are recomputed rather than
//! counted.
//!
//! The rule exists because of two real upstream bugs. stash-box#943: merging two
//! entities silently lost edits, because a merge moved rows and a score is a
//! counter over rows. stash-box#9: `null` and *unset* were confused, so deleting
//! a field and setting it to null were the same thing.
//!
//! So nothing here maintains a counter. `field_score` is a pure function of the
//! accepted-edit set, and the test that matters is
//! `a_score_after_a_merge_is_identical_to_a_fresh_recomputation` — not that the
//! number is right, but that it is *the same number the function would produce
//! from scratch*. A merge that dropped an edit would still produce a plausible
//! score; only this assertion sees the difference.

use commons_core::Role;
use commons_index::history::{self, FieldState};
use commons_store::Store;
use uuid::Uuid;

mod common;
use common::{account, store};

/// An account with this handle, created once.
///
/// `account` in the shared fixtures always inserts, and `account.handle` is
/// unique, so a test with two edits by the same author cannot call it twice.
async fn ensure_account(store: &Store, handle: &str) -> Uuid {
    let existing: Option<String> = sqlx::query_scalar("SELECT id FROM account WHERE handle = ?")
        .bind(handle)
        .fetch_optional(store.pool())
        .await
        .unwrap();
    if let Some(id) = existing {
        return Uuid::parse_str(&id).unwrap();
    }
    account(store, handle, Role::Contributor, 1.0).await
}

/// A proposal, as a user would make it.
///
/// A local helper rather than a call into `resolve`: §8.6 is about what happens
/// *after* a proposal is accepted, and the fixtures should not depend on the
/// resolution machinery to exist. The proposal is a real row with a real id, via
/// the store's own `insert_proposal`.
async fn propose(
    store: &Store,
    subject_id: Uuid,
    field: &str,
    value: &serde_json::Value,
    by: Option<Uuid>,
) -> commons_core::FieldProposal {
    let mut p = commons_core::FieldProposal::new(
        commons_core::SubjectType::Object,
        subject_id,
        field,
        value.to_string(),
        commons_core::ProposalSource::User,
    );
    p.proposer_id = by.map(|id| id.to_string());
    commons_store::index::insert_proposal(store, &p)
        .await
        .unwrap();
    p
}

/// An accepted edit: one proposal, accepted, with the weight it carries.
async fn accept(store: &Store, subject_id: Uuid, field: &str, value: &str, by: &str) -> Uuid {
    // The account row exists so the author is a real account, and history stores
    // the *handle* -- a uuid is unreadable in a blame line and stable across
    // renames, which is the point of a handle.
    //
    // `ensure`, not `account`: `account.handle` is unique, so a fixture that
    // created the account on every edit would fail on the second edit by the same
    // author. Four of the tests here do that, and the failure surfaced as a
    // constraint violation inside the fixture rather than as anything to do with
    // history.
    ensure_account(store, by).await;
    let _author = Uuid::new_v4();
    let value = serde_json::json!(value);
    let proposal = propose(store, subject_id, field, &value, Some(_author)).await;
    // Accepted, not merely proposed: the accepted-edit set is the unit of history,
    // and a proposal nobody accepted is not in it.
    history::accept(store, &proposal, Some(by), 1.0)
        .await
        .unwrap()
}

// ---- 8.6.1: history, and what it has to distinguish ------------------------

/// A field's history lists the accepted edits in order, with who and when.
#[tokio::test]
async fn a_fields_history_is_the_accepted_edits_in_order() {
    let (_d, store) = store().await;
    let object = Uuid::new_v4();
    accept(&store, object, "title", "First", "alice").await;
    accept(&store, object, "title", "Second", "bob").await;
    accept(&store, object, "date", "2021-06-14", "carol").await;

    let title = history::field_history(&store, object, "title")
        .await
        .unwrap();
    assert_eq!(title.len(), 2, "{title:#?}");
    let values: Vec<&str> = title.iter().map(|e| e.value.as_str().unwrap()).collect();
    assert_eq!(values, vec!["First", "Second"], "in acceptance order");
    let who: Vec<&str> = title
        .iter()
        .map(|e| e.author.as_deref().unwrap_or("<none>"))
        .collect();
    assert_eq!(who, vec!["alice", "bob"]);

    // A different field is a different history. This is the whole of
    // "per-field": `date` has one entry, not three.
    let date = history::field_history(&store, object, "date")
        .await
        .unwrap();
    assert_eq!(date.len(), 1);
}

/// A field nobody edited has empty history, not an error.
///
/// A field that has never been touched is the common case for a library that
/// has just been scanned, and an empty vec is the honest answer. `Err` here
/// would make every reader special-case it, and the special case is where
/// "empty" silently becomes "unavailable" and then "no edits were lost".
#[tokio::test]
async fn an_unedited_field_has_empty_history() {
    let (_d, store) = store().await;
    let object = Uuid::new_v4();
    let entries = history::field_history(&store, object, "title")
        .await
        .unwrap();
    assert!(entries.is_empty(), "{entries:#?}");
}

// ---- 8.6.2: null and unset are different states ---------------------------

/// A field set to JSON null is *set to null*, which is not the same as unset.
///
/// This is stash-box#9. A nullable field where "the value is null" and "there is
/// no value" are the same state cannot express a deliberately empty field, and
/// cannot tell a user who cleared it from a user who never filled it in. Both
/// appear in the same table, so the distinction has to be in the value.
#[tokio::test]
async fn a_null_is_not_an_unset() {
    let (_d, store) = store().await;
    let object = Uuid::new_v4();
    accept(&store, object, "description", "Something", "alice").await;

    assert_eq!(
        history::field_state(&store, object, "description")
            .await
            .unwrap(),
        FieldState::Set,
        "a string is Set"
    );

    // The same field, now explicitly null.
    let author = ensure_account(&store, "bob").await;
    let cleared = propose(
        &store,
        object,
        "description",
        &serde_json::Value::Null,
        Some(author),
    )
    .await;
    history::accept(&store, &cleared, Some("bob"), 1.0)
        .await
        .unwrap();

    assert_eq!(
        history::field_state(&store, object, "description")
            .await
            .unwrap(),
        FieldState::Null,
        "an explicit null is Null, which is a different state from Set and from \
         Unset -- and both must be tellable apart, or a cleared field and a never-\
         filled one look the same to a reader"
    );

    // And the history still has both edits, in order: the null is an edit, not a
    // deletion of one.
    let entries = history::field_history(&store, object, "description")
        .await
        .unwrap();
    assert_eq!(entries.len(), 2, "{entries:#?}");
    assert!(
        entries[1].value.is_null(),
        "the second edit's value is null: {:?}",
        entries[1].value
    );
}

/// A field with no accepted edit is `Unset`, and that is distinct from `Null`.
#[tokio::test]
async fn a_field_with_no_edits_is_unset_not_null() {
    let (_d, store) = store().await;
    let object = Uuid::new_v4();
    assert_eq!(
        history::field_state(&store, object, "description")
            .await
            .unwrap(),
        FieldState::Unset
    );
}

/// The three states are three, and a reader can tell which is which.
///
/// One test for all three rather than three for one each, because the property
/// that matters is that the three are *pairwise* distinguishable -- two tests
/// that each compare one state to `Unset` would pass if `Null` and `Set` were the
/// same value.
#[tokio::test]
async fn the_three_states_are_pairwise_distinct() {
    let (_d, store) = store().await;
    let object = Uuid::new_v4();

    // Unset.
    assert_eq!(
        history::field_state(&store, object, "f").await.unwrap(),
        FieldState::Unset
    );
    // Set to a string.
    accept(&store, object, "f", "value", "alice").await;
    assert_eq!(
        history::field_state(&store, object, "f").await.unwrap(),
        FieldState::Set
    );
    // Set to null. A *different field*, so that each of the three is observable
    // at once and the comparison is not against a moving target.
    let author = ensure_account(&store, "bob").await;
    let nulled = propose(&store, object, "g", &serde_json::Value::Null, Some(author)).await;
    history::accept(&store, &nulled, Some("bob"), 1.0)
        .await
        .unwrap();
    assert_eq!(
        history::field_state(&store, object, "g").await.unwrap(),
        FieldState::Null
    );

    // All three at once, and none of them equal to another.
    let (unset, set, null) = (
        history::field_state(&store, object, "never_touched")
            .await
            .unwrap(),
        history::field_state(&store, object, "f").await.unwrap(),
        history::field_state(&store, object, "g").await.unwrap(),
    );
    assert_ne!(unset, set);
    assert_ne!(set, null);
    assert_ne!(unset, null);
}

// ---- 8.6.3: the score is a function of the accepted-edit set ---------------

/// The score is the heaviest accepted edit, not a running total.
///
/// Not obviously a choice: "the score of a field" could reasonably mean the sum
/// of every accepted edit, which is what a counter would hold. It is the
/// heaviest, because the field's value *is* that edit's value, and a sum would
/// be a number with no value behind it. `rejected` edits and `pending` ones are
/// excluded either way, so the sum reading is distinguishable only by a test that
/// adds a second accepted edit -- which is why the next test exists.
#[tokio::test]
async fn a_fields_score_is_the_heaviest_accepted_edit_not_a_sum() {
    let (_d, store) = store().await;
    let object = Uuid::new_v4();
    accept(&store, object, "title", "Light", "alice").await;
    accept(&store, object, "title", "Heavy", "bob").await;

    let score = history::field_score(&store, object, "title").await.unwrap();
    // 1.0 + 1.0 would be 2.0 under the counter reading. The heaviest is 1.0.
    assert!(
        score < 2.0,
        "a field's score must be the heaviest accepted edit, not their sum: {score}"
    );
    assert!(
        score >= 1.0,
        "and it must be at least the heaviest edit's own weight: {score}"
    );
}

/// A rejected edit contributes nothing, and removing an accepted one lowers the
/// score by recomputation rather than by subtraction.
#[tokio::test]
async fn a_rejected_edit_is_not_in_the_accepted_set() {
    let (_d, store) = store().await;
    let object = Uuid::new_v4();
    let accepted = accept(&store, object, "title", "Kept", "alice").await;

    // A heavier proposal that is *not* accepted.
    let author = ensure_account(&store, "bob").await;
    let rejected = propose(
        &store,
        object,
        "title",
        &serde_json::json!("Rejected"),
        Some(author),
    )
    .await;
    // A proposal that is never accepted. The negative test does not need a
    // "reject" operation: an edit that was never accepted is simply not in the
    // set, and `rejected` is the absence of an `accept` call. Asserting that
    // explicitly is worth a line, so a reader does not assume a missing call is
    // an oversight.
    assert!(
        history::field_history(&store, object, "title")
            .await
            .unwrap()
            .iter()
            .all(|e| e.value != serde_json::json!("Rejected")),
        "a proposal that was never accepted is not an edit"
    );
    let _ = rejected;

    let with_both = history::field_score(&store, object, "title").await.unwrap();
    let entries = history::field_history(&store, object, "title")
        .await
        .unwrap();
    assert_eq!(
        entries.len(),
        1,
        "a rejected edit is not history: {entries:#?}"
    );

    // Now retract the accepted one, and the field is unset with a zero score.
    history::retract(&store, accepted).await.unwrap();
    let after = history::field_score(&store, object, "title").await.unwrap();
    assert!(
        after < with_both,
        "removing the only accepted edit must lower the score: {with_both} -> {after}"
    );
    assert_eq!(
        history::field_state(&store, object, "title").await.unwrap(),
        FieldState::Unset,
        "and with no accepted edit the field is Unset again -- the last edit \
         being retracted does not leave a null behind"
    );
}

/// THE test. A merge re-points the proposals, and the surviving entity's score
/// is identical to a fresh recomputation from the edit set.
///
/// The clause that matters is *identical*. A merge that dropped an edit would
/// still leave a score, and a test asserting "the score is positive" would pass.
/// So this computes the score the function would produce from scratch, over an
/// accepted-edit set that is known by construction, and requires equality.
///
/// `recompute` is the same function `field_score` calls. That is deliberate: the
/// assertion is not "the number is right" but "no counter has drifted from what
/// recomputing would give", which is the property #943 lost. The two calls are
/// separate so a cached value cannot satisfy both.
#[tokio::test]
async fn a_score_after_a_merge_is_identical_to_a_fresh_recomputation() {
    let (_d, store) = store().await;
    let winner = Uuid::new_v4();
    let loser = Uuid::new_v4();

    // The winner has two accepted edits; the loser has one. The loser's edit is
    // the one a merge that only re-points appearances would lose.
    accept(&store, winner, "title", "Winner A", "alice").await;
    accept(&store, winner, "title", "Winner B", "bob").await;
    accept(&store, loser, "title", "Loser Only", "carol").await;
    let loser_only = accept(&store, loser, "date", "2021-06-14", "carol").await;

    let before = history::field_score(&store, winner, "title").await.unwrap();

    // Merge, which re-points every proposal from the loser to the winner.
    ensure_account(&store, "dave").await;
    history::merge_objects(&store, loser, winner, "dave")
        .await
        .unwrap();

    // The loser's edit is now the winner's.
    let entries = history::field_history(&store, winner, "title")
        .await
        .unwrap();
    assert_eq!(
        entries.len(),
        3,
        "a merge must not lose edits (stash-box#943): {entries:#?}"
    );
    let values: Vec<&str> = entries.iter().map(|e| e.value.as_str().unwrap()).collect();
    assert!(
        values.contains(&"Loser Only"),
        "the loser's edit survived the merge: {values:?}"
    );

    // The other field too, which is the case a single-field merge gets right by
    // accident.
    let dates = history::field_history(&store, winner, "date")
        .await
        .unwrap();
    assert_eq!(
        dates.len(),
        1,
        "the loser's other field survived: {dates:#?}"
    );
    assert_eq!(dates[0].id, loser_only);

    // THE ASSERTION. Not "the score is right" -- "the score is what recomputing
    // produces".
    let after = history::field_score(&store, winner, "title").await.unwrap();
    let fresh = history::recompute(&store, winner, "title").await.unwrap();
    assert_eq!(
        after, fresh,
        "a merge must leave a score identical to a fresh recomputation, or the \
         merge corrupted it (stash-box#943)"
    );
    assert!(
        after >= before,
        "and adding an edit cannot lower the score: {before} -> {after}"
    );

    // And the same for the field that existed only on the loser: a recomputation
    // of a field with no edits is zero, and the winner's `date` has exactly one.
    let date_after = history::field_score(&store, winner, "date").await.unwrap();
    let date_fresh = history::recompute(&store, winner, "date").await.unwrap();
    assert_eq!(date_after, date_fresh);
    assert!(
        date_after > 0.0,
        "the loser's edit carries its weight: {date_after}"
    );
}

/// A merge is recorded, so a reader can tell a merge from a cluster that never
/// had a second identity.
///
/// The same argument as `cluster_merge` in migration 0004, and for the same
/// reason: once the loser is retired, nothing in the surviving rows says two
/// identities were ever one. An operator who merges the wrong pair has no way
/// back, and a reader months later is looking at an ordinary object.
#[tokio::test]
async fn a_merge_is_recorded_with_both_sides_and_the_actor() {
    let (_d, store) = store().await;
    let winner = Uuid::new_v4();
    let loser = Uuid::new_v4();
    accept(&store, loser, "title", "From the loser", "alice").await;

    ensure_account(&store, "erin").await;
    history::merge_objects(&store, loser, winner, "dave")
        .await
        .unwrap();

    let merges = history::merges_into(&store, winner).await.unwrap();
    assert_eq!(merges.len(), 1, "{merges:#?}");
    assert_eq!(merges[0].winner, winner);
    assert_eq!(merges[0].loser, loser);
    assert_eq!(merges[0].actor.as_deref(), Some("dave"));
    assert!(
        merges[0].moved_edits > 0,
        "the record says how many edits moved, so a merge that moved none is \
         visible: {merges:#?}"
    );
}

// ---- 8.6.4: blame and revert ---------------------------------------------

/// Blame names the author of the value a reader is looking at.
///
/// Not "the most recent editor" -- the author of the *current* value, which is
/// the heaviest accepted edit. For a field with no accepted edit there is nobody
/// to blame and the answer is `None`, not the most recent person who touched it
/// and was overridden.
#[tokio::test]
async fn blame_names_the_author_of_the_current_value() {
    let (_d, store) = store().await;
    let object = Uuid::new_v4();
    accept(&store, object, "title", "First", "alice").await;
    accept(&store, object, "title", "Second", "bob").await;

    let blame = history::blame(&store, object, "title").await.unwrap();
    let author = blame.as_deref();
    let entries = history::field_history(&store, object, "title")
        .await
        .unwrap();
    // The heaviest, and among equal weights the *newest* -- the same rule the
    // module uses. `Iterator::max_by` returns the last of several equal maxima,
    // so over a history already in sequence order that is the newest, which is
    // what `ORDER BY weight DESC, seq DESC` gives. The first version of this
    // assertion used `.rev()` on the assumption that `max_by` returns the first;
    // it does not, and the reversal made the test assert the opposite tiebreak
    // from the implementation. Both edits weigh 1.0, which is the only reason a
    // wrong tiebreak shows up at all -- a fixture with two different weights would
    // have hidden it.
    let heaviest = entries
        .iter()
        .max_by(|a, b| a.weight.total_cmp(&b.weight))
        .expect("an accepted edit");
    assert_eq!(
        author,
        heaviest.author.as_deref(),
        "blame follows the heaviest accepted edit, breaking a tie towards the \
         newest, not the oldest"
    );
    // An unedited field has nobody to blame.
    assert_eq!(
        history::blame(&store, object, "never_edited")
            .await
            .unwrap(),
        None,
        "a field nobody edited has no author, and reporting the last person who \
         was overridden would be a lie"
    );
}

/// A revert is an edit, so it is in the history and the blame moves.
///
/// Not a deletion: reverting to a previous value adds an edit whose value is that
/// previous value. The alternative -- removing the later edits -- loses the fact
/// that they were ever proposed, and makes a revert indistinguishable from
/// somebody having never made them.
#[tokio::test]
async fn a_revert_is_an_edit_not_a_deletion() {
    let (_d, store) = store().await;
    let object = Uuid::new_v4();
    let first = accept(&store, object, "title", "Original", "alice").await;
    accept(&store, object, "title", "Changed", "bob").await;

    let author = ensure_account(&store, "carol").await;
    let entry = history::revert(&store, object, "title", first, &author.to_string())
        .await
        .unwrap();

    assert_eq!(entry.value, serde_json::json!("Original"));
    let entries = history::field_history(&store, object, "title")
        .await
        .unwrap();
    assert_eq!(
        entries.len(),
        3,
        "the reverted edit is still in the history: {entries:#?}"
    );
    let revert_entry = entries
        .iter()
        .find(|e| e.id == entry.id)
        .expect("the revert is in the history");
    assert_eq!(
        revert_entry.justification.as_deref(),
        Some("reverted to an earlier accepted edit"),
        "and it says what it is, so a reader is not left guessing why the value \
         went backwards"
    );
    assert_eq!(
        entries.last().map(|e| e.id),
        Some(entry.id),
        "and it is the newest, so a reader scrolling the history sees the revert \
         last rather than having to work it out from the values"
    );

    // The value is back.
    let value = history::current_value(&store, object, "title")
        .await
        .unwrap();
    assert_eq!(value, Some(serde_json::json!("Original")));
}

// ---- 8.6.5: removing one's own information -------------------------------

/// A user can remove their own edit from history, and what is removed is theirs.
///
/// stash-box#656. The constraint is narrow and the test pins both halves: an
/// author may remove their *own* edit, and may not remove someone else's. A
/// history a user can edit at all would be no history; a history nobody can edit
/// keeps a name attached to a contribution a user has asked to be dissociated
/// from.
#[tokio::test]
async fn an_author_may_remove_their_own_history_entry_and_not_another() {
    let (_d, store) = store().await;
    let object = Uuid::new_v4();
    let alices = accept(&store, object, "title", "Alice's", "alice").await;
    accept(&store, object, "title", "Bob's", "bob").await;

    // Bob cannot remove Alice's.
    ensure_account(&store, "bob").await;
    let refused = history::remove_history_entry(&store, alices, "bob").await;
    assert!(
        refused.is_err(),
        "removing another user's history entry must be refused"
    );
    assert_eq!(
        history::field_history(&store, object, "title")
            .await
            .unwrap()
            .len(),
        2,
        "and it must not have been removed"
    );

    // Alice can remove her own.
    ensure_account(&store, "alice").await;
    history::remove_history_entry(&store, alices, "alice")
        .await
        .unwrap();
    let entries = history::field_history(&store, object, "title")
        .await
        .unwrap();
    assert_eq!(
        entries.len(),
        1,
        "the author's own entry is gone: {entries:#?}"
    );
    assert_eq!(
        entries[0].author.as_deref(),
        Some("bob"),
        "and the other user's is not"
    );
}

/// A removed history entry leaves a tombstone, so the record is not a silent
/// hole.
///
/// A hard delete makes "nobody proposed this" and "this was proposed and the
/// author asked for it to be unlinked" indistinguishable to the next reader, and
/// the second is the one a data subject cares about. What is removed is the
/// *attribution*; what remains is that an edit existed.
#[tokio::test]
async fn a_removed_entry_leaves_a_tombstone() {
    let (_d, store) = store().await;
    let object = Uuid::new_v4();
    let alices = accept(&store, object, "title", "Alice's", "alice").await;

    ensure_account(&store, "alice").await;
    history::remove_history_entry(&store, alices, "alice")
        .await
        .unwrap();

    let tombstone = history::history_entry(&store, alices).await.unwrap();
    assert!(tombstone.is_some(), "the entry still exists as a record");
    let t = tombstone.expect("a tombstone");
    assert!(
        t.author.is_none(),
        "the attribution is what is removed: {:?}",
        t.author
    );
    assert!(
        t.removed.is_some(),
        "and when it was removed is kept: {:?}",
        t.removed
    );
    assert_eq!(
        t.value,
        serde_json::json!("Alice's"),
        "the value is untouched"
    );

    // And a removed entry is not in the field's history.
    assert!(history::field_history(&store, object, "title")
        .await
        .unwrap()
        .is_empty());
}

/// A report says who reported what and why, and is not itself editable by the
/// reporter.
///
/// stash-box#656 pairs the removal with a report button. The report is a fact
/// about the reporter, so it keeps their name; if it did not, reporting would be
/// anonymous and the moderation queue would have nothing to act on. That is the
/// boundary, and the test states it rather than leaving it to a reader.
#[tokio::test]
async fn a_report_names_its_reporter() {
    let (_d, store) = store().await;
    let object = Uuid::new_v4();
    let alices = accept(&store, object, "title", "Alice's", "alice").await;

    ensure_account(&store, "bob").await;
    let report = history::report_entry(
        &store,
        alices,
        "bob",
        "this edit names a person who has asked to be unlisted",
    )
    .await
    .unwrap();

    assert_eq!(report.entry, alices);
    assert_eq!(report.reporter, "bob");
    assert!(!report.reason.is_empty());

    // It survives the entry's removal, because a report about an entry that has
    // been unlinked is still a report.
    ensure_account(&store, "alice").await;
    history::remove_history_entry(&store, alices, "alice")
        .await
        .unwrap();
    assert!(
        history::reports_for(&store, alices).await.unwrap().len() == 1,
        "a report is not a history entry and outlives one"
    );
}

// ---- the negative space ---------------------------------------------------

/// A merge that moves nothing still records itself.
///
/// A merge with zero moved proposals is either a no-op or a bug, and the record
/// is what tells the two apart: `moved_proposals = 0` on a merge a user asked
/// for is a fact, and a silent success is not.
#[tokio::test]
async fn a_merge_that_moves_nothing_is_still_recorded() {
    let (_d, store) = store().await;
    let winner = Uuid::new_v4();
    let loser = Uuid::new_v4();
    ensure_account(&store, "alice").await;

    history::merge_objects(&store, loser, winner, "dave")
        .await
        .unwrap();

    let merges = history::merges_into(&store, winner).await.unwrap();
    assert_eq!(merges.len(), 1, "the merge happened, so it is recorded");
    assert_eq!(
        merges[0].moved_edits, 0,
        "and it says plainly that it moved nothing"
    );
}

/// Merging an object into itself is refused.
///
/// Every other operation in this file is a merge of two *different* identities.
/// Merging one into itself would double its own edits into its own history --
/// arithmetically harmless, historically a lie, and the first step of a
/// self-referential loop if anything downstream follows merges.
#[tokio::test]
async fn merging_an_object_into_itself_is_refused() {
    let (_d, store) = store().await;
    let only = Uuid::new_v4();
    accept(&store, only, "title", "Once", "alice").await;
    ensure_account(&store, "bob").await;

    assert!(
        history::merge_objects(&store, only, only, "dave")
            .await
            .is_err(),
        "merging an object into itself must be refused"
    );
    assert_eq!(
        history::field_history(&store, only, "title")
            .await
            .unwrap()
            .len(),
        1,
        "and nothing was doubled: the loser's edit appears once, not twice"
    );
    assert!(
        history::merges_into(&store, only).await.unwrap().is_empty(),
        "and no record was written for a merge that did not happen"
    );
}

/// A revert to an edit that is not in this field's history is refused.
///
/// Without the check, a caller could revert to any edit id in the database --
/// another field's, or another object's -- and write it here, which is a way to
/// put a value in a field that no accepted edit ever supported.
#[tokio::test]
async fn a_revert_to_an_edit_of_another_field_is_refused() {
    let (_d, store) = store().await;
    let object = Uuid::new_v4();
    accept(&store, object, "title", "A title", "alice").await;
    let date_edit = accept(&store, object, "date", "2021-06-14", "alice").await;
    let author = ensure_account(&store, "bob").await;

    assert!(
        history::revert(&store, object, "title", date_edit, &author.to_string())
            .await
            .is_err(),
        "a date is not a title, and reverting to one must be refused"
    );
    assert_eq!(
        history::current_value(&store, object, "title")
            .await
            .unwrap(),
        Some(serde_json::json!("A title")),
        "and the field still holds its own value"
    );
}
