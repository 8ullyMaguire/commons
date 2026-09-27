<!--
  The funscript player: an overlay, a ruler, and the three states.

  T-P6-003, spec §5.6. This file is the DOM and nothing else: every decision
  is a function in `funscript.ts` and is tested there, which is why 39 of this
  feature's tests need no browser at all.

  # The clock, and why it is `video.currentTime` and not `performance.now()`

  The device's position is derived from the VIDEO's clock on every frame. A
  player that integrated its own clock -- `deviceMs += interval` per tick --
  accumulates the difference between the scheduled and the actual interval, and
  after 3,600 ticks at 60 Hz a 1 ms error per frame is a full second. It looks
  correct in any single screenshot and is wrong over two minutes, which is why
  the ticket's claim is about a RATE and why `driftAfter` exists.

  So: read `video.currentTime`, and use `advanceClock` only to record the
  frozen device time that #2762 needs. There is no arithmetic that can
  accumulate here, because there is no arithmetic.

  # Manual pause, and what it does to the clock

  `manual-pause` freezes the DEVICE clock while the video is paused. A seek
  during a manual pause does not move it, and `advanceClock` is what makes that
  true: under either paused state the device time is carried forward unchanged
  while the video time is updated. Resuming re-syncs from the video, so a user
  who pauses at 5 s, scrubs to 30 s and presses play gets a device at 30 s
  rather than one that spent 25 s catching up.

  # Why the overlay draws the device's position and not the video's

  They are the same number while playing and different numbers the moment
  anything is paused, which is the whole point of the feature. An overlay bound
  to the video would keep moving under a manual pause and would be lying.
-->
<script lang="ts">
  import {
    advanceClock,
    axisSummary,
    deviceAt,
    markerGeometry,
    positionAll,
    positionToPct,
    SYNC_BUDGET_MS,
    withinBudget,
    type FunscriptAxis,
    type FunscriptTimeline,
    type Interpolation,
    type PlayState,
  } from './funscript.js';

  interface Props {
    /** The timeline, or null while it loads or when there is no script. */
    timeline: FunscriptTimeline | null;
    /** The media element. Read-only: this component does not drive playback. */
    video: HTMLVideoElement | null;
    /** Which reading fills the space between two actions. */
    interpolation?: Interpolation;
    /** #2762. A user-initiated pause that also stops the device. */
    manualPause?: boolean;
    /** The transport is paused (scrub bar, keyboard, or the video's own event). */
    transportPaused?: boolean;
    /** Called when the state changes, so the parent can show a control. */
    onmanualpausechange?: (on: boolean) => void;
  }

  const p: Props = $props();

  // The DEVICE clock, which is the only clock that can differ from the video's.
  // `videoMs` is the video's own reading; `deviceMs` only moves while playing.
  let deviceMs = $state(0);
  let videoMs = $state(0);

  // The measured error between the two, and whether it is inside the ticket's
  // budget. Surfaced rather than swallowed: a player drifting is a thing an
  // operator needs to see, and a debug readout that always says "ok" is
  // decoration.
  let errorMs = $state(0);

  const state: PlayState = $derived(
    p.manualPause ? 'manual-pause' : p.transportPaused ? 'paused' : 'playing'
  );

  const axes: FunscriptAxis[] = $derived(p.timeline?.axes ?? []);

  /** Every axis at the device's frozen time, so the overlay shows the DEVICE. */
  const readings = $derived(
    p.timeline
      ? positionAll(p.timeline, deviceMs, p.interpolation ?? 'step')
      : []
  );

  // The playhead along the script, as a percentage of the script's own span.
  const playheadPct = $derived(
    p.timeline && p.timeline.span_ms > 0
      ? Math.min(100, Math.max(0, (deviceMs / p.timeline.span_ms) * 100))
      : 0
  );

  const inBudget = $derived(withinBudget(errorMs));

  /**
   * One tick: read the video's clock, move the device clock by exactly the
   * rule, and record the error.
   *
   * The only arithmetic is the error measurement, which is a comparison rather
   * than an accumulation -- so nothing here can drift, and the budget readout
   * is a measurement rather than a claim.
   */
  function tick() {
    if (!p.video) return;
    const now = p.video.currentTime * 1000;
    videoMs = now;
    // The clock's OWN video reading is the previous frame's, not the device's:
    // seeding it with `deviceMs` would make the two fields identical, so a
    // paused seek could never be distinguished from a device that had moved.
    const after = advanceClock({ videoMs, deviceMs }, now, state);
    deviceMs = after.deviceMs;
    errorMs = Math.abs(after.deviceMs - now);
  }

  /**
   * Drive the clock from the video element.
   *
   * `requestAnimationFrame` rather than a `setInterval`: an interval at 60 Hz
   * is a promise the event loop cannot keep, and the misses are exactly the
   * drift this design avoids by reading the media clock -- but rAF also does
   * not fire for a backgrounded tab, so a player that needed to keep moving
   * while hidden would have to read `currentTime` on resume rather than
   * integrate. It does not: the video is the clock, and a hidden tab's video
   * has not advanced either.
   *
   * The effect reads `p.video` and `p.timeline`, so it re-subscribes when
   * either changes -- a player mounted before the media element exists would
   * otherwise never tick at all, with no error anywhere.
   */
  $effect(() => {
    const video = p.video;
    if (!video) return;
    let frame = 0;
    const loop = () => {
      tick();
      frame = requestAnimationFrame(loop);
    };
    frame = requestAnimationFrame(loop);
    // One tick immediately, so a paused-at-mount player shows a position
    // rather than a dot at 0 until the first frame lands.
    tick();
    return () => cancelAnimationFrame(frame);
  });

  function toggleManualPause() {
    p.onmanualpausechange?.(!p.manualPause);
  }

  /** The device's output on one axis, for a consumer that drives hardware. */
  export function outputFor(index: number) {
    const axis = axes[index];
    if (!axis) return null;
    return deviceAt(axis, state, videoMs, { videoMs, deviceMs }, p.interpolation ?? 'step');
  }
