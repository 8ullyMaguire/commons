//! §14.1: the one-way door, made mechanical.
//!
//! `commons-store/src/query.rs` claims in its module docs that there is exactly
//! one sanctioned way to read objects, and names the test that keeps it that
//! way:
//!
//! > Object reads go through `query`; anything else is caught by a test in
//! > `crates/commons-consent/tests/policy.rs` that scans the workspace for
//! > `SELECT ... FROM object` outside this file.
//!
//! **That test does not exist.** `crates/commons-consent/tests/` contains only
//! `takedown.rs`. So the guarantee was real in intent and *unenforced* — the
//! same shape `query.rs` records for `consent_clause` itself, which "had existed
//! since T-P0-005 and [was] correct in intent, but for its whole life was a
//! `pub fn` called by nothing outside its own test module". One layer up, the
//! same layer, a documented guarantee with nothing behind it.
//!
//! The current state, measured at `09176b8`: **12 source files** in three
//! crates contain `FROM object`, and `query.rs` is one of them. Some of those
//! reads are legitimate and belong elsewhere in the rules; this test does not
//! claim they are all bugs. It claims that **the count is written down**, so
//! that the next read is a decision somebody makes rather than an accident
//! nobody notices — and so that a new read in a *fourth* crate fails.
//!
//! # Why an allowlist and not a ban
//!
//! Banning every `FROM object` outside `query.rs` would fail today on ten
//! existing files, most of them legitimately reading a column for a purpose
//! `query_sorted` does not serve (`locator`, `fuzzy`, `undo`, takedown
//! cascades). Pretending otherwise to get a green gate is the failure mode this
//! repo keeps hitting, and a test that has to be wrong to pass is worse than no
//! test: it teaches the next reader that the suite is negotiable.
//!
//! So the test pins the *current, reviewed* set. Adding a legitimate read means
//! adding it to `SANCTIONED` **with a reason**, and the reason is the part a
//! future reader can argue with. Removing an entry is a deletion that shows up
//! in a diff as a removal — which is the shape a dead door should take.
//!
//! # What this deliberately does not do
//!
//! It greps for `FROM object` in `src/` directories of the workspace. It does
//! not parse SQL, does not follow `format!`-built strings, and cannot catch a
//! read spelled `"FROM  object"` or assembled at runtime. A mechanical check
//! that claims completeness it does not have is the "confident zero" trap in a
//! new costume; the allowlist plus a review convention is honest about being a
//! tripwire, not a proof.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The workspace crates whose `src/` is scanned, and why each is in scope.
///
/// `commons-consent` and `commons-index` are here because both read `object`
/// today. A crate absent from this list is not exempt — `no_new_crate_can_read_object`
/// is what stops this list from becoming the loophole.
///
/// The per-line comments are load-bearing: `cargo fmt` collapses a three-element
/// array onto one line, which detaches the doc comment above it and trips
/// clippy's `empty line after doc comment`. Adding a fourth crate makes that
/// worse, not better.
const SCANNED_CRATES: &[&str] = &[
    "commons-consent", // takedown's existence guard
    "commons-index",   // duplicate-merge candidates
    "commons-store",   // the door, plus the searches and cascades
];

