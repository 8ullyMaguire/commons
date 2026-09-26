//! Stable string representations for every domain enum.
//!
//! These strings are wire format. They appear in URLs (§5.16), in exports
//! (§13.4), in the federation protocol (§13.1), and in the plugin API
//! (§5.18.1). Changing one is a breaking protocol change, which is why
//! `T-P0-002` pins each one with a test.
//!
//! Every repr is `snake_case`. None is a bare ordinal: ordinals reorder
//! silently when someone inserts a variant, and these values are persisted.

use serde::{Deserialize, Serialize};
use std::fmt;

/// The seven first-class content kinds (§5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectKind {
    Scene,
    Image,
    Gallery,
    Audio,
    Comic,
    Text,
    Interview,
}

impl ObjectKind {
    pub const ALL: [ObjectKind; 7] = [
        ObjectKind::Scene,
        ObjectKind::Image,
        ObjectKind::Gallery,
        ObjectKind::Audio,
        ObjectKind::Comic,
        ObjectKind::Text,
        ObjectKind::Interview,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            ObjectKind::Scene => "scene",
            ObjectKind::Image => "image",
            ObjectKind::Gallery => "gallery",
            ObjectKind::Audio => "audio",
            ObjectKind::Comic => "comic",
            ObjectKind::Text => "text",
            ObjectKind::Interview => "interview",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

/// Every enum with a stable `as_str` gets a `Display` that prints that exact
/// string. Implemented by macro so a new enum cannot forget it and so
/// `Display` and `as_str` can never drift apart.
macro_rules! impl_display_via_str {
    ($($ty:ty),+ $(,)?) => {
        $(impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        })+
    };
}

impl_display_via_str!(
    ObjectKind,
    FileState,
    OrganizedState,
    ConsentTier,
    ProposalSource,
    ProposerKind,
    RelationType,
    AppearanceType,
    ClusterState,
    ProducerKind,
    LocatorScheme,
    AttributeType,
    JobState,
    SubjectType,
    Role,
);

/// Whether a file is currently reachable (§6.3, stash#5683).
///
/// `Missing` exists so a detached volume is a state we can record once rather
/// than a condition we re-stat until the process dies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileState {
    Present,
    Missing,
    Unreadable,
    Remote,
}

