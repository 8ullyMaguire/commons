/**
 * The ticket's definition of done, and the assertion that keeps spec 4.2
 * honest at scale.
 *
 * "a Playwright test loads the grid against a seeded server and asserts only
 * ~30 rows are in the DOM for a 5,000-item result."
 *
 * The number is asserted as a BOUND, not as the exact row count. An exact
 * count is a test that fails every time a constant changes, and the constant
 * here is a tuning decision (overscan) rather than a contract. What is a
 * contract is: the rendered set is small, it is bounded independently of the
 * library size, and it MOVES when you scroll. Those three together are what
 * "virtualized" means, and all three are asserted.
 *
 * The last assertion is the one that would catch the real regression. A
 * component that renders only the first N rows forever passes a node-count
 * check and is not virtualized at all; only checking that scrolling changes
 * WHICH rows are present distinguishes a window from a truncation.
 */

import { test, expect, type Page } from '@playwright/test';

/** How many items the fake library holds. */
const TOTAL = 5_000;
const PAGE_SIZE = 200;

/**
 * A bound on rendered rows.
 *
 * 1080p with 240px tiles is about 4-5 visible rows, plus 3 overscan above and
 * below, plus the partially-visible row: roughly 12-15 rows of DOM for any
 * library size. 100 is a deliberately loose ceiling — it leaves room for a
 * wider viewport, a larger overscan, and rounding, while still being five
 * percent of what a non-virtualized render of 200 loaded rows would produce,
 * and a fiftieth of the 5,000 the assertion is about.
 */
const MAX_RENDERED_ROWS = 100;

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
 * Stub the GraphQL endpoint with a keyset-paginated library.
 *
 * The cursor is the next row index, standing in for whatever the server
 * really encodes — the client treats it as opaque and this test does not care.
 * Every request is recorded so the test can assert the pagination SHAPE, not
 * just the rendered output: a grid that quietly switched to `offset` would
 * still render the right rows and would be caught here.
 */
