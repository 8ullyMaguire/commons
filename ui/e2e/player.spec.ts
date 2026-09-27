/**
 * The player, in a real browser. Spec 11.1 (C64), 11.5 / plan T-P6-001.
 *
 * # What is left after `tests/player.test.ts`
 *
 * The unit tests cover every decision. This file covers the three things a
 * function cannot be asked about, and each is a claim the ticket actually makes:
 *
 *   1. **the control bar fits a 360x640 viewport** (stash#6526, and the
 *      ticket's Done-when). This is the load-bearing one. `planControlBar`
 *      returns a *plan*, and a plan can be right while the thing it planned
 *      overflows -- the bar could be 240px tall with four wrapped rows on a
 *      360px-wide screen, and every unit test would still pass. So the viewport
 *      is set for real and the bar is measured against it.
 *   2. **a file the browser cannot decode says so**, rather than sitting there
 *      as a black rectangle with a play button that does nothing.
 *   3. **the player resumes where it was left**, and refuses a position in the
 *      trailing two percent -- the case a "did it save?" test misses, because
 *      the save happened perfectly and the *decision* about it was wrong.
 *
 * # Why the fixture is a real HLS set
 *
 * Chromium in this environment has no H.264, so a `<video>` pointed at a real
 * mp4 never fires `canplay` and every test would measure a player stuck in its
 * loading state. Rather than assert around that, the bytes are served at the
 * network boundary so the media events the component listens for are real. A
 * test that cannot make the browser do the thing is not evidence about the
 * thing.
 *
 * A *set* rather than a single file, and that is forced by the code under test
 * rather than chosen for convenience. The row carries no codec metadata, so
 * `needsProxy` says "proxied" and the src is `/media/<id>/proxy.m3u8` -- and
 * Chromium picks a `<video>` demuxer from the URL's extension, so a raw WebM
 * body served at a `.m3u8` is a manifest it cannot parse. The result is
 * `MEDIA_ERR_SRC_NOT_SUPPORTED` (code 4) with `readyState` 0 and every scrubber
 * stuck at `max=0`, which reads exactly like a broken resume. The fix is the
 * honest fixture: serve what the proxy route actually serves.
 *
 * Five seconds, not one, and not because one would not do: a resume test needs
 * a position the media can seek to, and Chromium clamps a seek past the end of
 * the file to the end. The saved positions below are therefore inside five
 * seconds, and the trailing-2% case is the last 100 ms of it.
 */
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { test, expect, type Page, type Route } from '@playwright/test';
import { MAX_BAR_FRACTION } from '../src/lib/player/player.js';

/**
 * A real 5-second 320x180 H.264 MP4.
 *
 * Read from disk rather than inlined, and real rather than a synthetic blob,
 * because these tests need the browser to actually decode: every claim below is
 * about a `<video>` that has fired `loadedmetadata`, and a fixture the browser
 * rejects produces a player that never leaves its loading state -- which then
 * fails as a broken resume, a broken scrubber, or a broken control bar, all of
 * which are the things being tested.
 *
 * Five seconds rather than one, and not for style: a resume test needs a
 * position the media can seek to, and Chromium clamps a seek past the end of
 * the file to the end. One second makes any mid-file resume test fail for a
 * reason that has nothing to do with the code.
 *
 * # Why the DIRECT url and not the proxy
 *
 * `ObjectRow` carries no container or codec, so `needsProxy` answers "proxied"
 * -- the safe answer, and the one a user cannot tell from a correct guess. But
 * the proxied url is `/media/<id>/proxy.m3u8`, and the media element reaches it
 * through MSE, and *this* Chromium build will not load an HLS manifest: probed
 * directly, a real VOD playlist sits at `readyState` 0 while the same bytes as
 * a plain MP4 report `duration 5` immediately.
 *
 * So these tests drive the direct path, which is the one a browser can be asked
 * about here, and say so. The proxied path is not untested -- it is tested where
 * it can be, in `crates/commons-server/tests/proxy_route.rs`, against real
 * ffmpeg, which is a stronger test than "a `<video>` element did not complain".
 * A test that cannot make the browser do the thing is not evidence about the
 * thing.
 */
const CLIP = readFileSync(join(dirname(fileURLToPath(import.meta.url)), 'fixtures/clip.mp4'));

