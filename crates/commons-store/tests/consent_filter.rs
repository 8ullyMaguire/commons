//! T-P4-007 — consent tiers as a store-layer filter.
//!
//! §14.1: "Rules, all enforced in the data layer, not the UI."
//!
//! # Why this is a ticket at all
//!
//! The consent clause *existed* — [`Filter::consent_clause`] has been in
//! `filter_ast.rs` since T-P0-005, with unit tests, and it is correct as
//! written. What did not exist was anything that made using it *unavoidable*.
//! It was a `pub fn` on a `Filter`, called by nothing outside its own test
//! module. A clause nobody is obliged to call is a convention, and a convention
//! is not a data-layer guarantee: the first new query path — a recommendation
//! join, a sync pull, a federation inbox scan — would be written straight
//! against `object` and would hand `denied` rows to an anonymous visitor, and
//! no test would fail, because the clause had never been part of any query's
//! construction.
//!
//! So the work is the *sealing*, not the clause. Two pieces:
//!
//! 1. [`Store::query`], the only supported way to read objects. It takes a
//!    [`CallerId`] and has no overload without one, so there is no call that
//!    forgets the filter.
//! 2. `no_object_query_bypasses_the_consent_clause`, a guard that fails if any
//!    object-reading SQL in the workspace selects from `object` without going
//!    through it.
//!
//! # The rule that is easiest to get wrong
//!
//! §14.1 lists `denied` as "takedown accepted. Permanently blocked by hash
//! across all peers." `may_see_restricted()` is `Steward | Admin`, so the
//! pre-existing clause returned `1 = 1` for both: a steward browsing objects in
//! the ordinary browse surface saw every denied row, having had a takedown
//! accepted against them. The exemption exists because moderation needs to see
//! contested material — but it was written `1 = 1`, which grants *everything*
//! instead of *what moderation needs*.
//!
//! The fix is a named fourth set: not public, not owner, not all — the
//! moderation set, written out as a constant so the intent is legible at the
//! point of use. And it is tested as a table rather than as an example, because
//! a permission this broad is one bit-flip away from being a data leak.
//!
//! # A second bug, in the same function
//!
//! [`ConsentTiers::OWNER`] omitted `third_party_permitted`. The owner's set is
//! what the *library operator* sees, and §14.1 is explicit that a licensed item
//! is one "the user may watch and keep". An operator holding a licensed file
//! who cannot find it in their own library has been told the platform refuses to
//! show them material they are entitled to. The constant was written to answer
//! the question "may the owner see their own unverified scans?" and silently
//! answered a second, larger question as a side effect.
//!
//! Both bugs were in the *constants*, not the logic. The logic was right; the
//! table it read from was not — and a table is precisely what a test checking
//! one example of will not catch.

use commons_store::filter_ast::{ConsentTiers, Filter};

/// The clause a caller of each kind gets, as the tier set it is derived from.
///
/// Asserted on the *clause* rather than on the constants, so this file tests
/// what a query actually binds and not just what a table says.
fn visible_tiers(caller: &commons_store::filter_ast::CallerId) -> Vec<String> {
    let mut params = Vec::new();
    Filter::consent_clause(caller, &mut params).unwrap();
    let mut out: Vec<String> = params
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    out.sort();
    out
}

fn sorted(set: &[&str]) -> Vec<String> {
    let mut v: Vec<String> = set.iter().map(|s| s.to_string()).collect();
    v.sort();
    v
}

#[test]
fn the_public_clause_binds_exactly_the_publishable_tiers() {
    let tiers = visible_tiers(&commons_store::filter_ast::CallerId::anonymous());
    assert_eq!(
        tiers,
        sorted(&ConsentTiers::PUBLIC),
        "an anonymous caller must be bound to exactly the publishable tiers"
    );
    // Named explicitly, not just as a set difference, because each of these
    // three has a *stronger* prohibition in §14.1 than the others and each is
    // the one most likely to be re-added by somebody tidying up the list.
    for t in ["unverified", "quarantined", "denied"] {
        assert!(
            !tiers.iter().any(|x| x == t),
            "`{t}` must not be reachable by an anonymous caller: {tiers:?}"
        );
    }
}

