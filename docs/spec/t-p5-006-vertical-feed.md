# T-P5-006 item 9 — The vertical view (§10.6, #3859)

**Plan entry:** `docs/plans/implementation-plan.md` §T-P5-006 item 9
**Spec:** §10.6 (C59) — "The TikTok-style full-bleed vertical feed (#3859), which
is how a large amateur library is actually browsed on a phone, with swipe,
autoplay-on-focus, and preloading of the next few items."
**Preceded by:** item 8 (`docs/spec/t-p5-006-view-modes.md`).
**Module:** `ui/src/lib/api/feed.ts`, `ui/src/lib/components/VerticalFeed.svelte`.

---

## 1. What this ticket is, and the thing it turns out to need first

Four features are named: a full-bleed vertical feed, swipe, autoplay-on-focus,
and preloading the next few items. The last one is the load-bearing part, and
it forces a measurement before any of the others can be written.

**Preloading requires something to preload.** A feed preloads the *next item* —
the next page's media. Today a `coverPath` is a thumbnail, and there is no route
that serves original media at all: `commons-server`'s router is
`/healthz`, `/livez`, `/metrics` and nothing else (`crates/commons-server/src/lib.rs:76`).
There is no `/media`, no file route, no byte-range support. So "autoplay" today
would play a WebP thumbnail, and "preload the next few" would preload more of
them.

That is the honest scope statement, and it is the second time in this ticket
that §10.4/§10.6's surface list described a small UI feature on top of
infrastructure that does not exist (item 8 found the same about sorting). The
feed is the *easy* half. What it needs underneath is a media route that:

- serves a file by object id, **not by path** — a path in a URL is a path a user
  can edit, and §5.18 is explicit that the operator's disk is not a public API;
- supports **HTTP range requests**, because `<video>` seeks with them and
  without them a scrub bar either does nothing or re-downloads the whole file on
  every seek;
- runs the **consent check** before reading a byte. A file route that does not
  check is the single largest hole this codebase could have, and it is a hole
  that costs one `CallerId` at the call site and is nearly impossible to retrofit
  later — every caller must then be re-audited.

So the item is two commits in order: the media route, then the feed. The route is
built first because a feed that cannot load media cannot be tested, and a test
that asserts on a mocked URL is a test of the mock.

## 2. The decision that shapes the feed: a feed is a *window*, not a list

A vertical feed is one item per screen, so it is trivially virtualized — one
item is visible. The naive implementation is "keep an array, index into it", and
that is wrong in a way that is invisible until the array is large:

- an index into an array means every item before the current one is retained, so
  a 100,000-item feed holds 100,000 items in memory, and §4.2 says a virtualized
  view holds no more than a window of rows. The feed would be the one view that
  quietly violates it;
- an index is also not stable across a filter change. After a reload the index
  means something else, and the feed either jumps or shows the wrong item.

**So the feed's position is the item's own id, and the window is derived from
it.** The store's keyset cursor (item 8) already gives "the page after this
cursor", so moving forward is a cursor advance and moving backward is a cursor
the feed kept. There is no array index anywhere in the component, and the test
that matters is that a 100,000-item feed's retained rows stay bounded.

## 3. Swipe: the same gesture as the lightbox, with the axis inverted

`gestures.ts` already has `classifyDrag`, from the lightbox's #7147 work. It is
pure — the press and the release, no accumulated deltas — and that purity is the
fix for a dropped `pointermove` making a 400px drag measure as 4px. The feed
must **reuse it**, not write a second recognizer, for the same reason
`ImageGrid` composes `VirtualGrid` rather than being a grid: a second
implementation of gesture recognition is a second set of thresholds to tune and
a second bug to have.

The change the feed needs is the **axis**. A lightbox flick is horizontal
(`dx` dominant) and a feed flick is vertical (`dy` dominant), and a
`classifyDrag` that reports `pan` with a `dx`/`dy` pair is usable for both if
the *caller* decides which axis is the navigation axis. The decision is not
optional, though, because the pan/flick boundary means opposite things:

- in the **lightbox**, a drag past the slop is a *pan* — the user is moving the
  image and stopping at the edge does nothing;
