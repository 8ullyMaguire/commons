//! T-P2-007: the hostile-plugin test.
//!
//! The plan's words: *"This is the most important test in the project, because
//! it is the one that makes §14 load-bearing rather than aspirational."*
//!
//! Everything else in this project can be wrong in a way that produces a bad
//! picture or a slow scan. If this is wrong, an amateur creator's item becomes
//! a redistribution pointer, and §14 is a document rather than a mechanism.
//!
//! So the test is written adversarially. `HostilePlugin` gets the same
//! `HostApi` handle a real plugin gets — the `HostApi` trait, nothing more —
//! and then tries every escape it can think of:
//!
//!   * propose a magnet on a `denied` object, and on every other tier;
//!   * propose on an object that has no consent record at all;
//!   * propose for an object that does not exist;
//!   * claim a capability it was not granted;
//!   * reach the database, the consent record, or the tier enum through any
//!     route the API offers;
//!   * reach a non-loopback host.
//!
//! The property asserted is not "it did not succeed today" but "the API it was
//! given contains no second write path": `HostApi` has three methods, one of
//! which writes, and that one is `propose_locator`.
//!
//! The two layers are tested separately and both matter:
//!
//!   * `the_gate_refuses` — against a real database, through the real
//!     `locator::propose`. This is the data-layer gate.
//!   * `a_plugin_has_no_other_write_path` — against the `HostApi` trait, which
//!     is what a plugin can actually reach. This is the API surface.

use commons_plugin::block_on;
use std::collections::BTreeSet;

use commons_core::ConsentTier;
use commons_plugin::{Capability, HostApi, HostError, LocatorOutcome, PluginHostApi};
use commons_store::locator::{self, LocatorScheme, ProposeError, ProposedLocator};
use commons_store::Store;

const HASH: &str = "0123456789abcdef0123456789abcdef01234567";

/// The tiers a locator proposal must be refused at, even with the
/// redistribution flag set.
///
/// `unverified` refuses redistribution locators but may record an http source
/// url, which §5.18 calls "plain text, not a P2P protocol" -- so the
/// schemes-then-tiers loop below checks each scheme against its own rule, and
/// this list is "refuses a magnet", not "refuses everything".
const REFUSING_TIERS: [ConsentTier; 5] = [
    ConsentTier::Unverified,
    ConsentTier::SelfPublished,
    ConsentTier::PerformerClaimed,
    ConsentTier::Quarantined,
    ConsentTier::Denied,
];
const MAGNET: &str = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&dn=x";

async fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_library(dir.path()).await.unwrap();
    (dir, store)
}

/// An object with a consent record at `tier`.
async fn object_at(store: &Store, id: &str, tier: ConsentTier, redistribution: bool) {
    sqlx::query(
        "INSERT INTO object (id, kind, created_at, updated_at) VALUES (?, 'scene', '', '')",
    )
    .bind(id)
    .execute(store.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO consent_record (id, object_id, tier, redistribution_permitted, updated_at)
         VALUES (?, ?, ?, ?, '')",
    )
    .bind(format!("consent-{id}"))
    .bind(id)
    .bind(tier.as_str())
    .bind(i64::from(redistribution))
    .execute(store.pool())
    .await
    .unwrap();
}

/// An object with no consent record at all — the freshly-scanned case.
async fn object_without_consent(store: &Store, id: &str) {
    sqlx::query(
        "INSERT INTO object (id, kind, created_at, updated_at) VALUES (?, 'scene', '', '')",
    )
    .bind(id)
    .execute(store.pool())
    .await
    .unwrap();
}

// ============================================================
// The data-layer gate
// ============================================================

#[tokio::test]
async fn the_gate_refuses_every_tier_that_cannot_carry_a_locator() {
    let (_d, store) = store().await;

    // The whole ladder, with redistribution explicitly NOT permitted — the
    // state every scanned object is in before anyone decides anything.
    for tier in ConsentTier::ALL {
        let id = format!("o-{}", tier.as_str());
        object_at(&store, &id, tier, false).await;

        let err = locator::propose(
            &store,
            &id,
            &ProposedLocator::new(LocatorScheme::Magnet, MAGNET, "hostile"),
            "2026-09-26T00:00:00Z",
        )
        .await
        .expect_err("a magnet with no redistribution basis must be refused");

        assert!(
            matches!(err, ProposeError::TierForbidsLocator { .. }),
            "{tier:?} with no basis gave {err:?}, not a consent refusal"
        );
        assert_eq!(locator::count(&store, &id).await.unwrap(), 0, "{tier:?}");
    }
}