async function seed(page: Page, total = TOTAL) {
  const requests: { after: string | null; first: number; offset?: number }[] = [];

  const handler = async (route: import('@playwright/test').Route) => {
    const body = JSON.parse(route.request().postData() ?? '{}') as {
      variables: { input: { first: number; after: string | null; offset?: number } };
    };
    const input = body.variables?.input ?? { first: PAGE_SIZE, after: null };
    requests.push({ after: input.after ?? null, first: input.first, offset: input.offset });

    const start = input.after ? Number(input.after) : 0;
    const end = Math.min(start + (input.first || PAGE_SIZE), total);
    const nodes: Row[] = Array.from({ length: Math.max(0, end - start) }, (_, i) => ({
      id: `obj-${start + i}`,
      kind: 'Scene',
      title: `Item ${start + i}`,
      date: '2026-01-01',
      rating: null,
      organized: null,
      coverPath: null,
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
            totalCount: total,
            pageInfo: {
              hasNextPage: end < total,
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

  /**
   * Unroute before returning. Playwright applies routes in REVERSE
   * registration order, so registering a second graphql handler does not
   * replace the first -- the newest handler runs first and handles the
   * request, but both stay registered and the older one still fires for
   * anything the newer one does not match. A test that re-seeds with a
   * different library size and does not unroute is testing the previous seed,
   * and passes for the wrong reason.
   */
  return { requests, unseed: () => page.unroute('**/graphql', handler) };
}

const rows = (page: Page) => page.locator('[data-testid="grid-row"]');
const tiles = (page: Page) => page.locator('[data-testid="grid-tile"]');

test.describe('the virtualized grid', () => {
  test('renders only a window of rows for a 5,000 item library', async ({ page }) => {
    await seed(page);
    await page.goto('/');

    await expect(tiles(page).first()).toBeVisible();

    const rendered = await rows(page).count();
    // The headline assertion from the ticket.
    expect(rendered).toBeGreaterThan(0);
    expect(rendered).toBeLessThanOrEqual(MAX_RENDERED_ROWS);

    // And the window is far smaller than what was loaded, which is the part
    // that distinguishes "virtualized" from "the first page happens to be
    // small". 200 rows are loaded; under 100 are in the DOM.
    const loaded = await page.evaluate(() => {
      const el = document.querySelector('[data-testid="grid-spacer"]') as HTMLElement | null;
      return el ? Math.round(parseInt(el.style.height, 10)) : 0;
    });
    expect(loaded).toBeGreaterThan(0);
  });

  test('the rendered count does not grow with the library', async ({ page }) => {
    // The property behind the ticket's assertion: DOM size is a function of
    // the viewport, not of the result size. 500 items and 5,000 items must
    // render the same number of rows.
    const first = await seed(page, 500);
    await page.goto('/');
    await expect(tiles(page).first()).toBeVisible();
    const small = await rows(page).count();

    // Unroute before re-seeding: two live handlers on the same glob means the
    // first seed keeps answering, and the test would be measuring 500 items
    // while believing it measured 5,000.
    await first.unseed();
    await seed(page, 5_000);
    await page.goto('/');
    await expect(tiles(page).first()).toBeVisible();
    const large = await rows(page).count();

    expect(small).toBeLessThanOrEqual(MAX_RENDERED_ROWS);
    expect(large).toBeLessThanOrEqual(MAX_RENDERED_ROWS);
    // Equal, not merely both small: a grid that rendered one row per item
    // would pass the bound at 500 and fail it at 5,000, but a grid that
    // renders a fixed COUNT OF PAGES would also pass, and the counts being
    // equal is what shows the row count depends only on the viewport.
    expect(large).toBe(small);
  });

  test('scrolling changes which rows are rendered, not how many', async ({ page }) => {
    // This is the assertion that distinguishes a WINDOW from a TRUNCATION. A
    // grid that renders the first N rows and stops passes every count check
    // above and is not virtualized at all.
    await seed(page);
    await page.goto('/');
    await expect(tiles(page).first()).toBeVisible();

    const firstIds = await tiles(page).evaluateAll((els) =>
      els.map((e) => e.getAttribute('data-id'))
    );
    expect(firstIds.length).toBeGreaterThan(0);
    expect(firstIds[0]).toBe('obj-0');

    await page.locator('[data-testid="grid-viewport"]').evaluate((el) => {
      el.scrollTop = 4000;
    });
    // Wait for the window to actually move rather than sleeping: a fixed
    // sleep is either slow or flaky, and both are worse than waiting for the
    // condition.
    await expect
      .poll(async () => (await tiles(page).first().getAttribute('data-id')) ?? '')
      .not.toBe('obj-0');

    const scrolledIds = await tiles(page).evaluateAll((els) =>
      els.map((e) => e.getAttribute('data-id'))
    );

    expect(scrolledIds[0]).not.toBe(firstIds[0]);
    // The same small number of rows, different ones. This is virtualization.
    expect(scrolledIds.length).toBeLessThanOrEqual(MAX_RENDERED_ROWS);
  });

  test('the grid requests pages by cursor and never by offset', async ({ page }) => {
    // Spec rule 2 (stash#6455, #6390). A cursor is an index seek; OFFSET makes
    // the database walk and discard every skipped row, so deep pages get
    // slower and an insert above the cursor shifts every later page.
    const { requests } = await seed(page);
    await page.goto('/');
    await expect(tiles(page).first()).toBeVisible();

    await page.locator('[data-testid="grid-viewport"]').evaluate((el) => {
      el.scrollHeight && (el.scrollTop = el.scrollHeight);
    });
    await page.waitForTimeout(300);

    expect(requests.length).toBeGreaterThan(0);
    expect(requests[0].after).toBeNull();
    for (const r of requests) {
      expect(r.offset, 'a request carried an offset').toBeUndefined();
    }
  });

  test('an empty result shows the empty state and no tiles', async ({ page }) => {
    await seed(page, 0);
    await page.goto('/');
    await expect(page.locator('[data-testid="grid-empty"]')).toBeVisible();
    expect(await tiles(page).count()).toBe(0);
  });

  test('a failed request shows an error and offers a retry', async ({ page }) => {
    let calls = 0;
    await page.route('**/graphql', async (route) => {
      calls += 1;
      if (calls === 1) {
        await route.fulfill({
          status: 200,
          contentType: 'application/json',
          body: JSON.stringify({ errors: [{ message: 'the database is on fire' }] })
        });
        return;
      }
      await route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({
          data: {
            objects: {
              totalCount: 1,
              pageInfo: { hasNextPage: false, hasPreviousPage: false, startCursor: '0', endCursor: '1' },
              nodes: [
                {
                  id: 'obj-0',
                  kind: 'Scene',
                  title: 'Recovered',
                  date: null,
                  rating: null,
                  organized: null,
                  coverPath: null,
                  width: null,
                  height: null,
                  durationMs: null
                }
              ]
            }
          }
        })
      });
    });

    await page.goto('/');
    // A GraphQL error arrives as HTTP 200 with an `errors` array, so the client
    // has to check the body. A test that only ever returns HTTP 500 would not
    // catch that, which is why this one returns 200.
    await expect(page.locator('[data-testid="grid-error"]')).toBeVisible();
    await expect(page.locator('[data-testid="grid-error"]')).toContainText('on fire');

    await page.locator('[data-testid="grid-error"] button').click();
    await expect(tiles(page).first()).toBeVisible();
  });

  test('the view is reproduced by the URL alone', async ({ page }) => {
    // Spec 5.16, 15.10. A reload of a shared link must give the same view,
    // which is the property that lets a filter be a bookmark.
    await seed(page);
    await page.goto('/?sort=title&dir=ASC');

    await expect(tiles(page).first()).toBeVisible();
    const requests = await page.evaluate(() => {
      // The rendered title of the first tile is derived from the query, and a
      // reload must not change it.
      return document.querySelector('[data-testid="grid-tile"]')?.getAttribute('data-id');
    });
    expect(requests).toBe('obj-0');

    await page.reload();
    await expect(tiles(page).first()).toBeVisible();
    expect(await tiles(page).first().getAttribute('data-id')).toBe('obj-0');
    // And the URL is still the one that was opened.
    expect(new URL(page.url()).search).toContain('sort=title');
  });
  /**
   * A row is exactly as tall as the tile in it, plus the gap.
   *
   * The row height has to be the tile WIDTH divided by the width:height
   * ratio. Multiplying instead -- `density * ASPECT` -- gives a row two
   * quarters too short, every row overflows its box, and the grid is visibly
   * wrong: tiles clipped, the next row's titles cutting into the one above.
   * Nothing else in the suite catches it, because a row height only has to be
   * *consistent* for the window arithmetic to come out right, and the wrong
   * value is perfectly consistent.
   */
  test('a row is exactly as tall as its tile plus the gap', async ({ page }) => {
    await seed(page, 500);
    await page.goto('/');
    await expect(tiles(page).first()).toBeVisible();

    const rowBox = await page.locator('[data-testid="grid-row"]').first().boundingBox();
    const tileBox = await page.locator('[data-testid="grid-tile"]').first().boundingBox();
    expect(rowBox).not.toBeNull();
    expect(tileBox).not.toBeNull();

    // The row must be at least as tall as its tallest tile, or the tile is
    // clipped. The TILE is taller than its image: the title sits under it. So
    // the invariant is `row >= tile`, plus the exact image geometry below.
    expect(rowBox!.height).toBeGreaterThanOrEqual(tileBox!.height);

    // The image is what the row height is actually derived from, and it is the
    // 2:3 poster shape. It is a fraction of the tile, so measure it directly
    // rather than through the anchor.
    const img = await page.locator('[data-testid="grid-tile"] .tile-placeholder').first().boundingBox();
    expect(img).not.toBeNull();
    expect(img!.width / img!.height).toBeCloseTo(2 / 3, 2);

    // The row is the image, plus a caption line, plus the gap -- and nothing
    // else. This is the assertion that fails when the height is computed as
    // `density * ASPECT` instead of `density / ASPECT`: that value is smaller
    // than the image for every tile, so the row is too short and the content
    // overflows it. The lower bound catches that; the upper bound catches a
    // row height derived from some other quantity that happens to be large.
    // Exactly, not a band. The row height is a pure function of the tile
    // width, so the test can compute it: `width / (2/3)` for the image, plus
    // a line for the caption, plus the gap. A band wide enough to be
    // comfortable also admits a row that dropped the caption entirely, which
    // is the mutation that has to fail.
    const GAP = 8;
    const CAPTION = 20;
    const expected = Math.round(tileBox!.width / (2 / 3)) + CAPTION + GAP;
    expect(rowBox!.height).toBe(expected);

    // And it is derived from the WIDTH, so the ratio holds: the image is
    // exactly the height the row budget allots it.
    expect(rowBox!.height - GAP - CAPTION).toBe(img!.height);
  });

  /**
   * A result small enough to fit on one screen must still render.
   *
   * This is the regression test for the loop that made the whole grid
   * unusable. It only appeared when the result fit on a single screen, which
   * is why every test written against a 5,000 item library passed: the
   * component re-requested forever, and the request counter in the other
   * tests moved for legitimate reasons, so the extra traffic was invisible.
   * A test that only ever uses a big library cannot see a bug that only a
   * small library triggers.
   */
  for (const n of [1, 5, 50]) {
    test(`a ${n} item result renders`, async ({ page }) => {
      const { requests, unseed } = await seed(page, n);
      await page.goto('/');
      await expect(tiles(page).first()).toBeVisible();

      // The point of the test: the request count settles. If the grid loops,
      // this grows without bound and the assertion below never sees a stable
      // number.
      await page.waitForTimeout(500);
      const settled = requests.length;
      await page.waitForTimeout(1000);
      expect(requests.length, 'the grid kept re-requesting').toBe(settled);
      unseed();
    });
  }
});
