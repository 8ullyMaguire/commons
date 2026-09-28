# T-P6-006 — Plugin SDK completion — implementation plan

**Spec:** `docs/spec/t-p6-006-plugin-sdk-completion.md`
**Written:** 2026-09-28, after `b1ec6b3` (`phase-7-070-cast-dlna-external-players`).

**Status: not started.**

Read the spec first. It carries the probe output — twelve malformed manifests
against the current `Manifest`, eleven accepted — and that table is the
reason this ticket exists. Without it the work looks like adding some
validation, which is how you add `deny_unknown_fields`, close one row of
twelve, and stop.

---

## Step 1 — `deny_unknown_fields`, and the test that shows it is not enough

### 1.1 The attribute

`crates/commons-plugin/src/lib.rs`, on the `Manifest` derive:

```rust
/// A plugin's declared manifest. Parsed from the plugin's own metadata; every
/// field is untrusted.
///
/// `deny_unknown_fields` is the first line of defence and it is only the first:
/// it catches a misspelled KEY and nothing else. A manifest with
/// `id: ""`, `version: ""` or `api_version: 0` parses cleanly without it —
/// eleven of the twelve probe cases in the spec are value problems, not key
/// problems. `validate` is what catches those.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
```

**One line of attributes and one test.** The point of doing it alone is that
the test afterwards fails on the rows it was not supposed to fix, which is what
makes step 2 obviously necessary rather than optional.

### 1.2 The test file

`crates/commons-plugin/tests/manifest_schema.rs`

```rust
//! A1 and A2: the manifest schema validates a valid plugin and rejects an
//! invalid one, and a plugin with an undeclared capability fails to
//! instantiate.
//!
//! # The shape of this file
//!
//! One table of (json, verdict) cases, applied through the SAME public
//! entry point in every row. A manifest that is validated by one path and
//! installed by another is a manifest that will be validated by neither.

use commons_plugin::{Capability, Manifest, ManifestError};

/// A well-formed manifest, and the single source of every "valid" case.
const GOOD: &str = r#"{
  "id": "com.example.locator",
  "name": "Example Locator",
  "version": "1.0.0",
  "api_version": 1,
  "requested": ["read_library", "loopback_http"]
}"#;
```

Then the two halves.

**A1, valid:**

```rust
#[test]
fn a_well_formed_manifest_parses_and_validates() {
    let m: Manifest = serde_json::from_str(GOOD).expect("a good manifest parses");
    assert_eq!(m.id, "com.example.locator");
    assert!(m.validate().is_ok(), "{:?}", m.validate());
}
```

**A1, invalid — one test per class, so a failure names the class:**

```rust
#[test]
fn an_empty_id_is_refused() {
    assert_eq!(verdict(GOOD, |s| s.replace("com.example.locator", "")), "EmptyId");
}

#[test]
fn an_id_that_is_a_path_is_refused() {
    // Not "non-empty". `../../etc/passwd` is non-empty, and the id is the
    // key a reinstall (#6987) and an uninstall will look up.
    assert_eq!(verdict(GOOD, |s| s.replace("com.example.locator", "../../etc/passwd")), "IdNotReverseDns");
}

#[test]
fn an_empty_name_is_refused() {
    // A plugin with no name shows the user a blank row in a list of plugins.
    assert_eq!(verdict(GOOD, |s| s.replace("Example Locator", "")), "EmptyName");
}

#[test]
fn a_version_that_is_not_semver_is_refused() {
    // `version` is shown to the user and compared on update. "v2" and "latest"
    // are not versions, and a plugin that ships one cannot be compared to a
    // stored one.
    assert_eq!(verdict(GOOD, |s| s.replace("\"1.0.0\"", "\"latest\"")), "MalformedVersion");
}

#[test]
fn a_zero_api_version_is_refused() {
    // `install` refuses a *mismatch* against API_VERSION, which leaves 0 and
    // 9999 equally "a mismatch". Neither is a version anybody built against;
    // 0 in particular reads as "unset" in a language where the default is 0.
    assert_eq!(verdict(GOOD, |s| s.replace("\"api_version\": 1", "\"api_version\": 0")), "BadApiVersion");
}
```

And a **misspelled key**, which is the one `deny_unknown_fields` handles:

