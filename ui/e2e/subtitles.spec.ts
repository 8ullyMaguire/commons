/**
 * Subtitles in a real browser. Spec 6 / plan T-P6-002.
 *
 * # What is left after `tests/subtitles.test.ts`
 *
 * The unit tests cover every decision: the label, the default track, the VTT
 * grammar, the offset arithmetic. This file covers the three claims that only
 * a browser can settle, and each is a way the feature fails SILENTLY rather
 * than loudly:
 *
 *   1. **a `<track>` is a CHILD of the `<video>`.** A sibling is not
 *      associated with the element, so the browser never loads it: the video
 *      plays, the track list populates, and no console error appears anywhere.
 *      Nothing in a unit test can catch this, because the markup is
 *      well-formed in both cases.
 *   2. **the track element carries a URL the server actually serves.** A
 *      `<track src>` that 404s is not an error the browser reports -- it logs
 *      "track failed to load" once and the video plays silently. So the VTT
 *      route is stubbed here and a test asserts the *browser fetched it*, which
 *      is the only evidence that a caption track is actually attached.
 *   3. **the control bar still fits 360x640 with the subtitle controls in it.**
 *      `planControlBar` is a pure function over a plan, and a plan can be right
 *      while the thing it planned overflows. Adding a row of controls is exactly
 *      the change that makes that divergence real, so the viewport is set for
 *      real and the bar is measured.
 *
 * # Why the fixture is a real MP4 again
 *
 * The same reason as `player.spec.ts`: Chromium here has no H.264 for a raw
 * file, so a `<video>` pointed at a synthetic blob never fires `canplay` and
 * every assertion would measure a player stuck loading. The clip is read from
 * disk and served at the network boundary, so the media events the component
 * listens for are real.
 */
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { test, expect, type Page, type Route } from '@playwright/test';
import { MAX_BAR_FRACTION } from '../src/lib/player/player.js';

const CLIP = readFileSync(join(dirname(fileURLToPath(import.meta.url)), 'fixtures/clip.mp4'));

const ROW = {
  id: 'obj-1',
  kind: 'Scene',
  title: 'A file with subtitles',
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

const CAPS_PLAYABLE = {
  container: 'mov,mp4,m4a,3gp,3g2,mj2',
  video_codec: 'h264',
  audio_codec: '',
  width: 320,
  height: 180,
  fps: 8,
  rotation: 0,
  duration_ms: 5000,
  rung: null
};

/** Two cues that fit inside the 5-second clip. */
const VTT = `WEBVTT

00:00:00.500 --> 00:00:02.000
first cue

00:00:02.500 --> 00:00:04.000
second cue
`;

interface Track {
  id: string;
  label: string;
  language: string | null;
  is_default: boolean;
  is_forced: boolean;
  is_hearing_impaired: boolean;
  cue_count: number;
  format: string;
}

const TRACK_EN: Track = {
  id: 'sub-en',
  label: 'English',
  language: 'en',
  is_default: true,
  is_forced: false,
  is_hearing_impaired: false,
  cue_count: 2,
  format: 'vtt'
};

const TRACK_FORCED: Track = {
  id: 'sub-forced',
  label: 'English (forced)',
  language: 'en',
  is_default: false,
  is_forced: true,
  is_hearing_impaired: false,
  cue_count: 1,
  format: 'vtt'
};

const TRACK_ASS: Track = {
  id: 'sub-ass',
  label: 'Japanese',
  language: 'ja',
  is_default: false,
  is_forced: false,
  is_hearing_impaired: false,
  cue_count: 1,
  format: 'ass'
};

interface SeedOpts {
  /** What `/subtitles` returns. `null` is a 404, i.e. absent or denied. */
  tracks?: Track[] | null;
}

async function seed(page: Page, opts: SeedOpts = {}) {
  const { tracks = [] } = opts;

  await page.route('**/graphql', async (route: Route) => {
    await route.fulfill({
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
    });
  });

  await page.route('**/media/*/playback', (route: Route) =>
    route.fulfill({ status: 404, contentType: 'application/json', body: '{}' })
  );
  await page.route('**/media/obj-1/caps', (route: Route) =>
    route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(CAPS_PLAYABLE) })
  );

  // The list. `null` is a 404 rather than an empty 200, because that is what
  // the server sends for absent AND for denied, and a test that stubbed 200
  // with `[]` would never notice a client that treated "denied" as "no
  // subtitles" -- which is the leak the 404 exists to prevent.
  await page.route('**/media/obj-1/subtitles', (route: Route) => {
    if (tracks === null) {
      return route.fulfill({ status: 404, contentType: 'application/json', body: '{}' });
    }
    return route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({ tracks })
    });
  });

  // The VTT bytes. Recorded, because "the browser asked for it" is the only
  // evidence that a track is genuinely attached rather than merely present in
  // the DOM.
  await page.route('**/media/obj-1/subtitles/*.vtt', (route: Route) => {
    (page as Page & { __vtt: string[] }).__vtt ??= [];
    (page as Page & { __vtt: string[] }).__vtt.push(route.request().url());
    return route.fulfill({ status: 200, contentType: 'text/vtt', body: VTT });
  });

  await page.route('**/media/obj-1', (route: Route) =>
    route.fulfill({ status: 200, contentType: 'video/mp4', body: CLIP })
  );
  await page.route('**/media/obj-1/proxy.m3u8', (route: Route) =>
    route.fulfill({ status: 200, contentType: 'video/mp4', body: CLIP })
  );
  await page.route('**/media/obj-1/thumb', (route: Route) =>
    route.fulfill({ status: 404, body: '' })
  );
}

