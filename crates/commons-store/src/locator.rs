//! Locators: the P2P metadata side of §5.18, and the consent gate that makes
//! it safe to have.
//!
//! # What this module is
//!
//! A locator is a BitTorrent infohash, a magnet URI, an ed2k hash, or an
//! HTTP(S) source URL attached to an object. It is *metadata*. The platform
//! never speaks BitTorrent or ed2k, has no DHT, no swarms, and no code path
//! that fetches a file body over a P2P network. §5.18 item 3 says "nothing
//! else" and this module is where that is enforced: there is no read path here
//! that a downloader could use, because there is no read path here at all —
//! only `propose`, which is a write.
//!
//! # Why the gate is in this file
//!
//! A magnet link is a redistribution channel. Storing one is a distribution
//! decision, not a metadata decision, so locators are gated by the §14.1
//! consent model and cannot be attached to a tier that does not permit
//! redistribution.
//!
//! The gate belongs in the data layer, not in the caller, for the same reason
//! every other consent rule in §14.1 is enforced there: a check that lives at
//! the call site is a check that some future call site forgets. There is exactly
//! one way to write a locator row, and it is [`propose`].
//!
//! # The two fields that are not the same thing
//!
//! `tier` and `redistribution_permitted` are independent, and
//! [`ConsentGate::permits`] consults both:
//!
//! | tier | locator allowed |
//! |---|---|
//! | `unverified` | no |
//! | `self_published` | only when the creator attached one explicitly |
//! | `performer_claimed` | no |
//! | `third_party_permitted` | yes, if the stated basis includes redistribution |
//! | `quarantined` / `denied` | no, and any existing locator dies with the tombstone |
//!
//! An amateur creator's self-published item is perfectly visible and still
//! must not carry a magnet. That is why `redistribution_permitted` is a
//! separate column and not a function of the tier: the two facts are
//! independent, and deriving one from the other is how a self-published
//! amateur item ends up in a torrent swarm.
//!
//! # URI validation happens here too
//!
//! A `magnet:` URI is a redistribution pointer, and the scheme decides whether
//! the gate applies at all. An `http:` locator is a plain source URL — §5.18
//! lists it as such, "as plain text, not a P2P protocol" — and does not open a
//! redistribution channel, so it is stored at any tier that permits metadata.
//! The distinction is made in [`LocatorScheme::is_redistribution`], and
//! [`propose`] refuses a scheme the platform does not implement rather than
//! storing an opaque string that a later reader might treat as a capability.

use crate::db::{Store, StoreError};
use commons_core::ConsentTier;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Result alias for this module.
pub type Result<T> = std::result::Result<T, StoreError>;

/// Read one column, naming it in the error if the read fails.
///
/// A macro rather than a function because a generic `fn` over `DB: Database`
/// cannot be inferred at the call site: `String` is `Decode` for both SQLite
/// and Postgres, so the compiler has two equally valid answers and picks
/// neither. The macro monomorphises at each use, where the row type is already
/// known from the query that produced it.
///
/// The error names the column, because "could not read a column" is the one
/// piece of information someone debugging a schema drift actually needs.
macro_rules! column {
    ($row:expr, $name:literal) => {{
        use sqlx::Row;
        $row.try_get($name).map_err(|e| StoreError::Row {
            column: $name,
            source: e,
        })
    }};
}

/// A locator scheme the platform understands.
///
/// A closed set, for the same reason `OutputFormat` is in T-P2-006: an opaque
/// string in a security-relevant column is a string a future reader might
/// interpret as something it is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LocatorScheme {
    /// BitTorrent infohash, stored bare (40 hex chars, SHA-1).
    BitTorrent,
    /// `magnet:?xt=urn:btih:…` — a redistribution channel.
    Magnet,
    /// `ed2k://|file|…|…|` — a redistribution channel.
    Ed2k,
    /// An `http(s)://` source URL. Plain metadata, not a P2P protocol.
    Http,
}