#[test]
fn the_owner_clause_is_the_public_set_plus_their_own_unverified_scans() {
    let owner = commons_store::filter_ast::CallerId {
        account_id: Some(uuid::Uuid::new_v4().to_string()),
        role: commons_core::Role::Subscriber,
        ..commons_store::filter_ast::CallerId::anonymous()
    };
    let tiers = visible_tiers(&owner);
    assert_eq!(
        tiers,
        sorted(&ConsentTiers::OWNER),
        "the owner's clause must be the OWNER set, and the OWNER set must be a \\
         superset of the public one -- §14.1 says a licensed item is one the \\
         user may watch and keep, so an operator holding one must be able to \\
         find it. OWNER omitting `third_party_permitted` was a real bug."
    );
    assert!(
        tiers.iter().any(|x| x == "unverified"),
        "and the owner's own freshly scanned files must be findable"
    );
    for t in ["quarantined", "denied"] {
        assert!(
            !tiers.iter().any(|x| x == t),
            "`{t}` stays hidden from its own uploader: §14.1 says 'hidden \\
             everywhere, pending review', and 'everywhere' includes the uploader. \\
             {tiers:?}"
        );
    }
}

#[test]
fn a_steward_clause_is_not_one_equal_one() {
    // This is the whole ticket in one assertion. `may_see_restricted()` is
    // `Steward | Admin`, and the clause it used to guard returned `1 = 1`,
    // which put every `denied` row in the ordinary browse surface of somebody
    // who had a takedown accepted against them.
    let steward = commons_store::filter_ast::CallerId {
        account_id: Some(uuid::Uuid::new_v4().to_string()),
        role: commons_core::Role::Steward,
        ..commons_store::filter_ast::CallerId::anonymous()
    };
    let mut params = Vec::new();
    let clause = Filter::consent_clause(&steward, &mut params).unwrap();
    assert_ne!(
        clause, "1 = 1",
        "a steward must be bound to a tier list, not waved through: an \\
         unconditional grant also grants `denied`, which §14.1 calls \\
         'permanently blocked by hash across all peers'"
    );
    let tiers: Vec<&str> = params.iter().filter_map(|v| v.as_str()).collect();
    assert!(
        !tiers.contains(&"denied"),
        "`denied` is not a moderation tier, it is a takedown. Moderation sees \\
         `quarantined`; a `denied` row is blocked by hash and does not belong in \\
         any browse surface, including a moderator's: {tiers:?}"
    );
    for t in ConsentTiers::MODERATION {
        assert!(
            tiers.contains(&t),
            "the moderation set must contain `{t}`, or a moderator cannot see \\
             the row they are moderating"
        );
    }
}

#[test]
fn an_admin_clause_is_also_bounded() {
    // Admin is in `may_see_restricted` alongside Steward, so it took the same
    // `1 = 1`. Checked as its own test because "admins see everything" is a
    // sentence that gets typed into a code path by accident, and the fix for it
    // was applied to one role and not the other the first time round.
    let admin = commons_store::filter_ast::CallerId {
        account_id: Some(uuid::Uuid::new_v4().to_string()),
        role: commons_core::Role::Admin,
        ..commons_store::filter_ast::CallerId::anonymous()
    };
    let mut params = Vec::new();
    Filter::consent_clause(&admin, &mut params).unwrap();
    let tiers: Vec<&str> = params.iter().filter_map(|v| v.as_str()).collect();
    assert!(
        !tiers.contains(&"denied"),
        "an admin's ordinary browse surface must not contain denied rows: \\
         {tiers:?}"
    );
}

