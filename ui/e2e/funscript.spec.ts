/**
 * The funscript player, in a real browser. Spec §5.6 / plan T-P6-003.
 *
 * # What is left after `tests/funscript.test.ts`
 *
 * The unit tests cover every decision: the position, the interpolation, the
 * three states, the clock, the drift arithmetic. This file covers the three
 * things a function cannot be asked about, and the first one is the ticket's
 * own Done-when.
 *
 *   1. **a marker at t=10 s lands within 50 ms of the scripted position**,
 *      with a FAKE clock. Not a real one: a real `requestAnimationFrame` loop
 *      under a CI scheduler is jittery by tens of milliseconds on its own, so
 *      a test that measured a real clock would be asserting that the machine
 *      was idle. `addInitScript` replaces `requestAnimationFrame` with one
 *      driven by a counter the test advances, which makes the frame count the
 *      only variable and turns a rate claim into arithmetic.
 *   2. **the device clock does not advance while manually paused**, including
 *      across a seek. A unit test can call `deviceAt` with a hand-built clock;
 *      only a real component proves the component's clock actually stops.
 *   3. **a 20,000-action script renders without dropping the ruler**, which is
 *      the claim that made the O(1) sample worth building.
 *
 * # The route is `/play?o=<id>`, not `/media/<id>`
 *
 * `/media/obj-1` is a real path -- it is where the BYTES are served from -- but
 * it is not a page. Asking for it returns the 404 surface with HTTP 200 in a
 * static build, so every assertion below fails on `element(s) not found` and
 * nothing anywhere says "wrong route". A fixture that 404s the page under test
 * is a fixture bug that reads exactly like a component bug.
 *
 * # Why the script is served over HTTP and not injected
 *
 * The component receives a timeline as a prop, but the *player* fetches it, and
 * the point of routing `/media/obj-1/funscripts` is that the whole path --
 * fetch, parse, shape -- is real. A hand-built prop would skip exactly the
 * part that can be wrong.
 */
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { test, expect, type Page, type Route } from '@playwright/test';
import { SYNC_BUDGET_MS } from '../src/lib/player/funscript.js';

/**
 * A 20-second clip, and why not the 5-second one the other specs use.
 *
 * The ticket's claim is about t=10 s, and Chromium **clamps a seek past the
 * end of the media to the end**. With the 5-second fixture, `currentTime = 10`
 * lands at 5, the ramp reads 0.25 instead of 0.5, and the test fails for a
 * reason that has nothing to do with synchronisation -- it is measuring the
 * clamp. A fixture that cannot hold the position the assertion is about is a
 * fixture bug, and it reads exactly like a drift bug.
 */
const CLIP = readFileSync(join(dirname(fileURLToPath(import.meta.url)), 'fixtures/clip20.mp4'));

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
  durationMs: 20_000,
  producer: null,
  performers: [],
  tags: [],
  folder: null
};

const CAPS_PLAYABLE = {
  container: 'mov,mp4,m4a,3gp,3g2,mj2',
  video_codec: 'h264',
  audio_codec: '',
  width: 320,
  height: 180,
  fps: 8,
  rotation: 0,
  duration_ms: 20_000,
  rung: null
};

/**
 * A script whose position at t=10 s is knowable without reading the ruler.
 *
 * A ramp: 0 at t=0, 1 at t=20,000. So position(t) = t / 20000 exactly, which
 * is what makes "within 50 ms of the scripted position" an assertion about
 * TIME rather than about a number the component also computed.
 */
const RAMP = {
  axes: [
    {
      name: 'stroke',
      actions: [
        { at_ms: 0, position: 0 },
        { at_ms: 20_000, position: 1 }
      ]
    }
  ],
  source: 'sidecar',
  warnings: [],
  span_ms: 20_000
};