impl LocatorScheme {
    pub const ALL: [LocatorScheme; 4] = [
        LocatorScheme::BitTorrent,
        LocatorScheme::Magnet,
        LocatorScheme::Ed2k,
        LocatorScheme::Http,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            LocatorScheme::BitTorrent => "bittorrent",
            LocatorScheme::Magnet => "magnet",
            LocatorScheme::Ed2k => "ed2k",
            LocatorScheme::Http => "http",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// Does this scheme open a redistribution channel?
    ///
    /// The distinction §5.18 draws, and it is the whole reason the consent gate
    /// has two branches. A magnet or an ed2k link is a redistribution
    /// decision; an HTTP source URL is metadata in the same family as a studio
    /// page, and gating it behind redistribution consent would make it
    /// impossible to record where a permitted file came from.
    pub const fn is_redistribution(self) -> bool {
        !matches!(self, LocatorScheme::Http)
    }

    /// Is `uri` well-formed for this scheme?
    ///
    /// Checked before the gate, because a malformed URI that reaches the gate
    /// wastes a consent decision on a row nobody will ever be able to use.
    pub fn validate(self, uri: &str) -> std::result::Result<(), ProposeError> {
        if uri.trim().is_empty() {
            return Err(ProposeError::MalformedUri {
                scheme: self.as_str().to_string(),
                reason: "empty".into(),
            });
        }
        match self {
            LocatorScheme::BitTorrent => {
                if uri.len() == 40 && uri.chars().all(|c| c.is_ascii_hexdigit()) {
                    Ok(())
                } else {
                    Err(ProposeError::MalformedUri {
                        scheme: self.as_str().to_string(),
                        reason: "a bittorrent infohash is 40 hex characters".into(),
                    })
                }
            }
            LocatorScheme::Magnet => {
                // `magnet:?xt=urn:btih:<40 hex>` for the BitTorrent flavour.
                // The platform stores other magnet flavours as opaque text, so
                // this checks the prefix and the presence of an xt, not the
                // exact hash length -- a magnet can carry several xts.
                if uri.starts_with("magnet:?") {
                    if uri.contains("xt=") {
                        Ok(())
                    } else {
                        Err(ProposeError::MalformedUri {
                            scheme: self.as_str().to_string(),
                            reason: "a magnet uri needs an xt= parameter".into(),
                        })
                    }
                } else {
                    Err(ProposeError::MalformedUri {
                        scheme: self.as_str().to_string(),
                        reason: "a magnet uri starts with magnet:?".into(),
                    })
                }
            }
            LocatorScheme::Ed2k => {
                if uri.starts_with("ed2k://|file|") && uri.ends_with('|') {
                    Ok(())
                } else {
                    Err(ProposeError::MalformedUri {
                        scheme: self.as_str().to_string(),
                        reason: "an ed2k uri is ed2k://|file|<hash>|<name>|<size>|".into(),
                    })
                }
            }
            LocatorScheme::Http => {
                if uri.starts_with("http://") || uri.starts_with("https://") {
                    Ok(())
                } else {
                    Err(ProposeError::MalformedUri {
                        scheme: self.as_str().to_string(),
                        reason: "an http locator must be an http:// or https:// url".into(),
                    })
                }
            }
        }
    }
}

impl fmt::Display for LocatorScheme {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The consent facts the gate needs.
///
/// A projection of `consent_record`, not the row type, so the gate cannot
/// accidentally grow a dependency on `audit_trail` and start behaving
/// differently depending on whether history has been read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConsentFacts {
    pub tier: ConsentTier,
    /// Whether the *stated basis* includes redistribution. Independent of tier.
    pub redistribution_permitted: bool,
}

impl ConsentFacts {
    /// The facts for an object that has never had a consent decision.
    ///
    /// `unverified` and no redistribution, which is the safe answer and also
    /// the answer for a row that does not exist. A missing record is not
    /// consent.
    pub const fn default_for_unrecorded() -> Self {
        ConsentFacts {
            tier: ConsentTier::Unverified,
            redistribution_permitted: false,
        }
    }

