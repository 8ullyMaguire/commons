<!--
  The player. Spec §11.1 (C64), §11.5; plan T-P6-001.

  # The component is thin on purpose

  Every decision lives in `lib/player/player.ts`, and the reasons are all the
  same: each one has a wrong answer that is invisible. A control bar that
  overflows a phone still *looks* like a control bar. A scrubber that seeks to
  the wrong frame still *moves*. So the numbers are in tested functions and this
  file only draws them, wires events, and talks to the two endpoints.

  # What this component is responsible for

  1. choosing a source (direct or proxy) and putting it on the video element;
  2. restoring a position, and refusing three positions by name;
  3. the loop, armed only when both ends are set and ordered;
  4. the control bar, laid out from `planControlBar` for the live viewport;
  5. saving a position periodically and on the way out.

  # The claim only a browser can make

  The unit tests prove the *plan* for a 360x640 viewport. They cannot prove the
  bar fits. `e2e/player.spec.ts` sets that viewport for real and measures the
  bar against the window, so "fits" is checked against pixels rather than
  against a function's opinion of pixels.
-->
<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import { fetchMediaCaps, fetchPlayback, savePlayback, type MediaCaps } from '$lib/api/client.js';
  import {
    clipPath,
    isLoopArmed,
    longPressSpeed,
    loopMarkers,
    MAX_BAR_FRACTION,
    NARROW_BAR_HEIGHT_PX,
    planControlBar,
    playSource,
    resumePosition,
    SAVE_INTERVAL_MS,
    seekAccuracy,
    shouldDeinterlace,
    shouldSave,
    skipIntroWindow,
    videoStyle,
    type CropPanFlip,
    type DeinterlaceMode,
    type SourceInfo
  } from '$lib/player/player.js';

  /**
   * What the caller already knows about the file, if anything.
   *
   * All of it optional and all of it a hint: the component asks
   * `/media/<id>/caps` when it is not given `caps`, because a decision made from
   * a row with no codec columns is a guess and the guess is always "proxy".
   */
  let {
    objectId,
    source = {},
    title = null,
    introMarker = null,
    proxyHeight = null,
    caps = null
  }: {
    /** The object being played. */
    objectId: string;
    source?: SourceInfo;
    title?: string | null;
    /** Per-source intro marker, stash#634. Absent for most files. */
    introMarker?: { startMs: number; endMs: number } | null;
    /** The proxy rung, when one is already cached and known. */
    proxyHeight?: number | null;
    /** What the server said, when the caller already asked. */
    caps?: MediaCaps | null;
  } = $props();

  let video: HTMLVideoElement;
  let viewport = $state({ width: 1280, height: 800 });
  let position = $state(0);
  let duration = $state(0);
  let paused = $state(true);
  let playing = $state(false);
  let loop = $state<{ a_ms: number; b_ms: number } | null>(null);
  let loopArmed = $state(false);
  let speed = $state(1);
  let deinterlace: DeinterlaceMode = 'auto';
  let showBar = $state(true);
  let barTimer: ReturnType<typeof setTimeout> | null = null;
  let lastSavedAt: number | null = null;
  let holdStart = 0;
  let holdOrigin: { x: number; y: number } | null = null;
  let holdTimer: ReturnType<typeof setTimeout> | null = null;
  let interlaced = $state(false);
  let fieldOrder: string | null = null;
  let fps = $state<number | null>(null);
  let failed = $state<string | null>(null);
  /**
   * The saved state, once it has arrived.
   *
   * Held rather than applied on arrival, because `fetchPlayback` and
   * `loadedmetadata` are independent events and either can fire first. Writing
   * the position into `position` from the fetch and then having
   * `onLoadedMetadata` seek from it is a race: if metadata lands first, the
   * resume decision is computed from a position the fetch has not written yet,
   * and the file opens at the start with a saved state nobody can see. So the
   * fetch stores, and the metadata event is the single place the stored value
   * is applied -- which is also the only moment the seek can work.
   */
  let saved: {
    position_ms: number;
    duration_ms: number | null;
    /** Whether the user watched this to the end last time. */
    completed: boolean;
    loop: { a_ms: number; b_ms: number } | null;
  } | null = null;
  let resumeApplied = false;
  /**
   * Where the player decided to resume, in ms, or -1 for "start at the beginning".
   *
   * Exposed on the element because it is the claim that is the PLAYER's, and it
   * is the only part of the resume that can be asserted in a browser where a
   * seek is refused. The decision and the seek are different things: the first
   * is ours, the second is the browser's, and only the second is unreliable here.
   *
   * `null` until the decision is made, and the ATTRIBUTE is absent when the
   * decision is "start at the beginning". A sentinel for that -- -1, or even 0 --
   * is the wrong shape twice over: it is indistinguishable from "not decided
   * yet" (`?? undefined` keeps a 0, so a target of zero would render as
   * `data-resume-target="0"`), and it invites an assertion that passes against a
   * player that has not finished asking the server. Such a test passes in one
   * order and fails in another, and gets deleted as flaky.
   *
   * 0 is a real answer from `resumePosition` -- it is what "start at the
   * beginning" comes back as -- and the attribute is the place where that answer
   * is not a position, so it is dropped here rather than encoded.
   */
  let resumeTarget = $state<number | null>(null);
  /**
   * Whether the resume decision has been made at all.
   *
   * Separate from `resumeTarget` because "start at the beginning" and "not
   * decided yet" are both zero-ish, and conflating them is how an assertion ends
   * up passing against a player that has not finished asking the server.
   */
  let resumeDecided = $state(false);
  /**
   * How many times the resume seek has been re-issued.
   *
   * A seek is a request, and this browser refuses one it cannot satisfy -- for a
   * moment after `canplay`, while the range at 2.5s is still unbuffered. A
   * refusal is silent: no `error`, no `seeked`, `currentTime` unchanged. So the
   * resume is not one attempt but a bounded few, spaced on `timeupdate` and on
   * `canplay`, stopping as soon as the clock agrees.
   *
   * Bounded, because an unbounded retry on a file that genuinely cannot seek to
   * the saved position is a loop that never stops -- and the honest end state for
   * that file is "opened at the start", which is what it already is.
   */
  let resumeAttempts = 0;
  const RESUME_TRIES = 8;

  /**
   * What the server said, or null until it says.
   *
   * `caps` wins when the caller has it; otherwise the component asks. The ask is
   * not optional politeness: `ObjectRow` carries no container, no codecs and no
   * frame rate, and the database has no columns for them either, so a decision
   * made from a row is a guess. `playSource` is where that guess and the
   * server's answer are reconciled.
   */
  let resolved = $state<MediaCaps | null>(caps);
  const played = $derived(playSource(objectId, resolved, source, proxyHeight));
  const proxied = $derived(played.proxied);
  const url = $derived(played.url);

  const plan = $derived(planControlBar(viewport));
  const accuracy = $derived(seekAccuracy(fps));
  const intro = $derived(skipIntroWindow(!!introMarker, introMarker));
  const deinterlaced = $derived(shouldDeinterlace(deinterlace, { interlaced, fieldOrder, fps }));
  const skip = $derived(!!intro && position >= intro.startMs && position < intro.endMs);

  /** Crop/pan/flip state; a separate object so the style functions stay pure. */
  let crop: CropPanFlip = $state({ crop: null, panX: 0, panY: 0, rotate: 0, flipH: false, flipV: false });

  /**
   * Whether the user is dragging the scrubber.
   *
   * Not cosmetic. A range input whose `value` is bound to the playing position
   * fights the pointer: `timeupdate` fires several times a second, writes the
   * clock's idea of the position into the same variable the drag is writing, and
   * the thumb springs back to wherever playback is. The drag then ends at a
   * position the user did not choose, or does not register at all.
   */
  let seeking = $state(false);

  /**
   * The position a seek asked for, kept until the browser agrees.
   *
   * `video.currentTime = x` is a REQUEST. The browser refuses one it cannot
   * satisfy -- a range it has not buffered, a duration it has not measured -- and
   * says nothing when it refuses: `currentTime` keeps its old value and
   * `timeupdate` writes that old value straight back into `position`.
   *
   * The consequence is worse than a seek that does not happen. The user's
   * position is discarded, the scrubber springs back, and a marker dropped right
   * after the drag lands at the old position instead of the chosen one. Every
   * wrong answer here is silent, which is why the request is held rather than
   * assumed.
   */
  let pendingSeek = $state<number | null>(null);
  /**
   * When a pending seek gave up.
   *
   * The browser refuses a seek silently: no `error`, no `seeked`, and
   * `timeupdate` goes on reporting the old position. So a request cannot be held
   * forever -- after a bounded window the control gives up and reports where the
   * file really is, which is the only honest thing left to show. Without this
   * bound the scrubber would display a position the file is not at, for ever,
   * and every marker dropped afterwards would be at the wrong place.
   */
  let seekGaveUpAt = $state<number | null>(null);
  /** When the current pending seek was issued, for the grace window. */
  let seekStartedAt = 0;
  /**
   * How long a seek is given before the clock takes the position back.
   *
   * Long enough for a real seek to land on a slow connection, short enough that
   * a refusal is not a control showing a position the file is not at. 400 ms is
   * the same window the probe in the test measured a refusal inside, so a refused
   * seek is corrected well inside a second rather than lingering.
   */
  const SEEK_GRACE_MS = 400;

  function onTimeUpdate() {
    // The clock does not get a vote while a drag is in progress, nor while a
    // seek is outstanding -- until the outstanding window expires, at which
    // point the clock wins because it is the only thing left with evidence.
    if (seeking) return;
    if (pendingSeek !== null) {
      if (performance.now() - seekStartedAt < SEEK_GRACE_MS) return;
      seekGaveUpAt = performance.now();
      pendingSeek = null;
    }
    position = video.currentTime * 1000;
    if (loopArmed && loop) {
      const { a_ms, b_ms } = loop;
      if (position >= b_ms) {
        video.currentTime = a_ms / 1000;
        position = a_ms;
      }
    }
    // Skip the intro once per pass through it, not on every tick -- otherwise
    // playback jumps forward one tick at a time and never appears to move.
    if (intro && position >= intro.startMs && position < intro.endMs) {
      video.currentTime = intro.endMs / 1000;
      position = intro.endMs;
    }
  }

  /**
   * The one place a saved position is turned into a seek.
   *
   * Called from both `onLoadedMetadata` and the tail of `loadState`, because
   * those are the two things whose order is not fixed and the seek needs a
   * duration to decide against. Whichever runs second is the one that has both
   * halves; whichever runs first leaves nothing to do because `resumeApplied`
   * is not yet set. That flag is also why this is idempotent: `loadedmetadata`
   * fires more than once -- on a source change, on an HLS level change -- and
   * re-seeking to a saved position after the user has watched thirty seconds is
   * worse than not resuming at all.
   */
  function applySaved() {
    if (resumeApplied || !saved || !video || resumeAttempts >= RESUME_TRIES) return;
    loop = saved.loop;
    loopArmed = isLoopArmed(loop);
    const target = resumePosition(
      {
        position_ms: saved.position_ms,
        duration_ms: duration || saved.duration_ms || null,
        completed: saved.completed
      },
      duration || saved.duration_ms || null
    );
    resumeTarget = target;
    resumeDecided = true;
    if (target <= 0) {
      // Nothing to resume to, and that is a decision rather than a failure: the
      // file opens at the start and no retry will change it.
      resumeApplied = true;
      return;
    }
    resumeAttempts += 1;
    // The same rule as a scrubber drag: the request is held until the browser
    // agrees, or the clock writes the old position straight back over it.
    position = target;
    pendingSeek = target;
    video.currentTime = target / 1000;
  }

  function onLoadedMetadata() {
    duration = Number.isFinite(video.duration) ? video.duration * 1000 : 0;
    fps = source.fps ?? null;
    // The resume is NOT applied here. `loadedmetadata` means the browser has
    // measured a duration, not that it can seek: on a file it has not buffered,
    // `currentTime = x` is refused silently, `currentTime` keeps its old value,
    // and `seeked` never fires. The file then opens at the start with a saved
    // position nobody can see -- which is the failure the resume exists to
    // prevent, and it is invisible without a test that reads the element's own
    // clock. `canplay` is the moment a seek is honoured.
  }

  function onCanPlay() {
    failed = null;
    playing = true;
    // The first moment a seek is *likely* honoured. See `onLoadedMetadata` for
    // why not earlier, and `applySaved` for why this is a retry rather than a
    // single attempt: `canplay` can fire while the range at the saved position is
    // still unbuffered, and the refusal is silent.
    applySaved();
  }

  function onError() {
    // A <video> whose source it cannot decode fires `error` and then sits there
    // as a black rectangle. Saying so is the whole difference between a bug
    // report and a shrug.
    failed = `This browser cannot play this file${proxied ? '' : ' and the proxy could not help'}.`;
    playing = false;
  }

  async function loadState() {
    try {
      const state = await fetchPlayback(objectId);
      if (!state) {
        // A file with no saved state has a resume decision -- "start at the
        // beginning" -- and it is different from not having asked yet.
        resumeDecided = true;
        return;
      }
      if (state) {
        saved = {
          position_ms: state.position_ms,
          duration_ms: state.duration_ms,
          // Carried through, and load-bearing: `resumePosition` starts a
          // COMPLETED video again rather than dropping the user at the last
          // frame of something they have already watched. Dropping this field
          // made every completed video resume at its final position.
          completed: state.completed,
          loop:
            state.loop_a_ms !== null && state.loop_b_ms !== null
              ? { a_ms: state.loop_a_ms, b_ms: state.loop_b_ms }
              : null
        };
        // Metadata may already have fired, in which case `onLoadedMetadata`
        // saw no saved state and did nothing. This is the other caller of the
        // same function, not a second copy of the decision.
        applySaved();
      }
    } catch {
      // No saved state, or a server that could not be asked. Neither is worth
      // interrupting playback for: a file nobody has played has no state, and
      // that is the normal case for most of a library.
    }
  }

  async function save(event: 'tick' | 'pagehide') {
    const now = Date.now();
    if (!shouldSave(now, lastSavedAt, event)) return;
    lastSavedAt = now;
    try {
      // `loopMarkers` is the one place that decides what "unset" means on the
      // wire, so a half-set loop and an inverted one both leave here as NULLs
      // rather than as positions the server would reject.
      const markers = loopMarkers(loop?.a_ms, loop?.b_ms);
      await savePlayback(objectId, {
        position_ms: Math.round(video.currentTime * 1000),
        duration_ms: duration || null,
        ...markers
      });
    } catch {
      // A resume position that fails to save is a lost few seconds, not a
      // reason to interrupt playback with an alert.
    }
  }

  function toggle() {
    if (video.paused) video.play();
    else video.pause();
  }

  function nudge(ms: number) {
    video.currentTime = Math.max(0, (video.currentTime * 1000 + ms) / 1000);
  }

  /**
   * Drop a loop marker at the playhead.
   *
   * Two things here that were both wrong the obvious way.
   *
   * **The playhead is `position`, not `video.currentTime`.** A seek sets
   * `position` immediately and `currentTime` only once the browser has buffered
   * the range -- on a slow or unbuffered file the two disagree for as long as it
   * takes, so pressing A right after dragging the scrubber dropped the marker
   * where the file used to be. `position` is the user's intent, which is what a
   * marker is.
   *
   * **The other end is null when unset, not 0.** `0` is a position: the start of
   * the file. A half-set loop stored as `{a: 12000, b: 0}` is an INVERTED loop,
   * so `isLoopArmed` is false -- correct -- but the pair sent to the server is
   * also inverted, and `loopMarkers` throws both away rather than storing the
   * marker A the user just set. Null is how "not set" is spelled everywhere
   * else, including the database.
   */
  function setLoop(which: 'a' | 'b') {
    const at = Math.round(position);
    loop = which === 'a' ? { a_ms: at, b_ms: loop?.b_ms ?? 0 } : { a_ms: loop?.a_ms ?? 0, b_ms: at };
    loopArmed = isLoopArmed(loop);
  }

  function clearLoop() {
    loop = null;
    loopArmed = false;
  }

  // Long press for 2x, with a slop bound -- see `longPressSpeed`.
  function onHoldStart(e: PointerEvent) {
    holdOrigin = { x: e.clientX, y: e.clientY };
    holdStart = performance.now();
    holdTimer = setTimeout(() => {
      applySpeed(longPressSpeed({ held: true, heldForMs: performance.now() - holdStart, driftPx: 0 }));
    }, 300);
  }

  function onHoldMove(e: PointerEvent) {
    if (!holdOrigin) return;
    const drift = Math.hypot(e.clientX - holdOrigin.x, e.clientY - holdOrigin.y);
    applySpeed(
      longPressSpeed({ held: true, heldForMs: performance.now() - holdStart, driftPx: drift })
    );
  }

  function onHoldEnd() {
    if (holdTimer) clearTimeout(holdTimer);
    holdTimer = null;
    holdOrigin = null;
    applySpeed({ speed: 1, reason: 'idle' });
  }

  function applySpeed(v: { speed: number }) {
    if (v.speed === speed) return;
    speed = v.speed;
    video.playbackRate = speed;
  }

  // The bar hides itself while playing and comes back on movement or a tap.
  function poke() {
    showBar = true;
    if (barTimer) clearTimeout(barTimer);
    if (playing) barTimer = setTimeout(() => (showBar = false), 2500);
  }

  function onKey(e: KeyboardEvent) {
    switch (e.key) {
      case ' ':
      case 'k':
        e.preventDefault();
        toggle();
        break;
      case 'ArrowLeft':
        e.preventDefault();
        nudge(e.shiftKey ? -10_000 : -5000);
        break;
      case 'ArrowRight':
        e.preventDefault();
        nudge(e.shiftKey ? 10_000 : 5000);
        break;
      case 'j':
        video.currentTime = Math.max(0, video.currentTime - 10);
        break;
      case 'l':
        video.currentTime = Math.min(video.duration || Infinity, video.currentTime + 10);
        break;
      case '[':
        setLoop('a');
        break;
      case ']':
        setLoop('b');
        break;
      case 'm':
        video.muted = !video.muted;
        break;
      case 'f':
        video.requestFullscreen?.();
        break;
    }
    poke();
  }

  onMount(() => {
    const mq = window.matchMedia('(max-width: 480px)');
    const measure = () => (viewport = { width: window.innerWidth, height: window.innerHeight });
    measure();
    window.addEventListener('resize', measure);
    mq.addEventListener('change', measure);
    // `pagehide`, not `beforeunload` -- the latter is not fired when a mobile
    // browser discards the tab, which is exactly the case a resume is for.
    window.addEventListener('pagehide', () => save('pagehide'));
    loadState();
    // The ask, unless the caller already answered it. A failure here is not
    // reported: `playSource` falls back to the local rule, and a player that
    // says "cannot read this file" because a *metadata* call failed is worse
    // than one that guesses conservatively and plays.
    if (!resolved) {
      fetchMediaCaps(objectId)
        .then((c) => (resolved = c))
        .catch(() => (resolved = null));
    }
    const tick = setInterval(() => save('tick'), 1000);
    // The resume retry's driver. `canplay` fires once; a seek it could not
    // satisfy is refused without an event, so something has to ask again. A
    // short interval, and only while the resume is outstanding -- it stops the
    // moment the clock agrees (`seeked`) and after `RESUME_TRIES`.
    const resumeTick = setInterval(() => {
      if (resumeApplied) return;
      applySaved();
    }, 60);
    return () => {
      clearInterval(resumeTick);
      window.removeEventListener('resize', measure);
      mq.removeEventListener('change', measure);
      clearInterval(tick);
      if (barTimer) clearTimeout(barTimer);
    };
  });

  onDestroy(() => {
    if (holdTimer) clearTimeout(holdTimer);
  });

  function fmt(ms: number): string {
    const total = Math.max(0, Math.round(ms / 1000));
    const h = Math.floor(total / 3600);
    const m = Math.floor((total % 3600) / 60);
    const s = total % 60;
    const mm = h ? String(m).padStart(2, '0') : String(m);
    return h ? `${h}:${mm}:${String(s).padStart(2, '0')}` : `${mm}:${String(s).padStart(2, '0')}`;
  }