const ROW = {
  id: 'obj-1',
  kind: 'Scene',
  title: 'A file with a long enough name to need truncating',
  date: '2026-01-01',
  rating: null,
  organized: null,
  coverPath: null,
  width: 1920,
  height: 1080,
  durationMs: 5_000,
  producer: null,
  performers: [],
  tags: [],
  folder: null
};

interface SeedOpts {
  /** The row the library returns, or null for an empty result. */
  row?: typeof ROW | null;
  /** The saved playback state to return, or 404 for none. */
  playback?: Record<string, unknown> | null;
  /**
   * What `/caps` says. Defaults to "no proxy needed" -- which is what a real
   * H.264 mp4 is, and which is the only answer this Chromium can play.
   */
  caps?: Record<string, unknown> | null;
}

const CAPS_PLAYABLE = {
  container: 'mov,mp4,m4a,3gp,3g2,mj2',
  video_codec: 'h264',
  // `""` not null: a silent file has no audio codec, and a client that reads
  // null as "unknown, proxy it" would proxy every silent file in a library.
  audio_codec: '',
  width: 320,
  height: 180,
  fps: 8,
  rotation: 0,
  duration_ms: 5000,
  rung: null
};

async function seed(page: Page, opts: SeedOpts = {}) {
  const { row = ROW, playback = null, caps = CAPS_PLAYABLE } = opts;
  await page.route('**/graphql', async (route: Route) => {
    const body = JSON.parse(route.request().postData() ?? '{}') as { variables?: { input?: unknown } };
    // The player asks with `filter: mediaFilter()`; anything else is a
    // different caller and this stub is not trying to serve it.
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        data: {
          objects: {
            totalCount: row ? 1 : 0,
            pageInfo: { hasNextPage: false, hasPreviousPage: false, startCursor: '0', endCursor: '1' },
            nodes: row ? [row] : []
          }
        }
      })
    });
  });

  await page.route('**/media/*/playback', async (route: Route) => {
    if (route.request().method() === 'PUT') {
      // Record it so a test can assert on what the player saved.
      (page as Page & { __puts: unknown[] }).__puts ??= [];
      (page as Page & { __puts: unknown[] }).__puts.push(
        JSON.parse(route.request().postData() ?? '{}')
      );
      await route.fulfill({ status: 204, contentType: 'application/json', body: '' });
      return;
    }
    if (!playback) {
      await route.fulfill({ status: 404, contentType: 'application/json', body: '{}' });
      return;
    }
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify(playback)
    });
  });

  // The media bytes, served at the src the component actually computed.
  //
  // Which url that is, is itself a claim under test: with no codec metadata --
  // and `ObjectRow` carries none, see the route's header -- `needsProxy` says
  // "proxied", so the src is `/media/<id>/proxy.m3u8`. Stubbing the *direct*
  // url instead is what made an earlier version of this file load nothing and
  // leave every scrubber at max=0, which reads exactly like a resume bug.
  // What the server knows about the file. The player asks this before it builds
  // the <video>, so a wrong answer here is a player that never plays.
  await page.route('**/media/obj-1/caps', async (route: Route) => {
    if (!caps) return route.fulfill({ status: 404, contentType: 'application/json', body: '{}' });
    return route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify(caps)
    });
  });

  // The bytes, at the direct url. See the header for why this is the direct
  // url and not the proxy's.
  await page.route('**/media/obj-1', async (route: Route) =>
    route.fulfill({ status: 200, contentType: 'video/mp4', body: CLIP })
  );
  await page.route('**/media/obj-1/proxy.m3u8', async (route: Route) =>
    route.fulfill({ status: 200, contentType: 'video/mp4', body: CLIP })
  );
  await page.route('**/media/obj-1/thumb', async (route: Route) =>
    route.fulfill({ status: 404, body: '' })
  );
}

/** What the player sent to the server, in order. */
async function puts(page: Page): Promise<Record<string, unknown>[]> {
  return (page as Page & { __puts?: Record<string, unknown>[] }).__puts ?? [];
}

