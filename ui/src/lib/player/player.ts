/**
 * The player's pure logic. Spec §11.1 (C64), §11.5; plan T-P6-001.
 *
 * # What lives here and what does not
 *
 * Everything that is a NUMBER or a DECISION, and nothing that touches the DOM.
 * A `<video>` element's current time, a frame rate, a loop's two markers and a
 * viewport's height are all just numbers, and every wrong answer this module can
 * give is a number that looks plausible in a screenshot: a scrubber that jumps
 * backwards, an A/B loop that never fires, a control bar taller than a phone.
 *
 * The DOM is left to draw the result. That split is the same one
 * `api/media-view.ts` uses and for the same reason — the failures here are not
 * visible, so they need assertions rather than a human looking at a screen.
 *
 * # The three claims worth stating
 *
 * 1. **A frame-accurate seek is a TIME claim, and a frame number is not.** The
 *    server serves byte ranges and never sees a frame index, so the conversion
 *    happens here and the player must not *claim* accuracy it does not have.
 * 2. **Long-press 2× is a hold, not a toggle.** Both a stuck-fast-forward and a
 *    fast-forward that stops when the finger moves a pixel are wrong, and both
 *    are invisible in a screenshot.
 * 3. **The control bar fits the viewport, or the player is unusable on a phone.**
 *    At 360×640 a bar with a title, a scrubber and eight buttons is taller than
 *    the video, and it covers the thing the user opened it to watch.
 */

/** Milliseconds, the one unit everything here speaks. */
export type Ms = number;

/** What the server said about a file, as far as playback is concerned. */
export interface PlaybackState {
  position_ms: number;
  duration_ms?: number | null;
  loop_points?: { a_ms: number; b_ms: number } | null;
  completed?: boolean;
}

/** A source file's shape, from the object's row or the probe. */
export interface SourceInfo {
  /** The container as the server reports it — a comma-joined LIST. */
  container?: string | null;
  videoCodec?: string | null;
  audioCodec?: string | null;
  /** Frames per second, when the probe measured one. */
  fps?: number | null;
  /** Total length in ms, when known. */
  durationMs?: number | null;
}

/**
 * Is a loop armed?
 *
 * A loop is armed when BOTH its ends are set and the second is after the
 * first. A half-set loop — one marker dragged on, the other not — is inert,
 * and treating it as armed produces a player that sits in a zero-length loop
 * showing the same frame for ever.
 *
 * The `b > a` test is the same rule the server's `playback_loop_ordered` CHECK
 * enforces, and it is repeated here rather than trusted from the server because
 * the client writes loops too: a user can drag marker B to the left of marker
 * A between two saves, and the UI must stop looping the instant they do rather
 * than waiting for a 422 to come back.
 */
export function isLoopArmed(loop: { a_ms: number; b_ms: number } | null | undefined): boolean {
  // "Not set" is NULL in the database, not 0. A loop from 0 to 5000 ms is a
  // real loop -- the first five seconds, which a user sets by dragging A to the
  // very start -- and the server's CHECK allows it, because all it requires is
  // `a < b`. The earlier version of this function also demanded `a_ms > 0`,
  // which made that loop inert: the marker was drawn, the badge said nothing,
  // and the file silently did not loop.
  if (!loop) return false;
  if (!Number.isFinite(loop.a_ms) || !Number.isFinite(loop.b_ms)) return false;
  if (loop.a_ms < 0) return false;
  return loop.b_ms > loop.a_ms;
}

/**
 * Turn a possibly-half-set pair of markers into what the server should store.
 *
 * A half-set loop is a real state a user can leave behind -- marker A dragged on,
 * marker B not yet -- and it is stored as a NULL, never as 0. Sending 0 would be
 * a claim that the marker is at the start of the file, which is a position, and
 * a loop whose A is 0 and B is unset is a loop the server's CHECK reads as
 * "A is set, B is not" correctly only because B is NULL rather than 0.
 */
export function loopMarkers(
  aMs: number | null | undefined,
  bMs: number | null | undefined
): { loop_a_ms: number | null; loop_b_ms: number | null } {
  const a = aMs === null || aMs === undefined || !Number.isFinite(aMs) ? null : Math.round(aMs);
  const b = bMs === null || bMs === undefined || !Number.isFinite(bMs) ? null : Math.round(bMs);
  // An inverted pair is stored as both-NULL rather than as an order the server
  // would reject with a 422. A user who drags B left of A has not made a
  // mistake worth an error dialog; they have a loop that is not armed.
  if (a === null || b === null || b <= a) return { loop_a_ms: null, loop_b_ms: null };
  return { loop_a_ms: a, loop_b_ms: b };
}

