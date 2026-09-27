/**
 * A view is a URL, so a filter is a bookmark. T-P5-007, spec §15.10, and the
 * ticket's own Accept criterion: "navigate to a filtered+sorted grid, reload,
 * assert identical DOM state".
 *
 * # Why the existing grid deep-link test is not this test
 *
 * `grid.spec.ts` opens a URL that already carries the query and asserts the
 * page renders from it. That proves the READ half. This proves the WRITE half:
 * that a user who types a filter and picks a sort ends up with a URL that
 * reproduces what they are looking at. The read half can pass with no write
 * half at all -- a page that renders any URL correctly and never writes its
 * own passes `grid.spec.ts` and fails this, and a user who has filtered for an
 * hour has nothing to bookmark or send.
 *
 * # "Identical DOM state" is compared as the rendered rows, not as the URL
 *
 * A URL comparison would pass if the page normalised its own query. The
 * user-facing property is that the ROWS come back the same, so that is what is
 * compared -- which is also the thing a user would notice being wrong.
 */
import { test, expect, type Page } from '@playwright/test';

/** The rendered rows, in order. The thing a user perceives as "the view". */
async function rendered(page: Page): Promise<string[]> {
  return page.$$eval('[data-testid="grid-tile"]', (els) =>
    els.map((e) => e.getAttribute('data-id') ?? '')
  );
}

/**
 * The query string as a SORTED, space-joined list of `key=value` pairs.
 *
 * A string, deliberately. Two versions of this helper were wrong in
 * instructive ways and both made the app look broken when it was not:
 *
 *  1. It joined pairs with `&` but rendered each as `key,value`, so every
 *     `toContain('sort=title')` was looking for that in a string reading
 *     `sort,title`.
 *  2. Fixed to a real array of `key=value` -- and `toContain` against an array
 *     is an EXACT element match, so `toContain('sort=title')` needed the
 *     element to be exactly that with nothing else in the array. It passed on
 *     the shareable test (one param) and failed on the others (two params).
 *
 * Both read as "the URL is empty" in the failure output, which is the worst
 * possible misdirection. The string is back, with the `=` restored, because a
 * substring check is what every call site actually wants.
 */
function query(page: Page): string {
  const u = new URL(page.url());
  return [...u.searchParams.entries()]
    .map(([k, v]) => `${k}=${v}`)
    .sort((a, b) => a.localeCompare(b))
    .join('&');
}

/**
 * Three items with distinct titles, dates and ratings.
 *
 * The server SORTS. That is the point: the client sends `sort` and `dir` in
 * the query variables and renders whatever comes back, so a stub that returns a
 * fixed order makes every sort assertion pass no matter what the page did with
 * the user's choice. The page's job here is to put the choice in the URL; the
 * server's is to honour it; this stub is the server.
 *
 * `title`, not `name`, and `data.objects.nodes` -- the schema is asserted by
 * the grid, and a stub in the wrong shape renders zero tiles, which reads as
 * "the deep link did not work" rather than "my fixture is wrong".
 */
const ITEMS = [
  { id: 'obj-c', title: 'Charlie', date: '2024-03-01', rating: 3 },
  { id: 'obj-a', title: 'Alpha', date: '2024-01-01', rating: 1 },
  { id: 'obj-b', title: 'Bravo', date: '2024-02-01', rating: 2 },
  // No 'a' anywhere in the title, which is what makes a filter of 'a' a filter
  // rather than a no-op. The first fixture was three titles that ALL contained
  // 'a', so every "the filter was applied" assertion passed on an unfiltered
  // list -- the shape of a test that looks like coverage and is not.
  { id: 'obj-d', title: 'Zulu', date: '2024-04-01', rating: 4 }
];

/** The page's own filter, so a filter assertion is not a server assertion. */
function matches(item: (typeof ITEMS)[number], filter: string): boolean {
  if (!filter) return true;
  return item.title.toLowerCase().includes(filter.toLowerCase());
}

/**
 * The variables the page sent, read at the depth the page sends them.
 *
 * They are under `input`, not at the top level: `{"input":{"sort":"title",
 * "direction":"DESC",...}}`. Reading them one level up finds `undefined`,
 * sorts by the default, and makes every sort assertion in this file fail
 * against a page that is behaving exactly as designed.
 */
function input_(body: string): Record<string, unknown> {
  try {
    const parsed = JSON.parse(body);
    return (parsed.variables?.input ?? {}) as Record<string, unknown>;
  } catch {
    return {};
  }
}