```rust
#[test]
fn a_misspelled_field_is_refused() {
    // `capabilties`. Without `deny_unknown_fields` this parses and is
    // silently ignored -- the plugin installs with no capabilities at all and
    // the author never finds out why.
    let json = GOOD.replace("\"requested\"", "\"capabilties\"");
    assert!(
        serde_json::from_str::<Manifest>(&json).is_err(),
        "a typo'd key must not be ignored"
    );
}
```

**The helper**, which is what keeps the file honest:

```rust
/// Parse, then validate, and return the NAME of the refusal — or `"ok"`.
///
/// Returning a name rather than a bool is what makes a failure legible: a
/// table of twenty cases where every one prints `false` tells you nothing
/// about which class regressed.
fn verdict(base: &str, edit: impl FnOnce(&str) -> String) -> &'static str {
    let json = edit(base);
    let m: Manifest = match serde_json::from_str(&json) {
        Ok(m) => m,
        // A parse error is a refusal too, and the caller compares against a
        // name. Anything that fails here must be reported by a parse-error
        // test, not by a validate-error test.
        Err(_) => return "parse error",
    };
    match m.validate() {
        Ok(()) => "ok",
        Err(e) => e.name(),
    }
}
```

**A2 — the plan's "done when":**

```rust
#[test]
fn a_capability_that_does_not_exist_fails_to_instantiate() {
    // The unknown-name half, which serde already catches. It is here because
    // it is the criterion the ticket names, and a test that stops existing
    // when the behaviour it guards is refactored is a test that has stopped
    // guarding anything.
    let json = GOOD.replace("\"loopback_http\"", "\"loopback_htt\"");
    assert!(
        serde_json::from_str::<Manifest>(&json).is_err(),
        "an undeclared capability must not instantiate"
    );
}

#[test]
fn a_manifest_requesting_nothing_is_refused_rather_than_installed_useless() {
    // The other half. An empty `requested` is *schema*-valid, and `install`
    // already refuses it with `NoCapabilities` -- so this asserts the two
    // layers agree rather than duplicating the installer.
    let m: Manifest =
        serde_json::from_str(&GOOD.replace(r#"["read_library", "loopback_http"]"#, "[]"))
            .unwrap();
    assert!(m.validate().is_ok(), "an empty request is well-formed...");
    let mut i = commons_plugin::Installer::new(commons_plugin::HostPolicy::first_party());
    assert!(
        matches!(i.install(m), Err(commons_plugin::InstallError::NoCapabilities)),
        "...and the policy layer refuses it. Two layers, one outcome."
    );
}
```

**A privacy test**, because `name()` is a string the caller compares:

```rust
#[test]
fn the_refusal_names_are_stable_strings() {
    // `verdict` compares strings, so the names are part of the API. If this
    // test is deleted the strings become free to change and every `verdict`
    // assertion silently starts passing or failing for the wrong reason.
    assert_eq!(
        [
            ManifestError::EmptyId,
            ManifestError::IdNotReverseDns,
            ManifestError::EmptyName,
            ManifestError::MalformedVersion,
            ManifestError::BadApiVersion,
        ]
        .map(|e| e.name()),
        ["EmptyId", "IdNotReverseDns", "EmptyName", "MalformedVersion", "BadApiVersion"],
    );
}
```

### 1.3 Verify

```sh
export CARGO_TARGET_DIR=/home/alvaro/.cargo-target/commons
cargo fmt -p commons-plugin
cargo test -p commons-plugin --test manifest_schema
```

**Expected at this point: the misspelled-key test PASSES and the five value
tests FAIL TO COMPILE**, because `ManifestError` and `validate` do not exist
yet. That is the point of doing step 1 alone — it proves the attribute is
necessary and not sufficient, in the order a reader will follow.

**Commit:** `fix: a misspelled manifest key is an error, not an ignored field`

---

## Step 2 — `ManifestError` and `Manifest::validate`

### 2.1 The error type

`crates/commons-plugin/src/lib.rs`, next to `InstallError`:

