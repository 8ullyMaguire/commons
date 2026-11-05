//! What makes two rows the same thing, and which inputs are refused.

use commons_identify::{canonical, same, Identified, Scheme};

fn value(scheme: Scheme, raw: &str) -> String {
    canonical(scheme, raw)
        .value()
        .unwrap_or_else(|| panic!("{raw} should be a valid {scheme}"))
        .to_owned()
}

fn refused(scheme: Scheme, raw: &str) -> String {
    canonical(scheme, raw)
        .refusal()
        .unwrap_or_else(|| panic!("{raw} should be refused as {scheme}"))
        .to_owned()
}

#[test]
fn an_isbn_has_one_canonical_spelling_and_its_check_digit_is_checked() {
    // Three spellings, one book.
    for spelling in ["978-0-306-40615-7", "9780306406157", "978 0 306 40615 7"] {
        assert_eq!(value(Scheme::Isbn, spelling), "9780306406157", "{spelling}");
    }
    assert!(same(Scheme::Isbn, "978-0-306-40615-7", "9780306406157"));

    // ISBN-10 keeps its length, accepts X as the check character, and normalises it.
    assert_eq!(value(Scheme::Isbn, "0-306-40615-2"), "0306406152");
    assert_eq!(value(Scheme::Isbn, "080442957x"), "080442957X");

    // A wrong check digit is refused, and the sentence says why it matters.
    assert!(refused(Scheme::Isbn, "9780306406158").contains("check digit"));
    assert!(refused(Scheme::Isbn, "0306406153").contains("check digit"));
    // Wrong length is its own refusal, not a checksum one.
    assert!(refused(Scheme::Isbn, "12345").contains("10 or 13 digits"));
}

#[test]
fn an_isrc_is_uppercase_twelve_characters_with_a_shape() {
    assert_eq!(value(Scheme::Isrc, "gb-um7-24-00001"), "GBUM72400001");
    assert_eq!(value(Scheme::Isrc, "GB UM7 24 00001"), "GBUM72400001");
    assert!(refused(Scheme::Isrc, "GBUM7240000").contains("12 characters"));
    assert!(refused(Scheme::Isrc, "1BUM72400001").contains("two letters"));
    assert!(refused(Scheme::Isrc, "GBUM7240000X").contains("seven digits"));
}

#[test]
fn a_musicbrainz_id_is_a_uuid_in_lowercase() {
    assert_eq!(
        value(Scheme::Musicbrainz, "0F2B0F8F-1F5E-4C3A-9E3B-0F8E5C1A2B3C"),
        "0f2b0f8f-1f5e-4c3a-9e3b-0f8e5c1a2b3c"
    );
    assert!(
        same(
            Scheme::Musicbrainz,
            "{0f2b0f8f-1f5e-4c3a-9e3b-0f8e5c1a2b3c}",
            "0f2b0f8f1f5e4c3a9e3b0f8e5c1a2b3c"
        ),
        "uuid spellings collapse to one key"
    );
    assert!(refused(Scheme::Musicbrainz, "release-group-123").contains("UUID"));
}

#[test]
fn a_tmdb_id_without_its_kind_is_refused_rather_than_guessed() {
    assert_eq!(value(Scheme::Tmdb, "movie:603"), "movie:603");
    assert_eq!(value(Scheme::Tmdb, " TV:1399 "), "tv:1399");
    assert_eq!(
        value(Scheme::Tmdb, "movie:0603"),
        "movie:603",
        "leading zeros are the same page"
    );
    let bare = refused(Scheme::Tmdb, "603");
    assert!(
        bare.contains("movie:603") && bare.contains("tv:603"),
        "{bare}"
    );
    assert!(refused(Scheme::Tmdb, "film:603").contains("must be movie or tv"));
    assert!(refused(Scheme::Tmdb, "movie:").contains("must be a number"));
    assert!(refused(Scheme::Tmdb, "movie:0").contains("start at 1"));
}

#[test]
fn a_wikidata_item_is_q_and_digits_and_nothing_else() {
    assert_eq!(value(Scheme::Wikidata, "q42"), "Q42");
    assert_eq!(value(Scheme::Wikidata, " Q42 "), "Q42");
    assert!(refused(Scheme::Wikidata, "42").contains("Q followed by digits"));
    assert!(refused(Scheme::Wikidata, "Q").contains("Q followed by digits"));
    assert!(refused(Scheme::Wikidata, "Q4x").contains("Q followed by digits"));
}

#[test]
fn an_unrecognised_scheme_is_refused_and_a_refusal_is_never_a_value() {
    assert_eq!(Scheme::parse("isbn"), Some(Scheme::Isbn));
    assert_eq!(Scheme::parse(" MBID "), Some(Scheme::Musicbrainz));
    assert_eq!(Scheme::parse("doi"), None);
    assert_eq!(Scheme::all().len(), 5);

    let empty = canonical(Scheme::Isbn, "   ");
    assert!(empty.value().is_none());
    assert!(empty.refusal().is_some());
    // `same` is false when either side is refused: two invalid identifiers are not the same thing, and
    // treating them as equal would merge unrelated rows.
    assert!(!same(Scheme::Isbn, "nonsense", "nonsense"));
    assert!(matches!(empty, Identified::Refused(_)));
}

#[test]
fn a_scheme_round_trips_through_json_as_its_wire_word() {
    // The wire word is what a database column and an HTTP parameter carry, so the serde form has to be the same
    // word — not a Rust-variant spelling that a second language could not produce.
    for scheme in Scheme::all() {
        let json = serde_json::to_string(&scheme).expect("serialise");
        assert_eq!(json, format!("\"{}\"", scheme.as_str()));
        let back: Scheme = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back, scheme);
        assert_eq!(Scheme::parse(json.trim_matches('"')), Some(scheme));
    }
}