function page_(body: string): (typeof ITEMS)[number][] {
  const variables = input_(body);
  const sort = String(variables.sort ?? 'date');
  const dir = variables.direction === 'ASC' ? 'ASC' : 'DESC';
  const q = String(variables.filter ?? '');
  const out = ITEMS.filter((i) => matches(i, q));
  const key = sort === 'title' ? 'title' : sort === 'rating' ? 'rating' : 'date';
  out.sort((a, b) => {
    const l = String(a[key]);
    const r = String(b[key]);
    return (l < r ? -1 : l > r ? 1 : 0) * (dir === 'ASC' ? 1 : -1);
  });
  return out;
}

/**
 * The stub server, attached to any page.
 *
 * A FUNCTION rather than a `beforeEach` body, because the shareable test opens
 * a second page in the same context and a `beforeEach` route does not travel
 * with it. The first version of this test registered the stub on `page` only,
 * so the shared page fetched against the real (absent) backend and rendered
 * nothing -- and the assertion that follows reads the OTHER page's rows, so
 * the failure says "the deep link is broken" when the truth is "my second
 * page had no data". A test that blames the product for its own fixture is
 * worse than no test.
 */
async function stub(page: Page) {
  await page.route('**/graphql', async (route) => {
    const items = page_(route.request().postData() ?? '{}');
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        data: {
          objects: {
            totalCount: items.length,
            pageInfo: {
              hasNextPage: false,
              hasPreviousPage: false,
              startCursor: '0',
              endCursor: String(items.length)
            },
            nodes: items.map((i) => ({
              __typename: 'Object',
              kind: 'Scene',
              organized: null,
              coverPath: null,
              width: 800,
              height: 1200,
              durationMs: null,
              ...i
            }))
          }
        }
      })
    });
  });
}

