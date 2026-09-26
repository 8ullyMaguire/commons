/*
  Gesture recognition. Spec 10.2, stash#7147/#7148/#7149/#7154.

  # The boundaries, not a comfortable distance

  Each threshold is tested at n-1, n, and n+1. A test at slop*10 would pass for
  a constant off by a factor of ten, and an off-by-a-factor threshold is
  exactly what a threshold drifts to when someone tunes it by feel.
*/

import { test, describe } from 'node:test';
import assert from 'node:assert/strict';

import {
  classifyDrag,
  classifyPopstate,
  classifyWheel,
  FLICK_MS,
  FLICK_SLOP,
  WHEEL_NAVIGATE_DELTA,
  type PointerSample
} from '../src/lib/api/gestures.js';

const at = (x: number, y: number, t: number): PointerSample => ({ x, y, t });

describe('#7147 a fast drag must not navigate', () => {
  test('a drag past the slop is a pan however fast it was', () => {
    // 400px in 20ms: a real pan, and the case the upstream bug turned into a
    // page turn. The duration is *shorter* than FLICK_MS on purpose, because
    // "fast" is what makes it look like a flick to a naive implementation.
    assert.equal(classifyDrag(at(0, 0, 0), at(400, 0, 20)).kind, 'pan');
  });

  test('the distance is measured from the press, not from the moves', () => {
    // The mechanism of the bug. A recognizer that summed deltas would see only
    // whichever moves arrived before the release, so a lost pointermove makes a
    // 400px drag measure as 4px. This one measures endpoints, and the test
    // passes *no moves at all* -- there is nothing to lose.
    const out = classifyDrag(at(0, 0, 0), at(400, 0, 10));
    assert.equal(out.kind, 'pan');
    assert.equal(out.kind === 'pan' ? out.dx : 0, 400);
  });

  test('the slop boundary is exact: at, just under, just over', () => {
    // `travelled > FLICK_SLOP` is a pan, so *at* the slop has not passed it and
    // is still ambiguous enough to be a flick. Asserting `pan` here was my
    // error, not the code's: the slop is the point at which a drag stops being
    // ambiguous, and reaching it is not passing it.
    assert.equal(classifyDrag(at(0, 0, 0), at(FLICK_SLOP, 0, 10)).kind, 'flick');
    assert.equal(classifyDrag(at(0, 0, 0), at(FLICK_SLOP - 1, 0, 10)).kind, 'flick');
    assert.equal(classifyDrag(at(0, 0, 0), at(FLICK_SLOP + 1, 0, 10)).kind, 'pan');
  });

  test('the slop is a distance, not a per-axis check', () => {
    // 8px across and 8px down is 11.3px of travel. Per axis it would read as
    // two 8px movements and be called a flick, which is the diagonal drag.
    assert.equal(classifyDrag(at(0, 0, 0), at(8, 8, 10)).kind, 'pan');
  });

  test('a pan reports its own delta, and dy is negative going up', () => {
    const out = classifyDrag(at(100, 50, 0), at(130, 20, 10));
    assert.equal(out.kind, 'pan');
    if (out.kind !== 'pan') return;
    assert.equal(out.dx, 30);
    assert.equal(out.dy, -30);
  });
});

describe('#7147 the other half: a flick must still work', () => {
  test('a short quick release is a flick, so flick-to-advance still exists', () => {
    // The degenerate fix for #7147 is "never navigate on release", which breaks
    // flick-to-advance entirely. This is the test that stops that fix.
    assert.equal(classifyDrag(at(0, 0, 0), at(3, 0, 50)).kind, 'flick');
  });

  test('a long press that never moved is not a flick', () => {
    // Calling it a flick would make holding an image on a touch screen jump to
    // the next one, which is worse than the bug being fixed.
    assert.equal(classifyDrag(at(0, 0, 0), at(0, 0, FLICK_MS + 100)).kind, 'none');
  });

  test('the duration boundary is exact: one under, at, one over', () => {
    assert.equal(classifyDrag(at(0, 0, 0), at(1, 0, FLICK_MS - 1)).kind, 'flick');
    assert.equal(classifyDrag(at(0, 0, 0), at(1, 0, FLICK_MS)).kind, 'none');
    assert.equal(classifyDrag(at(0, 0, 0), at(1, 0, FLICK_MS + 1)).kind, 'none');
  });
});

