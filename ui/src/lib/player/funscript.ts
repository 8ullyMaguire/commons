/**
 * The funscript player: timing sync, and the three states (#2762).
 *
 * T-P6-003, spec §5.6. Answers stash#3031's browser half: a funscript drives
 * a position over time, and the position has to agree with the video.
 *
 * # What is in here and what is not
 *
 * Every decision a player makes is a function here: which interpolation, what
 * a given video time means, what the device should do in each of the three
 * states, and where a script's actions fall on a ruler. `FunscriptPlayer
 * .svelte` holds the DOM and nothing else, which is the rule the whole
 * `ui/src/lib` layout follows and the reason a browser is not needed to test
 * any of this.
 *
 * # The claim the ticket actually makes, and why it is a claim at all
 *
 * "A marker at t=10 s fires within 50 ms of the scripted position." A player
 * that is *merely* synchronised to within a frame looks fine at any single
 * moment you check, and drifts. The failure is a rate error, not an offset, so
 * it is invisible in a screenshot and obvious over two minutes — which means a
 * test that samples one position cannot see it. [`driftAfter`] is the function
 * that makes it seeable, and the e2e drives it against a fake clock.
 *
 * # Manual pause is a third state, not a boolean
 *
 * #2762 asks for a pause that stops the *device* as well as the video. That is
 * not `video.pause()`: a script that keeps running while the video is paused
 * drives a toy through a scene nobody is watching, and the third state is
 * distinguishable from the second by the position being **frozen** rather than
 * merely idle.
 *
 * The distinction is only worth making if something can tell the two apart, so
 * [`deviceAt`] is the function that answers "where should the device be, given
 * the video clock and the state", and a test asserts the freeze rather than
 * the pause. A pause test passes against an implementation that stopped
 * nothing.
 */

/** One action as the server sends it: the script's own `at`/`pos`. */
export interface FunscriptAction {
  at_ms: number;
  position: number;
}

/** One named axis. #6339: a script is N of these, not one. */
export interface FunscriptAxis {
  name: string;
  actions: FunscriptAction[];
}

/** The whole timeline, as `GET /media/:id/funscripts/:script_id` returns it. */
export interface FunscriptTimeline {
  axes: FunscriptAxis[];
  source: string;
  warnings: string[];
  span_ms: number;
}

/**
 * How the space between two actions is filled.
 *
 * Must match the server's `Interpolation`, because the two answers must agree:
 * a device driven from a client-side reading and an overlay drawn from the
 * server's would show two different positions at the same instant if the
 * client picked its own.
 */
export type Interpolation = 'step' | 'linear';

/**
 * The player's three states.
 *
 * - `playing` — video and device both advancing.
 * - `paused` — video stopped by the transport. The device **holds**.
 * - `manual-pause` — #2762. The device's position is **frozen**, not merely
 *   idle, and the distinction is what the state exists to record.
 */
export type PlayState = 'playing' | 'paused' | 'manual-pause';

/** What the device is being told to do right now. */
export interface DeviceOutput {
  /** The position on the axis, or null when the script has not started. */
  position: number | null;
  /** Which axis this applies to. */
  axis: string;
  /**
   * True when the value is held rather than advancing. A consumer that drives
   * hardware should not re-send a held position on every frame — and one that
   * draws a debug readout wants to show it, so the distinction is on the wire
   * rather than inferred from two equal samples.
   */
  held: boolean;
}

/** The time a player has been advancing, which is not the video's clock. */
export interface DeviceClock {
  /** The video time the device was last told about, in ms. */
  videoMs: number;
  /** The device's own position, which freezes under manual pause. */
  deviceMs: number;
}

/**
 * The position on one axis at a video time.
 *
 * Returns null before the first action — the script has not started, and
 * inventing a value is what a naive `clamp to index 0` does. A device given a
 * position before the script says to move has been told something false.
 */
export function positionAt(
  axis: FunscriptAxis,
  videoMs: number,
  interpolation: Interpolation = 'step',
): number | null {
  const a = axis.actions;
  if (a.length === 0) return null;
  if (videoMs < a[0].at_ms) return null;

  // The last action at or before `videoMs`. A binary search: a player calls
  // this every frame and a 20,000-action script at 60 Hz is 1.2 million linear
  // comparisons a minute.
  let lo = 0;
  let hi = a.length - 1;
  while (lo < hi) {
    const mid = (lo + hi + 1) >> 1;
    if (a[mid].at_ms <= videoMs) lo = mid;
    else hi = mid - 1;
  }
  const here = a[lo];

  if (interpolation === 'step') return here.position;
  if (lo === a.length - 1) return here.position;

  const next = a[lo + 1];
  const span = next.at_ms - here.at_ms;
  if (span <= 0) return here.position;
  const frac = (videoMs - here.at_ms) / span;
  return clamp01(here.position + (next.position - here.position) * frac);
}

/** The position on every axis at a video time, aligned by index. */
export function positionAll(
  timeline: FunscriptTimeline,
  videoMs: number,
  interpolation: Interpolation = 'step',
): Array<{ axis: string; position: number | null }> {
  return timeline.axes.map((axis) => ({
    axis: axis.name,
    position: positionAt(axis, videoMs, interpolation),
  }));
}

/** A value into 0..=1, because the axis is defined as that range. */
export function clamp01(v: number): number {
  if (Number.isNaN(v)) return 0;
  return v < 0 ? 0 : v > 1 ? 1 : v;
}

