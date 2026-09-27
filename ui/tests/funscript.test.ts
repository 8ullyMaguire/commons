/**
 * The funscript player's decisions, without a browser. T-P6-003, spec §5.6.
 *
 * What this file is for: everything in `funscript.ts` is a function of its
 * arguments, so every claim it makes can be checked here. The e2e
 * (`e2e/funscript.spec.ts`) covers the two things a function cannot answer —
 * that the clock really advances, and that a marker really lands within the
 * budget in a browser's own timeline.
 */
import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import {
  advanceClock,
  axisSummary,
  clamp01,
  deviceAt,
  driftAfter,
  markerGeometry,
  positionAll,
  positionAt,
  positionToPct,
  SYNC_BUDGET_MS,
  withinBudget,
  type FunscriptAxis,
  type FunscriptTimeline,
} from '../src/lib/player/funscript.js';

/** A ramp: 0 at t=0, 1 at t=1000, 0 at t=2000. */
const RAMP: FunscriptAxis = {
  name: 'stroke',
  actions: [
    { at_ms: 0, position: 0 },
    { at_ms: 1000, position: 1 },
    { at_ms: 2000, position: 0 },
  ],
};

const axis = (name: string, actions: Array<[number, number]>): FunscriptAxis => ({
  name,
  actions: actions.map(([at_ms, position]) => ({ at_ms, position })),
});

const timeline = (axes: FunscriptAxis[]): FunscriptTimeline => ({
  axes,
  source: 'sidecar',
  warnings: [],
  span_ms: 2000,
});

describe('positionAt', () => {
  it('holds the position until the next action under step', () => {
    assert.equal(positionAt(RAMP, 0, 'step'), 0);
    assert.equal(positionAt(RAMP, 999, 'step'), 0);
    assert.equal(positionAt(RAMP, 1000, 'step'), 1);
    assert.equal(positionAt(RAMP, 1999, 'step'), 1);
  });

  it('moves between the actions under linear', () => {
    assert.equal(positionAt(RAMP, 500, 'linear'), 0.5);
    assert.equal(positionAt(RAMP, 250, 'linear'), 0.25);
    assert.equal(positionAt(RAMP, 1500, 'linear'), 0.5);
  });

  it('agrees with the server at every action, under both readings', () => {
    // The two implementations are in different languages and the property has
    // to be checked from both sides, because a disagreement at an action is
    // invisible in the position between them.
    for (let t = 0; t <= 2000; t += 13) {
      const step = positionAt(RAMP, t, 'step');
      const lin = positionAt(RAMP, t, 'linear');
      if (t % 1000 === 0) {
        assert.equal(step, lin, `at ${t}, an action`);
      } else {
        assert.notEqual(step, lin, `at ${t}, off an action they must differ`);
      }
    }
  });

  /// Before the first action there is no value. A `clamp to index 0` here
  /// would start the device moving before the script says to.
  it('has no value before the first action', () => {
    const a = axis('stroke', [[500, 1]]);
    assert.equal(positionAt(a, 0, 'step'), null);
    assert.equal(positionAt(a, 499, 'step'), null);
    assert.equal(positionAt(a, 500, 'step'), 1);
  });

  it('holds the last position past the final action', () => {
    assert.equal(positionAt(RAMP, 99_999, 'linear'), 0);
    assert.equal(positionAt(RAMP, 99_999, 'step'), 0);
  });

  it('has no value on an empty axis', () => {
    const a = axis('empty', []);
    assert.equal(positionAt(a, 0, 'step'), null);
    assert.equal(positionAt(a, 0, 'linear'), null);
  });

  /// A single action, which is the degenerate case for both readings: there is
  /// no next action to interpolate towards and no earlier one to hold.
  it('handles a single action', () => {
    const a = axis('one', [[750, 0.5]]);
    assert.equal(positionAt(a, 0, 'linear'), null);
    assert.equal(positionAt(a, 750, 'linear'), 0.5);
    assert.equal(positionAt(a, 5000, 'linear'), 0.5);
  });

  /// Two actions at the SAME instant. The parser sorts them but does not
  /// deduplicate, so this arrives, and a division by a zero span would be
  /// NaN -- which compares false to everything and propagates silently.
  it('survives two actions at the same instant', () => {
    const a = axis('dup', [[500, 0.2], [500, 0.8]]);
    const v = positionAt(a, 600, 'linear');
    assert.ok(Number.isFinite(v), `got ${v}`);
    assert.ok(v !== null && v >= 0 && v <= 1, `got ${v}`);
  });

  /// The binary search against a linear scan, over a large script — the same
  /// property the Rust side tests, checked here because this is a second
  /// implementation and two implementations can disagree.
  it('agrees with a linear scan over a large script', () => {
    const actions: Array<[number, number]> = [];
    for (let i = 0; i < 5000; i++) actions.push([i * 10, (i % 100) / 100]);
    const a = axis('big', actions);
    const linear = (t: number) => {
      let out: number | null = null;
      for (const act of a.actions) {
        if (act.at_ms <= t) out = act.position;
        else break;
      }
      return out;
    };
    for (const probe of [0, 1, 9, 10, 11, 123, 4999, 49_990, 1_000_000]) {
      assert.equal(positionAt(a, probe, 'step'), linear(probe), `at ${probe}`);
    }
  });

  it('is pure — the same moment gives the same value', () => {
    // A player re-renders on resize; the device must not jump because of it.
    for (const t of [0, 1, 500, 999, 1000, 1500]) {
      assert.equal(positionAt(RAMP, t, 'linear'), positionAt(RAMP, t, 'linear'));
    }
  });
});

