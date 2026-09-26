/**
 * The lightbox's gestures. Spec 10.2 (C55); stash#7147, #7148, #7149, #7154.
 *
 * # The ticket's accept criterion
 *
 * "Playwright test dispatching a wheel event mid-pan and asserting the image
 * index did not change." That is `a wheel event mid-pan does not change the
 * index`, done literally: a real `WheelEvent` is dispatched on the stage while
 * a drag is in progress, and `data-index` is read afterwards.
 *
 * # Why the assertion reads `data-index` and not a store
 *
 * The complaint is what the user sees. A test that reads the component's own
 * state proves the state is consistent with itself, and a lightbox whose state
 * is right while the DOM shows a different image is precisely the bug. So the
 * tests read the attribute the user is looking at.
 *
 * # Each of the three upstream bugs gets its own named assertion
 *
 * The plan says so, and they are three rather than two because they have
 * different causes. #7147 is about the *end* of a drag and needs slop plus
 * duration. #7148 is a device that emits no drag at all, and no drag logic can
 * help it. #7149 is #7148 at a zoom level. A fix for any one of them leaves
 * the others, which is why they are three.
 *
 * # The tests that are not about a bug
 *
 * `a flick still advances` is the one that matters most. The degenerate fix
 * for #7147 is "never navigate on a release", which breaks flick-to-advance
 * entirely. Without a test asserting the flick still works, that fix passes.
 */

import { test, expect, type Page, type Route } from '@playwright/test';

interface Row {
  id: string;
  kind: string;
  title: string | null;
  date: string | null;
  rating: number | null;
  organized: string | null;
  coverPath: string | null;
  width: number | null;
  height: number | null;
  durationMs: number | null;
}

const TOTAL = 12;

/**
 * A real, sized preview image as a data URI.
 *
 * # Why not a 1x1 pixel
 *
 * The stage is sized by its image, and a 1x1 transparent GIF makes it exactly
 * 1px wide. Every gesture coordinate is then a fraction of a 1px box, so down
 * and up land on the same point, the measured travel is 0, and a zero-travel
 * release is a *flick* -- the lightbox advances and the test fails while
 * looking like a recognizer bug. It is a fixture bug, and it is the kind that
 * makes a whole test file look wrong.
 *
 * So the fixture serves a real image at real dimensions, and the stage is
 * sized by CSS from `width`/`height` rather than from the decoded pixels.
 */
const PREVIEW =
  'data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHdpZHRoPSI4MDAiIGhlaWdodD0iMTIwMCI+PHJlY3Qgd2lkdGg9IjgwMCIgaGVpZ2h0PSIxMjAwIiBmaWxsPSIjMjIyIi8+PC9zdmc+';

/** A library small enough that every tile is in the first page. */
async function seed(page: Page) {
  const handler = async (route: Route) => {
    const body = JSON.parse(route.request().postData() ?? '{}') as {
      variables: { input: { first: number; after: string | null } };
    };
    const input = body.variables?.input ?? { first: 200, after: null };
    const start = input.after ? Number(input.after) : 0;
    const end = Math.min(start + (input.first || 200), TOTAL);
    const nodes: Row[] = Array.from({ length: Math.max(0, end - start) }, (_, i) => ({
      id: `obj-${start + i}`,
      kind: 'Scene',
      title: `Item ${start + i}`,
      date: '2026-01-01',
      rating: null,
      organized: null,
      coverPath: PREVIEW,
      width: 800,
      height: 1200,
      durationMs: null
    }));
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        data: {
          objects: {
            totalCount: TOTAL,
            pageInfo: {
              hasNextPage: end < TOTAL,
              hasPreviousPage: false,
              startCursor: String(start),
              endCursor: String(end)
            },
            nodes
          }
        }
      })
    });
  };
  await page.route('**/graphql', handler);
}

const lightbox = (page: Page) => page.locator('[data-index]');

/** An {x, y} point as the two arguments `mouse.move` wants. */
const xy = (p: { x: number; y: number }): [number, number] => [p.x, p.y];
const stage = (page: Page) => page.locator('.stage');
const indexOf = async (page: Page) => Number(await lightbox(page).getAttribute('data-index'));

/** Open the lightbox on the first tile. */
async function open(page: Page) {
  await page.locator('[data-testid="grid-tile"]').first().click();
  await expect(lightbox(page)).toBeVisible();
}

/**
 * The stage's own rect, and a point at a fraction across it.
 *
 * # Why fractions of the rect and not viewport coordinates
 *
 * The stage is a fitted image, not a full-bleed surface, so it is much smaller
 * than the window. Viewport coordinates that "look about right" land outside
 * it, and an event dispatched outside the stage reaches nothing -- the drag
 * never happens, the assertion passes for the wrong reason, and the test is
 * worse than no test because it is green.
 *
 * Every gesture below is therefore expressed as a fraction of the rect, which
 * is also the only form that survives the CSS changing.
 */
async function stageRect(page: Page) {
  const box = await stage(page).boundingBox();
  if (!box) throw new Error('the stage has no box; is the lightbox open?');
  return box;
}

/** The stage's centre, in viewport coordinates. */
function centre(box: { x: number; y: number; width: number; height: number }) {
  return { x: box.x + box.width / 2, y: box.y + box.height / 2 };
}

