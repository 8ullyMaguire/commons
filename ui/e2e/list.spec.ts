/**
 * The list view's selection. Spec 10.6 (C53); plan T-P5-006 items 1-2.
 *
 * # The three things the unit tests cannot see
 *
 * The selection rules are pure and unit-tested, including the select-all
 * `XOR` and the `'unknown'` count. What a unit test cannot reach is the wiring:
 * that the checkbox reflects the mode, that shift-click reaches the table rather
 * than flipping the box it is over, and that a change of result prunes and says
 * so. Each of those is a bug that only exists between the value and the DOM.
 *
 * # The assertions read attributes, not the model
 *
 * `data-selected` on the table and on each row is what the count and the
 * highlight both derive from, so a table whose state is right while its rows
 * show the wrong thing fails here. Reading the component's own state would
 * prove the state is consistent with itself, which is the bug.
 */

import { test, expect, type Page, type Route } from '@playwright/test';

const TOTAL = 6;

async function seed(page: Page) {
  await page.route('**/graphql', async (route: Route) => {
    const body = JSON.parse(route.request().postData() ?? '{}') as {
      variables: { input: { first: number; after: string | null } };
    };
    const input = body.variables?.input ?? { first: 200, after: null };
    const start = input.after ? Number(input.after) : 0;
    const end = Math.min(start + (input.first || 200), TOTAL);
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        data: {
          objects: {
            totalCount: TOTAL,
            pageInfo: {
              hasNextPage: end < TOTAL,
              hasPreviousPage: false,
              startCursor: String(start),
              endCursor: String(end)
            },
            nodes: Array.from({ length: Math.max(0, end - start) }, (_, i) => ({
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
            }))
          }
        }
      })
    });
  });
}

const table = (page: Page) => page.locator('[data-testid="list-table"]');
const rows = (page: Page) => page.locator('[data-testid="list-row"]');
const rowSelect = (page: Page, id: string) =>
  page.locator(`[data-testid="list-row"][data-id="${id}"] [data-testid="row-select"]`);
const selectedCount = (page: Page) => table(page).getAttribute('data-selected');
const countText = (page: Page) => page.locator('[data-testid="selection-count"]').textContent();

async function openList(page: Page) {
  await seed(page);
  await page.goto('/list');
  await expect(rows(page)).toHaveCount(TOTAL);
}

test.describe('the list selection', () => {
  test('a row checkbox selects one row and the count follows', async ({ page }) => {
    await openList(page);
    expect(await selectedCount(page)).toBe('0');

    await rowSelect(page, 'obj-2').click();

    expect(await selectedCount(page)).toBe('1');
    // The row itself carries `data-selected`, so the selector matches the row
    // directly. Filtering for a *descendant* with the attribute looks like it
    // should work and silently matches nothing.
    await expect(page.locator('[data-testid="list-row"][data-selected="true"]')).toHaveCount(1);
    await expect(rowSelect(page, 'obj-2')).toBeChecked();
    await expect(rowSelect(page, 'obj-3')).not.toBeChecked();
  });

  test('shift-click extends over the loaded rows, including already-selected ones', async ({ page }) => {
    // The bug this catches: extending a range with a `reduce` of `toggle` would
    // deselect the rows in the span that were already selected, because
    // `toggle` cannot tell "make this selected" from "flip this".
    await openList(page);

    await rowSelect(page, 'obj-1').click();
    await expect(rowSelect(page, 'obj-1')).toBeChecked();

    // Shift-click the fourth row: rows 2, 3 and 4 join it.
    await rowSelect(page, 'obj-4').click({ modifiers: ['Shift'] });

    // Both the model and the box, because they are allowed to disagree and
    // did: an earlier version suppressed the checkbox's native toggle, so the
    // model said selected and the box said unchecked. A test that asserted
    // only the row attribute would have passed on that build, and the user
    // still could not select the row they were clicking.
    for (const id of ['obj-1', 'obj-2', 'obj-3', 'obj-4']) {
      await expect(rowSelect(page, id)).toBeChecked();
      await expect(page.locator(`[data-testid="list-row"][data-id="${id}"]`)).toHaveAttribute(
        'data-selected',
        'true'
      );
    }
    await expect(rowSelect(page, 'obj-5')).not.toBeChecked();
    await expect(page.locator('[data-testid="list-row"][data-id="obj-5"]')).toHaveAttribute(
      'data-selected',
      'false'
    );
    expect(await selectedCount(page)).toBe('4');
  });

  test('the header control selects everything, and one row can be turned back off', async ({ page }) => {
    // The select-all mode: every box reads checked, and the count is the
    // server's total rather than the loaded count -- the same number here, so
    // the assertion that matters is the deselect surviving.
    await openList(page);

    await page.locator('[data-testid="select-all"]').click();

    expect(await selectedCount(page)).toBe(String(TOTAL));
    for (let i = 0; i < TOTAL; i += 1) {
      await expect(rowSelect(page, `obj-${i}`)).toBeChecked();
    }

    // Turning one back off must stick -- a union of picked and excluded would
    // make this a no-op that the next re-render undoes.
    await rowSelect(page, 'obj-3').click();

    await expect(rowSelect(page, 'obj-3')).not.toBeChecked();
    await expect(rowSelect(page, 'obj-2')).toBeChecked();
    expect(await selectedCount(page)).toBe(String(TOTAL - 1));

    // And it is still off after a re-render triggered by something unrelated.
    await page.locator('[data-testid="select-all"]').hover();
    await expect(rowSelect(page, 'obj-3')).not.toBeChecked();
  });

  test('the header control is tri-state, and says "some" rather than nothing', async ({ page }) => {
    await openList(page);
    const head = page.locator('[data-testid="select-all"]');

    await expect(head).toHaveAttribute('data-state', 'none');
    await rowSelect(page, 'obj-1').click();
    // One of six: neither all nor none, and the control has to say so rather
    // than reading as "nothing is selected".
    await expect(head).toHaveAttribute('data-state', 'some');
    await expect(head).toHaveAttribute('aria-pressed', 'false');
  });

  test('the header control clears a select-all rather than adding to it', async ({ page }) => {
    await openList(page);
    const head = page.locator('[data-testid="select-all"]');

    await head.click();
    await expect(head).toHaveAttribute('data-state', 'all');
    expect(await selectedCount(page)).toBe(String(TOTAL));

    await head.click();
    await expect(head).toHaveAttribute('data-state', 'none');
    expect(await selectedCount(page)).toBe('0');
    await expect(rowSelect(page, 'obj-0')).not.toBeChecked();
  });

  test('the count is never a number the user could mistake for a total', async ({ page }) => {
    // With a server count, a select-all shows the server's number. The
    // `'unknown'` branch -- no server count at all -- is unit-tested; what this
    // pins is that whatever the mode, the text and the attribute agree.
    await openList(page);
    expect(await countText(page)).toContain('0 selected');

    await page.locator('[data-testid="select-all"]').click();
    expect(await countText(page)).toContain(`${TOTAL} selected`);
    expect(await selectedCount(page)).toBe(String(TOTAL));

    await rowSelect(page, 'obj-0').click();
    expect(await countText(page)).toContain(`${TOTAL - 1} selected`);
  });
});
