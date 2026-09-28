---
title: Plan — T-P6-004b interview derivation
date: 2026-09-28
status: not-started
implements: docs/spec/t-p6-004b-interview-derivation.md
---

# Implementation plan — T-P6-004b

**Spec:** `docs/spec/t-p6-004b-interview-derivation.md`. Read §4 before
starting; the four failure modes are the actual content of this ticket.

**Environment (required — the store panics without it, by design):**

```sh
export CARGO_TARGET_DIR=/home/alvaro/.cargo-target/commons
export PGHOST=127.0.0.1 PGUSER=postgres PGPASSWORD=smoke_pw
export DATABASE_URL="postgres://postgres:smoke_pw@127.0.0.1/postgres"
cd ~/code-local/rust/commons
```

`DATABASE_URL` points at the **`postgres` database**, not a project database:
`postgres_store()` runs `CREATE SCHEMA <uuid>` per test.

**Two rules that are constraints, not style:**

1. **`object_id` is a `String`.** Never parse it. The scanner writes
   `o-pending-<uuid>` and `phase-7-050-interview-routes` fixed exactly this.
2. **Every new column is a NEW migration file in BOTH trees** —
   `crates/commons-store/migrations/postgres/NNNN_*.sql` *and*
   `.../sqlite/NNNN_*.sql`. Never edit an applied migration: sqlx records
   applied versions and skips them, so an edit is a silent no-op. This has
   already produced three wrong-answer bugs in this repository.

---

## Step 1 — reuse `tag.importance`; do NOT add a second weight column

**CHECKED 2026-09-28 while writing this plan: step 1 is a no-op. There is
nothing to build here, and the plan says so rather than inventing a migration
to justify itself.**

`tag` already has a weight column. `migrations/*/0001_core.sql:196` declares:

```sql
CREATE TABLE tag (
  id          TEXT PRIMARY KEY,
  name        TEXT NOT NULL,
  parent_id   TEXT REFERENCES tag(id) ON DELETE SET NULL,
  namespace   TEXT NOT NULL DEFAULT 'canonical',
  color       TEXT,
  importance  REAL NOT NULL DEFAULT 1.0,
  UNIQUE (namespace, name)
);
```

And it is plumbed, not merely declared:

- `commons-core/src/domain.rs:221` — `pub importance: f64,` on `Tag`, with the
  doc comment "a tag with a tree position, a namespace, and an importance
  weight (§5.15)".
- Nothing anywhere expects a `tag.weight`. The only `.weight` reads are
  `commons-index/tests/resolve.rs:1064`, comparing two *different* weights on
  a person/tagger pair.

**So: do not add a `weight` column.** One table with `importance` and `weight`
meaning the same thing makes "which one does this query read?" unanswerable
from the schema — and §6a is a list of exactly that class of mistake.

There is a second `weight` in `0001_core.sql:246`, on the **`vote`** table — a
voter's reputation at cast time. Different concept, not a candidate for reuse.

**Confirm before starting** (in case the schema moved since this was written):

```sh
grep -n "importance" crates/commons-core/src/domain.rs
grep -n "importance  REAL" crates/commons-store/migrations/postgres/0001_core.sql
```

**Note the `REAL` caveat for step 2's new table:** `vote.weight` and
`tag.importance` are both `REAL` (float4 on Postgres). The new
`interview_quote` table in step 2 uses `DOUBLE PRECISION` deliberately, so a
quote's computed weight is not silently truncated to float4 while a tag's
importance is. Do not "harmonise" it back to `REAL` — different tables, and the
wider one is right for a computed value.

---

## Step 2 — quotes survive a re-transcription

The load-bearing property. `FieldProposal` already gets it right; copy that
shape rather than inventing one.

1. **New migration `NNNN_interview_quote.sql`** in both trees:

   ```sql
   CREATE TABLE interview_quote (
       id          TEXT PRIMARY KEY,
       object_id   TEXT NOT NULL,
       start_ms    INTEGER NOT NULL,
       end_ms      INTEGER NOT NULL,
       text        TEXT NOT NULL,
       weight      DOUBLE PRECISION NOT NULL DEFAULT 1.0,
       -- WHO decided this quote matters, and on what scale. Without this
       -- column a human's 1-5 and a model's 0.0-1.0 land in one ranking and
           -- nothing anywhere reports the mixture.
       weight_source TEXT NOT NULL CHECK (weight_source IN ('human', 'model')),
       created_at  INTEGER NOT NULL
   );
   CREATE INDEX interview_quote_object_idx ON interview_quote (object_id, start_ms);
   ```

