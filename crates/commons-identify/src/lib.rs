//! Stable identifiers — **identity over location**.
//!
//! A URL is where a thing was; an identifier is which thing it is. Two catalogue rows carrying the same ISBN are
//! the same book however many times they were imported from however many sites, and a track's ISRC survives
//! every re-upload of the file it lives in.
//!
//! # Why this is a crate rather than a module in one product
//!
//! Every product in the family faces the same question with different nouns: a fanfiction archive equating two
//! imports of one fic, a booru equating a repost of one image set, a video library equating a scene across two
//! studios' catalogues. The *answer* — canonical form plus a check that the answer is actually valid — does not
//! depend on the nouns. This crate owns the answer and nothing else: no storage, no HTTP, no product types.
//!
//! # What it refuses, and why that is the point
//!
//! * **Wrong check digits.** An ISBN that fails its own checksum is refused rather than stored, because an
//!   identifier that is wrong will confidently match the *wrong* thing — worse than having none.
//! * **Ambiguous identifiers.** A bare TMDB number does not say film or series, and guessing produces a link
//!   that quietly points at the wrong page. `movie:603` and `tv:1399` are the accepted shapes; `603` is a
//!   refusal that says what to write instead.
//! * **Empty input.** `"  "` is not an identifier, and normalising it to `""` would make every empty string
//!   compare equal to every other one.
//!
//! # Extending it
//!
//! A sixth scheme is an arm in [`Scheme`] plus a validator function; the scheme word is what gets stored, so a
//! caller never has to know the enum's numeric order. Nothing here is product-specific, and nothing here may
//! become so: a validator that needs a database is not an identifier.

use std::fmt;

use serde::{Deserialize, Serialize};

/// The identifier schemes this crate knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scheme {
    /// International Standard Book Number — 10 or 13 digits, with a check digit.
    Isbn,
    /// The Movie Database identifier, which is only meaningful with its kind.
    Tmdb,
    /// MusicBrainz release-group MBID — a UUID.
    Musicbrainz,
    /// International Standard Recording Code — 12 characters.
    Isrc,
    /// Wikidata item, `Q` followed by digits.
    Wikidata,
}

impl Scheme {
    /// The wire word, which is also the word stored in a database column.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Isbn => "isbn",
            Self::Tmdb => "tmdb",
            Self::Musicbrainz => "musicbrainz",
            Self::Isrc => "isrc",
            Self::Wikidata => "wikidata",
        }
    }

    /// Parse a wire word. A few aliases are accepted on the way in (`mbid`, `isbn13`, `wd`) because they are
    /// what callers actually type; only [`Scheme::as_str`] is written back out.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "isbn" | "isbn10" | "isbn13" => Some(Self::Isbn),
            "tmdb" => Some(Self::Tmdb),
            "musicbrainz" | "mbid" => Some(Self::Musicbrainz),
            "isrc" => Some(Self::Isrc),
            "wikidata" | "wd" => Some(Self::Wikidata),
            _ => None,
        }
    }

    /// Every scheme, in the order an interface lists them.
    #[must_use]
    pub fn all() -> [Self; 5] {
        [
            Self::Isbn,
            Self::Tmdb,
            Self::Musicbrainz,
            Self::Isrc,
            Self::Wikidata,
        ]
    }
}

impl fmt::Display for Scheme {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A canonical identifier, or the reason the input is not one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Identified {
    /// Canonical form, ready to store and compare.
    Canonical(String),
    /// Refused, with a sentence naming what is wrong.
    Refused(String),
}

impl Identified {
    /// The canonical value, when there is one.
    #[must_use]
    pub fn value(&self) -> Option<&str> {
        match self {
            Self::Canonical(value) => Some(value),
            Self::Refused(_) => None,
        }
    }

    /// The refusal, when there is one.
    #[must_use]
    pub fn refusal(&self) -> Option<&str> {
        match self {
            Self::Canonical(_) => None,
            Self::Refused(reason) => Some(reason),
        }
    }

    /// Whether this is a canonical value.
    #[must_use]
    pub fn is_canonical(&self) -> bool {
        matches!(self, Self::Canonical(_))
    }
}

/// Canonicalise `raw` as `scheme`.
///
/// # Examples
///
/// ```
/// use commons_identify::{canonical, Scheme};
/// assert_eq!(canonical(Scheme::Isbn, "978-0-306-40615-7").value(), Some("9780306406157"));
/// assert!(canonical(Scheme::Tmdb, "603").refusal().is_some());
/// ```
#[must_use]
pub fn canonical(scheme: Scheme, raw: &str) -> Identified {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Identified::Refused("an empty identifier is not an identifier".to_owned());
    }
    match scheme {
        Scheme::Isbn => isbn(trimmed),
        Scheme::Tmdb => tmdb(trimmed),
        Scheme::Musicbrainz => mbid(trimmed),
        Scheme::Isrc => isrc(trimmed),
        Scheme::Wikidata => wikidata(trimmed),
    }
}

/// Whether two (scheme, value) pairs name the same thing — the question this crate exists to answer.
///
/// A refusal on either side is `false`: two invalid identifiers are not the same thing, and treating them as
/// equal would merge unrelated rows.
#[must_use]
pub fn same(scheme: Scheme, left: &str, right: &str) -> bool {
    match (canonical(scheme, left), canonical(scheme, right)) {
        (Identified::Canonical(a), Identified::Canonical(b)) => a == b,
        _ => false,
    }
}

