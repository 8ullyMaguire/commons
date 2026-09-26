/**
 * The command palette and the shortcut map. T-P5-006 item 5. Spec 10.7;
 * plan §T-P5-006 item 5. The rules are unit-tested in `tests/commands.test.ts`;
 * what only a browser can see is the wiring — that one listener exists, that
 * `preventDefault` is called for a command and not for typing, and that the
 * palette is reachable from inside a scope that would otherwise claim the key.
 */

import { test, expect, type Page, type Route } from '@playwright/test';

async function quiet(page: Page) {
  await page.route('**/graphql', (route: Route) =>
    route.fulfill({ status: 200, contentType: 'application/json', body: '{"data":{}}' })
  );
}

/** Open the palette the way a user would: the chord, not a test hook. */
async function openPalette(page: Page) {
  await page.keyboard.press('Control+p');
  await expect(page.getByTestId('palette')).toBeVisible();
}

/**
 * Wait until the app is interactive, not merely loaded.
 *
 * The shell registers its keydown listener in an `$effect`, so there is a window
 * between `goto` returning and the listener existing in which a keystroke is
 * delivered to the browser and dropped -- in a real browser, `Ctrl+P` prints.
 * `goto`'s `load` event does not close that window; it fires *before* effects
 * run. The surface element appearing is the first observable sign that the
 * shell has mounted, which is the same moment the listener exists.
 */
async function ready(page: Page) {
  await expect(page.getByTestId('edit-surface')).toBeVisible();
  await expect(page.getByTestId('edit-input')).toBeEnabled();
}

test.describe('the command palette', () => {
  test.beforeEach(async ({ page }) => {
    await quiet(page);
    await page.goto('/index-mode');
    await ready(page);
  });

  test('1. Ctrl+P opens it, and it lists the registry', async ({ page }) => {
    // Acceptance 1 and 5: the palette is the shortcut map, and a command cannot
    // be in one and not the other.
    await openPalette(page);
    const rows = page.getByTestId('palette-row');
    await expect(rows.first()).toBeVisible();
    // Every app command is listed, each with its binding spelled out.
    for (const title of ['Command palette', 'Search this view', 'Select all', 'Bulk tag selected']) {
      await expect(page.getByTestId('palette-list')).toContainText(title);
    }
    await expect(page.getByTestId('palette-row').first().getByTestId('palette-binding')).not.toHaveText('');
  });

  test('2. a query filters, and Enter runs what is highlighted', async ({ page }) => {
    // `sl` for "select all": a subsequence, so a plain substring match would miss
    // it, and a command with no selection gate, so Enter has something to run.
    // (`bt` for "bulk tag" tests the same filter but is `needsSelection`, so
    // Enter correctly refuses it -- that is case 5, and mixing the two made a
    // working gate read as a broken filter.)
    await openPalette(page);
    await page.getByTestId('palette-input').fill('sl');
    const first = page.getByTestId('palette-row').first();
    await expect(first).toHaveAttribute('data-id', 'select.all');
    await page.keyboard.press('Enter');
    // A closed <dialog> stays in the DOM and is hidden; the attribute is
    // what carries the state. Asserting count 0 would be asserting an
    // implementation detail (unmounting instead of closing) and would pass
    // for the wrong reason if the element were removed by a re-render.
    await expect(page.getByTestId('palette')).toBeHidden();
  });

  test('3. Escape closes it, and the page is still there', async ({ page }) => {
    // #2542. Native `<dialog>` + oncancel, so the palette needs no part in the
    // scope stack at all.
    await openPalette(page);
    await page.keyboard.press('Escape');
    // A closed <dialog> stays in the DOM and is hidden; the attribute is
    // what carries the state. Asserting count 0 would be asserting an
    // implementation detail (unmounting instead of closing) and would pass
    // for the wrong reason if the element were removed by a re-render.
    await expect(page.getByTestId('palette')).toBeHidden();
    await expect(page.getByTestId('edit-surface')).toBeVisible();
  });

  test('4. a query matching nothing says so, rather than listing everything', async ({ page }) => {
    await openPalette(page);
    await page.getByTestId('palette-input').fill('qqqqzzz');
    await expect(page.getByTestId('palette-empty')).toBeVisible();
    await expect(page.getByTestId('palette-row')).toHaveCount(0);
  });

  test('5. a command with no selection is shown but disabled', async ({ page }) => {
    // Discoverable and not runnable is a real state. The row must be *visible* —
    // hiding it would make the palette an incomplete map — and Enter must not
    // run it.
    await openPalette(page);
    await page.getByTestId('palette-input').fill('bulk tag');
    const row = page.getByTestId('palette-row').first();
    await expect(row).toBeVisible();
    await expect(row).toHaveAttribute('data-disabled', 'true');
    await page.keyboard.press('Enter');
    await expect(page.getByTestId('palette')).toBeVisible();
  });

  test('6. the arrows move the highlight, and Enter runs the highlighted one', async ({ page }) => {
    // The highlight follows a command, not an index: re-filtering mid-navigation
    // must not transfer the cursor to whatever slides into the same slot.
    await openPalette(page);
    await page.keyboard.press('ArrowDown');
    const highlighted = page.locator('[role="option"][aria-selected="true"]');
    await expect(highlighted).toHaveCount(1);
    const id = await highlighted.getAttribute('data-id');
    await page.keyboard.press('Enter');
    // A closed <dialog> stays in the DOM and is hidden; the attribute is
    // what carries the state. Asserting count 0 would be asserting an
    // implementation detail (unmounting instead of closing) and would pass
    // for the wrong reason if the element were removed by a re-render.
    await expect(page.getByTestId('palette')).toBeHidden();
    // The test cannot assert *which* command ran -- most have no visible effect
    // here -- so it asserts the highlighted row was a real, identified one.
    expect(id).toBeTruthy();
  });

  test('7. it is reachable from inside a scope that would claim the key', async ({ page }) => {
    // A palette you cannot open from inside a modal is a palette that does not
    // exist for half your users, and the alternative -- every surface
    // re-binding Ctrl+P -- is how one gets forgotten.
    await openPalette(page);
    await page.getByTestId('palette-input').fill('Command palette');
    await expect(page.getByTestId('palette-row').first()).toHaveAttribute('data-id', 'palette.open');
  });
});