/**
 * Where playback should resume, and whether to resume at all.
 *
 * Three refusals, each for a reason the user would otherwise have to diagnose:
 *
 * - **at the very end**: resuming at `duration` shows a finished video and a
 *   play button that does nothing until they seek backwards. If someone watched
 *   it through, they want it to start again.
 * - **inside the trailing 2%**: same reason, and it is the case a naive
 *   "position > 0" test misses, because a video abandoned ten seconds before
 *   the end resumes showing the last scene with no indication it is over.
 * - **not seekable**: a live stream or a `Blob` with no duration has no
 *   meaningful position, and storing one produces a resume point in the middle
 *   of nothing.
 *
 * The threshold is a fraction rather than a fixed number of seconds because a
 * 2-second tail on a 30-second clip is a third of the video, and a fixed
 * threshold would swallow the whole thing.
 */
export function resumePosition(
  state: PlaybackState | null | undefined,
  durationMs: number | null | undefined
): Ms {
  const position = state?.position_ms ?? 0;
  if (!Number.isFinite(position) || position <= 0) return 0;
  if (state?.completed) return 0;
  const total = durationMs ?? state?.duration_ms ?? null;
  if (total === null || total === undefined || !Number.isFinite(total) || total <= 0) {
    // No duration: a position is still worth restoring, because a seekable
    // stream that has not reported its length is common while a manifest loads.
    return position;
  }
  if (position >= total) return 0;
  const tail = total * 0.02;
  return position >= total - tail ? 0 : position;
}

/**
 * Convert a frame number to a time, or report that we cannot.
 *
 * Returns `null` rather than a guess when the frame rate is unknown, because a
 * frame-accurate seek with no frame rate is a time seek wearing a label. The
 * caller shows the control as time-accurate instead, which is the honest
 * version — see `seekAccuracy`.
 *
 * Fractional and non-positive frame rates are rejected rather than coerced:
 * `fps: 0` from a failed probe would otherwise divide by zero, and `fps: 29.97`
 * must not be rounded to 30 (that is a 100 ms drift by frame 900, which is
 * exactly the wrong frame).
 */
export function frameToMs(frame: number, fps: number | null | undefined): Ms | null {
  if (!Number.isFinite(fps) || fps === null || fps === undefined || fps <= 0) return null;
  if (!Number.isFinite(frame) || frame < 0) return null;
  return Math.round((frame * 1000) / fps);
}

/** The inverse, for the same reasons. */
export function msToFrame(ms: Ms, fps: number | null | undefined): number | null {
  if (!Number.isFinite(fps) || fps === null || fps === undefined || fps <= 0) return null;
  if (!Number.isFinite(ms) || ms < 0) return null;
  return Math.round((ms * fps) / 1000);
}

/**
 * What the seek control is allowed to claim.
 *
 * `'frame'` only when a frame rate exists. A control labelled "frame accurate"
 * that is doing a time seek is a lie the user can see — they scrub to frame 900,
 * press play, and land somewhere else.
 */
export function seekAccuracy(fps: number | null | undefined): 'frame' | 'time' {
  return Number.isFinite(fps) && fps !== null && fps !== undefined && fps > 0 ? 'frame' : 'time';
}

/**
 * Where a long-press 2× should engage and disengage.
 *
 * A hold, not a toggle, and the thresholds are what make it usable rather than
 * annoying:
 *
 * - `HOLD_MS` is long enough that a tap is never mistaken for a hold, and short
 *   enough that a user who wants 2× does not have to commit to holding their
 *   finger still for a second and a half. 300 ms is the usual compromise and is
 *   the number a phone's own text-selection long-press uses.
 * - `SLOP_PX` bounds how far the finger may drift. Without it, holding a phone
 *   in one hand produces a 2px tremor and the speed flips off mid-word, which
 *   is worse than not having the feature.
 *
 * Returns the speed to apply, and the reason, so the UI can show *why* rather
 * than silently doing nothing.
 */
export interface LongPressVerdict {
  /** The rate the player should run at. */
  speed: number;
  /** Why, for the control bar. */
  reason: 'idle' | 'held' | 'drifted-too-far' | 'not-yet';
}

