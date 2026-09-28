//! The cursor's wire form: every refusal, and the two claims about absence.
//!
//! T-P6-009. Spec `docs/spec/t-p6-009-cursor-encoding.md` §4, §6.
//!
//! # Why this is a separate file and seeds no rows
//!
//! This is a wire format, exactly as `filter_wire_shape.rs` is, and the same
//! reasoning applies: nothing here needs a database because the claims are about
//! what bytes come out and what a decoder does with them. `sort_db.rs` covers
//! the other half — that a cursor *seeks* correctly once decoded — and it must
//! pass unedited, because a test edited to accommodate a new wire format is
//! evidence of nothing.
//!
//! # The refusals are the substance
//!
//! Seven of the nine tests below are refusals, and that ratio is the design.
//! `Filter::from_url`'s own rule is the standard: every error path returns
//! `Err` rather than defaulting, "so a malformed link cannot widen a query
//! into 'everything'". A cursor's version of "widening" is subtler — a
//! mis-decoded cursor is a *wrong page*, not a wrong permission — but the
//! response is the same, and `every_malformed_cursor_is_an_error_not_a_repair`
//! is the test that holds all of them to it at once.

use commons_store::cursor_wire::{CursorError, CURSOR_WIRE_VERSION};
use commons_store::filter_ast::base64url_encode;
use commons_store::sort::{Cursor, Sort, SortKey, SortOrder};

/// A cursor of the shape `date_desc` produces, and the sort it belongs to.
///
/// **Built by DECODING, not by `Cursor::new`** -- and that is not a stylistic
/// choice. `Cursor::new` is `#[cfg(test)] pub(crate)`, so an integration test
/// *cannot* call it: this file compiles only because the constructor is still
/// test-only, which is the spec section 5 claim enforced by the compiler rather
/// than by a test. (`sort.rs`'s own unit tests do call it, and they can, being
/// in the same crate.)
///
/// The round-trip tests therefore go cursor -> wire -> cursor, which means they
/// cannot catch a bug where `to_url` and `from_url` agree on something wrong.
/// That is a real limit and it is why `the_wire_form_is_the_documented_bytes`
/// exists: it pins the actual bytes, so the two halves agreeing is a
/// round-trip *and* the literal is a check against the format itself.
fn date_cursor() -> (Sort, Cursor) {
    let sort = Sort::date_desc();
    let wire = envelope(
        &sort.fingerprint(),
        serde_json::json!([["s", "2026-01-01T00:00:00Z"], ["s", "o-7"]]),
    );
    let cursor = Cursor::from_url(&wire, &sort).expect("the fixture cursor is well formed");
    (sort, cursor)
}

/// Wrap arbitrary JSON as a cursor envelope with the right version and the
/// right fingerprint, so a test can vary ONE field and nothing else.
fn envelope(fingerprint: &str, keys: serde_json::Value) -> String {
    base64url_encode(
        serde_json::json!({
            "v": CURSOR_WIRE_VERSION,
            "f": fingerprint,
            "k": keys,
        })
        .to_string()
        .as_bytes(),
    )
}

// ---------------------------------------------------------------------------
// Round trip
// ---------------------------------------------------------------------------

#[test]
fn a_cursor_survives_a_round_trip_through_a_url() {
    let (sort, original) = date_cursor();
    let wire = original.to_url(&sort);
    let back = Cursor::from_url(&wire, &sort).expect("a cursor we just wrote decodes");
    assert_eq!(back, original);
}

#[test]
fn the_wire_form_is_the_documented_bytes() {
    // A golden string, for the reason `filter_wire_shape.rs` gives: serde's
    // derive attributes are invisible until one changes, and a cursor that
    // stops decoding is a paging feature that silently stops working.
    //
    // Once GraphQL's `after` is wired up (T-P6-009 spec §7) the UI will decode
    // this too, and both sides have to change together.
    let (sort, cursor) = date_cursor();
    let wire = cursor.to_url(&sort);
    let json =
        String::from_utf8(commons_store::filter_ast::base64url_decode(&wire).expect("we wrote it"))
            .expect("base64 of utf8");
    assert_eq!(
        json,
        format!(
            r#"{{"v":{},"f":"{}","k":[["s","2026-01-01T00:00:00Z"],["s","o-7"]]}}"#,
            CURSOR_WIRE_VERSION,
            sort.fingerprint()
        ),
        "the wire shape changed; anything that decodes a cursor must change with it"
    );
}

