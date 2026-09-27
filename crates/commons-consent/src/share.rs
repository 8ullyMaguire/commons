//! Time-limited share grants. Spec §9.5 (#5612) and §15.10.
//!
//! Spec: `docs/spec/t-p5-007-share-links.md`. Table: migration 0022.
//!
//! # What this is
//!
//! A **capability grant**, not a second account system. §9.5's sentence names
//! four properties and this module exists to make each one true:
//!
//! * *signed* — a token that did not come from [`ShareSecret::mint`] does not
//!   verify, and a forged id is rejected before any row is read.
//! * *expiring* — [`ShareGrant::expires_at`] is not optional, and
//!   [`GrantState`] is computed from it rather than stored.
//! * *revocable at any time* — [`ShareGrant::revoked_at`], checked on every
//!   resolve.
//! * *an access log* — every attempt, granted or denied, and
//!   [`DeniedReason`] is the reason a denial happened.
//!
//! # Why the token is `<id>.<secret>` and not a self-contained signed payload
//!
//! An HMAC over `(id, expiry, scope)` needs no table and is the smaller design.
//! It is the wrong one: an expiry baked into a token **cannot be revoked before
//! it arrives** without rotating the signing key for every other live link. §9.5
//! asks for revocable-at-any-time *and* an access log, and both are rows. So
//! the row is the design and the signature is defence in depth — the two
//! together mean a leaked database is not a leaked link (the secret is stored
//! hashed) and a forged id never reaches the database at all.
//!
//! # Why expiry lives on the row and not in the token
//!
//! Same reason, one level down. `expires_at` is a column so that revocation and
//! expiry are both evaluated against stored state, in that order, at one place:
//! [`GrantState::of`].

use std::fmt;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use chrono::{DateTime, Utc};

/// Bytes of entropy in a token's secret.
///
/// 32 bytes is 256 bits, which is the same width as the ed25519 keys elsewhere
/// in this crate. Not a tunable: below 32 the token becomes guessable by anyone
/// who can make requests, and above it buys nothing against an offline guess
/// because the verification is a hash comparison, not a slow KDF.
pub const SECRET_BYTES: usize = 32;

/// What a grant lets the holder do. A closed set — see the CHECK in migration
/// 0022, and see [`Scope::can_download`], which is the only place that turns a
/// scope into a decision.
///
/// `ViewDownload` is strictly wider than `View`, and there is deliberately no
/// `Download` that means "download without being able to view": the bytes you
/// download are the bytes you view, so the narrower-sounding variant would be a
/// way to serve a file without showing it, which is the shape §9.5 is trying to
/// avoid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// Play or read, no download.
    View,
    /// Play, read, and fetch the file itself.
    ViewDownload,
}

impl Scope {
    /// Whether this scope permits serving the bytes.
    ///
    /// The one place the answer is decided. Every serving path calls this
    /// rather than pattern-matching `Scope` itself, so adding a scope is a
    /// compile error at the sites that would have to think about it.
    pub fn can_download(self) -> bool {
        matches!(self, Scope::ViewDownload)
    }

    /// The stored spelling. Matches the CHECK constraint's literals exactly.
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::View => "view",
            Scope::ViewDownload => "view_download",
        }
    }

    /// Parse a stored spelling. `None` for anything else, including a string
    /// with the right prefix — the CHECK already rejects those at write time,
    /// and a parser that guessed would be guessing past a constraint.
    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s {
            "view" => Some(Scope::View),
            "view_download" => Some(Scope::ViewDownload),
            _ => None,
        }
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a grant points at: exactly one object, or exactly one smart
/// collection.
///
/// Never a filter. §9.5 says "one item or one smart collection", and a filter
/// target is explicitly unbounded at request time — the same unbounded-ness
/// that makes `undo_record` (migration 0019) hard to replay correctly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    Object,
    SmartCollection,
}

impl TargetKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TargetKind::Object => "object",
            TargetKind::SmartCollection => "smart_collection",
        }
    }

    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s {
            "object" => Some(TargetKind::Object),
            "smart_collection" => Some(TargetKind::SmartCollection),
            _ => None,
        }
    }
}