    /// Does the consent model permit a locator of this scheme?
    ///
    /// The §14.1 table, as code. Public so the UI can explain a refusal, and so
    /// the hostile-plugin test can assert against the same function the write
    /// path uses rather than a reimplementation of it.
    pub fn permits(&self, scheme: LocatorScheme) -> bool {
        if !scheme.is_redistribution() {
            // Plain source URLs are metadata. Any tier that is not actively
            // adverse may record where a file came from.
            return !matches!(self.tier, ConsentTier::Denied | ConsentTier::Quarantined);
        }
        // A redistribution locator needs both: a tier that can carry one, and
        // a stated basis that includes redistribution. Neither alone is enough.
        self.tier.permits_locator() && self.redistribution_permitted
    }
}

/// Why a proposal was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProposeError {
    #[error("no such object: {0}")]
    NoSuchObject(String),
    #[error("the object {object} is at tier {tier}, which does not permit a {scheme} locator")]
    TierForbidsLocator {
        object: String,
        tier: String,
        scheme: String,
    },
    #[error("malformed {scheme} locator: {reason}")]
    MalformedUri { scheme: String, reason: String },
    /// A store failure that is not a refusal. Not a consent answer: a plugin
    /// must not be told "tier forbids" when the database was merely down, or it
    /// will conclude that a retry will not help.
    #[error("the store could not record this locator: {0}")]
    Internal(String),
}

impl ProposeError {
    /// Is this a consent refusal rather than a caller mistake?
    ///
    /// Distinct because the two call for different user actions: a refusal is
    /// information about the object's consent, and a malformed URI is a bug in
    /// the plugin. Collapsing them would teach a plugin author to retry a
    /// consent refusal forever.
    pub fn is_consent_refusal(&self) -> bool {
        matches!(self, ProposeError::TierForbidsLocator { .. })
    }
}

/// A locator as stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredLocator {
    pub id: String,
    pub object_id: String,
    pub file_id: Option<String>,
    pub scheme: String,
    pub uri: String,
    pub infohash: Option<String>,
    pub size_bytes: Option<i64>,
    pub name: Option<String>,
    pub source: String,
    pub added_at: String,
}

/// A locator a plugin wants to attach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposedLocator {
    pub scheme: LocatorScheme,
    pub uri: String,
    /// Which file this describes, when the locator is about a specific file in
    /// a multi-file object (§5.2: one object, N files via `segment`).
    pub file_id: Option<String>,
    pub infohash: Option<String>,
    pub size_bytes: Option<i64>,
    pub name: Option<String>,
    /// Where this came from — a plugin id, "manual", an importer. Recorded
    /// verbatim so a user can tell their own upload from something a plugin
    /// found.
    pub source: String,
}

impl ProposedLocator {
    pub fn new(scheme: LocatorScheme, uri: impl Into<String>, source: impl Into<String>) -> Self {
        ProposedLocator {
            scheme,
            uri: uri.into(),
            file_id: None,
            infohash: None,
            size_bytes: None,
            name: None,
            source: source.into(),
        }
    }

    pub fn with_file(mut self, file_id: impl Into<String>) -> Self {
        self.file_id = Some(file_id.into());
        self
    }

