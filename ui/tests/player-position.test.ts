/**
 * Player position in the URL. T-P5-007 part 2, spec §15.10.
 *
 * The e2e covers the one thing that needs a browser: that a reload actually
 * seeks. These cover the rest as a table, which is where the throttle lives --
 * a throttle tested only through a real clock is a throttle that passes.
 */

import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import {
  positionFromUrl,
  positionToUrlParam,
  withPosition,
  positionWriter,
  WRITE_EPSILON_S,
  WRITE_INTERVAL_MS
} from '../src/lib/player/position.js';

describe('positionFromUrl', () => {
  test('reads plain seconds', () => {
    assert.equal(positionFromUrl('?o=x&t=42'), 42);
    assert.equal(positionFromUrl('t=42'), 42);
    assert.equal(positionFromUrl('?t=0'), 0);
  });

  test('reads the mm:ss a person would say out loud', () => {
    // A link a user reads aloud should be writable the way they say it. Every
    // media tool writes it this way, so `?t=4:12` is a link people will make.
    assert.equal(positionFromUrl('?t=4:12'), 252);
    assert.equal(positionFromUrl('?t=0:05'), 5);
    assert.equal(positionFromUrl('?t=12:34'), 754);
    assert.equal(positionFromUrl('?t=100:00'), 6000);
  });

  test('a missing t is the start, not an error', () => {
    assert.equal(positionFromUrl(''), 0);
    assert.equal(positionFromUrl('?o=abc'), 0);
  });

  test('anything unusable is the start rather than NaN', () => {
    // A link someone edited. The video plays from the start, which is what a
    // link with no `t` does -- and the user is not wrong to be confused
    // about either. Returning NaN would poison every comparison downstream,
    // and `NaN === NaN` is false, so a throttle built on it writes every time.
    for (const bad of ['?t=abc', '?t=', '?t=  ', '?t=NaN', '?t=Infinity', '?t=x:y']) {
      assert.equal(positionFromUrl(bad), 0, `${bad} should be the start`);
    }
  });

  test('a negative position is refused', () => {
    // Honouring it would seek to a clamped 0 and write THAT back, turning a
    // bad link into a silently different one.
    assert.equal(positionFromUrl('?t=-5'), 0);
  });

  test('ignores a second t', () => {
    assert.equal(positionFromUrl('?t=10&t=99'), 10);
  });
});

describe('positionToUrlParam', () => {
  test('under a minute is bare seconds', () => {
    assert.equal(positionToUrlParam(0), '0');
    assert.equal(positionToUrlParam(9), '9');
    assert.equal(positionToUrlParam(59), '59');
  });

  test('over a minute is mm:ss, zero padded', () => {
    assert.equal(positionToUrlParam(60), '1:00');
    assert.equal(positionToUrlParam(252), '4:12');
    assert.equal(positionToUrlParam(3599), '59:59');
  });

  test('fractions are floored, not rounded', () => {
    // Flooring, so the URL never names a time the video has not reached. A
    // link that says 4:12 for 251.6s is a link that does not exist.
    assert.equal(positionToUrlParam(251.9), '4:11');
  });

  test('negative is clamped to the start', () => {
    assert.equal(positionToUrlParam(-1), '0');
  });

  test('round-trips through the reader', () => {
    for (const s of [0, 5, 59, 60, 252, 754, 6000]) {
      assert.equal(positionFromUrl(`?t=${positionToUrlParam(s)}`), s);
    }
  });
});

describe('withPosition', () => {
  test('sets t and keeps the other parameters', () => {
    assert.equal(withPosition('/play?o=abc', 252), '/play?o=abc&t=4:12');
  });

  test('replaces an existing t rather than appending a second', () => {
    assert.equal(withPosition('/play?o=abc&t=1:00', 252), '/play?o=abc&t=4:12');
  });

  test('removes t at the start, so a fresh video has no stale position', () => {
    // A video that has not been played yet must not carry `t=0` in the URL:
    // it makes "resume where I left off" indistinguishable from "someone
    // shared a link to the first second", and those are different things.
    assert.equal(withPosition('/play?o=abc&t=4:12', 0), '/play?o=abc');
  });

  test('a bare path works', () => {
    assert.equal(withPosition('/play', 30), '/play?t=30');
  });

  test('the colon is not percent-encoded', () => {
    // `URLSearchParams` would write `t=4%3A12`. The link still resolves, and
    // that is exactly the problem: the reason for `mm:ss` is that a person can
    // read the position off the link, and `4%3A12` is not that. This test is
    // the only thing standing between the readable form and the correct-looking
    // one.
    assert.equal(withPosition('/play?o=abc', 252), '/play?o=abc&t=4:12');
    assert.ok(!withPosition('/play?o=abc', 252).includes('%3A'));
  });

  test('a fragment is dropped, not spliced after the query', () => {
    // `/play?o=abc&t=4:12#notes` would put the fragment after a query that a
    // naive reader then re-parses wrong. Dropping it is honest: a position is
    // not something a fragment can carry.
    assert.equal(withPosition('/play?o=abc#notes', 30), '/play?o=abc&t=30');
  });

  test('an empty query does not leave a bare question mark', () => {
    assert.equal(withPosition('/play', 0), '/play');
  });
});

