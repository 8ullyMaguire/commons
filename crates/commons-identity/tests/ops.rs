//! T-P3-004 acceptance: §7.2's four operations, plus disambiguation.
//!
//! # What this ticket is really about
//!
//! Every one of these is an operation a person performs *because the automatic
//! path was wrong*. So the property worth testing is not that the rows changed
//! -- it is that afterwards the automatic path agrees with what was asked. A
//! merge that leaves the losing cluster's centroid reachable, or a split that
//! leaves the original's centroid untouched, produces a system that will
//! silently re-create the mistake the user just corrected. The recomputation
//! is the feature; the re-pointing is bookkeeping.
//!
//! Three of these are upstream bugs that cost users data (stash-box#943 lost
//! edits on merge, #610 could not choose a primary, #726/#714 blocked real
//! names), and two are parser bugs (#778/#5033 commas, #2293 non-ASCII). The
//! parser ones are cheap and go last, which is the wrong order if you are
//! judging the ticket by cost.

mod common;

use common::*;
use commons_identity::cluster::ops::{self, Alias, AliasScope, Disambiguation};
use commons_identity::cluster::ClusterError;
use commons_store::Store;

/// Does this cluster still exist?
///
/// Written here rather than exported from the store because the store's own
/// `get` is enough for everything the implementation needs, and a public
/// `exists` would be a second spelling of `get(..).is_some()` that callers
/// could use interchangeably with it.
async fn exists(store: &Store, id: &str) -> bool {
    cluster::store::get(store, id).await.unwrap().is_some()
}

/// Every appearance in the library, however many there are.
async fn appearance_count(store: &Store) -> usize {
    cluster::store::all_appearances(store).await.unwrap().len()
}

/// A cluster's decoded face centroid.
///
/// `store::get` returns the hex in a `ClusterRow`; decoding it here keeps the
/// test reading like the arithmetic it is checking, rather than like a hex
/// round-trip.
async fn centroid(store: &Store, id: &str) -> Option<Vec<f32>> {
    cluster::store::get(store, id)
        .await
        .unwrap()
        .and_then(|c| c.centroid)
}

/// Two clusters, `a` and `b`, far enough apart that only an explicit operation
/// would ever join them.
///
/// The `TempDir` is returned with the store. Dropping it deletes the SQLite
/// file while the pool still has it open, and the failure surfaces at the *next*
/// query as "unable to open database file" -- in a test with nothing to do with
/// the line that caused it. Same trap as `cluster.rs`'s ambiguity-margin test.
async fn two_clusters() -> (tempfile::TempDir, Store, Engine, String, String) {
    let (d, store, engine) = tuned(0.90, 0.7, 0.3).await;
    let base = Rng::new(71).unit();

    // `from_pair` rather than two `at`-distances from one origin, and the
    // reason is T-P3-003's: cosine distance does not add along an arc, so a
    // fixture that reads as "0.05 from here and 0.80 from here" produces two
    // vectors 0.417 apart -- which is inside the threshold, so the fixture
    // silently produced one cluster and every merge test below passed against
    // a pair that did not exist.
    object(&store, "a1").await;
    let a = engine
        .assign("a1", &Frame::from_pair(&base, 0.05), None)
        .await
        .unwrap()
        .cluster_id
        .expect("a created cluster names itself");

    // Far enough to clear the threshold with room to spare. The engine joins on
    // `distance <= threshold`, so a fixture separated by *exactly* the
    // threshold joins -- and the assert below is the only thing standing
    // between that and eleven tests that quietly assert against one cluster.
    object(&store, "b1").await;
    let b = engine
        .assign("b1", &Frame::from_pair(&base, 1.40), None)
        .await
        .unwrap()
        .cluster_id
        .expect("a created cluster names itself");
    assert_ne!(a, b, "the fixture starts with two distinct clusters");
    (d, store, engine, a, b)
}