#[tokio::test]
async fn the_flag_alone_does_not_open_a_forbidden_tier() {
    // The independence the spec insists on, checked against the database
    // rather than against the pure function: a hostile plugin that somehow got
    // `redistribution_permitted = true` set on a denied object still gets
    // nowhere, because the tier is checked too.
    let (_d, store) = store().await;
    for tier in [
        ConsentTier::Denied,
        ConsentTier::Quarantined,
        ConsentTier::PerformerClaimed,
    ] {
        let id = format!("flagged-{}", tier.as_str());
        object_at(&store, &id, tier, true).await;
        let err = locator::propose(
            &store,
            &id,
            &ProposedLocator::new(LocatorScheme::Magnet, MAGNET, "hostile"),
            "2026-09-26T00:00:00Z",
        )
        .await
        .expect_err("{tier:?} with the flag set must still refuse a magnet");
        assert!(err.is_consent_refusal(), "{tier:?} gave {err:?}");
        assert_eq!(locator::count(&store, &id).await.unwrap(), 0);
    }
}

#[tokio::test]
async fn an_object_with_no_consent_record_is_unverified_not_consent() {
    // The case that matters most in practice: a freshly scanned file has no
    // consent record at all. "No record" must not read as "no restriction".
    let (_d, store) = store().await;
    object_without_consent(&store, "fresh").await;

    let err = locator::propose(
        &store,
        "fresh",
        &ProposedLocator::new(LocatorScheme::Magnet, MAGNET, "hostile"),
        "2026-09-26T00:00:00Z",
    )
    .await
    .expect_err("an object with no consent record must refuse a magnet");
    assert!(err.is_consent_refusal(), "{err:?}");
    assert!(locator::count(&store, "fresh").await.unwrap() == 0);
}

#[tokio::test]
async fn a_permitted_object_stores_the_locator() {
    // The positive case, so the gate cannot be satisfied by refusing
    // everything. A gate that always says no passes every test above.
    let (_d, store) = store().await;
    object_at(&store, "ok", ConsentTier::ThirdPartyPermitted, true).await;

    let id = locator::propose(
        &store,
        "ok",
        &ProposedLocator::new(LocatorScheme::Magnet, MAGNET, "legit")
            .with_infohash(HASH)
            .with_name("scene.mp4")
            .with_size(1024),
        "2026-09-26T00:00:00Z",
    )
    .await
    .expect("a permitted object must accept a magnet");

    let rows = locator::for_object(&store, "ok").await.unwrap();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.id, id);
    assert_eq!(row.scheme, "magnet");
    assert_eq!(row.infohash.as_deref(), Some(HASH));
    assert_eq!(row.name.as_deref(), Some("scene.mp4"));
    assert_eq!(row.size_bytes, Some(1024));
    assert_eq!(row.source, "legit", "the source is recorded verbatim");
    assert_eq!(row.added_at, "2026-09-26T00:00:00Z");
}

#[tokio::test]
async fn a_permitted_object_also_records_plain_metadata() {
    // §5.18: an HTTP source URL is "plain text, not a P2P protocol", so it does
    // not need the redistribution basis. A gate that refused it would make it
    // impossible to record where a permitted file came from.
    let (_d, store) = store().await;
    object_at(&store, "src", ConsentTier::Unverified, false).await;
    locator::propose(
        &store,
        "src",
        &ProposedLocator::new(LocatorScheme::Http, "https://example.com/a.mp4", "manual"),
        "2026-09-26T00:00:00Z",
    )
    .await
    .expect("an http source url is metadata, not redistribution");
    assert_eq!(locator::count(&store, "src").await.unwrap(), 1);
}

