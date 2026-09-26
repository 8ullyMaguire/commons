/**
 * Unsaved-entry protection. Spec 10.7, 10.10; plan T-P5-006 item 4.
 *
 * # The plan's floor, and the three cases it does not name
 *
 * The plan says: "The unsaved-protection test must attempt navigation with an
 * unsaved edit and assert a confirm appears. **Done when: that test exists.**"
 * That is case 1. A test that exists is not a test that passes, and a guard that
 * prompts on *every* navigation would satisfy the letter of it while being
 * worse than nothing — so case 3 below is the one that makes the guard worth
 * having.
 *
 * The rules (`shouldPrompt`, `summary`, the ordering) are pure and unit-tested
 * in `tests/guard.test.ts`. What only a browser can see is the wiring: that
 * `page.url` is the trigger, that cancelling leaves the URL *and* the edit
 * alone, and that "leave anyway" consumes before navigating rather than after.
 *
 * # The ordering assertion
 *
 * Case 4 is the one the spec calls "the whole mechanism". A guard that clears
 * after the navigation lands re-prompts on the way out, and a user re-prompted
 * for the edit they just chose to lose learns to click through. The test walks
 * away, comes back, and asserts the second navigation is silent — which only
 * happens if the consume came first.
 */

import { test, expect, type Page, type Route } from '@playwright/test';

/** Nothing on the index route needs the network, but the layout is shared. */
async function quiet(page: Page) {
  await page.route('**/graphql', (route: Route) =>
    route.fulfill({ status: 200, contentType: 'application/json', body: '{"data":{}}' })
  );
}

/** Go to the editor surface and type something, leaving it unsaved. */
async function makeDirty(page: Page, text = 'a new title') {
  await page.getByTestId('edit-input').fill(text);
  await expect(page.getByTestId('edit-dirty')).toHaveText('dirty');
  await expect(page.getByTestId('unsaved-indicator')).toBeVisible();
}

/** The two toolbar links, by their visible names. */
const toLibrary = (page: Page) => page.getByRole('link', { name: 'Library' });
const toIndex = (page: Page) => page.getByRole('link', { name: 'Index' });

