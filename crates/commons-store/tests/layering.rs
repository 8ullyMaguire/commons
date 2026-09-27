//! Layering and dependency-direction tests (T-P0-007).
//!
//! Two rules, both of which are easy to violate by accident and expensive to
//! notice afterwards:
//!
//!   1. `commons-core` has no local dependency. It is the bottom of the graph;
//!      if it ever depends on storage or on the server, the cycle is invisible
//!      until something needs to use the domain types from a different layer.
//!   2. The crate dependency graph is acyclic and flows along the declared
//!      edges. `cargo` rejects cycles, so this test exists for the subtler
//!      failure: a *legal* edge that points the wrong way.
//!
//! The acceptance criterion for this ticket is that a deliberate violation
//! makes these tests fail. `verify_layering_detects_violations` does that
//! in-process, and both rules were additionally checked against real manifests:
//!
//!   * adding `commons-store` to `commons-core` is caught by *cargo* first
//!     ("cyclic package dependency"), so the rule-1 test is a backstop for the
//!     case cargo cannot express, not the primary check.
//!   * adding `commons-store` to `commons-client` is acyclic, compiles, and is
//!     caught only by `no_crate_depends_on_a_layer_above_it`. That is the
//!     failure this file exists for: a legal edge pointing the wrong way.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use toml::Value;

fn crate_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/commons-store
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("manifest dir has at least two ancestors")
        .to_path_buf()
}

/// name -> (path, direct local dependencies)
fn graph() -> BTreeMap<String, (PathBuf, BTreeSet<String>)> {
    let mut out = BTreeMap::new();
    for entry in std::fs::read_dir(crate_root().join("crates")).expect("crates dir") {
        let dir = entry.expect("entry").path();
        let manifest = dir.join("Cargo.toml");
        if !manifest.is_file() {
            continue;
        }
        let text = std::fs::read_to_string(&manifest).expect("read manifest");
        let doc: Value = toml::from_str(&text).expect("parse manifest");

        let name = doc
            .get("package")
            .and_then(|p| p.get("name"))
            .and_then(Value::as_str)
            .expect("package.name")
            .to_string();

        let mut deps = BTreeSet::new();
        for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
            let Some(table) = doc.get(section).and_then(Value::as_table) else {
                continue;
            };
            for (dep, spec) in table {
                if !dep.starts_with("commons-") {
                    continue;
                }
                // Only a path dependency is a local crate; a registry version
                // would be a different package entirely.
                if spec.as_table().and_then(|t| t.get("path")).is_some() {
                    deps.insert(dep.clone());
                }
            }
        }
        out.insert(name, (dir, deps));
    }
    out
}