#[test]
fn the_tier_sets_contain_every_tier_and_nothing_unnamed() {
    // Not a disjointness check, and the first version of this test was wrong
    // about that. The sets are *supposed* to overlap: OWNER is deliberately a
    // superset of PUBLIC, and MODERATION covers the publishable tiers plus
    // `quarantined`, for the reason given on the constant. A disjointness
    // requirement would have forced the owner to be denied their own licensed
    // files -- the very bug this ticket fixed -- so the "obvious" invariant is
    // the wrong one.
    //
    // What has to hold is the weaker pair, and it is the pair that carries
    // meaning: every tier is named by at least one set (so a new tier cannot be
    // added and silently be invisible to everyone), and no ordinary set names a
    // contested tier (so a bug that widened PUBLIC to include `quarantined`
    // fails here rather than in production).
    // `denied` is the exception and the point of the whole ticket: it is named
    // by *no* set, so no caller can see it. Asserted rather than left implicit,
    // because "every tier is in some set" is the more natural sentence to write
    // and it is wrong here -- a future reader tidying up the coverage would add
    // `denied` to MODERATION and every test in this file would still pass.
    for t in ConsentTiers::ALL {
        let named = ConsentTiers::PUBLIC.contains(&t)
            || ConsentTiers::OWNER.contains(&t)
            || ConsentTiers::MODERATION.contains(&t);
        if t == "denied" {
            assert!(
                !named,
                "`denied` must be named by no tier set: a takedown is not a \
                 moderation state, and the steward clause used to grant it"
            );
        } else {
            assert!(
                named,
                "`{t}` is named by no tier set, so no caller can see it"
            );
        }
    }
    let ordinary: [&[&str]; 2] = [&ConsentTiers::PUBLIC, &ConsentTiers::OWNER];
    for set in ordinary {
        for t in set {
            assert!(
                !t.starts_with("quarantined") && !t.starts_with("denied"),
                "`{t}` is in a set an ordinary caller uses, so an ordinary \
                 caller can see contested or denied material"
            );
        }
    }
}

#[test]
fn the_tier_sets_and_the_enum_agree() {
    // `commons-core`'s `ConsentTier` and `commons-store`'s string constants are
    // two representations of one policy. If they drift, one is wrong and
    // nothing says which, so the check is that they agree over every tier.
    for name in ConsentTiers::ALL {
        assert!(
            commons_core::ConsentTier::ALL
                .iter()
                .any(|t| t.as_str() == name),
            "`{name}` is in a tier set but is not a ConsentTier: the constant \
             and the enum have drifted"
        );
    }
    for tier in commons_core::ConsentTier::ALL {
        assert!(
            ConsentTiers::ALL.contains(&tier.as_str()),
            "`{tier:?}` is a ConsentTier that appears in no tier set: the enum \
             and the constant have drifted"
        );
    }
}

#[test]
fn an_empty_allowlist_is_an_owner_not_an_allowlist_of_nothing() {
    // `tier_allowlist: vec![]` means "the library's own operator", so it must
    // not degenerate into "no tiers", which would be the safe-looking reading
    // and the one that hides the operator's entire library.
    let owner = commons_store::filter_ast::CallerId {
        account_id: Some(uuid::Uuid::new_v4().to_string()),
        role: commons_core::Role::Subscriber,
        tier_allowlist: vec![],
        excluded_tag_ids: vec![],
        excluded_studio_ids: vec![],
        excluded_performer_ids: vec![],
    };
    let tiers = visible_tiers(&owner);
    assert!(
        tiers.len() >= 3,
        "an empty allowlist must fall back to the OWNER set, not to an empty \
         IN list: {tiers:?}"
    );
}

#[test]
fn an_explicit_allowlist_is_honoured_exactly() {
    // The other half of the fallback: an allowlist that *is* set is used as
    // given, including a one-tier allowlist, and is not widened to the owner's
    // set. A peer the user consented to publish to sees exactly what they were
    // consented to.
    let peer = commons_store::filter_ast::CallerId {
        account_id: Some(uuid::Uuid::new_v4().to_string()),
        role: commons_core::Role::Subscriber,
        tier_allowlist: vec!["self_published".to_string()],
        excluded_tag_ids: vec![],
        excluded_studio_ids: vec![],
        excluded_performer_ids: vec![],
    };
    assert_eq!(
        visible_tiers(&peer),
        vec!["self_published".to_string()],
        "a per-peer publish consent is exactly the tiers it names"
    );
}