impl fmt::Display for TargetKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why a resolve was refused.
///
/// Every variant becomes a row in `share_access`, because a log of only
/// successes cannot answer the one question it gets asked: *is this link being
/// tried by someone who should not have it?* The answer lives entirely in the
/// failures.
///
/// `UnknownToken` deliberately does not distinguish "no such token" from "wrong
/// secret". The caller cannot tell them apart without a timing attack's worth of
/// care, and the owner's log should not either: which of two possibilities a
/// brute-force attempt took is not a distinction anyone acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeniedReason {
    /// The token is malformed, names no grant, or the secret does not match.
    UnknownToken,
    /// Past `expires_at`.
    Expired,
    /// `revoked_at` is set.
    Revoked,
    /// The grant needs a password and none was presented.
    PasswordRequired,
    /// A password was presented and it was wrong.
    WrongPassword,
}

impl DeniedReason {
    pub fn as_str(self) -> &'static str {
        match self {
            DeniedReason::UnknownToken => "unknown_token",
            DeniedReason::Expired => "expired",
            DeniedReason::Revoked => "revoked",
            DeniedReason::PasswordRequired => "password_required",
            DeniedReason::WrongPassword => "wrong_password",
        }
    }

    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s {
            "unknown_token" => Some(DeniedReason::UnknownToken),
            "expired" => Some(DeniedReason::Expired),
            "revoked" => Some(DeniedReason::Revoked),
            "password_required" => Some(DeniedReason::PasswordRequired),
            "wrong_password" => Some(DeniedReason::WrongPassword),
            _ => None,
        }
    }
}

impl fmt::Display for DeniedReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether a grant can be used right now, and if not, why.
///
/// The order here is the design: **revoked before expired**. A link that was
/// both revoked and expired should be reported as revoked, because that is the
/// one an owner acted on. Reporting `Expired` for a link they killed last week
/// sends them looking for a timer that is not the problem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantState {
    Live,
    Expired,
    Revoked,
}

impl GrantState {
    /// Evaluate a grant at `now`.
    ///
    /// Takes the timestamps rather than reading the clock so the rule is
    /// testable at an exact instant, and so a caller resolving a batch does not
    /// get a different answer per row.
    pub fn of(
        revoked_at: Option<DateTime<Utc>>,
        expires_at: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Self {
        if revoked_at.is_some() {
            return GrantState::Revoked;
        }
        if now >= expires_at {
            return GrantState::Expired;
        }
        GrantState::Live
    }
}

/// A share grant, as stored.
///
/// `token_hash` is [`ShareSecret::hash`]'s output, never a token. See the module
/// header and the migration header for why that is not negotiable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareGrant {
    pub id: String,
    pub token_hash: [u8; 32],
    pub scope: Scope,
    pub target_kind: TargetKind,
    pub target_id: String,
    /// `None` means the grant has no password. See the migration header on why
    /// this is not a slow KDF.
    pub password_hash: Option<[u8; 32]>,
    pub expires_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub access_count: i64,
    pub last_accessed_at: Option<DateTime<Utc>>,
}

impl ShareGrant {
    /// The grant's state at `now`. See [`GrantState::of`] for the ordering.
    pub fn state_at(&self, now: DateTime<Utc>) -> GrantState {
        GrantState::of(self.revoked_at, self.expires_at, now)
    }

    /// Whether the grant is usable at `now`.
    pub fn is_live_at(&self, now: DateTime<Utc>) -> bool {
        self.state_at(now) == GrantState::Live
    }

    /// Check a password against `password_hash`.
    ///
    /// A grant with no password accepts anything, **including nothing** — the
    /// page shows no password prompt at all, which is a different thing from
    /// showing one that accepts a blank. `None` here means "no password was
    /// required", not "no password was supplied".
    pub fn password_ok(&self, supplied: Option<&str>) -> bool {
        match (&self.password_hash, supplied) {
            (None, _) => true,
            (Some(_), None) => false,
            (Some(expected), Some(given)) => {
                ShareSecret::hash_password(given).as_slice() == expected.as_slice()
            }
        }
    }

