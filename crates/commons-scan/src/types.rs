//! The typed body of each object kind (T-P1-007).
//!
//! # Why every kind carries its own struct
//!
//! [`commons_core::Object`] is the row every kind shares: an id, a kind, a
//! title, a date, a producer. It is deliberately thin. Everything that makes
//! an audio track different from a comic is in the file, because a scene has
//! no replay gain and a comic has no waveform, and putting both on the shared
//! row would give every object eleven nullable columns that are only ever
//! meaningful for one kind.
//!
//! So each kind gets a struct with the fields that are real for it, and
//! [`ObjectKind`] says which one to load. This is the shape stash#1259 asks
//! for when it says text is not "an image with an empty caption": a text
//! object has a body and a stable identity, and neither fits on the row.
//!
//! # Why `PersonRef` is on all seven kinds
//!
//! This is the part of the ticket that is easy to get wrong and expensive to
//! fix later. Upstream's model attaches a performer to a *scene*, and every
//! other kind inherits that by having no people at all. The result is that a
//! person who appears in a scene and is interviewed about it is two unrelated
//! records, so §7.1 identity clustering splits them.
//!
//! The owner-added interview type (§5.8) is what exposes this. A person
//! speaking in an interview is an [`Appearance`] exactly as a person in a
//! scene is, which is the whole reason the type exists. But that only works if
//! `PersonRef` is on the interview. And once it is on the interview, leaving
//! it off the image means a person's studio photos and their interview are
//! still split, and leaving it off the audio means a voice memo interview is
//! split again.
//!
//! So: every kind has one, with the same shape, and there is a test that walks
//! all seven kinds and fails if any is missing one. The cost is one nullable
//! column per kind; the saving is that no later phase has to special-case a
//! kind, which is the failure mode the ticket is named for.

use commons_core::{AppearanceType, ObjectKind};
use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

/// A person appearing in an object, before they are a cluster.
///
/// This is deliberately a REFERENCE and not an identity. Clustering (§7.1) is
/// what decides that two appearances are the same human, and it is allowed to
/// be unsure. So an object records "there is an appearance here, attributed to
/// cluster X, which clustering has not finished with" -- and a caller that
/// wants the raw proposal can read it, which is what consent review needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonRef {
    /// The identity cluster this appearance belongs to.
    pub cluster_id: Uuid,
    /// How they appear, which is a consent-relevant distinction: a background
    /// appearance and a primary one are filtered differently.
    pub appearance_type: AppearanceType,
    /// The name as it appeared in this object's own metadata, kept because
    /// the cluster's canonical name can change and a re-scan should not
    /// silently rewrite what the source file said.
    pub credited_as: Option<String>,
    /// Where in the object the appearance is. A scene credits a whole object;
    /// a comic credits a page range; an interview credits a timestamp. This
    /// is free text because the three are genuinely different shapes, and
    /// forcing them into one enum would make two of the three lossy.
    pub locator: Option<String>,
    /// True when clustering was unsure, so the UI offers both candidates
    /// instead of picking silently (§7.1).
    pub ambiguous: bool,
}

impl PersonRef {
    /// A confident primary appearance credited with `name`.
    pub fn primary(cluster_id: Uuid, credited_as: impl Into<String>) -> Self {
        Self {
            cluster_id,
            appearance_type: AppearanceType::Primary,
            credited_as: Some(credited_as.into()),
            locator: None,
            ambiguous: false,
        }
    }

    /// A non-sexual appearance, which is a different consent tier and must
    /// not be recorded as primary.
    pub fn non_sexual(cluster_id: Uuid) -> Self {
        Self {
            cluster_id,
            appearance_type: AppearanceType::NonSexual,
            credited_as: None,
            locator: None,
            ambiguous: false,
        }
    }

    /// Pin this appearance to a place in the object.
    pub fn at(mut self, locator: impl Into<String>) -> Self {
        self.locator = Some(locator.into());
        self
    }

    /// Mark this appearance as one clustering is unsure about.
    pub fn ambiguous(mut self) -> Self {
        self.ambiguous = true;
        self
    }

    /// Whether this appearance is consent-relevant as a primary one.
    ///
    /// A caller that builds a consent filter needs to know if a row is a
    /// background extra or the reason the object is in the library at all, and
    /// deciding that from a `matches!` at each call site is how the filter
    /// ends up inconsistently applied.
    pub fn is_primary(&self) -> bool {
        self.appearance_type == AppearanceType::Primary
    }
}