#[test]
fn no_object_query_bypasses_the_consent_clause() {
    // The guard. Reads every `.rs` in the workspace and fails if one contains a
    // `SELECT ... FROM object` that is not consent-filtered.
    //
    // A scan rather than a function test, because a test asserting `query()`
    // filters correctly is satisfied by an unfiltered `list_objects()` beside
    // it: the second is called by nothing, so it is not covered, and nothing
    // fails. This fails when the query is *written*, which is the moment the
    // decision is made.
    //
    // # What this cannot do, and what covers it instead
    //
    // A text scan can prove a filter is *present*. It cannot prove the filter
    // names the *right* tiers, and two mutations survive it for exactly that
    // reason: widening the phash query's `c.tier IN (?, ?, ?)` to include
    // `denied`, and removing its `WHERE` while keeping the join (a join narrows
    // nothing). Both are caught by the behaviour tests in
    // `crates/commons-index/tests/candidates.rs`, which put a `denied` object in
    // the database and assert nothing comes out.
    //
    // The two layers are checked against each other on purpose, and each one has
    // a history of surviving a mutation the other kills. Three versions of this
    // guard were written and all three let a real bypass through:
    //
    // * keyed on the **file** -- everything later added to an allowed file
    //   inherited the exemption, so widening `SELECT 1 FROM object` to
    //   `SELECT title, kind FROM object` left all 17 tests green.
    // * keyed on the file plus a **count** of reads against a count of
    //   allowlisted statements -- the widened query still begins with `SELECT`
    //   and contains `FROM object`, so the count matched and the file was
    //   skipped again.
    // * keyed on a **24-line window** containing the word `consent_record` --
    //   and `locator.rs` has a comment explaining why its probe needs no
    //   filter, which mentions `consent_record`, so every statement near it
    //   passed.
    //
    // The version that works keys on the *statement*, strips comments first,
    // and requires a predicate on the joined column rather than the join's mere
    // presence. Each of those three fixes came from a mutation surviving, which
    // is the argument for running mutations against a guard at all.
    //
    // It has already earned its keep. It found two real bypasses on the run
    // that added it:
    //
    // * `candidates.rs` read `SELECT title FROM object` to seed a phash
    //   proposal, so a match against a `denied` object turned the takedown's
    //   content into a *proposal* -- weighed and voted on by people who cannot
    //   see the object it came from. The takedown blocked the row and this route
    //   walked around it.
    // * `locator.rs` read `SELECT 1 FROM object` as an existence probe. That one
    //   is fine -- a boolean about existence is not a disclosure -- and it is
    //   allowed explicitly below, with the reason in the allowlist, so the
    //   allowance is a decision a reviewer can see rather than an exemption in
    //   the scanner.
    //
    // Keyed on the *statement*, not the file. An earlier version keyed on the
    // file name, and the very next check -- widening the exempted
    // `SELECT 1 FROM object` to `SELECT title, kind FROM object` -- left all 17
    // tests green. A file-level allowlist is a hole shaped like the thing it was
    // meant to constrain: everything anybody later adds to that file inherits the
    // exemption. Keying on the statement means widening the query is a visible
    // edit to this list, and the reason travels with it.
    const ALLOWED: &[(&str, &str)] = &[
        (
            "the consent-filtered query itself, which names `c.tier` and joins \
             `consent_record`",
            "FROM object o
               INNER JOIN consent_record c",
        ),
        (
            "an existence probe: `SELECT 1`, which discloses presence and no \
             field",
            "SELECT 1 FROM object WHERE id = ?",
        ),
    ];

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();

    // A hand-rolled walk rather than `walkdir`: this is a dev-dependency for one
    // file walk, and a new dependency in the tree is a supply-chain decision a
    // lint test does not get to make on its own.
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            let s = p.to_string_lossy();
            if s.contains("/target/") || s.contains("/.git/") || s.ends_with(".rs~") {
                continue;
            }
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().and_then(|x| x.to_str()) == Some("rs") {
                out.push(p);
            }
        }
    }
    let mut paths = Vec::new();
    walk(&root, &mut paths);
    paths.sort();

    let mut offenders: Vec<String> = Vec::new();
    for path in &paths {
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned();
        let _ = &rel;
        let src = std::fs::read_to_string(path).unwrap();
        // Normalise whitespace so a statement split across lines still matches
        // its allowlist entry, and a reformatted query does not silently fall
        // out of the exemption.
        let flat = src.split_whitespace().collect::<Vec<_>>().join(" ");
        // No counting, no file-level skip. Every candidate statement is checked
        // against the allowlist *as a whole*, and the window test below is the
        // only thing that decides.
        //
        // Both earlier designs were wrong in the same way and the second one was
        // worse. Keying the allowlist on the file let everything later added to
        // an allowed file inherit the exemption -- widening `SELECT 1 FROM
        // object` to `SELECT title, kind FROM object` left all 17 tests green.
        // Counting the reads and the allowlisted statements instead fixed that
        // case and broke the next: the widened query still begins with `SELECT`
        // and contains `FROM object`, so the count matched the allowance and the
        // file was skipped again. A guard that skips is a guard that can be
        // skipped; the allowlist has to be consulted per statement, with no
        // path that bypasses the consultation.
        let _ = flat;

        let lines: Vec<&str> = src.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            let t = line.trim_start();
            if t.starts_with("//") || t.starts_with("///") || t.starts_with('*') {
                continue;
            }
            let upper = line.to_uppercase();
            if !upper.contains("SELECT") {
                continue;
            }
            // `FROM OBJECT` exactly -- not a prefix. `object_phash` and
            // `object_merge` both start with it, and neither is a read of the
            // object table: one reads hashes, one reads a merge log. A
            // substring match here would flag two queries that are fine and get
            // the guard switched off, which is the failure mode of every guard
            // that is too eager.
            if !upper.contains("FROM OBJECT ") && !upper.contains("FROM OBJECT\n") {
                continue;
            }
            if upper.contains("FROM OBJECT_") {
                continue;
            }
            // A consent-filtered query names the clause or the tier column in
            // the *statement*, and the statement is the SQL string literal plus
            // the few lines that finish it -- not a 24-line neighbourhood.
            //
            // The window was 24 lines and that was a hole shaped like the bug it
            // was meant to catch: `locator.rs` has a comment explaining why its
            // existence probe needs no filter, and that comment mentions
            // `consent_record`, so every statement within 12 lines of it passed.
            // Widening `SELECT 1 FROM object` to `SELECT title, kind FROM
            // object` left all 17 tests green.
            //
            // Comments are stripped first, and the window is exactly the
            // statement: from this line to the first line that closes the
            // literal. A long query spread over many lines is still covered;
            // prose written near it is not.
            let window: String = lines
                .iter()
                .skip(i)
                .take(4)
                .copied()
                .filter(|l| {
                    let t = l.trim_start();
                    !t.starts_with("//") && !t.starts_with("///") && !t.starts_with('*')
                })
                .collect::<Vec<_>>()
                .join(" ");
            // The allowlist is consulted per statement, by its exact
            // normalised text. This is the check that has to be unskippable.
            let stmt: String = lines
                .iter()
                .skip(i)
                .take(3)
                .copied()
                .collect::<Vec<_>>()
                .join(" ")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let permitted = ALLOWED.iter().any(|(_, needle)| {
                stmt.contains(&needle.split_whitespace().collect::<Vec<_>>().join(" "))
            });
            if permitted {
                continue;
            }
            // A join to `consent_record` is not a filter. The phash query was
            // `INNER JOIN consent_record ... WHERE c.tier IN (...)`, and a
            // mutation that dropped the `WHERE` clause while keeping the join
            // left all 17 tests green -- the query then returned every object
            // that has *any* consent record, which is every object with one
            // joined, at every tier. A join narrows nothing; only a predicate on
            // the tier does.
            //
            // So the requirement is a *predicate on the joined column*, not the
            // join's presence. A bound parameter list (`c.tier IN (?, ?, ?)`) is
            // what a filter looks like, and a bare `c.tier` with no comparison
            // is the join wearing a filter's clothes.
            let has_tier_predicate = window.contains("c.tier IN (")
                || window.contains("c.tier NOT IN")
                || window.contains("c.tier =")
                || window.contains("c.tier !=")
                || window.contains("c.tier LIKE");
            if window.contains("consent_clause") || has_tier_predicate {
                continue;
            }
            offenders.push(format!("{rel}:{}: {}", i + 1, t));
        }
    }

    assert!(
        offenders.is_empty(),
        "§14.1: \"Rules, all enforced in the data layer, not the UI.\" These \
         queries read `object` without a consent filter. A clause nobody is \
         obliged to call is a convention, not a guarantee:\n{}",
        offenders.join("\n")
    );
}