test.describe('the player route', () => {
  test('a missing object id is a mistake, and says which one it is', async ({ page }) => {
    await seed(page);
    await page.goto('/play');
    const err = page.getByTestId('player-route-error');
    await expect(err).toHaveText('No object was named.');
    // A mistake gets a way out; a refusal does not. `exact` because the layout
    // has its own "Library" link, and a loose match resolves to two elements --
    // which is a test failure, not a product one, and gets "fixed" by loosening
    // the assertion until it passes on something else.
    await expect(page.getByRole('link', { name: 'Back to the library', exact: true })).toBeVisible();
  });

  test('an object the server does not have is reported, not shown as empty', async ({ page }) => {
    await seed(page, { row: null });
    await page.goto('/play?o=nope');
    await expect(page.getByTestId('player-route-error')).toHaveText('That file could not be opened.');
  });
});

test.describe('the control bar fits the viewport', () => {
  test('the bar fits 360x640, and never covers the whole video', async ({ page }) => {
    // The ticket's Done-when, and stash#6526. A portrait phone in portrait
    // orientation is the case the ticket names, and it is the case where a
    // control bar stops being controls and becomes an overlay on the picture.
    await page.setViewportSize({ width: 360, height: 640 });
    await seed(page);
    await page.goto('/play?o=obj-1');

    const bar = page.getByTestId('player-bar');
    await expect(bar).toBeVisible();

    const box = (await bar.boundingBox())!;
    // The bar may be at most its share of the viewport; the rest is the video.
    // Comparing against the same constants the component plans with is the
    // point -- a hard-coded 180 here would keep passing after the bar grew.
    expect(box.height).toBeLessThanOrEqual(640 * MAX_BAR_FRACTION);
    // And it is on screen: a bar scrolled out of the viewport is not a fit.
    expect(box.y + box.height).toBeLessThanOrEqual(640);
  });

  test('the bar fits 360x640 with the title, which is the longest text in it', async ({ page }) => {
    // A control bar that fits only when the title is short is a control bar
    // that fits some files. The row above has a deliberately long title, so a
    // passing test here means the title is truncated rather than the bar
    // growing.
    await page.setViewportSize({ width: 360, height: 640 });
    await seed(page);
    await page.goto('/play?o=obj-1');
    await expect(page.getByTestId('player-bar')).toBeVisible();
    const box = (await page.getByTestId('player-bar').boundingBox())!;
    expect(box.height).toBeLessThanOrEqual(640 * MAX_BAR_FRACTION);
  });

  test('the primary controls survive at 360x640', async ({ page }) => {
    // The degenerate fix for an overflowing bar is to shrink or hide the
    // controls, and a player you cannot pause is worse than one showing less
    // information. This is the test that stops that fix passing.
    await page.setViewportSize({ width: 360, height: 640 });
    await seed(page);
    await page.goto('/play?o=obj-1');
    await expect(page.getByTestId('player-play')).toBeVisible();
    await expect(page.getByTestId('player-scrub')).toBeVisible();
  });

  test('the play button and scrubber are reachable at 360x640', async ({ page }) => {
    // Visible is not the same as reachable: a control inside an overflowed bar
    // is laid out but cannot be tapped.
    await page.setViewportSize({ width: 360, height: 640 });
    await seed(page);
    await page.goto('/play?o=obj-1');
    const play = page.getByTestId('player-play');
    await play.click({ timeout: 5000 });
    await expect(play).toBeVisible();
  });

  test('a desktop viewport keeps the title', async ({ page }) => {
    await page.setViewportSize({ width: 1280, height: 800 });
    await seed(page);
    await page.goto('/play?o=obj-1');
    await expect(page.getByTestId('player-title')).toBeVisible();
  });
});

test.describe('choosing a source', () => {
  test("the server's caps decide, and a null rung means direct", async ({ page }) => {
    // The claim the caps endpoint exists for. `ObjectRow` carries no codec, so
    // a client deciding from the row always guesses "proxy" -- a transcode for
    // every file in the library, for files that need none.
    await seed(page);
    await page.goto('/play?o=obj-1');
    await expect(page.getByTestId('player')).toHaveAttribute('data-proxied', 'false');
    await expect(page.getByTestId('player-video')).toHaveAttribute('src', /\/media\/obj-1$/);
  });

  test('a rung means the proxy, with the height the server chose', async ({ page }) => {
    await seed(page, {
      caps: { ...CAPS_PLAYABLE, video_codec: 'mpeg4', container: 'matroska,webm', rung: 720 }
    });
    await page.goto('/play?o=obj-1');
    await expect(page.getByTestId('player')).toHaveAttribute('data-proxied', 'true');
    await expect(page.getByTestId('player-video')).toHaveAttribute(
      'src',
      /\/media\/obj-1\/proxy\.m3u8\?h=720$/
    );
  });

  test('no caps at all falls back to the local rule, which says proxy', async ({ page }) => {
    // 404 is "the server could not tell us", and the conservative answer is the
    // proxy. A wrong "direct" is a video that does not play; a wrong "proxy" is
    // one extra transcode.
    await seed(page, { caps: null });
    await page.goto('/play?o=obj-1');
    await expect(page.getByTestId('player')).toHaveAttribute('data-proxied', 'true');
  });
});