/// Merge re-points every appearance and leaves the loser gone.
#[tokio::test]
async fn merge_repoints_every_appearance_and_retires_the_loser() {
    let (_d, store, _engine, a, b) = two_clusters().await;
    let before = appearance_count(&store).await;
    assert_eq!(before, 2, "one appearance each");

    let report = ops::merge(&store, &a, &b, None).await.unwrap();
    assert_eq!(
        report.moved_appearances, 1,
        "the losing cluster's appearance moved: {report:?}"
    );
    assert_eq!(report.winner, a, "the named survivor is the winner");
    assert_eq!(report.loser, b);

    assert_eq!(
        appearance_count(&store).await,
        before,
        "a merge moves references; it never deletes an appearance"
    );
    assert!(
        !exists(&store, &b).await,
        "the losing cluster is retired, not left in place for a later \\
         assignment to join"
    );

    // The point of the operation: the automatic path now agrees.
    let remaining = cluster::all_with_members(&store).await.unwrap();
    assert_eq!(remaining.len(), 1, "one cluster, and it is the winner");
    assert_eq!(remaining[0].id, a);
}

/// The winner's centroid is recomputed from the union, not inherited from one
/// side.
///
/// A merge that keeps the winner's centroid is the bug this guards: the
/// absorbed cluster's members are now members, and the next assignment
/// compares against a mean that excludes them.
#[tokio::test]
async fn merge_recomputes_the_centroid_from_the_union_of_members() {
    let (_d, store, _engine, a, b) = two_clusters().await;

    let centroid_before = centroid(&store, &a).await.expect("a has one");
    ops::merge(&store, &a, &b, None).await.unwrap();
    let centroid_after = centroid(&store, &a).await.expect("still has one");

    assert_ne!(
        centroid_after, centroid_before,
        "the centroid moved, so it was recomputed rather than inherited"
    );

    // And it is the mean of *both* members: the merged cluster is now closer to
    // the absorbed cluster's member than the old centroid was.
    let absorbed = Frame::from_pair(&Rng::new(71).unit(), 1.40);
    let old = distance(&absorbed, &centroid_before);
    let new = distance(&absorbed, &centroid_after);
    assert!(
        new < old,
        "an appearance from the absorbed side is nearer the new centroid: \\
         {new} vs {old}"
    );
}

/// The merge record is written before the reference changes, and names both
/// sides. Without it there is no way to undo an operator's mistake, and a
/// re-point is not reversible from the data that survives it.
#[tokio::test]
async fn a_merge_writes_a_record_naming_both_sides() {
    let (_d, store, _engine, a, b) = two_clusters().await;
    ops::merge(&store, &a, &b, Some("a1")).await.unwrap();

    let records = ops::merge_records(&store).await.unwrap();
    assert_eq!(records.len(), 1, "one merge, one record: {records:?}");
    let r = &records[0];
    assert_eq!(
        (r.winner.as_str(), r.loser.as_str()),
        (a.as_str(), b.as_str())
    );
    assert_eq!(
        r.actor.as_deref(),
        Some("a1"),
        "the record names who did it, which is the only way a merge is auditable"
    );
    assert!(!r.at.is_empty(), "and when");
}