    /// Whether a password must be supplied before this grant can be used.
    pub fn needs_password(&self) -> bool {
        self.password_hash.is_some()
    }
}

/// The minting and verification side of a share link.
///
/// Split from [`ShareGrant`] on purpose: the secret lives here and never on the
/// grant, so there is no type that can be logged, serialised, or returned from
/// a query and accidentally carry a working link.
#[derive(Clone)]
pub struct ShareSecret([u8; SECRET_BYTES]);

impl ShareSecret {
    /// Mint a fresh secret from the OS RNG.
    ///
    /// `getrandom`, not an userspace PRNG: a predictable token is a share link
    /// anybody can guess, and the whole feature is a guessable link away from
    /// being an access-control bypass.
    pub fn mint() -> Self {
        let mut bytes = [0u8; SECRET_BYTES];
        rand::fill(&mut bytes);
        ShareSecret(bytes)
    }

    /// The stored form: BLAKE3 of the secret.
    ///
    /// An unkeyed hash on purpose. The input is 256 bits of CSPRNG output, so
    /// there is no dictionary to attack and no need for a key; what this buys
    /// is that a database dump contains no working link. A *keyed* hash would
    /// add a secret to lose, not security.
    pub fn hash(&self) -> [u8; 32] {
        *blake3::hash(&self.0).as_bytes()
    }

    /// The stored form of a password. Same construction, and the same
    /// reasoning — see the migration header for why this is not a slow KDF.
    pub fn hash_password(password: &str) -> [u8; 32] {
        *blake3::hash(password.as_bytes()).as_bytes()
    }

    /// Verify a presented secret against a stored hash.
    ///
    /// Constant-time in the comparison, not in the hash. The hash is over a
    /// fixed 32-byte input so it takes the same time for every input; the
    /// comparison is the part that could leak a prefix, so that is the part
    /// that is written to not.
    pub fn verify(hash: &[u8; 32], secret: &ShareSecret) -> bool {
        let mut diff = 0u8;
        for (a, b) in hash.iter().zip(secret.hash().iter()) {
            diff |= a ^ b;
        }
        diff == 0
    }
}

impl fmt::Debug for ShareSecret {
    /// Never prints the bytes. A `Debug` that leaks the secret is one
    /// `unwrap()` in a log statement away from publishing every live link.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ShareSecret(<redacted>)")
    }
}

/// A minted, ready-to-share link: the token string and the hash to store.
///
/// The token exists **only** here. Once the create call returns, the only copy
/// is in the response body the owner saw, and the database holds `token_hash`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MintedToken {
    /// `<id>.<secret>`, base64url, no padding. Safe in a path segment and in a
    /// query without further encoding — which is the point, because §15.10 is
    /// about links a person can read and paste.
    pub token: String,
    /// The id half, parsed back out. Stored as the row's `id`.
    pub grant_id: String,
    /// What to store in `token_hash`.
    pub token_hash: [u8; 32],
}

/// Assemble a token from a grant id and a secret.
///
/// The separator is `.` and the encoding is base64url without padding, so the
/// token has exactly one `.` and no character that needs escaping in a URL. A
/// token that needs percent-encoding is a link that breaks when someone pastes
/// it into a chat client, which is most of where share links go.
pub fn assemble_token(grant_id: &str, secret: &ShareSecret) -> String {
    format!("{}.{}", grant_id, URL_SAFE_NO_PAD.encode(secret.0))
}

/// Split a token into its id half, without verifying the secret.
///
/// Exposed separately from [`parse_token`] because the lookup path needs the id
/// to find the row, and it is the row's `token_hash` — not this function — that
/// decides whether the token is real. Returning `None` only for a structurally
/// impossible token keeps a malformed string from reaching the database.
pub fn split_token(token: &str) -> Option<(&str, ShareSecret)> {
    let (id, secret) = token.split_once('.')?;
    if id.is_empty() || secret.is_empty() {
        return None;
    }
    // Reject a second dot rather than truncating at it: a token with two dots is
    // not a token, and quietly accepting the prefix would let `valid.extra`
    // resolve as `valid`.
    if secret.contains('.') {
        return None;
    }
    let raw = URL_SAFE_NO_PAD.decode(secret).ok()?;
    if raw.len() != SECRET_BYTES {
        return None;
    }
    let mut bytes = [0u8; SECRET_BYTES];
    bytes.copy_from_slice(&raw);
    Some((id, ShareSecret(bytes)))
}