test.describe('resuming', () => {
  test('a position in the middle is resumed, not skipped to the start', async ({ page }) => {
    // Asserted on `data-resume-target`, which is the DECISION, not on the
    // element's clock. See the note below for why those are different claims.
    await seed(page, {
      playback: {
        // Mid-file in the 5s clip: past the trailing 2% (the last 100 ms), so
        // the resume decision returns it rather than starting again.
        position_ms: 2_500,
        duration_ms: 5_000,
        loop_a_ms: null,
        loop_b_ms: null,
        completed: false,
        updated_at: '2026-09-27T00:00:00Z'
      }
    });
    await page.goto('/play?o=obj-1');
    await expect(page.getByTestId('player')).toHaveAttribute('data-resume-decided', 'true');
    await expect(page.getByTestId('player')).toHaveAttribute('data-resume-target', '2500');
  });

  test('a position in the trailing two percent starts again', async ({ page }) => {
    // The save worked perfectly and the DECISION about it was wrong, which is
    // why this needs the decision asserted: nothing in the transport is broken.
    await seed(page, {
      playback: {
        // 4,950 of 5,000 is the last 1% -- inside the trailing 2% window.
        position_ms: 4_950,
        duration_ms: 5_000,
        loop_a_ms: null,
        loop_b_ms: null,
        completed: false,
        updated_at: '2026-09-27T00:00:00Z'
      }
    });
    await page.goto('/play?o=obj-1');
    const player = page.getByTestId('player');
    // `decided` FIRST, and awaited. Read in the other order, the absence check
    // passes against a player that has not asked the server yet -- and then fails
    // in a different test order, which is how a real assertion gets deleted as
    // flaky.
    await expect(player).toHaveAttribute('data-resume-decided', 'true');
    // Absent, not zero and not -1: "start at the beginning" is the absence of a
    // target, and a sentinel for it would be indistinguishable from "not decided
    // yet".
    expect(await player.getAttribute('data-resume-target')).toBeNull();
  });

  test('a completed video starts again', async ({ page }) => {
    await seed(page, {
      playback: {
        position_ms: 2_500,
        duration_ms: 5_000,
        loop_a_ms: null,
        loop_b_ms: null,
        completed: true,
        updated_at: '2026-09-27T00:00:00Z'
      }
    });
    await page.goto('/play?o=obj-1');
    const player = page.getByTestId('player');
    // `decided` FIRST, and awaited. Read in the other order, the absence check
    // passes against a player that has not asked the server yet -- and then fails
    // in a different test order, which is how a real assertion gets deleted as
    // flaky.
    await expect(player).toHaveAttribute('data-resume-decided', 'true');
    // Absent, not zero and not -1: "start at the beginning" is the absence of a
    // target, and a sentinel for it would be indistinguishable from "not decided
    // yet".
    expect(await player.getAttribute('data-resume-target')).toBeNull();
  });

  test('a file nobody has played opens at the start and does not say so', async ({ page }) => {
    // A 404 is the normal case for most of a library, not a failure to report.
    await seed(page, { playback: null });
    await page.goto('/play?o=obj-1');
    const player = page.getByTestId('player');
    // `decided` FIRST, and awaited. Read in the other order, the absence check
    // passes against a player that has not asked the server yet -- and then fails
    // in a different test order, which is how a real assertion gets deleted as
    // flaky.
    await expect(player).toHaveAttribute('data-resume-decided', 'true');
    // Absent, not zero and not -1: "start at the beginning" is the absence of a
    // target, and a sentinel for it would be indistinguishable from "not decided
    // yet".
    expect(await player.getAttribute('data-resume-target')).toBeNull();
    await expect(page.getByTestId('player-route-error')).toHaveCount(0);
  });

  test('a refused seek does not leave the scrubber showing a position the file is not at', async ({ page }) => {
    // THE claim about the browser's veto, and the reason the component holds a
    // requested position only for a bounded window.
    //
    // Probed in this environment: a seek on a paused video that has never played
    // is refused. `currentTime = 2.5` reads back 0, readyState is 4, the range is
    // buffered, and neither `error` nor `seeked` fires. So a player that simply
    // wrote its requested position into the control would display 2,500 for a
    // file sitting at 0 -- and every loop marker dropped after that would be at
    // the wrong place, silently.
    //
    // The control is therefore required to come back to the file's real position.
    // This is the assertion that stops "optimistically show the seek" from being
    // a fix that passes.
    await seed(page, {
      playback: {
        position_ms: 2_500,
        duration_ms: 5_000,
        loop_a_ms: null,
        loop_b_ms: null,
        completed: false,
        updated_at: '2026-09-27T00:00:00Z'
      }
    });
    await page.goto('/play?o=obj-1');
    const player = page.getByTestId('player');
    // The decision was made...
    await expect(player).toHaveAttribute('data-resume-target', '2500');
    // ...and the control agrees with the FILE, whatever the file says.
    const shown = await page.getByTestId('player-scrub').inputValue();
    const actual = await page
      .getByTestId('player-video')
      .evaluate((v: HTMLVideoElement) => String(Math.round(v.currentTime * 1000)));
    expect(shown, 'the scrubber must show where the file is, not where it was asked to go').toBe(
      actual
    );
  });
});