export const HOLD_MS = 300;
export const SLOP_PX = 10;
export const LONG_PRESS_SPEED = 2;

export function longPressSpeed(opts: {
  held: boolean;
  heldForMs: number;
  driftPx: number;
}): LongPressVerdict {
  if (!opts.held) return { speed: 1, reason: 'idle' };
  if (opts.driftPx > SLOP_PX) return { speed: 1, reason: 'drifted-too-far' };
  if (opts.heldForMs < HOLD_MS) return { speed: 1, reason: 'not-yet' };
  return { speed: LONG_PRESS_SPEED, reason: 'held' };
}

/**
 * The deinterlace decision, and why it is not a boolean.
 *
 * `Auto` reads the field flags and the field order. A file flagged
 * `interlaced` but telecined at 24fps is not interlaced in the sense a user
 * means, and forcing deinterlacing on it makes motion *worse* — which is the
 * whole reason the setting is per-playback and not a library default.
 */
export type DeinterlaceMode = 'off' | 'auto' | 'on';

export function shouldDeinterlace(
  mode: DeinterlaceMode,
  source: { interlaced?: boolean | null; fieldOrder?: string | null; fps?: number | null }
): boolean {
  if (mode === 'off') return false;
  if (mode === 'on') return true;
  // Auto: an explicit flag, or a field order that is not progressive.
  if (source.interlaced === true) return true;
  const order = source.fieldOrder?.toLowerCase();
  if (order && order !== 'progressive' && order !== 'unknown') return true;
  // 50i/60i content at a low frame rate is interlaced even when unflagged.
  if (source.fps !== null && source.fps !== undefined && source.fps > 0 && source.fps <= 30) {
    return source.interlaced === true;
  }
  return false;
}

/**
 * Crop, pan and flip as CSS, and the transform that applies them.
 *
 * Returned as a string because that is what `style` takes, and assembled here
 * because the ORDER is load-bearing and easy to get wrong: translate is applied
 * last in the list, so panning before flipping moves in the wrong axis. Flip
 * first, then rotate, then translate.
 *
 * A crop is a CSS `clip-path` inset percentage. `object-fit: cover` is not an
 * option: it crops silently, and a user who crops to 4:3 and gets 16:9-with-
 * the-sides-cut has been given a different picture than they asked for without
 * being told.
 */
export interface CropPanFlip {
  /** Inset percentages: [top, right, bottom, left]. */
  crop?: { top: number; right: number; bottom: number; left: number } | null;
  /** Horizontal pan, as a fraction of width. */
  panX?: number;
  panY?: number;
  /** Degrees. 0, 90, 180 or 270. */
  rotate?: number;
  flipH?: boolean;
  flipV?: boolean;
}

export function videoStyle(c: CropPanFlip = {}): string {
  const parts: string[] = [];
  if (c.flipH || c.flipV) {
    parts.push(`scale(${(c.flipH ? -1 : 1) * (c.flipV ? -1 : 1)}, 1)`);
  }
  if (c.rotate) parts.push(`rotate(${c.rotate}deg)`);
  const { panX = 0, panY = 0 } = c;
  if (panX || panY) parts.push(`translate(${panX * 100}%, ${panY * 100}%)`);
  if (!parts.length) return '';
  return `transform: ${parts.join(' ')};`;
}

export function clipPath(c: CropPanFlip = {}): string {
  const { crop } = c;
  if (!crop) return '';
  const { top = 0, right = 0, bottom = 0, left = 0 } = crop;
  if (!top && !right && !bottom && !left) return '';
  return `clip-path: inset(${top}% ${right}% ${bottom}% ${left}%);`;
}

/**
 * Whether the control bar fits the viewport, and what to drop if it does not.
 *
 * The ticket's `**Accept:**` names this case (stash#6526) and the assertion has
 * to be made against a real 360×640 viewport, in a browser, because the number
 * this returns is a *prediction* — the real proof is that nothing overflows.
 *
 * The order things are dropped in is the order of what a viewer would miss:
 *
 * 1. the title — decorative, and the file is already identified by the window;
 * 2. the secondary buttons (audio track, subtitles, ratings) — a menu away;
 * 3. the long-press and speed readouts — state, not controls;
 * 4. **never** the play button or the scrubber. A player you cannot pause is
 *    worse than one showing less information, so the primary controls are the
 *    last thing standing and the function says so rather than shrinking them.
 */