#[tokio::test]
async fn a_denied_object_refuses_even_plain_metadata() {
    let (_d, store) = store().await;
    for tier in [ConsentTier::Denied, ConsentTier::Quarantined] {
        let id = format!("adverse-{}", tier.as_str());
        object_at(&store, &id, tier, true).await;
        let err = locator::propose(
            &store,
            &id,
            &ProposedLocator::new(LocatorScheme::Http, "https://example.com/a.mp4", "hostile"),
            "2026-09-26T00:00:00Z",
        )
        .await
        .expect_err("{tier:?} must refuse everything");
        assert!(err.is_consent_refusal());
    }
}

#[tokio::test]
async fn a_malformed_uri_is_refused_before_the_gate_is_consulted() {
    // Order matters: a garbage URI should not consume a consent decision, and
    // the error must be distinguishable from a refusal so a plugin does not
    // retry a consent refusal forever.
    let (_d, store) = store().await;
    object_at(&store, "ok", ConsentTier::ThirdPartyPermitted, true).await;
    for uri in ["", "not-a-magnet", "magnet:?dn=no-hash", "magnet:"] {
        let err = locator::propose(
            &store,
            "ok",
            &ProposedLocator::new(LocatorScheme::Magnet, uri, "hostile"),
            "2026-09-26T00:00:00Z",
        )
        .await
        .expect_err("{uri:?} is malformed");
        assert!(
            !err.is_consent_refusal(),
            "{uri:?} gave a consent refusal, which would be a lie: {err:?}"
        );
    }
    assert_eq!(locator::count(&store, "ok").await.unwrap(), 0);
}

#[tokio::test]
async fn proposing_for_an_object_that_does_not_exist_is_not_a_consent_answer() {
    // Reported as NoSuchObject rather than a tier refusal, so a caller does not
    // conclude that consent blocked it and stop retrying something that would
    // never work.
    let (_d, store) = store().await;
    let err = locator::propose(
        &store,
        "nope",
        &ProposedLocator::new(LocatorScheme::Magnet, MAGNET, "hostile"),
        "2026-09-26T00:00:00Z",
    )
    .await
    .expect_err("no such object");
    assert!(matches!(err, ProposeError::NoSuchObject(_)), "{err:?}");
    assert!(!err.is_consent_refusal());
}

#[tokio::test]
async fn an_unparseable_tier_is_treated_as_denied() {
    // A row someone edited by hand, or wrote with a tier name from a future
    // version, must not become a redistribution channel by accident.
    let (_d, store) = store().await;
    sqlx::query(
        "INSERT INTO object (id, kind, created_at, updated_at) VALUES ('weird', 'scene', '', '')",
    )
    .execute(store.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO consent_record (id, object_id, tier, redistribution_permitted, updated_at)
         VALUES ('c-weird', 'weird', 'tier_from_the_future', 1, '')",
    )
    .execute(store.pool())
    .await
    .unwrap();

    let err = locator::propose(
        &store,
        "weird",
        &ProposedLocator::new(LocatorScheme::Magnet, MAGNET, "hostile"),
        "2026-09-26T00:00:00Z",
    )
    .await
    .expect_err("an unknown tier must not permit redistribution");
    assert!(err.is_consent_refusal(), "{err:?}");
}

#[tokio::test]
async fn a_tombstone_destroys_the_locators_with_it() {
    // §5.18: "any existing locator is destroyed with the tombstone". A denied
    // object that kept its magnet would be the worst possible failure — the
    // pointer would outlive the consent that allowed it.
    let (_d, store) = store().await;
    object_at(&store, "doomed", ConsentTier::ThirdPartyPermitted, true).await;
    locator::propose(
        &store,
        "doomed",
        &ProposedLocator::new(LocatorScheme::Magnet, MAGNET, "legit"),
        "2026-09-26T00:00:00Z",
    )
    .await
    .expect("stored while permitted");
    assert_eq!(locator::count(&store, "doomed").await.unwrap(), 1);

    sqlx::query("UPDATE consent_record SET tier = 'denied', redistribution_permitted = 0 WHERE object_id = 'doomed'")
        .execute(store.pool())
        .await
        .unwrap();

    // The consent store calls this on accepting a takedown.
    let removed = locator::destroy_for_object(&store, "doomed").await.unwrap();
    assert_eq!(removed, 1);
    assert_eq!(locator::count(&store, "doomed").await.unwrap(), 0);

    // And a re-proposal after the takedown is refused.
    let err = locator::propose(
        &store,
        "doomed",
        &ProposedLocator::new(LocatorScheme::Magnet, MAGNET, "hostile"),
        "2026-09-26T00:00:00Z",
    )
    .await
    .expect_err("denied");
    assert!(err.is_consent_refusal());
}