/// A merge never silently loses an accepted edit.
///
/// stash-box#943: merges dropped edits. The fix is structural -- nothing holds
/// a tally to lose, so the value is re-derived from the edit set every time it
/// is read. This asserts the observable consequence: the winning side's value
/// survives, and the losing side's proposal survives too (it is re-pointed,
/// not deleted).
#[tokio::test]
async fn a_merge_preserves_the_accepted_value_on_both_sides() {
    let (_d, store, engine) = tuned(0.90, 0.7, 0.3).await;
    let base = Rng::new(72).unit();

    let seed = Frame::from_pair(&base, 0.05);
    for (name, delta) in [("a1", 0.0), ("b1", 1.40)] {
        object(&store, name).await;
        let v = if delta == 0.0 {
            seed.clone()
        } else {
            Frame::from_pair(&seed, delta)
        };
        let id = engine
            .assign(name, &v, None)
            .await
            .unwrap()
            .cluster_id
            .unwrap();
        // One proposal, one accepting vote, per cluster.
        let proposal = ops::propose(
            &store,
            "performer",
            &id,
            "name",
            &serde_json::json!(if name == "a1" { "Ada" } else { "Grace" }),
            "user",
            None,
        )
        .await
        .unwrap();
        ops::vote(&store, &proposal, "acct-1", 1.0).await.unwrap();
    }

    let clusters = cluster::all_with_members(&store).await.unwrap();
    assert_eq!(clusters.len(), 2);
    let (a, b) = (clusters[0].id.clone(), clusters[1].id.clone());

    let value_before = ops::accepted_value(&store, "performer", &a, "name")
        .await
        .unwrap();
    assert!(
        value_before.is_some(),
        "the fixture's accepted value is actually accepted, or this test is \\
         checking nothing"
    );

    ops::merge(&store, &a, &b, None).await.unwrap();

    assert_eq!(
        ops::accepted_value(&store, "performer", &a, "name")
            .await
            .unwrap(),
        value_before,
        "the winning side's accepted value survives the merge"
    );
    // The loser's proposal is re-pointed, not orphaned: the data a user typed
    // still exists and can still be shown to them.
    let losers = ops::proposals_for(&store, "performer", &b, "name")
        .await
        .unwrap();
    assert_eq!(
        losers.len(),
        1,
        "the absorbed cluster's proposal was re-pointed, not deleted -- losing \\
         it is exactly stash-box#943"
    );
}

/// Split moves the named appearances and recomputes the original's centroid.
#[tokio::test]
async fn split_moves_the_named_appearances_out() {
    let (_d, store, engine) = tuned(0.90, 0.7, 0.3).await;
    let base = Rng::new(73).unit();

    let mut ids = Vec::new();
    let first = Frame::from_pair(&base, 0.05);
    for (i, name) in ["s1", "s2", "s3"].iter().enumerate() {
        object(&store, name).await;
        let v = if i == 0 {
            first.clone()
        } else {
            Frame::from_pair(&first, 0.05)
        };
        let out = engine.assign(name, &v, None).await.unwrap();
        ids.push((out.appearance_id, out.cluster_id.unwrap()));
    }
    let original = ids[0].1.clone();
    assert!(
        ids.iter().all(|(_, c)| *c == original),
        "one cluster to start"
    );

    let moved: Vec<String> = ids[1..].iter().map(|(a, _)| a.clone()).collect();
    let report = ops::split(&store, &original, &moved).await.unwrap();
    assert_eq!(report.moved.len(), 2, "both named appearances moved");
    assert_ne!(report.new_cluster, original);
    assert_eq!(report.original, original);

    let clusters = cluster::all_with_members(&store).await.unwrap();
    assert_eq!(clusters.len(), 2, "the original and the new one");
    for (appearance, _) in &ids[1..] {
        let row = cluster::appearance(&store, appearance)
            .await
            .unwrap()
            .unwrap();
        assert_ne!(
            row.cluster_id.as_deref(),
            Some(original.as_str()),
            "the named appearance is no longer in the original"
        );
    }
}