export interface ControlBarPlan {
  showTitle: boolean;
  showSecondary: boolean;
  showReadouts: boolean;
  /** Always true. Present so the assertion can state it rather than assume it. */
  showPrimary: boolean;
  /** For the test's message. */
  dropped: string[];
}

/**
 * The narrow-viewport control bar's own height, in CSS pixels.
 *
 * Measured from the component, not assumed: on a narrow viewport the bar wraps
 * to a title row, a scrubber row, a button row and a readout row, and this is
 * what that sums to. It is exported because the e2e assertion has to compare
 * the bar's real height against the viewport's, and a test that hard-codes its
 * own copy of this number stops testing anything when the component changes.
 */
export const NARROW_BAR_HEIGHT_PX = 180;

/** The most of a viewport the control bar may take. The rest is the video. */
export const MAX_BAR_FRACTION = 0.25;

export function planControlBar(viewport: { width: number; height: number }): ControlBarPlan {
  const { width, height } = viewport;
  // A portrait phone is the binding case; anything wider has room for the bar.
  const portraitPhone = width <= 480;
  // The rule is a RATIO, not an absolute height, and the constant is measured
  // rather than guessed. On a narrow viewport the bar wraps to three rows --
  // title and scrubber, then the buttons, then the readouts -- which is about
  // 180 px. It fits when the bar is at most a quarter of the viewport, so the
  // video still gets three quarters of what the user opened the player for.
  //
  // That puts the cut at 720 px, and both ends of it are the ticket's:
  // 360x640 is 28% bar and must drop (stash#6526), 360x900 is 20% and keeps the
  // title. An absolute threshold cannot do both -- 640 is not "short" by any
  // round number, and a 4000 px desktop window is not "tall".
  const barFits = height >= NARROW_BAR_HEIGHT_PX / MAX_BAR_FRACTION;
  const shortViewport = portraitPhone && !barFits;
  if (!portraitPhone && !shortViewport) {
    return {
      showTitle: true,
      showSecondary: true,
      showReadouts: true,
      showPrimary: true,
      dropped: []
    };
  }
  if (!shortViewport) {
    // Narrow but tall: rows, not controls. The scrubber and play button stay.
    return {
      showTitle: true,
      showSecondary: false,
      showReadouts: false,
      showPrimary: true,
      dropped: ['secondary', 'readouts']
    };
  }
  // Narrow AND short: the 360×640 case. Drop the title too.
  return {
    showTitle: false,
    showSecondary: false,
    showReadouts: false,
    showPrimary: true,
    dropped: ['title', 'secondary', 'readouts']
  };
}

/**
 * Whether a file needs the proxy, and the URL to use either way.
 *
 * The same membership rule the server applies, restated on the client for a
 * different reason: the client has to know BEFORE the `<video>` is constructed,
 * because a source the browser cannot decode produces a `<video>` that never
 * fires `canplay` and a user looking at a black rectangle. Deciding here means
 * the decision is made from data, and the refusal path can name the codec.
 *
 * `container` is a comma-joined list, as ffprobe reports it. Matching the whole
 * string against `'mp4'` never matches anything — see the note in
 * `commons-store/src/playback.rs`, where the same bug cost a transcode per file.
 */
export function needsProxy(source: SourceInfo): boolean {
  const containers = (source.container ?? '')
    .toLowerCase()
    .split(',')
    .map((s) => s.trim())
    .filter(Boolean);
  const has = (names: string[]) => containers.some((c) => names.includes(c));

  const video = (source.videoCodec ?? '').toLowerCase();
  const audio = (source.audioCodec ?? '').toLowerCase();

  const cOk = has(['mp4', 'm4v', 'webm']);
  const vOk = ['h264', 'avc1', 'vp8', 'vp9', 'av01'].includes(video);
  // A silent file is not a file the browser cannot play.
  const aOk = audio === '' || ['aac', 'mp3', 'opus', 'vorbis', 'flac'].includes(audio);
  if (!(cOk && vOk && aOk)) return true;

  // Recognised on all three, and the pairing has to be real: a browser will not
  // mux h264 into webm, and the list containing `webm` does not make it one.
  const isWebm = has(['webm']) && !has(['mp4', 'm4v']);
  const isMp4 = has(['mp4', 'm4v']);
  const webmPair = isWebm && ['vp8', 'vp9', 'av01'].includes(video) &&
    (audio === '' || ['opus', 'vorbis'].includes(audio));
  const mp4Pair = isMp4 && ['h264', 'avc1'].includes(video) &&
    (audio === '' || ['aac', 'mp3'].includes(audio));
  return !(webmPair || mp4Pair);
}