impl FileState {
    pub const ALL: [FileState; 4] = [
        FileState::Present,
        FileState::Missing,
        FileState::Unreadable,
        FileState::Remote,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            FileState::Present => "present",
            FileState::Missing => "missing",
            FileState::Unreadable => "unreadable",
            FileState::Remote => "remote",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

/// Tri-state organized/favourite (§5.13). Not a boolean: "unreviewed" and
/// "favourite" are different facts and a bool cannot hold both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrganizedState {
    Unreviewed,
    Organized,
    Favourite,
}

impl OrganizedState {
    pub const ALL: [OrganizedState; 3] = [
        OrganizedState::Unreviewed,
        OrganizedState::Organized,
        OrganizedState::Favourite,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            OrganizedState::Unreviewed => "unreviewed",
            OrganizedState::Organized => "organized",
            OrganizedState::Favourite => "favourite",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

/// The consent tier ladder (§14.1). This enum is the backbone of the safety
/// model: it gates visibility, federation, publication, and P2P locators.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsentTier {
    /// Scanned locally; consent not established. Private by default and not
    /// publishable. The default for a freshly scanned file.
    Unverified,
    /// The uploader asserts they are the creator and the subject consents.
    SelfPublished,
    /// A verified performer claim (§7.5) covers it.
    PerformerClaimed,
    /// Licensed or permitted on a stated basis.
    ThirdPartyPermitted,
    /// Reported or contested; hidden everywhere pending review.
    Quarantined,
    /// Takedown accepted; hash-blocked across all peers.
    Denied,
}

impl ConsentTier {
    pub const ALL: [ConsentTier; 6] = [
        ConsentTier::Unverified,
        ConsentTier::SelfPublished,
        ConsentTier::PerformerClaimed,
        ConsentTier::ThirdPartyPermitted,
        ConsentTier::Quarantined,
        ConsentTier::Denied,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            ConsentTier::Unverified => "unverified",
            ConsentTier::SelfPublished => "self_published",
            ConsentTier::PerformerClaimed => "performer_claimed",
            ConsentTier::ThirdPartyPermitted => "third_party_permitted",
            ConsentTier::Quarantined => "quarantined",
            ConsentTier::Denied => "denied",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// Whether an object in this tier may be seen by its **owner** in the
    /// library that holds it.
    ///
    /// Every tier except `Denied` and `Quarantined` is locally visible,
    /// including `Unverified`. This matters more than it looks: a freshly
    /// scanned file is `Unverified`, so if that tier were locally invisible
    /// then every new file in the library would be hidden in the very app
    /// holding it, and the local app would be useless. The tier governs
    /// *publication and sharing*, not the owner's own access to their disk.
    pub fn visible_to_owner(self) -> bool {
        !matches!(self, ConsentTier::Denied | ConsentTier::Quarantined)
    }

    /// Whether an object in this tier may be seen by a **remote viewer** — an
    /// anonymous visitor on a hosted index, or a user of another instance.
    ///
    /// `Unverified` is invisible here even though it is visible to its owner:
    /// that is the whole point of the tier. Consent not established means
    /// consent not established for anyone but the holder.
    pub fn visible_to_public(self) -> bool {
        matches!(
            self,
            ConsentTier::SelfPublished
                | ConsentTier::PerformerClaimed
                | ConsentTier::ThirdPartyPermitted
        )
    }

    /// Whether a P2P locator may be attached to an object in this tier
    /// (§5.18). A magnet is a redistribution channel, so storing one is a
    /// distribution decision, not a metadata decision.
    ///
    /// Note this is deliberately *not* derived from `visible_to_public`: the
    /// two answer different questions. An amateur creator's self-published
    /// item is perfectly visible and still must not carry a magnet.
    pub fn permits_locator(self) -> bool {
        matches!(self, ConsentTier::ThirdPartyPermitted)
    }

    /// Whether this tier may be published to opted-in federation peers (§13.1).
    pub fn permits_publication(self) -> bool {
        matches!(
            self,
            ConsentTier::SelfPublished
                | ConsentTier::PerformerClaimed
                | ConsentTier::ThirdPartyPermitted
        )
    }
}

/// Where a metadata proposal came from (§8.2). An automatic proposer is just
/// another proposer: `MlTagger` gets a vote like anybody else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalSource {
    Filename,
    Embedded,
    PhashMatch,
    Transcript,
    Caption,
    MlTagger,
    MlCaptioner,
    User,
    Peer,
    Scraper,
    Plugin,
}

impl ProposalSource {
    pub const ALL: [ProposalSource; 11] = [
        ProposalSource::Filename,
        ProposalSource::Embedded,
        ProposalSource::PhashMatch,
        ProposalSource::Transcript,
        ProposalSource::Caption,
        ProposalSource::MlTagger,
        ProposalSource::MlCaptioner,
        ProposalSource::User,
        ProposalSource::Peer,
        ProposalSource::Scraper,
        ProposalSource::Plugin,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            ProposalSource::Filename => "filename",
            ProposalSource::Embedded => "embedded",
            ProposalSource::PhashMatch => "phash_match",
            ProposalSource::Transcript => "transcript",
            ProposalSource::Caption => "caption",
            ProposalSource::MlTagger => "ml:tagger",
            ProposalSource::MlCaptioner => "ml:captioner",
            ProposalSource::User => "user",
            ProposalSource::Peer => "peer",
            ProposalSource::Scraper => "scraper",
            ProposalSource::Plugin => "plugin",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// Whether this source is a machine rather than a person. Machine
    /// proposals carry a `confidence` and participate in voting by weight
    /// (§8.1) — they never override.
    pub const fn is_automatic(self) -> bool {
        !matches!(self, ProposalSource::User)
    }
}

/// Who may author a proposal. Kept separate from `ProposalSource` because a
/// peer is a transport, not a source, and a plugin is an author whose source
/// varies per field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposerKind {
    User,
    Auto,
    Peer,
    Plugin,
}

impl ProposerKind {
    pub const ALL: [ProposerKind; 4] = [
        ProposerKind::User,
        ProposerKind::Auto,
        ProposerKind::Peer,
        ProposerKind::Plugin,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            ProposerKind::User => "user",
            ProposerKind::Auto => "auto",
            ProposerKind::Peer => "peer",
            ProposerKind::Plugin => "plugin",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

/// Typed edges between objects (§5.9). The duplicate checker and the
/// "related items" surface are both views over these, which is why they stop
/// being separate features.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationType {
    ExtraOf,
    PartOf,
    CompilationOf,
    SameSceneAs,
    ReEncodeOf,
    UnrelatedTo,
}

impl RelationType {
    pub const ALL: [RelationType; 6] = [
        RelationType::ExtraOf,
        RelationType::PartOf,
        RelationType::CompilationOf,
        RelationType::SameSceneAs,
        RelationType::ReEncodeOf,
        RelationType::UnrelatedTo,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            RelationType::ExtraOf => "extra_of",
            RelationType::PartOf => "part_of",
            RelationType::CompilationOf => "compilation_of",
            RelationType::SameSceneAs => "same_scene_as",
            RelationType::ReEncodeOf => "re_encode_of",
            RelationType::UnrelatedTo => "unrelated_to",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

/// How a person appears in an item (§7.12). Not every appearance is the same
/// kind, and conflating them is what makes "appear with" useless.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppearanceType {
    Primary,
    Cameo,
    NonSexual,
    GroupScene,
    Duologue,
    Background,
}

impl AppearanceType {
    pub const ALL: [AppearanceType; 6] = [
        AppearanceType::Primary,
        AppearanceType::Cameo,
        AppearanceType::NonSexual,
        AppearanceType::GroupScene,
        AppearanceType::Duologue,
        AppearanceType::Background,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            AppearanceType::Primary => "primary",
            AppearanceType::Cameo => "cameo",
            AppearanceType::NonSexual => "non_sexual",
            AppearanceType::GroupScene => "group_scene",
            AppearanceType::Duologue => "duologue",
            AppearanceType::Background => "background",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// Whether this appearance type means the person is in the item's main
    /// "appears with" graph. Cameos and background presence are excluded by
    /// default (stash#5444).
    pub const fn counts_as_appearance(self) -> bool {
        matches!(
            self,
            AppearanceType::Primary | AppearanceType::GroupScene | AppearanceType::Duologue
        )
    }
}

/// The lifecycle of an identity cluster (§7.1).
///
/// `Anonymous` is not a placeholder state — it is the default and a fully
/// valid, browsable condition. A library of material where nobody is identified
/// as a creator still gets its people linked; that is the entire premise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClusterState {
    Anonymous,
    Named,
    Claimed,
    Ambiguous,
}

impl ClusterState {
    pub const ALL: [ClusterState; 4] = [
        ClusterState::Anonymous,
        ClusterState::Named,
        ClusterState::Claimed,
        ClusterState::Ambiguous,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            ClusterState::Anonymous => "anonymous",
            ClusterState::Named => "named",
            ClusterState::Claimed => "claimed",
            ClusterState::Ambiguous => "ambiguous",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

/// Who produced an item (§5.12). One type with a discriminator rather than
/// separate Studio and Group tables: doujin circles, amateur uploaders and
/// studios share a metadata shape and a voting system, and only the
/// attribution rules differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProducerKind {
    Studio,
    Circle,
    Individual,
    Collective,
    Unknown,
}

impl ProducerKind {
    pub const ALL: [ProducerKind; 5] = [
        ProducerKind::Studio,
        ProducerKind::Circle,
        ProducerKind::Individual,
        ProducerKind::Collective,
        ProducerKind::Unknown,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            ProducerKind::Studio => "studio",
            ProducerKind::Circle => "circle",
            ProducerKind::Individual => "individual",
            ProducerKind::Collective => "collective",
            ProducerKind::Unknown => "unknown",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// Whether this producer is a person or a group of people rather than a
    /// business. Drives the consent attestation requirement in §14.1.
    pub const fn is_person_ish(self) -> bool {
        matches!(
            self,
            ProducerKind::Individual | ProducerKind::Collective | ProducerKind::Circle
        )
    }
}

/// Locator transports (§5.18). The `Locator` row and the consent gate are core;
/// the computation that produces these is the plugin's job (§5.18.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocatorScheme {
    Magnet,
    Ed2k,
    Infohash,
    Http,
}

impl LocatorScheme {
    pub const ALL: [LocatorScheme; 4] = [
        LocatorScheme::Magnet,
        LocatorScheme::Ed2k,
        LocatorScheme::Infohash,
        LocatorScheme::Http,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            LocatorScheme::Magnet => "magnet",
            LocatorScheme::Ed2k => "ed2k",
            LocatorScheme::Infohash => "infohash",
            LocatorScheme::Http => "http",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// Whether this locator names a P2P swarm rather than an ordinary source.
    /// Both are redistribution channels; only one is a P2P protocol.
    pub const fn is_p2p(self) -> bool {
        matches!(
            self,
            LocatorScheme::Magnet | LocatorScheme::Ed2k | LocatorScheme::Infohash
        )
    }
}

/// Declared type of a custom or built-in attribute (§7.7).
///
/// `Measurement` is the interesting one: date-stamped and multi-valued, which
/// is what makes automatic career span (§7.11) and age-at-scene (§7.6)
/// derivable rather than stored-and-drifting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttributeType {
    Single,
    Multi,
    Range,
    Ordinal,
    Boolean,
    Text,
    Date,
    Measurement,
}

impl AttributeType {
    pub const ALL: [AttributeType; 8] = [
        AttributeType::Single,
        AttributeType::Multi,
        AttributeType::Range,
        AttributeType::Ordinal,
        AttributeType::Boolean,
        AttributeType::Text,
        AttributeType::Date,
        AttributeType::Measurement,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            AttributeType::Single => "single",
            AttributeType::Multi => "multi",
            AttributeType::Range => "range",
            AttributeType::Ordinal => "ordinal",
            AttributeType::Boolean => "boolean",
            AttributeType::Text => "text",
            AttributeType::Date => "date",
            AttributeType::Measurement => "measurement",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }

