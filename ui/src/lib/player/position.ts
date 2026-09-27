/**
 * Player position in the URL. T-P5-007 part 2, spec §15.10.
 *
 * # The problem
 *
 * A video's position is a view state by the ticket's own sentence -- "every
 * view state is a resolvable URL: filters, sorts, view mode, tagger selection,
 * player position" -- and a link to `t=4:12` is the difference between sending
 * someone the part you liked and sending them the file.
 *
 * # Why this is a function and not a `$effect`
 *
 * Because the interesting behaviour is WHEN NOT to write. A `timeupdate` fires
 * roughly four times a second, so writing the URL on each one produces a
 * `replaceState` per frame: the address bar flickers, the browser's own
 * session history fills with hundreds of entries for one video, and the back
 * button walks through a single file's playback in quarter-second steps. That
 * is a worse bug than having no position in the URL at all.
 *
 * So the rule is: write when the position has actually moved, at most once per
 * interval, and always with `replaceState` -- never `pushState`. Both halves
 * are load-bearing and both are table-tested below.
 */

/** How often the URL may be rewritten, in ms. */
export const WRITE_INTERVAL_MS = 2_000;

/**
 * The smallest change worth recording, in seconds.
 *
 * Below this the write is noise: at one second of resolution a 4:12 link is
 * "4:12" either way, and rewriting for 0.4s of progress produces a URL that
 * differs from the previous one for no reason a recipient could notice.
 */
export const WRITE_EPSILON_S = 1;

/**
 * Read a position out of a URL.
 *
 * Returns 0 for anything unusable rather than throwing or returning NaN. A
 * malformed `t` is a link someone edited, not an error worth showing them --
 * the video plays from the start, which is what a link with no `t` does, and
 * the user is not wrong to be confused about either.
 *
 * Negative is rejected for the same reason: a position before the start is a
 * typo, and honouring it would seek to a clamped 0 and write that back,
 * turning a bad link into a silently different one.
 */
export function positionFromUrl(search: string): number {
  const raw = new URLSearchParams(search).get('t');
  if (raw === null || raw.trim() === '') return 0;
  // Accept `4:12` as well as `252`. A link a person reads out loud should be
  // writable the way they say it, and every media tool writes it this way.
  const colon = raw.indexOf(':');
  const seconds =
    colon === -1
      ? Number(raw)
      : Number(raw.slice(0, colon)) * 60 + Number(raw.slice(colon + 1));
  if (!Number.isFinite(seconds) || seconds < 0) return 0;
  return seconds;
}

/** Format a position for the URL: `252` becomes `4:12`. */
export function positionToUrlParam(seconds: number): string {
  const whole = Math.max(0, Math.floor(seconds));
  const m = Math.floor(whole / 60);
  const s = whole % 60;
  return m === 0 ? String(s) : `${m}:${String(s).padStart(2, '0')}`;
}

/**
 * A URL with `t` set, or removed when the position is zero.
 *
 * Handled by string rather than `URLSearchParams`, for one reason that took a
 * test to find: `searchParams.set('t', '4:12')` writes `t=4%3A12`. The link
 * still works -- the reader decodes it -- but the whole point of `mm:ss` is
 * that a person can read the position off the link, and `4%3A12` is not that.
 * Percent-encoding a character that is legal in a query value is correct
 * behaviour and the wrong outcome here.
 *
 * The `#` is stripped because a fragment is not part of a query and splicing
 * one after it produces a URL that is subtly wrong rather than obviously
 * broken.
 */
export function withPosition(href: string, seconds: number): string {
  const [beforeHash] = href.split('#');
  const [path, query = ''] = beforeHash.split('?');
  const kept = query
    .split('&')
    .filter((kv) => kv !== '' && !kv.startsWith('t='));
  if (seconds > 0) kept.push(`t=${positionToUrlParam(seconds)}`);
  return kept.length > 0 ? `${path}?${kept.join('&')}` : path;
}

/** What `update` was told, and whether the caller should write. */
export interface WriteDecision {
  /** True when the URL should be rewritten now. */
  readonly write: boolean;
  /** The value to write, when `write` is true. */
  readonly value: number;
  /** Why not, for the failure message and for anyone reading the test. */
  readonly reason: 'moved' | 'too-soon' | 'too-small' | 'unchanged' | 'idle';
}

export interface PositionWriterOptions {
  intervalMs?: number;
  epsilonS?: number;
  /** Injectable clock, so the throttle is testable without a real timer. */
  now?: () => number;
}

/**
 * The throttled writer.
 *
 * A closure rather than a class because there is no state a caller needs to
 * read back, and a class would invite someone to read `lastWritten` and build
 * a second source of truth on top of it.
 *
 * `push` is deliberately not offered. Every caller wants `replace`; a writer
 * that could push would have one caller that does, and that caller's back
 * button would be broken in a way nobody could reproduce from reading it.
 */
export function positionWriter(options: PositionWriterOptions = {}) {
  const intervalMs = options.intervalMs ?? WRITE_INTERVAL_MS;
  const epsilonS = options.epsilonS ?? WRITE_EPSILON_S;
  const now = options.now ?? (() => Date.now());

  let lastAt = Number.NEGATIVE_INFINITY;
  let lastValue: number | null = null;

  return {
    /** Force the next `update` to write. Used when the user seeks. */
    reset(): void {
      lastAt = Number.NEGATIVE_INFINITY;
      lastValue = null;
    },

    /**
     * Decide whether to write `seconds` at time `at`.
     *
     * Returns a decision rather than a boolean so a test can assert the
     * REASON, which is the only way to tell "throttled" apart from "the value
     * did not change" -- two failures with the same symptom and opposite fixes.
     */
    update(seconds: number, at: number = now()): WriteDecision {
      if (!Number.isFinite(seconds) || seconds < 0) {
        return { write: false, value: 0, reason: 'idle' };
      }
      if (lastValue !== null && seconds === lastValue) {
        return { write: false, value: seconds, reason: 'unchanged' };
      }
      if (lastValue !== null && Math.abs(seconds - lastValue) < epsilonS) {
        return { write: false, value: seconds, reason: 'too-small' };
      }
      if (at - lastAt < intervalMs) {
        return { write: false, value: seconds, reason: 'too-soon' };
      }
      lastAt = at;
      lastValue = seconds;
      return { write: true, value: seconds, reason: 'moved' };
    }
  };
}