/// The reviewed set: every `src/` file that contains `FROM object`, with the
/// reason it is allowed to.
///
/// **Adding a row here is adding a sanctioned object read.** The reason is not
/// decoration — it is the only thing that tells a future reader whether the
/// read was a considered exception or a copy-paste. A row added without a
/// reason that says *what the read is for* is a bug with a comment.
const SANCTIONED: &[(&str, &str)] = &[
    // query.rs is the door. Listed so the test's own claim ("outside this
    // file") is checkable rather than asserted in prose.
    (
        "commons-store/src/query.rs",
        "the sanctioned read; §14.1's one way in for a LIST of objects",
    ),
    // Everything below predates this test. They are pinned, not blessed -- the
    // point is that the count is now a number somebody chose rather than a
    // number nobody looked at. The reason for each says whether it READS,
    // DELETEs, or assembles SQL, because "reads object" was the claim and it is
    // true of only about half of them.
    (
        "commons-consent/src/takedown.rs",
        "SELECT COUNT(*) to test a subject's existence before a takedown; a \
         guard, not a listing, and it must see rows the subject no longer \
         consents to",
    ),
    (
        "commons-index/src/candidates.rs",
        "SELECT o.title for duplicate-merge candidates, keyed from a phash hit",
    ),
    (
        "commons-index/src/history.rs",
        "merge history for one object id; the other statements are \
         object_merge, and the one object read is the winner's current title",
    ),
    (
        "commons-store/src/bulk.rs",
        "ASSEMBLES the consent-gated FROM clause for bulk edits, reusing \
         filter_ast's consent SQL. It is a second door by construction and is \
         sanctioned on that basis: same consent clause, same clause builder",
    ),
    (
        "commons-store/src/db.rs",
        "DELETE, inside a #[cfg(test)] module -- cascade assertions, not a read",
    ),
    (
        "commons-store/src/fuzzy.rs",
        "SELECT over the fuzzy-search index, which returns object ids; the \
         DELETEs rebuild the index",
    ),
    (
        "commons-store/src/locator.rs",
        "path resolution by content hash; serves a file, not an object list",
    ),
    (
        "commons-store/src/relations.rs",
        "graph traversal over object_relation, keyed from an object id",
    ),
    (
        "commons-store/src/search.rs",
        "DELETE from the search indexes and object_alias; these are the test's \
         statement table plus the cascade that keeps a search row from outliving \
         its object",
    ),
    (
        "commons-store/src/tags.rs",
        "DELETE from object_tag; tag attachment, not a listing",
    ),
    (
        "commons-store/src/undo.rs",
        "SELECT the row it is undoing, and DELETE to reverse it",
    ),
];

/// The workspace root.
///
/// `CARGO_MANIFEST_DIR` is `crates/commons-consent`, so the root is **two**
/// levels up. This was the wrong number three times, in both directions, and
/// the way it went wrong is the reason this function is documented at all.
///
/// The members live at `<root>/crates/<name>`, so a reader who counts from the
/// test *file* (`crates/commons-consent/tests/foo.rs`) rather than from
/// `CARGO_MANIFEST_DIR` reaches for three or four and lands on `rust/` or
/// `code-local/`, neither of which has a `Cargo.toml`:
///
/// | levels up | resolves to | has `Cargo.toml`? |
/// |---|---|---|
/// | **2** | **`commons/`** | **yes** |
/// | 3 | `rust/` | no |
/// | 4 | `code-local/` | no |
///
/// Every wrong value was **silent**: the scan found zero files and
/// `every_object_read_is_sanctioned` **passed**. An empty set has no
/// unsanctioned members, so a scan pointed at the wrong directory reports
/// exactly the same success as a scan that found the code and approved all of
/// it. `the_scan_actually_covers_the_workspace` exists to make that impossible,
/// and it is why a wrong path here is now a loud failure.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// The directory holding the workspace members, which is `<root>/crates`.
fn crates_dir() -> PathBuf {
    workspace_root().join("crates")
}

/// Every workspace-relative `src/**.rs` path containing `FROM object`.
///
/// Scans **all** crates, not just `SCANNED_CRATES` — the allowlist is checked
/// against the full set so that a read in an unlisted crate shows up as
/// unsanctioned rather than being invisible.
fn reads_of_object() -> BTreeSet<String> {
    let crates = crates_dir();
    let mut found = BTreeSet::new();
    let Ok(entries) = std::fs::read_dir(&crates) else {
        return found;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        if !name.starts_with("commons-") || !dir.is_dir() {
            continue;
        }
        let src = dir.join("src");
        if !src.is_dir() {
            continue;
        }
        // The prefix is `crates/`, NOT `src/`. Passing `&src` here yields
        // bare filenames (`query.rs`), which match no SANCTIONED row and make
        // every file look unsanctioned -- a failure that reads like "the
        // allowlist is wrong" when the bug is the path it is comparing against.
        collect(&crates, &src, &mut found);
    }
    found
}

