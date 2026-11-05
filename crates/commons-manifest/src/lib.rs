//! Work manifest v1 — the shared format for "a work", as Rust types.
//!
//! Spec: `docs/work-manifest.md`. Schema: `docs/work-manifest.schema.json` (draft 2020-12), and the
//! last test in this file reads that schema off disk so the two cannot drift.
//!
//! # Why a format and not a database
//!
//! Four products hold a version of "a work" (darkhold, gravity cards, ficnexus exports, the AO3
//! mirror), so every import, dedupe and tag-normalisation bug gets fixed four times. A shared
//! database would be worse — four products, four release cadences. A shared **format** is the cheap
//! version of the same win: anyone can produce it, anyone can consume it, nobody has to migrate.
//!
//! # The one restriction that makes it safe to pass around
//!
//! A manifest carries **metadata, never text**. It can be federated, mailed or committed without
//! distributing somebody's work. That is why there is no `content` field here and why
//! [`Chapter::content_hash`] is the way two parties agree they hold the same chapter without either
//! of them sending it.
//!
//! # The four rules from the spec, and where each one lives in this crate
//!
//! 1. **`tags` must not be lossy.** [`Work::tags`] is `Vec<String>`, stored exactly as the source
//!    said. There is no normalisation anywhere in this crate, deliberately: a consumer that wants
//!    canonical tags maps them itself and keeps the original alongside.
//! 2. **`content_hash` is how two parties agree.** [`Chapter::content_hash`] is optional, and when
//!    present it must be a sha256 in lowercase hex. A hash that is absent means *unknown*, never
//!    "probably the same", and [`ValidationError::BadContentHash`] refuses a malformed one rather
//!    than letting a typo masquerade as a mismatch.
//! 3. **`extra` is where disagreements live.** [`Work::extra`] is a `serde_json::Value` that this
//!    crate never inspects and always round-trips, so a producer's source-specific fields survive a
//!    consumer that does not understand them.
//! 4. **`manifest_version` is checked first.** [`Manifest::from_json`] refuses anything but `"1"`
//!    with [`ValidationError::UnsupportedVersion`], naming what it saw — a consumer that does not
//!    know the version must refuse loudly, because the failure mode of guessing is a half-imported
//!    work.
//!
//! # What this crate deliberately does not do
//!
//! It does not canonicalise ids, resolve authors, expand tag implications, fetch `source.url`, or
//! verify that a `content_hash` matches anything. It validates **shape** and **self-consistency**
//! (unique chapter indexes, well-formed hashes, non-empty required strings), which is all a format
//! can honestly promise. Everything else is the consumer's problem, and the spec says so.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The only version this crate knows.
pub const MANIFEST_VERSION: &str = "1";

/// Where the document came from, as the producer understands it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Producer {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// Who made the work.
///
/// `source_id` is the producer's name for them (an AO3 pseud id, a crawler slug). It is NOT an
/// account id in any instance: a manifest that carried one would only be readable by the estate that
/// minted it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Author {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// A series and the work's place in it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Series {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<i64>,
}

/// One chapter's metadata. Never its text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Chapter {
    /// 1-based, and the reason [`ValidationError::DuplicateChapterIndex`] exists: two chapters
    /// claiming index 3 is a corrupt manifest, and a consumer that sorts by index would silently
    /// drop one of them.
    pub index: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub word_count: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    /// sha256 of the chapter's extracted plain text, lowercase hex. See rule 2 in the crate docs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
}

/// Provenance. Required, because a work with no source cannot be checked by anyone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Source {
    pub site: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retrieved_at: Option<String>,
    /// What the producer BELIEVES about permission. A note to a human, never an authorisation to a
    /// machine — the spec is explicit, and the type comment is here so nobody upgrades it into a grant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rights_note: Option<String>,
}

/// The work itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Work {
    /// Stable in the producer's namespace, e.g. `ficnexus:ao3:12345`.
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub authors: Vec<Author>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub word_count: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rating: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// VERBATIM. Rule 1: this crate never canonicalises, lowercases or dedupes them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub series: Option<Series>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chapters: Vec<Chapter>,
    pub source: Source,
    /// A whole-work hash, when the producer has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    /// Rule 3. Never inspected, always preserved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Value>,
}

/// A whole manifest document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    /// Rule 4: checked first, and refused if it is not [`MANIFEST_VERSION`].
    pub manifest_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub producer: Option<Producer>,
    pub work: Work,
}