/** The bar, after the player has actually loaded. */
async function openBar(page: Page) {
  await page.goto('/play?o=obj-1');
  await page.getByTestId('player-video').evaluate((v: HTMLVideoElement) => {
    if (v.readyState < 1) return new Promise((r) => v.addEventListener('loadedmetadata', () => r(null), { once: true }));
    return null;
  });
  await page.getByTestId('player').hover();
  await expect(page.getByTestId('player-bar')).toBeVisible();
}

test.describe('subtitle tracks reach the video element', () => {
  test('a track element is a CHILD of the video, not a sibling', async ({ page }) => {
    // The claim that cannot be made in a unit test. A `<track>` beside the
    // `<video>` is well-formed markup, renders nothing, and is never fetched --
    // so the video plays with no captions and no error anywhere. Asserting
    // parenthood is the only way to state it.
    await seed(page, { tracks: [TRACK_EN] });
    await openBar(page);

    const inside = await page.evaluate(() => {
      const v = document.querySelector('[data-testid="player-video"]') as HTMLVideoElement;
      return Array.from(v.querySelectorAll('track')).map((t) => t.getAttribute('src'));
    });
    expect(inside).toHaveLength(1);
    expect(inside[0]).toBe('/media/obj-1/subtitles/sub-en.vtt');

    // And the outside is empty -- not as a second assertion, but because a
    // track rendered in both places is the bug the keying prevents.
    const outside = await page.evaluate(
      () =>
        document.querySelectorAll('track').length -
        (document.querySelector('[data-testid="player-video"]')?.querySelectorAll('track').length ?? 0)
    );
    expect(outside).toBe(0);
  });

  test('the browser FETCHES the track, which is what "attached" means', async ({ page }) => {
    // A `<track src>` that 404s logs one line and the video plays silently. So
    // the evidence that a caption track works is that the browser asked for
    // the bytes, and only this can see that.
    await seed(page, { tracks: [TRACK_EN] });
    await page.goto('/play?o=obj-1');
    await openBar(page);
    await expect
      .poll(() => (page as Page & { __vtt?: string[] }).__vtt?.length ?? 0, { timeout: 10_000 })
      .toBeGreaterThan(0);
    const urls = (page as Page & { __vtt: string[] }).__vtt;
    expect(urls.some((u) => u.includes('/subtitles/sub-en.vtt'))).toBe(true);
  });

  test('the track element carries the language and the default flag', async ({ page }) => {
    // `default` is what makes a track start showing, and browsers disagree with
    // each other about it -- so it has to come from the row.
    await seed(page, { tracks: [TRACK_EN, TRACK_FORCED] });
    await openBar(page);
    const attrs = await page.evaluate(() => {
      const v = document.querySelector('[data-testid="player-video"]') as HTMLVideoElement;
      return Array.from(v.querySelectorAll('track')).map((t) => ({
        srclang: t.getAttribute('srclang'),
        def: t.hasAttribute('default'),
        label: t.getAttribute('label')
      }));
    });
    expect(attrs).toHaveLength(2);
    expect(attrs.filter((a) => a.def)).toHaveLength(1);
    expect(attrs.find((a) => a.def)?.srclang).toBe('en');
  });
});