describe('the throttle', () => {
  /** A writer with a hand-cranked clock. */
  const writer = () => positionWriter({ now: () => 0 });

  test('the first position always writes', () => {
    const w = writer();
    assert.deepEqual(w.update(42, 1_000), { write: true, value: 42, reason: 'moved' });
  });

  test('a second write inside the interval is refused, and says why', () => {
    const w = writer();
    w.update(42, 0);
    const d = w.update(50, WRITE_INTERVAL_MS - 1);
    assert.equal(d.write, false);
    // The reason is the point. "too soon" is fixed by waiting; "too small" is
    // fixed by lowering the epsilon; "unchanged" means there is no bug. A
    // boolean makes all three look the same.
    assert.equal(d.reason, 'too-soon');
  });

  test('a write after the interval is allowed', () => {
    const w = writer();
    w.update(42, 0);
    assert.equal(w.update(50, WRITE_INTERVAL_MS).write, true);
  });

  test('a change smaller than the epsilon is refused', () => {
    // At one second of resolution, 0.4s of progress is not a new URL worth
    // having -- and rewriting for it produces a link that differs from the
    // last one for nothing a recipient could notice.
    const w = writer();
    w.update(42, 0);
    const d = w.update(42 + WRITE_EPSILON_S - 0.1, 10_000);
    assert.equal(d.write, false);
    assert.equal(d.reason, 'too-small');
  });

  test('the same value twice writes once', () => {
    // The common case: a paused video still fires timeupdate, and a writer
    // that does not check this rewrites an identical URL forever.
    const w = writer();
    w.update(42, 0);
    const d = w.update(42, 10_000);
    assert.equal(d.write, false);
    assert.equal(d.reason, 'unchanged');
  });

  test('a backwards seek is a real move', () => {
    // The user scrubbed back. `Math.abs` means a backwards jump is not treated
    // as "too small" and the link does not go stale at the old position.
    const w = writer();
    w.update(500, 0);
    assert.equal(w.update(4, 10_000).write, true);
  });

  test('a non-finite or negative position is idle, not a write', () => {
    // Before the media loads, `currentTime` can be NaN. Writing `t=NaN` puts
    // a link in the address bar that no reader can parse.
    const w = writer();
    assert.equal(w.update(Number.NaN, 0).write, false);
    assert.equal(w.update(Number.POSITIVE_INFINITY, 0).write, false);
    assert.equal(w.update(-1, 0).write, false);
    assert.equal(w.update(Number.NaN, 0).reason, 'idle');
  });

  test('a seek forces the next write', () => {
    // The user scrubs to 5s and the URL must say 5s, not wait out the
    // interval from wherever playback happened to be.
    const w = writer();
    w.update(500, 0);
    w.update(501, 100);
    w.reset();
    assert.equal(w.update(5, 200).write, true);
  });

  test('a long video does not accumulate a history entry per frame', () => {
    // The property that makes the back button usable, stated as a count. A
    // `timeupdate` fires about four times a second, so ten minutes is ~2400
    // events; a correct writer emits about 300, and an uncorrected one emits
    // every one.
    const w = writer();
    let writes = 0;
    const TEN_MIN_AT_4HZ = 10 * 60 * 4;
    for (let i = 0; i < TEN_MIN_AT_4HZ; i += 1) {
      if (w.update(i * 0.25, i * 250).write) writes += 1;
    }
    assert.ok(
      writes <= TEN_MIN_AT_4HZ / 2,
      `expected far fewer writes than events, got ${writes} of ${TEN_MIN_AT_4HZ}`
    );
    assert.ok(writes >= 250, `expected the position to still be tracked, got ${writes}`);
  });
});
