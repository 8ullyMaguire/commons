//! Core domain structs (§5.1, §15.1).
//!
//! No I/O, no database types. These types are what crosses crate boundaries;
//! storage representation is `commons-store`'s business.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::enums::*;

/// A thing in the library, of any of the seven kinds (§5.1).
///
/// `kind` is a discriminator rather than seven tables: the metadata, voting,
/// consent and relation machinery is identical across kinds, and splitting it
/// per kind is how upstream ended up needing a separate bolt-on for every new
/// content type.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Object {
    pub id: Uuid,
    pub kind: ObjectKind,
    pub title: Option<String>,
    pub description: Option<String>,
    pub date: Option<String>,
    /// The resolved producer: studio, circle, individual, or collective
    /// (§5.12). A person is a valid producer, which is what lets amateur
    /// material be attributed at all.
    pub producer_id: Option<Uuid>,
    pub organized: OrganizedState,
    pub rating_sum: i64,
    pub rating_count: i64,
    pub created_at: String,
    pub updated_at: String,
}

impl Object {
    pub fn new(kind: ObjectKind) -> Self {
        let now = ts::now();
        Self {
            id: Uuid::new_v4(),
            kind,
            title: None,
            description: None,
            date: None,
            producer_id: None,
            organized: OrganizedState::Unreviewed,
            rating_sum: 0,
            rating_count: 0,
            created_at: now.clone(),
            updated_at: now,
        }
    }

    /// Mean rating, or `None` when unrated. Derived, never stored — a stored
    /// average drifts the moment a rating changes (§5.17).
    pub fn average_rating(&self) -> Option<f64> {
        if self.rating_count == 0 {
            None
        } else {
            Some(self.rating_sum as f64 / self.rating_count as f64)
        }
    }
}

/// A source file. `Object` is 1-to-n with `File` (§5.1), which is the primitive
/// that makes multi-scene files work without a parallel object type (§5.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileRecord {
    pub id: Uuid,
    pub object_id: Uuid,
    pub path: String,
    pub size_bytes: i64,
    pub mtime_ns: i64,
    /// xxh128: fast change detection (§6.2).
    pub hash_xxh128: Option<String>,
    /// BLAKE3: content identity, and the basis of move detection.
    pub hash_blake3: Option<String>,
    pub state: FileState,
}

/// A time range within a file. This one row answers stash#3530 (multi-scene
/// files), #2276 (multi-part) and #2511 (virtual compilations) at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Segment {
    pub file_id: Uuid,
    pub idx: i32,
    pub start_ms: i64,
    pub end_ms: i64,
}

impl Segment {
    pub fn contains(&self, at_ms: i64) -> bool {
        at_ms >= self.start_ms && at_ms < self.end_ms
    }
}

/// An archive (zip, cbz, 7z, …) and its member listing (§5.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Archive {
    pub id: Uuid,
    pub object_id: Uuid,
    pub path: String,
    pub compression: Option<String>,
    pub member_count: i64,
}

/// A typed, directed edge between two objects (§5.9).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObjectRelation {
    pub from_id: Uuid,
    pub to_id: Uuid,
    pub relation: RelationType,
    pub created_at: String,
    /// Set when a human asserted this, as opposed to a detector proposing it.
    pub asserted: bool,
}

/// A set of face embeddings believed to be one person, with no name required
/// (§7.1). `Anonymous` is the default state and is fully browsable — this is
/// the primitive that links a person across a corpus no scraper can reach.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersonCluster {
    pub id: Uuid,
    pub handle: Option<String>,
    pub state: ClusterState,
    /// L2-normalised mean face embedding, hex-encoded. Kept out of SQL
    /// (§0.4); this is the authoritative copy, the ANN index is a cache.
    pub centroid_hex: Option<String>,
    pub appearance_count: i64,
    pub created_at: String,
    pub updated_at: String,
}