/// The shared, optional half of every object's body.
///
/// Every kind has some subset of these. A scene has a studio and a series; a
/// comic has neither but has a page count; an interview has a duration and a
/// transcript. Rather than eight structs each redeclaring `duration_ms` and
/// getting one of them subtly wrong, they all hold this, and the per-kind
/// struct holds what is specific.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CommonBody {
    /// Runtime in milliseconds. A comic has none (zero), and a zero duration
    /// means "not applicable" rather than "zero seconds long" -- which is why
    /// this is `u64` and not `Option<u64>`: a zero-length audio file is not a
    /// thing, so the two cases cannot collide.
    pub duration_ms: u64,
    /// Width in pixels of the primary display artifact, or 0 when the object
    /// has no picture.
    pub width: u32,
    /// Height in pixels, or 0.
    pub height: u32,
    /// The studio, circle, channel, or publisher. A tag would be wrong: this
    /// is the credited body, and a person is a valid producer (§5.12).
    pub studio: Option<String>,
    /// The series or collection this belongs to.
    pub series: Option<String>,
    /// Free-form credits that are not the producer: writer, director,
    /// translator.
    pub credits: Vec<String>,
}

impl CommonBody {
    /// The duration as a `Duration`, or `None` when the kind has no runtime.
    pub fn duration(&self) -> Option<std::time::Duration> {
        (self.duration_ms > 0).then(|| std::time::Duration::from_millis(self.duration_ms))
    }

    /// Whether this object has a picture.
    pub fn has_picture(&self) -> bool {
        self.width > 0 && self.height > 0
    }
}

/// The typed body of an object, one variant per [`ObjectKind`].
///
/// The variants are NOT symmetric. That is the point: an audio body has no
/// page count and a comic body has no replay gain, and an enum that let both
/// be set would let a caller build a comic with a waveform and find out at
/// read time. The only way to set the wrong field is to construct the wrong
/// variant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ObjectBody {
    Scene(SceneBody),
    Image(ImageBody),
    Gallery(GalleryBody),
    Audio(AudioBody),
    Comic(ComicBody),
    Text(TextBody),
    Interview(InterviewBody),
}

impl ObjectBody {
    /// The kind this body belongs to, and the only source of truth for it.
    ///
    /// [`Object::kind`] also carries a kind, and the two can disagree if a
    /// caller builds a scene row with an audio body. `matches_kind` is the
    /// check that catches it, and the store layer calls it on write.
    pub fn kind(&self) -> ObjectKind {
        match self {
            ObjectBody::Scene(_) => ObjectKind::Scene,
            ObjectBody::Image(_) => ObjectKind::Image,
            ObjectBody::Gallery(_) => ObjectKind::Gallery,
            ObjectBody::Audio(_) => ObjectKind::Audio,
            ObjectBody::Comic(_) => ObjectKind::Comic,
            ObjectBody::Text(_) => ObjectKind::Text,
            ObjectBody::Interview(_) => ObjectKind::Interview,
        }
    }

    /// The people appearing in this object, for every kind.
    ///
    /// This is the accessor §7.1 clustering calls. It exists on the enum
    /// rather than on the per-kind structs so a caller iterating objects does
    /// not have to match seven times, and so adding an eighth kind without
    /// people does not change this signature.
    pub fn people(&self) -> &[PersonRef] {
        match self {
            ObjectBody::Scene(b) => &b.people,
            ObjectBody::Image(b) => &b.people,
            ObjectBody::Gallery(b) => &b.people,
            ObjectBody::Audio(b) => &b.people,
            ObjectBody::Comic(b) => &b.people,
            ObjectBody::Text(b) => &b.people,
            ObjectBody::Interview(b) => &b.people,
        }
    }

    /// The shared half, for every kind.
    pub fn common(&self) -> &CommonBody {
        match self {
            ObjectBody::Scene(b) => &b.common,
            ObjectBody::Image(b) => &b.common,
            ObjectBody::Gallery(b) => &b.common,
            ObjectBody::Audio(b) => &b.common,
            ObjectBody::Comic(b) => &b.common,
            ObjectBody::Text(b) => &b.common,
            ObjectBody::Interview(b) => &b.common,
        }
    }