describe('#7148 and #7149 the wheel', () => {
  test('a wheel event on a pannable image only pans', () => {
    // The trackpad case: the wheel IS the pan input, so a wheel that navigates
    // makes the image impossible to position.
    assert.equal(classifyWheel(1, true), 'pan');
    assert.equal(classifyWheel(400, true), 'pan');
    assert.equal(classifyWheel(-400, true), 'pan');
  });

  test('the sign of the delta does not decide the direction', () => {
    // On macOS a two-finger scroll down sends a positive deltaY; on Windows a
    // mouse wheel down sends a negative one. "Down means previous" by accident
    // is a bug only one platform ever reports.
    assert.equal(
      classifyWheel(WHEEL_NAVIGATE_DELTA, false),
      classifyWheel(-WHEEL_NAVIGATE_DELTA, false)
    );
  });

  test('a fitted image has nothing to pan, so a big wheel navigates', () => {
    assert.equal(classifyWheel(WHEEL_NAVIGATE_DELTA - 1, false), 'pan');
    assert.equal(classifyWheel(WHEEL_NAVIGATE_DELTA, false), 'navigate');
    assert.equal(classifyWheel(WHEEL_NAVIGATE_DELTA + 1, false), 'navigate');
  });

  test('a wheel event mid-drag is ignored, whatever the delta and the pan range', () => {
    // The ticket's accept criterion, as a unit. The two axes matter separately:
    //
    //   (not pannable, huge delta) would navigate if `dragging` were not checked
    //     first -- a drag on a *fitted* image has nowhere to pan, so this is the
    //     case a `pannable`-first ordering gets wrong, and it is the case the
    //     browser test actually reproduces.
    //   (pannable, huge delta) would pan anyway, so it is already correct
    //     without the check; asserted so the `dragging` branch cannot be
    //     "passed" by deleting the pannable path.
    for (const pannable of [false, true]) {
      for (const deltaY of [1, 40, WHEEL_NAVIGATE_DELTA - 1, WHEEL_NAVIGATE_DELTA, 400, -400]) {
        assert.equal(
          classifyWheel(deltaY, pannable, true),
          'ignore',
          `delta ${deltaY}, pannable ${pannable}: the drag in progress owns the wheel`
        );
      }
    }
  });

  test('the dragging flag is what changes the outcome, not the pan range', () => {
    // Paired with the test above, so dropping the `dragging` argument fails here
    // loudly instead of quietly turning that test into a duplicate of the
    // pannable case.
    assert.equal(classifyWheel(400, false, false), 'navigate');
    assert.equal(classifyWheel(400, false, true), 'ignore');
  });

  test('a trackpad-sized delta on a fitted image does not navigate', () => {
    // The specific complaint: a trackpad emits a stream of single-digit deltas,
    // and a fitted image is the one case where the wheel is not a pan. Treating
    // each as a page turn is what made a trackpad unusable.
    for (const delta of [1, 2, 3, 8, 15, 40]) {
      assert.equal(
        classifyWheel(delta, false),
        'pan',
        `a delta of ${delta} is noise, not a page turn`
      );
    }
  });
});

describe('#7154 back and the unsaved modal', () => {
  test('back closes a modal with nothing unsaved', () => {
    assert.equal(classifyPopstate(false), 'close');
  });

  test('back refuses to close a modal with unsaved edits', () => {
    // The deliberate loop: the entry is pushed back, the modal stays, the edits
    // stay. Losing an edit silently is worse than a back button that visibly
    // does nothing.
    assert.equal(classifyPopstate(true), 're-push');
  });
});

describe('the thresholds are the contract', () => {
  test('they are the documented values', () => {
    // These are the numbers a bug report is really about, so a change to one is
    // a change to behaviour a user has noticed. Asserted so a constant and its
    // documented value cannot drift apart.
    assert.equal(FLICK_SLOP, 10);
    assert.equal(FLICK_MS, 300);
    assert.equal(WHEEL_NAVIGATE_DELTA, 120);
  });
});