/**
 * Where the device should be, given the video clock and the player's state.
 *
 * The three states, and the reason they are three:
 *
 * - `playing` — the device tracks the video.
 * - `paused` — the device **holds its last position**. It does not track the
 *   video, which has stopped, and it does not rewind to zero.
 * - `manual-pause` — the device's clock is **frozen**, so the position it
 *   reports is the one from when the pause began, whatever the video does
 *   afterwards. If the user scrubs while manually paused, the device does not
 *   follow — that is the whole point, and a scrub to t=30 must not drive the
 *   device to t=30.
 *
 * `held` is true in both paused states and false while playing, and a consumer
 * that re-sends a held position every frame is talking to a toy that is
 * already there.
 */
export function deviceAt(
  axis: FunscriptAxis,
  state: PlayState,
  videoMs: number,
  clock: DeviceClock,
  interpolation: Interpolation = 'step',
): DeviceOutput {
  if (state === 'manual-pause') {
    // The frozen clock, NOT the video's. Reading the video here is the bug
    // this state exists to prevent: a scrub during a manual pause would
    // otherwise drag the device along with it.
    const frozen = positionAt(axis, clock.deviceMs, interpolation);
    return { position: frozen, axis: axis.name, held: true };
  }
  if (state === 'paused') {
    const held = positionAt(axis, clock.videoMs, interpolation);
    return { position: held, axis: axis.name, held: true };
  }
  return {
    position: positionAt(axis, videoMs, interpolation),
    axis: axis.name,
    held: false,
  };
}

/**
 * Advance the device clock.
 *
 * Under `playing` it follows the video exactly. Under either paused state it
 * does not move — and that is the freeze, stated as the one rule rather than
 * as two branches that happen to agree.
 */
export function advanceClock(
  clock: DeviceClock,
  videoMs: number,
  state: PlayState,
): DeviceClock {
  if (state === 'playing') {
    return { videoMs, deviceMs: videoMs };
  }
  // Paused: the video may have moved (a seek) and the device must not.
  return { videoMs, deviceMs: clock.deviceMs };
}

/**
 * The time error after `elapsedMs` of playback, in ms.
 *
 * The claim: a player that samples on `requestAnimationFrame` and reads
 * `video.currentTime` accumulates **no** systematic drift, because both are
 * driven by the same media clock. So this returns the *absolute* difference
 * between the video's clock and the device's, and a correct implementation
 * holds it at zero however long it runs.
 *
 * It exists as a function because the property is about a RATE, and a rate
 * cannot be observed from one sample: the ticket's 50 ms at t=10 s is a
 * statement about what happens at t=610 s too, and a test that only checks the
 * first one passes against a player that loses a frame per second.
 */
export function driftAfter(elapsedMs: number, ticks: number, intervalMs: number): number {
  if (ticks <= 0 || intervalMs <= 0) return 0;
  const scheduled = ticks * intervalMs;
  // The player's own accounting, and the media clock's. A frame that runs late
  // is not counted twice, which is the whole failure mode: a player that adds
  // a fixed interval per tick drifts by (actual - scheduled) per tick, and
  // after 3,600 ticks at 60 Hz that is a visible, growing error.
  return Math.abs(scheduled - elapsedMs);
}

/** The tick interval a player should aim for, and the one it must not miss. */
export const TICK_MS = 1000 / 60;
/** The ticket's budget: a marker at t=10 s fires within this. */
export const SYNC_BUDGET_MS = 50;

/**
 * Whether a player is within budget at `t` seconds.
 *
 * Expressed as a function so the e2e asserts the ticket's own sentence
 * rather than a number typed twice, and so the budget lives in one place: a
 * 50 ms tolerance written into three test files is three places to change it
 * and two that will be forgotten.
 */
export function withinBudget(errorMs: number): boolean {
  return Math.abs(errorMs) <= SYNC_BUDGET_MS;
}

/** The 2-axis overlay's label, and what a script with a different count says. */
export function axisSummary(timeline: FunscriptTimeline): string {
  const n = timeline.axes.length;
  if (n === 0) return 'no axes';
  if (n === 1) return '1 axis';
  if (n === 2) return '2 axes';
  return `${n} axes`;
}

/**
 * The width of a marker on a ruler, in percent, and its left edge.
 *
 * An action is an INSTANT, so a marker wider than a pixel claims a duration the
 * script does not have — and on a 20,000-action script a 1% width makes every
 * marker overlap its neighbour into a solid block, which reads as "the script
 * is one long stroke" and is the opposite of what the data says.
 */
export function markerGeometry(
  atMs: number,
  spanMs: number,
  rulerPx: number,
): { leftPct: number; widthPct: number } {
  if (spanMs <= 0) return { leftPct: 0, widthPct: 0 };
  const leftPct = clamp01(atMs / spanMs) * 100;
  // One device pixel, expressed as a percentage of the ruler, and never more
  // than the gap to the next marker. A marker that overlaps its neighbour is
  // worse than one that is a hair too thin.
  const onePxPct = rulerPx > 0 ? (1 / rulerPx) * 100 : 0;
  return { leftPct, widthPct: Math.min(onePxPct, 2) };
}

/** A position as a CSS percentage, for the overlay. */
export function positionToPct(position: number | null): number | null {
  return position === null ? null : clamp01(position) * 100;
}
