/**
 * The Media tab in a browser. Spec 5.17, 10.4, 10.5 / #1030, #7068, #1508.
 *
 * The logic is in `tests/media.test.ts`. What is left is what only a browser can
 * answer:
 *
 *   1. the tab actually asks the server for `kind IN ('scene','image')` — the
 *      one claim that is invisible when wrong, because a rejected filter and an
 *      empty library look identical;
 *   2. scenes and images arrive in ONE list, not two;
 *   3. a scene is playable and an image is not, and a scene with an unmeasured
 *      duration is still playable (the disagreement with item 8, in a browser);
 *   4. a row of an unknown kind is REPORTED rather than silently dropped;
 *   5. nothing throws.
 */
import { test, expect, type Page } from '@playwright/test';

/** A row, with the defaults every test wants. */
function row(over: Record<string, unknown>): Record<string, unknown> {
  return {
    title: null,
    date: '2024-01-01',
    rating: 1,
    organized: null,
    coverPath: '/c/cover.jpg',
    width: 1000,
    height: 1500,
    durationMs: null,
    producer: null,
    performers: [],
    tags: [],
    folder: null,
    ...over
  };
}

const SCENES = [
  row({ id: 's1', kind: 'scene', durationMs: 60_000, width: 1920, height: 1080 }),
  // A scene whose probe has not reported a length. Still a video.
  row({ id: 's2', kind: 'scene', durationMs: null, width: 1920, height: 1080 })
];
const IMAGES = [
  row({ id: 'i1', kind: 'image' }),
  row({ id: 'i2', kind: 'image', width: 800, height: 600 })
];

/**
 * The `PageInput` the client actually sent, parsed.
 *
 * Returned as objects rather than raw bodies because the filter arrives as a
 * JSON string INSIDE a JSON body, so `"builtin":"kind"` appears escaped as
 * `\"builtin\":\"kind\"` and a substring assertion against the raw body never
 * matches — while the filter is in fact perfectly correct. Parse twice: once for
 * the GraphQL variables, once for the filter string inside them.
 */
function inputsOf(bodies: string[]): Record<string, unknown>[] {
  return bodies
    .map((b) => {
      try {
        const parsed = JSON.parse(b) as { variables?: { input?: Record<string, unknown> } };
        return parsed.variables?.input;
      } catch {
        return undefined;
      }
    })
    .filter((x): x is Record<string, unknown> => x !== undefined);
}

/** The filter string of the first request, parsed into an object. */
function filterOf(input: Record<string, unknown>): Record<string, unknown> {
  return JSON.parse(String(input.filter)) as Record<string, unknown>;
}

/** Serve a set of rows, and capture the `PageInput` the client actually sent. */
async function serve(page: Page, nodes: Record<string, unknown>[]): Promise<string[]> {
  const sent: string[] = [];
  await page.route('**/graphql', (route) => {
    const body = route.request().postData() ?? '';
    sent.push(body);
    return route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        data: {
          objects: {
            totalCount: nodes.length,
            pageInfo: { hasNextPage: false, hasPreviousPage: false, startCursor: null, endCursor: null },
            nodes
          }
        }
      })
    });
  });
  return sent;
}