fn digits_only(text: &str) -> String {
    text.chars()
        .filter(|ch| !ch.is_whitespace() && *ch != '-')
        .collect()
}

fn isbn(raw: &str) -> Identified {
    let cleaned = digits_only(raw);
    match cleaned.len() {
        10 => {
            let body = &cleaned[..9];
            if !body.chars().all(|ch| ch.is_ascii_digit()) {
                return Identified::Refused(format!(
                    "{raw} is not an ISBN: the first nine characters must be digits"
                ));
            }
            let Some(check) = cleaned.chars().nth(9) else {
                return Identified::Refused("an empty identifier is not an identifier".to_owned());
            };
            let check_value = match check {
                'X' | 'x' => 10,
                ch if ch.is_ascii_digit() => ch.to_digit(10).unwrap_or(0),
                _ => {
                    return Identified::Refused(format!(
                        "{raw} is not an ISBN: the check character must be a digit or X"
                    ))
                }
            };
            // ISBN-10: weights 10..2 over the first nine characters, plus the check value, divisible by 11.
            let sum: u32 = body
                .chars()
                .enumerate()
                .map(|(index, ch)| (10 - index as u32) * ch.to_digit(10).unwrap_or(0))
                .sum::<u32>()
                + check_value;
            if !sum.is_multiple_of(11) {
                return Identified::Refused(format!(
                    "{raw} fails its ISBN-10 check digit — an identifier that is wrong will match the wrong book"
                ));
            }
            Identified::Canonical(cleaned.to_uppercase())
        }
        13 => {
            if !cleaned.chars().all(|ch| ch.is_ascii_digit()) {
                return Identified::Refused(format!(
                    "{raw} is not an ISBN-13: every character must be a digit"
                ));
            }
            // ISBN-13: alternating 1/3 weights, divisible by 10.
            let sum: u32 = cleaned
                .chars()
                .enumerate()
                .map(|(index, ch)| {
                    let value = ch.to_digit(10).unwrap_or(0);
                    match index % 2 {
                        0 => value,
                        _ => value * 3,
                    }
                })
                .sum();
            if !sum.is_multiple_of(10) {
                return Identified::Refused(format!(
                    "{raw} fails its ISBN-13 check digit — an identifier that is wrong will match the wrong book"
                ));
            }
            Identified::Canonical(cleaned)
        }
        other => Identified::Refused(format!(
            "{raw} is not an ISBN: 10 or 13 digits after removing hyphens, found {other}"
        )),
    }
}

fn isrc(raw: &str) -> Identified {
    let cleaned: String = raw
        .chars()
        .filter(|ch| !ch.is_whitespace() && *ch != '-')
        .collect::<String>()
        .to_uppercase();
    if cleaned.len() != 12 {
        return Identified::Refused(format!(
            "{raw} is not an ISRC: 12 characters (country, registrant, year, designation), found {}",
            cleaned.len()
        ));
    }
    let bytes = cleaned.as_bytes();
    let country_ok = bytes[..2].iter().all(u8::is_ascii_alphabetic);
    let registrant_ok = bytes[2..5].iter().all(u8::is_ascii_alphanumeric);
    let serial_ok = bytes[5..].iter().all(u8::is_ascii_digit);
    if !(country_ok && registrant_ok && serial_ok) {
        return Identified::Refused(format!(
            "{raw} is not an ISRC: two letters, three alphanumerics, then seven digits"
        ));
    }
    Identified::Canonical(cleaned)
}

fn mbid(raw: &str) -> Identified {
    match uuid::Uuid::parse_str(raw.trim()) {
        // Lowercase, hyphenated: the form MusicBrainz prints, so two spellings are one key rather than two rows
        // that never compare equal.
        Ok(parsed) => Identified::Canonical(parsed.hyphenated().to_string()),
        Err(_) => Identified::Refused(format!(
            "{raw} is not a MusicBrainz identifier: a release group is named by a UUID"
        )),
    }
}

fn tmdb(raw: &str) -> Identified {
    let trimmed = raw.trim().to_ascii_lowercase();
    let Some((kind, number)) = trimmed.split_once(':') else {
        return Identified::Refused(format!(
            "{raw} does not say whether it is a film or a series — write movie:{raw} or tv:{raw}"
        ));
    };
    let kind = kind.trim();
    if kind != "movie" && kind != "tv" {
        return Identified::Refused(format!("{raw}: the kind must be movie or tv, not {kind}"));
    }
    let number = number.trim();
    if number.is_empty() || !number.chars().all(|ch| ch.is_ascii_digit()) {
        return Identified::Refused(format!(
            "{raw}: the identifier after {kind}: must be a number"
        ));
    }
    let normalised = number.trim_start_matches('0');
    if normalised.is_empty() {
        return Identified::Refused(format!("{raw}: TMDB identifiers start at 1"));
    }
    Identified::Canonical(format!("{kind}:{normalised}"))
}

fn wikidata(raw: &str) -> Identified {
    let trimmed = raw.trim().to_uppercase();
    let mut chars = trimmed.chars();
    if chars.next() != Some('Q') || !chars.all(|ch| ch.is_ascii_digit()) || trimmed.len() < 2 {
        return Identified::Refused(format!(
            "{raw} is not a Wikidata item: Q followed by digits, e.g. Q42"
        ));
    }
    Identified::Canonical(trimmed)
}