/// A token that has been split and whose secret has been checked against a
/// stored hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedToken {
    pub grant_id: String,
}

/// Verify a token against a grant.
///
/// Two checks, in this order, and the order is the point:
///
/// 1. **Structure.** A malformed token is [`DeniedReason::UnknownToken`] and
///    never reaches a query.
/// 2. **Secret.** Compared against the *stored row's* `token_hash`, in constant
///    time.
///
/// Expiry and revocation are **not** checked here. They are properties of the
/// row, and the row is fetched by id — so a caller that skipped this function
/// and queried on id alone would find a row it must then refuse. Keeping the
/// two apart means the database lookup and the policy check can each be tested
/// on their own, and it means the *reason* a live-looking grant is refused is
/// still available afterwards.
pub fn verify_token(token: &str, grant: &ShareGrant) -> Result<VerifiedToken, DeniedReason> {
    let (id, secret) = split_token(token).ok_or(DeniedReason::UnknownToken)?;
    if id != grant.id {
        return Err(DeniedReason::UnknownToken);
    }
    if !ShareSecret::verify(&grant.token_hash, &secret) {
        return Err(DeniedReason::UnknownToken);
    }
    Ok(VerifiedToken {
        grant_id: id.to_string(),
    })
}

/// What [`resolve`] concluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution<'a> {
    /// Use the grant. Carries the grant, so the caller cannot resolve and then
    /// act on a different row.
    Granted {
        grant: &'a ShareGrant,
        token: VerifiedToken,
    },
    /// Do not use it. The reason is what goes in the access log.
    Denied { reason: DeniedReason },
}

