/**
 * The feed, in a browser. §10.6 / #3859.
 *
 * The pure decisions — the commit threshold, the clamping, the prefetch
 * distance — are in `tests/feed.test.ts`, where they can be tested at the
 * boundary. What is left here is the four things only a browser can answer:
 *
 *   1. exactly one item plays, and it is the focused one;
 *   2. a drag past the threshold advances and a short one does not;
 *   3. the focused video is muted and playsinline — a claim about the DOM that
 *      decides whether this works on the phone the ticket names;
 *   4. a still in the feed is an image, not a video that failed to load.
 *
 * The route is the real one, driven through a mocked GraphQL endpoint, because a
 * component mounted into a blank page is a component with none of the
 * surrounding behaviour: no keyset paging, no URL focus, no store reset.
 */
import { expect, test, type Page, type Route } from '@playwright/test';

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
 * Three clips and one still.
 *
 * Mixed on purpose: a fixture of only clips cannot tell a still from a clip,
 * and `media-view.test.ts` makes the same argument the same argument for the same reason. The
 * still is LAST so the feed has to be focused onto it to reach it.
 */
const ROWS: Row[] = [
  { id: 'f1', kind: 'Scene', title: 'one', date: '2026-01-01', rating: null, organized: null, coverPath: '/x/f1.jpg', width: 1080, height: 1920, durationMs: 4000 },
  { id: 'f2', kind: 'Scene', title: 'two', date: '2026-01-02', rating: null, organized: null, coverPath: '/x/f2.jpg', width: 1080, height: 1920, durationMs: 5000 },
  { id: 'f3', kind: 'Scene', title: 'three', date: '2026-01-03', rating: null, organized: null, coverPath: '/x/f3.jpg', width: 1080, height: 1920, durationMs: 6000 },
  { id: 'f4', kind: 'Clip', title: 'four, a still', date: '2026-01-04', rating: null, organized: null, coverPath: '/x/f4.jpg', width: 800, height: 1200, durationMs: null },
];

/**
 * A real 1x1 PNG. `autoplay` needs decodable bytes, and a 404 renders as a
 * zero-size element that every geometry assertion would then measure.
 */
const PIXEL = Buffer.from(
  'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==',
  'base64',
);

async function openFeed(page: Page, focusIndex = 0, rows: Row[] = ROWS) {
  // The cover glob FIRST so it is matched before the graphql route, and
  // unrouted by the same glob it registered with: Playwright applies routes in
  // REVERSE registration order, so a second handler does not replace the first.
  const cover = async (route: Route) =>
    route.fulfill({ status: 200, contentType: 'image/png', body: PIXEL });
  await page.route('**/x/*.jpg', cover);

  const gql = async (route: Route) =>
    route.fulfill({
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
              endCursor: String(rows.length),
            },
            nodes: rows,
          },
        },
      }),
    });
  await page.route('**/graphql', gql);

  await page.goto(`/vertical?focus=${focusIndex}`);

  const feed = page.getByTestId('vertical-feed');
  await expect(feed).toBeVisible();
  // Wait for the HEIGHT, not merely for an item. `clientHeight` is 0 until
  // layout runs, and a commit decided against a 0px viewport always springs
  // back — so a test that drags too early sees a feed that refuses to move and
  // concludes the gesture is broken. Same class of bug as waiting for a tile
  // instead of the row that holds it.
  await expect
    .poll(async () => (await feed.boundingBox())?.height ?? 0)
    .toBeGreaterThan(0);

  return {
    feed,
    async unseed() {
      await page.unroute('**/graphql', gql);
      await page.unroute('**/x/*.jpg', cover);
    },
  };
}

const focused = (page: Page) => page.locator('[data-testid="feed-slide"][data-focus="true"]');

test('one item plays, and it is the focused one', async ({ page }) => {
  const { unseed } = await openFeed(page);

  await expect(page.locator('[data-testid="feed-slide"][data-playing="true"]')).toHaveCount(1);
  await expect(focused(page)).toHaveCount(1);

  // The count alone is not the claim: a feed that plays whichever slide happens
  // to be last also has exactly one. This is the conjunction.
  expect(
    await focused(page).evaluate((el) => el.getAttribute('data-playing') === 'true'),
    'the focused slide is the playing one',
  ).toBe(true);

  await unseed();
});

test('a drag past the threshold advances, a short one does not', async ({ page }) => {
  const { feed, unseed } = await openFeed(page);
  const box = (await feed.boundingBox())!;
  const cx = box.x + box.width / 2;
  const y = (f: number) => box.y + box.height * f;

  // A SHORT drag: 10% of the height, under COMMIT_THRESHOLD. Playwright's mouse
  // has no drag primitive, so this is down / moves / up -- and the component
  // deliberately does not accumulate deltas, so the NUMBER of moves must not
  // change the outcome. Three moves is what makes that claim testable: an
  // implementation that summed per-move deltas would measure something else.
  await page.mouse.move(cx, y(0.6));
  await page.mouse.down();
  for (const f of [0.55, 0.52, 0.5]) await page.mouse.move(cx, y(f));
  await page.mouse.up();
  await expect(focused(page)).toHaveAttribute('data-index', '0');

  // A LONG drag, upward: the finger moves up and the NEXT item comes from
  // below. That inversion is `axis: 'up'` in the unit test, and here it has to
  // move the focus for real.
  await page.mouse.move(cx, y(0.7));
  await page.mouse.down();
  for (const f of [0.6, 0.4, 0.1]) await page.mouse.move(cx, y(f));
  await page.mouse.up();
  await expect(focused(page)).toHaveAttribute('data-index', '1');

  await unseed();
});