fn collect(root: &Path, dir: &Path, out: &mut BTreeSet<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(root, &path, out);
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if !text.contains("FROM object") {
            continue;
        }
        let Ok(rel) = path.strip_prefix(root) else {
            continue;
        };
        out.insert(rel.to_string_lossy().replace('\\', "/"));
    }
}

fn sanctioned() -> BTreeSet<String> {
    SANCTIONED.iter().map(|(p, _)| (*p).to_string()).collect()
}

/// The scan found the workspace, and found something in it.
///
/// Split out from `every_object_read_is_sanctioned` because an empty result
/// makes that test pass for the wrong reason: an empty set has no unsanctioned
/// members, so a scan pointed at the wrong directory is indistinguishable from
/// a clean workspace. The floor is `>= 1` and the root is identified by name,
/// so "I looked in the wrong place" and "there is genuinely nothing to review"
/// cannot both be reported as success.
#[test]
fn the_scan_actually_covers_the_workspace() {
    let root = workspace_root();
    assert!(
        root.join("Cargo.toml").is_file(),
        "workspace_root() resolved to {}, which has no Cargo.toml -- the scan \
         would silently cover nothing",
        root.display()
    );
    // Belt and braces: the workspace manifest must name the crates we scan.
    let manifest =
        std::fs::read_to_string(root.join("Cargo.toml")).expect("a readable workspace manifest");
    for name in SCANNED_CRATES {
        assert!(
            manifest.contains(name),
            "the workspace manifest does not mention `{name}`, so SCANNED_CRATES \
             has drifted from the real workspace"
        );
    }
    // The scan must see the door, by name, and a realistic number of others.
    // This is the assertion that converts "found nothing" from a pass into a
    // failure. At `09176b8` there were 12 such files across 3 crates; the floor
    // is 2 so a scan that finds only the door is also treated as broken rather
    // than as good news.
    let found = reads_of_object();
    assert!(
        found.iter().any(|p| p == "commons-store/src/query.rs"),
        "the scan did not find the sanctioned read itself, so it is not looking \
         where it claims. Found: {found:?}"
    );
    assert!(
        found.len() >= 2,
        "the scan found only {} object read(s); at 09176b8 there were 12 across \
         3 crates, so the scan is under-collecting rather than the tree being \
         clean",
        found.len()
    );
}