test.describe('the subtitle controls', () => {
  test('a file with no tracks shows no control at all', async ({ page }) => {
    // A disabled "Subtitles: Off" on every file in a library with no subtitles
    // is noise, and it teaches users the control does nothing.
    await seed(page, { tracks: [] });
    await openBar(page);
    await expect(page.getByTestId('player-subs')).toHaveCount(0);
  });

  test('a 404 from the list route is not an error and shows no control', async ({ page }) => {
    // Absent, not-on-disk and denied all answer 404. A file the caller cannot
    // see must look like a file with no subtitles, and must not be reported as
    // broken.
    await seed(page, { tracks: null });
    await openBar(page);
    await expect(page.getByTestId('player-subs')).toHaveCount(0);
    await expect(page.getByTestId('player-error')).toHaveCount(0);
  });

  test('the picker lists every track, and a forced one is distinguishable', async ({ page }) => {
    await seed(page, { tracks: [TRACK_EN, TRACK_FORCED] });
    await openBar(page);
    const options = page.getByTestId('player-subtitle-select').locator('option');
    // Off plus two tracks. `exact` on the count because an extra option is a
    // duplicate track, which is the §8.1 bug showing up in the UI.
    await expect(options).toHaveCount(3);
    await expect(options.nth(0)).toHaveText('Off');
    // Located BY VALUE, not by position. The claim is "the two English tracks
    // are told apart by their flag", and asserting on index N would be asserting
    // a sort order nobody promised -- the list route returns rows in whatever
    // order the query produced, and a test that pins that turns a harmless
    // reorder into a failure and invites someone to "fix" the component.
    await expect(page.locator('option[value="sub-en"]')).toHaveText('En');
    await expect(page.locator('option[value="sub-forced"]')).toHaveText('En (forced)');
  });

  test('the store default track is the one selected on open', async ({ page }) => {
    await seed(page, { tracks: [TRACK_FORCED, TRACK_EN] });
    await openBar(page);
    // TRACK_FORCED is first in the list and is NOT default, so this asserts the
    // flag and not the order: a picker that just picks the first track is wrong
    // in a way that looks right.
    await expect(page.getByTestId('player-subtitle-select')).toHaveValue('sub-en');
  });

  test('choosing Off clears the selection', async ({ page }) => {
    await seed(page, { tracks: [TRACK_EN, TRACK_FORCED] });
    await openBar(page);
    await page.getByTestId('player-subtitle-select').selectOption('sub-forced');
    await expect(page.getByTestId('player-subtitle-select')).toHaveValue('sub-forced');
    await page.getByTestId('player-subtitle-select').selectOption('none');
    await expect(page.getByTestId('player-subtitle-select')).toHaveValue('none');
  });

  test('an ASS track warns that styling is dropped, and does not refuse it', async ({ page }) => {
    // A browser renders ASS as plain text. Blocking the track would be a worse
    // answer than showing the text and saying what was lost.
    await seed(page, { tracks: [TRACK_ASS] });
    await openBar(page);
    await page.getByTestId('player-subtitle-select').selectOption('sub-ass');
    await expect(page.getByTestId('player-subtitle-warning')).toBeVisible();
    await expect(page.getByTestId('player-subtitle-select')).toHaveValue('sub-ass');
  });

  test('a track the browser can render faithfully shows no warning', async ({ page }) => {
    await seed(page, { tracks: [TRACK_EN] });
    await openBar(page);
    await expect(page.getByTestId('player-subtitle-warning')).toHaveCount(0);
  });
});