2. **`commons-store/src/interview.rs`**, alongside `propose_correction`:

   ```rust
   pub async fn propose_quote(
       store: &Store,
       object_id: &str,          // String, always — see rule 1
       start_ms: i32,
       end_ms: i32,
       text: &str,
       weight: f64,
       source: WeightSource,
       by: Option<&str>,
   ) -> Result<Uuid>
   ```

   Reject `end_ms <= start_ms` and an empty/whitespace `text` — a zero-length
   quote exists, renders, and is meaningless. Mirror the validation
   `propose_correction` already does rather than inventing a second style.

3. **A re-transcription must not delete quotes.** `replace_transcript` deletes
   `interview_word` and `interview_window` children. It must **not** touch
   `interview_quote`, and the reason it does not is that the quote is keyed on
   `object_id`, not `transcript_id`. Write that in a comment on the delete, or
   the next person will "tidy up" the inconsistency.

4. **Tests** in `crates/commons-store/tests/interview_db.rs`, inside the
   existing `both_engines!` macro:

   - `a_quote_survives_a_re_transcription` — record a transcript, propose a
     quote, re-transcribe with different words, assert the quote and its
     weight are still there. **This is the test that fails if step 3 is
     skipped.**
   - `a_zero_length_quote_is_refused`
   - `a_whitespace_quote_is_refused`
   - `a_human_weight_and_a_model_weight_are_both_storable` — and the two
     `weight_source` values round-trip, which is what makes §4.2 detectable
     later rather than a comment nobody reads.

**Verify:**

```sh
cargo test -p commons-store --test interview_db -- --test-threads=1
```

Expected: all pass, including the pre-existing 24. The re-transcription test
is the one that matters; run it alone and confirm it is not vacuous:

```sh
cargo test -p commons-store --test interview_db a_quote_survives
```

**Commit:** `feat: quotes are keyed on the object, so a re-transcription cannot drop them`

---

## Step 3 — the mixed-scale test, before any ranking code

**Deliberately out of order.** §4.2 is a silent failure: nothing errors, the
ordering is just wrong, and it is far cheaper to write the test before the
feature than to debug the feature after.

1. Add a pure function to `commons-store/src/interview.rs` (no I/O, so it is
   testable without a database):

   ```rust
   /// Compare a human's 1-5 against a model's 0.0-1.0.
   ///
   /// The two are NOT on one scale and this is the only place that knows it.
   /// `human` is taken at face value; `model` is multiplied into the human
   /// band. Sorting without going through here is how a model's best output
   /// ends up below a human's worst.
   pub fn comparable_weight(weight: f64, source: WeightSource) -> f64
   ```

2. Test it in a **unit test in the same file** (not the db suite — it has no
   I/O): the boundaries. Human 1 < human 5, model 0.0 < model 1.0, and the
   cross case — a model at full confidence outranks a human's minimum, and a
   model at zero does not outrank anything. That last one is the assertion
   that fails if someone later "simplifies" the mapping to a plain multiply.

3. Also assert the **table is not already sorted for you**: build a list in a
   deliberately interleaved order, sort it through `comparable_weight`, and
   assert the two sources come out in separate bands.

**Verify:**

```sh
cargo test -p commons-store --lib comparable_weight -- --nocapture
```

**Commit:** `test: a human's weight and a model's weight are not the same unit`

---

## Step 4 — a model topic is a proposal, not a tag

**No new proposal type, and no enum change.** `FieldProposal` already carries
everything needed — verified, not assumed:

- `commons-core/src/domain.rs:238` — `pub source: ProposalSource,`
- `:239` — `pub proposer_kind: ProposerKind,`
- `:242` — `pub confidence: Option<f64>,` documented as "set for machine
  proposals; `None` for humans"

So a model topic is an existing `FieldProposal` with `proposer_kind = Machine`
and a confidence. **Adding a parallel `TopicProposal` would give one concept
two storage models**, and the §6a class of bug follows from exactly that.

1. `commons-store/src/interview.rs`:

   ```rust
   pub async fn propose_topic(store: &Store, object_id: &str, topic: &str,
                              weight: f64, by: Option<&str>) -> Result<Uuid>
   pub async fn accept_topic(store: &Store, proposal_id: Uuid) -> Result<Tag>
   pub async fn reject_topic(store: &Store, proposal_id: Uuid) -> Result<()>
   ```

   `accept_topic` is the only path that writes a `Tag` with a model-derived
   `importance`. `reject_topic` deletes the proposal and **writes no trace** —
   the user is not re-asked.
2. **Check whether `propose_topic` would re-insert a topic a user already
   rejected** on the next run, and if so scope it so it does not. A library
   that re-asks what you dismissed is a library you stop opening.
