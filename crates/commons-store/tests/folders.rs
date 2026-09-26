//! Folders: resolution, cycles, ordering, breadcrumbs.
//!
//! T-P5-006 item 6. Spec §5.14, §9.5; plan §T-P5-006 item 6. The design and
//! its reasons are in `docs/spec/t-p5-006-folders.md`; this file checks that the
//! code does what that document claims.
//!
//! # What is worth testing here
//!
//! Resolution is a tree rewrite, and a tree rewrite is easy to get right on the
//! happy path and wrong in three specific ways: a cycle that recurses forever, a
//! diamond that is mistaken for a cycle, and a reference that leaks into the
//! output instead of being replaced. Each has a test below, and each is written
//! as the *claim* rather than the mechanism, so a future rewrite that breaks the
//! claim fails the test rather than passing a structural check.
//!
//! The test source is a literal `HashMap` rather than a database. That is
//! deliberate: `FolderSource` is a trait for exactly this reason, and a test that
//! needed a database to check a rewrite would be a test nobody runs.

use std::collections::HashMap;

use commons_core::{ObjectKind, Role};
use commons_store::filter_ast::{BuiltinField, CallerId, CmpOp, Engine, FieldRef, Filter, Value};
use commons_store::folders::*;

/// A `FolderSource` over a literal map. Fails loudly on an id it does not know,
/// so a test that adds a `Saved` reference without adding the folder gets an
/// error naming the id rather than a silently empty result.
struct MapSource(HashMap<String, Filter>);