/// The recomputed centroid is the ticket's point: without it the split does not
/// stick, because the next assignment joins the error straight back.
#[tokio::test]
async fn split_recomputes_the_originals_centroid() {
    let (_d, store, engine) = tuned(0.90, 0.7, 0.3).await;
    let base = Rng::new(74).unit();

    let mut appearances = Vec::new();
    // All three join one cluster, so the split has something to divide. The
    // distances are from the *first*, not from a shared origin: see the note
    // in `two_clusters`.
    let first = Frame::from_pair(&base, 0.05);
    for (name, delta) in [("s1", 0.0), ("s2", 0.20), ("s3", 0.22)] {
        object(&store, name).await;
        let out = engine
            .assign(
                name,
                &if delta == 0.0 {
                    first.clone()
                } else {
                    Frame::from_pair(&first, delta)
                },
                None,
            )
            .await
            .unwrap();
        appearances.push((out.appearance_id, out.cluster_id.unwrap()));
    }
    let original = appearances[0].1.clone();
    let before = centroid(&store, &original).await.unwrap();

    let moved: Vec<String> = appearances[1..].iter().map(|(a, _)| a.clone()).collect();
    let new_cluster = ops::split(&store, &original, &moved)
        .await
        .unwrap()
        .new_cluster;
    let after = centroid(&store, &original).await.unwrap();

    assert_ne!(after, before, "the original's centroid was recomputed");
    assert_eq!(
        after.len(),
        before.len(),
        "and it is still a vector of the right width"
    );

    // The specific error that prompted the ticket: the appearance that caused
    // the split is no longer inside the original's neighbourhood.
    let offender = Frame::from_pair(&Frame::from_pair(&base, 0.05), 0.20);
    assert!(
        distance(&offender, &after) > distance(&offender, &before),
        "the split appearance is further from the original than it was, so the \\
         original no longer claims it: {} vs {}",
        distance(&offender, &after),
        distance(&offender, &before)
    );

    // And the new cluster is the home of exactly what was moved.
    let new_centroid = centroid(&store, &new_cluster).await.unwrap();
    assert!(
        distance(&offender, &new_centroid) < distance(&offender, &after),
        "and nearer the new one, which is what makes the split stick"
    );
}

/// An empty split is refused: it would create an empty cluster, which is a
/// person-shaped row that is not a person.
#[tokio::test]
async fn a_refused_split_writes_nothing() {
    let (_d, store, engine) = tuned(0.90, 0.7, 0.3).await;
    let base = Rng::new(76).unit();
    let first = Frame::from_pair(&base, 0.05);
    for (i, name) in ["r1", "r2"].iter().enumerate() {
        object(&store, name).await;
        let v = if i == 0 {
            first.clone()
        } else {
            Frame::from_pair(&first, 0.05)
        };
        engine.assign(name, &v, None).await.unwrap();
    }
    let before = cluster::store::all_with_members(&store)
        .await
        .unwrap()
        .len();

    let id = cluster::store::all_with_members(&store).await.unwrap()[0]
        .id
        .clone();
    assert!(
        matches!(
            ops::split(&store, &id, &[]).await,
            Err(ClusterError::EmptySplit)
        ),
        "splitting nothing is refused"
    );
    assert_eq!(
        cluster::store::all_with_members(&store)
            .await
            .unwrap()
            .len(),
        before,
        "a refused split left no cluster behind -- the check has to come before \
         the insert, or a failed split accumulates empty person-shaped rows"
    );
}

#[tokio::test]
async fn splitting_nothing_is_refused() {
    let (_d, store, _engine, a, _b) = two_clusters().await;
    assert!(
        matches!(
            ops::split(&store, &a, &[]).await,
            Err(ClusterError::EmptySplit)
        ),
        "an empty split is a mistake, not a no-op"
    );
}

/// Aliases are rows, and a comma inside one does not split it.
///
/// stash-box#778 / stash#5033: the CSV round-trip split "Doe, Jane" into two
/// aliases and the data was lost. The test is cheap and the bug is real, so it
/// goes in.
#[tokio::test]
async fn an_alias_containing_a_comma_round_trips_as_one_alias() {
    let (_d, store, _engine) = tuned(0.90, 0.7, 0.3).await;
    let performer = ops::create_performer(&store, "Jane Doe").await.unwrap();

    ops::add_alias(&store, &performer, "Doe, Jane", AliasScope::Unscoped)
        .await
        .unwrap();

    let all = ops::aliases(&store, &performer).await.unwrap();
    let names: Vec<&str> = all.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["Doe, Jane"],
        "one alias containing a comma, not two aliases: {names:?}"
    );

    // The CSV form is where the bug bit, so round-trip that specifically.
    let csv = ops::aliases_to_csv(&store, &performer).await.unwrap();
    let back = ops::parse_aliases_csv(&csv);
    assert_eq!(
        back.len(),
        1,
        "the CSV round-trip did not split it: {csv:?}"
    );
    assert_eq!(back[0].name, "Doe, Jane");
}