- in the **feed**, a drag past the slop is a *pan* the feed applies as a
  transform while the finger is down, and it **commits to the next item on
  release** if the drag was long enough. There is no partial state to preserve:
  the next item is a different page, and a half-swiped page is not a page.

So `classifyDrag` stays untouched and the feed adds `commitThreshold`: the
fraction of the viewport a drag must cross to move. That number is the whole of
the feed's swipe feel, and it is a constant in one place with a test at, just
under, and just over it — the same boundary discipline as the flick thresholds
already in the file.

**The failure this prevents:** a feed where a small drag navigates feels broken
in a way that is hard to report, because "it jumped" is not a bug anyone can
reproduce deliberately. A drag that crosses 30% of the screen moves; one that
crosses 29% springs back.

## 4. Autoplay-on-focus, and the one rule that is not negotiable

Exactly one item plays. Not the focused one and not the one being preloaded —
only the one at the exact focus index. The reason is bandwidth and battery, and
§10.8 (C61) asks for both: three simultaneously playing videos in a feed is a
phone at 4% battery an hour, and the user cannot see two of them anyway.

**Autoplay is muted and it is `playsinline`.** Not a style choice:

- unmuted autoplay is refused by every current browser, so a feed that tries it
  silently does nothing on the platform the ticket names (phone). The muted
  attribute is not a workaround, it is the only spelling that works;
- `playsinline` stops iOS Safari taking the video fullscreen, which would
  navigate away from the feed and lose the position. §10.8 asks for mobile web
  as a supported layout (#771, #6335) and this is the attribute that makes it
  one.

**A still image in the feed is not a video that failed.** Item 8's `tileShape`
already answers "what is this" from a duration; the feed reuses it, so a still
renders as a still at full bleed and the rule "one item plays" becomes "one item
*may* play".

**And focus is a function of the scroll position, not of a timer.** An
autoplay feed that advances on its own is a slideshow nobody asked for; the item
under the viewport centre is the focused one, and swiping is how you change it.

## 5. Preloading: the next *two*, and why not more

`PREFETCH_AHEAD = 2`. Three is the number people reach for and it is wrong here:
preload is a *request*, and a feed the user swipes through quickly issues one
request per item. At three ahead, a fast swipe through a video feed queues
downloads for items already three screens past, and on a phone that is
measurable data spent on nothing. Two is enough to cover a normal swipe
(autoplay starts before the swipe finishes) and is the number the test pins.

Preload means `preload="auto"` on the next items' elements and **nothing else** —
no fetch, no Range request, no prefetch of the byte range. The browser's own
preloader is already the right implementation, and a hand-rolled one is a second
HTTP client with its own cache and its own bugs.

## 6. Acceptance

`feed.ts` (pure, no DOM):
- `nextFocus(current, delta, total)` clamps at both ends — no wrap. A feed that
  wraps from the last item to the first is a carousel, and §10.6 says feed.
- `commitThreshold`: a drag at, just under, and just over it.
- `preloadFor(focus)`: exactly the items at `focus+1` and `focus+2`, and
  nothing before `focus` or beyond `focus+2`.

`VerticalFeed.svelte` (Playwright):
- one item fills the viewport and is the only one playing;
- a swipe past the threshold advances focus; a swipe short of it does not;
- the retained row count stays bounded on a 100,000-item feed;
- the focused item's element carries `muted` and `playsinline`.

The media route (Rust, both engines where it touches the store):
- a range request returns 206 and the right bytes, not the whole file;
- an object the caller may not see is 404, not 403 — the existence of a file is
  itself consent information.

## 7. Not in this ticket

- **Video scrubbing and quality selection** are §10.5 / #5067 / #2397.
- **Group-by and auto-scroll in the wall** (#6544, #6955) stay out: auto-scroll
  is a playback behaviour and belongs with this view, but it is a different
  feature from a feed.
- **A real byte-range media route for archives.** An object backed by a zip
  needs extraction; this ticket serves single files and says so, rather than
  half-implementing archives.
- **The full §5.18 locator/federation story.** A media route that serves local
  files is the first step; the remote case is its own ticket.

## 8. What the implementation found

To be written once the code lands.