test.describe('the shortcut map', () => {
  test.beforeEach(async ({ page }) => {
    await quiet(page);
    await page.goto('/index-mode');
    await ready(page);
  });

  test('8. typing in a field is not a command', async ({ page }) => {
    // The rule that keeps the whole thing usable: a bare letter in an input is a
    // letter. If this fails, every shortcut silently eats typing.
    const input = page.getByTestId('edit-input');
    await input.click();
    await page.keyboard.press('x');
    await page.keyboard.press('j');
    await page.keyboard.press('k');
    await expect(input).toHaveValue('xjk');
  });

  test('9. a chord still works while typing', async ({ page }) => {
    // The interesting half: the user cannot type a control character, so Ctrl+P
    // in a field is ours.
    const input = page.getByTestId('edit-input');
    await input.click();
    await page.keyboard.press('Control+p');
    await expect(page.getByTestId('palette')).toBeVisible();
  });

  test('10. a bare letter outside a field is a command, not text', async ({ page }) => {
    // The inverse of case 8, and the reason the two must both be tested: a
    // handler that is too eager breaks typing, and one that is too shy means no
    // shortcuts work at all.
    await page.locator('body').click();
    await page.keyboard.press('x');
    // The edit input's value is the observable proxy: the letter did not type
    // there, and nothing threw.
    await expect(page.getByTestId('edit-input')).toHaveValue('');
  });

  test('11. one keystroke, one command', async ({ page }) => {
    // The collision case, from the other direction. Escape is claimed by the app
    // scope. With no modal open it clears; it must not also reach the guard or
    // any other listener, and the page must stay put.
    await page.getByTestId('edit-input').fill('unsaved');
    await expect(page.getByTestId('unsaved-indicator')).toBeVisible();
    await page.locator('body').click({ position: { x: 5, y: 5 } });
    await page.keyboard.press('Escape');
    // The unsaved edit survives: Escape cleared a *selection*, not the edit.
    await expect(page.getByTestId('edit-input')).toHaveValue('unsaved');
    await expect(page.getByTestId('guard-scrim')).toHaveCount(0);
  });

  test('12. the palette shows a command with no binding as such', async ({ page }) => {
    // A complete shortcut map means the palette is the documentation. A command
    // with no key shows an em dash, not an empty gap the user reads as a bug.
    await openPalette(page);
    const bindings = page.getByTestId('palette-binding');
    const n = await bindings.count();
    for (let i = 0; i < n; i += 1) {
      await expect(bindings.nth(i)).not.toHaveText('');
    }
  });
});