impl PersonCluster {
    pub fn new_anonymous() -> Self {
        let now = ts::now();
        Self {
            id: Uuid::new_v4(),
            handle: None,
            state: ClusterState::Anonymous,
            centroid_hex: None,
            appearance_count: 0,
            created_at: now.clone(),
            updated_at: now,
        }
    }
}

/// A person appearing in an item, with the evidence for the link (§7.1, §7.12).
///
/// The composite score components are stored, not just the result: §7.4
/// requires the UI to be able to say *why* two items were linked, and a single
/// opaque float makes that impossible.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Appearance {
    pub id: Uuid,
    pub object_id: Uuid,
    pub cluster_id: Uuid,
    pub appearance_type: AppearanceType,
    pub distance: Option<f64>,
    pub face_score: Option<f64>,
    pub body_score: Option<f64>,
    pub face_id: Option<Uuid>,
    /// Set when clustering was unsure, so the UI can show both candidates
    /// rather than silently guessing (§7.1).
    pub ambiguous: bool,
    pub source: ProposalSource,
    pub created_at: String,
}

/// A named, claimed profile layered over one or more clusters (§7.5, §7.6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Performer {
    pub id: Uuid,
    pub name: String,
    pub aliases: Vec<String>,
    pub gender: Option<String>,
    pub nationality: Option<String>,
    pub birth_date: Option<String>,
    pub status: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// A studio, circle, individual or collective (§5.12).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Producer {
    pub id: Uuid,
    pub kind: ProducerKind,
    pub name: String,
    pub aliases: Vec<String>,
    pub urls: Vec<String>,
    /// Computed from item dates, never authored (§7.11).
    pub career_start: Option<String>,
    pub career_end: Option<String>,
    pub defunct: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// A release, series, compilation or franchise (§5.12, §8.10).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Group {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub code: Option<String>,
    pub producer_id: Option<Uuid>,
    pub created_at: String,
}

/// A tag with a tree position, a namespace, and an importance weight (§5.15).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tag {
    pub id: Uuid,
    pub name: String,
    pub parent_id: Option<Uuid>,
    /// `canonical`, `site:<host>`, `ml:<model>`, or `user:<id>` (§5.15).
    pub namespace: String,
    pub color: Option<String>,
    /// How much this tag counts toward similarity and search ranking.
    pub importance: f64,
}

/// A single candidate value for one field of one subject (§8.1).
///
/// This type is the structural core of the whole design. Metadata is not a
/// column per field; it is a set of proposals per `(subject, field)` that vote
/// to a settled value. A plain `title` column would be the wrong shape for a
/// crowd-curated index.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldProposal {
    pub id: Uuid,
    pub subject_type: SubjectType,
    pub subject_id: Uuid,
    pub field: String,
    /// Always JSON: a proposal may be a string, a number, a list, or null.
    pub value_json: String,
    pub source: ProposalSource,
    pub proposer_kind: ProposerKind,
    pub proposer_id: Option<String>,
    /// Set for machine proposals; `None` for humans.
    pub confidence: Option<f64>,
    pub created_at: String,
}

impl FieldProposal {
    pub fn new(
        subject_type: SubjectType,
        subject_id: Uuid,
        field: impl Into<String>,
        value_json: impl Into<String>,
        source: ProposalSource,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            subject_type,
            subject_id,
            field: field.into(),
            value_json: value_json.into(),
            source,
            proposer_kind: if source.is_automatic() {
                ProposerKind::Auto
            } else {
                ProposerKind::User
            },
            proposer_id: None,
            confidence: None,
            created_at: ts::now(),
        }
    }
}

/// A ballot on one proposal, for one field only (§8.3).
///
/// Weight is per-field on purpose: agreeing with people about titles says
/// nothing about their judgement on tags.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Vote {
    pub id: Uuid,
    pub proposal_id: Uuid,
    pub account_id: Uuid,
    /// The field this vote was cast on, denormalised so reputation can be
    /// computed per field without a join.
    pub field: String,
    /// Reputation at cast time, so history does not silently re-weight.
    pub weight: f64,
    pub created_at: String,
    pub retracted: bool,
}

