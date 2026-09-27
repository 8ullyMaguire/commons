# T-P5-006 item 14 — The wall: group-by and auto-scroll (`#6544`, `#6955`)

**Plan entry:** `docs/plans/implementation-plan.md` §T-P5-006 item 14
**Spec:** §10.4 View modes; §4.2 virtualization throughout
**Follows:** item 13 (`docs/spec/t-p5-006-ignore-list.md`)
**Modules:** `ui/src/lib/api/wall.ts`, `ui/tests/helpers/row.ts` (extracted)
**Component:** `ui/src/lib/components/Wall.svelte`
**Route:** `ui/src/routes/wall/+page.svelte`
**Tests:** `ui/tests/wall.test.ts` (38), `ui/e2e/wall.spec.ts` (11)
**Mutations:** `scripts/mutate-wall.mjs` — 28 applied, 28 killed, 0 survived, 0 stale

---

## 1. The decision the ticket does not mention: grouping breaks fixed-row-height

`VirtualGrid` fixes the row height, and that is what lets it compute the scroll
height of a 100,000 item library from the count alone — the scrollbar is right
on the first frame and the user can drag to the end immediately.

A grouped wall has a **variable** row height: a header, then however many rows
the group happens to need. So the wall does not reuse `VirtualGrid`'s scrolling;
it has its own, and `wall.ts` owns the geometry. The component holds only DOM.

Two ways out of the variable-height problem, and the honest one was chosen:

- measure the groups, which needs the whole list before the first paint;
- reserve a **placeholder** per group and say the scrollbar is approximate.

`groupExtents` returns an `exact` flag precisely because the scrollbar is
approximate while groups load. A scrollbar that is approximately right must not
be *presented* as exact, and pretending otherwise is how a virtualizer ships a
scroll range that jumps under the user's thumb.

`medianGroupHeight` is the median, not the mean, for a reason worth keeping:
with the mean, one group of 5,000 items makes every other section reserve 5,000
items of blank space.

## 2. Three bugs the tests found in the pure module

All three shipped into the first test run and all three would have been visible
to a user:

- **`String(null)` is the four characters "null".** `groupValue` did
  `String(row.organized)` for a `string | null`, so every unfiled row landed in a
  section literally titled `null`. Rating had the same shape.
- **`yearOf('2024')` returned `null`.** The regex demanded a `-`. A year-only
  date is what a badly-named file produces, and "group by year" on such a
  library produced no sections at all — silently, because an empty result is a
  valid result.
- **Auto-scroll re-pinned on "close enough".** The unpin path checked
  `atBottom`, which includes the slack. So a user who scrolled up by 40 pixels
  of a 48-pixel slack got dragged straight back down — the exact behaviour the
  feature exists to prevent, re-entering through the unpin path. It now requires
  the **actual** bottom to re-pin.

## 3. The test that named a direction, not a magnitude

```ts
assert.ok(before.groupHeights[1] >= after.groupHeights[1]);
```

`groupExtents` reserves space for groups that have not loaded, and the first
version reserved the median placeholder. That is a wall which **shrinks** as
rows arrive, moving everything under the user's cursor. A pending group is
therefore at least `max(placeholder, known + pending)`.

The assertion is directional on purpose. "It is tall enough" passes on a wall
that teleports; only "it never gets shorter" catches the shrink.

## 4. Two component bugs only the browser could find

Both are the same shape as bugs already recorded in the harness skill, which is
the argument for the skill existing.

**`KeysetStore` is a class with a `get state()`.** A getter over `#state` is not
a tracked dependency, so `const rows = $derived(store.rows)` computed once at
mount and never again. The wall rendered an empty list while looking finished,
and the build was happy. It mirrors the store now, and reassigns after every
load — a `.then` on the *initial* load catches one page and none of the rest,
which is a wall that stops growing exactly when the user starts scrolling.

**Sections are not content.** The section elements exist the instant the query
starts and hold nothing until the page lands, so the e2e's
`toHaveCount` on a section passed against an unloaded wall. It waits for a
**tile**. This is the harness lesson about `toBeVisible()` not waiting for the
row, arriving from the other direction: the element under test was there and
empty.

## 5. The extracted row factory, and the trap it removed

Adding four grouping fields to `ObjectRow` broke four test files at once, each
with its own `function row(...)`. That is the wrong number of files to touch for
a field most of them do not use, and the failure mode is quiet — a helper that
omits a field and is loosely typed keeps compiling long after the shape moved.

`ui/tests/helpers/row.ts` is now the one place a row is built, and it is total:
every field of `ObjectRow`, with the null case spelled out. A new field breaks
this factory and nothing else.