    pub const fn holds_multiple(self) -> bool {
        matches!(self, AttributeType::Multi | AttributeType::Measurement)
    }
}

/// Job lifecycle (§6.3). `Skipped` is a first-class terminal state: a file that
/// fails repeatedly must leave the queue, not loop in it (stash#2913).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Done,
    Failed,
    Skipped,
    Cancelled,
}

impl JobState {
    pub const ALL: [JobState; 6] = [
        JobState::Queued,
        JobState::Running,
        JobState::Done,
        JobState::Failed,
        JobState::Skipped,
        JobState::Cancelled,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            JobState::Queued => "queued",
            JobState::Running => "running",
            JobState::Done => "done",
            JobState::Failed => "failed",
            JobState::Skipped => "skipped",
            JobState::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }

    pub const fn is_terminal(self) -> bool {
        !matches!(self, JobState::Queued | JobState::Running)
    }
}

/// What a metadata proposal is about (§8.1).
///
/// Deliberately polymorphic rather than object-scoped: a performer's name and
/// a studio's name vote by exactly the same machinery as an object's title,
/// and a design that hard-coded `object_id` here would fork the voting system
/// into two implementations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubjectType {
    Object,
    Performer,
    Producer,
    Group,
    Tag,
    Cluster,
}

impl SubjectType {
    pub const ALL: [SubjectType; 6] = [
        SubjectType::Object,
        SubjectType::Performer,
        SubjectType::Producer,
        SubjectType::Group,
        SubjectType::Tag,
        SubjectType::Cluster,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            SubjectType::Object => "object",
            SubjectType::Performer => "performer",
            SubjectType::Producer => "producer",
            SubjectType::Group => "group",
            SubjectType::Tag => "tag",
            SubjectType::Cluster => "cluster",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

/// Account roles (§8.1, §12.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// No login at all. Read-only, consent-tier-visible content only.
    Public,
    /// Logged in, may vote, may not curate.
    Subscriber,
    /// May propose and curate.
    Contributor,
    /// May resolve the moderation queue.
    Steward,
    /// Everything, plus settings and peer config.
    Admin,
}

impl Role {
    pub const ALL: [Role; 5] = [
        Role::Public,
        Role::Subscriber,
        Role::Contributor,
        Role::Steward,
        Role::Admin,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Role::Public => "public",
            Role::Subscriber => "subscriber",
            Role::Contributor => "contributor",
            Role::Steward => "steward",
            Role::Admin => "admin",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// Whether this role may author metadata proposals.
    pub const fn may_curate(self) -> bool {
        matches!(self, Role::Contributor | Role::Steward | Role::Admin)
    }

    /// Whether this role may resolve moderation items.
    pub const fn may_moderate(self) -> bool {
        matches!(self, Role::Steward | Role::Admin)
    }

    /// Whether this role may see quarantined and denied objects.
    pub const fn may_see_restricted(self) -> bool {
        matches!(self, Role::Steward | Role::Admin)
    }

    /// The highest-privilege role implied by holding this one.
    pub const fn escalate(self) -> Self {
        match self {
            Role::Public => Role::Public,
            other => other,
        }
    }
}

/// Locale-independent timestamp helper. One format everywhere, so the two
/// engines compare and sort identically and no query needs a timezone function.
pub mod ts {
    use chrono::{DateTime, SecondsFormat, Utc};