test('the focused video is muted and playsinline', async ({ page }) => {
  const { unseed } = await openFeed(page);
  const v = page.getByTestId('feed-video').first();

  // `muted` is not a style choice. Unmuted autoplay is REFUSED by every current
  // browser, so a feed that tries it silently does nothing on the platform the
  // ticket names. `playsinline` is what stops iOS Safari taking the video
  // fullscreen and navigating away, losing the position -- which is what makes
  // §10.8's mobile-web requirement (#771, #6335) satisfiable at all.
  //
  // The IDL PROPERTY, not the attribute, and the distinction is the whole test.
  // Svelte compiles a bare `muted` on an element to a *property* assignment,
  // not to an attribute write, so the markup has no `muted` in it at all --
  // and asserting the attribute is asserting something this framework does not
  // emit. The property is also the only one that matters: it is what the
  // autoplay gate reads, so a feed that renders `muted` as an attribute and
  // never sets the property would pass an attribute assertion and play nothing.
  expect(await v.evaluate((el) => (el as HTMLVideoElement).muted)).toBe(true);
  expect(await v.evaluate((el) => (el as HTMLVideoElement).playsInline)).toBe(true);

  await unseed();
});

test('a still in the feed is an image, not a video that failed', async ({ page }) => {
  const { unseed } = await openFeed(page, 3); // ROWS[3] is the only still

  await expect(page.getByTestId('feed-still')).toHaveCount(1);
  // Three clips, one still: the still was not rendered as a <video> with a
  // broken source, which is the failure a "render everything as video" feed has
  // and which looks, in a screenshot, like nothing at all.
  await expect(page.getByTestId('feed-video')).toHaveCount(3);

  await unseed();
});

test('the feed renders a window, not every row it is given', async ({ page }) => {
  // The claim that makes "a feed is a window, not a list" more than a comment.
  // 60 rows through a `WINDOW` of 12: a component that iterates its prop
  // renders 60 slides and this is one number.
  const MANY: Row[] = Array.from({ length: 60 }, (_, i) => ({
    id: `m${i}`,
    kind: 'Clip',
    title: `item ${i}`,
    date: null,
    rating: null,
    organized: null,
    coverPath: '/x/m.jpg',
    width: 100,
    height: 100,
    durationMs: null,
  }));
  const { unseed } = await openFeed(page, 0, MANY);

  const rendered = await page.getByTestId('feed-slide').count();
  // The exact bound, not `< 60`: a `< 60` is satisfied by any window at all, so
  // doubling WINDOW would pass it.
  expect(rendered, 'a window of 12, not all 60').toBe(12);

  await unseed();
});

test('only the focused item and the next two preload', async ({ page }) => {
  // Found by mutation: with `preload="auto"` hardcoded on every video, every
  // other test stayed green -- and so did the first version of THIS test, for a
  // reason worth recording.
  //
  // `ROWS` is three clips and a still, and the focus is 0, so every video in
  // the feed sits within PREFETCH_AHEAD of the focus. `preloadAttr` returns
  // 'auto' for all of them and the mutant returns 'auto' for all of them: the
  // same list. A fixture whose every element is inside the filter cannot test
  // the filter, which is the same trap as `media-view.test.ts`'s "a fixture
  // with one of everything cannot test a filter".
  //
  // So this needs SIX clips at focus 0: 0, 1 and 2 preload; 3, 4 and 5 do not,
  // and 3 is the boundary. The still from `ROWS` is not reused because it is
  // not a video and would make the count arithmetic confusing.
  const CLIPS: Row[] = Array.from({ length: 6 }, (_, i) => ({
    id: `c${i}`,
    kind: 'Clip',
    title: `clip ${i}`,
    date: null,
    rating: null,
    organized: null,
    coverPath: '/x/c.jpg',
    width: 100,
    height: 100,
    durationMs: 1000,
  }));
  const { unseed } = await openFeed(page, 0, CLIPS);

  const preloads = await page
    .locator('[data-testid="feed-video"]')
    .evaluateAll((els) =>
      els.map((el) => ({
        index: (el.closest('[data-testid="feed-slide"]') as HTMLElement).dataset.index,
        preload: (el as HTMLVideoElement).preload
      }))
    );

  // Focus is 0 and PREFETCH_AHEAD is 2, so exactly 0, 1 and 2 are 'auto'.
  // The list is the assertion: a `< 3` count would be satisfied by a feed that
  // preloads nothing at all, and a '5 is auto' check would be satisfied by one
  // that preloads everything.
  expect(
    preloads.filter((p) => p.preload === 'auto').map((p) => Number(p.index)).sort((a, b) => a - b),
    'exactly the focused item and the two ahead',
  ).toEqual([0, 1, 2]);

  await unseed();
});