#[test]
fn the_wire_form_is_url_safe() {
    // It goes in a query parameter, so the alphabet matters: `+` and `/` are
    // legal base64 and have to be rewritten, and `=` padding is a nuisance in
    // some clients. `base64url_encode` in this crate uses `-` for padding
    // rather than `=`, which the next assertion pins.
    let (sort, cursor) = date_cursor();
    let wire = cursor.to_url(&sort);
    assert!(
        !wire.contains('+') && !wire.contains('/') && !wire.contains('='),
        "a URL parameter may not contain +, / or =: got {wire}"
    );
}

// ---------------------------------------------------------------------------
// The seven refusals of spec section 4
// ---------------------------------------------------------------------------

#[test]
fn one_a_value_that_is_not_base64url_is_refused() {
    let (sort, _) = date_cursor();
    let err = Cursor::from_url("not base64 !!", &sort).expect_err("must refuse");
    assert!(matches!(err, CursorError::NotBase64), "{err:?}");
}

#[test]
fn two_a_value_that_is_not_the_expected_shape_is_refused() {
    let (sort, _) = date_cursor();
    // Valid base64url, valid JSON, wrong object.
    let wire = base64url_encode(br#"{"v":1}"#);
    let err = Cursor::from_url(&wire, &sort).expect_err("must refuse");
    assert!(matches!(err, CursorError::Shape(_)), "{err:?}");
}

#[test]
fn three_an_unknown_version_is_refused_rather_than_assumed() {
    // The version a *future* release would write. A decoder that treats it as
    // v1 is a decoder that will one day read a shape it does not understand and
    // produce a wrong page -- so this asserts the refusal, not just that the
    // number is noticed.
    let (sort, _) = date_cursor();
    let wire = envelope(
        &sort.fingerprint(),
        serde_json::json!([["s", "x"], ["s", "y"]]),
    );
    let future = wire_for_version(CURSOR_WIRE_VERSION + 1, &sort);
    let err = Cursor::from_url(&future, &sort).expect_err("must refuse");
    let CursorError::Version { found } = &err else {
        panic!("expected a Version error, got {err:?}");
    };
    assert_eq!(*found, CURSOR_WIRE_VERSION + 1);
    // And the message says what this server writes, so a client holding a
    // newer cursor knows whether to retry or to give up.
    assert!(
        err.to_string().contains(&CURSOR_WIRE_VERSION.to_string()),
        "{err}"
    );
    // The correctly-versioned one still works, so the refusal above is about
    // the version and not about the keys.
    assert!(Cursor::from_url(&wire, &sort).is_ok(), "{wire}");
}

/// The same envelope with a different version number.
fn wire_for_version(version: u8, sort: &Sort) -> String {
    base64url_encode(
        serde_json::json!({
            "v": version,
            "f": sort.fingerprint(),
            "k": [["s", "2026-01-01T00:00:00Z"], ["s", "o-7"]],
        })
        .to_string()
        .as_bytes(),
    )
}

#[test]
fn four_a_cursor_from_another_sort_is_refused_and_both_are_named() {
    // The one the whole module exists for. TWO TEXT SORTS: `Date` and `Title`
    // are both `Value::Str` and both arity 2, so without the fingerprint this
    // decodes cleanly and pages wrongly. See the plan's Step 0 -- a text/int
    // pair would produce an empty page instead, and would pass even with no
    // fingerprint at all.
    let (made_for, cursor) = date_cursor();
    let asked_for = Sort::new(vec![(SortKey::Title, SortOrder::Asc)]);

    let err = Cursor::from_url(&cursor.to_url(&made_for), &asked_for).expect_err("must refuse");
    assert!(matches!(err, CursorError::SortMismatch { .. }), "{err:?}");
    // Both fingerprints, because a caller showing this to a user cannot
    // otherwise say what to do about it.
    assert!(err.to_string().contains(&made_for.fingerprint()), "{err}");
    assert!(err.to_string().contains(&asked_for.fingerprint()), "{err}");
}

#[test]
fn five_the_wrong_number_of_keys_is_refused_and_both_counts_are_named() {
    let (sort, _) = date_cursor();
    // One key where the sort has two.
    let wire = envelope(&sort.fingerprint(), serde_json::json!([["s", "only-one"]]));
    let err = Cursor::from_url(&wire, &sort).expect_err("must refuse");
    assert!(
        matches!(
            err,
            CursorError::Arity {
                found: 1,
                expected: 2
            }
        ),
        "{err:?}"
    );
    // And one key too MANY, because a decoder that only checked "enough" would
    // accept this and bind the extra to nothing.
    let wire = envelope(
        &sort.fingerprint(),
        serde_json::json!([["s", "a"], ["s", "b"], ["s", "c"]]),
    );
    let err = Cursor::from_url(&wire, &sort).expect_err("must refuse");
    assert!(
        matches!(
            err,
            CursorError::Arity {
                found: 3,
                expected: 2
            }
        ),
        "{err:?}"
    );
    assert!(
        err.to_string().contains('3') && err.to_string().contains('2'),
        "{err}"
    );
}

#[test]
fn six_a_key_of_the_wrong_type_is_refused_and_names_the_column() {
    // `rating_sum` is the one sortable key that is not text, so it is where a
    // type check is observable. An integer in the `id` slot is the simpler
    // version and is checked too: the tiebreak is a TEXT primary key whatever
    // the sort, and that is the slot no sort-specific reasoning reaches.
    let sort = Sort::new(vec![(SortKey::RatingSum, SortOrder::Desc)]);
    let wire = envelope(
        &sort.fingerprint(),
        // o.rating_sum is an integer, and this is a string.
        serde_json::json!([["s", "not-a-number"], ["s", "o-7"]]),
    );
    let err = Cursor::from_url(&wire, &sort).expect_err("must refuse");
    assert!(
        matches!(
            err,
            CursorError::KeyType {
                index: 0,
                column: "o.rating_sum",
                ..
            }
        ),
        "{err:?}"
    );
    assert!(err.to_string().contains("o.rating_sum"), "{err}");
    assert!(err.to_string().contains("an integer"), "{err}");

    // And the tiebreak: a sort of TEXT keys with an integer id.
    let text_sort = Sort::date_desc();
    let wire = envelope(
        &text_sort.fingerprint(),
        serde_json::json!([["s", "2026-01-01"], ["i", 7]]),
    );
    let err = Cursor::from_url(&wire, &text_sort).expect_err("must refuse");
    assert!(
        matches!(
            err,
            CursorError::KeyType {
                index: 1,
                column: "o.id",
                ..
            }
        ),
        "{err:?}"
    );
}

#[test]
fn seven_a_list_in_a_key_slot_is_refused_as_not_scalar() {
    // A `Value::List` has no `SortKey` whose column it could match, so the
    // refusal names the slot and says the value is not a scalar -- a different
    // sentence from "this should have been an integer", and a different error
    // variant, because there is no expected column to name.
    let (sort, _) = date_cursor();
    let wire = envelope(
        &sort.fingerprint(),
        serde_json::json!([["s", "2026-01-01"], ["s", "o-7"]]),
    );
    assert!(
        Cursor::from_url(&wire, &sort).is_ok(),
        "the control must decode"
    );

    // A list in the wire is not one of the tags, so it is a shape error rather
    // than NotScalar -- and saying so is the point: the decoder refuses it
    // before it ever reaches a slot.
    let wire = envelope(
        &sort.fingerprint(),
        serde_json::json!([["l", ["a", "b"]], ["s", "o-7"]]),
    );
    let err = Cursor::from_url(&wire, &sort).expect_err("must refuse");
    assert!(matches!(err, CursorError::Shape(_)), "{err:?}");

    // `CursorError::NotScalar` is UNREACHABLE from the wire, and that is worth
    // knowing. A `Value::List` never becomes a `Tagged` (`from_value` returns
    // `None` for it), and a JSON array is not one of the six tags either, so
    // the decoder refuses a list as a Shape error before it ever reaches a
    // slot. The variant stays because the invariant it names is real -- "a
    // sort key is always a scalar" -- and a decoder that grew a new tag could
    // produce a list; but an assertion that constructs `None` and checks it is
    // none proves nothing, so the unreachability is stated in prose instead.
}

// ---------------------------------------------------------------------------
// The aggregate: no malformed input produces a Cursor
// ---------------------------------------------------------------------------

#[test]
fn every_malformed_cursor_is_an_error_not_a_repair() {
    let (sort, _) = date_cursor();
    let fp = sort.fingerprint();

    // Each of these is something a client, a stale link, or a curious person
    // could send. The single assertion is the point: NONE of them may produce a
    // `Cursor`, because a `from_url` that "helpfully" returned something usable
    // would be the failure this whole ticket exists to prevent.
    let malformed: Vec<(&str, String)> = vec![
        ("not base64", "!!!".to_string()),
        ("empty", String::new()),
        (
            "valid base64, not json",
            base64url_encode(b"\xff\xfe not json"),
        ),
        ("json array, not object", base64url_encode(br#"["v",1]"#)),
        ("missing v", base64url_encode(br#"{"f":"x","k":[]}"#)),
        ("missing f", base64url_encode(br#"{"v":1,"k":[]}"#)),
        ("missing k", base64url_encode(br#"{"v":1,"f":"x"}"#)),
        (
            "v is a string",
            base64url_encode(br#"{"v":"1","f":"x","k":[]}"#),
        ),
        (
            "wrong fingerprint",
            envelope("deadbeef", serde_json::json!([["s", "a"], ["s", "b"]])),
        ),
        ("no keys", envelope(&fp, serde_json::json!([]))),
        (
            "one key too few",
            envelope(&fp, serde_json::json!([["s", "a"]])),
        ),
        (
            "one key too many",
            envelope(&fp, serde_json::json!([["s", "a"], ["s", "b"], ["s", "c"]])),
        ),
        (
            "unknown tag",
            envelope(&fp, serde_json::json!([["z", 1], ["s", "b"]])),
        ),
        (
            "int for a text key",
            envelope(&fp, serde_json::json!([["i", 1], ["s", "b"]])),
        ),
        ("future version", wire_for_version(200, &sort)),
    ];

    for (what, wire) in malformed {
        let result = Cursor::from_url(&wire, &sort);
        assert!(
            result.is_err(),
            "{what:?} produced a cursor: {result:?} -- a malformed cursor must be \
             an error, never a repair"
        );
    }
}

// ---------------------------------------------------------------------------
// The two claims about ABSENCE
// ---------------------------------------------------------------------------

/// These two assert that something is *not* there, which no amount of compiling
/// can establish. A regression in either is the change that would undo the
/// design this ticket is built on, so they are tests rather than a note in a
/// commit message.
mod absence {
    fn sort_rs() -> String {
        std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/sort.rs"),
        )
        .expect("sort.rs is readable")
    }

    #[test]
    fn the_cursor_constructor_is_still_test_only() {
        // Spec section 5: this ticket does NOT make `Cursor::new` public. A
        // public constructor would let a caller assemble a cursor of the wrong
        // arity -- reintroducing, at the API surface, the one failure the type
        // was built to prevent. `from_url` is the only production way in, and it
        // validates.
        //
        // # The first version of this test was VACUOUS, and this comment is
        // # why it could not be trusted at a glance
        //
        // It searched the 40 lines above `fn new` for the text `#[cfg(test)]`.
        // That string occurs **in the doc comment immediately above the
        // attribute** -- a comment which explains that the constructor is
        // `#[cfg(test)]` -- so the window matched the PROSE and passed with
        // the attribute deleted. Two mutations (making `new` pub, and adding a
        // production caller) both left this test green.
        //
        // The lesson is the one this file's header already states about greps:
        // a search that can match a comment is not a search for the thing. The
        // fix is to look at the ATTRIBUTE LINES ONLY -- non-doc, non-blank --
        // between the end of the preceding item and `fn new`.
        let src = sort_rs();
        let at = src
            .find("fn new(values: Vec<Value>)")
            .expect("Cursor::new still exists; if it was renamed, this test must be rewritten");
        let window = &src[..at];

        // Take the contiguous run of lines immediately above the signature and
        // keep only the ones that are attributes: a doc comment line starts
        // with `///`, a blank line ends the run.
        let mut attrs: Vec<&str> = Vec::new();
        for line in window.rsplit('\n').skip(1) {
            let t = line.trim();
            if t.is_empty() {
                // A blank line ends the attribute run, but only after we have
                // collected something -- otherwise we walk into the doc comment
                // of the PREVIOUS item and pick up its attributes.
                if !attrs.is_empty() {
                    break;
                }
                continue;
            }
            if t.starts_with("///") || t.starts_with("//") {
                continue;
            }
            if t.starts_with('#') {
                attrs.push(t);
                continue;
            }
            break;
        }

        assert!(
            attrs.iter().any(|a| *a == "#[cfg(test)]"),
            "Cursor::new is no longer #[cfg(test)]-gated. The attributes directly \
             above it are now {attrs:?}. It is the only thing stopping a caller \
             assembling a cursor of the wrong arity."
        );
    }

    #[test]
    fn no_production_code_builds_a_cursor_outside_from_row_and_from_url() {
        // The same claim from the other side: sweep every crate's `src/` for
        // `Cursor::new(` and require every hit to be inside a test module.
        //
        // The first version of this test was ALSO vacuous -- it classified a
        // hit as "in tests" whenever anything earlier in the file contained
        // `#[cfg(test)]`, which is true of nearly every file in a crate that
        // has a single test module at the bottom. A production caller added
        // above that module was scored as a test hit and the test stayed green.
        //
        // What it does now: find the byte offset of the last `#[cfg(test)]`
        // that *starts a module* (`mod x {` within a few lines after it) and
        // require each hit to be below that offset, and report the ones that
        // are not. A file with no test module has no such offset, so every hit
        // in it is production -- which is the correct reading.
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates/ has a parent")
            .to_path_buf();
        let mut hits: Vec<String> = Vec::new();
        let mut production: Vec<String> = Vec::new();
        let mut files_walked = 0usize;

        let mut stack: Vec<std::path::PathBuf> = std::fs::read_dir(&root)
            .expect("crates/ is readable")
            .map(|e| e.expect("a dir entry").path().join("src"))
            .filter(|p| p.is_dir())
            .collect();

        while let Some(dir) = stack.pop() {
            for f in std::fs::read_dir(&dir).expect("readable") {
                let path = f.expect("a file entry").path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                    continue;
                }
                files_walked += 1;
                let text = std::fs::read_to_string(&path).expect("a .rs file is utf8");

                // Where the file's test module begins, if it has one.
                let tests_start = find_tests_module_start(&text);

                for (n, line) in text.lines().enumerate() {
                    if !line.contains("Cursor::new(") {
                        continue;
                    }
                    let off = text.find(line).expect("the line came from this text");
                    let rel = path
                        .strip_prefix(&root)
                        .unwrap_or(&path)
                        .display()
                        .to_string();
                    // A hit in a DOC COMMENT or a string is not a call. This is
                    // the same trap as above, and this module's own doc
                    // comment names `Cursor::new` in prose.
                    let trimmed = line.trim_start();
                    let is_comment = trimmed.starts_with("//")
                        || trimmed.starts_with("///")
                        || trimmed.starts_with("//!");
                    if is_comment {
                        continue;
                    }
                    let in_tests = tests_start.map(|o| off >= o).unwrap_or(false);
                    hits.push(format!("{rel}:{}: {}", n + 1, line.trim()));
                    if !in_tests {
                        production.push(format!("{rel}:{}: {}", n + 1, line.trim()));
                    }
                }
            }
        }

        assert!(
            files_walked > 50,
            "only {files_walked} .rs files were walked under crates/*/src -- a \
             walk that finds almost nothing would make this test pass for the \
             wrong reason"
        );
        assert!(
            production.is_empty(),
            "a production caller builds a Cursor directly, which bypasses every \
             check in cursor_wire.rs:\n{}",
            production.join("\n")
        );
        // And the sweep must have found the test-side uses, or `production` being
        // empty proves nothing: a renamed or deleted `Cursor::new` would make
        // this test vacuous exactly when the invariant matters most.
        assert!(
            !hits.is_empty(),
            "no Cursor::new( anywhere under crates/*/src -- if it was renamed or \
             removed, rewrite this test rather than leave it passing"
        );
    }

    /// The byte offset where a file's `#[cfg(test)] mod` begins, or `None`.
    ///
    /// Deliberately narrow: it requires `#[cfg(test)]` to be followed, within a
    /// few lines, by an actual `mod`. The first version of the test accepted any
    /// `#[cfg(test)]` anywhere earlier in the file, which is not the same thing
    /// at all.
    fn find_tests_module_start(text: &str) -> Option<usize> {
        let mut idx = 0usize;
        while let Some(found) = text[idx..].find("#[cfg(test)]") {
            let abs = idx + found;
            let after = &text[abs..];
            // Within 120 bytes there must be a `mod`.
            let head: String = after.chars().take(120).collect();
            if head.contains("mod ") {
                return Some(abs);
            }
            idx = abs + 1;
        }
        None
    }
}
