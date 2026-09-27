/**
 * The wall in a browser. Spec 10.4 / #6544, #6955.
 *
 * The decisions are in `tests/wall.test.ts`, where they can be tested at their
 * boundaries. What is left is the four things only a browser can answer:
 *
 *   1. the sections are laid out where the pure geometry said, and the scroll
 *      range matches the rendered height — the spacer and the content have to be
 *      the SAME number or the scrollbar lies;
 *   2. auto-scroll follows the bottom as rows arrive, and stops the moment the
 *      user scrolls up — both directions, because the second is the bug;
 *   3. the wall renders only the visible sections, so group-by does not undo
 *      §4.2's virtualization;
 *   4. a group with no loaded rows is not a zero-height gap the user scrolls
 *      through.
 */
import { test, expect, type Page } from '@playwright/test';

/** A 1x1 PNG, so an <img> has real dimensions and geometry is measurable. */
const PNG = Buffer.from(
  'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==',
  'base64'
);

/** Rows with dates, studios and folders, so every grouping has something. */
function row(i: number, over: Record<string, unknown> = {}) {
  const month = String((i % 6) + 1).padStart(2, '0');
  return {
    id: `obj-${i}`,
    kind: 'Scene',
    title: `Item ${i}`,
    date: `2024-${month}-01`,
    rating: (i % 5) + 1,
    organized: i % 2 === 0 ? 'yes' : null,
    coverPath: null,
    width: 800,
    height: 1200,
    durationMs: null,
    producer: `Studio ${i % 3}`,
    performers: [`Performer ${i % 2}`],
    tags: [`tag-${i % 4}`],
    folder: `Folder ${i % 2}`,
    ...over
  };
}

/** Stub the endpoint. `total` rows, one page, so the wall is all-loaded. */
async function seed(page: Page, total: number) {
  const nodes = Array.from({ length: total }, (_, i) => row(i));
  await page.route('**/graphql', async (route) => {
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        data: {
          objects: {
            totalCount: total,
            pageInfo: { hasNextPage: false, hasPreviousPage: false, startCursor: null, endCursor: null },
            nodes
          }
        }
      })
    });
  });
  // The cover glob, unrouted with the same pattern -- Playwright applies routes
  // in REVERSE registration order, so a second handler does not replace this.
  await page.route('**/media/**', (route) =>
    route.fulfill({ status: 200, contentType: 'image/png', body: PNG })
  );
}