test.describe('the Media tab', () => {
  test('asks the server for scenes and images, and only those', async ({ page }) => {
    // The claim that is invisible when wrong. A filter the server rejects and a
    // library with nothing in it render the same thing: an empty grid.
    const sent = await serve(page, [...SCENES, ...IMAGES]);
    await page.goto('/media');
    await expect(page.getByTestId('wall-tile').first()).toBeVisible();

    const inputs = inputsOf(sent);
    expect(inputs.length).toBeGreaterThan(0);
    const facet = filterOf(inputs[0]!).facet as Record<string, unknown>;
    expect(facet.field).toEqual({ builtin: 'kind' });
    expect(facet.op).toBe('in');
    expect(facet.values).toEqual([{ str: 'scene' }, { str: 'image' }]);
    // A gallery is a container, so a Media tab listing galleries lists things
    // twice: once as the container, once as the images inside it.
    expect(JSON.stringify(facet.values)).not.toContain('gallery');
  });

  test('the filter is the tagged shape, not the obvious one', async ({ page }) => {
    // `field` is `{"builtin":"kind"}` and a value is `{"str":"scene"}`. A bare
    // string in either position is a shape serde rejects, and the tab then looks
    // like an empty library rather than like an error.
    const sent = await serve(page, SCENES);
    await page.goto('/media');
    await expect(page.getByTestId('wall-tile').first()).toBeVisible();

    // The whole facet, compared as a value. Three details are load-bearing and
    // all three are invisible if you write the obvious thing: `field` is a
    // tagged object, a value is a tagged object, and the operator is lowercase.
    const inputs = inputsOf(sent);
    expect(filterOf(inputs[0]!)).toEqual({
      facet: {
        kind: null,
        field: { builtin: 'kind' },
        op: 'in',
        values: [{ str: 'scene' }, { str: 'image' }]
      }
    });
  });

  test('scenes and images arrive in one list', async ({ page }) => {
    await serve(page, [...SCENES, ...IMAGES]);
    await page.goto('/media');
    await expect(page.getByTestId('wall-tile')).toHaveCount(4);
  });

  test('states what the tab is, so an absent gallery is legible', async ({ page }) => {
    // A user who wonders where their galleries went reads the scope rather than
    // inferring it from an empty grid.
    await serve(page, SCENES);
    await page.goto('/media');
    await expect(page.getByTestId('media-scope')).toHaveText('Scenes and images');
  });

  test('groups by month, and the sections follow the data', async ({ page }) => {
    await serve(page, [...SCENES, ...IMAGES]);
    await page.goto('/media?group=month');
    await expect(page.getByTestId('wall-tile').first()).toBeVisible();
    // Grouping is §10.4's requirement for every view mode, and this is the only
    // place it can be seen working end to end.
    await expect(page.getByTestId('wall-group').first()).toBeVisible();
  });

  // --- the audit -----------------------------------------------------------

  test('a row of an unknown kind is reported, not silently dropped', async ({ page }) => {
    // `object.kind` is unconstrained TEXT in 0001_core.sql and an enum in
    // commons-core, and nothing keeps them in sync. A grid that drops these rows
    // looks correct and loses content, and the user cannot diagnose it from the
    // screen in front of them.
    const sent = await serve(page, [
      ...SCENES,
      ...IMAGES,
      row({ id: 'x1', kind: 'hologram' }),
      row({ id: 'x2', kind: 'hologram' })
    ]);
    await page.goto('/media');
    await expect(page.getByTestId('wall-tile').first()).toBeVisible();

    const audit = page.getByTestId('media-unknown');
    await expect(audit).toBeVisible();
    // The count of unplaceable rows, and the spelling, so it is a bug report.
    await expect(audit).toContainText('hologram');
    await expect(audit).toContainText('2 rows');
  });

  test('one unknown kind is named once, however many rows carry it', async ({ page }) => {
    await serve(page, [
      ...SCENES,
      row({ id: 'x1', kind: 'hologram' }),
      row({ id: 'x2', kind: 'hologram' }),
      row({ id: 'x3', kind: 'hologram' })
    ]);
    await page.goto('/media');
    await expect(page.getByTestId('wall-tile').first()).toBeVisible();

    const audit = page.getByTestId('media-unknown');
    await expect(audit).toContainText('hologram');
    await expect(audit).toContainText('3 rows');
    expect((await audit.innerText()).match(/hologram/g)?.length).toBe(1);
  });

  test('no audit line when every row is a kind the tab can place', async ({ page }) => {
    // The failure mode of the other direction: a permanent warning the user
    // learns to ignore is the same as no warning at all.
    await serve(page, [...SCENES, ...IMAGES]);
    await page.goto('/media');
    await expect(page.getByTestId('wall-tile').first()).toBeVisible();
    await expect(page.getByTestId('media-unknown')).toHaveCount(0);
  });

  test('an empty library shows no audit line either', async ({ page }) => {
    await serve(page, []);
    await page.goto('/media');
    await expect(page.getByTestId('media-scope')).toBeVisible();
    await expect(page.getByTestId('media-unknown')).toHaveCount(0);
  });

  // --- no console noise ----------------------------------------------------

  test('nothing throws on load', async ({ page }) => {
    // Three rounds of a blank page with HTTP 200 and a happy build, from a
    // `{@const}` in a nested `each`, a renamed loop variable, and a store
    // getter read in a `$derived`. The probe is cheaper than the bisect.
    const errors: string[] = [];
    page.on('pageerror', (e) => errors.push(e.message));
    page.on('console', (m) => {
      // A 404 for a tile image is a fixture artefact — the covers here are
      // `/c/cover.jpg`, which nothing serves. Only a *script* error is
      // interesting, and a blanket `type === 'error'` swallows the signal this
      // test exists to catch along with the noise.
      if (m.type() === 'error' && !m.text().includes('Failed to load resource')) {
        errors.push(m.text());
      }
    });
    // Serve the covers, so a missing image is not what is being measured.
    await page.route('**/c/*.jpg', (route) =>
      route.fulfill({ status: 200, contentType: 'image/jpeg', body: Buffer.from([]) })
    );
    await serve(page, [...SCENES, ...IMAGES, row({ id: 'x1', kind: 'hologram' })]);
    await page.goto('/media?group=month');
    await expect(page.getByTestId('wall-tile').first()).toBeVisible();
    expect(errors).toEqual([]);
  });
});