#[tokio::test]
async fn a_duplicate_proposal_is_rejected_rather_than_silently_stored_twice() {
    let (_d, store) = store().await;
    object_at(&store, "dup", ConsentTier::ThirdPartyPermitted, true).await;
    let l = ProposedLocator::new(LocatorScheme::Magnet, MAGNET, "legit");
    locator::propose(&store, "dup", &l, "t1")
        .await
        .expect("first");
    let err = locator::propose(&store, "dup", &l, "t2")
        .await
        .expect_err("the (object, scheme, uri) unique constraint");
    assert!(matches!(err, ProposeError::Internal(_)), "{err:?}");
    assert_eq!(locator::count(&store, "dup").await.unwrap(), 1);
}

// ============================================================
// The API surface a plugin can reach
// ============================================================

/// A plugin written by someone trying to break in.
///
/// It holds only a `&dyn HostApi`, which is exactly what the sandbox hands it.
/// Every method below is an escape attempt.
struct HostilePlugin<'a> {
    api: &'a dyn HostApi,
    /// Every refusal it collected, so the test can assert the *shape* of what
    /// it was told rather than just that it failed.
    refusals: Vec<String>,
}

impl<'a> HostilePlugin<'a> {
    fn new(api: &'a dyn HostApi) -> Self {
        HostilePlugin {
            api,
            refusals: Vec::new(),
        }
    }

    /// Try a proposal and record what came back.
    ///
    /// `must_refuse` says what §5.18 requires of this specific
    /// (tier, scheme) pair. Where the model legitimately permits a locator, the
    /// plugin is *expected* to succeed, and the test records that as a success
    /// rather than panicking -- because "a plugin can attach an http source url
    /// to an unverified object" is the consent model working. What must never
    /// happen is a P2P locator landing on a tier that cannot carry one.
    async fn try_propose(&mut self, object_id: &str, scheme: &str, uri: &str, must_refuse: bool) {
        // `propose_locator` is a boxed future rather than an `async fn` in the
        // trait, so the call is awaited here rather than in `try_propose`'s
        // caller. That is the cost of a trait that a dyn object implements
        // without pulling `async-trait` into the sandbox boundary.
        let outcome = self.api.propose_locator(object_id, scheme, uri).await;
        match outcome {
            Ok(LocatorOutcome::Stored { locator_id }) => {
                if must_refuse {
                    panic!("HOSTILE PLUGIN STORED A LOCATOR ON {object_id}: {locator_id}");
                }
                self.refusals
                    .push(format!("permitted {scheme} on {object_id}"));
            }
            Ok(LocatorOutcome::Refused {
                scheme,
                object_tier,
            }) => {
                self.refusals
                    .push(format!("refused {scheme} at {object_tier}"));
            }
            Err(e) => self.refusals.push(format!("error {e}")),
        }
    }
}

