# T-P5-006 item 15 — Windowing the wall, and closing §4.2

**Plan entry:** `docs/plans/implementation-plan.md` §T-P5-006 item 15
**Spec:** §4.2 virtualization throughout; §10.4
**Closes the gap recorded in:** `docs/spec/t-p5-006-wall.md` §9
**Module:** `ui/src/lib/api/wall.ts` (`groupLayout`, `wallWindow`, `rowRange`, `rowHeightOf`)
**Component:** `ui/src/lib/components/Wall.svelte`
**Tests:** `ui/tests/wall-window.test.ts` (21), `ui/e2e/wall.spec.ts` (4 new, 15 total)
**Mutations:** `scripts/mutate-wall.mjs` — 41 applied, 41 killed, 0 survived, 0 stale

---

## 1. What item 14 left open, stated as a §4.2 violation

Item 14's spec said it plainly: the wall virtualized per *page*, not per pixel,
so a group holding 50,000 rows rendered 50,000 tiles. That is not a cosmetic
shortfall. §4.2 says virtualization throughout, and the DOM is the thing that
stops the browser.

The reason it was left open is worth recording, because it is a real design
tension rather than an oversight. `VirtualGrid` gets §4.2 for free from one
decision: the row height is fixed, so the first visible row is
`floor(scrollTop / rowHeight) - OVERSCAN` and the whole window is O(1). A grouped
wall has variable row heights, so there is no single `rowHeight` to divide by.

The resolution is to keep the O(1) property per *group* rather than for the wall
as a whole, and to recover each group's row height from the group's own reserved
height — an exact recovery, not an estimate, because `groupHeight` assembled the
height from the same arithmetic `groupLayout` takes apart.

## 2. The bug the structure invites, and why it is subtle

A group's row window has to be computed from **its own** scroll offset:

```ts
const localTop = Math.max(0, scrollTop - b.top);
```

The naive version divides the *wall's* `scrollTop` by the row height. For the
first group that is correct, which is exactly what makes it survive a casual
look: a group starting 5,000px down the wall has a local offset of zero, so the
naive window lands tens of thousands of rows past the end of that group. The
group then renders its header and no tiles.

Three tests pin it, and the e2e asserts the rendered consequence — *every group
that overlaps the viewport has tiles in it* — because that is the shape a user
sees: a section header with blank space under it.

## 3. A second bug: overscan past the end of a group

`first = floor(localTop / rowHeight) - overscan`, then `last = max(first, min(loadedRows, first + visible))`.

For a group **entirely above the viewport** — which overscan deliberately
includes — `localTop` exceeds the group's whole height, so `rawFirst` is past
the end, and the `Math.max(first, last)` guard then inflated `last` *above*
`loadedRows`. The wall asked for rows 0..3 of a 2-row group. Harmless in the DOM
(the slice clamps) and wrong in the arithmetic, which is the kind of wrong that
becomes a visible bug the moment the slice stops clamping.

`first` is now clamped to `loadedRows` before `last` is derived from it, and the
test asserts the invariant at five scroll positions rather than one:

```ts
assert.ok(last <= box.loadedRows);
```

## 4. A test that named the wrong thing, and was wrong

```ts
const first = layout.boxes[w.groupIndices[0]!]!;
assert.ok(first.top + first.height > scrollTop);
```

This failed, and the failure was correct: the first *rendered* group is the
**overscan** one, which starts above the viewport. Conflating "first rendered"
with "first visible" is the mistake, and asserting it would have encoded a false
claim. It is now the claim worth making:

```ts
// every group that overlaps the viewport is rendered
for (const gi of overlapping) assert.ok(w.groupIndices.includes(gi));
```

A test that pins a wrong invariant is worse than a missing test, because it
survives the fix and fails after it.

## 5. Two component bugs, both silent

**`{@const}` inside a nested `each` is a runtime error.** Rewriting the loop to
index the window put `{@const rowWindow = ...}` where it referenced a name the
`{#each}` did not declare, and inside a `<section>` rather than as an immediate
child of the block. The build succeeded, the route returned 200, and the console
said `rowWindow is not defined`. The whole wall was blank with no compile error.

**The renamed loop variable.** `{#each groups as g}` became
`{#each window.groupIndices as gi}` while the body still said `g.indices`. The
build is happy; the surface is empty. Both are recorded in the harness skill,
because both have the same signature — *a green build and an empty page* — and
the fix for each is a console probe, not a re-read of the source.

Also: the first attempt looked up the row window with
`window.rowWindows[window.groupIndices.indexOf(gi)]` inside the template. That is
O(n²) over the rendered set and a re-derivation of an answer `wallWindow`
already returned in order. It is now positional (`as gi, slot`).

## 6. Results

### Unit — `ui/tests/wall-window.test.ts`, 21 tests

| group | the load-bearing claim |
|---|---|
| `groupLayout` | offsets stack with the gap; no trailing gap; `loadedRows` separate from reserved `rows` |
| `wallWindow` | the rendered set is **bounded** independently of group count |
| | the window **moves** when the scroll does — a window, not a truncation |
| | every group **overlapping** the viewport is rendered |
| | overscan keeps a part-scrolled group |
| | a group far down still renders rows (the local-offset bug) |
| | `last <= loadedRows` at five scroll positions |
| | a zero-height viewport renders the first group, not nothing |
| | a negative scroll offset (macOS overscroll) yields no negative row |
| `at scale` | 5,000 rows: a bounded tile count |
| | **every group is reachable** by scrolling to it |

The last one is the test that matters most. A window that renders only the top
groups passes every count assertion above; walking the whole wall and checking
each group renders when the viewport is on it is what rules that out.

### Browser — `ui/e2e/wall.spec.ts`, 4 new

| claim | why only a browser |
|---|---|
| a bounded tile count for 5,000 rows | real DOM, real layout |
| **the rendered tiles change on scroll** | a window, not a truncation |
| no on-screen group is empty | the local-offset bug as seen |
| `data-rendered-groups < data-groups` | the bound, readable without counting the DOM |

## 7. Mutations — 41 total, 41 killed, 0 survived, 0 stale

The 13 added for windowing, and the ones worth naming:

- `wallWindow` renders every group (the `break` made conditional) → killed.
- **`wallWindow` uses the wall's scroll offset for the row window** → killed,
  by the far-down-group test.
- `wallWindow` ignores the viewport height → killed.
- `wallWindow` lets `first` run past the end of its group → killed, by the
  invariant test at five positions.
- `rowHeightOf` ignores the gap between rows → killed.
- `groupLayout` counts pending rows as loaded → killed.
- `groupLayout` puts every group at the same top → killed.
- the binary search converges on the wrong end → killed, by the far-down test.

Two of the script's own patterns had to be fixed to get here, both the same
class of error as item 14's stale-mutant bug: a **multi-line** pattern in a
single-quoted JS string is a syntax error, not a pattern. The script did not
parse at all, which is louder than item 14's silent version and cost one round
each.

## 8. What is still open

- **Server-side grouping.** The `GroupKey` switch is client-side, over the loaded
  page. A real group-by is a query parameter and a `GROUP BY`, and belongs with
  T-P6-007. Unchanged from item 14.
- **Windowing is per group, so a single group taller than the viewport** still
  renders all its loaded rows. The `rowRange` slice bounds what a group
  contributes to a screenful; a 50,000-row group contributes one screenful per
  viewport, which is correct, but the *placeholder* height for a pending group
  remains the median rather than an exact future.
- **The other two plan items** are folder view (#1586) and unified media view
  (#1030).
