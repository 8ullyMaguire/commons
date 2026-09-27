<!--
  The vertical feed: one item per screen, swiped, autoplayed. §10.6, #3859.

  # Why the position is an index into `items` and not a cursor

  The spec says a feed, and the obvious implementation is "keep the array, keep
  an index into it". For a list that is fine; for this it is wrong in a way
  that is invisible until the array is large:

  * An index into an array means everything before it is retained, so a
    100,000-item feed holds 100,000 items in memory. §4.2 says a virtualized
    view holds no more than a window of rows, and the feed would be the one
    view that quietly violates it while looking like the view that could not.
  * An index is not stable across a filter change. After a reload the index
    means something else, so the feed either jumps or shows the wrong item.

  So: the index is clamped into range on every render (`nextFocus` does the
  clamping), and the caller passes in a *window* of items with `focusIndex`
  relative to that window. The e2e spec asserts the rendered item count stays
  bounded, which is the claim that makes the above more than a comment.

  # What composes and what does not

  `classifyCommit` from `feed.ts` decides, and it is the same pure decision the
  unit tests cover. The pointer handling here is only: record the press, record
  the release, hand the two to `classifyCommit`. No accumulated deltas, no
  threshold inline — a dropped `pointermove` cannot make a 400px drag measure
  as 4px, because there is no per-move accumulation to drop.

  `gestures.ts`'s `classifyDrag` is NOT reused, and the reason is worth stating
  because it looks like an oversight. `classifyDrag` is horizontal (it tests
  `dx` dominance) because it belongs to the lightbox, where the flick is a
  horizontal pan of one image. The feed's flick is the same gesture with the
  axis swapped, and its pan/flick boundary means something different: in the
  lightbox a drag past the slop is a pan to preserve, here a drag past the
  threshold is a page change. Reusing a recognizer whose semantics differ is
  how a lightbox and a feed end up disagreeing about what a flick is; the
  shared piece is the *idea* (press/release samples, pure classify), and the two
  files each spell it for their own axis.
-->
<script lang="ts">
  import { classifyCommit, isPlaying, nextFocus, preloadAttr } from '$lib/api/feed.js';
  import type { ObjectRow } from '$lib/api/client.js';

  /**
   * The window of items the feed is showing, and where the focused one sits
   * inside it. `focusIndex` is relative to `items`, not to the whole library.
   */
  interface Props {
    items: ObjectRow[];
    focusIndex?: number;
    /** Called with the next focus index once a drag commits. */
    onfocuschange?: (next: number) => void;
  }

  let { items = [], focusIndex = 0, onfocuschange }: Props = $props();

  // A press and a release, and nothing between. See the header: no accumulated
  // deltas means no dropped-event failure mode.
  let pressY: number | null = $state(null);
  let viewportH = $state(0);

  const focus = $derived(nextFocus(focusIndex, 0, items.length));

  function onPointerDown(e: PointerEvent) {
    // `primary` and not `button`: a right-click drag should not navigate a
    // feed, and `button` is 0 for the primary button on both mouse and touch.
    if (!e.isPrimary || e.button !== 0) return;
    pressY = e.clientY;
  }

  function onPointerUp(e: PointerEvent) {
    if (pressY === null) return;
    const dy = e.clientY - pressY;
    pressY = null;
    const verdict = classifyCommit(dy, viewportH);
    if (verdict.kind === 'commit') {
      // Dragging the content up reveals the NEXT item; down goes back.
      onfocuschange?.(nextFocus(focus, verdict.axis === 'up' ? 1 : -1, items.length));
    }
  }

  /** The `src` for an item: a still is an image, a clip is a video. */
  function mediaSrc(row: ObjectRow): string {
    // `coverPath` for both, and `<video>` for both, is what a browser decides
    // to do with the content type — the route sets it. A separate `videoUrl`
    // field does not exist in `ObjectRow` and inventing one here would mean the
    // component knows something the query does not.
    return row.coverPath ?? '';
  }

  /** A still renders as a still, and does not "fail to play". */
  function isVideo(row: ObjectRow): boolean {
    return row.durationMs != null && row.durationMs > 0;
  }
</script>

<div
  class="feed"
  data-testid="vertical-feed"
  bind:clientHeight={viewportH}
  onpointerdown={onPointerDown}
  onpointerup={onPointerUp}
>
  {#each items as row, i (row.id)}
    <div
      class="slide"
      data-testid="feed-slide"
      data-focus={i === focus ? 'true' : 'false'}
      data-playing={isPlaying(i, focus) ? 'true' : 'false'}
      data-index={i}
    >
      {#if isVideo(row)}
        <video
          src={mediaSrc(row)}
          data-testid="feed-video"
          preload={preloadAttr(i, focus, items.length)}
          muted
          playsinline
          autoplay={isPlaying(i, focus)}
          loop
          disableremoteplayback
        ></video>
      {:else}
        <img src={mediaSrc(row)} alt={row.title} data-testid="feed-still" loading="lazy" />
      {/if}
      <p class="title">{row.title}</p>
    </div>
  {/each}
</div>

<style>
  .feed {
    /* A feed is a stack of screens, not a scrolling page: `overflow: hidden`
       plus a flex column of full-height slides. `scroll-snap` would be the
       obvious alternative and is deliberately not used — snapping fights the
       drag-commit rule, because the browser then animates to the nearest snap
       point whether or not the threshold was crossed, and a drag of 29% that
       springs back would still settle on the next item. */
    height: 100dvh;
    overflow: hidden;
    display: flex;
    flex-direction: column;
    /* `100dvh` rather than `100vh`: on mobile, `vh` includes the area behind
       the URL bar, so a `100vh` slide is taller than the visible screen and the
       focused item's bottom edge is under the bar. `dvh` is the dynamic
       viewport, and this is the whole reason the feed is specified for phones. */
    touch-action: pan-y;
    user-select: none;
  }

  .slide {
    flex: 0 0 100%;
    height: 100%;
    position: relative;
    display: grid;
    place-items: center;
    overflow: hidden;
  }

  .slide img,
  .slide video {
    max-width: 100%;
    max-height: 100%;
    /* Full bleed is "cover", not "contain": §10.6 says full-bleed, and this is
       the one view where cropping is right, because a portrait clip in a
       portrait frame has nothing to crop. */
    object-fit: cover;
  }

  .title {
    position: absolute;
    inset-inline: 0;
    bottom: 0;
    margin: 0;
    padding: 0.75rem 1rem;
    /* A scrim, not a solid bar: the title has to be readable over an arbitrary
       frame of arbitrary video, and no fixed colour is readable over all of
       them. */
    background: linear-gradient(transparent, rgb(0 0 0 / 0.75));
    color: var(--fg);
    font-size: 0.9375rem;
  }
</style>