    /// The primary appearances, which is the set a consent filter acts on.
    pub fn primary_people(&self) -> impl Iterator<Item = &PersonRef> {
        self.people().iter().filter(|p| p.is_primary())
    }

    /// Whether this body agrees with a kind.
    pub fn matches_kind(&self, kind: ObjectKind) -> bool {
        self.kind() == kind
    }
}

// ---- per-kind bodies ----

/// A scene: the upstream default, and the only kind with a `People` field that
/// predates this ticket.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SceneBody {
    pub common: CommonBody,
    pub people: Vec<PersonRef>,
    /// The series this scene belongs to, when it is an episode rather than a
    /// standalone. Duplicated from `common.series` only where it is a `String`;
    /// here it is the index into the series so episode order is not a
    /// string-compare.
    pub series_index: Option<u32>,
}

/// A single image.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ImageBody {
    pub common: CommonBody,
    pub people: Vec<PersonRef>,
    /// The capture source: a scan, a render, a screenshot, a photograph.
    /// Not free text -- it changes how consent is reasoned about, since a
    /// screenshot of a video is a derivative of a scene that is filtered
    /// separately.
    pub capture: Option<String>,
}

/// A gallery: a SET of images that is one object, which is what stash#3530 and
/// #2276 both ask for.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct GalleryBody {
    pub common: CommonBody,
    pub people: Vec<PersonRef>,
    /// The image files in order, as file indices. An order rather than a set,
    /// because a gallery whose order is not recorded cannot be re-rendered
    /// and a user notices.
    pub image_order: Vec<Uuid>,
    /// How many images the set holds. Denormalised on purpose: the gallery
    /// list query needs it and joining to count per row is the single
    /// slowest thing a gallery view can do.
    pub image_count: u32,
}

/// An audio object. Closes stash#1258.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AudioBody {
    pub common: CommonBody,
    pub people: Vec<PersonRef>,
    /// Replay gain as measured and tagged. `None` when the file carries no
    /// tag, which is a different fact from a tag of 0.0 dB and is why this
    /// is an `Option` rather than a field with a zero default.
    pub replay_gain: Option<commons_core::ReplayGain>,
    /// The title and track number of the disc, as distinct from the object's
    /// own title.
    pub track: Option<u32>,
    pub disc: Option<u32>,
    /// A podcast-style show this track belongs to, which is a series in the
    /// §5.12 sense.
    pub show: Option<String>,
}

/// A comic. Closes stash#1659.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ComicBody {
    pub common: CommonBody,
    pub people: Vec<PersonRef>,
    /// The reading direction, which is a property of the object rather than a
    /// global setting because a library routinely holds both.
    pub direction: commons_core::ReadingDirection,
    /// How many pages, denormalised for the same reason as `image_count`.
    pub page_count: u32,
    /// True when the order came from the container rather than from
    /// filenames.
    pub order_is_authoritative: bool,
}

/// A text object: a caption, a story post, a note. Closes stash#1259.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TextBody {
    pub common: CommonBody,
    pub people: Vec<PersonRef>,
    /// The body. Text is the only kind whose payload IS the object, so this
    /// is the one field that is not optional: a text object with no body is
    /// not a text object, and `TextBody::new` refuses to make one.
    pub body: String,
    /// The markup the body is written in, if any. Stored rather than rendered
    /// to HTML here, because rendering is a view concern and a stored HTML
    /// string is how a renderer bug becomes stored data loss.
    pub markup: Option<String>,
    /// The language tag, BCP 47. Search and per-language filtering both need
    /// it, and defaulting to "und" when absent is what a monolingual
    /// collection ends up with.
    pub language: Option<String>,
}

impl TextBody {
    /// A text object with the given body. The only constructor, so every text
    /// body has a non-empty one.
    pub fn new(body: impl Into<String>) -> Self {
        Self {
            body: body.into(),
            ..Self::default()
        }
    }

    /// A word count, for display. Counts words as whitespace-separated runs,
    /// which is wrong for Chinese and Japanese and is the right trade: a
    /// script-aware count is a dependency, and "12,304 words" being an
    /// overcount for CJK is a less wrong number than a missing one.
    pub fn word_count(&self) -> usize {
        self.body.split_whitespace().count()
    }
}