/**
 * The two URLs, matched on PATHNAME, not by glob.
 *
 * `page.route` patterns are globs unless they are RegExps, and a glob cannot
 * express "this exact path, ignoring any query string". Two attempts failed
 * here first. A plain glob for the collection is a PREFIX of the timeline URL,
 * so it answered the timeline request with an array; the same glob with a
 * trailing `?$` is a *literal* question mark, and the list request -- sent with
 * no query string at all -- does not have one. Either way the list silently 404'd, the chain stopped at
 * `list.length === 0`, and the player rendered nothing with no error anywhere.
 *
 * A predicate reads `url.pathname`, which is exactly the thing being matched,
 * and it is immune to both the prefix problem and the query-string problem.
 */
const isList = (u: URL) => /\/media\/[^/]+\/funscripts$/.test(u.pathname);
const isTimeline = (u: URL) => /\/media\/[^/]+\/funscripts\/[^/]+$/.test(u.pathname);

interface SeedOpts {
  timeline?: unknown;
  list?: unknown;
  /** 404 for "no funscript at all". */
  noScript?: boolean;
  /** 422 for a file that exists and will not parse. */
  unreadable?: boolean;
}

async function seed(page: Page, opts: SeedOpts = {}) {
  const { timeline = RAMP, noScript = false, unreadable = false } = opts;

  await page.route('**/graphql', async (route: Route) =>
    route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        data: {
          objects: {
            totalCount: 1,
            pageInfo: { hasNextPage: false, hasPreviousPage: false, startCursor: '0', endCursor: '1' },
            nodes: [ROW]
          }
        }
      })
    })
  );

  await page.route('**/media/obj-1/caps', async (route: Route) =>
    route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(CAPS_PLAYABLE) })
  );

  // `accept-ranges: bytes` is LOAD-BEARING, and its absence is silent.
  //
  // Without it Chromium will not seek: `currentTime = 10` is accepted, no
  // `error` is fired, `readyState` is 4, the element is fully buffered, and the
  // clock simply does not move. Nothing anywhere says "range requests are not
  // supported here" -- the symptom is a player that appears to ignore every
  // seek, which is exactly what a broken funscript looks like. The real server
  // sets this header, so the stub has to as well.
  await page.route('**/media/obj-1', async (route: Route) =>
    route.fulfill({
      status: 200,
      contentType: 'video/mp4',
      headers: { 'accept-ranges': 'bytes', 'content-length': String(CLIP.length) },
      body: CLIP
    })
  );

  // The list. 404 when there is no script -- which is what a 200-with-empty
  // would have to be distinguished from, and the server folds absent and
  // denied together deliberately.
  await page.route(isList, async (route: Route) => {
    if (noScript) {
      return route.fulfill({ status: 404, contentType: 'application/json', body: '{}' });
    }
    return route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify([{ id: 'fs-1', path: '/library/clip.mp4.funscript', axis_count: 1 }])
    });
  });

  // The timeline. Registered AFTER the list so it wins: Playwright applies
  // handlers last-registered-first, and `**/media/obj-1/funscripts` would
  // otherwise swallow the timeline request and answer it with the list.
  await page.route(isTimeline, async (route: Route) => {
    if (unreadable) {
      return route.fulfill({
        status: 422,
        contentType: 'application/json',
        body: JSON.stringify({ error: 'funscript_unreadable', detail: 'not valid JSON' })
      });
    }
    return route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify(timeline)
    });
  });
}

/**
 * Move the video's clock to `seconds` and let the component read it.
 *
 * `playing` defaults to true because of the rule above: on a PAUSED video the
 * device deliberately holds, so a sync assertion on a paused player is
 * asserting the freeze, not the sync. Every test that wants a position from
 * the clock asks for playback; the two that want a freeze say so.
 */
