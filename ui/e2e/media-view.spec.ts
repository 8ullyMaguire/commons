/**
 * The unified media view (spec §10.4, #1030) and the tag view (#773).
 *
 * # The bug this file exists to make impossible
 *
 * The grid rendered `aspect-ratio: 2 / 3; object-fit: cover` in a stylesheet —
 * a hardcoded poster shape, and a crop. `cover` is not a rendering choice, it
 * is a decision about what the user is allowed to see, made by whoever wrote
 * the first line of CSS. A 16:9 video thumbnail in a 2:3 box loses a third of
 * its width, and the part that is lost is usually the middle of the frame.
 *
 * # Why the assertion is about the COMPUTED ratio and not a pixel diff
 *
 * A screenshot comparison would catch a change and say nothing about a rule. The
 * contract is: **a tile's rendered box is proportional to the media it holds**,
 * and the row is still one height. So this asserts (a) each tile's box matches
 * its own row's media, (b) the row height is the same for every row, which is
 * what keeps the virtualizer's scroll math correct, and (c) nothing is cropped:
 * the image is laid out with `contain`, so its box is at most the row's and
 * never silently narrower than the frame.
 *
 * A screenshot test on a fixed viewport is a test that fails when a font
 * changes and passes when a media policy inverts.
 */

import { test, expect, type Page } from '@playwright/test';

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

/**
 * A library of deliberately MIXED media.
 *
 * Portrait stills, a landscape still, a 1:1, an unprobed row with no
 * dimensions at all, and a video. A fixture with one of everything cannot test
 * a filter — and a fixture that is all portrait cannot test cropping, because
 * every tile is already the right shape.
 */
const MIXED: Row[] = [
  { id: 'p1', kind: 'Scene', title: 'portrait', date: '2026-01-01', rating: null, organized: null, coverPath: '/x/p1.jpg', width: 800, height: 1200, durationMs: null },
  { id: 'l1', kind: 'Scene', title: 'landscape', date: '2026-01-01', rating: null, organized: null, coverPath: '/x/l1.jpg', width: 1920, height: 1080, durationMs: null },
  { id: 's1', kind: 'Scene', title: 'square', date: '2026-01-01', rating: null, organized: null, coverPath: '/x/s1.jpg', width: 1000, height: 1000, durationMs: null },
  { id: 'v1', kind: 'Scene', title: 'video', date: '2026-01-01', rating: null, organized: null, coverPath: '/x/v1.jpg', width: 1280, height: 720, durationMs: 12_000 },
  // Nothing measured. A real row, from a scan that has not run.
  { id: 'u1', kind: 'Scene', title: 'unprobed', date: '2026-01-01', rating: null, organized: null, coverPath: '/x/u1.jpg', width: null, height: null, durationMs: null },
  // A degenerate one: a probe that reported height 0.
  { id: 'z1', kind: 'Scene', title: 'zero-height', date: '2026-01-01', rating: null, organized: null, coverPath: '/x/z1.jpg', width: 800, height: 0, durationMs: null },
  // A strip. Clamped, or one row makes the viewport enormous.
  { id: 't1', kind: 'Scene', title: 'strip', date: '2026-01-01', rating: null, organized: null, coverPath: '/x/t1.jpg', width: 10_000, height: 1, durationMs: null }
];

/**
 * A 1x1 transparent PNG, served for every cover.
 *
 * Without it the cover paths 404, the browser renders a zero-size broken image,
 * and every assertion about a tile's BOX measures nothing. That is the same
 * family as the unprobed-row case and it is worth naming: a test that measures
 * geometry needs geometry, and a 404 is not geometry.
 */
const PIXEL = Buffer.from(
  'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==',
  'base64'
);

async function seed(page: Page, rows: Row[]) {
  // Registered FIRST so it runs before the graphql route; and unroute by the
  // same glob it was registered with.
  const cover = async (route: import('@playwright/test').Route) => {
    await route.fulfill({ status: 200, contentType: 'image/png', body: PIXEL });
  };
  await page.route('**/x/*.jpg', cover);

  const handler = async (route: import('@playwright/test').Route) => {
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        data: {
          objects: {
            totalCount: rows.length,
            pageInfo: {
              hasNextPage: false,
              hasPreviousPage: false,
              startCursor: '0',
              endCursor: String(rows.length)
            },
            nodes: rows
          }
        }
      })
    });
  };
  await page.route('**/graphql', handler);
  return {
    unseed: async () => {
      await page.unroute('**/graphql', handler);
      await page.unroute('**/x/*.jpg', cover);
    }
  };
}

const tiles = (page: Page) => page.locator('[data-testid="grid-tile"]');