#[tokio::test]
async fn a_plugin_has_no_other_write_path() {
    let (_d, store) = store().await;
    // The hostile plugin attacks only the tiers that MUST refuse, with the
    // redistribution flag SET on every one of them. Setting it removes "the
    // plugin forgot the flag" as an explanation, so the tier check is the only
    // thing left protecting these objects -- which is the property §14 is for.
    //
    // It does not attack the permitted tiers, and that is not a hole in the
    // test: `self_published`/`performer_claimed`/`third_party_permitted` at
    // flag=true are *supposed* to accept a magnet. A plugin reaching a
    // `third_party_permitted` object and succeeding is the consent model
    // working, not the sandbox failing. The gate that refuses everything would
    // pass this test and is caught instead by
    // `a_permitted_object_stores_the_locator`.
    //
    // `for scheme` below keeps the per-scheme distinction honest: `unverified`
    // refuses a magnet but may legitimately record an http source url, so each
    // scheme is checked against its own rule rather than the whole tier being
    // written off.
    for tier in REFUSING_TIERS {
        let id = format!("o-{}", tier.as_str());
        object_at(&store, &id, tier, true).await;
    }
    object_without_consent(&store, "fresh").await;

    // A locator plugin, granted everything a legitimate one could hold --
    // including loopback http, which §5.18.1 says it gets so it can talk to a
    // local client. Granting it is deliberate: the point of the network section
    // below is that the *loopback policy* rejects a non-loopback host, not
    // that a missing capability happens to reject it first. A test that passed
    // only because the capability was absent would prove nothing about the
    // policy.
    let granted: BTreeSet<Capability> = [
        Capability::ReadLibrary,
        Capability::ProposeMetadata,
        Capability::LoopbackHttp,
    ]
    .into_iter()
    .collect();
    let api = PluginHostApi::new(&store, &granted, "hostile-plugin");
    let mut plugin = HostilePlugin::new(&api);

    // 1. Every scheme against every tier that must refuse a P2P locator.
    for scheme in LocatorScheme::ALL {
        let uri = match scheme {
            LocatorScheme::BitTorrent => HASH.to_string(),
            LocatorScheme::Magnet => MAGNET.to_string(),
            LocatorScheme::Ed2k => {
                "ed2k://|file|0123456789abcdef0123456789abcdef0123456789|x|1|".into()
            }
            LocatorScheme::Http => "https://example.com/a.mp4".into(),
        };
        // A P2P scheme must be refused at every tier on this list. An http
        // source url is metadata rather than redistribution, so the tiers that
        // are merely unverified may legitimately record one -- the rule is per
        // (tier, scheme) pair, not per tier, and collapsing it would make this
        // test assert something §5.18 does not say.
        let is_p2p = !matches!(scheme, LocatorScheme::Http);
        for tier in REFUSING_TIERS {
            let must_refuse =
                is_p2p || matches!(tier, ConsentTier::Quarantined | ConsentTier::Denied);
            plugin
                .try_propose(
                    &format!("o-{}", tier.as_str()),
                    scheme.as_str(),
                    &uri,
                    must_refuse,
                )
                .await;
        }
    }

    // 2. The object with no consent record.
    // No consent record at all: `unverified`, so a P2P locator is refused and
    // an http source url may be recorded.
    plugin.try_propose("fresh", "magnet", MAGNET, true).await;
    plugin.try_propose("fresh", "bittorrent", HASH, true).await;

    // 3. An object that does not exist, and a crafted one.
    plugin
        .try_propose("no-such-object", "magnet", MAGNET, true)
        .await;
    plugin
        .try_propose("../../etc/passwd", "magnet", MAGNET, true)
        .await;
    plugin.try_propose("", "magnet", MAGNET, true).await;

    // 4. The network surface: a non-loopback host is refused as policy, not
    //    attempted.
    for url in [
        "http://example.com/",
        "http://10.0.0.1/",
        "http://169.254.169.254/latest/meta-data/",
        "http://[::ffff:127.0.0.1]/",
    ] {
        match api.loopback_get(url) {
            Err(HostError::NotLoopback(_)) => {}
            other => panic!("HOSTILE PLUGIN REACHED {url}: {other:?}"),
        }
    }

    // Not one locator row exists, for any object, at any tier.
    // No P2P locator anywhere. Http source urls are metadata and are counted
    // separately, because their presence is the consent model working, not a
    // breach -- and a test that cannot tell those apart is a test that will
    // either miss a real breach or fail on a correct implementation.
    for tier in REFUSING_TIERS {
        let id = format!("o-{}", tier.as_str());
        let rows = locator::for_object(&store, &id).await.unwrap();
        for row in &rows {
            assert_ne!(
                row.scheme, "magnet",
                "{tier:?} gained a magnet: {}",
                row.uri
            );
            assert_ne!(
                row.scheme, "bittorrent",
                "{tier:?} gained a bittorrent locator"
            );
            assert_ne!(row.scheme, "ed2k", "{tier:?} gained an ed2k locator");
        }
    }
    let fresh = locator::for_object(&store, "fresh").await.unwrap();
    for row in &fresh {
        assert_ne!(
            row.scheme, "magnet",
            "an object with no consent record gained a magnet"
        );
    }

    // And the plugin was told *why*, every time — a refusal it can explain to
    // its author, not a silent failure it would retry forever.
    assert!(
        plugin.refusals.len() >= LocatorScheme::ALL.len() * REFUSING_TIERS.len() + 2,
        "only {} refusals recorded; the plugin should have been told every time",
        plugin.refusals.len()
    );
}