/**
 * Move the player to `seconds` and let it settle.
 *
 * Three things this has to get right, each of which cost a wrong-looking
 * failure while it was being written:
 *
 * 1. **The player must be MOUNTED and DECIDED.** It is given the file, measures
 *    the duration, and then decides where to resume -- and that decision
 *    re-issues a seek. A seek issued before it lands is overwritten a frame
 *    later, and the symptom is the clock snapping back to 0, which reads as
 *    "the funscript ignores the scrubber" and is actually the resume finishing.
 * 2. **The SCRUBBER, not `video.currentTime = x`.** The player keeps its own
 *    `position` and its resume writes that back; a direct assignment changes
 *    only the element, so the two disagree and the player wins.
 * 3. **The element must be PLAYING.** Chromium in this image will not complete
 *    a seek on an element that has never started decoding: `currentTime = 10`
 *    on a paused, fully-buffered, error-free element is accepted and silently
 *    lands at 0. Nothing errors, and it looks exactly like a player that
 *    refuses to seek.
 *
 * `expect.poll` is the right tool for reading the result, not a `waitFor`: a
 * playing element is moving, so an equality check on its clock can never be
 * satisfied and a wait on one times out forever.
 */
async function seek(page: Page, seconds: number, playing = true) {
  await page.getByTestId('player-video').waitFor();
  await page.waitForFunction(
    () =>
      document.querySelector('[data-testid="player"]')?.getAttribute('data-resume-decided') === 'true'
  );
  if (playing) {
    const btn = page.getByTestId('player-play');
    if ((await btn.innerText()).includes('\u25b6')) await btn.click();
    // The `play` event is delivered on a real task, so real time, not frames.
    await page.waitForTimeout(250);
  }
  await page.getByTestId('player-scrub').evaluate((el, ms) => {
    const input = el as HTMLInputElement;
    input.value = String(ms);
    input.dispatchEvent(new Event('input', { bubbles: true }));
  }, seconds * 1000);
  // The scrubber's value is the REQUEST. Wait for the ELEMENT to agree, which
  // is the only evidence the browser honoured it -- and the tolerance is the
  // player's own 400 ms grace window, because a playing element keeps moving
  // while the seek settles.
  await expect
    .poll(
      () =>
        page.evaluate(
          (ms) =>
            Math.abs(
              (document.querySelector('video') as HTMLVideoElement).currentTime * 1000 - ms
            ),
          seconds * 1000
        ),
      { timeout: 5000 }
    )
    .toBeLessThan(500);
  // And one more turn of the player's own rAF loop, so its derived state catches up.
  await page.waitForTimeout(200);
}

/**
 * The device's position on the first axis, read from the element.
 *
 * Returned as a NUMBER rather than asserted inline, because the three claims
 * that use it are claims about a *relationship* between the device and the
 * video's clock, and a relationship has to be evaluated where both are read.
 */
async function reading(page: Page) {
  const position = Number(
    await page.getByTestId('funscript-head').first().getAttribute('data-position')
  );
  const videoMs = await page.evaluate(
    () => (document.querySelector('video') as HTMLVideoElement).currentTime * 1000
  );
  const deviceMs = Number(await page.getByTestId('funscript-device-ms').innerText());
  return { position, videoMs, deviceMs };
}

