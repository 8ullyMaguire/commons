//! Serde shape invariants (T-P0-004).
//!
//! These are not style rules. Each one encodes a failure that actually
//! happened while building this crate and that fails only at runtime, or only
//! in a code path no test exercised:
//!
//!   * An internally-tagged enum whose variant payload is a bare `String`,
//!     `f64`, `bool` or integer cannot be serialized at all. serde rejects it
//!     with "cannot serialize tagged newtype variant", so the derive compiles,
//!     the type looks fine, and the first shareable URL silently fails.
//!   * Two mutually recursive, internally-tagged types overflow the trait
//!     solver (E0275) on rustc 1.98. `Filter` holds `Vec<Value>` and `Value` is
//!     recursive; with both internally tagged the build does not terminate,
//!     regardless of `recursion_limit`.
//!
//! Both are checked structurally so a future enum cannot reintroduce them.

use std::fs;
use std::path::{Path, PathBuf};

/// The workspace root. `CARGO_MANIFEST_DIR` is `crates/commons-store`, so two
/// levels up is the repo root -- one level up is `crates/`.
fn crate_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("manifest dir has at least two ancestors")
        .to_path_buf()
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else if p.extension().is_some_and(|e| e == "rs") {
            out.push(p);
        }
    }
}

fn rust_sources() -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(&crate_root().join("crates"), &mut out);
    out
}