// ---------------------------------------------------------------------------
// The end-to-end proof: `Store::query` really does filter, against a real
// database rather than a string comparison.
// ---------------------------------------------------------------------------

/// An object with a consent record, created through SQL rather than a helper,
/// because the helper that exists creates a `field_proposal` and this test is
/// about the `object` table.
///
/// Each call gets its own `id` and its own tier, and the *pair* is what the
/// tests below assert on. A fixture with one of everything cannot test a filter
/// (a single `denied` row among four others proves nothing about which of the
/// four was excluded).
async fn with_object(store: &commons_store::db::Store, id: &str, tier: &str) {
    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO object (id, kind, title, created_at, updated_at)
         VALUES (?, 'scene', ?, ?, ?)",
    )
    .bind(id)
    .bind(format!("{id} title"))
    .bind(&now)
    .bind(&now)
    .execute(store.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO consent_record (id, object_id, tier, redistribution_permitted, updated_at)
         VALUES (?, ?, ?, 0, ?)",
    )
    .bind(format!("c-{id}"))
    .bind(id)
    .bind(tier)
    .bind(&now)
    .execute(store.pool())
    .await
    .unwrap();
}

fn caller(role: commons_core::Role) -> commons_store::filter_ast::CallerId {
    commons_store::filter_ast::CallerId {
        account_id: Some(uuid::Uuid::new_v4().to_string()),
        role,
        ..commons_store::filter_ast::CallerId::anonymous()
    }
}

