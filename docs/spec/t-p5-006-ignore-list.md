# T-P5-006 item 13 — Per-field ignore lists (`#2318`, `#2399`)

**Plan entry:** `docs/plans/implementation-plan.md` §T-P5-006 item 13
**Spec:** §10.10 Bulk editing (C86); §15.6 bulk data entry
**Preceded by:** item 12 (`docs/spec/t-p5-006-csv.md`)
**Modules:** `ui/src/lib/api/ignore-list.ts`, `ui/src/lib/api/tagger-fields.ts`
**Component:** `ui/src/lib/components/IgnoreListSettings.svelte`
**Route:** `ui/src/routes/tagger-settings/+page.svelte`
**Tests:** `ui/tests/ignore-list.test.ts` (66), `ui/e2e/ignore-list.spec.ts` (15)
**Mutations:** `scripts/mutate-ignore-list.mjs` — 28 applied, 28 killed, 0 exempt

---

## 1. The two issues are one mechanism at two different points

- **#2318** — "Add ability to ignore more fields when using scene tagger."
- **#2399** — "Scene Tagger — add option to exclude specific metadata fields
  from search query."

Both are "exclude some fields", and the difference is load-bearing rather than
cosmetic:

- an **applied** ignore list decides what a scrape *writes*, so an ignored field
  gets no proposal and the tagger's "Accept all" cannot overwrite a field the
  user curates by hand;
- a **search** ignore list decides what a *query* looks at. Nothing is written.

Conflating them produces the worst of both: a user who excludes a field from
the search silently stops getting its value, with nothing on screen saying why.
So they are two lists over one shared vocabulary, never merged. The e2e asserts
independence directly — tick one, assert the other is untouched.

## 2. The decision a `Set` cannot make

The obvious implementation is a `Set<string>`. That is the bug: with a set,
"ignore nothing" and "ignore everything" are **both an empty set**, and #2399's
actual use case is an allow-list wearing an ignore-list's clothes — "search on
title and date, ignore the other forty fields".

So the scope is a pair:

```ts
interface Scope {
  readonly ignoreAll: boolean;      // true: everything EXCEPT `fields`
  readonly fields: readonly string[];
}
```

`isIgnored` is three lines, and the inversion happens in exactly one place. A
caller that got it backwards would be a bug in every caller at once rather than
one.

The mode is a flag rather than a magic string in the list, because a list
containing `"*"` is a list that can be half-populated with it.

## 3. The vocabulary is derived from the schema, and a test pins it there

`FieldProposal.field` is a `String`, which is **correct**: §8.1's whole design
is that metadata is not a column per field, so a proposal can name a field this
build has never heard of and still be stored, voted on and displayed. An enum
would undo that.

The cost is that a UI has no list of checkboxes to offer.
`tagger-fields.ts` is that list, and it is a *starting point* rather than a
limit — `unknownEntries` reports an entry matching nothing here **without
removing it**, because a scraper with an unfamiliar field is normal and must
still be silenceable. A settings row that silently dropped it would be a setting
that is quietly wrong.

`tests/ignore-list.test.ts` reads `crates/commons-core/src/domain.rs` and
checks **both directions**:

- every offered name is a real column (or a declared rename);
- every user-facing column is either offered **or** on the not-offerable list.

The second direction is the one that rots. A struct gains a field, the list does
not, and the field is then un-ignorable with nothing indicating why. The test
names the struct and the field in its failure message.

Located through `COMMONS_UI_SRC`, not `import.meta.url` — the compiled test runs
from a temp directory, so a module-relative path resolves into that temp tree
and `readFileSync` throws, which looks like a broken vocabulary and is actually
a broken path.

## 4. A test that found a real omission

The pinning test failed on its first run: `Object.kind` was neither offered nor
excluded. It is the §5.1 discriminator between the seven object kinds, and a
tagger is invoked *on* an object, so its kind is already decided. It is now
excluded, with the reason in the module.

## 5. The design change a test forced: per-subject exclusions

The first version of the exclusion list was **flat** — a list of names. A test
asserting no name is both offered and excluded then failed, and the failure was
correct:

- `Object.kind` is the §5.1 discriminator and must **not** be offerable;
- `Producer.kind` (studio, circle, individual, collective — §5.12) is metadata
  a tagger absolutely **should** propose.

A flat name list has one entry for `kind` and the two subjects need opposite
verdicts, so it has to pick one and be wrong. It is now
`NOT_OFFERABLE_BY_SUBJECT`, and there is a test that pins the pair:

```ts
assert.equal(isOfferable('object', 'kind'), false);
assert.equal(isOfferable('producer', 'kind'), true);
```

A second test of the same shape is worth calling out because it **cannot fail**:

```ts
for (const f of fields) {
  assert.equal(NOT_OFFERABLE_FIELDS.includes(f) && offered.has(f), false);
}
```

It iterates the offered fields and then asks whether the field it is already
iterating is in both lists — false by construction. Rewritten to compute the
overlap per subject, which is the question it was meant to ask.

## 6. Two bugs only the browser could find

**A checkbox that unticked itself.** The tick handler and the checkbox's
`checked` both used `isIgnored`. Under an inverted scope the list holds the
fields that are **kept**, and the label says "only the ticked fields are used" —
so a tick must mean *membership*. Using `isIgnored` meant every box unticked the
moment the inversion went on, leaving the user looking at an allow-list in which
nothing was allowed.

**A URL that lied.** The scope was written with a bare
`history.replaceState`, which SvelteKit does not observe: the address bar
changed and `page.url` did not. The e2e caught it by asserting the URL *after* a
reload — the copied link was not the state the user was looking at. It is now
`goto(..., { replaceState: true, keepFocus: true })`, so a checkbox does not
push a history entry per tick and the router agrees with the address bar.

## 7. Results

### Unit — `ui/tests/ignore-list.test.ts`, 66 tests

| group | the load-bearing claim |
|---|---|
| `normaliseField` | case and space, or a list that is one entry and looks like two |
| `isValidFieldName` | permissive about shape, strict about what breaks a query |
| `normaliseAll` | sorted and deduped, or two identical settings that diff differently |
| `isIgnored` | both directions of the inversion |
| `ignoredFields` / `keptFields` | input order preserved; the two partition the input |
| `splitByField` | **both** halves, so a dropped field is visible |
| `scopeFrom` | an invalid entry is dropped and **reported** |
| `scopeToQuery` | inverted → `keep=`; empty-keep ≠ empty-query |
| `unknownEntries` | reported, never removed |
| `describeScope` | names the fields, not a count |
| vocabulary | pinned to `domain.rs` in both directions |

### Browser — `ui/e2e/ignore-list.spec.ts`, 15 tests

| claim | why only a browser |
|---|---|
| both lists, different wording | the rendered headings |
| ticking produces the scope the summary claims | two independent derivations agreeing |
| **the two lists are independent** | #2318 vs #2399 |
| the summary names fields, not a count | the rendered sentence |
| **inversion reads as an allow-list** | label *and* checkbox states |
| turning inversion off means ignore nothing | the other direction of the same control |
| an unknown field is reported and still ignored | both halves of the message |
| a field added by name | the free-text path |
| an invalid name rejected | the guard |
| **the scope round-trips through the URL** | asserted *after a reload* |
| an inverted scope reloads as inverted | the `invert=1` marker |
| `career_start` not offered on an object | §7.11 |
| **`kind`: excluded on object, offered on producer** | the pair a flat list cannot express |
| the query each list would use is shown | the serialisation |

## 8. Mutations — `scripts/mutate-ignore-list.mjs`

**28 applied, 28 killed, 0 survived, 0 stale.** First run: 25/28. All three
survivors were test gaps, and two of them are the "named a boundary without
straddling it" shape for the fourth and fifth time in this project:

- `ignoredFields` gains a `.sort()`. The first test's input was
  `['title', 'description', 'date']` — already alphabetical, so sorting changed
  nothing. It also asserted a **single** ignored field, and one field sorts to
  itself. The fixed test uses two ignored fields in non-alphabetical order, the
  smallest case where the two implementations differ.
- `isOfferable` inverted. The function is what a settings UI calls, and it had
  **no test of its own** — the other tests read the lists directly. A function
  with no caller under test and no test of its own is the easiest kind of bug to
  ship. It now has one that walks both lists.
- `Producer.career_start` becomes offerable. The general "every user-facing
  column is offered" test skips it silently, because it is deliberately not
  user-facing. §7.11's derived columns are now excluded **by name**.

## 9. What is deliberately not here

- **No persistence.** `onchange` hands the two scopes to the parent; the route
  round-trips them through the URL. The GraphQL server is T-P6-007.
- **No tagger to apply them to.** `splitByField` is the seam — it takes
  proposals and a scope and returns both halves — but no scraper runs yet, so
  nothing calls it in production. That is the next item, not this one.
- **No per-source scoping.** #2318 is about fields, not about which scraper.
  A future "ignore this field *from this source*" is a third dimension and a
  separate decision.

## 10. §10.10 is complete

Item 13 was the last of §10.10's actions: the bulk-edit modal (#5336, item 6),
right-click paste (#7139, item 11), unsaved-entry protection (#6466, #3253,
items 7–8), CSV and paste-parse import (#1296, #431, items 11–12), the per-field
ignore lists (#2318, #2399, item 13), and create-from-subpage / create-all-
missing (#3694, #1017, #3122, item 10).

The plan is 13 of 17.