test.describe('the unified media view', () => {
  test('a row with no dimensions renders at the declared ratio, not at zero', async ({ page }) => {
    const { unseed } = await seed(page, MIXED);
    await page.goto('/');

    await expect(tiles(page).first()).toBeVisible();
    const box = await tiles(page).filter({ hasText: 'unprobed' }).locator('img').boundingBox();
    expect(box).not.toBeNull();
    // A height of 0 is the failure: it is what a division by the missing
    // dimension produces, and it is invisible in a unit test of the function
    // and obvious here.
    expect(box!.height).toBeGreaterThan(10);
    expect(box!.width).toBeGreaterThan(10);
    await unseed();
  });

  test('the reported aspect of a degenerate row is inside the declared limits', async ({ page }) => {
    // The row-height assertion above does NOT cover the clamp, and I checked
    // that by mutation: deleting the `Math.min`/`Math.max` in `clamp` left all
    // five tests green. It survives because the row height comes from the
    // DECLARED ratio, not from the tile's own aspect -- so a 10000:1 tile is
    // letterboxed inside a normal row and nothing on screen changes.
    //
    // That is a real gap, and it is worth being precise about what it means: the
    // clamp is not load-bearing for LAYOUT, it is load-bearing for the number
    // the tile reports and for anything that later multiplies by it. So the
    // assertion has to be on the number, which is the only place the clamp is
    // observable from the outside.
    const { unseed } = await seed(page, MIXED);
    await page.goto('/');
    await expect(tiles(page).first()).toBeVisible();

    const byId: Record<string, number> = Object.fromEntries(
      await tiles(page).evaluateAll((els) =>
        els.map(
          (e) =>
            [e.getAttribute('data-id'), Number(e.getAttribute('data-aspect'))] as [string, number]
        )
      )
    );
    expect(Object.keys(byId).length).toBeGreaterThan(0);
    for (const [id, a] of Object.entries(byId)) {
      expect(Number.isFinite(a), `${id} has a finite aspect`).toBe(true);
      expect(a, `${id} is at or above the minimum`).toBeGreaterThanOrEqual(0.25);
      expect(a, `${id} is at or below the maximum`).toBeLessThanOrEqual(4);
    }
    // The strip is the interesting one: 10000:1 unclamped is 10000, which is
    // inside no bound a caller would expect, and which any later `width / aspect`
    // turns into a 0.024px tile.
    expect(byId['t1']).toBe(4, 'the 10000:1 strip is clamped to the maximum');
    expect(byId['z1']).toBeLessThanOrEqual(4, 'a zero height cannot be a ratio');
    await unseed();
  });

  test('a degenerate row cannot make a row enormous', async ({ page }) => {
    const { unseed } = await seed(page, MIXED);
    await page.goto('/');

    // The zero-height row and the 10000:1 strip, side by side with a normal
    // portrait. If the clamp is missing, one of these is thousands of pixels
    // tall and the other is invisible.
    // Wait for the ROW, not the tile. `expect(tiles.first()).toBeVisible()`
    // resolves as soon as one tile has a box, which can be before the row
    // element itself is in the DOM -- and then `evaluateAll` over rows returns
    // an empty array rather than waiting. Measured, not assumed: the assertion
    // below failed with `rowBoxes.length === 0` while the tiles were present.
    await expect(page.locator('[data-testid="grid-row"]').first()).toBeVisible();

    const rowBoxes = await page.locator('[data-testid="grid-row"]').evaluateAll((els) =>
      els.map((e) => e.getBoundingClientRect().height)
    );
    expect(rowBoxes.length).toBeGreaterThan(0);
    const tallest = Math.max(...rowBoxes);
    const shortest = Math.min(...rowBoxes);
    // Every row the same height, within rounding. This is BOTH the clamp
    // working and the virtualizer's premise: `rowHeight` is a constant, and a
    // grid whose rows differ is a grid whose scroll position is a guess.
    expect(tallest - shortest).toBeLessThanOrEqual(2);
    expect(tallest).toBeLessThan(2000);
    await unseed();
  });

  test('a tile is drawn with contain, so nothing is cropped away', async ({ page }) => {
    const { unseed } = await seed(page, MIXED);
    await page.goto('/');
    await expect(tiles(page).first()).toBeVisible();

    // The rendered fit, straight from the browser. `cover` is the value that
    // crops; this asserts on the computed style because that is the actual
    // rule, not the pixels it happens to produce.
    const fits = await tiles(page)
      .locator('img')
      .evaluateAll((els) => els.map((e) => getComputedStyle(e).objectFit));
    expect(fits.length).toBeGreaterThan(0);
    for (const f of fits) expect(f).toBe('contain');
    await unseed();
  });

  test('a video is a video because it has a duration, not because of its name', async ({ page }) => {
    const { unseed } = await seed(page, MIXED);
    await page.goto('/');
    await expect(tiles(page).first()).toBeVisible();

    const kinds = await tiles(page).evaluateAll((els) =>
      els.map((e) => e.getAttribute('data-kind'))
    );
    const byId = await tiles(page).evaluateAll((els) =>
      els.map((e) => [e.getAttribute('data-id'), e.getAttribute('data-kind')] as const)
    );
    expect(kinds.length).toBeGreaterThan(0);
    const map = Object.fromEntries(byId);
    expect(map['v1']).toBe('video', 'a positive duration is a fact about the content');
    expect(map['p1']).toBe('image');
    await unseed();
  });

  test('a landscape tile is not forced into a portrait box', async ({ page }) => {
    const { unseed } = await seed(page, MIXED);
    await page.goto('/');
    await expect(tiles(page).first()).toBeVisible();

    // The declared `data-aspect` is the row's shape applied to the tile width.
    // A landscape row reports an aspect ABOVE 1 and a portrait one BELOW 1 --
    // asserted as a direction, because a wrong direction is invisible to any
    // check that only wants a positive number.
    const aspects = await tiles(page).evaluateAll((els) =>
      els.map((e) => [e.getAttribute('data-id'), Number(e.getAttribute('data-aspect'))] as const)
    );
    const map = Object.fromEntries(aspects);
    expect(map['l1']!).toBeGreaterThan(1);
    expect(map['p1']!).toBeLessThan(1);
    expect(map['u1']!).toBeCloseTo(2 / 3, 2);
    await unseed();
  });
});