    /// ISO-8601 UTC with second precision, e.g. `2026-09-26T11:02:00Z`.
    pub fn now() -> String {
        Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
    }

    pub fn from_unix(secs: i64) -> String {
        DateTime::from_timestamp(secs, 0)
            .unwrap_or_else(|| DateTime::from_timestamp(0, 0).expect("epoch is valid"))
            .to_rfc3339_opts(SecondsFormat::Secs, true)
    }

    pub fn parse(s: &str) -> Option<DateTime<Utc>> {
        DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|dt| dt.with_timezone(&Utc))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pinned wire strings. `T-P0-002`'s definition of done: a silent
    /// change to any of these is a breaking protocol change, so each is
    /// asserted literally rather than derived from the variant order.
    #[test]
    fn object_kind_strings_are_pinned() {
        assert_eq!(ObjectKind::Scene.as_str(), "scene");
        assert_eq!(ObjectKind::Gallery.as_str(), "gallery");
        assert_eq!(ObjectKind::Interview.as_str(), "interview");
        for k in ObjectKind::ALL {
            assert_eq!(ObjectKind::parse(k.as_str()), Some(k));
        }
    }

    #[test]
    fn consent_tier_strings_are_pinned() {
        assert_eq!(ConsentTier::Unverified.as_str(), "unverified");
        assert_eq!(ConsentTier::SelfPublished.as_str(), "self_published");
        assert_eq!(
            ConsentTier::ThirdPartyPermitted.as_str(),
            "third_party_permitted"
        );
        assert_eq!(ConsentTier::Denied.as_str(), "denied");
        for t in ConsentTier::ALL {
            assert_eq!(ConsentTier::parse(t.as_str()), Some(t));
        }
    }

