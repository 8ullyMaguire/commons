# T-P5-004 — Duplicate and similar detection

**Spec:** §9.7, §5.18 (content-hash identity only), §5.9 (the two relations)
**Crate:** `commons-scan/src/dedup.rs`, `crates/commons-store/src/relations.rs`
**Plan ticket:** T-P5-004 in `docs/plans/implementation-plan.md`

---

## 1. What this ticket is

§9.7 names four signals and then says the duplicate checker is *a view over
relations*. The four signals, strongest first:

1. **Content-hash identity** (§5.18). Two files with the same `blake3` are the
   same bytes. This is the only signal that is a fact rather than an estimate,
   so it is the only one that may assert a relation on its own.
2. **xxh128 equality.** Weaker than blake3 and not worth a separate query —
   §6.2 uses it as a fast *change* detector, not an identity. Dedup reads
   `blake3` only. Stating this explicitly because the file has two hash
   columns and a reader will assume both are consulted.
3. **phash Hamming distance** (#1220). An estimate: near-duplicates
   (re-encodes, transcodes) have a small Hamming distance, unrelated files
   occasionally have a small one too. Never sufficient on its own.
4. **Size and bitrate** (#5067, #2397). A tiebreak for choosing which copy to
   keep, never evidence of duplication.

The key architectural decision, taken directly from the spec: **a relation is
stored; a duplicate is computed.** `same_scene_as` and `re_encode_of` are rows
in `object_relation` (0001, already present) and the checker is a query over
them. This is why the ticket's issue list — #39, #5786, #5823, #1220 — is one
feature and not four.

## 2. What exists and what is missing

| Piece | State |
|---|---|
| `object_relation` table | exists in 0001, `UNIQUE (from_id, to_id, relation)` |
| A write path for `object_relation` | **missing** — nothing in any crate writes to it |
| `RelationType` enum, `ALL`, `as_str`, `parse` | exists, `commons-core/src/enums.rs` |
| `file.hash_blake3`, `file.hash_xxh128` | exist, indexed (0001) |
| `object_phash`, `file_phash` | exist, 0009 |
| Hamming distance | `fn hamming` in `commons-index/src/candidates.rs`, **private** |
| A duplicate view | **missing** |

So the honest statement of the ticket is: the schema has been ready since 0001
and the code has never been written. That is why this ticket is additive plus
one migration rather than a refactor.

## 3. Design

### 3.1 Relations are directed, and direction carries meaning

`object_relation` is `(from_id, to_id, relation)` — directed, and
`UNIQUE (from_id, to_id, relation)`. `ReEncodeOf` means "this is a re-encode of
that", so direction is the whole content of the claim: `A re_encode_of B` and
`B re_encode_of A` are different facts and only one is usually true.

Three consequences, each of which needs a decision:

- **The pair must be stored once, not twice.** Storing both directions would
  make `A same_scene_as B` and `B same_scene_as A` two rows with two
  timestamps and two audit trails, and a query would have to pick one. So
  `SameSceneAs` is stored with a canonical direction — the two ids sorted —
  while `ReEncodeOf` keeps its natural direction. Two different rules for two
  relations, in one function, is the kind of thing that needs saying out loud.
- **`UnrelatedTo` is the negation of a `SameSceneAs`, and #1656 ("mark as not
  duplicate") is what writes it.** It must be able to point at a pair that no
  relation currently exists for, or marking a false positive as a non-duplicate
  would itself require asserting a duplicate first. This is the one relation
  with no precondition.
- **A relation between two objects of different kinds is a store error, not a
  silent row.** `same_scene_as` between a scene and a performer is meaningless
  and a viewer joining across it produces nonsense. Asserted, because
  `object_relation` has no CHECK for it and SQLite would not enforce one
  anyway.

### 3.2 Auto-merge is off by default, and reversible when on

#2094: auto-merge by rules, opt-in and reversible. Two requirements that pull
in opposite directions, so they are separated into two different mechanisms:

- **`AutoMergeConfig` is a value the caller passes per run, not a stored
  setting.** Nothing reads it back from the database. A default of
  `enabled: false` on the struct, plus the requirement that a caller construct
  one explicitly to turn it on, means "off" is the state a program is in when it
  does not know dedup exists.
- **Reversibility is a consequence of the relation model, not a separate
  undo log.** An auto-merge that is reversible must be able to say what it
  merged. So `AutoMergeOutcome` records the ids it touched and the relation it
  wrote, and `unmerge` takes that back out. The alternative — a snapshot of
  everything that changed — is a second source of truth that can disagree with
  the relations, and this way there is exactly one.

Auto-merge is also **restricted to `SameSceneAs`**: it never writes
`ReEncodeOf`, because a wrong auto-merge there destroys the distinction
between a re-encode and the original, which is the information #5067's
bitrate comparison is for.

### 3.3 False positives are first-class data

The plan's accept criterion: *"a dedup feature that cannot be wrong is not
trustworthy."* So a suppression is a row, not an absence, and it is queryable
— `unrelated(id_a, id_b)` returns whether a specific pair was ruled out, which
is what a UI needs to show "you marked this as not a duplicate" rather than
silently re-proposing it every scan.

Suppression is **per-pair, not per-candidate**: ruling out `A`/`B` does not
rule out `A`/`C`, because the reason `A` and `B` are not duplicates (different
performers, different day) frequently does not apply to `C`.

### 3.4 The phash threshold is a decision, not a default

`hamming()` returning `u32::MAX` on a length mismatch means "infinitely far",
which is right for a *comparison* and wrong for a *query*: a phash stored by a
different algorithm is not a distant neighbour, it is not a neighbour at all,
and including it in the candidate set is a category error. So the query filters
`algorithm = ?` as well as the distance, and a test asserts a foreign-algorithm
hash produces no candidates at all rather than being ranked last.

`hamming` moves from `commons-index` (private) to a shared home, because two
crates now need it and a copy is a divergence waiting to happen. It goes in
`commons-core` next to the relation enum, since that is where the
cross-crate vocabulary already lives.

## 4. Acceptance, per the ticket

> fixture set with one byte-identical pair, one re-encode, and one false
> positive; assert each is classified correctly and that auto-merge is off by
> default and reversible when on.

| # | Test | Asserts |
|---|---|---|
| 1 | `byte_identical_files_are_the_same_scene` | same blake3 → `SameSceneAs` proposed, and identical in both directions |
| 2 | `a_re_encode_is_not_a_same_scene` | different blake3, phash within threshold → `ReEncodeOf`, not `SameSceneAs` |
| 3 | `the_false_positive_is_suppressible_and_stays_suppressed` | a phash false positive, suppressed per-pair, and the suppression survives a re-scan |
| 4 | `auto_merge_is_off_unless_a_config_turns_it_on` | default config writes nothing |
| 5 | `an_auto_merge_can_be_unmerged` | outcome → `unmerge` → relation gone, ids back |
| 6 | `a_suppression_of_one_pair_does_not_suppress_another` | #3's per-pair rule |
| 7 | `a_relation_between_two_kinds_is_refused` | §3.1's assertion |
| 8 | `a_foreign_algorithm_phash_is_not_a_neighbour` | §3.4 |

Eight tests, each on SQLite **and** Postgres — the same corpus applied to both,
compared, and each engine's answer asserted. §3.5's cross-engine equality is a
property of the product, so a test that only runs on one engine is a test of one
engine.

### 4.1 The mutations, and what they found

`scripts/mutate-dedup.py` applies **twelve** behaviour-changing mutations
across the ticket and requires each to turn a named test red. It verifies the
replacement actually changed the file before believing the result, because a
`replace` that matched one of two arms otherwise reports a survivor that means
nothing.

**Result: 12 applied, 11 killed, 1 exempt, 0 survived.**

The one exemption is `len() > 1` in `same_scene_groups`. A group is built per
`same_scene_as` row from its two ends, and `assert_relation` refuses a
self-relation, so every group has two or more members and the filter never
removes anything a caller can reach. It is a guard against a future row type,
not a rule with a behaviour. Deleting it to make the number read 12/12 would
trade a cheap guard for a number.

**The mutations found one real bug and two classes of lying test.**

1. **A dismissal lost to a proposal.** `relation_between` ordered by
   `created_at LIMIT 1` across both relation types, so a `same_scene_as` written
   before an `unrelated_to` won — and `auto_merge` consults `is_ruled_out`
   before every merge, so the next pass re-merged the pair the user had just
   split. #1656 is "mark not duplicate" and it has to mean it. A suppression is
   the later and stronger claim, so it now orders first.
2. **The `enabled` flag was untested.** Every existing test asserted an
   *absence* — "zero merges" — and a pass that is switched off and a pass with
   nothing to do both produce zero. The test now takes a row count before and
   after, and then proves the corpus was live by switching the pass on and
   watching it write.
3. **Ties in `preferred_copy` were untested.** The fixture had two files of
   equal size, so "return the first argument" and "return the larger file"
   agreed. Equal sizes in *opposite* argument order is the case that matters: an
   order-dependent tie-break makes SQLite and Postgres pick different originals
   for the same data, since row order differs between them.

Two of the mutations initially reported SURVIVED for a reason worth recording:
the script ran `--test relations` only, so the `orient` unit tests in the lib
were never compiled and the runner could not have seen them. A mutation script
that cannot see a test is not measuring the suite.
