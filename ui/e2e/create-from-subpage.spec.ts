/**
 * Create from subpage, in a browser. §10.10 / #3694, #1017, #3122.
 *
 * The decisions — the summary strings, the classify split, the button label —
 * are in `tests/create.test.ts`, where they can be tested at the boundary. What
 * is left here is the three things only a browser can answer:
 *
 *   1. the button is live when it should be and dead when it should not be,
 *      which is the DOM's `disabled` rather than a boolean;
 *   2. the result line appears where the user is looking, not in a toast that
 *      the modal makes unclickable;
 *   3. the request that leaves carries the rows from the URL, so the URL is the
 *      state rather than a component variable that a reload would lose.
 *
 * The route is the real one, driven through a mocked GraphQL endpoint.
 */
import { expect, test, type Page, type Route } from '@playwright/test';

/**
 * The rows the server will say are new, existing, and refused.
 *
 * A fixture of only-new rows cannot tell the "already there" case from the
 * "created" one, and that distinction is the entire point of the action — a
 * response that only ever says "created" passes every other test here.
 */
const OUTCOME = { created: 1, existing: 2, refused: 0 };

interface CreateBody {
  readonly created: number;
  readonly existing: number;
  readonly refused: number;
}

async function openCreate(
  page: Page,
  outcome: CreateBody = OUTCOME,
  opts: { fail?: boolean; rows?: readonly string[] } = {}
): Promise<{ sent: string[][]; unseed: () => Promise<void> }> {
  const rows = opts.rows ?? ['one', 'two', 'three'];
  const sent: string[][] = [];

  const gql = async (route: Route) => {
    const body = route.request().postDataJSON() as { variables?: { rows?: string[] } };
    sent.push(body.variables?.rows ?? []);

    if (opts.fail) {
      return route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({ errors: [{ message: 'server said no' }] }),
      });
    }
    return route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({ data: { createAllMissing: outcome } }),
    });
  };
  await page.route('**/graphql', gql);

  const qs = rows.map((r) => `rows=${encodeURIComponent(r)}`).join('&');
  await page.goto(`/create-from-subpage?${qs}`);
  await expect(page.getByTestId('create-from-subpage')).toBeVisible();

  return {
    sent,
    async unseed() {
      await page.unroute('**/graphql', gql);
    },
  };
}

test.describe('create from subpage', () => {
  test('the button is live and says what it will create', async ({ page }) => {
    // No outcome known yet, so the label names the pasted count — and a
    // fixture where every row is new is what a user sees on an empty library.
    await openCreate(page, { created: 3, existing: 0, refused: 0 });
    const apply = page.getByTestId('create-apply');
    await expect(apply).toBeEnabled();
    await expect(apply).toHaveText('Create 3');
  });

  test('pressing it sends the rows from the URL and reports both counts', async ({ page }) => {
    const { sent, unseed } = await openCreate(page);

    await page.getByTestId('create-apply').click();

    // The claim that the state lives in the URL: the request carries exactly
    // what the link said, so a reload reproduces the same create.
    await expect.poll(() => sent.length).toBe(1);
    expect(sent[0]).toEqual(['one', 'two', 'three']);

    // And the result names what it skipped, rather than reporting 3 created.
    const result = page.getByTestId('create-result');
    await expect(result).toBeVisible();
    await expect(result).toHaveText('1 row created, 2 were already in your library.');
    await unseed();
  });

  test('a run that created nothing says the rows were already there', async ({ page }) => {
    // The failure a single success count produces: "Created 0 rows" reads as a
    // failure when in fact the library is full of exactly these.
    await openCreate(page, { created: 0, existing: 3, refused: 0 });
    await page.getByTestId('create-apply').click();
    await expect(page.getByTestId('create-result')).toHaveText(
      '3 rows were already in your library.'
    );
  });

  test('a refused row is named, not swallowed', async ({ page }) => {
    await openCreate(page, { created: 1, existing: 0, refused: 1 });
    await page.getByTestId('create-apply').click();
    const result = page.getByTestId('create-result');
    await expect(result).toHaveText('1 row created, 1 row refused.');
    // And it is rendered as the warning state, not as a clean success.
    await expect(result).toHaveAttribute('data-state', 'partial');
  });

  test('an error is shown in the dialog, and no result is claimed', async ({ page }) => {
    // The shape this guards: a thrown request rendered as "0 created" would
    // tell a user their paste was empty when it never completed.
    await openCreate(page, OUTCOME, { fail: true });
    await page.getByTestId('create-apply').click();
    await expect(page.getByTestId('create-error')).toBeVisible();
    await expect(page.getByTestId('create-result')).toHaveCount(0);
  });

  test('with nothing pasted the button is dead', async ({ page }) => {
    await openCreate(page, OUTCOME, { rows: [] });
    const apply = page.getByTestId('create-apply');
    await expect(apply).toBeDisabled();
    await expect(page.getByTestId('create-scope')).toHaveText('Nothing to create.');
  });

  test('a title with a space and an ampersand survives the URL', async ({ page }) => {
    // A separator a user can type into their own data is a separator that
    // eventually splits a row in the wrong place, which is why `rows` repeats
    // rather than joining.
    const rows = ['Two Words', 'A & B', 'quote " and \\ backslash'];
    const { sent, unseed } = await openCreate(page, OUTCOME, { rows });
    await page.getByTestId('create-apply').click();
    await expect.poll(() => sent.length).toBe(1);
    expect(sent[0]).toEqual(rows);
    await unseed();
  });
});
