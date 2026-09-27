/**
 * The bulk-edit modal. Spec 10.7; plan T-P5-006 item 3.
 *
 * # What only a browser can see
 *
 * The outcome states and the scope strings are pure and unit-tested in
 * `tests/bulk.test.ts`, including the `blocked`/`empty` split and every
 * singular/plural boundary. What a unit test cannot reach is the wiring that
 * actually threatens the user:
 *
 *  - the confirm button is disabled while the scope is unknown, so a select-all
 *    whose count has not arrived cannot be confirmed against a guess;
 *  - the *server's* numbers drive the message, not the selection's size — the
 *    two differ whenever a selected object is not the caller's to see;
 *  - a write that fails clears the previous outcome, so the modal never shows
 *    "Tagged 12 objects" above a red error;
 *  - the button in the page is disabled with nothing selected, so the modal
 *    cannot open on a write with no scope.
 *
 * # The assertions read the DOM, not the state
 *
 * Same rule as `list.spec.ts`: reading a component's own state proves it is
 * consistent with itself, which is not the bug. Every assertion here is about
 * what a user would see.
 */

import { test, expect, type Page, type Route } from '@playwright/test';

const TOTAL = 4;
const TAGS = [
  { id: 'tag-beach', name: 'beach' },
  { id: 'tag-2026', name: '2026' }
];

/**
 * Seed every GraphQL document this page sends.
 *
 * One route on the GraphQL path with a per-operation switch rather than one
 * route per
 * endpoint, because the transport sends every request to the same URL: an
 * object query, the tag list and the bulk mutation are three documents on one
 * path, and a test that stubs them separately would be stubbing a REST API this
 * app does not have. `tests/invariants.test.ts` is what forced that shape --
 * the first version of this page used `fetch('/api/bulk/tag')` and passed every
 * functional test while failing the invariant.
 *
 * `bulkApplied` is what the server "did", deliberately not derived from the
 * request: several tests below are about the request saying one thing and the
 * server doing another.
 */
interface Bulk {
  applied?: number;
  skipped_invisible?: number;
  requested?: number;
}

async function seed(
  page: Page,
  opts: { bulk?: Bulk; failBulk?: string } = {}
): Promise<{ target?: unknown; tagId?: unknown; source?: unknown }> {
  const sent: { target?: unknown; tagId?: unknown; source?: unknown } = {};

  await page.route('**/graphql', async (route: Route) => {
    const body = JSON.parse(route.request().postData() ?? '{}') as {
      query?: string;
      operationName?: string;
      variables?: Record<string, never>;
    };
    const doc = body.query ?? '';
    const vars = (body.variables ?? {}) as Record<string, never>;

    if (/\bquery\s+BulkTags\b/.test(doc)) {
      return route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({ data: { tags: TAGS } })
      });
    }

    if (/\bmutation\s+BulkApplyTag\b/.test(doc)) {
      sent.target = vars.target;
      sent.tagId = vars.tagId;
      sent.source = vars.source;
      // `opts` is read per request, not captured: a test assigns
      // `server.bulk` after `beforeEach` seeded, so a copy taken at seed time
      // would silently answer with the defaults.
      const bulk = { applied: 0, skipped_invisible: 0, requested: 0, ...opts.bulk };
      if (opts.failBulk) {
        return route.fulfill({
          status: 200,
          contentType: 'application/json',
          body: JSON.stringify({ errors: [{ message: opts.failBulk }] })
        });
      }
      return route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({
          data: {
            bulkApplyTag: {
              applied: bulk.applied,
              skippedInvisible: bulk.skipped_invisible,
              requested: bulk.requested
            }
          }
        })
      });
    }

    // The object page.
    const input = (vars.input ?? { first: 200, after: null }) as {
      first: number;
      after: string | null;
    };
    const startAt = input.after ? Number(input.after) : 0;
    const end = Math.min(startAt + (input.first || 200), TOTAL);
    return route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        data: {
          objects: {
            totalCount: TOTAL,
            pageInfo: {
              hasNextPage: end < TOTAL,
              hasPreviousPage: false,
              startCursor: String(startAt),
              endCursor: String(end)
            },
            nodes: Array.from({ length: Math.max(0, end - startAt) }, (_, i) => ({
              id: `obj-${startAt + i}`,
              kind: 'Scene',
              title: `Item ${startAt + i}`,
              date: '2026-01-01',
              rating: null,
              durationMs: null,
              organized: false,
              path: `/item/${startAt + i}`,
              thumbnailUrl: null
            }))
          }
        }
      })
    });
  });

  return sent;
}