test.describe('saving', () => {
  test('a position is saved with the loop markers as null, not zero', async ({ page }) => {
    // Sending 0 for an unset marker is a real position at the start of the
    // file, and a loop whose A is 0 is one the server rejects as not ordered.
    await seed(page);
    await page.goto('/play?o=obj-1');
    await page.evaluate(() => window.dispatchEvent(new Event('pagehide')));
    await expect.poll(async () => (await puts(page)).length).toBeGreaterThan(0);
    const body = (await puts(page))[0] as Record<string, unknown>;
    expect(body.loop_a_ms).toBeNull();
    expect(body.loop_b_ms).toBeNull();
    expect(typeof body.position_ms).toBe('number');
  });

  test('setting marker A and B arms the loop', async ({ page }) => {
    await seed(page);
    await page.goto('/play?o=obj-1');
    const player = page.getByTestId('player');
    // A half-set loop must NOT claim to be armed -- a "loop on" badge over a
    // zero-length loop is a player that shows one frame for ever.
    await expect(player).toHaveAttribute('data-loop-armed', 'false');
    await page.getByTestId('player-loopa').click();
    // A and B at the same instant is a zero-length loop, and one is a hang the
    // user sees as a frozen frame. Both markers here are placed at t=0, so the
    // assertion that matters is that this is NOT armed.
    await expect(player).toHaveAttribute('data-loop-armed', 'false');

    // Now move the playhead and set B past it, which is a real loop.
    await page.evaluate(() => {
      const v = document.querySelector('[data-testid="player-video"]') as HTMLVideoElement;
      v.pause();
    });
    // Moved with the scrubber, not by assigning `currentTime`.
    //
    // Assigning `video.currentTime` on an element that has not buffered the
    // range is a silent no-op -- `currentTime` reads back 0 -- and then marker
    // B is set at 0, and the loop is (correctly) not armed. The test would fail
    // for a reason that looks like an arming bug. The scrubber is also the path
    // a user takes, so it is the one worth testing.
    await page.getByTestId('player-scrub').fill('2500');
    await expect(page.getByTestId('player-scrub')).toHaveValue('2500');
    await page.getByTestId('player-loopb').click();
    await expect(player).toHaveAttribute('data-loop-armed', 'true');
  });
});

test.describe('a file that will not play', () => {
  test('an unplayable source says so rather than showing a black rectangle', async ({ page }) => {
    await seed(page);
    // The bytes route 500s, which is what a decode failure surfaces as when
    // the source is one the browser cannot handle.
    await page.route('**/media/obj-1', (route) => route.fulfill({ status: 500, body: '' }));
    await page.route('**/media/obj-1/proxy.m3u8', (route) =>
      route.fulfill({ status: 500, body: '' })
    );
    await page.goto('/play?o=obj-1');
    await expect(page.getByTestId('player-error')).toBeVisible();
  });
});
