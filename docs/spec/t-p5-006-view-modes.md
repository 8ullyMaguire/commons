# T-P5-006 item 8 — View modes: tag view, secondary sort, per-user density

**Plan entry:** `docs/plans/implementation-plan.md` §T-P5-006 item 8
**Spec:** §10.4 (view modes, C57)
**Preceded by:** item 7 (`docs/spec/t-p5-006-undo.md`).
**Module:** `ui/src/lib/api/view.ts`, `ui/src/lib/api/media-view.ts`,
`ui/src/lib/components/TagView.svelte`.

---

## 1. What this ticket actually is

§10.4 lists eight things: wall with group-by and auto-scroll (#6544, #6955),
rich list tables (#517, #899), folder view (#1586), unified media view (#1030),
tag view (#773), secondary-sort correctness (#7068, #1508), per-user density,
and virtualization throughout.

Six are already built under earlier items — the wall is `ImageGrid` +
`VirtualGrid`, the list table is `ListTable`, folders are item 6, and
virtualization runs through every view already. So this item is the remaining
three, and they turn out to be one thing rather than three.

**The finding that shapes the item: a "unified media view" and a "tag view" are
the same view with a different tile.**

Both are a grid over objects where the tile's shape is derived from the object's
own media rather than from a fixed poster ratio. The unified view (#1030) is the
complaint that a library of mixed stills and video renders every tile as 2:3
posters, so portraits get letterboxed and widescreen gets cropped. The tag view
(#773) is the same grid with the tile showing the tag's own image. One component
with a tile-shape function covers both, and the density question (§10.4's
"per-user density") is the same question asked of tile size.

So the item is: **derive the tile shape from the row, and make the derivation a
pure function that can be tested without a DOM.**

## 2. The decision that shapes everything

There are two ways to size a tile for a row of unknown media.

**(a) Square tiles, object-fit: cover.** Every tile is one size, the image is
cropped to fill it. The grid is a regular lattice, the row height is a constant,
and virtualization is trivial. Cropping is the cost: a 16:9 video thumbnail in a
square tile loses a third of itself, and the part that is lost is usually the
centre of the frame.

**(b) Proportional tiles, one aspect ratio per row.** The row height is the
tallest tile in the row and the others are centred within it. Nothing is
cropped, and the grid is still virtualizable — but only if "the row" is a real
concept the virtualizer can compute, which means the row height depends on which
rows are in it, which depends on the layout, which is the thing virtualization
exists to avoid computing.

(b) is what this ships, and the reason is worth stating because it looks like the
expensive choice: **a cropped thumbnail is a lie about the media.** A person
scanning a wall of tiles is choosing what to open, and a tile that has silently
removed 30% of the frame is a tile that cannot be scanned. The cost is real but
it is bounded and it is paid in one place — `rowHeightFor` — rather than in
every tile.

The trap is the interaction with virtualization, and it is the reason this file
exists rather than three lines in a component: **a proportional row height makes
the row height a function of the row's contents, so the virtualizer's window
depends on data it has not loaded.** `VirtualGrid` computes its window from a
fixed row height today. If the height varies, the scroll math is wrong, and the
symptom is not a crash — it is a grid that scrolls to the wrong place, which
reads as "the app is broken" rather than "the row height is dynamic."

The resolution is a **declared** row height, not a computed one: the view
declares a target aspect ratio, and each row's height is that ratio applied to
its own width. A row's height therefore depends only on its own tile width, which
the virtualizer already knows, and the window stays computable without loading
the rows inside it. Rows of genuinely mixed media get a slightly ragged bottom
edge inside one row, which is the honest rendering and is what every photo
browser does.

## 3. The tile-shape function, and why it is separate

`media-view.ts` exports one pure function:

```ts
export function tileShape(row: ObjectRow, target: number): TileShape
```

returning `{ kind: 'image' | 'video' | 'audio' | 'unknown', aspect: number,
playable: boolean }`. It is a pure function of a row and a target ratio, with no
DOM, no fetch, and no store — which means the whole decision surface of the view
is testable in a unit test, and a bug in it shows up as a number rather than as
a screenshot.

Three properties it must have, each a test:

- **A row with no dimensions gets the target ratio, not zero.** `width: null` is
  a real row — a scan that has not run yet — and dividing by it is how a single
  unprobed object makes the entire grid render at zero height.
- **A degenerate ratio is clamped.** `height: 0` or a 10000:1 strip must not be
  allowed to become the row height; the clamp is what stops one malformed row
  from making the viewport 40000 pixels tall.
- **The aspect is width ÷ height, consistently.** `VirtualGrid` already has a
  comment about getting this backwards once, and a wrong direction here is
  invisible in a test that only checks the ratio is positive.

**What decides video from image is `durationMs`, not the file extension.** An
extension is a claim about a filename; a duration is a fact about the content.
The row already carries `durationMs`, so the question is answered without
guessing, and a `.mp4` that failed to probe is `unknown` rather than being
rendered as a video that will not play.

## 4. Secondary sort (#7068, #1508)

**Measured first, and the measurement invalidated the section I had written.** I
specced this as a *correctness* fix — a keyset cursor carrying `(sort, id)` where
it must carry `(primary, secondary, id)`, producing repeats and skips at page
boundaries. That is a real bug shape, but there is no such cursor here:

- `Store::query` (`crates/commons-store/src/query.rs:178`) hard-codes
  `ORDER BY o.date DESC, o.id`. The `sort` string in `GridQuery` is never sent,
  never read, and has no server-side meaning.
- `Store::query` takes `(filter, caller, limit)` — **no offset and no cursor**.
  `ObjectPage` is `{ rows, has_more }`. The "keyset cursor" in
  `ui/src/lib/api/keyset.ts` is a client-side row list with a dedup set, not a
  seek position.

So the honest scope is: **there is no sort and no server-side pagination at
all.** Secondary sort cannot be a fix; it requires the primary sort, and both
require the pagination. The ticket's own wording ("secondary-sort correctness")
is the kind of phrase that presumes the rest of the stack exists, and here it
does not — which is the second time in this item that §10.4's surface list
described something as a small fix to something that was not built.

This is not a reason to shrink the item. It is a reason to build the sort as a
total order from the start, because the expensive mistake is building a single-key
sort now and discovering the cursor problem later, on top of a working query.

**The design, and the part that is not optional:** the sort is a *tuple* of keys,
always ending in `id`. `ORDER BY <k1> <dir1>, <k2> <dir2>, o.id ASC` — the trailing
`id` unconditionally, with its own fixed direction, is what makes the order total
and therefore makes a cursor correct. A secondary key with *its own* direction
spells `[{"date":"DESC"},{"rating":"ASC"}]`; the `id` is not a key the user
chooses, because a user-chosen `id` direction would break the tiebreak that the
id exists to provide.

Keyset pagination (`WHERE (k1, k2, id) > (?, ?, ?)`) needs row-value comparison,
which **Postgres has and SQLite does not** — SQLite's row values compare
lexicographically as blobs, and `>` on a row value is not the tuple comparison
the ORDER BY implies. The portable spelling is the explicit three-way form, which
is the same trap the undo CTE just walked into: the obvious SQL is the one engine
only. The test that matters is therefore not "sorting works" but **a key with more
ties than fit in one page, asserting page 2 repeats no id from page 1** — the
boundary is the only place a non-total order shows.

## 5. Per-user density

Density is already in the URL (`ViewState.density`, default 240). "Per-user"
means it is remembered between sessions, which the URL cannot do — §5.16 says
view state lives in the URL, and a URL cannot be a preference.

The resolution that keeps both rules: **the URL is authoritative while the user
is moving the control, and the stored preference is the default the URL is
compared against.** Opening a shared link with `?density=400` shows 400; opening
the bare URL shows whatever the user last chose. The stored value is a default,
not an override — a link that carries a density must not be defeated by a
preference, because a shared link that renders differently on the recipient's
machine is not a shared link.

## 6. Acceptance

- `media-view.ts`: the three properties in §3, plus a test that the aspect
  direction is width ÷ height.
- `view.ts`: `secondary` survives `encodeView`/`decodeView` round-trip.
- Secondary sort: a page-boundary test with a tied key (§4).
- Density: a test that an explicit URL density beats the stored default (§5).
- `TagView.svelte`: a Playwright spec per the ticket's accept line.

## 7. Not in this ticket

- The group-by and auto-scroll half of the wall (#6544, #6955) — auto-scroll is
  a playback behaviour and belongs with the vertical view in item 9.
- Vertical feed (#3859) is item 9 and is a different view entirely, not another
  tile shape.
- Server-side secondary sort: the sort tuple is compiled into the query here and
  the store already takes a sort string, so no migration is needed. If a sort
  key ever needs an index that does not exist, that is a new migration and a new
  ticket.

## 8. What the implementation found

**Both first-run test failures were real, and they were the same bug in two
places: I was reading evidence from one field when the row carried two.**

`kindOf` originally branched on `coverPath` to decide a row was an image. A
still with measured `width`/`height` but no thumbnail — a cover that has not
been generated, or a format the thumbnaller skipped — came out `unknown` and fell
back to the declared ratio as though nothing were known. Dimensions are
themselves the product of a probe and count as evidence, so the check is now
either one. The symmetry matters: the function has *two* signals (a cover, a
duration) plus the dimensions, and treating one as necessary when another
suffices is the bug both failures were reaching for.

The second failure was mine, and the honest reading is that the test was wrong
and not the code. `audio` was reachable on `durationMs === 0`, which looks like a
zero-length audio file and is actually a probe that **failed** — a failed scan
also reports 0. So a failed video probe rendered as playable audio. The branch is
gone. Nothing in `ObjectRow` distinguishes an audio file from a video one: `kind`
is a Scene label, not a container — so any audio handling here would be a guess.
`audio` stays in the type, unreachable, and the comment says why, because the
alternative is removing a variant the lightbox will need the moment the row
carries a container.

**The test I wrote was the thing that was wrong**, twice over: it asserted that a
zero-duration row with dimensions is `unknown`, when in fact it is a still with
nothing to play — the failure to measure a duration says nothing about what the
file *is*. The `unknown` case is narrower than I assumed, and the test now pins
the real one: no duration, no cover, no dimensions. A test that encodes my
guess rather than the behaviour is the one that has to change, and the instinct
to "fix the code to match my test" would have made it worse.

**A dead variant is worse than a missing one.** Leaving `audio` in the union with
a comment beats deleting it: the next reader sees the gap *and* the reason, and
adding a variant that the lightbox will need anyway costs one line. A union with
no unreachable members is a claim that the codebase has no unanswered questions,
which is not true and is not something a type should assert.

**What this did not need.** No store change, no migration, no API change. The
spec assumed a media view would need the store to expose media facts; the row
already carried `kind`, `width`, `height` and `durationMs` from the existing
GraphQL selection, because the grid needed them for a poster layout. The
assumption was wrong in the cheap direction — which is the only good direction,
and is a reminder to check the existing selection before speccing a new one.

**And, from the sort and the grid: two of my own tests were the broken ones, and
one of them was load-bearing for nothing.**

The clamp mutation SURVIVED the first five Playwright tests, all green. The
reason is structural and worth stating because it is the shape of the whole
item: the row height comes from the *declared* ratio, not from the tile's own
aspect, so deleting the `Math.min`/`Math.max` in `clamp` leaves a 10000:1 strip
letterboxed inside a perfectly normal-looking row. **The clamp is not
load-bearing for layout — it is load-bearing for the number the tile reports and
for anything that multiplies by it later.** So the assertion had to move to
`data-aspect`, which is the only place the clamp is observable from outside. A
test that passes while the behaviour it names is deleted is worse than no test,
because it is a claim of coverage.

**The spec's §2 prediction held, and the cost was paid where it said it would
be.** Proportional tiles could not go into `rowHeight` — the virtualizer's
window depends on it — so the row keeps the declared ratio and the *tile* is
proportional inside it. The result is that `clamp` is nearly unobservable, which
is the price, paid in testability rather than in correctness.

**Two debugging findings that are now comments in the tests, because both cost a
round each.** A cover that 404s renders as a zero-size broken image, so every
geometry assertion measures nothing — a test that measures geometry needs
geometry. And `expect(tiles.first()).toBeVisible()` resolves as soon as one tile
has a box, which can be *before* the row element is in the DOM, so
`evaluateAll` over rows returns `[]` rather than waiting. Wait for the row, not
the tile.

**The tag view needed a query that did not exist, and the reason it was a LEFT
JOIN is the whole design.** `all_tags` returns every tag and no count, and the
count is what makes the grid a view rather than a list. But the count must come
from a LEFT JOIN: an inner join drops every tag nobody has applied, and a tag
created and never used is exactly the one somebody reorganising a library wants
to see. Then `COUNT(ot.object_id)`, not `COUNT(*)`, because a `COUNT(*)` over a
LEFT JOIN reports **1** for a tag with no objects — off by one on precisely the
rows the LEFT JOIN was written to preserve. Both mutations are killed by the
same two tests, which is the correct shape: a test that pins the zero case pins
the join and the count together, because they are one decision.

**The count is deliberately NOT consent-filtered, and that is a judgement call
worth flagging.** §14.1 governs what a caller may *see*, not what exists. A
count that dropped unverified items would make a heavily-used tag look unused,
and the person reorganising would delete it. Clicking a tile still goes through
the normal filtered query, so visibility is never wrong — only the library's own
arithmetic is. If the owner disagrees, this is the line to change, and it is one
query rather than a policy.