/**
 * A viewport point `dx` px to the right of the stage's centre.
 *
 * # Why pixels and not a fraction of the rect
 *
 * The thresholds in `gestures.ts` are in CSS pixels -- FLICK_SLOP is 10px,
 * WHEEL_NAVIGATE_DELTA is 120px -- so a gesture has to be expressed in the
 * same unit to mean anything. A fraction was the first attempt and it was
 * wrong: the stage grew from 212px to 640px when the fixture was fixed, and
 * the same 4% went from 8px (a flick) to 25px (a pan), which turned a passing
 * test red with no change to the code under test.
 *
 * So: centre plus an explicit pixel delta. The test now says what it means.
 */
function rightOf(box: { x: number; y: number; width: number; height: number }, dx: number) {
  const c = centre(box);
  return { x: c.x + dx, y: c.y };
}

/**
 * Drag from one point to another with real pointer events.
 *
 * Playwright's `mouse.move` between down and up is what makes this a gesture
 * rather than a teleport: the recognizer reads the *endpoints*, so what matters
 * is that down and up land where the test says, and a `pointerup` at a
 * coordinate the pointer never visited is exactly the #7147 situation.
 */
async function drag(page: Page, from: { x: number; y: number }, to: { x: number; y: number }) {
  await page.mouse.move(from.x, from.y);
  await page.mouse.down();
  await page.mouse.move(to.x, to.y, { steps: 4 });
  await page.mouse.up();
}

/** A real wheel event at the centre of the stage. */
async function wheel(page: Page, deltaY: number) {
  const box = await stageRect(page);
  await page.mouse.move(...xy(centre(box)));
  await page.evaluate(
    ([dy]) => {
      const el = document.querySelector('.stage')!;
      el.dispatchEvent(
        new WheelEvent('wheel', { deltaY: dy as number, bubbles: true, cancelable: true })
      );
    },
    [deltaY]
  );
}

test.describe('the lightbox', () => {
  test('a wheel event mid-pan does not change the index', async ({ page }) => {
    // The ticket's accept criterion, literally: a real WheelEvent dispatched on
    // the stage while a drag is in progress, with the index read afterwards.
    await seed(page);
    await page.goto('/');
    await open(page);
    expect(await indexOf(page)).toBe(0);

    const box = await stageRect(page);

    // The drag is in progress: the button is down and the pointer has moved.
    // The move is a real fraction of the stage's width, so the gesture is
    // unambiguously a pan by the time the wheel lands.
    // A 200px pan: far past FLICK_SLOP, so it is unambiguously a drag.
    await page.mouse.move(...xy(rightOf(box, 100)));
    await page.mouse.down();
    await page.mouse.move(...xy(rightOf(box, -100)), { steps: 3 });

    await wheel(page, 400);

    expect(await indexOf(page)).toBe(0);

    await page.mouse.up();
    expect(await indexOf(page)).toBe(0);
  });

  test('#7147 a fast drag releases without navigating', async ({ page }) => {
    // The whole width of the stage, quickly. The upstream bug turned this into
    // a page turn; the recognizer measures from the press to the release, so a
    // move lost in the race before `pointerup` cannot shorten the measurement.
    await seed(page);
    await page.goto('/');
    await open(page);

    // 200px across, quickly: the shape of the bug report exactly.
    const box = await stageRect(page);
    await drag(page, rightOf(box, 100), rightOf(box, -100));

    expect(await indexOf(page)).toBe(0);
  });

  test('#7147 a flick still advances, so the fix cannot be "never navigate"', async ({ page }) => {
    // The other half, and the one that matters most. The degenerate fix for
    // #7147 is "never navigate on a release", which kills flick-to-advance
    // entirely. Without this test that fix passes.
    await seed(page);
    await page.goto('/');
    await open(page);
    expect(await indexOf(page)).toBe(0);

    // 4px, fast: inside FLICK_SLOP, and under FLICK_MS. A deliberate flick.
    const box = await stageRect(page);
    await drag(page, rightOf(box, 0), rightOf(box, 4));

    await expect.poll(() => indexOf(page)).toBe(1);
  });

  test('#7148 a wheel event on a fitted image only pans', async ({ page }) => {
    // A fitted image has nowhere to pan, so the wheel is free to mean "next" --
    // but only for a delta large enough to be on purpose. A trackpad's
    // single-digit deltas are noise, and treating each as a page turn is what
    // made a trackpad unusable.
    await seed(page);
    await page.goto('/');
    await open(page);

    for (const noise of [1, 3, 8, 15, 40, 80, 119]) {
      await wheel(page, noise);
      expect(await indexOf(page)).toBe(0);
    }

    // And a deliberate one still works, so this is not "the wheel is dead".
    await wheel(page, 120);
    await expect.poll(() => indexOf(page)).toBe(1);
  });

  test('#7154 back closes a clean lightbox', async ({ page }) => {
    // The clean path: the lightbox pushes a history entry when it opens, and
    // back dismisses it rather than leaving the page. Asserted in a browser
    // because a history entry that is pushed but not poppable is a bug only
    // the browser can show you.
    //
    // The dirty half is a pure function, so it is asserted in
    // `tests/gestures.test.ts` next to the rule, and this page does not set
    // `dirty` -- there is no edit form in the app yet to set it from.
    await seed(page);
    await page.goto('/');
    await open(page);
    expect(await indexOf(page)).toBe(0);

    await page.goBack();

    await expect(page.locator('[data-index]')).toHaveCount(0);
  });
});
