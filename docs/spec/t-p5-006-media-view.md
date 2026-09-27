# T-P5-006 item 17 — The unified media view

**Plan entry:** `docs/plans/implementation-plan.md` §T-P5-006 item 17
**Spec:** §5.17 (organization views), §10.4 (view modes), §10.5 (play from grid)
**Closes:** #1030 (a "Media" tab combining scenes and images)
**Preceded by:** item 8 (`ui/src/lib/api/media-view.ts`, tile shape — a
different file, and the boundary between them is a real design decision, §5)

---

## 1. The model was already unified

§5.3 says "Image is an Object, so it votes like one", and §5.17 lists "media view
unifying scenes+images (#1030)" as an organization view. So the first question is
whether anything needs building underneath, and the answer is no:

- `Object.kind` is a `TEXT` column on the single `object` table, indexed
  (`0001_core.sql:15,27`);
- `ObjectKind` is an enum in `commons-core/src/enums.rs` with `Scene` and `Image`
  among its seven values;
- `CmpOp::In` exists in `filter_ast.rs` and compiles a value list.

So a media view is **one facet**: `kind IN ('scene', 'image')`. No join, no union
table, no second identity to reconcile. Every operation that works on a scene
already works on an image, which is the point §5.3 was making.

That is why this is a small item. The interesting content of #1030 is not
"show two kinds together" — that is one `In` — it is the places where a scene and
an image *cannot* be treated identically.

## 2. `media.ts` is not `media-view.ts`

There are two files with confusingly similar names, and keeping them apart is a
decision rather than an accident:

| | `media-view.ts` (item 8) | `media.ts` (item 17) |
|---|---|---|
| Question | what shape is this tile? | is this row in the view, and where does it sort? |
| Exports | `tileShape`, `ASPECT_LIMITS`, `POSTER_RATIO` | `MEDIA_KINDS`, `isPlayable`, `compareMedia`, `auditMedia`, `mediaFilter` |
| Knows the tile's pixel box | yes | no |

Duplicating the ratio logic would give two copies of `ASPECT_LIMITS` that
disagree within a release, and the disagreement shows up as a wall whose images
and scenes do not line up. `tests/media.test.ts` asserts `media.ts` exports **no**
ratio function of its own, so the boundary cannot erode quietly.

### 2.1 The one deliberate disagreement

Item 8's tile-level `isPlayable` requires a strictly positive `durationMs`, for a
good reason: a failed probe reports `0`, and a tile that offers to play a
zero-length video opens onto nothing.

Item 17's membership-level `isPlayable` is true for any scene or audio row,
because **duration is metadata, not capability** — and this module has a fact
item 8 does not: the kind. A scene whose probe failed is still a video.

The two coexist, and a test names the disagreement explicitly
(`item 8 and item 17 disagree about a zero-duration scene on purpose`) because
"why do these two functions contradict each other" is otherwise a question
nobody can answer from the code.

## 3. The wire shape is checked, not assumed

`view.ts` is explicit: the filter is "one opaque, versioned, base64url-encoded
string... it is the thing the server parses", and no UI code parses it. So the
Media tab has to **build** one, and serde's external tagging has three details
that are all invisible if you write the obvious thing:

```json
{"facet":{"kind":null,"field":{"builtin":"kind"},"op":"in",
          "values":[{"str":"scene"},{"str":"image"}]}}
```

- `field` is `{"builtin":"kind"}`, not `"kind"`;
- a value is `{"str":"scene"}`, not `"scene"`;
- `op` is `"in"`, not `"In"`.

Getting any of them wrong does not render badly — the server rejects the filter,
and the tab looks like an empty library.

So the shape is **emitted as data from Rust** and compared on both sides:

- `crates/commons-store/tests/filter_wire_shape.rs` asserts the string and that
  it round-trips through `serde_json`;
- `ui/tests/media.test.ts` reads that Rust file and fails if its own copy has
  drifted.

A change to serde's attributes now fails a test rather than a tab.

## 4. The known limitation: the tab replaces `?q=`

Because the filter is opaque, a `kind` restriction cannot be ANDed onto a user's
`?q=cat` without decoding it — which is the thing the design forbids. So the tab
**replaces** the URL's filter, and `?q=` is not honoured.

This is a real gap, not a shortcut, and it is the first thing to fix when the
filter codec moves to the client (spec §5.16 wants the whole AST round-tripping
through the URL). What the page does instead of hiding it: `mediaFilter` replaces,
`looksLikeFilter` decides whether a hand-edited `q` is worth carrying, and both
the route header and this document say so plainly.

## 5. What is in the tab, and what is not

`MEDIA_KINDS = ['scene', 'image']` — an explicit list, not `KINDS.filter(media)`:

- **gallery is excluded** because a gallery is a *container*. A Media tab listing
  galleries lists things twice: once as the container, once as the images inside
  it. That is the specific way "unified" goes wrong.
- **text, audio, comic, interview are excluded** because a Media tab containing
  them is a Library tab that lost the plot.

The one constant decides what the tab means; the route reads it rather than
repeating the list.

## 6. Ordering across kinds (#7068, #1508)

Both issues are about a secondary sort that is only defined *within* a kind, which
interleaves two kinds in an arrangement that changes as rows arrive.

`compareMedia` makes the tie total: kind in a **fixed** order, then the id. The
id tiebreak is not decoration — without it the comparator is not a total order,
and rows that tie exchange places depending on which page boundary they landed
on. With it, **adding a row cannot move the others**, which is the property the
tests assert.

The comparator runs *after* the server's sort has put the primary field in order,
so kind and id only ever break ties. That division of labour is what makes the
cross-kind tiebreak safe: it is not re-sorting, it is making a tie total.

## 7. The audit: the load-bearing part

`object.kind` is unconstrained `TEXT` in the database and an enum in
`commons-core`, and **nothing at the boundary keeps them in sync**. So an unknown
kind is an ordinary event, not corruption.

A grid that drops those rows looks correct and silently loses content — the one
failure a user cannot diagnose from the screen in front of them. So:

- `auditMedia` reports `unknownKinds` (distinct spellings, not per row) and the
  count of rows it could not place;
- the route renders that as a status line naming the kind, because "some items
  are missing" is unactionable and `hologram` is a bug report;
- `KINDS` is asserted equal to `ObjectKind::as_str` **by reading the Rust file**,
  so a kind added in Rust and not here fails a test;
- an unknown kind keeps its wire spelling as a section label, because "undefined"
  tells the user nothing.

"Excluded by a narrower tab" and "this build cannot place it" are kept distinct:
the first is what was asked for, the second is a bug.

## 8. Verification

```
targets ok: 76, tests passed: 1310, tests failed: 0
ℹ tests 661  ℹ pass 661  ℹ fail 0        # 65 new
156 passed                                 # e2e, 10 new in media.spec.ts
29 killed, 0 survived, 0 stale (of 29)
```

The e2e's load-bearing test asserts the **filter the client actually sent**,
parsed twice — once for the GraphQL variables, once for the filter string inside
them. The first version string-matched the raw POST body, where the filter is
escaped (`\"builtin\":\"kind\"`) and a correct filter never matches; the failure
read as "the filter is wrong" when the filter was perfect.

## 9. Follow-ups

1. **§4: the tab replaces `?q=`** until the filter codec moves client-side. The
   first real gap in this item.
2. **No `kind` group-by.** `GROUP_KEYS` in `wall.ts` lists *data* fields, and
   adding `kind` there would offer a no-op grouping on every other tab. A
   scenes-vs-images split needs a real grouped wall, which is item 14/15's code
   with a `kindSection` key.
3. **Server-side counts by kind** for a "412 scenes, 1,933 images" summary.
   `byKind` is ready for it and is already keyed in facet order.
