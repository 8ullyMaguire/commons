/**
 * Player position in the URL. T-P5-007 part 2, spec §15.10.
 *
 * `ui/tests/player-position.test.ts` proves the throttle. This proves the one
 * thing that cannot be proved without a browser: that `/play?o=<id>&t=42`
 * actually seeks the media element, and that playing rewrites the URL.
 *
 * The stub needs `accept-ranges: bytes` before any of this can be asserted. A
 * `<video>` pointed at a fulfilled body with no range support will not seek:
 * the assignment is accepted, no `error` fires, `readyState` is 4, the range
 * looks fully buffered -- and `currentTime` never moves. Every assertion
 * downstream then reads as "this player ignores the scrubber", which is a real
 * bug shape and the wrong one.
 */

import { expect, test, type Page, type Route } from '@playwright/test';

/** A real, seekable MP4. Reused from `player.spec.ts`'s fixture. */
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const HERE = dirname(fileURLToPath(import.meta.url));
const CLIP = readFileSync(join(HERE, 'fixtures', 'clip20.mp4'));

const ROW = {
  __typename: 'Object',
  id: 'obj-1',
  kind: 'Scene',
  title: 'A Clip',
  date: '2024-01-01',
  rating: 1,
  organized: null,
  coverPath: null,
  width: 640,
  height: 360
};

const CAPS = { videoCodec: 'h264', audioCodec: 'aac', container: 'mp4', width: 640, height: 360 };

async function harness(page: Page) {
  await page.route('**/graphql', async (route: Route) =>
    route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        data: {
          objects: {
            totalCount: 1,
            pageInfo: {
              hasNextPage: false,
              hasPreviousPage: false,
              startCursor: '0',
              endCursor: '1'
            },
            nodes: [ROW]
          }
        }
      })
    })
  );
  await page.route('**/media/obj-1/caps', async (route: Route) =>
    route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(CAPS) })
  );
  await page.route('**/media/obj-1/playback', async (route: Route) => {
    if (route.request().method() === 'PUT') {
      return route.fulfill({ status: 204, contentType: 'application/json', body: '' });
    }
    return route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({ position_ms: 0, duration_ms: 10_000, completed: false })
    });
  });
  // NO `accept-ranges` header, deliberately. Adding it is the documented way
  // to make a `<video>` seekable, and it is also what made this file fail:
  // with the header Chromium treats the fulfilled body as a real ranged
  // response and reports DEMUXER_ERROR_COULD_NOT_PARSE, so nothing plays and
  // `timeupdate` never fires. Without it the fixture decodes, playback runs,
  // and the seek assertions pass too -- so the header was never what made
  // seeking work here, it only made playback impossible.
  const bytes = async (route: Route) =>
    route.fulfill({ status: 200, contentType: 'video/mp4', body: CLIP });
  // BOTH the direct url and the proxy's. Registering only the proxy looks
  // complete -- the string `proxy.m3u8` is right there in the DOM -- and leaves
  // the direct url unhandled, so the request goes to the real dev server,
  // answers 404, and the element reports DEMUXER_ERROR. `networkState` of 3
  // (NETWORK_NO_SOURCE) with a 90 KB fixture that never loads is the tell.
  // A REGEX, not a glob. The player appends a signature query -- `?h=undefined`
  // when it has no token -- and Playwright's `**/media/obj-1` glob does not
  // match a URL with a query string on it, so the route silently never fires,
  // the request escapes to the dev server, and the element sits at
  // networkState 3 (NETWORK_NO_SOURCE) with DEMUXER_ERROR. The route is
  // registered, the URL is visible in the DOM, and nothing is stubbed: the
  // failure looks like a broken player, not a broken test.
  await page.route(/media\/obj-1(\/proxy\.m3u8)?(\?.*)?$/, bytes);
  await page.route('**/media/obj-1/thumb', (route: Route) => route.fulfill({ status: 404, body: '' }));
}

const video = (page: Page) => page.locator('video');

/**
 * Give the scrubber a usable range, without a decoder.
 *
 * With no H.264, `video.duration` is `null`, so the scrubber renders `max=0`
 * and every `fill` is a "Malformed value". Setting `max` on the CONTROL
 * directly is what works: defining `duration` on the media element does not,
 * because the component binds the range from its own `$state` updated on
 * `loadedmetadata`, and that event never fires here. The assertions are only
 * about the URL, so the range is a UI fact and not a claim about decoding.
 */
async function withScrubRange(page: Page, ms: number): Promise<void> {
  await page.locator('[data-testid="player-scrub"]').evaluate((el, ms) => {
    (el as HTMLInputElement).max = String(ms);
  }, ms);
}
const seconds = (page: Page) =>
  video(page).evaluate((v: HTMLVideoElement) => v.currentTime);

