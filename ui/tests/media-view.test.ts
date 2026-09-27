/**
 * The tile-shape function. Spec 10.4, plan T-P5-006 item 8.
 *
 * # Why this is a pure function and not a component
 *
 * The whole decision surface of a media view is one question per row: what
 * shape is this tile, and is it playable? That question has a wrong answer in
 * several ways that are all invisible in a screenshot — a row that renders at
 * zero height, a strip that makes the viewport 40000px tall, an aspect ratio
 * inverted so every tile is a letterbox. Each of those is a number, so the
 * function that produces the number is where the test belongs, and the DOM is
 * left to draw it.
 */

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { tileShape, ASPECT_LIMITS } from '../src/lib/api/media-view.js';
import { makeRow } from './helpers/row.js';

/** The row factory, from the one place every UI test builds a row. */
const row = makeRow;

/** The default target: the 2:3 poster shape most libraries are full of. */
const T = 2 / 3;

test('a row with no dimensions gets the target ratio rather than a division by zero', () => {
  // A scan that has not run yet. `width: null` is a real row, and treating it
  // as 0 is how one unprobed object makes the whole grid render at zero height.
  const r = row({ width: null, height: null });
  const s = tileShape(r, T);
  assert.equal(s.aspect, T, 'the target, not NaN');
  assert.ok(Number.isFinite(s.aspect), 'and never a non-number');
  assert.equal(s.kind, 'unknown', 'nothing is known about it yet');
});

test('a degenerate dimension is clamped rather than becoming the row height', () => {
  // height: 0 is a zero-division; a 10000:1 strip is a real probe result on a
  // banner scan. Either one left unclamped makes the viewport enormous.
  for (const bad of [
    { width: 800, height: 0 },
    { width: 0, height: 1200 },
    { width: 800, height: -5 },
    { width: 10000, height: 1 }
  ]) {
    const s = tileShape(row(bad), T);
    assert.ok(
      s.aspect >= ASPECT_LIMITS.min && s.aspect <= ASPECT_LIMITS.max,
      `clamped: ${bad.width}x${bad.height} gave ${s.aspect}`
    );
  }
});

test('the aspect is width divided by height, and the test says which way round', () => {
  // A portrait is TALLER than wide, so its ratio is BELOW 1. A ratio above 1
  // for a portrait row is the width/height inversion, and it is invisible in a
  // test that only checks the number is positive.
  const portrait = tileShape(row({ width: 800, height: 1200 }), T);
  assert.ok(portrait.aspect < 1, '800x1200 is portrait, so aspect < 1');

  const landscape = tileShape(row({ width: 1920, height: 1080 }), T);
  assert.ok(landscape.aspect > 1, '1920x1080 is landscape, so aspect > 1');

  // And the value is the actual quotient, not merely the right side of 1.
  assert.equal(portrait.aspect, 800 / 1200);
  assert.equal(landscape.aspect, 1920 / 1080);
});

test('a duration is what makes a row playable, not a file extension', () => {
  // An extension is a claim about a filename. A duration is a fact about the
  // content, and the row carries one.
  const video = tileShape(row({ kind: 'Scene', durationMs: 12_000 }), T);
  assert.equal(video.kind, 'video');
  assert.equal(video.playable, true, 'something to play');

  // A file that claims to be a video and never probed is unknown, NOT a video
  // that will not play when pressed.
  const unprobed = tileShape(row({ kind: 'Scene', durationMs: null, coverPath: null }), T);
  assert.notEqual(unprobed.kind, 'video', 'no duration means no claim of video');
  assert.equal(unprobed.playable, false);
});

test('a zero duration is not a duration', () => {
  // A probe that FAILS can report 0 rather than NULL. Rendering that as a
  // playable zero-length video produces a tile that opens onto nothing.
  //
  // Note what the kind is: this row has dimensions, so it is a still, not a
  // mystery. The first version of this test asserted `unknown` and was wrong --
  // the failure to measure a duration says nothing about what the file is, it
  // only means there is nothing to play. A row with no duration AND no
  // dimensions is the actual `unknown`, and the last test covers that.
  const s = tileShape(row({ durationMs: 0 }), T);
  assert.equal(s.playable, false, 'a zero-length video is not playable');
  assert.equal(s.kind, 'image', 'dimensions still identify it as a still');
});

test('a row with nothing measured at all is the only unknown', () => {
  // No duration, no cover, no dimensions: nothing has been probed, so the view
  // genuinely cannot say what this is. This is the case that falls back to the
  // declared ratio and is the one worth a distinct badge.
  const s = tileShape(row({ width: null, height: null, durationMs: null, coverPath: null }), T);
  assert.equal(s.kind, 'unknown');
  assert.equal(s.playable, false);
});

test('an image is an image whether or not it has been probed', () => {
  const s = tileShape(row({ kind: 'Image', width: 640, height: 480 }), T);
  assert.equal(s.kind, 'image');
  assert.equal(s.playable, false, 'a still has nothing to play');
});

test('the ratio survives being asked twice', () => {
  const r = row();
  assert.deepEqual(tileShape(r, T), tileShape(r, T), 'no hidden state');
});
