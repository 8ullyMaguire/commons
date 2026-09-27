<!--
  The tag view: every tag in the library as a grid of tiles (spec §10.4, #773).

  # What this is, and why it is not a filter

  "Display all tags in a single page" reads like a list of names, and a list of
  names is what `all_tags` already gives. The thing that makes it a *view* is
  the count on each tile: a tag on 40,000 objects and a tag on 3 are different
  things, and somebody reorganising a library needs to see which is which
  before clicking. So the tile shows the name AND the number, and the number is
  the reason the query is a LEFT JOIN — a tag applied to nothing is precisely
  the one worth seeing, because it is a tag somebody created and never used.

  # Why it is a grid of square tiles and not the object grid

  A tag has no aspect ratio. It is a name and a count, and the shape of a thing
  with no intrinsic shape is a decision about the *set*: squares, because a
  square tile is the one shape that reads as "this is a label" rather than "this
  is media you are about to look at". Reusing the object grid's poster ratio here
  would imply each tag is a picture, which is a claim nobody made.

  The `contain` is inherited from the shared tile CSS and is not a coincidence:
  the same rule that stops a 16:9 video being cropped stops a long tag name
  being cropped. Both are cases of a grid silently deciding what the user is
  allowed to read.

  # The count is not consent-filtered

  `Store::all_tags_with_counts` counts every object carrying the tag, including
  ones this caller may not see (§14.1). That is deliberate and the reasoning is
  in the store: a count that silently dropped unverified items would make a
  heavily-used tag look unused, and a person reorganising would delete it. The
  count is a fact about the library; consent governs what a caller may *open*.
  Clicking a tile still goes through the normal filtered query, so the two never
  disagree about what is visible — only about what exists.
-->
<script lang="ts">
  /**
   * A tag tile is square.
   *
   * Not the object grid's poster ratio: a tag has no intrinsic shape, and
   * borrowing 2:3 would imply each tag is a picture. A name reads better on a
   * wide tile, so 1:1 and a short count underneath. Named here rather than
   * inlined so the reason travels with the number.
   */
  const TAG_TILE_RATIO = 1;

  interface Props {
    /** The tags, already counted, in name order. */
    tags: readonly { readonly id: string; readonly name: string; readonly count: number }[];
    /** Tile width in CSS pixels. The same knob the object grid uses. */
    density?: number;
  }

  let { tags, density = 180 }: Props = $props();

  /**
   * A tag is a name, and names are longer than tiles.
   *
   * Truncated at one line with an ellipsis, with the full name in `title` so a
   * hover still gives it up. Wrapping instead would make the tiles different
   * heights, and a grid whose tiles differ in height is a grid whose rows differ
   * in height, which is the virtualizer's whole premise — the same constraint
   * `VirtualGrid` documents about its row height, arriving here by a different
   * route.
   */
</script>

<div class="tag-wall" style:--tile-aspect={TAG_TILE_RATIO} data-testid="tag-wall">
  {#each tags as tag (tag.id)}
    <a
      class="tag-tile"
      href="?q=tags%3D{tag.name}"
      style:width="{density}px"
      data-testid="tag-tile"
      data-tag-id={tag.id}
      data-count={tag.count}
      title={tag.name}
    >
      <span class="tag-name">{tag.name}</span>
      <span class="tag-count" data-testid="tag-count">{tag.count}</span>
    </a>
  {:else}
    <p class="tag-empty" data-testid="tag-empty">No tags yet.</p>
  {/each}
</div>

<style>
  .tag-wall {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    align-content: flex-start;
  }
  .tag-tile {
    flex: none;
    box-sizing: border-box;
    aspect-ratio: var(--tile-aspect, 2 / 3);
    display: flex;
    flex-direction: column;
    justify-content: space-between;
    gap: 4px;
    padding: 8px;
    text-decoration: none;
    color: inherit;
    background: #1c1c1c;
    border: 1px solid #2e2e2e;
    border-radius: 4px;
    aspect-ratio: var(--tile-aspect, 1);
  }
  .tag-name {
    font-size: 0.8rem;
    line-height: 1.2;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .tag-count {
    font-size: 0.7rem;
    color: var(--muted);
    font-variant-numeric: tabular-nums;
  }
  .tag-empty {
    color: var(--muted);
  }
</style>
