//! The filter wire shape, printed rather than described. #1030, item 17.
//!
//! `ui/src/lib/api/media.ts` builds a `kind IN (...)` facet in TypeScript, and
//! this file is where that TypeScript's *assumption* is checked. The UI cannot
//! import the Rust enum, and serde's external tagging has three details that are
//! all invisible if you write the obvious thing:
//!
//!   - `field` is `{"builtin":"kind"}`, not `"kind"`;
//!   - a value is `{"str":"scene"}`, not `"scene"`;
//!   - `op` is `in`, not `In`.
//!
//! Get any one of them wrong and the server rejects the filter, or — worse —
//! accepts a filter that means something else. So the shape is emitted as data,
//! `ui/tests/media.test.ts` compares its hard-coded copy against this, and a
//! change to either side fails a test rather than a tab.
//!
//! The expected value lives in the TS test, so this side stays the authority and
//! does not need updating when the UI is refactored; a reviewer comparing the two
//! reads this file and the one assertion in the TS test.

use commons_store::filter_ast::{BuiltinField, CmpOp, FieldRef, Filter, Value};

/// The media tab's facet, exactly as the UI builds it.
fn media_facet() -> Filter {
    Filter::Facet {
        kind: None,
        field: FieldRef::Builtin(BuiltinField::Kind),
        op: CmpOp::In,
        values: vec![Value::Str("scene".into()), Value::Str("image".into())],
    }
}

#[test]
fn the_media_facet_is_the_shape_the_ui_builds() {
    let json = serde_json::to_string(&media_facet()).expect("serialisable");
    assert_eq!(
        json,
        r#"{"facet":{"kind":null,"field":{"builtin":"kind"},"op":"in","values":[{"str":"scene"},{"str":"image"}]}}"#,
        "the wire shape changed; ui/src/lib/api/media.ts kindFacet() and \
         ui/tests/media.test.ts both encode it, and both must change with it"
    );
}

#[test]
fn the_facet_parses_back_to_the_same_value() {
    let facet = media_facet();
    let json = serde_json::to_string(&facet).unwrap();
    let back: Filter = serde_json::from_str(&json).expect("round trips");
    assert_eq!(facet, back);
}

#[test]
fn a_conjunction_of_the_facet_and_a_text_filter() {
    // The shape `view.ts` describes: the filter is a JSON string, and `{"and":[…]}`
    // is how two filters combine. Kept as a test so a change to `Filter`'s serde
    // attributes is visible from the UI side.
    let both = Filter::And(vec![media_facet(), Filter::Text { q: "cat".into() }]);
    let json = serde_json::to_string(&both).unwrap();
    assert!(json.starts_with(r#"{"and":["#), "got {json}");
    let back: Filter = serde_json::from_str(&json).unwrap();
    assert_eq!(both, back);
}

#[test]
fn an_unknown_kind_is_a_string_the_server_also_accepts() {
    // The reason `parseKind` returns null rather than throwing: the database
    // column is unconstrained TEXT, so a kind this build has never heard of
    // reaches the server as an ordinary string, and the server's job is to
    // match it like any other value. Refusing to SEND it would hide rows that
    // are legitimately in the library.
    let odd = Filter::Facet {
        kind: None,
        field: FieldRef::Builtin(BuiltinField::Kind),
        op: CmpOp::In,
        values: vec![Value::Str("hologram".into())],
    };
    let json = serde_json::to_string(&odd).unwrap();
    let back: Filter = serde_json::from_str(&json).unwrap();
    assert_eq!(odd, back);
}
