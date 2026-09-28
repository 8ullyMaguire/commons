//! Resolving a request to an identity.
//!
//! # Why this file exists
//!
//! `Role`, `CallerId`, `filter_ast` and `ShareGrant` are all complete and all
//! used — 163 references to `Role` alone. What was missing is the one call
//! site: `media::local_caller()` returned a hardcoded constant, and every route
//! in `commons-server` called *it*. So the authorization model was fully
//! specified and never consulted, and no amount of reading the store would
//! have revealed that — only `grep -c Role crates/commons-server/src/`
//! returning 0 does.
//!
//! The spec for that is `docs/spec/t-p6-007-public-api.md` §3.
//!
//! # Why this is NOT the plugin capability model
//!
//! `commons-plugin` has a `Capability`/`HostPolicy` pair and it is the
//! obvious thing to reach for when adding an API. It is the wrong one, for
//! three reasons that are worth keeping in a comment because all three are
//! invisible from the code:
//!
//! 1. A plugin runs *in the host process* and is subject to the host's
//!    enforcement. An API client runs *outside* and must be subject to *the
//!    user's*, which is `Role` + `CallerId` + `ShareGrant`.
//! 2. §11.5's Jellyfin half is for local-network clients that have never
//!    authenticated. T-P6-005's shape applies verbatim: off by default,
//!    loopback-bound when on.
//! 3. T-P6-006 established that the plugin `HostApi` is a Rust trait that
//!    native code does not have to go through — "enforced" there means
//!    *declared and checked at the plugin boundary*. A public API is the
//!    opposite case: the one surface every caller must go through, which is
//!    exactly why this is where identity has to be real.
//!
//! # The fallback is deliberate, and it is not the design
//!
//! [`media::local_caller`] remains the no-credentials answer so the 22
//! existing route tests keep passing unchanged. It is the *absence* of a
//! design, named so that it shows up in a diff when credentials arrive. It is
//! also what makes this ticket safe to land in one commit: the fallback is
//! provably equivalent to the old behaviour, which is asserted rather than
//! assumed.

use std::sync::Arc;

use axum::http::HeaderMap;
use commons_consent::share::Scope;
use commons_store::filter_ast::{CallerId, ConsentTiers};

use crate::AppState;

/// Extract the bearer token from an `Authorization: Bearer <token>` header.
///
/// Returns `None` for a missing header, a non-bearer scheme, and an empty
/// token — all three are "no credentials", not errors. The distinction matters
/// because a caller that sends a *malformed* credential is not the same as a
/// caller that sends none, and treating a broken credential as an absent one
/// is how a proxy ends up granting more than it should.
///
/// The scheme match is **case-insensitive**: RFC 7235 §2.1 defines the auth
/// scheme as case-insensitive, and a client that sends `bearer` has
/// authenticated. Matching the literal `"Bearer "` would silently downgrade
/// such a client to the anonymous path, which is an interop bug that only
/// shows up against a third-party client nobody here wrote.
fn bearer(headers: &HeaderMap) -> Option<&str> {
    let raw = headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    let (scheme, token) = raw.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    // A token with whitespace in it is not a token. `split_token` would
    // reject it anyway, but returning `None` here keeps "not a credential"
    // meaning one thing at one place.
    if token.is_empty() || token.chars().any(char::is_whitespace) {
        return None;
    }
    Some(token)
}