/// Non-ASCII names never fail auto-tag (stash#2293).
#[tokio::test]
async fn a_non_ascii_name_auto_tags_successfully() {
    let (_d, store, _engine) = tuned(0.90, 0.7, 0.3).await;
    let performer = ops::create_performer(&store, "Björk Guðmundsdóttir")
        .await
        .unwrap();
    ops::add_alias(&store, &performer, "Björk", AliasScope::Unscoped)
        .await
        .unwrap();

    // The tag is derived from the name. It must exist, and it must round-trip
    // through storage intact -- the bug was an ASCII-only guard rejecting the
    // row outright.
    let tag = ops::auto_tag_for("Björk");
    ops::ensure_tag(&store, &tag).await.unwrap();
    assert_eq!(
        tag, "performer:björk",
        "the tag is a normalisation of the name, not an ASCII filter on it"
    );

    let stored = ops::tag_exists(&store, &tag).await.unwrap();
    assert!(
        stored,
        "and it was stored: the auto-tag is not a silent skip"
    );
}

/// The primary name is selectable (#610), and exactly one is primary.
#[tokio::test]
async fn exactly_one_alias_is_primary_and_it_can_be_chosen() {
    let (_d, store, _engine) = tuned(0.90, 0.7, 0.3).await;
    let performer = ops::create_performer(&store, "Ada Lovelace").await.unwrap();
    ops::add_alias(&store, &performer, "Ada L.", AliasScope::Unscoped)
        .await
        .unwrap();
    ops::add_alias(
        &store,
        &performer,
        "Countess of Lovelace",
        AliasScope::Unscoped,
    )
    .await
    .unwrap();

    let all = ops::aliases(&store, &performer).await.unwrap();
    let primaries: Vec<&Alias> = all.iter().filter(|a| a.is_primary).collect();
    assert_eq!(
        primaries.len(),
        1,
        "exactly one primary, chosen rather than whichever the sort returned \\
         first: {all:?}"
    );

    ops::set_primary(&store, &performer, "Ada L.")
        .await
        .unwrap();
    let all = ops::aliases(&store, &performer).await.unwrap();
    let primaries: Vec<&str> = all
        .iter()
        .filter(|a| a.is_primary)
        .map(|a| a.name.as_str())
        .collect();
    assert_eq!(
        primaries,
        vec!["Ada L."],
        "the chosen alias is the primary, and the previous one stepped down"
    );
}

/// A name that is already an alias is a *warning* (#726/#714), not a refusal.
///
/// In an amateur corpus a shared name is ordinary data. A hard error here would
/// reject real names; the platform allows the collision and flags it.
#[tokio::test]
async fn a_colliding_alias_is_allowed_and_flagged() {
    let (_d, store, _engine) = tuned(0.90, 0.7, 0.3).await;
    let one = ops::create_performer(&store, "Sam Smith").await.unwrap();
    let two = ops::create_performer(&store, "Samuel Smith").await.unwrap();

    ops::add_alias(&store, &one, "S. Smith", AliasScope::Unscoped)
        .await
        .unwrap();

    let outcome = ops::add_alias(&store, &two, "S. Smith", AliasScope::Unscoped)
        .await
        .unwrap();
    assert!(
        outcome.collision.is_some(),
        "the collision is reported, so the UI can show it"
    );
    assert!(
        outcome.added,
        "and the alias is still added: refusing here would reject real data"
    );

    // Which is the point: the two performers now both answer to it.
    let mut hits = ops::performers_for_alias(&store, "S. Smith").await.unwrap();
    hits.sort();
    let mut expected = vec![one, two];
    expected.sort();
    assert_eq!(
        hits, expected,
        "an ambiguous alias resolves to both, which is why it was flagged"
    );
}