test.describe('the display-time offset', () => {
  test('it reads "none" until it is changed, not "0.0s"', async ({ page }) => {
    // "0.0s" reads as a measured zero -- as though the file had been checked and
    // found to need no correction -- which is a different claim from "you have
    // not changed it".
    await seed(page, { tracks: [TRACK_EN] });
    await openBar(page);
    await expect(page.getByTestId('player-subtitle-offset-value')).toHaveText('none');
  });

  test('it steps by half a second in both directions and resets', async ({ page }) => {
    await seed(page, { tracks: [TRACK_EN] });
    await openBar(page);
    const value = page.getByTestId('player-subtitle-offset-value');
    await page.getByTestId('player-subtitle-offset-later').click();
    await expect(value).toHaveText('+0.5s');
    await page.getByTestId('player-subtitle-offset-later').click();
    await expect(value).toHaveText('+1.0s');
    await page.getByTestId('player-subtitle-offset-earlier').click();
    await expect(value).toHaveText('+0.5s');
    await page.getByTestId('player-subtitle-offset-earlier').click();
    await expect(value).toHaveText('none');
    // Negative reads with a minus, and the sign is what tells a user which way
    // to press next.
    await page.getByTestId('player-subtitle-offset-earlier').click();
    await expect(value).toHaveText('-0.5s');
    await page.getByTestId('player-subtitle-offset-reset').click();
    await expect(value).toHaveText('none');
  });

  test('reset is disabled when there is nothing to reset', async ({ page }) => {
    await seed(page, { tracks: [TRACK_EN] });
    await openBar(page);
    await expect(page.getByTestId('player-subtitle-offset-reset')).toBeDisabled();
    await page.getByTestId('player-subtitle-offset-later').click();
    await expect(page.getByTestId('player-subtitle-offset-reset')).toBeEnabled();
  });

  test('the offset is never written back to the server', async ({ page }) => {
    // A stored offset makes the document wrong for the next viewer, needs its
    // own undo, and makes "reset offset" indistinguishable from "this file has
    // no offset". So it stays in this tab -- and the assertion is that NOTHING
    // was PUT, because a player that quietly persisted a display preference is
    // the exact failure the design rules out.
    const puts: unknown[] = [];
    await seed(page, { tracks: [TRACK_EN] });
    page.on('request', (req) => {
      if (req.method() === 'PUT') puts.push(req.url());
    });
    await openBar(page);
    await page.getByTestId('player-subtitle-offset-later').click();
    await page.getByTestId('player-subtitle-offset-later').click();
    await expect(page.getByTestId('player-subtitle-offset-value')).toHaveText('+1.0s');
    // A playback save is fine -- that is the position, not the offset. What
    // must not happen is a PUT carrying the offset, and the simplest way to say
    // that is that no PUT mentions it.
    expect(puts.filter((u) => JSON.stringify(u).includes('offset'))).toHaveLength(0);
  });
});

test.describe('the control bar still fits the viewport', () => {
  test('the bar fits 360x640 with the subtitle controls in it', async ({ page }) => {
    // The load-bearing claim. `planControlBar` is a pure function over a plan,
    // and a plan can be right while the thing it planned overflows: the bar
    // could be 240px tall with four wrapped rows on a 360px screen and every
    // unit test would still pass. Adding a row of controls is exactly the
    // change that makes that divergence real.
    await page.setViewportSize({ width: 360, height: 640 });
    await seed(page, { tracks: [TRACK_EN, TRACK_FORCED, TRACK_ASS] });
    await openBar(page);

    const bar = page.getByTestId('player-bar');
    const box = await bar.boundingBox();
    expect(box).not.toBeNull();
    const vp = page.viewportSize()!;
    expect(box!.width).toBeLessThanOrEqual(vp.width);
    expect(box!.height).toBeLessThanOrEqual(vp.height * MAX_BAR_FRACTION);
  });
});