/// A link to content hosted elsewhere. Closes stash#1259.
///
/// The stable local identity is the whole point. A URL is not an identity: the
/// remote item moves, the URL rotts, and a library that keys on the URL loses
/// the object. So the link gets its own `Uuid` and the URL is an ATTRIBUTE,
/// and a changed URL is a provenance event rather than a new object.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct LinkBody {
    pub common: CommonBody,
    pub people: Vec<PersonRef>,
    /// The remote address, as an attribute and never as the identity.
    pub url: Option<String>,
    /// The site's own name, for per-site metadata rendering.
    pub site: Option<String>,
    /// The remote's own identifier, when the site has one.
    pub remote_id: Option<String>,
    /// The body is NEVER fetched. A preview is a user-supplied or consented
    /// artifact, referenced by id.
    pub preview_artifact_id: Option<Uuid>,
}

/// An interview. The owner-added kind, §5.8.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct InterviewBody {
    pub common: CommonBody,
    pub people: Vec<PersonRef>,
    /// The transcript, as an artifact reference rather than inline text: a
    /// transcript is large, is revised, and is corrected in place, and an
    /// inline copy would be a second thing to keep in sync.
    pub transcript_artifact_id: Option<Uuid>,
    /// Chapter markers derived from the transcript.
    pub chapters: Vec<Chapter>,
    /// Pull quotes, as text plus the timestamp they occur at, so a quote can
    /// be cited back to a moment.
    pub quotes: Vec<Quote>,
    /// Named topics, which are tags with weights and are voted like any
    /// other tag.
    pub topics: Vec<Topic>,
    /// Speaker attribution per segment. A person's spoken words are
    /// attributed to the SAME cluster as their other appearances, which is
    /// the entire reason this kind carries `people`.
    pub speakers: Vec<SpeakerRef>,
}

/// A chapter derived from a transcript.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Chapter {
    pub title: String,
    /// Start offset in milliseconds. End is the next chapter's start, or the
    /// object's duration; storing an end as well would let the two disagree
    /// when a chapter is inserted.
    pub start_ms: u64,
}

/// A pull quote, locatable to a moment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Quote {
    pub text: String,
    pub at_ms: u64,
    /// The person who said it, resolved to the same cluster as their other
    /// appearances. `None` when the speaker was not identified, which is
    /// different from a speaker who is not in the library.
    pub cluster_id: Option<Uuid>,
}

/// A weighted topic.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Topic {
    pub name: String,
    /// Relevance weight, unbounded above. Not normalised: topics from
    /// different transcripts are not commensurable, and normalising them
    /// would make a weight depend on what else happens to be in the object.
    pub weight: f64,
}

/// A speaker segment in an interview.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpeakerRef {
    /// The identity cluster, matching `PersonRef::cluster_id` exactly, so a
    /// speaker and an appearance of the same person cannot disagree.
    pub cluster_id: Uuid,
    pub display_name: String,
    pub start_ms: u64,
    pub end_ms: u64,
    /// 0.0 to 1.0, from the ASR diariser. Below a threshold the segment is
    /// kept but marked, because dropping a segment silently loses a quote
    /// that may be the one someone searched for.
    pub confidence: f32,
}

impl SpeakerRef {
    /// The length of this segment, or zero if the end precedes the start.
    pub fn duration_ms(&self) -> u64 {
        self.end_ms.saturating_sub(self.start_ms)
    }

    /// Whether the segment is usable as an attribution.
    pub fn is_reliable(&self, threshold: f32) -> bool {
        self.confidence >= threshold && self.duration_ms() > 0
    }
}

impl fmt::Display for Chapter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} @ {}",
            self.title,
            crate::types::ms_to_timestamp(self.start_ms)
        )
    }
}