Getting it running cost a runner change that is worth recording, because the
failure is invisible: **`run-tests.mjs` globs `tests/*.test.ts`, which is not
recursive.** A helper under `tests/helpers/` is therefore never compiled into
the temp dir, the importing file fails to resolve it, and `node --test` reports
one failure with no assertion — while the whole file's tests silently vanish
from the count. The suite went 490 → 459 with two failures, and the cause was a
glob. One line of the runner fixed it.

## 6. A mutation script that reported 22 stale out of 28 and looked fine

The first run was `6 killed, 0 survived, 22 stale`. Zero survivors is the
number a lazy script reports when it is not finding the source at all.

`substitute` compiled **string** patterns as regexes. Half the patterns are
source lines full of metacharacters — `(` in `groupRows(...)`, `?` in
`performers?.[0]`, the `[]` of array indexing — and a regex built from those
does not match the literal line. Every one of those mutants was reported stale
and never tested.

A mutation script whose output is mostly `STALE` is a broken script that looks
like a test result. The fix is to match strings literally, and the tell is in
the count: a high stale number is a matcher bug, not a drifted source.

After the fix, 28/28 killed on the first full run, and the 26 that had been
reporting green-or-stale were mostly never actually exercised.

## 7. Results

### Unit — `ui/tests/wall.test.ts`, 38 tests

| group | the load-bearing claim |
|---|---|
| `monthOf` / `yearOf` | parsed from the **string**; `new Date('2024-06-01')` is midnight UTC and files it under May west of Greenwich |
| `groupValue` | `null` for absent, so `groupRows` can drop the row; never a placeholder string |
| `groupRows` | first-seen order, so a descending query is not re-sorted; a valueless row is dropped |
| `pendingPerGroup` | proportional, so the scrollbar is right at the top |
| geometry | `columns >= 1` always; no trailing gap; median not mean |
| `groupExtents` | `exact` is false while loading; **a group never shrinks** |
| `autoScroll` | slack survives momentum; unpin is sticky; only the true bottom re-pins |

### Browser — `ui/e2e/wall.spec.ts`, 11 tests

| claim | why only a browser |
|---|---|
| sections are labelled | rendered headers |
| **every tile is in the section its field names** | grouping partitions, not just decorates |
| no headers when ungrouped | `groupLabel('none') === null` reaches the DOM |
| an unrecognised group falls back | a stale bookmark shows the grid |
| **the scroll range matches the content** | spacer vs. `data-total`, within a pixel |
| sections sit at the computed offsets | the layout matches the pure geometry |
| an all-loaded wall reports exact | the flag, not the height |
| the group key round-trips through the URL | a re-navigated wall |
| **follows the bottom, then stops** | both directions, with `data-pinned` |
| a wall that fits its viewport does not scroll | no negative max scroll |

## 8. Mutations — `scripts/mutate-wall.mjs`

**28 applied, 28 killed, 0 survived, 0 stale.** The ones worth naming:

- `groupExtents` reserves only the placeholder → **killed**, by the shrink test.
- `groupExtents` reports an approximate wall as exact → killed, by `exact`.
- `groupRows` sorts the groups → killed, by the first-seen-order test.
- `monthOf` parses with `Date`/`getMonth` → killed, by the string-parsing tests.
- `autoScroll` keeps following a user who scrolled up → killed.
- `autoScroll` re-pins an unpinned wall near the bottom → killed, by the stickiness test.
- `medianGroupHeight` uses the mean → killed, by the huge-group test.

## 9. What is deliberately not here

- **Not virtualized per section.** Every group renders its tiles; the
  virtualization is at the *page* level (`KeysetStore`), not the pixel level.
  A grouped wall with 50,000 rows in one group renders 50,000 tiles, which
  violates §4.2. This is the known gap and it is the next item, not a claim
  this one makes.
- **No server-side grouping.** The `GroupKey` union is a client-side switch over
  the loaded page. Real group-by is a query parameter and a `GROUP BY`, and
  belongs with T-P6-007.
- **No drag between sections**, no collapse/expand of a section, no persisted
  grouping preference. None are in #6544.

## 10. A pre-existing flake, recorded

`commons-jobs`' `dropping_a_supervised_child_leaves_no_zombie` failed once
during the full-suite run: "left 3 zombies, up from 2". It passes 3/3 in
isolation. The assertion counts **system-wide** zombie processes, so a busy host
with other test threads running makes it non-deterministic. Not caused by this
item and not fixed by it; the full suite was re-run and is green. It is worth
fixing separately — a test that reads a machine-global counter is a test that
will fail again in CI.