    #[test]
    fn proposal_source_ml_names_use_colon_namespace() {
        // The `ml:` prefix is the honesty mechanism in §5.15: an ML tag is
        // always visibly an ML tag, never disguised as canonical.
        assert_eq!(ProposalSource::MlTagger.as_str(), "ml:tagger");
        assert_eq!(ProposalSource::MlCaptioner.as_str(), "ml:captioner");
        assert!(ProposalSource::MlTagger.is_automatic());
        assert!(!ProposalSource::User.is_automatic());
        for s in ProposalSource::ALL {
            assert_eq!(ProposalSource::parse(s.as_str()), Some(s));
        }
    }

    #[test]
    fn owner_visibility_and_public_visibility_are_different_questions() {
        // The distinction this whole test exists to pin: a freshly scanned
        // file is `Unverified`, and it must still be visible in the app
        // holding it. `Unverified` governs *sharing*, not local access.
        assert!(
            ConsentTier::Unverified.visible_to_owner(),
            "a fresh scan must be visible to its owner or the local app is useless"
        );
        assert!(
            !ConsentTier::Unverified.visible_to_public(),
            "unverified must not be visible to a remote viewer"
        );

        for tier in ConsentTier::ALL {
            assert!(
                !tier.visible_to_public() || tier.visible_to_owner(),
                "{tier}: anything public must also be owner-visible"
            );
        }

        // Quarantined and denied are invisible to everyone but stewards, on
        // both questions.
        for tier in [ConsentTier::Quarantined, ConsentTier::Denied] {
            assert!(!tier.visible_to_owner(), "{tier} must be owner-hidden");
            assert!(!tier.visible_to_public(), "{tier} must be public-hidden");
        }
    }

    #[test]
    fn locator_permission_is_not_derived_from_visibility() {
        // §5.18: a magnet is a redistribution channel, so storing one is a
        // distribution decision. An amateur creator's self-published item is
        // perfectly public and still must not carry one. This predicate is
        // therefore independent of both visibility questions above.
        assert!(ConsentTier::SelfPublished.visible_to_public());
        assert!(
            !ConsentTier::SelfPublished.permits_locator(),
            "self-published is visible but must not carry a magnet"
        );
        assert!(ConsentTier::ThirdPartyPermitted.permits_locator());
        assert!(!ConsentTier::Unverified.permits_locator());
        assert!(!ConsentTier::PerformerClaimed.permits_locator());
        assert!(!ConsentTier::Quarantined.permits_locator());
        assert!(!ConsentTier::Denied.permits_locator());
    }