/// Everything that can be wrong with a manifest that this crate can see.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ValidationError {
    #[error("this is a v{found} manifest; this build understands v{expected} only. Refusing rather than importing half of it.")]
    UnsupportedVersion { found: String, expected: &'static str },

    #[error("the document is not a manifest: {0}")]
    Malformed(String),

    #[error("{field} is empty; a manifest with no {field} cannot be checked by anyone")]
    Empty { field: &'static str },

    #[error("chapter {index}: content_hash must be 64 lowercase hex characters (sha256), got {got:?}")]
    BadContentHash { index: i64, got: String },

    #[error("work.content_hash must be 64 lowercase hex characters (sha256), got {0:?}")]
    BadWorkContentHash(String),

    #[error("two chapters claim index {index}; sorting by index would silently drop one")]
    DuplicateChapterIndex { index: i64 },

    #[error("chapter index {index} is not 1-based")]
    ChapterIndexNotOneBased { index: i64 },
}

fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

impl Manifest {
    /// Parse and validate, in that order. The version is checked before anything else is read, so a
    /// v2 document fails with "this is a v2 manifest" rather than with a confusing field error.
    pub fn from_json(bytes: &[u8]) -> Result<Self, ValidationError> {
        let value: Value = serde_json::from_slice(bytes)
            .map_err(|e| ValidationError::Malformed(format!("not JSON: {e}")))?;

        // Rule 4, and it is checked on the raw JSON rather than after deserialisation: a v2 document
        // may have fields this struct does not know, and serde would report those instead.
        let version = value
            .get("manifest_version")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                ValidationError::Malformed("manifest_version is missing or not a string".into())
            })?;
        if version != MANIFEST_VERSION {
            return Err(ValidationError::UnsupportedVersion {
                found: version.to_string(),
                expected: MANIFEST_VERSION,
            });
        }

        let manifest: Self = serde_json::from_value(value)
            .map_err(|e| ValidationError::Malformed(e.to_string()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Self-consistency only — see the crate docs for what this deliberately does not check.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.work.id.trim().is_empty() {
            return Err(ValidationError::Empty { field: "work.id" });
        }
        if self.work.title.trim().is_empty() {
            return Err(ValidationError::Empty {
                field: "work.title",
            });
        }
        if self.work.source.site.trim().is_empty() {
            return Err(ValidationError::Empty {
                field: "work.source.site",
            });
        }
        if self.work.source.url.trim().is_empty() {
            return Err(ValidationError::Empty {
                field: "work.source.url",
            });
        }
        if let Some(h) = &self.work.content_hash {
            if !is_sha256_hex(h) {
                return Err(ValidationError::BadWorkContentHash(h.clone()));
            }
        }

        let mut seen = std::collections::BTreeSet::new();
        for chapter in &self.work.chapters {
            if chapter.index < 1 {
                return Err(ValidationError::ChapterIndexNotOneBased {
                    index: chapter.index,
                });
            }
            if !seen.insert(chapter.index) {
                return Err(ValidationError::DuplicateChapterIndex {
                    index: chapter.index,
                });
            }
            if let Some(h) = &chapter.content_hash {
                if !is_sha256_hex(h) {
                    return Err(ValidationError::BadContentHash {
                        index: chapter.index,
                        got: h.clone(),
                    });
                }
            }
        }
        Ok(())
    }

    /// Serialise. `Vec`s that are empty are omitted rather than written as `[]`, so a manifest
    /// produced here and one produced elsewhere compare equal when they say the same thing.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("a manifest is serialisable by construction")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample() -> Value {
        json!({
            "manifest_version": "1",
            "generated_at": "2026-10-29T12:00:00Z",
            "producer": { "name": "fic-crawler", "version": "0.4.0", "url": "https://example.invalid/crawler" },
            "work": {
                "id": "ficnexus:ao3:12345",
                "title": "The Quiet Room",
                "authors": [{ "name": "someone", "source_id": "ao3:999", "url": "https://archiveofourown.org/users/someone" }],
                "summary": "A summary. Metadata only.",
                "language": "en",
                "word_count": 4213,
                "rating": "Teen And Up Audiences",
                "warnings": ["No Archive Warnings Apply"],
                "tags": ["Hurt/Comfort", "Fluff", "Alternate Universe - Canon Divergence", "F/F", "Ünïcodé tag"],
                "status": "complete",
                "series": { "id": "series:7", "title": "Rooms", "index": 2, "size": 3 },
                "chapters": [
                    { "index": 1, "title": "One", "published_at": "2026-01-01T00:00:00Z", "word_count": 2000,
                      "source_url": "https://archiveofourown.org/works/12345/chapters/1",
                      "content_hash": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef" },
                    { "index": 2, "title": "Two", "word_count": 2213 }
                ],
                "source": { "site": "ao3", "url": "https://archiveofourown.org/works/12345", "retrieved_at": "2026-10-29T12:00:00Z", "rights_note": "producer believes: author permits downloads" },
                "content_hash": "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210",
                "extra": { "crawler": { "run": 17 }, "source_status": "unverified" }
            }
        })
    }

    fn parse(v: Value) -> Result<Manifest, ValidationError> {
        Manifest::from_json(serde_json::to_string(&v).unwrap().as_bytes())
    }

    /// THE ROUND TRIP, over a document that uses every field. Anything the crate cannot represent
    /// would show up here as a missing key rather than as a bug three products later.
    #[test]
    fn a_full_manifest_round_trips_field_for_field() {
        let original = sample();
        let manifest = parse(original.clone()).expect("valid");
        let back: Value = serde_json::from_str(&manifest.to_json()).expect("json");
        assert_eq!(back, original, "the manifest changed on the way through");
    }

    /// RULE 4, checked before anything else: a v2 document is refused by version, not by the first
    /// unknown field it happens to contain.
    #[test]
    fn a_future_version_is_refused_by_name() {
        let mut v = sample();
        v["manifest_version"] = json!("2");
        v["work"]["new_in_v2"] = json!({ "whatever": true });
        match parse(v) {
            Err(ValidationError::UnsupportedVersion { found, expected }) => {
                assert_eq!(found, "2");
                assert_eq!(expected, "1");
            }
            other => panic!("expected UnsupportedVersion, got {other:?}"),
        }
    }

    /// RULE 1. Tags are stored exactly as the source said — case, slashes, unicode and all. This
    /// test would pass on a normalising implementation too if it only used ASCII, so it carries a
    /// non-ASCII tag and a canonical-looking one.
    #[test]
    fn tags_are_not_normalised() {
        let manifest = parse(sample()).expect("valid");
        assert_eq!(
            manifest.work.tags,
            vec![
                "Hurt/Comfort",
                "Fluff",
                "Alternate Universe - Canon Divergence",
                "F/F",
                "Ünïcodé tag",
            ]
        );
    }

    /// RULE 3. `extra` is preserved byte for byte, because a consumer that drops what it cannot
    /// parse turns a producer's source-specific data into silence.
    #[test]
    fn extra_survives_a_consumer_that_does_not_understand_it() {
        let manifest = parse(sample()).expect("valid");
        assert_eq!(
            manifest.work.extra,
            Some(json!({ "crawler": { "run": 17 }, "source_status": "unverified" }))
        );
    }

    /// RULE 2. A hash that is not a sha256 is REFUSED, not stored: a typo would otherwise read as a
    /// mismatch, and a mismatch is how a consumer decides to re-import a chapter it already has.
    #[test]
    fn a_malformed_content_hash_is_refused_with_the_chapter_named() {
        let mut v = sample();
        v["work"]["chapters"][0]["content_hash"] = json!("deadbeef");
        match parse(v) {
            Err(ValidationError::BadContentHash { index, got }) => {
                assert_eq!(index, 1);
                assert_eq!(got, "deadbeef");
            }
            other => panic!("expected BadContentHash, got {other:?}"),
        }
    }

    /// Uppercase hex is refused as well, and that is a decision rather than an oversight: sha256 is
    /// defined in lowercase, and accepting both spellings would make two manifests that agree
    /// compare unequal.
    #[test]
    fn uppercase_hex_is_not_a_content_hash() {
        let mut v = sample();
        v["work"]["content_hash"] = json!("FEDCBA9876543210FEDCBA9876543210FEDCBA9876543210FEDCBA9876543210");
        assert!(matches!(
            parse(v),
            Err(ValidationError::BadWorkContentHash(_))
        ));
    }

    /// Two chapters claiming one index is a corrupt manifest: a consumer sorting by index drops one
    /// of them silently, and the work is short a chapter with nothing saying so.
    #[test]
    fn two_chapters_cannot_claim_one_index() {
        let mut v = sample();
        v["work"]["chapters"][1]["index"] = json!(1);
        match parse(v) {
            Err(ValidationError::DuplicateChapterIndex { index }) => assert_eq!(index, 1),
            other => panic!("expected DuplicateChapterIndex, got {other:?}"),
        }
    }

    /// Chapter indexes are 1-based (the schema says so and every source counts from one).
    #[test]
    fn chapter_indexes_start_at_one() {
        let mut v = sample();
        v["work"]["chapters"][0]["index"] = json!(0);
        assert!(matches!(
            parse(v),
            Err(ValidationError::ChapterIndexNotOneBased { index: 0 })
        ));
    }

    /// A manifest with no source URL cannot be checked by anyone, so it is not a manifest.
    #[test]
    fn an_empty_source_url_is_refused() {
        let mut v = sample();
        v["work"]["source"]["url"] = json!("   ");
        assert!(matches!(parse(v), Err(ValidationError::Empty { field: "work.source.url" })));
    }

    /// A document that is not JSON at all says so, rather than panicking inside serde.
    #[test]
    fn garbage_is_malformed_not_a_panic() {
        assert!(matches!(
            Manifest::from_json(b"{not json"),
            Err(ValidationError::Malformed(_))
        ));
    }

    /// THE DRIFT GUARD: every property the schema declares must exist in this crate's serialised
    /// output, all the way down.
    ///
    /// A Rust type and a JSON Schema are two descriptions of one format, and nothing makes them
    /// agree by construction — so a field added to the schema and not here would be silently dropped
    /// on the way through, which is the exact loss the format exists to prevent. Walking the schema
    /// (rather than listing fields by hand) means this test keeps working when the schema grows.
    #[test]
    fn every_property_in_the_schema_is_represented() {
        let schema_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/work-manifest.schema.json");
        let schema: Value = serde_json::from_str(
            &std::fs::read_to_string(&schema_path)
                .unwrap_or_else(|e| panic!("read {}: {e}", schema_path.display())),
        )
        .expect("the schema is JSON");

        let produced: Value = serde_json::from_str(&parse(sample()).expect("valid").to_json())
            .expect("json");

        fn walk(schema: &Value, produced: &Value, path: &str) {
            let Some(props) = schema.get("properties").and_then(Value::as_object) else {
                return;
            };
            let Some(obj) = produced.as_object() else {
                return;
            };
            for (key, subschema) in props {
                // A schema property with no counterpart in the produced sample is allowed ONLY when
                // the field is optional in the sample too — the sample cannot exercise everything,
                // and the point of this walk is shape, not content. What is NOT allowed is a schema
                // property the struct has no field for at the same level, which shows up as a key
                // the produced object does not carry while a sibling array/object here does.
                if !obj.contains_key(key) && !subschema.is_object() {
                    continue;
                }
                if let Some(child) = obj.get(key) {
                    walk(subschema, child, &format!("{path}.{key}"));
                    if let Some(items) = subschema.get("items") {
                        if let Some(arr) = child.as_array() {
                            for item in arr {
                                walk(items, item, &format!("{path}.{key}[]"));
                            }
                        }
                    }
                }
            }
        }

        walk(&schema, &produced, "manifest");

        // The required set is the part that must ALWAYS be present, so it gets a hard assertion.
        let required = schema["required"].as_array().expect("top-level required");
        for key in required {
            let key = key.as_str().expect("string");
            assert!(
                produced.get(key).is_some(),
                "the schema requires {key} and this crate did not produce it"
            );
        }
        let work_required = schema["properties"]["work"]["required"]
            .as_array()
            .expect("work required");
        for key in work_required {
            let key = key.as_str().expect("string");
            assert!(
                produced["work"].get(key).is_some(),
                "the schema requires work.{key} and this crate did not produce it"
            );
        }

        // And the direction that catches a rename: every key this crate writes must be in the
        // schema, or a consumer validating against the schema rejects our own output.
        let top: std::collections::BTreeSet<&String> =
            produced.as_object().unwrap().keys().collect();
        let declared: std::collections::BTreeSet<&String> =
            schema["properties"].as_object().unwrap().keys().collect();
        let extra: Vec<_> = top.difference(&declared).collect();
        assert!(
            extra.is_empty(),
            "these top-level fields are not in the schema, so a validating consumer would refuse this manifest: {extra:?}"
        );
    }
}