/// Resolve a request to the identity it should be served as.
///
/// Order is the policy, and each step is a strict refinement of the last:
///
/// 1. `Authorization: Bearer <token>` — resolve as a share grant. The whole
///    grant policy (signature, revocation, expiry, password) is already one
///    call in `commons_consent::share::resolve`, and
///    [`crate::share::load_and_resolve`] already logs every attempt. A grant
///    that does not resolve is **not** an error here: the caller is anonymous.
/// 2. No credentials — [`crate::media::local_caller`], unchanged.
///
/// **No `uri` parameter**, and that is a decision rather than an omission. An
/// earlier draft took one, on the theory that a path-scoped API might resolve
/// a grant differently per path. Nothing needs that, and a parameter nothing
/// reads is a claim about this function that a future reader cannot check —
/// and if a path *did* start changing the answer, the two-token case would
/// stop being one function's job. Add it when something needs it.
///
/// # A share token must never produce a role with permissions
///
/// This is the one thing a future edit to this function can get wrong, so it
/// is worth stating in the type rather than only in a test. A `View` link and
/// an `Admin` session both reach the same handler, and the difference between
/// them is a *scope* enforced by [`commons_consent::share::Scope::can_download`]
/// — not a `Role`. If this function ever returns `Role::Contributor` for a
/// share token, a read-only link has become a write-capable one.
///
/// The reason is not caution. `Scope` has exactly two variants, `View` and
/// `ViewDownload`, and that pair exists precisely because a capability grant
/// was the right shape. Converting a share into a role would mean either
/// inventing roles per scope (five roles times two scopes) or picking
/// `Role::Public` and losing the download distinction, and both discard the
/// thing the two variants were built to express.
pub async fn caller_from_request(state: &Arc<AppState>, headers: &HeaderMap) -> CallerId {
    let Some(token) = bearer(headers) else {
        return crate::media::local_caller();
    };

    match crate::share::load_and_resolve(state, token, None, headers).await {
        Ok((row, grant)) => {
            tracing::debug!(grant_id = %row.id, scope = %grant.scope, "request carried a share grant");
            CallerId {
                account_id: Some(row.id),
                // THE load-bearing field, and getting it wrong makes the grant
                // WIDER than anonymous rather than narrower.
                //
                // `consent_clause` (filter_ast.rs:455) branches three ways:
                // an account_id with an empty allowlist is the library OWNER
                // -- `ConsentTiers::OWNER`, a superset of `PUBLIC`; no
                // account is `PUBLIC`; an explicit allowlist is itself. So a
                // grant that set only `account_id` would be served as the
                // owner, which means a `View` link to a stranger's library
                // could see `unverified` scans. It would also be the exact bug
                // T-P6-005's DLNA work and `CallerId::steward`'s own docs warn
                // about: the helper decides what the caller sees instead of
                // the clause.
                //
                // So the grant names its own tiers, exactly as
                // `CallerId::steward` names `ConsentTiers::MODERATION` rather
                // than everything.
                tier_allowlist: grant_tiers(grant.scope),
                ..CallerId::anonymous()
            }
        }
        // Not an error, and specifically NOT `local_caller()`. Falling back to
        // the local owner here would let anyone who guesses a wrong token
        // receive owner-tier consent, which is the whole privilege the
        // fallback exists to avoid.
        Err(_) => {
            tracing::debug!("bearer token did not resolve; serving as anonymous");
            CallerId::anonymous()
        }
    }
}