test.describe('the position in the URL', () => {
  test('t= seeks the media element', async ({ page }) => {
    await harness(page);
    await page.goto('/play?o=obj-1&t=4');
    await expect(video(page)).toBeVisible();

    // The assertion is on the element's own clock, not on a state flag: a
    // player can hold a resume target, report it, and still not move the
    // media, and only `currentTime` distinguishes those.
    await expect.poll(() => seconds(page), { timeout: 10_000 }).toBeGreaterThanOrEqual(3.5);
  });

  test('a link with no t starts at the beginning', async ({ page }) => {
    await harness(page);
    await page.goto('/play?o=obj-1');
    await expect(video(page)).toBeVisible();
    await expect.poll(() => seconds(page), { timeout: 10_000 }).toBeLessThan(1);
  });

  test('a t in the URL outranks a saved server position', async ({ page }) => {
    await harness(page);
    // The server says "you were at 9 seconds". The link says 4. The link
    // wins, because a link is a statement about where someone should start and
    // a stale server-side resume that silently overrides it is the link lying.
    await page.route('**/media/obj-1/playback', async (route: Route) => {
      if (route.request().method() === 'PUT') {
        return route.fulfill({ status: 204, contentType: 'application/json', body: '' });
      }
      return route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({ position_ms: 9000, duration_ms: 10_000, completed: false })
      });
    });
    await page.goto('/play?o=obj-1&t=2');
    await expect(video(page)).toBeVisible();
    await expect.poll(() => seconds(page), { timeout: 10_000 }).toBeLessThan(5);
  });

  test('a reload resumes where the link said', async ({ page }) => {
    await harness(page);
    await page.goto('/play?o=obj-1&t=3');
    await expect(video(page)).toBeVisible();
    await expect.poll(() => seconds(page), { timeout: 10_000 }).toBeGreaterThanOrEqual(2.5);

    await page.reload();
    await expect(video(page)).toBeVisible();
    await expect.poll(() => seconds(page), { timeout: 10_000 }).toBeGreaterThanOrEqual(2.5);
  });

  test('the URL is written when the position moves, in readable form', async ({
    page
  }) => {
    await harness(page);
    await page.goto('/play?o=obj-1');
    await expect(video(page)).toBeVisible();
    const url0 = page.url();

    // This Chromium has no H.264, so `play()` never decodes and playback's
    // `timeupdate` never fires -- the constraint `player.spec.ts` documents.
    // Rather than assert around a player that cannot move, the write path is
    // driven the way this browser can drive it: the SCRUBBER, which is the
    // path a user takes and which the media element honours without a decoder.
    //
    // `video.currentTime = 1` would be the shorter line and is a silent no-op
    // on an element that has not buffered the range -- `currentTime` reads back
    // 0, no error fires, and the test fails looking like a broken player.
    await withScrubRange(page, 5000);
    await page.getByTestId('player-scrub').fill('2500');
    await expect(page.getByTestId('player-scrub')).toHaveValue('2500');

    // The write is throttled to 2 s and the scrubber may not have moved the
    // clock yet, so the assertion is on the URL gaining a `t` at all, and the
    // readable form is asserted as soon as it is there.
    await expect
      .poll(() => new URL(page.url()).searchParams.get('t'), { timeout: 8_000 })
      .not.toBeNull();
    expect(page.url(), 'the object id must survive the rewrite').toContain('o=obj-1');

    // Readable, not percent-encoded. `4%3A12` is a link nobody can read the
    // position off, and readability is the entire reason the position is in
    // the URL rather than in a cookie.
    const raw = new URL(page.url()).search;
    expect(raw, `t should be mm:ss, not escaped: ${raw}`).not.toContain('%3A');
    expect(new URL(page.url()).searchParams.get('t')).toMatch(/^\d+(:\d{2})?$/);
    expect(url0, 'sanity: the url started without a t').not.toContain('t=');
  });

  test('playing does not push a history entry per tick', async ({ page }) => {
    await harness(page);
    await page.goto('/play?o=obj-1');
    await expect(video(page)).toBeVisible();
    const before = await page.evaluate(() => history.length);

    // Same constraint as above: no decoder, so the position is moved with the
    // scrubber, which the media element honours anyway. `replaceState` must
    // keep `history.length` where it was -- a `pushState` per write is what
    // makes the back button walk through one video in quarter-second steps.
    await withScrubRange(page, 5000);
    await page.getByTestId('player-scrub').fill('2500');
    await expect(page.getByTestId('player-scrub')).toHaveValue('2500');
    await page.waitForTimeout(2_500);
    for (const ms of ['3000', '3500', '4000']) {
      await page.getByTestId('player-scrub').fill(ms);
      await page.waitForTimeout(2_200);
    }

    const after = await page.evaluate(() => history.length);
    expect(after, 'the back button must still work').toBe(before);
  });

  test('t=0 is not written, so a fresh video has no stale position', async ({ page }) => {
    await harness(page);
    await page.goto('/play?o=obj-1');
    await expect(video(page)).toBeVisible();
    // Unplayed, the URL must stay clean. A `t=0` makes "resume where I left
    // off" indistinguishable from "someone shared a link to the first
    // second", and those are different things.
    await page.waitForTimeout(1200);
    expect(new URL(page.url()).searchParams.get('t')).toBeNull();
  });

  test('a nonsense t plays from the start rather than erroring', async ({ page }) => {
    await harness(page);
    await page.goto('/play?o=obj-1&t=banana');
    await expect(video(page)).toBeVisible();
    // A link someone edited is not an error worth showing them. The video plays
    // from the start, which is what a link with no `t` does.
    await expect.poll(() => seconds(page), { timeout: 10_000 }).toBeLessThan(1);
  });
});