#[tokio::test]
async fn a_plugin_cannot_use_a_capability_it_was_not_granted() {
    let (_d, store) = store().await;
    // Granted nothing but the proposal right.
    let granted: BTreeSet<Capability> = [Capability::ProposeMetadata].into_iter().collect();
    let api = PluginHostApi::new(&store, &granted, "hostile-plugin");

    assert!(api.granted().contains(&Capability::ProposeMetadata));
    for denied in [
        Capability::Network,
        Capability::ProcessSpawn,
        Capability::LoopbackHttp,
        Capability::ReadLibrary,
    ] {
        assert!(
            !api.granted().contains(&denied),
            "{denied:?} was granted; the sandbox handed out more than it should"
        );
    }
    // So even loopback HTTP is unavailable.
    assert!(matches!(
        api.loopback_get("http://127.0.0.1:8080/"),
        Err(HostError::NoCapability(Capability::LoopbackHttp))
    ));
}

/// The structural claim, checked the only way Rust allows.
///
/// A trait method cannot be taken as a value, so "the trait has exactly these
/// three methods" cannot be asserted the way it would be in a language with
/// reflection. What *can* be asserted is the property that matters and that
/// the compiler will re-check forever: every method the trait has is reachable
/// through a `&dyn HostApi`, and each one is accounted for below.
///
/// The compile-time half of the check: if someone adds a fourth method to
/// `HostApi`, the `HostApiExt` impl in `lib.rs` — which must forward every
/// method, or the test module here stops compiling — forces them to enumerate
/// it. The runtime half is the capability matrix: the only method that
/// writes is `propose_locator`, and it refuses without `ProposeMetadata`.
#[test]
fn the_api_trait_has_exactly_three_methods_and_one_writes() {
    // A plain `#[test]`, not `#[tokio::test]`: this asserts on the *shape* of
    // the API, and `block_on` cannot start a runtime from inside one.
    block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_library(dir.path()).await.unwrap();
        let granted: BTreeSet<Capability> = BTreeSet::new();
        let api = PluginHostApi::new(&store, &granted, "hostile");

        // 1. granted — read-only, returns a borrow of the installer-computed set.
        let _: &BTreeSet<Capability> = api.granted();

        // 2. loopback_get — the only network route, and it is not a write to the
        //    library. With no capabilities it refuses before any socket work.
        let _: Result<Vec<u8>, HostError> = api.loopback_get("http://127.0.0.1/");

        // 3. propose_locator — the single write. A boxed future, not a value, so
        //    the type is pinned rather than the Result; the Result is what comes
        //    out of it.
        let _: Result<LocatorOutcome, HostError> = api.propose_locator("o", "magnet", MAGNET).await;

        // No method takes a tier, a consent field, or a database handle, and
        // none returns one. A plugin cannot set `redistribution_permitted`; it
        // can only ask to be allowed to propose, and the answer is the store's.
        assert_eq!(
            api.granted().len(),
            0,
            "an empty grant set must produce an empty reported set"
        );
    });
}