/// Scoping is what makes a shared name a non-issue where it can be scoped.
#[tokio::test]
async fn a_scoped_alias_may_shadow_an_unscoped_one() {
    let (_d, store, _engine) = tuned(0.90, 0.7, 0.3).await;
    let producer = ops::create_producer(&store, "Studio X").await.unwrap();
    let one = ops::create_performer(&store, "Sam Smith").await.unwrap();
    let two = ops::create_performer(&store, "Samuel Smith").await.unwrap();

    ops::add_alias(&store, &one, "Sam", AliasScope::Unscoped)
        .await
        .unwrap();
    // The same string under a different studio is a different alias, so the
    // uniqueness constraint is not violated and nothing is flagged.
    let outcome = ops::add_alias(&store, &two, "Sam", AliasScope::Producer(producer.clone()))
        .await
        .unwrap();
    assert!(
        outcome.collision.is_none(),
        "a scoped alias is not a collision with an unscoped one: {outcome:?}"
    );

    let scoped = ops::aliases(&store, &two).await.unwrap();
    assert_eq!(scoped.len(), 1);
    assert_eq!(scoped[0].scope, AliasScope::Producer(producer.clone()));
}

/// `not_same_as` is a first-class assertion and automation only ever proposes.
#[tokio::test]
async fn not_same_as_prevents_a_future_automatic_merge() {
    let (_d, store, _engine, a, b) = two_clusters().await;

    ops::disambiguate(
        &store,
        Disambiguation::NotSameAs {
            left: a.clone(),
            right: b.clone(),
            note: None,
        },
    )
    .await
    .unwrap();

    assert!(ops::blocked_pair(&store, &a, &b).await.unwrap());
    assert!(
        !ops::blocked_pair(&store, &a, &a).await.unwrap(),
        "and the assertion is directional in storage but symmetric in meaning"
    );
}

/// A merge that contradicts a `not_same_as` is refused unless the operator says
/// so. An assertion a bulk operation can override silently is not an assertion.
#[tokio::test]
async fn merging_a_blocked_pair_requires_an_explicit_override() {
    let (_d, store, _engine, a, b) = two_clusters().await;
    ops::disambiguate(
        &store,
        Disambiguation::NotSameAs {
            left: a.clone(),
            right: b.clone(),
            note: Some("different people, verified".into()),
        },
    )
    .await
    .unwrap();

    let refused = ops::merge(&store, &a, &b, None).await;
    assert!(
        matches!(refused, Err(ClusterError::BlockedByAssertion { .. })),
        "the merge is refused, naming why: {refused:?}"
    );
    assert!(
        exists(&store, &a).await && exists(&store, &b).await,
        "and nothing was changed on the way to refusing it"
    );

    ops::merge(&store, &a, &b, Some("a1")).await.unwrap();
    assert!(
        !exists(&store, &b).await,
        "an explicit actor overrides the assertion -- the operator is the \\
         authority, and the record says who decided"
    );
}

/// Moving *every* appearance out is refused.
///
/// A split that empties the original is refused: it would leave a cluster with
/// no members, and an empty cluster is a person-shaped row that is not a person
/// -- invisible to every future assignment, so it can never be filled again and
/// never appears in a count that would explain it.
#[tokio::test]
async fn moving_every_appearance_is_refused() {
    let (_d, store, engine) = tuned(0.90, 0.7, 0.3).await;
    let base = Rng::new(75).unit();
    let first = Frame::from_pair(&base, 0.05);

    let mut appearances = Vec::new();
    for (i, name) in ["e1", "e2"].iter().enumerate() {
        object(&store, name).await;
        let v = if i == 0 {
            first.clone()
        } else {
            Frame::from_pair(&first, 0.05)
        };
        let out = engine.assign(name, &v, None).await.unwrap();
        appearances.push((out.appearance_id, out.cluster_id.unwrap()));
    }
    let original = appearances[0].1.clone();
    let all: Vec<String> = appearances.iter().map(|(a, _)| a.clone()).collect();

    assert!(
        matches!(
            ops::split(&store, &original, &all).await,
            Err(ClusterError::EmptySplit)
        ),
        "splitting every appearance out is refused: the original would be left \
         as a person-shaped row with no people in it"
    );
    assert!(
        exists(&store, &original).await,
        "and the original is still there, unchanged"
    );
    assert_eq!(
        cluster::store::all_with_members(&store)
            .await
            .unwrap()
            .len(),
        1,
        "no empty cluster was left behind either"
    );
}