</script>

<div
  class="player"
  class:playing
  bind:clientWidth={viewport.width}
  style:max-height="{NARROW_BAR_HEIGHT_PX / MAX_BAR_FRACTION}px"
  role="application"
  aria-label="Video player"
  tabindex="0"
  on:keydown={onKey}
  on:pointerdown={poke}
  data-testid="player"
  data-proxied={proxied}
  data-loop-armed={loopArmed}
  data-save-interval={SAVE_INTERVAL_MS}
  data-resume-target={resumeTarget && resumeTarget > 0 ? resumeTarget : undefined}
  data-resume-decided={resumeDecided}
>
  <!-- svelte-ignore a11y_media_has_caption -->
  <video
    bind:this={video}
    src={url}
    style={videoStyle(crop) + clipPath(crop)}
    playsinline
    preload="metadata"
    poster={`/media/${encodeURIComponent(objectId)}/thumb`}
    on:timeupdate={onTimeUpdate}
    on:loadedmetadata={onLoadedMetadata}
    on:canplay={onCanPlay}
    on:error={onError}
    on:play={() => ((playing = true), poke())}
    on:pause={() => ((playing = false), (paused = true), poke())}
    on:pointerdown={onHoldStart}
    on:pointermove={onHoldMove}
    on:pointerup={onHoldEnd}
    on:pointercancel={onHoldEnd}
    data-testid="player-video"
  />

  {#if failed}
    <p class="failed" role="alert" data-testid="player-error">{failed}</p>
  {/if}

  {#if skip}
    <span class="skip" data-testid="player-skip">Skipping intro</span>
  {/if}

  {#if showBar}
    <div class="bar" data-testid="player-bar">
      {#if plan.showTitle && title}
        <span class="title" data-testid="player-title">{title}</span>
      {/if}

      <!--
        The scrubber. `max` is the duration in ms and the value is the position
        in ms, so a 1000x wider scale is not needed and the DOM stays in the
        unit everything else in this player speaks.
      -->
      <input
        class="scrub"
        type="range"
        min="0"
        max={duration || 0}
        value={position}
        on:pointerdown={() => (seeking = true)}
        on:pointerup={() => (seeking = false)}
        on:input={(e) => {
          // `position` is written FIRST and the element second, so anything the
          // user can do next -- drop a marker, save, read the readout -- sees
          // the position they dragged to rather than the one the file has
          // reached. The reverse order is a race with the browser.
          position = Number(e.currentTarget.value);
          pendingSeek = position;
          seekStartedAt = performance.now();
          video.currentTime = position / 1000;
        }}
        on:seeked={() => {
          resumeApplied = true;
          // The browser has caught up, or refused. Either way the clock is
          // authoritative again -- and on a refusal `position` goes back to where
          // the file really is, which is the only honest place for it to be.
          pendingSeek = null;
          position = video.currentTime * 1000;
        }}
        aria-label="Seek"
        data-testid="player-scrub"
        data-accuracy={accuracy}
      />

      <div class="row">
        <button on:click={toggle} data-testid="player-play" aria-label={paused ? 'Play' : 'Pause'}>
          {paused ? '▶' : '❚❚'}
        </button>
        <button on:click={() => nudge(-5000)} aria-label="Back 5 seconds" data-testid="player-back">
          «
        </button>
        <button on:click={() => nudge(5000)} aria-label="Forward 5 seconds" data-testid="player-fwd">
          »
        </button>
        <button on:click={() => setLoop('a')} class:armed={loopArmed} data-testid="player-loopa">
          A
        </button>
        <button on:click={() => setLoop('b')} class:armed={loopArmed} data-testid="player-loopb">
          B
        </button>
        {#if plan.showSecondary}
          <button on:click={clearLoop} data-testid="player-loopclear">Clear</button>
          <button on:click={() => (video.muted = !video.muted)} data-testid="player-mute">
            {video?.muted ? 'Unmute' : 'Mute'}
          </button>
          <button
            on:click={() => (deinterlace = deinterlace === 'auto' ? 'off' : deinterlace === 'off' ? 'on' : 'auto')}
            data-testid="player-deint"
            data-mode={deinterlace}
            data-on={deinterlaced}
          >
            Deint: {deinterlace}
          </button>
          <button on:click={() => video.requestFullscreen?.()} data-testid="player-full">⛶</button>
        {/if}
        {#if plan.showReadouts}
          <span class="readout" data-testid="player-time">{fmt(position)} / {fmt(duration)}</span>
          {#if speed !== 1}
            <span class="readout" data-testid="player-speed">{speed}×</span>
          {/if}
          {#if accuracy === 'frame'}
            <span class="readout" data-testid="player-accuracy">frame</span>
          {/if}
        {/if}
      </div>
    </div>
  {/if}
</div>

<style>
  .player {
    position: relative;
    display: flex;
    align-items: center;
    justify-content: center;
    background: #000;
    color: #eee;
    min-height: 60vh;
    outline: none;
  }
  video {
    max-width: 100%;
    max-height: 100%;
    width: 100%;
  }
  .bar {
    position: absolute;
    inset: auto 0 0 0;
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    padding: 0.5rem;
    background: linear-gradient(transparent, rgba(0, 0, 0, 0.85));
  }
  .title {
    font-size: 0.85rem;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .scrub {
    width: 100%;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 0.35rem;
    flex-wrap: wrap;
  }
  button {
    background: #222;
    color: #eee;
    border: 1px solid #444;
    border-radius: 4px;
    padding: 0.35rem 0.6rem;
    font-size: 0.85rem;
    cursor: pointer;
  }
  button.armed {
    border-color: #6cf;
    color: #6cf;
  }
  .readout {
    font-size: 0.8rem;
    font-variant-numeric: tabular-nums;
    opacity: 0.85;
  }
  .failed {
    position: absolute;
    inset: 50% 1rem auto;
    text-align: center;
    color: #f88;
  }
  .skip {
    position: absolute;
    top: 0.5rem;
    left: 0.5rem;
    font-size: 0.8rem;
    background: rgba(0, 0, 0, 0.6);
    padding: 0.2rem 0.4rem;
    border-radius: 3px;
  }
  /* A short viewport: the bar is one compact row, no title. The plan function
     already decided what to drop -- this only makes the drop look deliberate. */
  @media (max-width: 480px) {
    .bar {
      padding: 0.35rem;
    }
    .row {
      flex-wrap: nowrap;
      overflow-x: auto;
    }
  }
</style>