```rust
/// Why a manifest is not a manifest. Distinct variants because §5.18.1 shows
/// the user *which* part of their plugin is wrong, and a single `Invalid`
/// cannot do that — the same reason `InstallError` has five variants rather
/// than one.
///
/// A **schema** refusal, distinct from `InstallError` which is a **policy**
/// refusal. A manifest asking for `network` is well-formed; whether the user
/// grants it is `HostPolicy`'s business. Conflating the two would make the
/// plugin API unusable for the first-party scrapers, which §11.4 expects to
/// ask for internet access.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ManifestError {
    #[error("plugin id is empty")]
    EmptyId,
    #[error("plugin id {0:?} is not reverse-DNS lowercase: a-z, 0-9, dot, dash, underscore")]
    IdNotReverseDns(String),
    #[error("plugin name is empty")]
    EmptyName,
    #[error("plugin version {0:?} is not semver")]
    MalformedVersion(String),
    #[error("api_version {0} is not a version this host was ever built against")]
    BadApiVersion(u32),
}

impl ManifestError {
    /// A stable machine-readable name. `Display` is for a person; this is for
    /// a test table and for a UI that wants to attach a field to a message.
    pub const fn name(&self) -> &'static str {
        match self {
            Self::EmptyId => "EmptyId",
            Self::IdNotReverseDns(_) => "IdNotReverseDns",
            Self::EmptyName => "EmptyName",
            Self::MalformedVersion(_) => "MalformedVersion",
            Self::BadApiVersion(_) => "BadApiVersion",
        }
    }
}
```

### 2.2 The validator

```rust
impl Manifest {
    /// Check everything serde cannot.
    ///
    /// Split deliberately from `Installer::install`, which is where the
    /// temptation is to put it. `install` is not the only thing that reads a
    /// manifest: §5.18.1's disclosure panel renders one *before* anyone
    /// decides to install, and a future gallery listing parses them all. A
    /// check that only exists on the install path is a check the other two
    /// paths do not have.
    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.id.is_empty() {
            return Err(ManifestError::EmptyId);
        }
        if !is_reverse_dns(&self.id) {
            return Err(ManifestError::IdNotReverseDns(self.id.clone()));
        }
        if self.name.is_empty() {
            return Err(ManifestError::EmptyName);
        }
        if !is_semver(&self.version) {
            return Err(ManifestError::MalformedVersion(self.version.clone()));
        }
        // A mismatch against `API_VERSION` is `install`'s job, because a
        // mismatch is a POLICY question ("this build speaks 1, that plugin
        // speaks 2"). 0 and a wildly-future number are different: no build
        // ever spoke either, so they are malformed rather than unsupported.
        if self.api_version == 0 {
            return Err(ManifestError::BadApiVersion(self.api_version));
        }
        Ok(())
    }
}

/// Lowercase reverse-DNS-ish, and nothing that means anything to a filesystem.
///
/// **The separators are the point, not the length check.** `..` is what makes
/// `../../etc/passwd` a traversal; `/` and `\` are what make an id a path on
/// the platforms that take one. A validator that allows them has validated
/// nothing, and the id is the key a reinstall (#6987) and an uninstall will
/// look up — so this is a shape check on something that will be used as a
/// name, not a presence check.
///
/// **No dot is REQUIRED, deliberately.** The crate's own fixture is
/// `id: "commons-locator"`, and requiring a dot would refuse the one
/// first-party plugin §11.4 names. So the rule is "the characters must be
/// safe and the separators must not mean anything", not "this must look like a
/// domain". A strict reverse-DNS check is a reasonable alternative and would
/// be a breaking change to the fixture — decide it deliberately, do not
/// discover it in a failing test.
///
/// Verified against 9 inputs before being written down: `commons-locator`
/// and `com.example.locator` accepted; `../../etc/passwd`, `""`, `.leading`,
/// `trailing.`, `a..b`, `A.B`, `a/b` and `a\\b` all refused.
fn is_reverse_dns(id: &str) -> bool {
    !id.is_empty()
        && !id.starts_with('.')
        && !id.ends_with('.')
        && !id.contains("..")
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '-' | '_'))
}
```