impl MapSource {
    fn new(pairs: Vec<(&str, Filter)>) -> Self {
        Self(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }
}

impl FolderSource for MapSource {
    fn filter_of(&self, id: &str) -> Result<Option<Filter>, FolderError> {
        Ok(self.0.get(id).cloned())
    }
}

/// `tagged <name>`, the filter used as a stand-in for anything real.
fn tagged(name: &str) -> Filter {
    Filter::Facet {
        kind: None,
        field: FieldRef::Builtin(BuiltinField::Tag),
        op: CmpOp::In,
        values: vec![Value::Str(name.to_string())],
    }
}

fn saved(id: &str) -> Filter {
    Filter::Saved { id: id.to_string() }
}

/// The caller used only for compilation tests.
fn caller() -> CallerId {
    CallerId::anonymous()
}

#[test]
fn a_filter_with_no_saved_reference_is_untouched() {
    let src = MapSource::new(vec![]);
    let f = tagged("beach");
    let (out, n) = resolve_saved(&f, &src, &mut std::collections::HashSet::new()).unwrap();
    assert_eq!(
        out, f,
        "a filter with no reference must come back identical"
    );
    assert_eq!(n.expanded, 0);
    assert_eq!(n.cut, 0);
    assert!(!n.dropped_anything(), "nothing was dropped");
}

#[test]
fn a_reference_is_replaced_by_the_folder_own_filter() {
    // The core claim of §5.14: a folder is a saved query, so opening one is
    // running its query. The substitution happens in place, so the user's other
    // terms keep their meaning rather than being conjoined with a whole extra
    // filter.
    let src = MapSource::new(vec![("beach", tagged("summer"))]);
    let f = Filter::And(vec![saved("beach"), tagged("2024")]);
    let (out, n) = resolve_saved(&f, &src, &mut std::collections::HashSet::new()).unwrap();
    assert_eq!(n.expanded, 1);
    assert_eq!(n.cut, 0);
    assert_eq!(
        out,
        Filter::And(vec![tagged("summer"), tagged("2024")]),
        "the reference is replaced where it stood, not appended"
    );
    // And critically: no `Saved` node survives, because a survivor would reach
    // the compiler and now fail there.
    assert!(
        !format!("{out:?}").contains("Saved"),
        "resolution must leave no Saved node: {out:?}"
    );
}

#[test]
fn a_nested_reference_expands_to_a_full_tree() {
    let src = MapSource::new(vec![
        ("outer", Filter::And(vec![saved("inner"), tagged("a")])),
        ("inner", tagged("b")),
    ]);
    let f = saved("outer");
    let (out, n) = resolve_saved(&f, &src, &mut std::collections::HashSet::new()).unwrap();
    assert_eq!(n.expanded, 2, "both references were expanded");
    assert_eq!(n.cut, 0);
    assert_eq!(out, Filter::And(vec![tagged("b"), tagged("a")]));
}

#[test]
fn a_diamond_is_not_mistaken_for_a_cycle() {
    // Two folders both referencing a third. A visited-set implementation that
    // does not pop on the way out reports a false cycle here, and a diamond is
    // completely ordinary -- it is what happens when a user makes two folders
    // that both start from the same saved one.
    let src = MapSource::new(vec![
        ("a", saved("shared")),
        ("b", saved("shared")),
        ("shared", tagged("s")),
    ]);
    let f = Filter::Or(vec![saved("a"), saved("b")]);
    let (out, n) = resolve_saved(&f, &src, &mut std::collections::HashSet::new()).unwrap();
    assert_eq!(
        n.cut, 0,
        "a diamond is not a cycle: nothing was dropped: {n:?}"
    );
    // Four, not three: `shared` is expanded once under each branch, because
    // each branch genuinely needs the filter and sharing a subtree between them
    // would mean sharing mutable structure. The count is references expanded,
    // not distinct folders visited.
    assert_eq!(
        n.expanded, 4,
        "two branches, each expanding `shared`: {n:?}"
    );
    assert_eq!(
        out,
        Filter::Or(vec![tagged("s"), tagged("s")]),
        "and neither branch became 'match nothing', which is what a \
         false-positive cycle would have produced"
    );
}

#[test]
fn a_direct_cycle_resolves_to_matching_nothing() {
    // Reachable by editing a folder's filter to name itself. The enclosing view
    // must still open: a folder that contributes no members is a state a user
    // can see, whereas an error here takes down every query that referenced it.
    let src = MapSource::new(vec![("loop", saved("loop"))]);
    let (out, n) =
        resolve_saved(&saved("loop"), &src, &mut std::collections::HashSet::new()).unwrap();
    assert_eq!(n.cut, 1, "exactly one reference was cut: {n:?}");
    assert_eq!(
        n.expanded, 1,
        "one expansion -- the reference to `loop` was looked up and found, \
         but its filter is the same reference, so the inner one was cut"
    );
    assert_eq!(
        out,
        Filter::Or(vec![]),
        "a self-reference expands to 'match nothing' -- Or([]), which \
         compiles to 1 = 0"
    );
    assert!(n.dropped_anything(), "and the caller can be told so");
}

#[test]
fn a_mutual_cycle_terminates() {
    let src = MapSource::new(vec![("a", saved("b")), ("b", saved("a"))]);
    let (out, n) = resolve_saved(&saved("a"), &src, &mut std::collections::HashSet::new()).unwrap();
    assert_eq!(
        n.cut, 1,
        "`a` expanded into `b`, `b` expanded into `a`, and `a` was already on \
         the path: {n:?}"
    );
    assert_eq!(out, Filter::Or(vec![]), "and it matches nothing");
}

#[test]
fn a_deep_chain_beyond_the_limit_is_an_error_not_a_hang() {
    // The depth limit is a backstop for a chain the visited-set cannot see,
    // which happens when a filter is edited to point at a folder whose own
    // filter was already expanded. This cannot be built by hand from a literal
    // map, so it is checked by driving the limit directly: a chain of
    // `MAX_FOLDER_DEPTH` distinct folders is fine, and the guard is one past it.
    let mut pairs: Vec<(String, Filter)> = Vec::new();
    for i in 0..=MAX_FOLDER_DEPTH as usize {
        pairs.push((format!("f{i}"), saved(&format!("f{}", i + 1))));
    }
    // The terminus has to exist, or the failure is `NotFound` and the test
    // passes for the wrong reason -- which is precisely what the first version
    // of this test did.
    pairs.push((format!("f{}", MAX_FOLDER_DEPTH as usize + 1), tagged("end")));
    let map: HashMap<String, Filter> = pairs.into_iter().collect();
    let src = MapSource(map);
    let f = saved("f0");
    match resolve_saved(&f, &src, &mut std::collections::HashSet::new()) {
        Ok((out, _)) => panic!("a chain past the limit must not resolve: {out:?}"),
        Err(FolderError::TooDeep) => {}
        Err(other) => panic!("wrong error: {other:?}"),
    }
}

#[test]
fn a_missing_folder_is_reported_by_id() {
    // A message naming the id, rather than a parse error about a filter string
    // that was never the problem.
    let src = MapSource::new(vec![]);
    let err = resolve_saved(&saved("nope"), &src, &mut std::collections::HashSet::new())
        .expect_err("a missing folder must not resolve to 'match nothing'");
    assert_eq!(
        err,
        FolderError::NotFound("nope".to_string()),
        "a missing folder is not the same as an empty one: a typo in a URL \
         should say so rather than showing a view with no results"
    );
}

#[test]
fn an_empty_and_compiles_to_match_everything_which_is_the_dangerous_direction() {
    // This is the one that has to be asserted rather than assumed. `And([])`
    // compiling to `1 = 1` is correct for a filter a user wrote, and *wrong*
    // for a cycle we introduced -- a cycle that resolved to "match everything"
    // would turn a broken folder into a query returning the whole library.
    //
    // So the cycle case must NOT rely on `And([])`. It is tested here so the
    // property is on the record: the safety of a cyclic folder depends on the
    // resolver returning something that compiles to false, and `And([])` is the
    // wrong thing.
    let f = Filter::And(Vec::new());
    let sql = f.to_sql(Engine::Sqlite, &caller()).unwrap();
    assert_eq!(
        sql.sql.trim(),
        "(1 = 1)",
        "confirmed: And([]) is match-everything, so it is NOT the cycle answer"
    );

    // The correct answer for a cycle is `Or([])`, which compiles to false.
    let none = Filter::Or(Vec::new());
    let sql = none.to_sql(Engine::Sqlite, &caller()).unwrap();
    assert_eq!(
        sql.sql.trim(),
        "(1 = 0)",
        "Or([]) is 'match nothing', which is what a cycle must become"
    );
}

#[test]
fn a_cycle_resolves_to_or_empty_rather_than_and_empty() {
    // The corrected rule, and the test that would have caught the first
    // implementation. A cycle returned `And([])`, which compiles to `1 = 1` --
    // so a folder whose filter named itself returned the *entire library*
    // instead of nothing. The unit tests for resolution passed, because
    // `And([])` is what they expected; nothing checked what it compiles to.
    let src = MapSource::new(vec![("loop", saved("loop"))]);
    let (out, _) =
        resolve_saved(&saved("loop"), &src, &mut std::collections::HashSet::new()).unwrap();
    assert_eq!(
        out,
        Filter::Or(vec![]),
        "a cycle must compile to 'match nothing', never to 'match everything'"
    );
    let sql = out.to_sql(Engine::Sqlite, &caller()).unwrap();
    assert!(
        sql.sql.contains("1 = 0"),
        "and the SQL must actually exclude: {}",
        sql.sql
    );
}

#[test]
fn an_unresolved_reference_fails_at_the_compiler_with_a_named_error() {
    // The regression this file exists for. `Filter::Saved` compiled to
    // `o.saved_filter_ids LIKE ?` -- a column no migration creates -- so any
    // query with a saved reference failed at the database with "no such column".
    // The only test touching it asserted the serde shape and never ran the SQL.
    let err = saved("beach")
        .to_sql(Engine::Sqlite, &caller())
        .unwrap_err();
    assert_eq!(
        err,
        commons_store::filter_ast::FilterError::UnresolvedSavedReference,
        "an unresolved reference must fail loudly here, not emit SQL for a \
         column that does not exist"
    );
    assert!(
        err.to_string().contains("resolve_saved"),
        "the message must name the fix: {err}"
    );
}

#[test]
fn resolution_output_compiles_cleanly() {
    // The end-to-end property: after resolution there is nothing the compiler
    // will reject, and every value is bound rather than interpolated.
    let src = MapSource::new(vec![
        ("beach", tagged("summer")),
        ("people", tagged("portrait")),
    ]);
    let f = Filter::And(vec![
        saved("beach"),
        Filter::Or(vec![saved("people"), Filter::Not(Box::new(tagged("x")))]),
        Filter::Text { q: "100%".into() },
    ]);
    let (out, _) = resolve_saved(&f, &src, &mut std::collections::HashSet::new()).unwrap();
    let sql = out.to_sql(Engine::Sqlite, &caller()).unwrap();
    assert!(!sql.sql.contains("Saved"));
    // The wildcard must be in the bound value, never in the SQL text.
    assert!(!sql.sql.contains("100%"), "interpolated: {}", sql.sql);
    assert!(
        sql.params
            .iter()
            .any(|v| matches!(v, Value::Str(s) if s.contains("100"))),
        "the query is a bound parameter, escaped and wrapped: {:?}",
        sql.params
    );
    // The LIKE metacharacters are escaped in the *value*, which is what stops
    // a search for "100%" from becoming a wildcard.
    assert!(
        sql.params
            .iter()
            .all(|v| !matches!(v, Value::Str(s) if s.contains("100%"))),
        "the percent must be escaped in the bound value: {:?}",
        sql.params
    );
}

#[test]
fn sibling_order_is_by_position_then_id() {
    // Total order. A listing with no defined order between two equal positions
    // can repeat or skip a row across a page boundary, which is a bug that
    // looks like a missing object rather than like a sort.
    assert!(order_by_position_sql().contains("f.position ASC"));
    assert!(order_by_position_sql().contains("f.id ASC"));
    // And not by name: a tree that re-sorts when something is renamed is
    // re-ordering under the user's cursor.
    assert!(!order_by_position_sql().contains("f.name"));
}

#[test]
fn the_root_predicate_is_written_down_rather_than_remembered() {
    // A missing root clause does not return the wrong rows; it returns every
    // row. So the roots case lives here as a named constant.
    assert_eq!(parent_predicate_sql(), "f.parent_id IS NULL");
}

#[test]
fn breadcrumbs_run_from_a_folder_to_its_root() {
    let by_id: HashMap<String, Folder> = [
        ("c", folder("c", "Child", Some("b"), 0)),
        ("b", folder("b", "Mid", Some("a"), 0)),
        ("a", folder("a", "Root", None, 0)),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    let child = by_id["c"].clone();
    let trail = ancestry(&child, &|id: &str| by_id.get(id).cloned());
    let names: Vec<&str> = trail
        .iter()
        .map(|f| match f {
            FolderRef::Expanded(f) => f.name.as_str(),
            FolderRef::Empty => "<missing>",
        })
        .collect();
    assert_eq!(names, vec!["Child", "Mid", "Root"], "nearest first");
}

#[test]
fn a_breadcrumb_stops_at_a_deleted_ancestor_rather_than_failing() {
    let by_id: HashMap<String, Folder> = [("c", folder("c", "Child", Some("gone"), 0))]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    let child = by_id["c"].clone();
    let trail = ancestry(&child, &|id: &str| by_id.get(id).cloned());
    assert_eq!(trail.len(), 2);
    assert_eq!(trail[1], FolderRef::Empty, "the gap is shown, not hidden");
}

#[test]
fn a_cycle_in_the_tree_terminates_the_breadcrumb() {
    // A parent_id cycle is *not* something the resolver can see: resolution
    // walks filters, not parents. It is reachable by dragging a folder into its
    // own descendant, so `ancestry` needs its own guard.
    let by_id: HashMap<String, Folder> = [
        ("a", folder("a", "A", Some("b"), 0)),
        ("b", folder("b", "B", Some("a"), 0)),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    let start = by_id["a"].clone();
    let trail = ancestry(&start, &|id: &str| by_id.get(id).cloned());
    assert!(
        trail.len() <= MAX_FOLDER_DEPTH as usize + 1,
        "a parent cycle must not loop: got {}",
        trail.len()
    );
}

#[test]
fn a_folder_is_never_null_filtered() {
    // `filter` is not `Option<Filter>`. A NULL reaching the query would mean
    // "no constraint", which is `1 = 1` -- a folder that silently matches the
    // whole library.
    let f = folder("x", "X", None, 0);
    assert!(
        matches!(f.filter, Filter::All),
        "the default is 'match everything before the consent conjunction', \
         which is explicit rather than a NULL"
    );
    let sql = f.filter.to_sql(Engine::Sqlite, &caller()).unwrap();
    assert!(sql.sql.contains("1 = 1"), "so it is visible in the SQL");
}

#[test]
fn notify_is_opt_in() {
    // A subscription that defaulted on would be a background job per folder for
    // every user who ever made one.
    assert!(!folder("x", "X", None, 0).notify);
}

#[test]
fn a_role_that_may_curate_may_write_a_folder() {
    // Curation, not moderation. A Contributor curates -- they can propose and
    // curate -- and under a `may_see_restricted` gate they could not make a
    // folder, with the button simply hidden and nothing saying why.
    assert!(may_write(Role::Contributor));
    assert!(may_write(Role::Steward));
    assert!(may_write(Role::Admin));
    // A Subscriber may vote, not curate. `may_see_restricted` is also false
    // here, so this case does not distinguish the two gates -- the Contributor
    // case above is the one that does.
    assert!(!may_write(Role::Subscriber));
    assert!(!may_write(Role::Public));
}

#[test]
fn kind_is_preserved_through_resolution() {
    // A facet constrained to one object kind must stay constrained. Resolution
    // rewrites structure, and a rewrite that dropped `kind` would silently widen
    // a query from "scenes tagged X" to "anything tagged X".
    let src = MapSource::new(vec![("f", tagged("a"))]);
    let f = Filter::Facet {
        kind: Some(ObjectKind::Scene),
        field: FieldRef::Builtin(BuiltinField::Tag),
        op: CmpOp::In,
        values: vec![Value::Str("x".into())],
    };
    let (out, _) = resolve_saved(&f, &src, &mut std::collections::HashSet::new()).unwrap();
    match out {
        Filter::Facet { kind, .. } => assert_eq!(kind, Some(ObjectKind::Scene)),
        other => panic!("structure changed: {other:?}"),
    }
}

fn folder(id: &str, name: &str, parent: Option<&str>, position: i32) -> Folder {
    Folder {
        id: id.to_string(),
        name: name.to_string(),
        filter: Filter::All,
        parent_id: parent.map(str::to_string),
        position,
        icon: None,
        notify: false,
    }
}