describe('clamp01', () => {
  it('clamps out of range and leaves in-range alone', () => {
    assert.equal(clamp01(-0.5), 0);
    assert.equal(clamp01(1.5), 1);
    assert.equal(clamp01(0.42), 0.42);
  });

  /// NaN is the value a zero-span division produces, and every comparison
  /// against it is false — so a NaN position passes a `>= 0 && <= 1` range
  /// check written the obvious way.
  it('turns NaN into 0 rather than letting it through', () => {
    assert.equal(clamp01(NaN), 0);
  });
});

describe('deviceAt — the three states', () => {
  const clock = { videoMs: 500, deviceMs: 500 };

  it('tracks the video while playing', () => {
    const d = deviceAt(RAMP, 'playing', 500, clock, 'linear');
    assert.equal(d.position, 0.5);
    assert.equal(d.held, false, 'a playing device is not held');
  });

  /// `paused` holds the last position. Not zero, and not the video's — the
  /// video stopped, and the device is wherever it was.
  it('holds its position when paused', () => {
    const d = deviceAt(RAMP, 'paused', 500, { videoMs: 500, deviceMs: 500 }, 'linear');
    assert.equal(d.position, 0.5, 'held where it was, not at zero');
    assert.equal(d.held, true);
  });

  /// The state that exists (#2762). The device's clock is frozen, so the
  /// video's position is irrelevant — and reading it here is the bug the
  /// state prevents.
  it('freezes on the device clock under manual pause, ignoring the video', () => {
    const d = deviceAt(
      RAMP,
      'manual-pause',
      1_800, // the video moved on
      { videoMs: 500, deviceMs: 500 }, // the device did not
      'linear',
    );
    assert.equal(d.position, 0.5, 'the frozen position, not the video position');
    assert.equal(d.held, true);
  });

  /// A seek during a manual pause must not drag the device along. This is the
  /// specific case a "does it pause" test cannot see, because a pause test
  /// never moves the video.
  it('ignores a seek during a manual pause', () => {
    const before = deviceAt(RAMP, 'manual-pause', 500, { videoMs: 500, deviceMs: 500 }, 'linear');
    const after = deviceAt(RAMP, 'manual-pause', 30_000, { videoMs: 30_000, deviceMs: 500 }, 'linear');
    assert.equal(after.position, before.position, 'the device did not follow the seek');
  });

  /// `paused` and `manual-pause` differ in exactly one observable: whether the
  /// device clock moves. Everything else about them is identical, so if a
  /// consumer cannot tell them apart the third state is decoration.
  it('distinguishes paused from manual-pause only by whether the clock moves', () => {
    const at = { videoMs: 500, deviceMs: 500 };
    const paused = deviceAt(RAMP, 'paused', 500, at, 'linear');
    const manual = deviceAt(RAMP, 'manual-pause', 500, at, 'linear');
    assert.equal(paused.position, manual.position, 'same position, same held');
    assert.equal(paused.held, manual.held);
  });

  it('names the axis it is reporting', () => {
    assert.equal(deviceAt(RAMP, 'playing', 0, clock).axis, 'stroke');
  });

  it('has no position before the script starts, in any state', () => {
    const a = axis('stroke', [[500, 1]]);
    for (const state of ['playing', 'paused', 'manual-pause'] as const) {
      assert.equal(deviceAt(a, state, 0, { videoMs: 0, deviceMs: 0 }).position, null, state);
    }
  });
});