/// A minimal semver check: `MAJOR.MINOR.PATCH`, each a run of ASCII digits,
/// optionally followed by `-prerelease` and/or `+build`.
///
/// **Hand-rolled, not the `semver` crate — and that is a correction to an
/// earlier draft of this plan, which said `semver::Version::parse` and called
/// it "a lucky break". `semver` is in `commons-plugin`'s
/// `[dependencies]` and is used NOWHERE in the crate** (`grep -rn "semver"
/// crates/commons-plugin/src/` returns nothing). So the dependency is dead
/// weight that `clippy::unused_crate_dependencies` would flag the moment it
/// was enabled, and the choice is between using it and removing it.
///
/// Remove it. Three reasons, the first being the one that matters:
/// 1. A hand-rolled check is *exactly the property under test* and therefore
///    readable in the same file as the test that pins it. `Version::parse` is
///    a black box whose edge cases nobody can check by reading — and it has
///    edge cases (see the leading-zero rule below, which `semver` also
///    applies, and which a naive version of this gets backwards).
/// 2. `semver` accepts `1.2.3+build.5`; a plugin manifest does not need build
///    metadata, and a validator looser than the field deserves is a validator
///    nobody can rely on when comparing two stored versions.
/// 3. An unused dependency kept "in case" is how a crate ends up with eleven.
///
/// **If you would rather use the crate, use it and delete this — but do not
/// leave both, and do not leave the dependency unused.**
///
/// # The leading-zero rule, and the bug I wrote first
///
/// `!p.starts_with('0')` looks right and is **wrong**: it rejects a bare `"0"`,
/// so it refuses `1.0.0` and `0.1.0` — every real version. The rule is *no
/// leading zero* (`len(p) > 1 && p.starts_with('0')`), which still refuses
/// `01.0.0` and `1.00.0`.
///
/// Found by running all 22 cases in the test table below through a Python
/// transcription of this function before writing it down. Three of the 22
/// failed, and two of them were the most important inputs there are. A
/// validator that rejects `1.0.0` fails loudly and would have been caught in
/// a test; the point of checking first is that it did not cost a test cycle.
fn is_semver(v: &str) -> bool {
    let core = v.split(['-', '+']).next().unwrap_or_default();
    let parts: Vec<&str> = core.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.chars().all(|c| c.is_ascii_digit())
                && !(p.len() > 1 && p.starts_with('0'))
        })
}

### 2.3 The 22-case table for `is_semver`

The function above is small and was wrong on first write, so it gets a table
rather than the three cases a `MalformedVersion` test would use. **These are
the 22 inputs verified against the implementation, not inputs chosen after the
fact** — they are the cases from the Python check that caught the leading-zero
bug, transcribed.

```rust
#[test]
fn a_version_must_be_three_dot_separated_numbers() {
    // The accept side. `0.0.0` and `0.1.0` are here because the first
    // implementation of this check REJECTED THEM -- `!p.starts_with('0')`
    // refuses a bare "0" -- and they are the inputs most likely to be typed.
    for good in [
        "1.0.0", "0.0.0", "0.1.0", "10.20.30", "1.2.3-alpha.1", "1.2.3+build.5",
        "1.2.3-alpha.1+build.5", "1.0.0-",
    ] {
        assert!(is_semver(good), "{good:?} should be a version");
    }
    // The reject side: too few parts, too many, empty, non-numeric, a leading
    // zero, whitespace, and a non-ASCII digit that a naive `is_numeric()`
    // would accept -- Arabic-Indic digits ARE numeric to Unicode and are not
    // digits to semver.
    for bad in [
        "1.0", "1", "", "latest", "v2", "1.0.0.0", "01.0.0", "1.00.0", "1..0",
        "1.0.", " 1.0.0", "1.0.0 ", "1.-1.0", "1.0.0a", "\u{0661}.\u{0660}.0",
    ] {
        assert!(!is_semver(bad), "{bad:?} should NOT be a version");
    }
}
```

**`\u{0661}.\u{0660}.0` is the case worth keeping.** `char::is_numeric()`
returns `true` for Arabic-Indic digits, and `char::is_digit(10)` does not.
Rust's own `char::is_ascii_digit` is the right call and this test is why.

### 2.4 Call it from `install`

One line, and it goes **first** — before the API-version check, so a manifest
that is malformed is reported as malformed rather than as a version mismatch:

```rust
pub fn install(&mut self, manifest: Manifest) -> Result<Installed, InstallError> {
    // Schema before policy. A malformed manifest reported as "wrong API
    // version" sends the author to fix the wrong thing, and a manifest with
    // `id: "../.."` reaching the duplicate-id check has already been
    // recorded in a BTreeSet.
    manifest.validate().map_err(InstallError::Malformed)?;
    // ... the existing checks
```