    #[test]
    fn publication_permission_tracks_public_visibility() {
        // What may leave this instance is exactly what a remote viewer may
        // see; unverified content stays put until consent is established.
        for tier in ConsentTier::ALL {
            assert_eq!(
                tier.permits_publication(),
                tier.visible_to_public(),
                "{tier}: publication and public visibility must agree"
            );
        }
    }

    #[test]
    fn quarantined_and_denied_are_steward_only() {
        for tier in [ConsentTier::Quarantined, ConsentTier::Denied] {
            assert!(!tier.visible_to_public(), "{tier} must not be public");
            assert!(!tier.visible_to_owner(), "{tier} must not be owner-visible");
        }
        assert!(!Role::Subscriber.may_see_restricted());
        assert!(Role::Steward.may_see_restricted());
        assert!(!Role::Public.may_curate());
        assert!(!Role::Subscriber.may_curate());
        assert!(Role::Contributor.may_curate());
    }

    #[test]
    fn role_strings_are_pinned() {
        assert_eq!(Role::Public.as_str(), "public");
        assert_eq!(Role::Admin.as_str(), "admin");
        for r in Role::ALL {
            assert_eq!(Role::parse(r.as_str()), Some(r));
        }
    }

    #[test]
    fn job_states_have_terminal_semantics() {
        assert!(!JobState::Queued.is_terminal());
        assert!(!JobState::Running.is_terminal());
        for s in [
            JobState::Done,
            JobState::Failed,
            JobState::Skipped,
            JobState::Cancelled,
        ] {
            assert!(s.is_terminal(), "{s} must be terminal");
        }
    }

    #[test]
    fn appearance_types_separate_cameo_from_presence() {
        // stash#5444: cameos and background presence are excluded from the
        // "appear with" graph by default.
        assert!(AppearanceType::Primary.counts_as_appearance());
        assert!(AppearanceType::GroupScene.counts_as_appearance());
        assert!(!AppearanceType::Cameo.counts_as_appearance());
        assert!(!AppearanceType::Background.counts_as_appearance());
        assert!(!AppearanceType::NonSexual.counts_as_appearance());
    }

    #[test]
    fn all_enums_round_trip_through_serde() {
        macro_rules! round_trip {
            ($ty:ty, $all:expr) => {
                for v in $all {
                    let s = serde_json::to_string(&v).unwrap();
                    let back: $ty = serde_json::from_str(&s).unwrap();
                    assert_eq!(v, back, "round trip failed for {s}");
                }
            };
        }
        round_trip!(ObjectKind, ObjectKind::ALL);
        round_trip!(FileState, FileState::ALL);
        round_trip!(OrganizedState, OrganizedState::ALL);
        round_trip!(ConsentTier, ConsentTier::ALL);
        round_trip!(ProposalSource, ProposalSource::ALL);
        round_trip!(ProposerKind, ProposerKind::ALL);
        round_trip!(RelationType, RelationType::ALL);
        round_trip!(AppearanceType, AppearanceType::ALL);
        round_trip!(ClusterState, ClusterState::ALL);
        round_trip!(ProducerKind, ProducerKind::ALL);
        round_trip!(LocatorScheme, LocatorScheme::ALL);
        round_trip!(AttributeType, AttributeType::ALL);
        round_trip!(JobState, JobState::ALL);
        round_trip!(SubjectType, SubjectType::ALL);
        round_trip!(Role, Role::ALL);
    }

    #[test]
    fn multi_valued_attributes_hold_multiple() {
        assert!(AttributeType::Multi.holds_multiple());
        assert!(AttributeType::Measurement.holds_multiple());
        assert!(!AttributeType::Single.holds_multiple());
        assert!(!AttributeType::Text.holds_multiple());
    }

    #[test]
    fn p2p_schemes_are_distinguished_from_http() {
        assert!(LocatorScheme::Magnet.is_p2p());
        assert!(LocatorScheme::Ed2k.is_p2p());
        assert!(LocatorScheme::Infohash.is_p2p());
        assert!(!LocatorScheme::Http.is_p2p());
    }

    #[test]
    fn timestamps_are_lexically_sortable() {
        let a = ts::from_unix(1_700_000_000);
        let b = ts::from_unix(1_800_000_000);
        assert!(a < b, "{a} must sort before {b}");
        assert!(a.ends_with('Z'), "timestamps are UTC");
        assert_eq!(a, "2023-11-14T22:13:20Z");
    }
}