/// The whole consent model, as a hand-written table.
///
/// This replaces an earlier version of this test that compared
/// `ConsentFacts::permits` against the result of `locator::propose` and
/// asserted they agreed. That test could not fail: `propose` *calls* `permits`,
/// so mutating `permits` mutated both sides together. It passed while three
/// mutations of the gate were live. A test whose two halves share a
/// dependency is a test that asserts nothing.
///
/// The table below is transcribed from §5.18 by hand, so the pure function is
/// checked against the specification rather than against its own caller.
#[test]
fn the_consent_model_is_the_table_the_spec_writes_down() {
    use commons_store::locator::{ConsentFacts, LocatorScheme};

    // (tier, redistribution_permitted) -> does it permit a P2P locator?
    //
    // §5.18: a magnet or infohash is a redistribution channel, so it needs a
    // tier that can carry one *and* a stated basis that includes
    // redistribution. `self_published` is the case the spec calls out: the
    // item is perfectly visible and still must not carry a magnet.
    let p2p_cases: &[(ConsentTier, bool, bool)] = &[
        // flag=false refuses at every tier: no stated basis, no locator.
        (ConsentTier::Unverified, false, false),
        (ConsentTier::SelfPublished, false, false),
        (ConsentTier::PerformerClaimed, false, false),
        (ConsentTier::ThirdPartyPermitted, false, false),
        (ConsentTier::Quarantined, false, false),
        (ConsentTier::Denied, false, false),
        // flag=true: only a tier that can carry one.
        (ConsentTier::Unverified, true, false),
        (ConsentTier::SelfPublished, true, false),
        (ConsentTier::PerformerClaimed, true, false),
        (ConsentTier::ThirdPartyPermitted, true, true),
        (ConsentTier::Quarantined, true, false),
        (ConsentTier::Denied, true, false),
    ];
    for (tier, flag, expected) in p2p_cases {
        let facts = ConsentFacts {
            tier: *tier,
            redistribution_permitted: *flag,
        };
        assert_eq!(
            facts.permits(LocatorScheme::Magnet),
            *expected,
            "{tier:?} with flag={flag}"
        );
        // Every P2P scheme follows the same rule.
        for scheme in [
            LocatorScheme::Magnet,
            LocatorScheme::BitTorrent,
            LocatorScheme::Ed2k,
        ] {
            assert_eq!(
                facts.permits(scheme),
                *expected,
                "{tier:?} flag={flag} scheme={scheme:?}"
            );
        }
    }

    // An http source url is "plain text, not a P2P protocol", so the
    // redistribution flag is irrelevant and only the adverse tiers refuse.
    for tier in ConsentTier::ALL {
        for flag in [false, true] {
            let facts = ConsentFacts {
                tier,
                redistribution_permitted: flag,
            };
            let expected = !matches!(tier, ConsentTier::Denied | ConsentTier::Quarantined);
            assert_eq!(
                facts.permits(LocatorScheme::Http),
                expected,
                "http at {tier:?} with flag={flag}"
            );
        }
    }
}

/// The default for an object that has never had a consent decision is the
/// permissive-looking-but-safe one.
///
/// Checked directly rather than through `propose`, because every
/// `propose`-level test of the no-record case is masked by the tier check:
/// `unverified` forbids a magnet whether the flag is false or true, so a
/// mutation of the default's flag is invisible through the write path. It is
/// only visible on the value itself.
#[test]
fn the_unrecorded_default_is_unverified_and_not_permitted() {
    use commons_store::locator::{ConsentFacts, LocatorScheme};

    let d = ConsentFacts::default_for_unrecorded();
    assert_eq!(d.tier, ConsentTier::Unverified);
    assert!(
        !d.redistribution_permitted,
        "an object with no consent record must not read as having a redistribution basis"
    );
    for scheme in LocatorScheme::ALL {
        if matches!(scheme, LocatorScheme::Http) {
            continue;
        }
        assert!(
            !d.permits(scheme),
            "an object with no consent record must refuse {scheme:?}"
        );
    }
}