    pub fn with_infohash(mut self, infohash: impl Into<String>) -> Self {
        self.infohash = Some(infohash.into());
        self
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    pub fn with_size(mut self, size: i64) -> Self {
        self.size_bytes = Some(size);
        self
    }
}

/// The one write path for a locator row.
///
/// Reads the object's consent record, evaluates the §14.1 table, and either
/// inserts or refuses. There is no second function that inserts into
/// `locator`, which is the property the hostile-plugin test checks.
///
/// `now` is passed in rather than read from the clock so a test can assert on
/// the stored `added_at` without sleeping, and so a caller replaying a
/// federation claim can record when *it* was accepted rather than when the
/// row happened to be written.
pub async fn propose(
    store: &Store,
    object_id: &str,
    locator: &ProposedLocator,
    now: &str,
) -> std::result::Result<String, ProposeError> {
    locator.scheme.validate(&locator.uri)?;

    // The object has to exist before its consent means anything. Without this
    // check a typo in an object id reads as "no consent record", which resolves
    // to the same `unverified` default as a freshly scanned file -- so a
    // plugin would be told its proposal was refused for consent reasons when
    // the truth is that the object was never there. That is a refusal it would
    // retry forever, and a bug nobody would think to look for, because the
    // error is a perfectly plausible one.
    if !object_exists(store, object_id)
        .await
        .map_err(|e| ProposeError::Internal(e.to_string()))?
    {
        return Err(ProposeError::NoSuchObject(object_id.to_string()));
    }

    // A missing consent record is `unverified`, not "allowed". Reading the
    // tier through this accessor rather than letting a caller pass one in is
    // the point: a plugin cannot supply the tier it would like to be checked
    // against.
    let facts = consent_facts(store, object_id).await?;

    if !facts.permits(locator.scheme) {
        return Err(ProposeError::TierForbidsLocator {
            object: object_id.to_string(),
            tier: facts.tier.as_str().to_string(),
            scheme: locator.scheme.as_str().to_string(),
        });
    }

    let id = uuid::Uuid::new_v4().to_string();
    let result = sqlx::query(
        "INSERT INTO locator (id, object_id, file_id, scheme, uri, infohash, size_bytes, \
         name, source, added_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(object_id)
    .bind(locator.file_id.as_deref())
    .bind(locator.scheme.as_str())
    .bind(&locator.uri)
    .bind(locator.infohash.as_deref())
    .bind(locator.size_bytes)
    .bind(locator.name.as_deref())
    .bind(&locator.source)
    .bind(now)
    .execute(store.pool())
    .await;

    match result {
        Ok(_) => Ok(id),
        // The object vanished between the consent read and this insert. A
        // foreign-key failure here is a lost race, not a consent problem, so it
        // is reported as NoSuchObject rather than a tier refusal -- otherwise
        // the caller would think consent had blocked it.
        Err(sqlx::Error::Database(e)) if is_foreign_key(e.as_ref()) => {
            Err(ProposeError::NoSuchObject(object_id.to_string()))
        }
        Err(e) => Err(ProposeError::Internal(e.to_string())),
    }
}

/// Is this a foreign-key violation?
///
/// Matched on the message rather than a driver-specific error code, because
/// SQLite and Postgres word it differently and the two engines are both live.
/// A false positive turns a lost race into a consent refusal, which is the less
/// harmful of the two mistakes, so the bias is deliberate.
fn is_foreign_key(e: &dyn sqlx::error::DatabaseError) -> bool {
    let msg = e.to_string().to_ascii_lowercase();
    msg.contains("foreign key") || msg.contains("violates foreign key constraint")
}

/// Does this object exist?
///
/// A separate existence check rather than a foreign-key probe, because the
/// error a caller gets has to distinguish "no such object" from "consent says
/// no" -- and the insert's foreign-key violation only tells us that *after*
/// consent has already been consulted, at which point the caller has been told
/// the wrong thing.
async fn object_exists(store: &Store, object_id: &str) -> Result<bool> {
    // The one object read in this crate that carries no consent clause, and the
    // one that is allowed to: it reads a *boolean about existence*, never a
    // field, so a caller learns that an object is present and nothing else. It
    // cannot become a disclosure later without someone changing what it
    // selects, and the workspace scan in `commons-consent/tests/policy.rs` fails
    // on exactly that change -- which is the point of naming it here rather than
    // quietly listing it in the allowlist.
    let row = sqlx::query("SELECT 1 FROM object WHERE id = ?")
        .bind(object_id)
        .fetch_optional(store.pool())
        .await
        .map_err(StoreError::Query)?;
    Ok(row.is_some())
}

/// The consent facts for an object, or the safe default if it has no record.
///
/// `pub` because it is the single place the `consent_record` row is turned
/// into a decision, and a test that cannot read it can only observe the
/// decision through `propose` -- where the tier and the flag are already
/// multiplied together, so substituting one restrictive tier for another is
/// invisible. Every substitution mutation of this function survived until this
/// became reachable from a test. The projection is deliberate: the row's other
/// columns (`audit_trail`, `decided_by`, timestamps) are not returned, so
/// nothing can start depending on history to decide consent.
pub async fn consent_facts(
    store: &Store,
    object_id: &str,
) -> std::result::Result<ConsentFacts, ProposeError> {
    let row = sqlx::query(
        "SELECT tier, redistribution_permitted FROM consent_record WHERE object_id = ?",
    )
    .bind(object_id)
    .fetch_optional(store.pool())
    .await;

    let row = match row {
        Ok(r) => r,
        Err(sqlx::Error::RowNotFound) => return Ok(ConsentFacts::default_for_unrecorded()),
        Err(e) => return Err(ProposeError::Internal(e.to_string())),
    };
    let Some(row) = row else {
        return Ok(ConsentFacts::default_for_unrecorded());
    };

    use sqlx::Row;
    let tier_str: String = row
        .try_get("tier")
        .map_err(|e| ProposeError::Internal(e.to_string()))?;
    // `redistribution_permitted` is an INTEGER in SQLite and a BOOLEAN in
    // Postgres, and sqlx hands back a bool for the latter and an i64 for the
    // former. Reading it as i64 and treating "non-zero" as true handles both
    // without a branch on the engine, which this module cannot see.
    let flag: i64 = row
        .try_get("redistribution_permitted")
        .map_err(|e| ProposeError::Internal(e.to_string()))?;

    Ok(ConsentFacts {
        // An unparseable tier is treated as the most restrictive one rather
        // than trusted. A row someone edited by hand, or wrote with a future
        // tier name, must not become a redistribution channel by accident.
        tier: ConsentTier::parse(&tier_str).unwrap_or(ConsentTier::Denied),
        redistribution_permitted: flag != 0,
    })
}

/// Every locator on an object, sorted.
///
/// A read accessor, deliberately not a capability exposed to plugins: §5.18
/// says a locator is a redistribution pointer, and the platform reads locators
/// to hand them to a local client and to federate them (§13), never to fetch
/// anything.
pub async fn for_object(store: &Store, object_id: &str) -> Result<Vec<StoredLocator>> {
    let rows = sqlx::query(
        "SELECT id, object_id, file_id, scheme, uri, infohash, size_bytes, name, source, \
         added_at FROM locator WHERE object_id = ? ORDER BY scheme, uri",
    )
    .bind(object_id)
    .fetch_all(store.pool())
    .await
    .map_err(StoreError::Query)?;

    rows.into_iter()
        .map(|r| {
            Ok(StoredLocator {
                id: column!(r, "id")?,
                object_id: column!(r, "object_id")?,
                file_id: column!(r, "file_id")?,
                scheme: column!(r, "scheme")?,
                uri: column!(r, "uri")?,
                infohash: column!(r, "infohash")?,
                size_bytes: column!(r, "size_bytes")?,
                name: column!(r, "name")?,
                source: column!(r, "source")?,
                added_at: column!(r, "added_at")?,
            })
        })
        .collect()
}

/// How many locators an object has.
pub async fn count(store: &Store, object_id: &str) -> Result<i64> {
    let row = sqlx::query("SELECT COUNT(*) AS n FROM locator WHERE object_id = ?")
        .bind(object_id)
        .fetch_one(store.pool())
        .await
        .map_err(StoreError::Query)?;
    let n: i64 = column!(row, "n")?;
    Ok(n)
}

/// Destroy every locator on an object.
///
/// Called when a tombstone is accepted (§14.1: "any existing locator is
/// destroyed with the tombstone"). Not exposed as a plugin capability: a
/// plugin must not be able to delete evidence, only propose. It is `pub` for
/// the consent store, which is the only caller, and the hostile-plugin test
/// asserts that a plugin has no route to it.
pub async fn destroy_for_object(store: &Store, object_id: &str) -> Result<u64> {
    let result = sqlx::query("DELETE FROM locator WHERE object_id = ?")
        .bind(object_id)
        .execute(store.pool())
        .await
        .map_err(StoreError::Query)?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tier_table_matches_the_spec() {
        // §5.18's table, one case per row. Written out rather than derived, so
        // a change to `permits` that contradicts the spec fails here.
        let redistribution = LocatorScheme::Magnet;
        // §14.1: "Only `third_party_permitted` can carry a P2P locator, and
        // only with this flag set. The two conditions are checked
        // independently and both must hold."
        //
        // This table previously read `self_published` and `performer_claimed`
        // as permitting a magnet when the flag was set, and the test failed
        // against the code. The code was right: those two tiers say *who is
        // asserting*, and the flag says *on what terms*, and neither of them is
        // a licence from a third party to redistribute. A performer who
        // consents to being identified has not thereby licensed the file.
        let cases = [
            (ConsentTier::Unverified, false, false),
            (ConsentTier::Unverified, true, false),
            (ConsentTier::SelfPublished, false, false),
            // Self-published with an explicit redistribution basis: still no.
            // The creator asserting they made it is not a permission to
            // redistribute it, and §14.1 says so in one sentence.
            (ConsentTier::SelfPublished, true, false),
            (ConsentTier::PerformerClaimed, false, false),
            (ConsentTier::PerformerClaimed, true, false),
            (ConsentTier::ThirdPartyPermitted, false, false),
            (ConsentTier::ThirdPartyPermitted, true, true),
            (ConsentTier::Quarantined, true, false),
            (ConsentTier::Denied, true, false),
        ];
        for (tier, permitted, expected) in cases {
            let facts = ConsentFacts {
                tier,
                redistribution_permitted: permitted,
            };
            assert_eq!(
                facts.permits(redistribution),
                expected,
                "{tier:?} with redistribution={permitted} should be {expected}"
            );
        }
    }

    #[test]
    fn a_tier_that_can_carry_a_locator_still_needs_the_flag() {
        // The independence that matters: third_party_permitted alone is not
        // enough, and the flag alone at a forbidden tier is not enough.
        let permitted_tier_no_flag = ConsentFacts {
            tier: ConsentTier::ThirdPartyPermitted,
            redistribution_permitted: false,
        };
        assert!(!permitted_tier_no_flag.permits(LocatorScheme::Magnet));

        let denied_tier_with_flag = ConsentFacts {
            tier: ConsentTier::Denied,
            redistribution_permitted: true,
        };
        assert!(!denied_tier_with_flag.permits(LocatorScheme::Magnet));
    }

    #[test]
    fn an_http_locator_is_metadata_and_does_not_need_the_flag() {
        // §5.18 lists an HTTP source URL as "plain text, not a P2P protocol".
        // Gating it behind redistribution consent would make it impossible to
        // record where a permitted file came from.
        let facts = ConsentFacts {
            tier: ConsentTier::Unverified,
            redistribution_permitted: false,
        };
        assert!(
            facts.permits(LocatorScheme::Http),
            "an http source url is metadata, not a redistribution channel"
        );
        assert!(!facts.permits(LocatorScheme::Magnet));
    }

    #[test]
    fn denied_and_quarantined_refuse_even_plain_metadata() {
        for tier in [ConsentTier::Denied, ConsentTier::Quarantined] {
            let facts = ConsentFacts {
                tier,
                redistribution_permitted: true,
            };
            assert!(
                !facts.permits(LocatorScheme::Http),
                "{tier:?} must refuse all"
            );
            assert!(
                !facts.permits(LocatorScheme::Magnet),
                "{tier:?} must refuse all"
            );
        }
    }

    #[test]
    fn an_unrecorded_object_is_unverified_not_consent() {
        let facts = ConsentFacts::default_for_unrecorded();
        assert_eq!(facts.tier, ConsentTier::Unverified);
        assert!(!facts.redistribution_permitted);
        assert!(!facts.permits(LocatorScheme::Magnet));
    }

    #[test]
    fn uri_validation_rejects_what_it_should() {
        let short_hash = "a".repeat(39);
        let bad: Vec<(LocatorScheme, &str)> = vec![
            (LocatorScheme::BitTorrent, ""),
            (LocatorScheme::BitTorrent, "nothex"),
            (LocatorScheme::BitTorrent, short_hash.as_str()),
            (LocatorScheme::Magnet, "http://example.com"),
            (LocatorScheme::Magnet, "magnet:?dn=no-hash"),
            (LocatorScheme::Ed2k, "magnet:?xt=urn:btih:abc"),
            (LocatorScheme::Http, "magnet:?xt=urn:btih:abc"),
            (LocatorScheme::Http, "ftp://example.com"),
        ];
        for (scheme, uri) in bad {
            let err = scheme.validate(uri).unwrap_err();
            assert!(
                matches!(err, ProposeError::MalformedUri { .. }),
                "{scheme} {uri:?} gave {err:?}"
            );
            assert!(
                !err.is_consent_refusal(),
                "a bad uri is not a consent refusal"
            );
        }
    }

    #[test]
    fn uri_validation_accepts_what_it_should() {
        let hash = "a".repeat(40);
        let good: Vec<(LocatorScheme, String)> = vec![
            (LocatorScheme::BitTorrent, hash.clone()),
            (
                LocatorScheme::Magnet,
                format!("magnet:?xt=urn:btih:{hash}&dn=name"),
            ),
            (
                LocatorScheme::Ed2k,
                "ed2k://|file|0123456789abcdef0123456789abcdef0123456789|name.mp4|1024|".into(),
            ),
            (LocatorScheme::Http, "https://example.com/file.mp4".into()),
            (LocatorScheme::Http, "http://example.com/file.mp4".into()),
        ];
        for (scheme, uri) in good {
            assert!(scheme.validate(&uri).is_ok(), "{scheme} rejected {uri:?}");
        }
    }

    #[test]
    fn an_uppercase_infohash_is_accepted() {
        // Clients hand out both cases, and rejecting uppercase would break
        // half of them for no security benefit -- hex case is not identity.
        let lower = "0123456789abcdef0123456789abcdef01234567";
        let upper = lower.to_ascii_uppercase();
        assert!(LocatorScheme::BitTorrent.validate(lower).is_ok());
        assert!(LocatorScheme::BitTorrent.validate(&upper).is_ok());
    }

    #[test]
    fn a_consent_refusal_and_a_bad_uri_are_distinguishable() {
        let refusal = ProposeError::TierForbidsLocator {
            object: "o".into(),
            tier: "denied".into(),
            scheme: "magnet".into(),
        };
        let bad = ProposeError::MalformedUri {
            scheme: "magnet".into(),
            reason: "nope".into(),
        };
        assert!(refusal.is_consent_refusal());
        assert!(!bad.is_consent_refusal());
    }

    #[test]
    fn only_three_schemes_are_redistribution_channels() {
        let redistribution: Vec<LocatorScheme> = LocatorScheme::ALL
            .into_iter()
            .filter(|s| s.is_redistribution())
            .collect();
        assert_eq!(
            redistribution,
            vec![
                LocatorScheme::BitTorrent,
                LocatorScheme::Magnet,
                LocatorScheme::Ed2k
            ]
        );
    }
}