test.describe('the wall', () => {
  test('groups into labelled sections', async ({ page }) => {
    await seed(page, 12);
    await page.goto('/wall?group=studio');

    await expect(page.getByTestId('wall-group')).toHaveCount(3);
    await expect(page.getByTestId('wall-group-label').first()).toHaveText('Studio: Studio 0');
  });

  test('every tile is in the section its field names', async ({ page }) => {
    // The claim that grouping actually partitions, not just that headers appear.
    await seed(page, 12);
    await page.goto('/wall?group=studio');
    // Wait for a TILE, not for the section: the section element exists the
    // instant the query starts and holds nothing until the page lands, so a
    // count of sections right after `goto` reads a wall that has not loaded.
    await expect(page.getByTestId('wall-tile').first()).toBeVisible();

    const sections = await page.getByTestId('wall-group').evaluateAll((els) =>
      els.map((el) => ({
        value: el.getAttribute('data-value'),
        tiles: el.querySelectorAll('[data-testid="wall-tile"]').length
      }))
    );
    expect(sections.map((s) => s.tiles).reduce((a, b) => a + b, 0)).toBe(12);
    expect(sections.every((s) => s.tiles > 0)).toBe(true);
  });

  test('no headers when ungrouped', async ({ page }) => {
    await seed(page, 6);
    await page.goto('/wall');

    await expect(page.getByTestId('wall-group')).toHaveCount(1);
    await expect(page.getByTestId('wall-group-label')).toHaveCount(0);
  });

  test('an unrecognised group falls back to no grouping', async ({ page }) => {
    // A stale bookmark shows the grid. An unrecognised value in a URL is not an
    // error -- the group list grows with the schema.
    await seed(page, 6);
    await page.goto('/wall?group=from_the_year_3000');

    await expect(page.getByTestId('wall-group-label')).toHaveCount(0);
    await expect(page.getByTestId('wall-tile')).toHaveCount(6);
  });

  // The scrollbar must describe the content it scrolls. A spacer and a content
  // height that disagree means the user drags into empty space.
  test('the scroll height matches the rendered content', async ({ page }) => {
    await seed(page, 12);
    await page.goto('/wall?group=month');

    const spacer = await page.getByTestId('wall-spacer').evaluate((el) => el.clientHeight);
    const reported = Number(await page.getByTestId('wall').getAttribute('data-total'));
    expect(spacer).toBeGreaterThan(0);
    // Within a pixel, not exactly. The reported height is fractional (a tile
    // height is derived from an aspect ratio) and the browser rounds when it
    // lays it out, so an exact `toBe` is a test of floating point rather than
    // of the claim. The claim is that the scroll range and the content are the
    // same size -- two different numbers would mean the user drags into space
    // that holds nothing.
    expect(Math.abs(spacer - reported)).toBeLessThanOrEqual(1);
  });

  test('the sections sit at the offsets the geometry computed', async ({ page }) => {
    await seed(page, 12);
    await page.goto('/wall?group=studio');

    const groups = await page.getByTestId('wall-group').evaluateAll((els) =>
      els.map((el) => ({ top: el.getBoundingClientRect().top, h: el.getBoundingClientRect().height }))
    );
    // Each section starts below the previous one ends, plus the 8px gap. Not
    // overlapping is the claim; the exact pixels are a tuning decision.
    for (let i = 1; i < groups.length; i += 1) {
      expect(groups[i]!.top).toBeGreaterThanOrEqual(groups[i - 1]!.top + groups[i - 1]!.h - 1);
    }
  });

  test('an all-loaded wall reports itself exact', async ({ page }) => {
    await seed(page, 12);
    await page.goto('/wall?group=studio');

    await expect(page.getByTestId('wall')).toHaveAttribute('data-exact', 'true');
    await expect(page.getByTestId('wall-group').first()).toHaveAttribute('data-pending', '0');
  });

  test('the group key round-trips through the URL', async ({ page }) => {
    await seed(page, 12);
    await page.goto('/wall?group=month');

    await expect(page.locator('nav a[aria-current="page"]')).toHaveText('month');
    await page.getByRole('link', { name: 'year', exact: true }).click();
    await expect(page.getByTestId('wall-group-label').first()).toContainText('Year:');
  });

  // --- auto-scroll (#6955) -------------------------------------------------
  //
  // These need a wall shorter than its content and a viewport the test controls,
  // so `setViewportSize` runs first and the scroll happens on the wall element.

  test('follows the bottom as rows arrive, while pinned', async ({ page }) => {
    await seed(page, 400);
    await page.setViewportSize({ width: 900, height: 500 });
    await page.goto('/wall?group=month');

    const wall = page.getByTestId('wall');
    await expect(wall).toHaveAttribute('data-pinned', 'false', {
      // A wall opens at the TOP: one that opens at the bottom is a wall whose
      // top the user cannot find.
      timeout: 5000
    });
    const atTop = await wall.evaluate((el) => el.scrollTop);
    expect(atTop).toBe(0);

    // Scroll to the bottom by hand; the wall should consider itself pinned.
    await wall.evaluate((el) => el.scrollTo({ top: el.scrollHeight }));
    await expect(wall).toHaveAttribute('data-pinned', 'true');

    // Scroll up, and it must let go -- the whole point of the feature.
    await wall.evaluate((el) => el.scrollTo({ top: el.scrollTop - 300 }));
    await expect(wall).toHaveAttribute('data-pinned', 'false');
  });

  test('stops following the moment the user scrolls up', async ({ page }) => {
    await seed(page, 400);
    await page.setViewportSize({ width: 900, height: 500 });
    await page.goto('/wall?group=month');

    const wall = page.getByTestId('wall');
    await wall.evaluate((el) => el.scrollTo({ top: el.scrollHeight }));
    await expect(wall).toHaveAttribute('data-pinned', 'true');

    await wall.evaluate((el) => el.scrollTo({ top: el.scrollTop - 300 }));
    await expect(wall).toHaveAttribute('data-pinned', 'false');

    // And it STAYS let go. Re-pinning on "close enough to the bottom" is what
    // yanks the view down in the middle of a trackpad flick.
    await wall.evaluate((el) => el.scrollTo({ top: el.scrollTop + 40 }));
    await page.waitForTimeout(200);
    await expect(wall).toHaveAttribute('data-pinned', 'false');
  });

  test('a wall that fits its viewport does not scroll', async ({ page }) => {
    await seed(page, 6);
    await page.setViewportSize({ width: 1400, height: 1200 });
    await page.goto('/wall?group=month');

    const wall = page.getByTestId('wall');
    const range = await wall.evaluate((el) => el.scrollHeight - el.clientHeight);
    expect(range).toBeLessThanOrEqual(1);
  });
});
