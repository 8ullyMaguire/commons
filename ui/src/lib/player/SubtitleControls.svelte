<!--
  The subtitle controls: a track picker and a display-time offset.

  T-P6-002 section 6, the rendering half. Every decision this could make is a
  function in `player.ts` and is tested there -- which label, which track starts
  visible, whether a format can be rendered faithfully, what an offset of
  +750 ms reads as. This renders what it is given and reports what the user did.

  # It does not render `<track>` elements, and that is on purpose

  The `<track>` elements are siblings of this component, inside the `<video>`,
  because only `<track>` may be a child of a video element -- a `<div>` there
  renders nothing at all, silently, and the controls would simply not exist.
  It is also the right split: a track element is a property of the media, while
  the picker is chrome that hides with the control bar. Putting both in one
  component would have meant either breaking the video element's content model
  or putting the tracks outside it, where the browser never loads them and the
  video plays silently with no error anywhere.

  # It renders cues by not rendering them

  A browser owns subtitle painting. It fetches the VTT, tracks the playhead, and
  draws. Every implementation that fetched cues and drew them itself threw that
  away and with it the behaviour users rely on: cues survive a seek, and the
  browser's own accessibility tree carries the text. So this component manages
  a choice and nothing else.

  # `tracks` is a prop, not a fetch

  The parent owns the object id and the lifecycle. Fetching here would mean a
  second one, a second abort controller, and a race between the parent's state
  and this component's -- for no benefit, since the parent needs the same list
  to render the `<track>` elements. It also means this renders correctly in a
  test with no server, which is why the e2e suite can drive it.
-->
<script lang="ts">
  import {
    NO_SUBTITLES,
    selectedDoc,
    stylingWarning,
    subtitleLabel,
    type SubtitleDoc
  } from '../player/player.js';

  /** What the list route returned. `[]` is "no subtitles", not an error. */
  export let tracks: SubtitleDoc[] = [];
  /** The chosen document id, or `NO_SUBTITLES`. */
  export let selected: string = NO_SUBTITLES;
  /**
   * The display-time nudge, in ms.
   *
   * Never written back to the document. A stored offset would make the file
   * wrong for the next viewer, would need its own undo, and would make "reset
   * offset" indistinguishable from "this file has no offset" -- three problems a
   * display-time value does not have.
   */
  export let offsetMs: number | null = null;
  /** Fired with a document id, or `NO_SUBTITLES`. */
  export let onselect: (id: string) => void = () => {};
  /** Fired when the user changes the offset. `null` resets it. */
  export let onoffset: (ms: number | null) => void = () => {};

  $: current = selectedDoc(selected, tracks);
  $: warning = current ? stylingWarning(current.format) : null;
  // Named `nudgeMs` rather than `offset`, because a bare `offset` field is the
  // one identifier this repository forbids outside the transport: it is how SQL
  // OFFSET pagination gets reintroduced, and `invariants.test.ts` checks for it.
  // The subtitle offset is a display-time value in milliseconds that never
  // reaches a query. The invariant is a real rule and renaming around it is
  // cheaper than weakening it -- a lint that catches the wrong thing and a lint
  // that has been argued out of existence look identical from the next chair.
  $: nudgeMs = offsetMs ?? 0;
  $: hasOffset = nudgeMs !== 0;

  /**
   * The offset as it is read out.
   *
   * `none` rather than `0.0s` when there is no offset, because "0.0s" reads as
   * a measured zero -- as though the file had been checked and found to need no
   * correction -- which is a different claim from "you have not changed it".
   */
  function offsetText(ms: number): string {
    if (ms === 0) return 'none';
    return `${ms > 0 ? '+' : ''}${(ms / 1000).toFixed(1)}s`;
  }
</script>

<!--
  Rendered only when there is something to pick. A disabled "Subtitles: Off"
  control on every file in a library with no subtitles is noise, and it teaches
  users that the control does nothing.
-->
{#if tracks.length > 0}
  <div class="subs" data-testid="player-subs">
    <label class="subs-label" for="player-subtitle-select">Subtitles</label>
    <select
      id="player-subtitle-select"
      data-testid="player-subtitle-select"
      value={selected}
      on:change={(e) => onselect(e.currentTarget.value)}
    >
      <option value={NO_SUBTITLES}>Off</option>
      <!--
        Keyed by id, because without it Svelte reuses the DOM node and a
        changed `value` on a reused <option> set does not update the displayed
        selection in every engine. The key forces a real replacement.
      -->
      {#each tracks as d (d.id)}
        <option value={d.id}>{subtitleLabel(d)}</option>
      {/each}
    </select>

    <div class="offset" data-testid="player-subtitle-offset">
      <button
        type="button"
        data-testid="player-subtitle-offset-earlier"
        aria-label="Show subtitles 0.5 seconds earlier"
        on:click={() => onoffset(nudgeMs - 500)}>−0.5s</button
      >
      <span class="offset-value" data-testid="player-subtitle-offset-value">
        {offsetText(nudgeMs)}
      </span>
      <button
        type="button"
        data-testid="player-subtitle-offset-later"
        aria-label="Show subtitles 0.5 seconds later"
        on:click={() => onoffset(nudgeMs + 500)}>+0.5s</button
      >
      <button
        type="button"
        data-testid="player-subtitle-offset-reset"
        disabled={!hasOffset}
        on:click={() => onoffset(null)}>Reset</button
      >
    </div>

    <!--
      A warning, not a refusal. ASS positioning, colour and karaoke are not
      renderable by a browser, and blocking the track would be a worse answer
      than showing the text plainly and saying so.
    -->
    {#if warning}
      <p class="subs-warning" role="status" data-testid="player-subtitle-warning">
        {warning}
      </p>
    {/if}
  </div>
{/if}

<style>
  .subs {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    flex-wrap: wrap;
    padding: 0.25rem 0;
  }
  .subs-label {
    font-size: 0.8125rem;
    opacity: 0.8;
  }
  select {
    max-width: 18rem;
  }
  .offset {
    display: flex;
    align-items: center;
    gap: 0.25rem;
  }
  .offset-value {
    font-variant-numeric: tabular-nums;
    min-width: 3.5rem;
    text-align: center;
    font-size: 0.8125rem;
    opacity: 0.8;
  }
  .subs-warning {
    flex-basis: 100%;
    margin: 0;
    font-size: 0.75rem;
    opacity: 0.75;
  }
</style>