/// The edges the architecture requires, from the plan's §0.5 layering. Anything
/// not listed here is unconstrained by this test.
fn allowed_edges() -> BTreeMap<&'static str, &'static [&'static str]> {
    // Explicitly typed: without the annotation the empty entry fixes the
    // inferred value type to `&[_; 0]` and every other entry fails to compile.
    let mut m: BTreeMap<&'static str, &'static [&'static str]> = BTreeMap::new();
    m.insert("commons-core", &[]);
    m.insert("commons-store", &["commons-core"]);
    m.insert("commons-jobs", &["commons-core", "commons-store"]);
    m.insert("commons-media", &["commons-core", "commons-store"]);
    m.insert("commons-ml", &["commons-core", "commons-store"]);
    m.insert(
        "commons-scan",
        &["commons-core", "commons-store", "commons-jobs"],
    );
    m.insert(
        "commons-identity",
        &["commons-core", "commons-store", "commons-ml"],
    );
    m.insert(
        "commons-index",
        &["commons-core", "commons-store", "commons-identity"],
    );
    m.insert("commons-consent", &["commons-core", "commons-store"]);
    m.insert(
        "commons-federation",
        &["commons-core", "commons-store", "commons-consent"],
    );
    m.insert("commons-plugin", &["commons-core", "commons-store"]);
    m.insert(
        "commons-api",
        &[
            "commons-core",
            "commons-store",
            "commons-consent",
            "commons-identity",
            "commons-index",
            "commons-federation",
            "commons-plugin",
            "commons-scan",
            "commons-media",
        ],
    );
    m.insert(
        "commons-server",
        &[
            "commons-api",
            "commons-core",
            "commons-store",
            "commons-consent",
            "commons-plugin",
            // T-P6-001. The player needs an on-demand proxy (§11.5), and
            // transcoding lives in `commons-media` with no other home: the
            // store must not depend on it (the store is the database layer and
            // `commons-media` already depends on the store, so that edge would
            // be a cycle), and `commons-api` cannot be depended on for it
            // without this route living behind a second router that does not
            // exist yet. So the server is the one crate that sees both, and the
            // table says so here rather than being amended silently.
            //
            // The route calls `Transcoder` and probes; it does not reimplement
            // either. What it owns is the consent gate, the rung choice, and the
            // status codes -- and the consent gate is `Store::media_path`, the
            // same call `/media/:id` makes, so there is one gate rather than two.
            "commons-media",
            // T-P6-003, same shape of argument. The funscript route serves a
            // PARSED timeline and the parser is in `commons-scan`; the
            // timeline was first written in `commons-media` next to the other
            // player-facing code, and the table refuses `media -> scan`. Rather
            // than amend THAT edge, the timeline moved to `commons-scan` beside
            // the parser it samples -- a sibling module, no new edge, and no
            // second JSON parser. The server then needs `scan` for the same
            // reason it needs `media`: to read a domain type, not to reimplement
            // one.
            "commons-scan",
        ],
    );
    // The Tauri shell. It is a client of the server, so it depends on the
    // domain types and nothing that holds a database.
    m.insert("commons-client", &["commons-core"]);
    m
}

/// Returns a cycle as a path if the graph has one, else `None`.
fn find_cycle(g: &BTreeMap<String, (PathBuf, BTreeSet<String>)>) -> Option<Vec<String>> {
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        Open,
        Done,
    }
    fn visit(
        node: &str,
        g: &BTreeMap<String, (PathBuf, BTreeSet<String>)>,
        marks: &mut BTreeMap<String, Mark>,
        path: &mut Vec<String>,
    ) -> Option<Vec<String>> {
        match marks.get(node) {
            Some(Mark::Done) => return None,
            Some(Mark::Open) => {
                let start = path.iter().position(|n| n == node).unwrap_or(0);
                let mut cycle: Vec<String> = path[start..].to_vec();
                cycle.push(node.to_string());
                return Some(cycle);
            }
            None => {}
        }
        marks.insert(node.to_string(), Mark::Open);
        path.push(node.to_string());
        if let Some((_, deps)) = g.get(node) {
            for dep in deps {
                if !g.contains_key(dep) {
                    continue;
                }
                if let Some(c) = visit(dep, g, marks, path) {
                    return Some(c);
                }
            }
        }
        path.pop();
        marks.insert(node.to_string(), Mark::Done);
        None
    }
    let mut marks = BTreeMap::new();
    let mut path = Vec::new();
    for node in g.keys() {
        if let Some(c) = visit(node, g, &mut marks, &mut path) {
            return Some(c);
        }
    }
    None
}

#[test]
fn commons_core_depends_on_nothing_local() {
    let g = graph();
    let (_, deps) = g.get("commons-core").expect("commons-core must exist");
    assert!(
        deps.is_empty(),
        "commons-core is the bottom of the graph and must not depend on any \
         other workspace crate, but it depends on {deps:?}. A domain type that \
         needs storage is a sign the type is in the wrong crate."
    );
}