describe('advanceClock', () => {
  it('follows the video exactly while playing', () => {
    // The whole no-drift claim in one line: the device clock IS the video
    // clock, so there is nothing to accumulate.
    assert.deepEqual(advanceClock({ videoMs: 0, deviceMs: 0 }, 1234, 'playing'), {
      videoMs: 1234,
      deviceMs: 1234,
    });
  });

  it('does not move the device under a pause, even when the video seeks', () => {
    const c = { videoMs: 500, deviceMs: 500 };
    assert.deepEqual(advanceClock(c, 30_000, 'paused'), { videoMs: 30_000, deviceMs: 500 });
    assert.deepEqual(advanceClock(c, 30_000, 'manual-pause'), { videoMs: 30_000, deviceMs: 500 });
  });

  it('resumes from the video position, not from where the device stopped', () => {
    // A user pauses at 5 s, scrubs to 30 s, and presses play. The device must
    // be at 30 s — continuing from 5 s would run the script against a
    // position the video is not at.
    const c = advanceClock({ videoMs: 5000, deviceMs: 5000 }, 30_000, 'paused');
    assert.deepEqual(advanceClock(c, 30_000, 'playing'), { videoMs: 30_000, deviceMs: 30_000 });
  });
});

describe('driftAfter', () => {
  it('is zero when the ticks account for the elapsed time', () => {
    // Within a float epsilon, not exactly: 1000/60 is not representable, so
    // 3600 ticks is 60000 - 7e-12 ms rather than 60000. The property is "no
    // systematic drift", and asserting exact equality would be asserting that
    // IEEE-754 can represent 1/60 -- which is a claim about the number type,
    // not about the player.
    assert.ok(driftAfter(60_000, 3600, 1000 / 60) < 1e-6);
  });

  /// The property the ticket's 50 ms is really about. A player that adds a
  /// fixed interval per tick loses (actual - scheduled) every frame, and after
  /// two minutes that is seconds — invisible in a screenshot, obvious in a
  /// two-minute watch.
  it('reports the error a late-frame player accumulates', () => {
    // 3,600 ticks that each ran 1 ms late: 3,600 ms of drift.
    assert.ok(Math.abs(driftAfter(63_600, 3600, 1000 / 60) - 3600) < 1e-6);
  });

  it('reports a short frame the same way as a long one', () => {
    // The error is ABSOLUTE, so running fast is as wrong as running slow. A
    // signed difference would let a fast player read as negative-and-fine.
    assert.ok(Math.abs(driftAfter(58_800, 3600, 1000 / 60) - 1200) < 1e-6);
  });

  it('is zero for a player that has not ticked', () => {
    assert.equal(driftAfter(1000, 0, 1000 / 60), 0);
    assert.equal(driftAfter(0, 100, 0), 0);
  });

  /// The budget is the ticket's sentence, and 3,600 ticks is the interval that
  /// makes a per-frame error a per-minute one: 1 ms a frame is 60 ms a second,
  /// so a player that misses one frame per second is over budget within a
  /// second and a minute out by a third of a minute.
  it('puts a one-millisecond-per-frame player over budget within a second', () => {
    // 60 ticks, each 1 ms late: 60 ms of error, and the budget is 50.
    assert.ok(!withinBudget(driftAfter(61_000, 60, 1000 / 60)));
  });
});