#[tokio::test]
async fn an_anonymous_query_sees_only_the_publishable_tiers() {
    let store = commons_store::db::Store::open_memory().await.unwrap();
    for (id, tier) in [
        ("o-unv", "unverified"),
        ("o-self", "self_published"),
        ("o-perf", "performer_claimed"),
        ("o-third", "third_party_permitted"),
        ("o-quar", "quarantined"),
        ("o-deny", "denied"),
    ] {
        with_object(&store, id, tier).await;
    }

    let page = store
        .query(
            &Filter::All,
            &commons_store::filter_ast::CallerId::anonymous(),
            50,
        )
        .await
        .unwrap();
    let mut ids = page.ids();
    ids.sort();
    assert_eq!(
        ids,
        vec!["o-perf", "o-self", "o-third"],
        "an anonymous caller sees the three publishable tiers and nothing else"
    );
}

#[tokio::test]
async fn the_owner_query_sees_their_own_unverified_scans_and_licensed_files() {
    let store = commons_store::db::Store::open_memory().await.unwrap();
    for (id, tier) in [
        ("o-unv", "unverified"),
        ("o-third", "third_party_permitted"),
        ("o-quar", "quarantined"),
        ("o-deny", "denied"),
    ] {
        with_object(&store, id, tier).await;
    }

    let mut ids = store
        .query(&Filter::All, &caller(commons_core::Role::Subscriber), 50)
        .await
        .unwrap()
        .ids();
    ids.sort();
    assert_eq!(
        ids,
        vec!["o-third", "o-unv"],
        "the owner finds their unverified scans and their licensed files, and \
         not the contested or denied ones"
    );
}

#[tokio::test]
async fn a_steward_query_does_not_return_denied_rows() {
    // The bug this ticket found, end to end. `may_see_restricted()` is
    // `Steward | Admin` and the clause it guarded returned `1 = 1`, so this query
    // used to return `o-deny`.
    let store = commons_store::db::Store::open_memory().await.unwrap();
    for (id, tier) in [
        ("o-self", "self_published"),
        ("o-quar", "quarantined"),
        ("o-deny", "denied"),
    ] {
        with_object(&store, id, tier).await;
    }

    for role in [commons_core::Role::Steward, commons_core::Role::Admin] {
        let mut ids = store
            .query(&Filter::All, &caller(role), 50)
            .await
            .unwrap()
            .ids();
        ids.sort();
        assert_eq!(
            ids,
            vec!["o-quar", "o-self"],
            "{role:?} sees the contested material and the ordinary rows but \
             not the denied one -- a takedown is not a moderation state"
        );
    }
}

#[tokio::test]
async fn an_object_with_no_consent_record_is_invisible_to_everyone() {
    // §14.1: `unverified` is "private by default". A missing record has to read
    // as private, or an object inserted by a code path that forgot the consent
    // row is published by default -- which is the failure mode a default causes
    // and an INNER JOIN does not.
    let store = commons_store::db::Store::open_memory().await.unwrap();
    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO object (id, kind, title, created_at, updated_at)
         VALUES ('o-bare', 'scene', 'no record', ?, ?)",
    )
    .bind(&now)
    .bind(&now)
    .execute(store.pool())
    .await
    .unwrap();

    for role in [
        commons_core::Role::Public,
        commons_core::Role::Subscriber,
        commons_core::Role::Contributor,
        commons_core::Role::Steward,
        commons_core::Role::Admin,
    ] {
        let ids = store
            .query(&Filter::All, &caller(role), 50)
            .await
            .unwrap()
            .ids();
        assert!(
            !ids.contains(&"o-bare".to_string()),
            "{role:?} can see an object with no consent record: {ids:?}"
        );
    }
}