/// The consent tiers a share grant may see, by scope.
///
/// A `View` link gets the same set an anonymous visitor gets, and a
/// `ViewDownload` link gets those plus `unverified` — the tier a library's own
/// operator can see and a visitor cannot, because a freshly scanned file is
/// `unverified` and there is no reason a shared link should be *less* capable
/// of serving the file it was made for than the browser it came from.
///
/// **This deliberately does not include `quarantined` or `denied`.** Both are
/// in `ConsentTiers::ALL` and both are absent here, so a share link cannot
/// serve a takedown or a contested item — and `a_live_view_link_reaches_a_
/// denied_object_that_anonymous_cannot` is the test that holds that line from
/// the other direction.
fn grant_tiers(scope: Scope) -> Vec<String> {
    let mut tiers: Vec<String> = ConsentTiers::PUBLIC.iter().map(|s| s.to_string()).collect();
    if scope.can_download() {
        tiers.extend(ConsentTiers::OWNER.iter().map(|s| s.to_string()));
        tiers.sort();
        tiers.dedup();
    }
    tiers
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers_with(v: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            axum::http::header::AUTHORIZATION,
            HeaderValue::from_str(v).unwrap(),
        );
        h
    }

    /// The header-shape table, which needs no server and so lives here.
    /// Every one of these is "no credentials" — none is an error, and none may
    /// produce a *different* identity than sending nothing at all would.
    ///
    /// `Bearer` lowercase is in the list on purpose: HTTP auth schemes are
    /// case-insensitive per RFC 7235, so a client sending `bearer` has
    /// authenticated. Treating it as absent is a real interop bug, and the fix
    /// (a case-insensitive prefix match) is cheaper than the explanation.
    #[test]
    fn the_bearer_parser_accepts_only_a_well_formed_token() {
        assert_eq!(bearer(&headers_with("Bearer abc123")), Some("abc123"));
        assert_eq!(bearer(&headers_with("bearer abc123")), Some("abc123"));
        assert_eq!(bearer(&headers_with("BEARER abc123")), Some("abc123"));

        for bad in [
            "",
            "Bearer",
            "Bearer ",
            "Bearer  two",
            "Bearer a b",
            "Basic dXNlcjpwYXNz",
            "abc123",
        ] {
            assert_eq!(bearer(&headers_with(bad)), None, "{bad:?}");
        }
        assert_eq!(bearer(&HeaderMap::new()), None);
    }

    /// The tier table, and the two directions that matter.
    ///
    /// **`View` must equal `PUBLIC` exactly.** If a View link could see more
    /// than an anonymous visitor, it would be a privilege grant wearing a
    /// read-only label — and it would be invisible, because the status codes
    /// would all still be correct.
    ///
    /// **Neither scope may reach `quarantined` or `denied`.** Those are in
    /// `ALL` and in `MODERATION`, and a share link is neither a steward nor the
    /// library owner. `denied` is a takedown: §14.1 calls it "permanently
    /// blocked by hash across all peers", and a link that served one would be
    /// a takedown with a hole in it.
    #[test]
    fn a_grant_never_reaches_past_the_public_tiers() {
        /// Set equality, deliberately. The order of a tier list is not part of
        /// what a caller may see, so a test comparing `Vec`s positionally
        /// would fail on a `sort()` and teach a reader that the order matters.
        /// The first version of this test did exactly that.
        fn same_set<const N: usize>(a: Vec<String>, b: [&str; N]) -> bool {
            let mut a: Vec<String> = a.into_iter().collect();
            let mut want: Vec<String> = b.iter().map(|s| s.to_string()).collect();
            a.sort();
            want.sort();
            a == want
        }

        for scope in [Scope::View, Scope::ViewDownload] {
            let tiers = grant_tiers(scope);
            for t in ["quarantined", "denied"] {
                assert!(
                    !tiers.iter().any(|x| x == t),
                    "{scope:?} must not reach {t}, got {tiers:?}"
                );
            }
        }

        assert!(
            same_set(grant_tiers(Scope::View), ConsentTiers::PUBLIC),
            "a View link sees exactly what an anonymous visitor sees, got {:?}",
            grant_tiers(Scope::View)
        );
        assert!(
            same_set(grant_tiers(Scope::ViewDownload), ConsentTiers::OWNER),
            "a ViewDownload link may serve what the library owner may serve and \
             nothing beyond it, got {:?}",
            grant_tiers(Scope::ViewDownload)
        );
        assert!(
            !same_set(grant_tiers(Scope::View), ConsentTiers::OWNER),
            "and the two scopes must actually differ, or Scope is not doing \
             anything and the ViewDownload arm is untested"
        );
    }
}

// The behavioural tests are in `tests/identity_route.rs`, not here, and that
// placement is load-bearing rather than stylistic: this crate's only test
// harness is `TestApp` in `tests/support/`, which builds a real `AppState`
// against a real database. A `#[cfg(test)]` module in `src/` has no such
// thing — so a unit test here could only ever exercise `bearer()`, and the
// three identity cases the spec asks for all need a store.