/**
 * What the server said a file is, as far as choosing a source goes.
 *
 * A structural mirror of the server's `MediaCaps` rather than an import, because
 * `player.ts` is pure and the transport is not -- and because the fields this
 * needs are a subset, so widening the endpoint later does not ripple here.
 */
export interface Caps {
  container: string;
  video_codec: string;
  audio_codec: string;
  /** The proxy height this file needs, or null when it needs none. */
  rung: number | null;
}

/**
 * Choose a source: direct or proxy, and the url either way.
 *
 * # Why the server's answer wins
 *
 * `caps` is the server's own probe and the same `rung_for` the proxy route
 * judges with. A client that decided differently would send a proxy request the
 * route refuses with 422 -- or skip a proxy it needs and leave a `<video>` that
 * never fires `canplay`, which a user sees as a black rectangle. So when caps is
 * present it IS the decision, and `needsProxy` is not consulted.
 *
 * # Why the local rule is the fallback and not a second opinion
 *
 * With no caps the caller could not ask, and `needsProxy(source)` is all there
 * is. It is deliberately asymmetric: with no metadata it says "proxy". A wrong
 * "direct" is a video that does not play; a wrong "proxy" is one extra transcode,
 * and it is visible in a cost rather than in a user's face. When the database
 * grows codec columns -- which it must, for a frame-accurate seek to have an fps
 * -- the fallback stops being a guess.
 *
 * # The rung is a ceiling on work, not a target
 *
 * A file needing only 480 gets 480 even when the caller asks for 1080, because a
 * rung the source does not need is an upscale, and the proxy route enforces that
 * server-side. A caller's explicit height is honoured when it is at or below the
 * rung and quietly reduced when it is not: the server would clamp it anyway, and
 * a url asking for the impossible is a worse thing to hand someone reading a log.
 */
export function playSource(
  objectId: string,
  caps: Caps | null,
  source: SourceInfo,
  proxyHeight?: number | null
): { proxied: boolean; url: string } {
  const direct = `/media/${encodeURIComponent(objectId)}`;

  if (caps) {
    if (caps.rung === null) return { proxied: false, url: direct };
    const asked =
      proxyHeight && proxyHeight > 0 ? Math.min(proxyHeight, caps.rung) : caps.rung;
    return { proxied: true, url: `${direct}/proxy.m3u8?h=${asked}` };
  }

  if (!needsProxy(source)) return { proxied: false, url: direct };
  const q = proxyHeight ? `?h=${proxyHeight}` : '';
  return { proxied: true, url: `${direct}/proxy.m3u8${q}` };
}

/**
 * When to save, and what to send.
 *
 * A periodic save, plus one on the way out. The interval is 5 seconds because
 * the cost is one tiny PUT and the cost of losing a position is the user
 * rewinding; a shorter interval buys nothing a person can perceive, and a
 * longer one loses up to that much on a crash.
 *
 * `pagehide` rather than `beforeunload`: `beforeunload` is not fired on mobile
 * Safari when a tab is discarded, which is precisely the case a resume position
 * exists for.
 */
export const SAVE_INTERVAL_MS = 5000;

export function shouldSave(
  nowMs: number,
  lastSavedAtMs: number | null,
  event: 'tick' | 'pagehide' | 'seeked' | 'pause'
): boolean {
  if (event === 'pagehide') return true;
  if (lastSavedAtMs === null) return true;
  return nowMs - lastSavedAtMs >= SAVE_INTERVAL_MS;
}

/**
 * The skip-intro window, and whether it applies to THIS file.
 *
 * Per-source (stash#634): a marker set on one file must not silently skip the
 * first thirty seconds of a different one. So the caller passes whether the
 * current object has a marker, and this returns the window only then.
 */
export function skipIntroWindow(
  hasMarker: boolean,
  marker: { startMs: number; endMs: number } | null | undefined
): { startMs: number; endMs: number } | null {
  if (!hasMarker || !marker) return null;
  const { startMs, endMs } = marker;
  if (!Number.isFinite(startMs) || !Number.isFinite(endMs) || endMs <= startMs || startMs < 0) {
    return null;
  }
  return { startMs, endMs };
}