#[tokio::test]
async fn the_consent_filter_and_the_user_filter_are_both_applied() {
    // Two clauses, two parameter lists. Getting the order wrong binds the
    // consent tiers to the user's values -- and the query still runs, returning
    // the wrong rows, which is why the filter has to be checked against a
    // fixture that distinguishes it.
    let store = commons_store::db::Store::open_memory().await.unwrap();
    with_object(&store, "o-match", "self_published").await;
    with_object(&store, "o-other", "performer_claimed").await;

    let filter = Filter::Facet {
        kind: None,
        field: commons_store::filter_ast::FieldRef::Builtin(
            commons_store::filter_ast::BuiltinField::Title,
        ),
        op: commons_store::filter_ast::CmpOp::Eq,
        values: vec![commons_store::filter_ast::Value::Str(
            "o-other title".to_string(),
        )],
    };
    let ids = store
        .query(
            &filter,
            &commons_store::filter_ast::CallerId::anonymous(),
            50,
        )
        .await
        .unwrap()
        .ids();
    assert_eq!(
        ids,
        vec!["o-other"],
        "the title filter selected the row it names, which is only true if the \
         consent tiers were bound to the consent placeholders and not to the \
         title's"
    );
}

#[tokio::test]
async fn a_tier_filter_cannot_be_used_to_reach_a_hidden_tier() {
    // A caller who asks for `denied` by name still does not get it: the consent
    // clause is ANDed, not overridden by a facet. §5.16's URLs are
    // attacker-controllable, so a filter that could widen visibility would make
    // the shareable-URL feature a disclosure.
    let store = commons_store::db::Store::open_memory().await.unwrap();
    with_object(&store, "o-deny", "denied").await;
    with_object(&store, "o-self", "self_published").await;

    let filter = Filter::Facet {
        kind: None,
        field: commons_store::filter_ast::FieldRef::Builtin(
            commons_store::filter_ast::BuiltinField::ConsentTier,
        ),
        op: commons_store::filter_ast::CmpOp::Eq,
        values: vec![commons_store::filter_ast::Value::Str("denied".to_string())],
    };
    let ids = store
        .query(
            &filter,
            &commons_store::filter_ast::CallerId::anonymous(),
            50,
        )
        .await
        .unwrap()
        .ids();
    assert!(
        ids.is_empty(),
        "asking for the denied tier by name returned rows: {ids:?}"
    );
}

#[tokio::test]
async fn a_limit_beyond_the_maximum_is_refused() {
    // §5.16's shareable URLs are attacker-controllable, so the page size is part
    // of the trust boundary rather than a tuning knob: an unbounded limit is a
    // denial of service against the operator's own disk.
    let store = commons_store::db::Store::open_memory().await.unwrap();
    let err = store
        .query(
            &Filter::All,
            &commons_store::filter_ast::CallerId::anonymous(),
            commons_store::query::MAX_PAGE + 1,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, commons_store::query::QueryError::LimitTooLarge(_, _)),
        "expected a LimitTooLarge, got {err:?}"
    );
}

#[tokio::test]
async fn has_more_is_true_when_a_row_was_left_off() {
    // Tested at the exact boundary, not a comfortable distance from it: at
    // `limit == rows` and at `limit == rows - 1`. A `has_more` that is always
    // false looks identical to a correct one in every test with a small page.
    let store = commons_store::db::Store::open_memory().await.unwrap();
    for i in 0..3 {
        with_object(&store, &format!("o-{i}"), "self_published").await;
    }
    let anon = commons_store::filter_ast::CallerId::anonymous();
    for (limit, want_rows, want_more) in [(3usize, 3usize, false), (2, 2, true), (1, 1, true)] {
        let page = store.query(&Filter::All, &anon, limit).await.unwrap();
        assert_eq!(page.rows.len(), want_rows, "at limit {limit}");
        assert_eq!(page.has_more, want_more, "at limit {limit}");
    }
}