/// A `blocked_pair` lookup is symmetric even when only one direction was asked
/// about.
///
/// `disambiguate` writes both directions, so a test that only asks about
/// `(a, b)` would pass against an implementation that checked one column and
/// never noticed. Asserting the reverse is what makes the storage redundant
/// rather than accidentally sufficient.
#[tokio::test]
async fn a_blocked_pair_reads_as_blocked_in_both_directions() {
    let (_d, store, _engine, a, b) = two_clusters().await;
    ops::disambiguate(
        &store,
        Disambiguation::NotSameAs {
            left: a.clone(),
            right: b.clone(),
            note: None,
        },
    )
    .await
    .unwrap();

    assert!(ops::blocked_pair(&store, &a, &b).await.unwrap());
    assert!(
        ops::blocked_pair(&store, &b, &a).await.unwrap(),
        "the reverse direction is blocked too: the assertion is a statement \
         about the pair, not about the order it was typed in"
    );
}

/// Exactly one primary, always — after a demotion as well as a promotion.
///
/// The failure this guards is a half-applied `set_primary`: two primaries is a
/// state the UI cannot render, because it has to pick one to show and there is
/// nothing to pick on. The first test proves a promotion happened; this proves
/// the old one stepped down.
#[tokio::test]
async fn choosing_a_new_primary_leaves_exactly_one() {
    let (_d, store, _engine) = tuned(0.90, 0.7, 0.3).await;
    let performer = ops::create_performer(&store, "Ada Lovelace").await.unwrap();
    for name in ["Ada L.", "Countess of Lovelace", "A. Lovelace"] {
        ops::add_alias(&store, &performer, name, AliasScope::Unscoped)
            .await
            .unwrap();
    }

    ops::set_primary(&store, &performer, "Ada L.")
        .await
        .unwrap();
    let primaries: Vec<String> = ops::aliases(&store, &performer)
        .await
        .unwrap()
        .into_iter()
        .filter(|a| a.is_primary)
        .map(|a| a.name)
        .collect();
    assert_eq!(
        primaries,
        vec!["Ada L.".to_string()],
        "exactly one primary after the change, not two"
    );

    // And once more, to catch a demotion that only works the first time.
    ops::set_primary(&store, &performer, "A. Lovelace")
        .await
        .unwrap();
    let primaries: Vec<String> = ops::aliases(&store, &performer)
        .await
        .unwrap()
        .into_iter()
        .filter(|a| a.is_primary)
        .map(|a| a.name)
        .collect();
    assert_eq!(
        primaries,
        vec!["A. Lovelace".to_string()],
        "and exactly one after a second change"
    );
}

/// Choosing an alias the performer does not have is refused, and changes
/// nothing.
///
/// Without this a typo would demote the existing primary and then fail to
/// promote anything, leaving a performer with no primary at all.
#[tokio::test]
async fn choosing_an_unknown_alias_changes_nothing() {
    let (_d, store, _engine) = tuned(0.90, 0.7, 0.3).await;
    let performer = ops::create_performer(&store, "Ada Lovelace").await.unwrap();
    ops::add_alias(&store, &performer, "Ada L.", AliasScope::Unscoped)
        .await
        .unwrap();
    let before = ops::aliases(&store, &performer).await.unwrap();

    assert!(ops::set_primary(&store, &performer, "Not An Alias")
        .await
        .is_err());
    assert_eq!(
        ops::aliases(&store, &performer).await.unwrap(),
        before,
        "a failed set_primary is a no-op, not a demotion with nothing to replace it"
    );
}

/// A self-referential assertion is refused.
///
/// `not_same_as(a, a)` would be a row that blocks a from merging with itself,
/// and the engine's own `SelfMerge` check would then refuse a merge that is
/// already impossible. It is a mistake, and a named one.
#[tokio::test]
async fn an_assertion_against_itself_is_refused() {
    let (_d, store, _engine, a, _b) = two_clusters().await;
    assert!(
        matches!(
            ops::disambiguate(
                &store,
                Disambiguation::NotSameAs {
                    left: a.clone(),
                    right: a.clone(),
                    note: None,
                },
            )
            .await,
            Err(ClusterError::SelfMerge)
        ),
        "a cluster cannot be asserted against itself"
    );
    assert!(
        !ops::blocked_pair(&store, &a, &a).await.unwrap(),
        "and the failed assertion left nothing behind"
    );
}

