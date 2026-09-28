//! T-P6-007 acceptance criterion 5: `async-graphql` is **used or removed**.
//!
//! # Why this is a test and not a commit message
//!
//! The criterion says "with a test-or-grep recorded in the commit". A grep
//! recorded in a commit message is true exactly once and decays silently: the
//! next person adds a `#[cfg(feature = "graphql")]` handler, the workspace
//! `Cargo.toml` grows a `[workspace.dependencies]` entry back, and nothing
//! fails. This is the T-P6-006 `semver`/`uuid` lesson applied to a much larger
//! version of itself, so it gets the same treatment in the opposite direction —
//! the finding is pinned.
//!
//! # The part that was actually wrong
//!
//! The member-crate declaration in `crates/commons-api/Cargo.toml` was removed
//! and verified. The **workspace** declaration was not, and it was invisible to
//! every check that reads `Cargo.lock` — a `[workspace.dependencies]` entry that
//! no member crate inherits never reaches the lock file at all. So the cleanup
//! looked finished: the crate was clean, the lock had zero mentions, and a
//! `grep async-graphql Cargo.lock` gate passed. Two independent checks agreed
//! with a wrong conclusion, which is the shape this file exists to catch.

use std::process::Command;

fn cargo_toml_text(path: &str) -> String {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    // crates/commons-api -> workspace root
    let root = root
        .parent()
        .and_then(|p| p.parent())
        .expect("two levels up");
    std::fs::read_to_string(root.join(path)).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

/// No workspace manifest declares `async-graphql`.
///
/// Both files, because the two are independent declarations and removing one
/// leaves the other looking deliberate. `commons-api`'s is a comment explaining
/// the removal; the root's was the live entry.
#[test]
fn no_manifest_declares_async_graphql() {
    for path in ["Cargo.toml", "crates/commons-api/Cargo.toml"] {
        let text = cargo_toml_text(path);
        for (i, line) in text.lines().enumerate() {
            let trimmed = line.trim();
            // A comment is documentation, not a declaration.
            if trimmed.starts_with('#') {
                continue;
            }
            assert!(
                !trimmed.starts_with("async-graphql"),
                "{path}:{} still declares async-graphql: {trimmed}\n\
                 Nothing in the workspace references it. Either use it, or delete \
                 this — a declared dependency is not a feature.",
                i + 1
            );
        }
    }
}

/// No Rust source references `async_graphql`.
///
/// **This file is excluded by name, and that exclusion is load-bearing.** The
/// literal `async_graphql` appears in this file's own assertions and in its
/// module docs, so a plain `grep -rl` over `crates/` matches this file and
/// nothing else — the test fails on itself and reports a clean workspace as
/// dirty. Excluding it explicitly, with the reason written down, is what keeps
/// a reader from "simplifying" the exclusion back out.
///
/// The alternative — building the needle at runtime from pieces so the literal
/// never appears — is cleverer than it is worth, and it makes the grep
/// unreadable in the one place a person would go to check it.
///
/// If this ever fails on ANOTHER file, GraphQL is genuinely being built and the
/// declarations belong: delete this test deliberately rather than widening the
/// exclusion.
#[test]
fn no_source_file_references_async_graphql() {
    const SELF: &str = "crates/commons-api/tests/declared_dependencies.rs";
    let out = Command::new("grep")
        .args(["-rl", "async_graphql", "--include=*.rs", "crates/"])
        .current_dir(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .and_then(|p| p.parent())
                .expect("workspace root"),
        )
        .output()
        .expect("grep runs");
    // Owned Strings, not `&str`: the `Cow` from `from_utf8_lossy` is a
    // temporary, and borrowing from it into a collection that outlives the
    // statement does not compile.
    let offenders: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && *l != SELF)
        .map(str::to_string)
        .collect();
    assert!(
        offenders.is_empty(),
        "these files reference async_graphql:\n{}\n\
         The GraphQL server is a deferred half of T-P6-007 and belongs to its \
         own ticket.",
        offenders.join("\n")
    );
}

/// The workspace lock file does not resolve them either.
///
/// Kept as its own test even though it passed throughout the bug: it is the
/// check that gave the false all-clear, and asserting it explicitly means
/// someone removing it has to delete a test rather than just a line.
#[test]
fn the_lock_file_does_not_resolve_async_graphql() {
    let lock = cargo_toml_text("Cargo.lock");
    assert!(
        !lock.contains("async-graphql"),
        "Cargo.lock resolves async-graphql, so something depends on it"
    );
}