/// The guarantee the module docs claim, now actually enforced.
#[test]
fn every_object_read_is_sanctioned() {
    let found = reads_of_object();
    let allowed = sanctioned();

    let unsanctioned: Vec<&String> = found.difference(&allowed).collect();
    assert!(
        unsanctioned.is_empty(),
        "these files read `FROM object` and are not in SANCTIONED:\n{}\n\n\
         Either the read belongs in `query_sorted` (the one sanctioned door), or \
         it is a deliberate second read and belongs in SANCTIONED with a reason. \
         An unlisted read is the failure `query.rs`'s docs say this test catches, \
         so it is being caught.",
        unsanctioned
            .iter()
            .map(|s| format!("  - {s}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// The allowlist is not allowed to rot into a blanket permission.
///
/// A row in `SANCTIONED` whose file has since stopped reading `object` is a
/// permission for a read that no longer exists, and it is how an allowlist
/// becomes indistinguishable from a ban with extra steps. Removing a read must
/// remove its row.
#[test]
fn no_sanctioned_read_has_disappeared() {
    let found = reads_of_object();
    let allowed = sanctioned();
    let stale: Vec<&String> = allowed.difference(&found).collect();
    assert!(
        stale.is_empty(),
        "SANCTIONED lists files that no longer read `FROM object`:\n{}\n\n\
         Delete these rows. A permission for a read that is gone is a hole in \
         the allowlist that the next reader cannot see.",
        stale
            .iter()
            .map(|s| format!("  - {s}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// A fourth crate reading objects must fail, not slip past the list.
///
/// `every_object_read_is_sanctioned` would already catch it, because the scan
/// covers every crate. This test names that explicitly, because the failure it
/// guards against is a reader *adding a crate to a skip list* rather than a
/// reader adding a read.
#[test]
fn no_new_crate_can_read_object() {
    // The members live at <root>/crates/<name>, so this reads crates_dir() and
    // NOT workspace_root(). Reading the root makes it iterate the *repo* root,
    // where nothing is named `commons-*` -- which makes every name in
    // SCANNED_CRATES look like a renamed crate. The failure mode is a test
    // accusing a real crate of having been renamed.
    let mut crates = BTreeSet::new();
    for entry in std::fs::read_dir(crates_dir()).expect("a crates directory") {
        let entry = entry.expect("a directory entry");
        let dir = entry.path();
        let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        if name.starts_with("commons-") && dir.join("src").is_dir() {
            crates.insert(name.to_string());
        }
    }
    for name in SCANNED_CRATES {
        assert!(
            crates.contains(*name),
            "SCANNED_CRATES lists `{name}`, which is not a workspace crate. \
             A crate that has been renamed or removed leaves the scan quietly \
             covering less than it claims."
        );
    }
    // Every crate with a src/ must be scannable, and the ones that read objects
    // must be named. Deriving the list rather than pinning it is deliberate:
    // the assertion is about coverage, not about a frozen number.
    let reading: BTreeSet<String> = reads_of_object()
        .iter()
        .filter_map(|p| p.split('/').next().map(str::to_string))
        .collect();
    for crate_name in &reading {
        assert!(
            SCANNED_CRATES.contains(&crate_name.as_str()),
            "crate `{crate_name}` reads `FROM object` but is not in SCANNED_CRATES. \
             Either the scan does not reach it, or its reads are unsanctioned and \
             nobody is looking."
        );
    }
}

/// The tripwire's own limits, asserted so they cannot be forgotten.
///
/// This test cannot catch a query built by `format!`, one spelled with extra
/// whitespace, or one on a table aliased differently. Recording that here
/// means the next person who widens the grep knows what they are replacing,
/// and means a reviewer reading "we have a test for this" knows its size.
#[test]
fn the_tripwire_catches_the_common_spellings_only() {
    let dir = std::env::temp_dir().join(format!("cq-tripwire-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temp dir");

    // Caught, and expected to be: the case is exactly `FROM object`.
    let caught = [
        "SELECT o.id FROM object o",
        "SELECT * FROM object WHERE 1=1",
        "SELECT COUNT(*) FROM object WHERE id = ?",
        "SELECT 1 FROM object",
    ];
    for (i, text) in caught.iter().enumerate() {
        std::fs::write(dir.join(format!("caught{i}.sql")), text).expect("a write");
    }
    // Missed, and documented as missed. Each is a real object read this grep
    // cannot see, which is the honest size of the guarantee.
    //
    // **`select 1 from object` is in this list, not the one above, and that is
    // the sharpest limit here.** The scan is a case-sensitive
    // `contains("FROM object")`, because that is the form the SQL in this
    // workspace actually uses. A lowercase query is perfectly legal SQL, so a
    // reader who lowercases one string escapes the tripwire entirely.
    //
    // It is not fixed here, and the reason is worth stating rather than
    // hiding: case-insensitive matching would also match prose -- this file's
    // own comments say "`FROM object`", `query.rs`'s docs quote the phrase, and
    // every one of those is a hit. Widening the grep to fix a real gap would
    // add the documentation to the allowlist and make it worse. The fix is a
    // real SQL parse, which is a different piece of work.
    let missed = [
        "select 1 from object",
        "SELECT * FROM  object",
        "SELECT * FROM \"object\"",
        "SELECT * FROM ob_ject",
    ];
    for (i, text) in missed.iter().enumerate() {
        std::fs::write(dir.join(format!("missed{i}.sql")), text).expect("a write");
    }

    let hits = std::fs::read_dir(&dir)
        .expect("a temp dir")
        .flatten()
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter(|t| t.contains("FROM object"))
        .count();

    assert_eq!(
        hits,
        caught.len(),
        "the grep's hit set changed; the `missed` list above documents the \
         spellings it cannot see and must be updated with it"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