/// An object's consent record (§14.1).
///
/// `redistribution_permitted` is the field that gates P2P locators (§5.18)
/// and it is *not* derivable from the tier: an amateur creator's
/// self-published item is perfectly visible and still must not carry a magnet.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConsentRecord {
    pub id: Uuid,
    pub object_id: Uuid,
    pub tier: ConsentTier,
    pub attestation: Option<Attestation>,
    pub redistribution_permitted: bool,
    pub decided_by: Option<Uuid>,
    pub decided_at: Option<String>,
    pub audit_trail: Vec<ConsentEvent>,
}

/// Who attested consent to what, when, and on what stated basis (§14.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Attestation {
    pub attested_by: Option<Uuid>,
    pub attested_at: String,
    /// Free text: the stated basis. Kept verbatim because "on what basis" is
    /// the question a takedown or audit actually asks.
    pub basis: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConsentEvent {
    pub at: String,
    pub actor: Option<Uuid>,
    pub from_tier: Option<ConsentTier>,
    pub to_tier: Option<ConsentTier>,
    pub reason: Option<String>,
}

/// A P2P or source locator attached to an object (§5.18).
///
/// The row and the tier gate are core. Computing ed2k hashes and infohashes
/// is the plugin's job (§5.18.1) — a plugin can only ever reach this through
/// `locator.propose`, which re-checks the tier.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Locator {
    pub id: Uuid,
    pub object_id: Uuid,
    pub file_id: Option<Uuid>,
    pub scheme: LocatorScheme,
    pub uri: String,
    pub infohash: Option<String>,
    pub size_bytes: Option<i64>,
    pub name: Option<String>,
    pub added_at: String,
    pub source: ProposalSource,
}

/// A timeline point (§5.11).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Marker {
    pub id: Uuid,
    pub object_id: Uuid,
    pub title: String,
    pub start_ms: i64,
    pub end_ms: Option<i64>,
    pub primary_tag_id: Option<Uuid>,
    pub rating: Option<i32>,
    pub created_at: String,
}

/// A durable unit of slow work (§6.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub id: Uuid,
    pub kind: String,
    pub state: JobState,
    /// Idempotency key: a watcher firing fifty times for one file must
    /// produce one job.
    pub dedupe_key: String,
    pub target_id: Option<Uuid>,
    pub attempts: i32,
    pub last_error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// A generated file, keyed so a replaced source invalidates exactly its own
/// artifacts and nothing else (§10.1, stash#7155).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Artifact {
    pub id: Uuid,
    pub file_id: Uuid,
    pub kind: String,
    pub path: String,
    pub mtime_ns: i64,
    pub size_bytes: i64,
    /// Bumped when generation output changes, so one bump invalidates
    /// everything once and correctly.
    pub generator_version: u32,
    pub created_at: String,
}