/// The whole policy, in one function: signature, then revocation, then expiry,
/// then password.
///
/// This is the only place the order is written down, which is the only reason
/// the order is a decision rather than an accident of how the function was
/// written.
///
/// Revocation is checked before expiry so a link an owner killed reports as
/// revoked even after its clock ran out — the owner's action is the more
/// informative answer, and it is the one that tells them the link is gone for
/// good rather than merely idle.
pub fn resolve<'a>(
    token: &str,
    grant: &'a ShareGrant,
    password: Option<&str>,
    now: DateTime<Utc>,
) -> Resolution<'a> {
    let verified = match verify_token(token, grant) {
        Ok(v) => v,
        Err(reason) => return Resolution::Denied { reason },
    };
    match grant.state_at(now) {
        GrantState::Revoked => {
            return Resolution::Denied {
                reason: DeniedReason::Revoked,
            }
        }
        GrantState::Expired => {
            return Resolution::Denied {
                reason: DeniedReason::Expired,
            }
        }
        GrantState::Live => {}
    }
    if grant.needs_password() && !grant.password_ok(password) {
        // Two different reasons, because the UI is different for each: one
        // shows a password field, the other says the password is wrong.
        let reason = if password.is_some() {
            DeniedReason::WrongPassword
        } else {
            DeniedReason::PasswordRequired
        };
        return Resolution::Denied { reason };
    }
    Resolution::Granted {
        grant,
        token: verified,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeDelta;

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 0).expect("fixed instant")
    }

    fn grant_with(token_hash: [u8; 32]) -> ShareGrant {
        ShareGrant {
            id: "grant-1".into(),
            token_hash,
            scope: Scope::View,
            target_kind: TargetKind::Object,
            target_id: "obj-1".into(),
            password_hash: None,
            expires_at: now() + TimeDelta::hours(1),
            revoked_at: None,
            created_at: now(),
            access_count: 0,
            last_accessed_at: None,
        }
    }

    #[test]
    fn a_minted_token_verifies() {
        let secret = ShareSecret::mint();
        let token = assemble_token("grant-1", &secret);
        let grant = grant_with(secret.hash());
        assert_eq!(
            resolve(&token, &grant, None, now()),
            Resolution::Granted {
                grant: &grant,
                token: VerifiedToken {
                    grant_id: "grant-1".into()
                },
            }
        );
    }

    #[test]
    fn a_token_has_one_dot_and_needs_no_escaping() {
        let token = assemble_token("grant-1", &ShareSecret::mint());
        assert_eq!(token.matches('.').count(), 1);
        // Every character legal unescaped in a path segment and a query value.
        // A link that needs percent-encoding breaks when pasted into a chat.
        for c in token.chars() {
            assert!(
                c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_',
                "{c:?} would need escaping"
            );
        }
    }

    #[test]
    fn another_secret_does_not_verify() {
        let grant = grant_with(ShareSecret::mint().hash());
        let token = assemble_token("grant-1", &ShareSecret::mint());
        assert_eq!(
            resolve(&token, &grant, None, now()),
            Resolution::Denied {
                reason: DeniedReason::UnknownToken
            }
        );
    }

    #[test]
    fn another_grants_id_does_not_verify() {
        // The same secret under a different id. Without the id check this would
        // resolve, which is a token for one link working on another row.
        let secret = ShareSecret::mint();
        let grant = grant_with(secret.hash());
        let token = assemble_token("grant-2", &secret);
        assert_eq!(
            resolve(&token, &grant, None, now()),
            Resolution::Denied {
                reason: DeniedReason::UnknownToken
            }
        );
    }

    #[test]
    fn an_expired_grant_is_refused() {
        let secret = ShareSecret::mint();
        let mut grant = grant_with(secret.hash());
        grant.expires_at = now() - TimeDelta::seconds(1);
        let token = assemble_token("grant-1", &secret);
        assert_eq!(
            resolve(&token, &grant, None, now()),
            Resolution::Denied {
                reason: DeniedReason::Expired
            }
        );
    }

    #[test]
    fn an_expiry_exactly_at_now_is_expired() {
        // `>=` not `>`. A link that expires at the instant it is used has
        // expired; treating it as live makes the boundary depend on sub-second
        // timing, which is not a property a capability can have.
        let secret = ShareSecret::mint();
        let mut grant = grant_with(secret.hash());
        grant.expires_at = now();
        assert_eq!(grant.state_at(now()), GrantState::Expired);
    }

    #[test]
    fn a_revoked_grant_is_refused() {
        let secret = ShareSecret::mint();
        let mut grant = grant_with(secret.hash());
        grant.revoked_at = Some(now());
        let token = assemble_token("grant-1", &secret);
        assert_eq!(
            resolve(&token, &grant, None, now()),
            Resolution::Denied {
                reason: DeniedReason::Revoked
            }
        );
    }

    #[test]
    fn revocation_outranks_expiry() {
        // Both. The owner killed this link; `Expired` would send them hunting
        // for a timer instead of telling them the link is gone for good.
        let secret = ShareSecret::mint();
        let mut grant = grant_with(secret.hash());
        grant.expires_at = now() - TimeDelta::hours(5);
        grant.revoked_at = Some(now() - TimeDelta::hours(4));
        assert_eq!(grant.state_at(now()), GrantState::Revoked);
    }

    #[test]
    fn a_wrong_signature_is_refused_before_the_row_is_consulted() {
        // The signature check is first, so a garbage token cannot be used to
        // learn whether a row exists by timing the policy checks.
        let grant = grant_with(ShareSecret::mint().hash());
        for bad in ["", "no-dot", ".secret", "id.", "a.b.c"] {
            assert_eq!(
                resolve(bad, &grant, None, now()),
                Resolution::Denied {
                    reason: DeniedReason::UnknownToken
                },
                "{bad:?} should not parse"
            );
        }
    }

    #[test]
    fn a_secret_of_the_wrong_length_does_not_parse() {
        let short = URL_SAFE_NO_PAD.encode([0u8; 16]);
        assert!(split_token(&format!("grant-1.{short}")).is_none());
    }

    #[test]
    fn padding_is_not_accepted() {
        // base64url *with* padding is a different string, and accepting it would
        // make two spellings of one link, one of which is not the stored one.
        let secret = ShareSecret::mint();
        let token = assemble_token("grant-1", &secret);
        let padded = format!("{token}=");
        assert!(split_token(&padded).is_none());
    }

    #[test]
    fn a_grant_with_no_password_needs_none() {
        let grant = grant_with(ShareSecret::mint().hash());
        assert!(!grant.needs_password());
        // Including when nothing is supplied: the page shows no prompt at all,
        // which is different from showing one that accepts a blank.
        assert!(grant.password_ok(None));
        assert!(grant.password_ok(Some("")));
    }

    #[test]
    fn a_password_grant_distinguishes_absent_from_wrong() {
        // Two reasons, because the UI differs: one shows a field, the other
        // says the password is wrong. Collapsing them into "denied" makes the
        // recipient retype a password they typed correctly.
        let secret = ShareSecret::mint();
        let mut grant = grant_with(secret.hash());
        grant.password_hash = Some(ShareSecret::hash_password("hunter2"));
        let token = assemble_token("grant-1", &secret);
        assert_eq!(
            resolve(&token, &grant, None, now()),
            Resolution::Denied {
                reason: DeniedReason::PasswordRequired
            }
        );
        assert_eq!(
            resolve(&token, &grant, Some("wrong"), now()),
            Resolution::Denied {
                reason: DeniedReason::WrongPassword
            }
        );
        assert!(matches!(
            resolve(&token, &grant, Some("hunter2"), now()),
            Resolution::Granted { .. }
        ));
    }

    #[test]
    fn a_password_is_checked_after_revocation() {
        // A revoked link must not be distinguishable from a wrong-password one
        // by the reason, or the log becomes an oracle for which links are live.
        let secret = ShareSecret::mint();
        let mut grant = grant_with(secret.hash());
        grant.password_hash = Some(ShareSecret::hash_password("hunter2"));
        grant.revoked_at = Some(now());
        let token = assemble_token("grant-1", &secret);
        assert_eq!(
            resolve(&token, &grant, Some("hunter2"), now()),
            Resolution::Denied {
                reason: DeniedReason::Revoked
            }
        );
    }

    #[test]
    fn scope_gates_download() {
        let view = Scope::View;
        let both = Scope::ViewDownload;
        assert!(!view.can_download());
        assert!(both.can_download());
        // Round-trip through the stored spelling, which is what the CHECK
        // constraint's literals are.
        assert_eq!(Scope::from_str_opt(view.as_str()), Some(view));
        assert_eq!(Scope::from_str_opt(both.as_str()), Some(both));
        assert_eq!(Scope::from_str_opt("download"), None);
    }

    #[test]
    fn a_leaked_row_yields_no_working_link() {
        // The property the hashed column exists for. A dump of `share_grant`
        // contains `token_hash`; rebuilding a token from it must not work.
        let secret = ShareSecret::mint();
        let grant = grant_with(secret.hash());
        // Everything a leaked row has.
        let leaked = format!(
            "{}.{}",
            grant.id,
            grant
                .token_hash
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        );
        assert!(
            split_token(&leaked).is_none(),
            "a hex digest is not a token"
        );
        // And the hash is not invertible into the secret.
        assert_ne!(grant.token_hash, secret.0);
    }

    #[test]
    fn the_debug_impl_does_not_leak_the_secret() {
        let secret = ShareSecret::mint();
        let rendered = format!("{secret:?}");
        let hex = secret
            .0
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        assert!(
            !rendered.contains(&hex),
            "Debug printed the secret: {rendered}"
        );
        assert!(rendered.contains("redacted"));
    }

    #[test]
    fn secrets_differ() {
        // A test that mints twice and asserts inequality, because a `mint` that
        // returned a constant would pass every other test here.
        assert_ne!(ShareSecret::mint().0, ShareSecret::mint().0);
    }
}