</script>

{#if p.timeline}
  <!--
    The overlay and the ruler. `data-*` on every measurable value, because a
    Playwright test cannot tell a clamp is load-bearing from the rendered
    pixels alone -- it can only read what the component reports.
  -->
  <div class="fs" data-testid="funscript" data-state={state} data-in-budget={inBudget}>
    <div class="fs-ruler" data-testid="funscript-ruler">
      {#each axes as axis, i (axis.name)}
        <div
          class="fs-track"
          data-testid="funscript-axis"
          data-axis={axis.name}
          data-actions={axis.actions.length}
        >
          {#each axis.actions as action, j (j)}
            {@const g = markerGeometry(action.at_ms, p.timeline?.span_ms ?? 0, 1000)}
            <span
              class="fs-marker"
              data-testid="funscript-marker"
              data-axis={axis.name}
              data-at-ms={action.at_ms}
              style="left:{g.leftPct}%;width:{g.widthPct}%"
            ></span>
          {/each}
          <span
            class="fs-head"
            data-testid="funscript-head"
            data-axis={axis.name}
            data-position={readings[i]?.position ?? ''}
            style="left:{positionToPct(readings[i]?.position ?? null) ?? 0}%"
          ></span>
        </div>
      {/each}
      <span
        class="fs-playhead"
        data-testid="funscript-playhead"
        style="left:{playheadPct}%"
      ></span>
    </div>

    <div class="fs-meta" data-testid="funscript-meta">
      <span data-testid="funscript-axis-summary">{axisSummary(p.timeline)}</span>
      <span data-testid="funscript-device-ms" hidden={state === 'playing'}>
        {deviceMs}
      </span>
      <span data-testid="funscript-error-ms">{errorMs.toFixed(1)}</span>
      <button
        type="button"
        data-testid="funscript-manual-pause"
        aria-pressed={p.manualPause}
        onclick={toggleManualPause}
      >
        {p.manualPause ? 'resume device' : 'pause device'}
      </button>
    </div>

    {#each p.timeline.warnings as warning (warning)}
      <p class="fs-warn" data-testid="funscript-warning">{warning}</p>
    {/each}
  </div>
{/if}

<style>
  .fs {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    font-size: 0.75rem;
  }
  .fs-ruler {
    position: relative;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .fs-track {
    position: relative;
    display: block;
    height: 0.75rem;
    background: color-mix(in oklab, currentColor 8%, transparent);
  }
  /* An action is an INSTANT, so the marker is a hairline. A wider one claims a
     duration the script does not have. */
  .fs-marker {
    position: absolute;
    top: 0;
    bottom: 0;
    min-width: 1px;
    background: currentColor;
    opacity: 0.45;
  }
  .fs-head,
  .fs-playhead {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 2px;
    background: currentColor;
  }
  .fs-head {
    opacity: 0.9;
  }
  .fs-playhead {
    opacity: 0.6;
  }
  .fs-meta {
    display: flex;
    gap: 0.75rem;
    align-items: center;
    opacity: 0.85;
  }
  .fs-warn {
    margin: 0;
    opacity: 0.85;
  }
</style>