impl Artifact {
    /// An artifact matches only if *every* input that could change its
    /// contents still matches.
    pub fn is_valid_for(&self, mtime_ns: i64, size_bytes: i64, generator_version: u32) -> bool {
        self.mtime_ns == mtime_ns
            && self.size_bytes == size_bytes
            && self.generator_version == generator_version
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Account {
    pub id: Uuid,
    pub handle: String,
    pub role: Role,
    pub reputation: f64,
    pub disabled: bool,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rating {
    pub id: Uuid,
    pub subject_type: SubjectType,
    pub subject_id: Uuid,
    pub account_id: Uuid,
    /// 1–5 stars.
    pub stars: i32,
    pub created_at: String,
}

impl Rating {
    /// Ratings are 1–5. Rejecting out-of-range values here rather than in a
    /// handler keeps one rule in one place.
    pub fn is_valid(&self) -> bool {
        (1..=5).contains(&self.stars)
    }
}

/// A federation peer (§13.1). Peers are configured explicitly; there is no
/// ambient discovery, which is also what keeps §5.18.1's "no DHT" property
/// true of the federation layer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Peer {
    pub id: String,
    pub endpoint: String,
    pub public_key: Vec<u8>,
    pub enabled: bool,
    pub added_at: String,
}

/// A signed statement exchanged with a peer (§13.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Claim {
    pub id: Uuid,
    pub peer_id: String,
    pub kind: String,
    pub subject_type: SubjectType,
    pub subject_id: Option<Uuid>,
    pub payload_json: String,
    pub content_hash: String,
    pub signature: Vec<u8>,
    pub created_at: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_object_starts_unreviewed_and_unrated() {
        let o = Object::new(ObjectKind::Scene);
        assert_eq!(o.organized, OrganizedState::Unreviewed);
        assert_eq!(o.average_rating(), None);
        assert_eq!(o.kind, ObjectKind::Scene);
    }

    #[test]
    fn average_rating_is_derived_not_stored() {
        let mut o = Object::new(ObjectKind::Image);
        o.rating_sum = 12;
        o.rating_count = 4;
        assert_eq!(o.average_rating(), Some(3.0));
    }

    #[test]
    fn segment_containment_is_half_open() {
        let s = Segment {
            file_id: Uuid::new_v4(),
            idx: 0,
            start_ms: 1000,
            end_ms: 2000,
        };
        assert!(s.contains(1000));
        assert!(s.contains(1999));
        assert!(!s.contains(2000), "end is exclusive");
        assert!(!s.contains(999));
    }

    #[test]
    fn artifact_validity_requires_every_input_to_match() {
        let a = Artifact {
            id: Uuid::new_v4(),
            file_id: Uuid::new_v4(),
            kind: "sprite".into(),
            path: "/tmp/s.webp".into(),
            mtime_ns: 100,
            size_bytes: 200,
            generator_version: 3,
            created_at: ts::now(),
        };
        assert!(a.is_valid_for(100, 200, 3));
        // A file replaced at the same path keeps mtime and size but changes
        // content: the artifact must not be considered valid. This is the
        // stash#7155 / #2773 case, and it is caught by the size/mtime check
        // being necessary-but-not-sufficient, with generator_version the
        // third leg.
        assert!(!a.is_valid_for(101, 200, 3));
        assert!(!a.is_valid_for(100, 201, 3));
        assert!(!a.is_valid_for(100, 200, 4));
    }

    #[test]
    fn proposal_infers_proposer_kind_from_source() {
        let human = FieldProposal::new(
            SubjectType::Object,
            Uuid::new_v4(),
            "title",
            "\"a title\"",
            ProposalSource::User,
        );
        assert_eq!(human.proposer_kind, ProposerKind::User);

        let machine = FieldProposal::new(
            SubjectType::Object,
            Uuid::new_v4(),
            "title",
            "\"a title\"",
            ProposalSource::MlCaptioner,
        );
        assert_eq!(machine.proposer_kind, ProposerKind::Auto);
    }

    #[test]
    fn anonymous_cluster_is_valid_and_is_the_default() {
        let c = PersonCluster::new_anonymous();
        assert_eq!(c.state, ClusterState::Anonymous);
        assert!(c.handle.is_none());
        assert_eq!(c.appearance_count, 0);
    }

    #[test]
    fn consent_record_defaults_to_no_redistribution() {
        let c = ConsentRecord {
            id: Uuid::new_v4(),
            object_id: Uuid::new_v4(),
            tier: ConsentTier::Unverified,
            attestation: None,
            redistribution_permitted: false,
            decided_by: None,
            decided_at: None,
            audit_trail: vec![],
        };
        assert!(!c.tier.permits_locator());
    }

    #[test]
    fn ratings_outside_one_to_five_are_invalid() {
        let mut r = Rating {
            id: Uuid::new_v4(),
            subject_type: SubjectType::Object,
            subject_id: Uuid::new_v4(),
            account_id: Uuid::new_v4(),
            stars: 3,
            created_at: ts::now(),
        };
        assert!(r.is_valid());
        r.stars = 0;
        assert!(!r.is_valid());
        r.stars = 6;
        assert!(!r.is_valid());
    }
}