const rowSelect = (page: Page, id: string) =>
  page.locator(`[data-testid="list-row"][data-id="${id}"] [data-testid="row-select"]`);

/** Select `n` rows by clicking their checkboxes, and open the modal. */
async function selectAndOpen(page: Page, n: number) {
  for (let i = 0; i < n; i += 1) {
    await rowSelect(page, `obj-${i}`).click();
  }
  await page.getByTestId('open-bulk').click();
  await expect(page.getByTestId('bulk-modal')).toBeVisible();
}

/** Choose a tag in the modal's select. */
async function chooseTag(page: Page, id: string) {
  await page.getByTestId('bulk-tag-select').selectOption(id);
}

test.describe('the bulk modal', () => {
  /**
   * The server's answer for this test.
   *
   * A shared object set before navigation rather than a second `page.route`
   * inside a test. Playwright runs handlers last-registered-first, so routing
   * again after `goto` shadows the handler serving the page query and the table
   * never fills -- a test bug that reads exactly like a broken component.
   */
  let server: { bulk?: Bulk; failBulk?: string } = {};
  let sent: { target?: unknown; tagId?: unknown; source?: unknown } = {};

  test.beforeEach(async ({ page }) => {
    server = {};
    sent = await seed(page, server);
    await page.goto('/list');
    await expect(page.locator('[data-testid="list-row"]').first()).toBeVisible();
  });

  test('cannot be opened with nothing selected', async ({ page }) => {
    // A modal that opens on an empty selection asks the user to confirm a write
    // with no scope, and its own message then says "no objects selected" —
    // which is the dialog telling the user it should not be open.
    await expect(page.getByTestId('open-bulk')).toBeDisabled();
  });

  test('names the scope before the write, not after', async ({ page }) => {
    await selectAndOpen(page, 2);
    await expect(page.getByTestId('bulk-scope')).toHaveText('2 objects selected.');
    // Nothing applied yet, so nothing reported.
    await expect(page.getByTestId('bulk-result')).toHaveCount(0);
  });

  test('does not enable confirm until a tag is chosen', async ({ page }) => {
    await selectAndOpen(page, 2);
    // The scope is known and the write would reach something, so the only thing
    // left gating the button is the tag.
    await expect(page.getByTestId('bulk-apply')).toBeDisabled();
    await chooseTag(page, 'tag-beach');
    await expect(page.getByTestId('bulk-apply')).toBeEnabled();
  });

  test('reports the server count, not the size of the selection', async ({ page }) => {
    // The request names four objects; the server reached two and refused two.
    // A modal reporting the request size tells the user four were tagged.
    server.bulk = { applied: 2, skipped_invisible: 2, requested: 4 };
    await selectAndOpen(page, 4);
    await chooseTag(page, 'tag-beach');
    await page.getByTestId('bulk-apply').click();

    const result = page.getByTestId('bulk-result');
    await expect(result).toHaveAttribute('data-state', 'partial');
    await expect(result).toContainText('Tagged 2 objects');
    await expect(result).toContainText('2 of your selection is not visible to you');
  });

  test('a fully blocked write is not reported as an empty one', async ({ page }) => {
    // Everything the user selected was invisible to them. Reporting "nothing
    // matched" would tell them their library is empty.
    server.bulk = { applied: 0, skipped_invisible: 3, requested: 3 };
    await selectAndOpen(page, 3);
    await chooseTag(page, 'tag-beach');
    await page.getByTestId('bulk-apply').click();

    const result = page.getByTestId('bulk-result');
    await expect(result).toHaveAttribute('data-state', 'blocked');
    await expect(result).toContainText('not visible to you');
    await expect(result).toContainText('nothing was changed');
  });

  test('sends a select-all as a QUERY target, never as the loaded page', async ({ page }) => {
    // The data-loss bug wearing a friendly label: a select-all that degrades to
    // the 4 loaded ids tags 4 of 4,000 and reports that it tagged everything.
    server.bulk = { applied: 4000 };

    await page.getByTestId('select-all').click();
    await expect(page.locator('[data-testid="list-row"][data-selected="true"]').first()).toBeVisible();
    await page.getByTestId('open-bulk').click();
    await expect(page.getByTestId('bulk-scope')).toContainText('Everything matching this search');
    await chooseTag(page, 'tag-beach');
    await page.getByTestId('bulk-apply').click();
    await expect(page.getByTestId('bulk-result')).toHaveAttribute('data-state', 'done');

    expect(sent.target).toMatchObject({ kind: 'QUERY' });
    expect((sent.target as { ids?: unknown }).ids).toBeUndefined();
    expect(sent.tagId).toBe('tag-beach');
  });

  test('sends a hand-picked selection as an IDS target', async ({ page }) => {
    // The other direction: a select-all must not turn into IDS, and a
    // hand-picked set must not turn into QUERY. Both directions are the same
    // bug -- a bulk edit against a set the user did not choose.
    server.bulk = { applied: 2, requested: 2 };

    await selectAndOpen(page, 2);
    await chooseTag(page, 'tag-beach');
    await page.getByTestId('bulk-apply').click();
    await expect(page.getByTestId('bulk-result')).toHaveAttribute('data-state', 'done');

    expect(sent.target).toMatchObject({ kind: 'IDS' });
    expect((sent.target as { ids?: string[] }).ids).toHaveLength(2);
  });

  test('carries a deselected id in the target, not by dropping the row', async ({ page }) => {
    // A select-all with one row turned off. The exclusion has to reach the
    // server: applied client-side, it is a guess about which rows the query
    // matches, and a selection that disagrees with the query is a bulk edit
    // against a different set than the user saw.
    server.bulk = { applied: 3999 };

    await page.getByTestId('select-all').click();
    await rowSelect(page, 'obj-1').click();
    await page.getByTestId('open-bulk').click();
    await chooseTag(page, 'tag-beach');
    await page.getByTestId('bulk-apply').click();
    await expect(page.getByTestId('bulk-result')).toHaveAttribute('data-state', 'done');

    const target = sent.target as { kind: string; excluded?: string[] };
    expect(target.kind).toBe('QUERY');
    expect(target.excluded).toHaveLength(1);
  });

  test('a failed write clears the previous outcome', async ({ page }) => {
    let calls = 0;
    await page.unroute('**/graphql');
    await page.route('**/graphql', async (route: Route) => {
      const body = JSON.parse(route.request().postData() ?? '{}') as { query?: string };
      calls += 1;
      if (/mutation\s+BulkApplyTag/.test(body.query ?? '')) {
        if (calls > 1) {
          return route.fulfill({
            status: 200,
            contentType: 'application/json',
            body: JSON.stringify({ errors: [{ message: 'no such tag' }] })
          });
        }
        return route.fulfill({
          status: 200,
          contentType: 'application/json',
          body: JSON.stringify({
            data: { bulkApplyTag: { applied: 2, skippedInvisible: 0, requested: 2 } }
          })
        });
      }
      // The page query and the tag list still have to work.
      if (/query\s+BulkTags/.test(body.query ?? '')) {
        return route.fulfill({
          status: 200,
          contentType: 'application/json',
          body: JSON.stringify({ data: { tags: TAGS } })
        });
      }
      return route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({
          data: {
            objects: {
              totalCount: TOTAL,
              pageInfo: { hasNextPage: false, hasPreviousPage: false, startCursor: '0', endCursor: '3' },
              nodes: Array.from({ length: TOTAL }, (_, i) => ({
                id: `obj-${i}`, kind: 'Scene', title: `Item ${i}`, date: '2026-01-01',
                rating: null, durationMs: null, organized: false, path: `/i/${i}`, thumbnailUrl: null
              }))
            }
          }
        })
      });
    });

    await selectAndOpen(page, 2);
    await chooseTag(page, 'tag-beach');
    await page.getByTestId('bulk-apply').click();
    await expect(page.getByTestId('bulk-result')).toContainText('Tagged 2 objects');

    await chooseTag(page, 'tag-2026');
    await page.getByTestId('bulk-apply').click();

    // The old success line and the new error cannot both be on screen: one
    // dialog, one action, two contradictory reports.
    await expect(page.getByTestId('bulk-error')).toContainText('no such tag');
    await expect(page.getByTestId('bulk-result')).toHaveCount(0);
  });

  test('escape closes without writing', async ({ page }) => {
    server.bulk = { applied: 2, requested: 2 };

    await selectAndOpen(page, 2);
    await chooseTag(page, 'tag-beach');
    // The dialog's own Escape, not a synthetic keydown: `showModal()` gives it
    // a native one that fires `close`, and a handler doing it as well would
    // call oncancel twice.
    await page.getByTestId('bulk-modal').press('Escape');
    await expect(page.getByTestId('bulk-modal')).not.toBeVisible();
    expect(sent.target).toBeUndefined();
  });

  // ---- the undo offer (item 7) -------------------------------------------
  //
  // These live here rather than in an `undo.spec.ts` because the offer is
  // rendered INSIDE this dialog, not in a floating toast. A toast was the first
  // design and it cannot work: this dialog is `showModal()`, which makes the
  // rest of the page inert, so a toast shown while the modal is open cannot be
  // clicked at all. Playwright reports that as "bulk-modal intercepts pointer
  // events", which names the wrong element and reads as a component bug.

  test('offers an undo counting what the write changed', async ({ page }) => {
    server.bulk = { applied: 2, skipped_invisible: 0, requested: 2 };
    await selectAndOpen(page, 2);
    await chooseTag(page, 'tag-beach');
    // Before the write, nothing is offered.
    await expect(page.getByTestId('bulk-undo')).toHaveCount(0);
    await page.getByTestId('bulk-apply').click();

    // The server's count, not the selection's — the two differ whenever a
    // selected object is not the caller's to see.
    await expect(page.getByTestId('bulk-undo-line')).toContainText('Undo 2 changes');
    await expect(page.getByTestId('bulk-undo')).toBeVisible();
  });

  test('offers nothing when the write changed nothing', async ({ page }) => {
    // Every selected object was invisible. "Undo 0 changes" with a live button
    // is a button that restores nothing.
    server.bulk = { applied: 0, skipped_invisible: 3, requested: 3 };
    await selectAndOpen(page, 3);
    await chooseTag(page, 'tag-beach');
    await page.getByTestId('bulk-apply').click();
    await expect(page.getByTestId('bulk-result')).toBeVisible();
    await expect(page.getByTestId('bulk-undo')).toHaveCount(0);
  });

  test('pressing Undo leaves a failure and the button does not come back', async ({ page }) => {
    // There is no mutation operation in the client at all, so the undo cannot be
    // performed. A button still on screen after the user pressed it is how they
    // press it six times.
    server.bulk = { applied: 2, skipped_invisible: 0, requested: 2 };
    await selectAndOpen(page, 2);
    await chooseTag(page, 'tag-beach');
    await page.getByTestId('bulk-apply').click();
    await expect(page.getByTestId('bulk-undo')).toBeVisible();

    await page.getByTestId('bulk-undo').click();
    await expect(page.getByTestId('bulk-undo-line')).toContainText('Undo failed');
    await expect(page.getByTestId('bulk-undo')).toHaveCount(0);
  });
});