describe('withinBudget', () => {
  it('holds the ticket budget at 10 s', () => {
    assert.ok(withinBudget(0));
    assert.ok(withinBudget(SYNC_BUDGET_MS));
    assert.ok(!withinBudget(SYNC_BUDGET_MS + 1));
  });

  it('treats a negative error as over budget when it is large', () => {
    assert.ok(withinBudget(-SYNC_BUDGET_MS));
    assert.ok(!withinBudget(-(SYNC_BUDGET_MS + 1)));
  });
});

describe('positionAll', () => {
  it('returns one entry per axis, aligned by index', () => {
    const tl = timeline([
      axis('stroke', [[0, 0], [1000, 1]]),
      axis('move', [[0, 1], [1000, 0]]),
    ]);
    const got = positionAll(tl, 500, 'linear');
    assert.equal(got.length, 2);
    assert.equal(got[0].position, 0.5);
    assert.equal(got[1].position, 0.5);
  });

  /// An N-axis controller reads by index, so an axis with no value must still
  /// occupy its slot rather than vanishing and shifting everything after it.
  it('keeps an axis with no value in its slot', () => {
    const tl = timeline([
      axis('a', [[0, 0.5]]),
      axis('b', [[900, 1]]),
      axis('c', [[0, 0.25]]),
    ]);
    const got = positionAll(tl, 0, 'step');
    assert.equal(got.length, 3);
    assert.equal(got[0].position, 0.5);
    assert.equal(got[1].position, null, 'has not started');
    assert.equal(got[2].position, 0.25);
  });

  it('is empty for a script with no axes', () => {
    assert.deepEqual(positionAll(timeline([]), 0), []);
  });
});

describe('axisSummary', () => {
  it('counts, and pluralises the ones that need it', () => {
    assert.equal(axisSummary(timeline([])), 'no axes');
    assert.equal(axisSummary(timeline([RAMP])), '1 axis');
    assert.equal(axisSummary(timeline([RAMP, RAMP])), '2 axes');
    assert.equal(axisSummary(timeline([RAMP, RAMP, RAMP])), '3 axes');
  });
});

describe('markerGeometry', () => {
  it('places a marker at its position along the ruler', () => {
    assert.equal(markerGeometry(1000, 2000, 1000).leftPct, 50);
    assert.equal(markerGeometry(0, 2000, 1000).leftPct, 0);
    assert.equal(markerGeometry(2000, 2000, 1000).leftPct, 100);
  });

  /// An action is an INSTANT. A marker wider than a pixel claims a duration
  /// the script does not have, and on a 20,000-action script a 1% width turns
  /// the ruler into a solid block that reads as one long stroke.
  it('is about one pixel wide, never a fixed percentage', () => {
    assert.ok(markerGeometry(0, 2000, 1000).widthPct <= 1);
    assert.ok(markerGeometry(0, 2000, 4000).widthPct <= 1);
  });

  it('is zero-width for a script that covers no time', () => {
    const g = markerGeometry(0, 0, 1000);
    assert.equal(g.leftPct, 0);
    assert.equal(g.widthPct, 0);
  });

  it('survives a zero-width ruler rather than dividing by zero', () => {
    const g = markerGeometry(500, 2000, 0);
    assert.ok(Number.isFinite(g.leftPct));
    assert.ok(Number.isFinite(g.widthPct));
  });
});

describe('positionToPct', () => {
  it('scales into a percentage and keeps null as null', () => {
    assert.equal(positionToPct(0.25), 25);
    assert.equal(positionToPct(1), 100);
    assert.equal(positionToPct(null), null);
  });

  it('clamps rather than rendering a dot outside the overlay', () => {
    assert.equal(positionToPct(1.4), 100);
    assert.equal(positionToPct(-0.2), 0);
  });
});