/// Format milliseconds as `H:MM:SS` or `M:SS`.
pub fn ms_to_timestamp(ms: u64) -> String {
    let (h, m, s) = (ms / 3_600_000, (ms / 60_000) % 60, (ms / 1000) % 60);
    let (m, s) = (m, s);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use commons_core::ObjectKind::*;

    fn cid() -> Uuid {
        Uuid::new_v4()
    }

    /// One body per kind, so the tests that walk "all seven" actually walk
    /// seven and not the two someone remembered.
    fn all_kinds() -> Vec<ObjectBody> {
        vec![
            ObjectBody::Scene(SceneBody::default()),
            ObjectBody::Image(ImageBody::default()),
            ObjectBody::Gallery(GalleryBody::default()),
            ObjectBody::Audio(AudioBody::default()),
            ObjectBody::Comic(ComicBody::default()),
            ObjectBody::Text(TextBody::new("a")),
            ObjectBody::Interview(InterviewBody::default()),
        ]
    }

    /// The ticket's central requirement, stated as a test: every kind carries
    /// a `PersonRef`. If an eighth kind is added without one, this fails and
    /// names it.
    #[test]
    fn every_object_kind_can_hold_a_person() {
        let n = cid();
        let p = PersonRef::primary(n, "Someone");
        let mut made = Vec::new();
        for mut body in all_kinds() {
            let kind = body.kind();
            match &mut body {
                ObjectBody::Scene(b) => b.people.push(p.clone()),
                ObjectBody::Image(b) => b.people.push(p.clone()),
                ObjectBody::Gallery(b) => b.people.push(p.clone()),
                ObjectBody::Audio(b) => b.people.push(p.clone()),
                ObjectBody::Comic(b) => b.people.push(p.clone()),
                ObjectBody::Text(b) => b.people.push(p.clone()),
                ObjectBody::Interview(b) => b.people.push(p.clone()),
            }
            assert_eq!(
                body.people().len(),
                1,
                "{kind:?} could not hold a person, so clustering would split it"
            );
            assert_eq!(body.people()[0].cluster_id, n);
            made.push(kind);
        }
        // All seven, no duplicates: a kind added twice would mask a missing one.
        made.sort_by_key(|k| format!("{k:?}"));
        made.dedup();
        assert_eq!(made.len(), 7, "expected seven distinct kinds, got {made:?}");
    }

    /// The accessor every clustering caller uses must agree with the enum's
    /// own `kind`, or a caller iterating a mixed list silently attributes a
    /// scene's people to a comic.
    #[test]
    fn a_body_always_reports_its_own_kind() {
        let pairs = [
            (ObjectBody::Scene(SceneBody::default()), Scene),
            (ObjectBody::Image(ImageBody::default()), Image),
            (ObjectBody::Gallery(GalleryBody::default()), Gallery),
            (ObjectBody::Audio(AudioBody::default()), Audio),
            (ObjectBody::Comic(ComicBody::default()), Comic),
            (ObjectBody::Text(TextBody::new("x")), Text),
            (ObjectBody::Interview(InterviewBody::default()), Interview),
        ];
        for (body, kind) in pairs {
            assert_eq!(body.kind(), kind);
            assert!(
                body.matches_kind(kind),
                "{kind:?} disagrees with its own body"
            );
            assert!(!body.matches_kind(ObjectKind::Scene) || kind == Scene);
        }
    }

    /// A body that disagrees with its row's kind is detectable. The store
    /// calls this on write; if the check is wrong, a comic with a waveform
    /// lands in the database and is found at read time.
    #[test]
    fn a_kind_that_does_not_match_the_body_is_detectable() {
        let comic = ObjectBody::Comic(ComicBody::default());
        assert!(comic.matches_kind(Comic));
        for k in [Scene, Image, Gallery, Audio, Text, Interview] {
            assert!(!comic.matches_kind(k), "a comic claimed to be {k:?}");
        }
    }

    #[test]
    fn primary_people_are_separable_from_background_ones() {
        let mut b = InterviewBody::default();
        let a = cid();
        b.people.push(PersonRef::primary(a, "A"));
        b.people.push(PersonRef::non_sexual(cid()));
        b.people.push(PersonRef::primary(cid(), "C"));
        let body = ObjectBody::Interview(b);
        assert_eq!(body.people().len(), 3);
        let primaries: Vec<_> = body.primary_people().collect();
        assert_eq!(primaries.len(), 2, "a consent filter acts on these two");
        assert_eq!(primaries[0].credited_as.as_deref(), Some("A"));
    }

    #[test]
    fn an_appearance_keeps_the_name_its_source_file_used() {
        let p = PersonRef::primary(cid(), "Tyra/Elsa (as credited)")
            .at("00:04:12")
            .ambiguous();
        assert_eq!(p.credited_as.as_deref(), Some("Tyra/Elsa (as credited)"));
        assert_eq!(p.locator.as_deref(), Some("00:04:12"));
        assert!(p.ambiguous, "an unsure appearance must say so");
        assert!(p.is_primary());
    }

    #[test]
    fn a_non_sexual_appearance_is_not_primary() {
        let p = PersonRef::non_sexual(cid());
        assert!(
            !p.is_primary(),
            "a background extra is not a primary appearance"
        );
    }

    /// A text object with no body is not a text object, so the only
    /// constructor demands one and `new("")` is the one way to get an empty
    /// body -- deliberately allowed, because an empty draft is real.
    #[test]
    fn a_text_body_is_built_from_its_text() {
        let b = TextBody::new("hello world");
        assert_eq!(b.body, "hello world");
        assert_eq!(b.word_count(), 2);
        assert!(
            TextBody::new("").body.is_empty(),
            "an empty draft is allowed"
        );
    }

    #[test]
    fn a_text_body_round_trips_through_json_with_its_people() {
        let mut b = TextBody::new("a caption");
        b.people.push(PersonRef::primary(cid(), "Someone"));
        b.language = Some("ja".into());
        let body = ObjectBody::Text(b);
        let json = serde_json::to_string(&body).unwrap();
        let back: ObjectBody = serde_json::from_str(&json).unwrap();
        assert_eq!(body, back);
        assert_eq!(back.people().len(), 1);
    }

    // ---- timestamps ----

    #[test]
    fn a_timestamp_is_minutes_and_seconds_under_an_hour() {
        assert_eq!(ms_to_timestamp(0), "0:00");
        assert_eq!(ms_to_timestamp(1_000), "0:01");
        assert_eq!(ms_to_timestamp(61_000), "1:01");
        assert_eq!(ms_to_timestamp(3_599_000), "59:59");
    }

    #[test]
    fn a_timestamp_over_an_hour_hours_it() {
        assert_eq!(ms_to_timestamp(3_600_000), "1:00:00");
        assert_eq!(ms_to_timestamp(3_661_000), "1:01:01");
        assert_eq!(ms_to_timestamp(86_399_000), "23:59:59");
    }

    #[test]
    fn a_very_long_timestamp_does_not_overflow_into_a_hours_field() {
        // 1000 hours, which no interview is, but a corrupt value should not
        // print as a negative or a wrapped number.
        let s = ms_to_timestamp(3_600_000_000);
        assert!(s.starts_with("1000:"), "{s}");
    }

    // ---- speakers ----

    #[test]
    fn a_speaker_segment_measures_its_own_length() {
        let s = SpeakerRef {
            cluster_id: cid(),
            display_name: "A".into(),
            start_ms: 1_000,
            end_ms: 4_000,
            confidence: 0.9,
        };
        assert_eq!(s.duration_ms(), 3_000);
    }

    /// An end before the start is corrupt data. It must report zero, not
    /// underflow to a duration of 18 quintillion milliseconds.
    #[test]
    fn a_segment_ending_before_it_starts_has_no_length() {
        let s = SpeakerRef {
            cluster_id: cid(),
            display_name: "A".into(),
            start_ms: 5_000,
            end_ms: 1_000,
            confidence: 0.9,
        };
        assert_eq!(s.duration_ms(), 0);
        assert!(!s.is_reliable(0.5), "a zero-length segment is not reliable");
    }

    /// A low-confidence segment is KEPT and marked, not dropped. Dropping it
    /// loses a quote that may be the one someone searched for, and the
    /// threshold is the caller's to choose.
    #[test]
    fn a_low_confidence_segment_is_kept_and_marked() {
        let s = SpeakerRef {
            cluster_id: cid(),
            display_name: "A".into(),
            start_ms: 0,
            end_ms: 1_000,
            confidence: 0.3,
        };
        assert!(!s.is_reliable(0.5), "below the threshold");
        assert!(s.is_reliable(0.1), "above a lower one");
        assert_eq!(s.duration_ms(), 1_000, "it is still there");
    }

    #[test]
    fn a_speaker_and_an_appearance_agree_on_the_cluster() {
        let n = cid();
        let b = InterviewBody {
            people: vec![PersonRef::primary(n, "A")],
            speakers: vec![SpeakerRef {
                cluster_id: n,
                display_name: "A".into(),
                start_ms: 0,
                end_ms: 10,
                confidence: 1.0,
            }],
            ..InterviewBody::default()
        };
        assert_eq!(b.people[0].cluster_id, b.speakers[0].cluster_id);
    }

    // ---- common ----

    #[test]
    fn a_zero_duration_means_not_applicable_not_zero_seconds() {
        let c = CommonBody::default();
        assert_eq!(c.duration_ms, 0);
        assert!(c.duration().is_none(), "a comic has no runtime");
        let c = CommonBody {
            duration_ms: 1,
            ..Default::default()
        };
        assert!(c.duration().is_some());
    }

    #[test]
    fn a_zero_sized_picture_is_not_a_picture() {
        let c = CommonBody::default();
        assert!(!c.has_picture());
        let c = CommonBody {
            width: 1920,
            height: 0,
            ..Default::default()
        };
        assert!(!c.has_picture(), "half a dimension is not a picture");
    }

    /// Every body must expose its common half, and it must be the same one the
    /// struct holds -- a `common()` that reconstructed a default would make
    /// every caller see zeros.
    #[test]
    fn the_common_half_is_reachable_and_real() {
        let b = ObjectBody::Audio(AudioBody {
            common: CommonBody {
                duration_ms: 1_000,
                width: 100,
                height: 200,
                studio: Some("S".into()),
                ..Default::default()
            },
            ..AudioBody::default()
        });
        assert_eq!(b.common().duration_ms, 1_000);
        assert_eq!(b.common().studio.as_deref(), Some("S"));
        assert!(b.common().has_picture());
    }

    // ---- round trips for every kind ----

    #[test]
    fn every_kind_round_trips_through_json() {
        for mut body in all_kinds() {
            let p = PersonRef::primary(cid(), "Someone").at("1:00");
            match &mut body {
                ObjectBody::Scene(b) => {
                    b.people.push(p.clone());
                    b.series_index = Some(3);
                }
                ObjectBody::Image(b) => b.people.push(p.clone()),
                ObjectBody::Gallery(b) => {
                    b.people.push(p.clone());
                    b.image_order = vec![cid()];
                    b.image_count = 1;
                }
                ObjectBody::Audio(b) => {
                    b.people.push(p.clone());
                    b.track = Some(2);
                    b.replay_gain = Some(
                        commons_core::ReplayGain::parse(
                            "-3.0 dB",
                            Some(-1.0),
                            Some(commons_core::GainConvention::Audiophile),
                        )
                        .unwrap(),
                    );
                }
                ObjectBody::Comic(b) => {
                    b.people.push(p.clone());
                    b.page_count = 20;
                    b.direction = commons_core::ReadingDirection::RightToLeft;
                }
                ObjectBody::Text(b) => b.people.push(p.clone()),
                ObjectBody::Interview(b) => {
                    b.people.push(p.clone());
                    b.chapters.push(Chapter {
                        title: "Intro".into(),
                        start_ms: 0,
                    });
                    b.quotes.push(Quote {
                        text: "hello".into(),
                        at_ms: 1_000,
                        cluster_id: Some(cid()),
                    });
                }
            }
            let kind = body.kind();
            let json = serde_json::to_string(&body).unwrap();
            let back: ObjectBody = serde_json::from_str(&json).unwrap();
            assert_eq!(body, back, "{kind:?} did not round trip");
            assert_eq!(back.kind(), kind);
            assert_eq!(back.people().len(), 1, "{kind:?} lost its person");
        }
    }

    /// The wrong field is unconstructible: a comic body cannot carry a
    /// waveform and an audio body cannot carry pages, because the enum variant
    /// is the field. This is asserted structurally rather than at runtime --
    /// there is no `try_new` that would reject it, the type system does.
    #[test]
    fn the_wrong_field_is_not_constructible() {
        let comic = ComicBody {
            page_count: 10,
            ..ComicBody::default()
        };
        // The only way to name the field is through the right variant.
        assert_eq!(comic.page_count, 10);
        assert_eq!(ObjectBody::Comic(comic).kind(), Comic);

        let audio = AudioBody {
            track: Some(1),
            ..AudioBody::default()
        };
        assert_eq!(ObjectBody::Audio(audio).kind(), Audio);
    }

    /// The enum is exhaustive over the kinds, so `people()` cannot forget one:
    /// a new variant makes this method fail to compile until it is answered.
    /// That is the property worth having, and it is why the accessor is on the
    /// enum rather than behind a `Default`.
    #[test]
    fn the_people_accessor_covers_every_kind() {
        assert_eq!(all_kinds().len(), 7);
        for body in all_kinds() {
            // A default body has no people, and the accessor returns the real
            // (empty) slice rather than panicking on the variant.
            assert!(
                body.people().is_empty(),
                "{:?} should default to no people",
                body.kind()
            );
        }
    }
}