So `InstallError` gains:

```rust
#[error("plugin manifest is not valid: {0}")]
Malformed(#[from] ManifestError),
```

**Note the ordering this forces.** `install` currently checks `api_version`
first. With `validate` first, a manifest with both a bad id *and* a bad
version now reports the id. That is the right precedence — a malformed field
is a bug in the plugin and a version mismatch is a fact about the host — but
it changes what `an_api_version_mismatch_is_refused_rather_than_adapted`
sees if that test's fixture also has a bad id. **It uses `locator_plugin()`,
which is well-formed, so it is unaffected** — but check it rather than
assuming, and if it does break, fix the fixture, not the order.

### 2.5 Drop the dead `semver` dependency

`crates/commons-plugin/Cargo.toml` loses `semver = { workspace = true }`.

It is declared and used nowhere, and `is_semver` above deliberately does not
use it. **Check `uuid` in the same pass** — it is also declared and, on the
grep, also unused in this crate. Do not remove it if something uses it; the
point is that a declared dependency nobody uses is worth a look, and this pass
is the look.

```sh
grep -n "semver\|uuid" crates/commons-plugin/src/*.rs   # expect: no hits
```

Removing a dependency is its own commit-worthy change, so it goes in with
step 2 rather than as a drive-by.

### 2.6 Verify

```sh
cargo test -p commons-plugin
```

**Expected:** `manifest_schema` 9 passed / 0 failed, and the existing 12 unit
+ 21 integration tests still pass. **Both numbers must be checked** — a suite
that goes from 12+21 to 9+33 has broken four existing tests and gained nine,
and the total alone would not show it.

**Commit:** `feat: Manifest::validate, and install refuses a malformed manifest`

---

## Step 3 — the milestone

### 3.1 The final gate, all of it

```sh
export CARGO_TARGET_DIR=/home/alvaro/.cargo-target/commons
export PGHOST=127.0.0.1 PGUSER=postgres PGPASSWORD=smoke_pw
export DATABASE_URL="postgres://postgres:smoke_pw@127.0.0.1/postgres"
cargo build --workspace
cargo test --workspace --no-fail-fast -- --test-threads=1
cargo clippy --workspace --all-targets 2>&1 | grep -cE '^warning: [a-z]'
cargo fmt --all -- --check
```

**Expected:** build clean; **0 failed**; clippy **0**; fmt clean.

**Record the pre-ticket counts in a worktree BEFORE the first commit** if you
intend to state a delta. `git worktree add /tmp/base006 <commit before step
1>` and run the identical command there. T-P6-004b and T-P6-005 each shipped
a wrong delta because the "baseline" was taken after the work started.

**Count the new tests from the FILES, not from `git diff`.** This host has
`diff.external = difft`, so `git diff | grep -c '^+'` returns **0** for a large
diff — a confident zero that reads like a result. `grep -cE '^\s*#\[test\]' <file>`.

### 3.2 Docs

- `docs/HANDOFF.md` — the probe table and the "schema vs policy" split. The
  table is the part a future reader needs: it is the evidence that a validated
  manifest is not a sandboxed plugin.
- `docs/plans/implementation-plan.md` — T-P6-006's Progress line to **done**.
- `~/secondbrain/90-Meta/2026-09-28-master-build-todo.md` — the status table.

### 3.3 Tag and mirror

```sh
git tag -a phase-7-080-plugin-sdk-manifest -m "..."
cd /home/alvaro/mnt/thinkcentre/personal/documents/code/rust/commons
git fetch origin --tags && git merge --ff-only origin/main
# confirm both rev-parse HEAD values are identical
```

---

## What to do if a step's test passes before the feature is written

Stop and check the test actually ran. This repository has been bitten four
times by this and the symptom is always the same: a suite that reports green
in 0.00s, or a filter that matched nothing and reads as "nothing broke".

- `test result: ok. 0 passed` is a FAILURE to investigate, not a pass.
- A fixture with a FIXED id passes exactly once and then dies on the primary
  key against a database that persists. Every fixture here derives its own.
- **Run a new test TWICE.** The second run is against a database that already
  has the first run's rows.