/// Strip `//` comments so prose naming a type is not parsed as code. Doc
/// comments (`///`) are kept: they cannot contain an item definition.
fn strip_comments(src: &str) -> String {
    src.lines()
        .filter(|l| {
            let t = l.trim_start();
            !(t.starts_with("//") && !t.starts_with("///"))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Given a position just after a `#[serde(...)]` attribute, find the enum that
/// attribute applies to and return its name and body. Handles a `#[derive]`
/// between the two, in either order.
fn enum_after(src: &str, mut pos: usize) -> Option<(String, String)> {
    // Skip whitespace and any further attributes.
    loop {
        let rest = &src[pos..];
        let trimmed = rest.trim_start();
        let skipped = rest.len() - trimmed.len();
        if trimmed.starts_with("#[") {
            let end = trimmed.find(")]")? + 2;
            pos += skipped + end;
            continue;
        }
        pos += skipped;
        break;
    }

    let rest = &src[pos..];
    let open = rest.find('{')?;
    let head = &rest[..open];
    let name = head
        .lines()
        .rev()
        .find_map(|l| {
            let t = l.trim();
            t.strip_prefix("pub enum ")
                .or_else(|| t.strip_prefix("enum "))
        })
        .map(|n| n.split_whitespace().next().unwrap_or("").to_string())?;

    let body_start = pos + open;
    let bytes = src.as_bytes();
    let mut depth = 0usize;
    let mut i = body_start;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some((name, src[body_start..i].to_string()));
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

const PRIMITIVES: [&str; 9] = [
    "String", "&str", "f64", "f32", "i64", "i32", "u64", "u32", "bool",
];

fn variants_with_primitive_payload(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in body.lines() {
        let t = line.trim();
        let Some(open) = t.find('(') else { continue };
        let head = t[..open].trim();
        let Some(name) = head.split_whitespace().next_back() else {
            continue;
        };
        if !name.starts_with(|c: char| c.is_uppercase() || c == '_') {
            continue;
        }
        let Some(rel_close) = t[open..].find(')') else {
            continue;
        };
        let payload = t[open + 1..open + rel_close]
            .trim()
            .trim_end_matches(',')
            .trim();
        if PRIMITIVES.contains(&payload) {
            out.push(format!("{name}({payload})"));
        }
    }
    out
}

#[test]
fn no_internally_tagged_enum_carries_a_bare_primitive() {
    let mut violations = Vec::new();

    for path in rust_sources() {
        let raw = fs::read_to_string(&path).unwrap();
        let src = strip_comments(&raw);
        let rel = path
            .strip_prefix(crate_root())
            .unwrap_or(&path)
            .display()
            .to_string();

        let mut idx = 0usize;
        while let Some(found) = src[idx..].find("#[serde(") {
            let at = idx + found;
            let Some(c) = src[at..].find(")]") else {
                break;
            };
            let close = at + c + 2;
            if src[at..close].contains("tag = ") {
                if let Some((name, body)) = enum_after(&src, close) {
                    for v in variants_with_primitive_payload(&body) {
                        violations.push(format!("{rel}: {name}::{v}"));
                    }
                }
            }
            idx = close;
        }
    }

    assert!(
        violations.is_empty(),
        "these internally-tagged variants cannot be serialized; switch the enum to \
         externally tagged:\n  {}",
        violations.join("\n  ")
    );
}

#[test]
fn filter_value_and_fieldref_are_externally_tagged() {
    // Named explicitly because these three are the combination that overflows
    // the trait solver when internally tagged. A blanket "no internal tags"
    // rule would be wrong: internal tagging is fine and more compact when every
    // payload is a struct or a map.
    // crate_root() is already the workspace root.
    let path = crate_root().join("crates/commons-store/src/filter_ast.rs");
    let src = strip_comments(&fs::read_to_string(&path).expect("filter_ast.rs"));

    for name in ["Filter", "Value", "FieldRef"] {
        let needle = format!("enum {name} ");
        let at = src
            .find(&needle)
            .unwrap_or_else(|| panic!("enum {name} not found in filter_ast.rs"));
        let before = &src[..at];
        let attr_start = before
            .rfind("#[serde(")
            .unwrap_or_else(|| panic!("{name} has no serde attribute"));
        let attr_end = before[attr_start..]
            .find(")]")
            .map(|i| attr_start + i + 2)
            .expect("malformed serde attribute");
        let attr = &src[attr_start..attr_end];
        assert!(
            !attr.contains("tag = "),
            "{name} is internally tagged, which overflows the trait solver in \
             combination with the other recursive types in this module. Use \
             external tagging."
        );
    }
}

#[test]
fn round_tripping_a_shared_filter_is_lossless() {
    // The failure this guards against was silent: `to_url` once fell back to
    // `Filter::All` on a serialization error, so a broken link produced a URL
    // that decoded to "match everything". Build a filter that exercises every
    // variant and assert the round trip is exact.
    use commons_store::{BuiltinField, CallerId, CmpOp, Engine, FieldRef, Filter, Value};

    let filter = Filter::And(vec![
        Filter::And(vec![]),
        Filter::Or(vec![Filter::Not(Box::new(Filter::Text {
            q: "free text".into(),
        }))]),
        Filter::Facet {
            kind: Some(commons_core::ObjectKind::Scene),
            field: FieldRef::Builtin(BuiltinField::Rating),
            op: CmpOp::Between,
            values: vec![Value::Float(1.0), Value::Float(5.0)],
        },
        Filter::Facet {
            kind: None,
            field: FieldRef::Custom("my_field".into()),
            op: CmpOp::In,
            values: vec![
                Value::Str("a".into()),
                Value::Int(2),
                Value::Float(2.5),
                Value::Bool(true),
                Value::Null,
                Value::List(vec![Value::Str("nested".into())]),
            ],
        },
        Filter::Saved {
            id: "saved-1".into(),
        },
        Filter::All,
    ]);

    let url = filter.to_url();
    let back = Filter::from_url(&url).expect("a URL we just produced must decode");
    assert_eq!(filter, back, "filter did not survive the URL round trip");

    // The URL round trip preserves `Filter::Saved`, and that is the extent of
    // what this test claims: a saved reference is a *reference*, not a query.
    //
    // Compiling is a different step, and the filter cannot be compiled as it
    // stands. A `Saved` reaching the compiler means the caller skipped
    // `folders::resolve_saved`, and the compiler says so
    // (`UnresolvedSavedReference`) rather than emitting SQL for it. It used to
    // emit `o.saved_filter_ids LIKE ?` -- a column no migration creates, so
    // every query containing a saved reference failed at the database with
    // "no such column". The error now names the missing step instead of a
    // column, which sends a developer to the right place instead of to the
    // schema.
    let err = filter
        .to_sql(Engine::Postgres, &CallerId::anonymous())
        .expect_err("an unresolved Saved reference must not compile");
    assert!(
        matches!(err, commons_store::FilterError::UnresolvedSavedReference),
        "expected UnresolvedSavedReference, got {err:?}"
    );
}

#[test]
fn a_saved_reference_resolves_to_its_definitions_filter() {
    // The other half of the contract: `Saved` is an indirection, and after
    // resolution the AST is ordinary SQL with no trace of the folder.
    use commons_store::filter_ast::{
        BuiltinField, CallerId, CmpOp, Engine, FieldRef, Filter, Value,
    };
    use commons_store::folders::{self, FolderError, FolderSource};
    use std::collections::HashMap;

    /// The store's own name for a folder that does not exist. Spelled out here
    /// rather than reusing a helper from `folders.rs` so the test exercises the
    /// trait as a caller outside the crate would.
    struct Defs(HashMap<String, Filter>);

    impl FolderSource for Defs {
        fn filter_of(&self, id: &str) -> Result<Option<Filter>, FolderError> {
            Ok(self.0.get(id).cloned())
        }
    }

    let defs = Defs(
        [(
            "saved-1".to_string(),
            Filter::Facet {
                kind: Some(commons_core::ObjectKind::Scene),
                field: FieldRef::Builtin(BuiltinField::Rating),
                op: CmpOp::Gte,
                values: vec![Value::Float(4.0)],
            },
        )]
        .into_iter()
        .collect(),
    );

    let unresolved = Filter::And(vec![
        Filter::Saved {
            id: "saved-1".into(),
        },
        Filter::All,
    ]);
    let (resolved, report) = folders::resolve_saved(&unresolved, &defs, &mut Default::default())
        .expect("a reference with a definition resolves");

    assert_eq!(report.expanded, 1, "exactly one reference was expanded");
    let sql = resolved
        .to_sql(Engine::Postgres, &CallerId::anonymous())
        .expect("a resolved filter compiles");
    assert!(
        !sql.sql.contains("saved-1"),
        "the folder id must not reach the SQL: {:?}",
        sql.sql
    );
    assert!(
        !sql.sql.contains("saved_filter_ids"),
        "no membership column: the query is the folder's own filter"
    );
}

#[test]
fn a_saved_reference_to_a_missing_folder_is_an_error_not_an_empty_result() {
    // The direction that matters. A missing folder that resolved to "match
    // nothing" would be a silent empty list, and a folder that resolved to
    // "match everything" would be worse: the user opens their "Highly rated"
    // folder and sees the entire library. Both are quiet. An error is not.
    use commons_store::filter_ast::Filter;
    use commons_store::folders::{self, FolderError, FolderSource};

    /// A source with no folders in it. A unit struct rather than one wrapping
    /// an empty map: the map would be a field nothing reads, and a source that
    /// answers "no" for everything does not need storage to say so.
    struct Empty;
    impl FolderSource for Empty {
        fn filter_of(&self, _id: &str) -> Result<Option<Filter>, FolderError> {
            Ok(None)
        }
    }

    let f = Filter::And(vec![Filter::Saved { id: "gone".into() }]);
    let err = folders::resolve_saved(&f, &Empty, &mut Default::default())
        .expect_err("a missing folder must not resolve");
    assert_eq!(err, FolderError::NotFound("gone".to_string()));
}