/// The scope is what decides whether two aliases collide, in both directions.
///
/// The first direction is the ordinary case: a studio's name and the person's
/// own name are different aliases that share a spelling. The reverse is the one
/// a one-sided test would miss -- an unscoped alias added *after* a scoped one of
/// the same string must also be unflagged.
#[tokio::test]
async fn a_collision_is_only_reported_within_the_same_scope() {
    let (_d, store, _engine) = tuned(0.90, 0.7, 0.3).await;
    let producer = ops::create_producer(&store, "Studio X").await.unwrap();
    let one = ops::create_performer(&store, "Sam Smith").await.unwrap();
    let two = ops::create_performer(&store, "Samuel Smith").await.unwrap();

    // Scoped first, then unscoped with the same string.
    let scoped = ops::add_alias(&store, &one, "Sam", AliasScope::Producer(producer.clone()))
        .await
        .unwrap();
    assert!(scoped.collision.is_none(), "nothing to collide with yet");

    let unscoped = ops::add_alias(&store, &two, "Sam", AliasScope::Unscoped)
        .await
        .unwrap();
    assert!(
        unscoped.collision.is_none(),
        "a studio's scoped 'Sam' and an unscoped 'Sam' are different aliases: \
         {unscoped:?}"
    );

    // Now a genuine collision: same scope, different performer.
    let three = ops::create_performer(&store, "Sammy Smith").await.unwrap();
    let real = ops::add_alias(&store, &three, "Sam", AliasScope::Unscoped)
        .await
        .unwrap();
    assert!(
        real.collision.is_some(),
        "two unscoped aliases of the same string *are* a collision, so the \
         scope check above is not simply never flagging anything"
    );
    assert_eq!(real.collision.unwrap(), vec![two.clone()]);

    // And two scoped-to-the-same-studio aliases do collide.
    let four = ops::create_performer(&store, "Samson Smith").await.unwrap();
    let same_studio = ops::add_alias(&store, &four, "Sam", AliasScope::Producer(producer))
        .await
        .unwrap();
    assert_eq!(
        same_studio.collision,
        Some(vec![one.clone()]),
        "same studio, same string, different performer: a real collision"
    );
}

/// Splitting an appearance that belongs to a *different* cluster is refused.
///
/// The check is what stops a split from quietly reaching across clusters. An
/// appearance already in cluster B, named in a split of cluster A, would either
/// be silently ignored (leaving the operator believing it moved) or moved out of
/// B as a side effect of an operation they aimed at A. Both are worse than a
/// refusal, and the refusal has to name which appearance was wrong.
#[tokio::test]
async fn splitting_an_appearance_from_another_cluster_is_refused() {
    let (_d, store, _engine, a, b) = two_clusters().await;

    // `b`'s appearance, named in a split of `a`.
    let foreign = cluster::store::all_appearances(&store)
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.cluster_id.as_deref() == Some(b.as_str()))
        .expect("the fixture's second cluster has an appearance")
        .id;

    let err = ops::split(&store, &a, std::slice::from_ref(&foreign)).await;
    assert!(
        matches!(err, Err(ClusterError::NoSuchAppearance(ref id)) if *id == foreign),
        "the refusal names the appearance, so the operator can see which one was \
         wrong: {err:?}"
    );

    // And nothing moved: the foreign appearance is still in `b`, and `b` still
    // exists with its member.
    let row = cluster::store::appearance(&store, &foreign)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        row.cluster_id.as_deref(),
        Some(b.as_str()),
        "a refused cross-cluster split did not move anything"
    );
    assert_eq!(
        cluster::store::all_with_members(&store)
            .await
            .unwrap()
            .len(),
        2,
        "and created no cluster on the way to refusing"
    );
}
