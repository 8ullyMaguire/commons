# T-P6-009 — implementation plan

Spec: `docs/spec/t-p6-009-cursor-encoding.md`. **Step 0 below is an ANSWER, not
a task** — it is the third time a `#[cfg(test)]` boundary has turned out to be
load-bearing, and recording the measurement is cheaper than rediscovering it.

Work in `~/code-local/rust/commons`. **NEVER** put `CARGO_TARGET_DIR` under
`/tmp` (16G tmpfs, this workspace's target is 5.7G).

```sh
export CARGO_TARGET_DIR=/home/alvaro/.cargo-target/commons
export PGHOST=127.0.0.1 PGUSER=postgres PGPASSWORD=smoke_pw
export DATABASE_URL="postgres://postgres:smoke_pw@127.0.0.1/postgres"
```

---

## Step 0 — ANSWERED: the hazard is cross-*sort*, not cross-*type*

The spec (§2) says a wrong-sort cursor "binds cleanly, runs, returns rows, and
pages wrongly". Measuring it splits that into two cases, and **only one of them
is dangerous**, so the tests must target that one:

- **Cross-type** (`Date` cursor seeked in `RatingSum`): binds a `Value::Str` to
  an INTEGER column. The comparison is never true, so the predicate yields
  **no rows** — an *empty page*. Visible, if unhelpful.
- **Cross-sort among the four text keys** (`Date`, `Title`, `Kind`, `AddedAt`
  are **all** `Value::Str`, all arity 2): `o.title < '2026-01-01'` is a
  perfectly valid text comparison. It returns rows. **The page is wrong and
  nothing errors.**

`after_binds`'s only check is `cursor.len() == all_keys().len()` — a **length**
check. Two text sorts of equal arity pass it.

**So the fingerprint in the wire form is not defence in depth; it is the only
guard against a silently wrong page.** Every test below that claims to prove
mismatch detection uses two *text* sorts. A test using `Date` vs `RatingSum`
would pass with no fingerprint at all and prove nothing.

## Step 1 — the wire types, in `crates/commons-store/src/cursor_wire.rs`

New module, registered in `lib.rs` next to `pub mod sort;`. Keeping it
separate from `sort.rs` is deliberate: `sort.rs` is a finished, reviewed file
about SQL, and a wire format is a different concern with a different failure
mode.

```rust
//! The wire form of a keyset cursor. T-P6-009. Spec t-p6-009 §2, §3.

use serde::{Deserialize, Serialize};

use crate::filter_ast::{base64url_decode, base64url_encode, Value};
use crate::sort::{Cursor, Sort};

/// The encoding this module writes. Bump on any incompatible change.
pub const CURSOR_WIRE_VERSION: u8 = 1;

/// What a [`CursorError`] says, in a form the caller can show a user.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CursorError {
    #[error("not a cursor: the value is not valid base64url")]
    NotBase64,
    #[error("not a cursor: the value is not the expected JSON shape ({0})")]
    Shape(String),
    #[error("unsupported cursor version {found}; this server writes version {CURSOR_WIRE_VERSION}")]
    Version { found: u8 },
    #[error("this cursor was made for a different sort (its fingerprint is {found}, this query's is {expected})")]
    SortMismatch { found: String, expected: String },
    #[error("wrong number of keys: this cursor carries {found}, this sort has {expected}")]
    Arity { found: usize, expected: usize },
    #[error("key {index} is {got}, but this sort's {column} column is {expected}")]
    KeyType {
        index: usize,
        column: &'static str,
        got: &'static str,
        expected: &'static str,
    },
    #[error("key {index} is a {got}, and a sort key is always a scalar")]
    NotScalar { index: usize, got: &'static str },
}
```

Every variant's `Display` names **what was wrong and what was expected**.
`SortMismatch` names both fingerprints; `Arity` names both counts. A caller
turning one of these into a UI message cannot do it well without that, and the
GraphQL resolver's whole design is that a refusal explains itself.

**`NotScalar` is not redundant with `KeyType`.** A `Value::List` has no
`SortKey` whose column it could match, so the "expected" column does not exist
to name. Two variants, because the messages differ: "this should have been an
int" versus "this should have been a scalar at all".

### 1a. The tagged value

```rust
/// One key, with its `Value` variant spelled out.
///
/// The tag is load-bearing. Without it `Value::Int(42)` and `Value::Str("42")`
/// are the same JSON, and `Null` and `Str("")` are both `""` or both `null`.
/// A cursor decoding an int where the column is text binds a number to a string
/// and the comparison silently does the wrong thing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Tagged {
    /// `["s", "the string"]`
    S(String),
    /// `["i", 42]`
    I(i64),
    /// `["f", 1.5]` — no sort key produces one today; the variant exists so
    /// adding one does not require a wire change, and so a cursor carrying one
    /// is refused by arity/type rather than by a JSON parse error.
    F(f64),
    /// `["b", true]`
    B(bool),
    /// `["n"]` — a NULL column, and the reason `o.date` needs a tag at all.
    N,
}
```

`rename_all = "lowercase"` gives `S`→`s`, `I`→`i`. **The enum is private** — it
is a wire detail and nothing outside this module may name it. `pub` would make
it a second thing to keep in step with `Value`'s six variants, and the
`Value`→`Tagged` mapping below is the single place that knows both.

### 1b. The envelope

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Wire {
    v: u8,
    /// The sort's fingerprint. See `Sort::fingerprint`.
    f: String,
    k: Vec<Tagged>,
}
```

### 1c. The conversions, in one place each

```rust
impl Tagged {
    fn to_value(self) -> Value { /* S→Str, I→Int, F→Float, B→Bool, N→Null */ }
    fn from_value(v: &Value) -> Option<Self> { /* List → None */ }
    fn type_name(&self) -> &'static str { /* "a string", "an integer", ... */ }
}

impl Cursor {
    /// The wire form. Infallible, for the reason `Filter::to_url` is: the
    /// payload is four scalars and a Vec of them, and there is no state here
    /// that could fail to serialize.
    pub fn to_url(&self) -> String { ... }

    /// Decode a cursor **for a specific sort**. Untrusted input: every failure
    /// is `Err`, none defaults or repairs.
    pub fn from_url(s: &str, sort: &Sort) -> Result<Self, CursorError> { ... }
}
```

**`to_url` panics on serialization failure, exactly as `Filter::to_url` does**,
with the same justification quoted in its comment: infallible by construction, so
a failure is a crate bug and is reported as one rather than swallowed. The
precedent is not incidental — an earlier `to_url` fell back to `Filter::All`,
"which turns a serialization bug into a user silently seeing the entire
library."

`from_url` performs §4's seven checks **in this order**, and the order is
load-bearing: decode base64 → parse JSON → version → fingerprint → arity →
per-key type. Each check narrows what the next can assume, and a version or
fingerprint mismatch reported as a type error would point the reader at the
wrong thing.

## Step 2 — `Sort::fingerprint`, in `sort.rs`

```rust
impl Sort {
    /// A short, stable identifier for *this sort*, as its `ORDER BY` text.
    ///
    /// Derived from `order_by()` rather than a hand-written label per sort,
    /// because `order_by()` is already the canonical description and changes
    /// automatically when a `SortKey` is added or a direction flips. A
    /// hand-written label is a second thing to forget, and forgetting it is
    /// silent — which is the hazard the whole encoding exists to close.
    ///
    /// FNV-1a, not `DefaultHasher`: the latter's output is unspecified across
    /// releases, and this value is persisted in shareable URLs. Same reason
    /// `stable_usn` stopped using it.
    ///
    /// **8 hex chars is a fingerprint, not a security boundary.** 32 bits needs
    /// a deliberate search to collide, and this guards correctness — a cursor
    /// used with the wrong sort — not authority. Stated here because the next
    /// reader will otherwise assume it load-bearing for security and either
    /// strengthen it needlessly or weaken it on purpose.
    pub fn fingerprint(&self) -> String { /* FNV-1a over order_by(), 8 hex */ }
}
```

**`order_by()` is `pub`, `all_keys()` is private** — so `fingerprint` must live
in `sort.rs` and cannot be built from outside. That is a real constraint on the
design and worth noticing: it is why this is a `Sort` method rather than a
function in `cursor_wire.rs`.

## Step 3 — the unit tests, in `sort.rs`'s existing `mod tests`

Add to the module that already has `the_order_always_ends_in_id` and
`a_cursor_from_another_sort_cannot_seek_in_this_one`:

1. `the_fingerprint_changes_when_a_direction_flips` — `(Date, Desc)` vs
   `(Date, Asc)`. **The silent one:** same keys, same arity, same value types.
2. `the_fingerprint_changes_when_a_key_is_added` — `(Date, id)` vs
   `(Date, RatingSum, id)`. Arity 2 vs 3, so arity alone would catch it; this
   test says the fingerprint is not relying on that.
3. `the_fingerprint_is_stable` — same sort twice, equal; and a **hard-coded
   literal**, so a change to `order_by()`'s text cannot silently invalidate
   every cursor in every saved URL. That is a real cost of deriving it from
   `order_by()`, and a test that the cost is *noticed* is the mitigation.
4. `a_cursor_round_trips_through_its_wire_form` — multi-key, with a `Null`.
5. `a_cursor_from_another_sort_cannot_seek_in_this_one` — **extend the existing
   test** rather than adding a near-duplicate; it currently only covers a length
   mismatch via a panic. Keep the panic test (it is the arity guard) and add the
   refusal beside it.

**The load-bearing test, and the one to write first:**

```rust
#[test]
fn a_cursor_from_another_text_sort_is_refused_rather_than_seeking() {
    // BOTH sorts are [Str, Str] and arity 2. See Step 0: this is the case that
    // produces a silently wrong page rather than an error, and a test using two
    // sorts of different value types would pass with no fingerprint at all.
    let made_for = Sort::date_desc();
    let asked_for = Sort::new(vec![(SortKey::Title, SortOrder::Asc)]);

    let wire = Cursor::new(vec![
        Value::Str("2026-01-01".to_string()),
        Value::Str("o-7".to_string()),
    ]).to_url();

    let err = Cursor::from_url(&wire, &asked_for).expect_err(
        "a date cursor seeked in a title sort must be refused, not accepted",
    );
    assert!(
        matches!(err, CursorError::SortMismatch { .. }),
        "expected a SortMismatch, got {err:?}",
    );
    // The message names both, because a caller showing it to a user cannot
    // otherwise say what went wrong.
    assert!(err.to_string().contains("different sort"), "{err}");
}
```

## Step 4 — `crates/commons-store/tests/cursor_wire.rs`

Integration, and a *new* file rather than an addition to `sort_db.rs`, because
this tests no database: it is a wire format, exactly as `filter_wire_shape.rs`
is a wire-format test that seeds no rows.

- All seven of §4's refusals, one test each, each asserting the **variant** not
  just the message (a message-only assertion passes on the wrong variant).
- `a_cursor_survives_a_round_trip_through_a_url` — with a `Value::Null` in
  slot 0, because `o.date` is nullable and a NULL that decodes as `""` is a
  cursor that seeks to the wrong place.
- **A golden-string test**, mirroring `filter_wire_shape.rs`'s stated purpose:
  the exact wire text is asserted, so a serde attribute change is visible. And a
  comment saying the UI will encode this too once §7 lands, so both sides change
  together.
- `every_socumented_refusal_is_an_error_not_a_repair` — iterates the malformed
  inputs and asserts none of them produces a `Cursor`. A `from_url` that
  "helpfully" returned `Filter::All`-equivalent on a bad input is the failure
  this whole ticket exists to avoid.

## Step 5 — the absence tests

Two claims are about something **not** being there, and neither is provable by
compiling:

1. `the_cursor_constructor_is_still_test_only` — reads `sort.rs` and asserts
   `pub(crate) fn new` still carries `#[cfg(test)]`. This is the plan's
   falsifiable claim for §5's "does not make `Cursor::new` public", and a
   regression here is the one change that would undo the type's premise.
2. `no_cursor_is_built_anywhere_outside_from_row_and_from_url` — greps
   `crates/*/src/` for `Cursor::new` and requires the only hit to be inside
   `#[cfg(test)]`. Same shape as
   `commons-consent/tests/object_read_invariant.rs`.

Both belong in `cursor_wire.rs`'s test module, and both are *about absence*, so
the doc comment should say so — a reader who finds a source-grepping test
wondering why should find the explanation next to it.

## Step 6 — the gate

Run **three** times, not two, and the reason is this session's own history: the
first pair of runs at the previous baseline was run 1 clean and run 2 FAILED on
a second-resolution clock flake. Two runs is the minimum that can find a 50%
flake; a ~2% one wants one more.

```sh
for n in 1 2 3; do
  cargo test --workspace > /home/alvaro/.hermes/profiles/coding/cache/scratch/t69-$n.log 2>&1
  # GATE: a run that produced NO results must not read as a pass.
  suites=$(grep -cE "^test result" /home/alvaro/.hermes/profiles/coding/cache/scratch/t69-$n.log)
  if [ "$suites" -eq 0 ]; then echo "RUN $n GATE FAIL: nothing ran"; continue; fi
  if grep -qE "^test .* FAILED|panicked at" /home/alvaro/.hermes/profiles/coding/cache/scratch/t69-$n.log; then
    echo "RUN $n GATE FAIL"; grep -E "^test .* FAILED" /home/alvaro/.hermes/profiles/coding/cache/scratch/t69-$n.log | head -3
  else
    echo -n "RUN $n PASS: "
    python3 - <<'PY'
import re,sys
p=f=i=s=0
for line in open('/home/alvaro/.hermes/profiles/coding/cache/scratch/t69-1.log',errors='replace'):
    m=re.match(r'test result: \w+\. (\d+) passed; (\d+) failed; (\d+) ignored',line)
    if m: p+=int(m.group(1)); f+=int(m.group(2)); i+=int(m.group(3)); s+=1
print(f"passed={p} failed={f} ignored={i} suites={s}")
PY
  fi
done
```

**Use a regex, not `awk` field-splitting.** An `awk -F'[ ;]' '{p+=$4; i+=$8}'`
undercounts `ignored` by one on a `0 passed` line, silently, because the leading
`test result: ok. ` shifts the fields. That reported `ignored=0` on a run whose
log plainly contained a 1 — *a gate contradicting its own input is worse than no
gate*. The heredoc above hardcodes the filename; fix that per run or use a
loop variable, and do not ship a gate whose count is wrong in the safe-looking
direction.

Also: **`sort_db.rs` must pass UNEDITED on both engines.** It is the plan's
falsifiable claim that this is additive, and a test edited to accommodate a new
wire format would be evidence of nothing.

## Step 7 — docs, tag, mirror

1. `CHANGELOG.md` — a section noting `Cursor::to_url` / `from_url` exist and
   that `Cursor::new` is still not public. **Not a version bump**: versioning is
   by path prefix and no route changed, which is the same reasoning T-P6-008
   applied when it refused `1.1.0`.
2. `docs/spec/t-p6-008-graphql.md` §4b and F8 — mark F8 done and say what §7
   now is.
3. `docs/HANDOFF.md` — one paragraph. Its §4 currently claims
   "no GraphQL operation" and "no server-side schema"; T-P6-008 corrected that,
   and this ticket changes the `after:` line underneath it.
4. Tag `phase-7-130-cursor-wire`, then `git pushall --tags`, then mirror to
   `~/code/rust/commons` by `git merge --ff-only` and **verify by comparing
   trees** — `git rev-parse HEAD^{tree}` on both — not by reading the merge
   output.