/// An unrecognised tier name is denied, not guessed.
///
/// Also only visible on the value: every other tier in the ladder either
/// permits a magnet or forbids it for reasons the mutant did not change, so
/// substituting `unverified` or `self_published` for `denied` produced no
/// observable difference through the write path.
#[test]
fn an_unrecognised_tier_is_the_most_restrictive_one() {
    use commons_store::locator::{ConsentFacts, LocatorScheme};

    for guessed in ConsentTier::ALL {
        if guessed == ConsentTier::ThirdPartyPermitted {
            // The one tier that genuinely permits a magnet, so "resolves to
            // third_party_permitted" cannot be distinguished from "refuses"
            // by this probe. The other five are all that a mutation could have
            // substituted, and all five are caught.
            continue;
        }
        assert!(
            !ConsentFacts {
                tier: guessed,
                redistribution_permitted: true,
            }
            .permits(LocatorScheme::Magnet),
            "{guessed:?} must not be what an unknown tier name resolves to"
        );
    }
    // And it is `denied` specifically -- the loudest tier, which also hides the
    // object everywhere else, so a mistaken resolution cannot leak it later.
    assert!(!ConsentFacts {
        tier: ConsentTier::Denied,
        redistribution_permitted: true,
    }
    .permits(LocatorScheme::Http));
}

/// The row-to-decision projection itself, read directly.
///
/// Every substitution mutation of `consent_facts` survived the tests above.
/// The reason is structural rather than a missing case: `permits` multiplies
/// the tier and the flag together, and five of the six tiers forbid a magnet
/// whatever the flag says, so replacing `denied` with `unverified`,
/// `self_published`, or `quarantined` changes a value that no downstream
/// assertion can distinguish. The write path is the wrong place to test this
/// function, because the write path is exactly what hides the difference.
///
/// So the function is public and tested on its own output. It returns a
/// projection rather than the row, which is what makes that safe to expose:
/// no caller can reach `audit_trail` or `decided_by` through it, so consent
/// cannot start depending on whether history has been read.
#[tokio::test]
async fn the_projection_reads_the_stored_tier_and_flag_verbatim() {
    use commons_store::locator::consent_facts;

    let (_d, store) = store().await;

    // Every tier x flag combination round-trips exactly.
    for tier in ConsentTier::ALL {
        for flag in [false, true] {
            let id = format!("proj-{}-{}", tier.as_str(), flag);
            object_at(&store, &id, tier, flag).await;
            let facts = consent_facts(&store, &id).await.unwrap();
            assert_eq!(
                (facts.tier, facts.redistribution_permitted),
                (tier, flag),
                "{id} did not read back as stored"
            );
        }
    }

    // No consent record: the safe default, and it is not a per-object guess.
    object_without_consent(&store, "proj-absent").await;
    let facts = consent_facts(&store, "proj-absent").await.unwrap();
    assert_eq!(facts.tier, ConsentTier::Unverified);
    assert!(!facts.redistribution_permitted);

    // An unrecognised tier name is the most restrictive tier. Asserted on the
    // value, because `denied` and `unverified` are indistinguishable through
    // the magnet rule and only differ where it matters: http metadata.
    sqlx::query(
        "INSERT INTO object (id, kind, created_at, updated_at) VALUES ('proj-weird', 'scene', '', '')",
    )
    .execute(store.pool())
    .await
    .unwrap();
    for weird in [
        "tier_from_the_future",
        "",
        "DENIED",
        "denied ",
        "third party permitted",
    ] {
        sqlx::query("DELETE FROM consent_record WHERE object_id = 'proj-weird'")
            .execute(store.pool())
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO consent_record (id, object_id, tier, redistribution_permitted, updated_at)
             VALUES ('c-weird', 'proj-weird', ?, 1, '')",
        )
        .bind(weird)
        .execute(store.pool())
        .await
        .unwrap();

        let facts = consent_facts(&store, "proj-weird").await.unwrap();
        assert_eq!(
            facts.tier,
            ConsentTier::Denied,
            "{weird:?} resolved to {:?}",
            facts.tier
        );
        // Which is what distinguishes it: an unknown tier must not record
        // metadata either, so a hand-edited row cannot become a source url.
        assert!(
            !facts.permits(LocatorScheme::Http),
            "{weird:?} resolved to {:?}, which would allow a source url",
            facts.tier
        );
    }

    // The exact stored spelling *is* accepted, so the parse is not simply
    // always-deny: a broken parse would look identical to a safe one on every
    // test above.
    for tier in ConsentTier::ALL {
        assert_eq!(ConsentTier::parse(tier.as_str()), Some(tier));
    }
    assert_eq!(ConsentTier::parse("denied"), Some(ConsentTier::Denied));
    assert_eq!(ConsentTier::parse("nope"), None);
}