test.describe('the unsaved guard', () => {
  test.beforeEach(async ({ page }) => {
    await quiet(page);
    await page.goto('/index-mode');
    await expect(page.getByTestId('edit-surface')).toBeVisible();
  });

  test('1. navigating with an unsaved edit asks first', async ({ page }) => {
    // The plan's own floor.
    await makeDirty(page);
    await toLibrary(page).click();

    await expect(page.getByTestId('guard-scrim')).toBeVisible();
    await expect(page.getByTestId('guard-message')).toContainText('unsaved change');
  });

  test('2. staying changes nothing at all', async ({ page }) => {
    // Not "the dialog closed" — the URL did not move, and the edit is still
    // there and still counted. A guard that resolves false and then clears
    // state has discarded the edit *and* left the user where they were, which
    // is the worst of both outcomes.
    await makeDirty(page);
    await toLibrary(page).click();
    await expect(page.getByTestId('guard-scrim')).toBeVisible();

    await page.getByTestId('guard-stay').click();

    await expect(page.getByTestId('guard-scrim')).toHaveCount(0);
    expect(new URL(page.url()).pathname).toBe('/index-mode');
    // The edit survived, and is still reported as unsaved.
    await expect(page.getByTestId('edit-input')).toHaveValue('a new title');
    await expect(page.getByTestId('edit-dirty')).toHaveText('dirty');
    await expect(page.getByTestId('unsaved-indicator')).toBeVisible();
  });

  test('3. navigating with nothing unsaved does not ask', async ({ page }) => {
    // The case a test written only against the floor would never check, and the
    // one that decides whether the guard is a feature or wallpaper.
    await expect(page.getByTestId('unsaved-indicator')).toHaveCount(0);
    await toLibrary(page).click();

    await expect(page.getByTestId('guard-scrim')).toHaveCount(0);
    await expect(page).toHaveURL(/\/$/);
  });

  test('4. leaving anyway consumes before navigating, so it does not ask twice', async ({ page }) => {
    // "The whole mechanism", per the spec. Consuming after the navigation lands
    // re-prompts on the way out.
    await makeDirty(page);
    await toLibrary(page).click();
    await expect(page.getByTestId('guard-scrim')).toBeVisible();
    await page.getByTestId('guard-leave').click();

    await expect(page).toHaveURL(/\/$/);
    await expect(page.getByTestId('guard-scrim')).toHaveCount(0);
    // The indicator is gone: the edit was abandoned, not merely hidden.
    await expect(page.getByTestId('unsaved-indicator')).toHaveCount(0);

    // And back again, with no second prompt. If the consume had come after the
    // navigation, this click would raise the scrim a second time.
    await toIndex(page).click();
    await expect(page).toHaveURL(/\/index-mode$/);
    await expect(page.getByTestId('guard-scrim')).toHaveCount(0);
  });

  test('5. saving clears the registration, so the guard goes quiet', async ({ page }) => {
    // A surface that sets a boolean and forgets to clear it on save gives the
    // user a phantom prompt forever with nothing left to save -- which is the
    // other way this feature becomes wallpaper.
    await makeDirty(page);
    await page.getByTestId('edit-save').click();
    await expect(page.getByTestId('edit-dirty')).toHaveText('clean');
    await expect(page.getByTestId('unsaved-indicator')).toHaveCount(0);

    await toLibrary(page).click();
    await expect(page.getByTestId('guard-scrim')).toHaveCount(0);
  });

  test('6. a failed save blocks with nothing pending, and says which it is', async ({ page }) => {
    // `isUnsaved(0, true)`: the user hit save, it failed, and there is nothing
    // in flight. The prompt must not call this "unsaved changes" -- a user told
    // only that assumes their work is held somewhere and leaves.
    await makeDirty(page);
    await page.getByTestId('edit-save').click();
    await page.getByTestId('edit-fail').click();
    await expect(page.getByTestId('edit-dirty')).toHaveText('dirty');

    await toLibrary(page).click();
    const msg = page.getByTestId('guard-message');
    await expect(msg).toContainText('failed to save');
    await expect(msg).toContainText('already be lost');
  });

  test('7. the prompt names the surface', async ({ page }) => {
    // §10.7: a long task confirms with its scope spelled out. "Are you sure?"
    // does not, and does not let the user tell one unsaved edit from three.
    await makeDirty(page);
    await toLibrary(page).click();
    await expect(page.getByTestId('guard-message')).toContainText('Index demo object');
  });

  test('8. the back button is guarded too', async ({ page }) => {
    // Not just the links. A guard that only intercepts clicks is bypassed by the
    // one navigation gesture every browser offers.
    //
    // Two fixture details this needs, both learned the hard way. The navigation
    // to `/list` must be an in-app click: `page.goto` is a full document load
    // that destroys the page component, so the surface deregisters on the way
    // out and there is nothing left to guard when the history entry lands. And
    // it has to happen at all -- with only one entry in the app's history,
    // `goBack` leaves the site entirely rather than going to a sibling route.
    await makeDirty(page);
    await toLibrary(page).click();
    await expect(page.getByTestId('guard-scrim')).toBeVisible();
    await page.getByTestId('guard-leave').click();
    await expect(page).toHaveURL(/\/$/);

    // Now back with a *fresh* edit, so the registry is populated again.
    await toIndex(page).click();
    await expect(page).toHaveURL(/\/index-mode$/);
    await makeDirty(page, 'a second edit');

    await page.goBack();

    // The back button is a history navigation, not a click, so the capture-phase
    // handler cannot see it. The URL watcher does, and it asks.
    await expect(page.getByTestId('guard-scrim')).toBeVisible();
    // The page is already unmounted by this point -- there is nothing to
    // intercept -- so the guard confirms rather than blocks. That limit is
    // stated in the layout's header and is a property of the platform, not a
    // shortcut.
    expect(new URL(page.url()).pathname).toBe('/');
  });

  test('9. a confirmed dialog is announced as one', async ({ page }) => {
    // `alertdialog`, not `dialog`: this blocks a navigation already in flight,
    // and a screen reader treating it as a passive panel will not interrupt.
    await makeDirty(page);
    await toLibrary(page).click();
    const confirm = page.getByRole('alertdialog');
    await expect(confirm).toBeVisible();
    await expect(confirm).toHaveAttribute('aria-modal', 'true');
  });
});