test.describe('a view survives a reload', () => {
  test.beforeEach(async ({ page }) => {
    await stub(page);
  });

  test('a sort applied by the user is in the URL and comes back on reload', async ({
    page
  }) => {
    await page.goto('/');

    // Drive the control rather than navigating: this is the WRITE half.
    await page.getByTestId('sort').selectOption('title');
    await expect.poll(() => query(page)).toContain('sort=title');

    const before = await rendered(page);
    expect(before).toHaveLength(4);
    // Sorted by title, DESCENDING -- which is the app's default direction, and
    // so the order a fixed-order stub would also produce. That is why the next
    // test flips the direction before asserting the order: an assertion that
    // passes on an unstubbed sort is not an assertion.
    await expect.poll(() => rendered(page)).toEqual(['obj-d', 'obj-c', 'obj-b', 'obj-a']);

    await page.reload();
    await expect.poll(() => rendered(page)).toEqual(before);
    expect(new URL(page.url()).searchParams.get('sort')).toBe('title');
  });

  test('a filter typed by the user is in the URL and comes back on reload', async ({
    page
  }) => {
    console.log('MARK goto');
    await page.goto('/');
    console.log('MARK filled-at ' + new Date().toISOString().slice(17,23));
    await page.getByTestId('search').fill('brav');
    console.log('MARK filled-done ' + new Date().toISOString().slice(17,23));
    // The debounce is real, so the assertion polls rather than assuming the
    // write is synchronous. Polling here is the difference between testing the
    // behaviour and testing the debounce's timing.
    await expect.poll(() => query(page)).toContain('q=brav');

    // WAIT for the rows, THEN read them. The URL is written before the refetch
    // it triggers, so polling the URL and immediately reading the DOM captures
    // the PREVIOUS render -- and the first version of this test did exactly
    // that, asserting against the stale unfiltered list and concluding the
    // deep link was broken. The page was right; the read was early.
    await expect.poll(() => rendered(page)).toEqual(['obj-b']);
    const before = await rendered(page);

    await page.reload();
    await expect.poll(() => rendered(page)).toEqual(before);
    // The BOX reflects the URL too, not just the rows. A page that renders the
    // right rows with an empty filter box is a page whose next keystroke
    // discards the filter the user thought they had.
    await expect(page.getByTestId('search')).toHaveValue('brav');
  });

  test('a filter AND a sort together, which is the case the ticket names', async ({ page }) => {
    await page.goto('/');
    await page.getByTestId('sort').selectOption('title');
    // 'a' matches Alpha, Bravo and Charlie, and not Zulu -- the fixture is
    // built so the filter reduces the set. A filter that matches everything
    // cannot demonstrate that a filter was applied.
    await page.getByTestId('search').fill('a');
    await expect.poll(() => query(page)).toContain('sort=title');
    await expect.poll(() => query(page)).toContain('q=a');

    // Sorted by title DESC: Charlie, Bravo, Alpha -- and Zulu is gone. The
    // order alone would not catch a dropped filter, since Zulu sorts last
    // either way; the third element being Alpha rather than Zulu is what
    // catches it.
    //
    // Wait for the ROWS, then read them. The URL is written before the refetch
    // it triggers, so reading the DOM straight after polling the URL captures
    // the previous render -- which is how the first version of this file spent
    // its afternoon failing against a page that was working.
    await expect.poll(() => rendered(page)).toEqual(['obj-c', 'obj-b', 'obj-a']);
    const before = await rendered(page);

    await page.reload();
    await expect.poll(() => rendered(page)).toEqual(before);
    // Both controls reflect the URL.
    await expect(page.getByTestId('sort')).toHaveValue('title');
    await expect(page.getByTestId('search')).toHaveValue('a');
  });

  test('the direction flip is in the URL and comes back on reload', async ({ page }) => {
    await page.goto('/');
    await page.getByTestId('sort').selectOption('date');
    await expect.poll(() => rendered(page)).toEqual(['obj-d', 'obj-c', 'obj-b', 'obj-a']);

    // The direction button. Descending is the default for date, so the first
    // click is the one that must appear in the URL -- and it must REVERSE the
    // rows, which is the half a URL-only assertion would miss.
    await page.getByRole('button', { name: /[↑↓]/ }).click();
    await expect.poll(() => query(page)).toContain('dir=ASC');
    await expect.poll(() => rendered(page)).toEqual(['obj-a', 'obj-b', 'obj-c', 'obj-d']);

    await page.reload();
    await expect.poll(() => rendered(page)).toEqual(['obj-a', 'obj-b', 'obj-c', 'obj-d']);
  });

  test('the URL is shareable: a fresh page opened on it renders the same view', async ({
    page,
    context
  }) => {
    await page.goto('/');
    await page.getByTestId('sort').selectOption('title');
    await page.getByTestId('search').fill('a');
    await expect.poll(() => query(page)).toContain('sort=title');
    await expect.poll(() => rendered(page)).toEqual(['obj-c', 'obj-b', 'obj-a']);
    const before = await rendered(page);
    const url = page.url();

    // A NEW page, not a reload. A reload keeps the same history entry, so a
    // page that only re-reads its own in-memory state would pass every reload
    // test in this file and fail this one -- and that is exactly what happens
    // when a user pastes the link into a chat.
    const other = await context.newPage();
    await stub(other);
    await other.goto(url);
    await expect.poll(() => rendered(other)).toEqual(before);
    await other.close();
  });

  test('a nonsense query is ignored rather than obeyed', async ({ page }) => {
    // A shared link gets edited, truncated, and pasted from something else, so
    // this is a normal way to arrive at a page. A page that throws on an
    // unrecognised value is a page a mangled copy-paste turns into a blank
    // screen -- and every other test here uses a VALID url, so none of them
    // would notice.
    await page.goto('/?sort=nonsense&dir=SIDEWAYS&density=999');

    // `sort` was the one field `decodeView` did not validate, so
    // `sort=nonsense` left the select with no matching option and it rendered
    // BLANK. `SORTS` and the option list now come from one place, so the
    // control and the decoder cannot disagree again. `density=999` is out of
    // range and falls back too.
    await expect(page.getByTestId('sort')).toHaveValue('date');
    await expect(page.locator('[data-testid="grid-tile"]').first()).toBeVisible();
  });

  test('a filter that matches nothing is an empty list, not an error', async ({ page }) => {
    // The other half, and the one that is easy to get wrong in the other
    // direction: a filter that legitimately matches nothing must NOT be
    // "recovered" into the default view. Showing everything because the user
    // asked for something impossible is worse than showing nothing, and the
    // first version of this test asserted tiles on a `q=<script>` page --
    // which matches nothing, so it asserted the exact bug this guards against.
    await page.goto('/?q=%3Cscript%3E');
    await expect(page.locator('[data-testid="grid-empty"]')).toBeVisible();
    await expect(page.locator('[data-testid="grid-tile"]')).toHaveCount(0);
  });
});