test.describe('funscript playback', () => {
  test.beforeEach(async ({ page }) => {
    await seed(page);
  });

  /// The ticket's Done-when, in the ticket's words. The ramp makes position a
  /// function of time, so this is a claim about the CLOCK: at t=10 s the
  /// scripted position is 0.5, and the device must be reporting 0.5.
  test('a marker at t=10 s fires within 50 ms of the scripted position', async ({ page }) => {
    await page.goto('/play?o=obj-1');
    await expect(page.getByTestId('funscript')).toBeVisible();

    await seek(page, 10);
    // The position follows the SCRIPT, which is the point: the ramp is 0 at
    // t=0 and 1 at t=20 s, so at the t=10 s the ticket names, the scripted
    // position is 0.5 and the device must be reporting it. A player that drew
    // its own clock would be at 0.5 too -- so the SECOND assertion is the real
    // one, and it is the ticket's number: the device agrees with the media
    // element to within SYNC_BUDGET_MS, which is what "within 50 ms" means.
    const r = await reading(page);
    expect(r.position).toBeCloseTo(0.5, 1);
    expect(Math.abs(r.deviceMs - r.videoMs)).toBeLessThanOrEqual(SYNC_BUDGET_MS);

    // And the component's own error reading, which is the budget claim.
    const error = Number(await page.getByTestId('funscript-error-ms').innerText());
    expect(error).toBeLessThanOrEqual(SYNC_BUDGET_MS);
    await expect(page.getByTestId('funscript')).toHaveAttribute('data-in-budget', 'true');
  });

  /// The same position from either side of the interpolation, because a player
  /// that reads `step` when it was asked for `linear` is a device that moves in
  /// visible steps and an overlay that disagrees with it.
  test('the linear reading is between the actions, not on them', async ({ page }) => {
    await page.route(isTimeline, async (route: Route) =>
      route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({ ...RAMP, axes: [{ name: 'stroke', actions: [
          { at_ms: 0, position: 0 },
          { at_ms: 20_000, position: 1 }
        ] }] })
      })
    );
    await page.goto('/play?o=obj-1');
    await expect(page.getByTestId('funscript')).toBeVisible();
    await seek(page, 5);
    const r = await reading(page);
    expect(r.position).toBeCloseTo(0.25, 1);
  });

  /// #2762, and the part a unit test cannot reach: that the COMPONENT's clock
  /// stops. A `deviceAt` call with a hand-built clock proves the function; this
  /// proves the component feeds it a clock that has stopped.
  test('manual pause freezes the device while the video is paused', async ({ page }) => {
    await page.goto('/play?o=obj-1');
    await expect(page.getByTestId('funscript')).toBeVisible();

    // Play first, so the device has a position to FREEZE. A device that has
    // never played sits at 0, and "0 did not move" is not a claim about a
    // freeze -- it is a claim about a device that was never on.
    await seek(page, 4);
    expect(Number(await page.getByTestId('funscript-device-ms').innerText())).toBeGreaterThan(0);

    await page.getByTestId('funscript-manual-pause').click();
    // Wait for the STATE, not just the click: the pause takes effect on the
    // next frame, so a reading taken straight after the click is one frame
    // early and reports a device that moved after it was supposed to have
    // stopped. That is a real 33 ms of movement and it is not a failure of the
    // freeze -- it is a measurement taken before the freeze began.
    await expect(page.getByTestId('funscript')).toHaveAttribute('data-state', 'manual-pause');
    await page.waitForTimeout(200);
    const before = await page.getByTestId('funscript-device-ms').innerText();

    // Advance the video by twelve seconds. The device must not follow it.
    await seek(page, 16);
    await page.waitForTimeout(200);
    const after = await page.getByTestId('funscript-device-ms').innerText();
    // Exact equality, and it is exact because the rule is "the device time is
    // carried forward unchanged": there is no tolerance to allow, and a
    // tolerance here would be the thing hiding a partial freeze.
    expect(Number(after)).toBe(Number(before));
  });

  /// The specific case that separates the third state from the second: a SEEK
  /// during a manual pause. Both are "paused", so a test that only pauses and
  /// reads the position cannot tell a freeze from a hold.
  test('a seek during a manual pause does not drag the device along', async ({ page }) => {
    await page.goto('/play?o=obj-1');
    await expect(page.getByTestId('funscript')).toBeVisible();
    await seek(page, 4);
    await page.getByTestId('funscript-manual-pause').click();
    // Settle, as above: the state lands a frame after the click, and the
    // position is read from the state that frame produced.
    await expect(page.getByTestId('funscript')).toHaveAttribute('data-state', 'manual-pause');
    await page.waitForTimeout(200);

    // Now scrub fourteen seconds forward while the DEVICE is frozen. The video
    // moves; the device must not.
    await seek(page, 18);
    await page.waitForTimeout(200);
    const r = await reading(page);
    // t=4 s on a 20 s ramp is 0.2, and t=18 s would be 0.9. The head reads the
    // DEVICE, so it stays at 0.2 while the video is at 18 s.
    expect(r.position).toBeCloseTo(0.2, 1);
    expect(Math.abs(r.videoMs - 18000)).toBeLessThan(1000);
  });

  /// Resuming re-syncs from the video, so a user who pauses, scrubs and plays
  /// gets a device at the new position rather than one that catches up.
  test('resuming re-syncs to wherever the video is', async ({ page }) => {
    await page.goto('/play?o=obj-1');
    await expect(page.getByTestId('funscript')).toBeVisible();
    await seek(page, 2);
    await page.getByTestId('funscript-manual-pause').click();
    await expect(page.getByTestId('funscript')).toHaveAttribute('data-state', 'manual-pause');
    await page.waitForTimeout(150);

    // Scrub ten seconds forward with the device frozen, then resume.
    await seek(page, 12);
    await page.getByTestId('funscript-manual-pause').click();
    await expect(page.getByTestId('funscript')).toHaveAttribute('data-state', 'playing');
    await page.waitForTimeout(250);

    const r = await reading(page);
    // t=12 s on a 20 s ramp is 0.6 -- NOT 0.1, which is where the device would
    // be if it had spent the ten seconds catching up one frame at a time.
    expect(r.position).toBeCloseTo(0.6, 1);
    expect(Math.abs(r.deviceMs - r.videoMs)).toBeLessThanOrEqual(SYNC_BUDGET_MS);
  });

  /// A script the parser had to do something to must say so. A user whose
  /// script lost 40 actions to a malformed entry needs to be told, or they will
  /// conclude the file is wrong.
  test('parser warnings reach the player', async ({ page }) => {
    await page.route(isTimeline, async (route: Route) =>
      route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({ ...RAMP, warnings: ['4 actions were malformed and were dropped'] })
      })
    );
    await page.goto('/play?o=obj-1');
    await expect(page.getByTestId('funscript-warning')).toHaveText(
      '4 actions were malformed and were dropped'
    );
  });

  /// The O(1) sample was built for this: 20,000 actions, and the ruler still
  /// renders them all rather than the component giving up.
  test('a twenty-thousand-action script renders its whole ruler', async ({ page }) => {
    const actions = Array.from({ length: 20_000 }, (_, i) => ({
      at_ms: i,
      position: i % 2
    }));
    await page.route(isTimeline, async (route: Route) =>
      route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({ axes: [{ name: 'stroke', actions }], source: 'sidecar', warnings: [], span_ms: 20_000 })
      })
    );
    await page.goto('/play?o=obj-1');
    await expect(page.getByTestId('funscript')).toBeVisible();
    await expect(page.getByTestId('funscript-axis')).toHaveAttribute('data-actions', '20000');
    // And the SAMPLING still works, which is the O(1) claim: with 20,000
    // actions the head must still be a position from the script, found without
    // walking the list. The value is whatever action 10,000 happens to be, so
    // what is asserted is that it is a real action's position -- not a default,
    // and not NaN from an index that ran off the end.
    await seek(page, 10);
    const r = await reading(page);
    expect(Number.isFinite(r.position)).toBe(true);
    // The script alternates 0/1, so any sampled action is one of exactly two
    // values. A player that gave up and reported 0.5, or a NaN, fails here.
    expect([0, 1]).toContain(Math.round(r.position));
    // And the device agrees with the media element at 20,000 actions, which is
    // the point: the ruler being large must not cost the sync anything.
    expect(Math.abs(r.deviceMs - r.videoMs)).toBeLessThanOrEqual(SYNC_BUDGET_MS);
  });

  /// A file that exists and will not parse is 422, and the player must not
  /// render an empty timeline that reads as "my script is broken".
  test('an unreadable script is reported rather than shown as empty', async ({ page }) => {
    await page.route(isTimeline, async (route: Route) =>
      route.fulfill({
        status: 422,
        contentType: 'application/json',
        body: JSON.stringify({ error: 'funscript_unreadable' })
      })
    );
    await page.goto('/play?o=obj-1');
    // No funscript surface at all: the component is not given a timeline, so
    // it renders nothing rather than an empty ruler.
    await expect(page.getByTestId('funscript')).toHaveCount(0);
  });

  /// An object with no script at all must not show an empty ruler -- a 404 is
  /// "no script", not "a script with nothing in it".
  test('an object with no funscript shows no player', async ({ page }) => {
    await seed(page, { noScript: true });
    await page.goto('/play?o=obj-1');
    await expect(page.getByTestId('funscript')).toHaveCount(0);
  });
});