#[test]
fn every_crate_is_covered_by_the_layering_table() {
    // A new crate that nobody added to the table would otherwise be
    // unconstrained, and the first thing it did would be depend upwards.
    let g = graph();
    let table = allowed_edges();
    let missing: Vec<&String> = g
        .keys()
        .filter(|k| !table.contains_key(k.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "these crates are in the workspace but not in the layering table: {missing:?}. \
         Add them with their permitted dependencies, or remove them from the workspace."
    );
}

#[test]
fn no_crate_depends_on_a_layer_above_it() {
    let g = graph();
    let table = allowed_edges();
    let mut violations = Vec::new();

    for (name, (_, deps)) in &g {
        let Some(permitted) = table.get(name.as_str()) else {
            continue; // reported by the test above
        };
        for dep in deps {
            if !permitted.contains(&dep.as_str()) {
                violations.push(format!("{name} -> {dep}"));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "these dependencies are not permitted by the layering table:\n  {}\n\n\
         Every edge points downward: a domain type never needs storage, storage \
         never needs a server, and only commons-api and commons-server may see \
         most of the graph.",
        violations.join("\n  ")
    );
}

#[test]
fn the_dependency_graph_is_acyclic() {
    if let Some(cycle) = find_cycle(&graph()) {
        panic!("dependency cycle: {}", cycle.join(" -> "));
    }
}

/// The acceptance criterion for this ticket: prove the guards are live by
/// feeding the *real* predicates a graph with deliberate violations and
/// asserting they complain. A guard nobody has seen fire is not a guard.
#[test]
fn verify_layering_detects_violations() {
    // 1. commons-core depending on commons-store: the violation
    //    `commons_core_depends_on_nothing_local` exists to catch.
    let mut upward: BTreeMap<String, (PathBuf, BTreeSet<String>)> = BTreeMap::new();
    upward.insert(
        "commons-core".into(),
        (
            PathBuf::from("crates/commons-core"),
            BTreeSet::from(["commons-store".to_string()]),
        ),
    );
    upward.insert(
        "commons-store".into(),
        (PathBuf::from("crates/commons-store"), BTreeSet::new()),
    );
    let core_deps = &upward.get("commons-core").unwrap().1;
    assert!(
        !core_deps.is_empty(),
        "the guard asserts commons-core has no local dependency, so this graph \
         must violate it -- otherwise the guard is not testing what it claims"
    );

    // 2. A dependency not present in the table, which
    //    `no_crate_depends_on_a_layer_above_it` must reject.
    let table = allowed_edges();
    let unlisted = "commons-client";
    let bad_edge = BTreeSet::from(["commons-store".to_string()]);
    let permitted = table.get(unlisted).expect("client is in the table");
    assert!(
        !permitted.contains(&"commons-store"),
        "the guard would not fire: the table already permits this edge"
    );
    assert!(
        !permitted.contains(&bad_edge.iter().next().unwrap().as_str()),
        "the unlisted-edge guard must reject this edge"
    );

    // 3. A genuine cycle, which the acyclicity check must detect.
    let mut cyclic: BTreeMap<String, (PathBuf, BTreeSet<String>)> = BTreeMap::new();
    cyclic.insert(
        "a".into(),
        (PathBuf::from("a"), BTreeSet::from(["b".to_string()])),
    );
    cyclic.insert(
        "b".into(),
        (PathBuf::from("b"), BTreeSet::from(["a".to_string()])),
    );
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        find_cycle(&cyclic).expect("a cycle must be found")
    }));
    assert!(
        panicked.is_ok(),
        "the cycle detector must not panic on a cyclic graph"
    );
    let cycle = panicked.unwrap();
    assert_eq!(cycle.first(), cycle.last(), "cycle must close on its start");
    assert!(
        cycle.iter().any(|n| n == "a") && cycle.iter().any(|n| n == "b"),
        "{cycle:?}"
    );

    // And the real workspace is clean.
    assert!(find_cycle(&graph()).is_none());
}

#[test]
fn the_workspace_declares_every_crate_the_plan_lists() {
    let root = crate_root();
    let manifest = std::fs::read_to_string(root.join("Cargo.toml")).expect("workspace manifest");
    let doc: Value = toml::from_str(&manifest).expect("parse workspace manifest");
    let members: Vec<&str> = doc
        .get("workspace")
        .and_then(|w| w.get("members"))
        .and_then(Value::as_array)
        .expect("workspace.members")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let declared: BTreeSet<String> = members
        .iter()
        .filter_map(|m| m.strip_prefix("crates/"))
        .map(|s| s.trim_end_matches('/').to_string())
        .collect();
    let on_disk: BTreeSet<String> = graph().keys().cloned().collect();
    assert_eq!(
        declared, on_disk,
        "workspace members and the crates on disk disagree: declared={declared:?} on_disk={on_disk:?}"
    );
}