3. **Tests:** a proposed topic is not in the tag list; accepting it puts it
   there with its `importance`; rejecting it leaves the tag list exactly as it
   was; a rejected topic is not re-proposed by the next run.

**Verify:**

```sh
cargo test -p commons-store --test interview_db topic -- --test-threads=1
```

**Commit:** `feat: a model topic is a proposal until a person accepts it`

---

## Step 5 — diarisation proposes, it does not merge

1. **No new table.** `PersonCluster` exists. This step writes a *proposal* into
   the existing proposal mechanism.
2. `commons-store/src/interview.rs`:

   ```rust
   /// Propose that the speaker in this window is the person in `cluster`.
   ///
   /// Never performs the merge. Automatic cluster merging is unrecoverable
   /// once a client has rendered the merged card — spec §4.4.
   pub async fn propose_speaker_cluster(
       store: &Store, object_id: &str, transcript_id: Uuid,
       cluster_id: Uuid, start_ms: i32, end_ms: i32, speaker: &str,
   ) -> Result<Uuid>
   ```

3. **Tests:** proposing writes no change to `PersonCluster` membership; two
   proposals for the same window are idempotent; a proposal naming a cluster
   from a *different* object is refused (the same scoping bug
   `a_correction_is_scoped_to_one_object` catches for corrections).

**Verify:**

```sh
cargo test -p commons-store --test interview_db speaker -- --test-threads=1
```

**Commit:** `feat: diarisation proposes a cluster merge and performs none`

---

## Step 6 — the server routes

1. `commons-server/src/interview.rs` gains `GET /media/:id/quotes` and
   `GET /media/:id/topics`, each gated on **the same 404 `GET /media/:id`
   uses** — absent, off-disk and denied are indistinguishable, and a new route
   that forgets this re-opens a library-probing hole.
2. `GET /media/:id/quotes?offset=&limit=` reuses the paging shape from
   `get_words`, including the `truncated` flag. Do not invent a second pager.
3. Topics ship the **proposal state** in the response
   (`proposed: bool`), so a client can render "suggested" without inferring it
   from an absent field. The existing module docs make this exact argument for
   `confidence: null` vs `0` and for `end_ms: null`; follow it.
4. **Tests** in `commons-server/tests/interview_route.rs`: 404 parity with
   `GET /media/:id` (assert the bodies are byte-identical, as
   `an_absent_object_and_a_denied_object_answer_byte_identical_404s` does),
   paging, and a proposed topic marked as proposed.

**Verify:**

```sh
cargo test -p commons-server --test interview_route -- --test-threads=1
```

**Commit:** `feat: quotes and topics over HTTP, with the proposal state in the body`

---

## Step 7 — docs, and the milestone

1. `docs/HANDOFF.md` — a section in the house style: what was built, the
   bugs it found, and what is deliberately not done.
2. `docs/plans/implementation-plan.md` — update T-P6-004b's Progress line to
   done, and note that T-P6-004b is now the unblocked follow-on for the Q&A
   page the parent split out.
3. `crates/commons-ml/tests/asr_timing.rs` — **do not** touch. It is the
   parent's criterion 1 and it passes.

**Final gate, all of it:**

```sh
export CARGO_TARGET_DIR=/home/alvaro/.cargo-target/commons
export PGHOST=127.0.0.1 PGUSER=postgres PGPASSWORD=smoke_pw
export DATABASE_URL="postgres://postgres:smoke_pw@127.0.0.1/postgres"
cargo build --workspace
cargo test --workspace -- --test-threads=1
cargo clippy --workspace --all-targets 2>&1 | grep -cE '^warning: [a-z]'
```

Expected: build clean; **0 failed**; the warning count not *higher* than it was
before you started (record it first — an unrelated cleanup is a separate
commit, never mixed into a feature).

```sh
git tag -a phase-7-060-interview-derivation -m "..."
```

Then mirror: the canonical copy is `~/code-local/rust/commons`; sync
`~/code/rust/commons` with a `--ff-only` fetch/merge and confirm both
`git rev-parse HEAD` values match.

---

## What to do if a step's test passes before the feature is written

Stop and check the test actually ran. This repository has been bitten twice:

- A suite that returned early when `DATABASE_URL` was unset reported
  **"29 passed" in 0.00s** while running nothing.
- sqlx skips already-applied migrations, so mutating one is a silent no-op and
  a mutation "survives" for a reason that has nothing to do with the test.

So: **a whitebois/commons integration binary that finishes in 0.00s skipped.**
Real runs take seconds. And a migration you edited after it was applied did
not change the schema.
