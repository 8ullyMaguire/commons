/**
 * The multi-value field's right-click paste, in a browser. §10.10 / #7139, §9.4.
 *
 * The parsing decisions are in `tests/paste.test.ts`, where they can be tested
 * at their boundaries. What is left here is the four things only a browser can
 * answer, and every one of them is about an event rather than a value:
 *
 *   1. a right-click opens the menu instead of the browser's own;
 *   2. a real `paste` event reaches the field and produces a preview;
 *   3. the preview does NOT change the field until it is confirmed — a paste
 *      that lands immediately is not undoable by hand in a 400-tag field;
 *   4. Esc dismisses the preview without applying it, which is the whole
 *      reason the preview exists separately from the paste.
 */
import { expect, test, type Page } from '@playwright/test';

/** Open the page with some existing tags. */
async function openTags(page: Page, tags: readonly string[] = ['existing']): Promise<void> {
  const qs = tags.map((t) => `tag=${encodeURIComponent(t)}`).join('&');
  await page.goto(`/tags${qs ? `?${qs}` : ''}`);
  await expect(page.getByTestId('multi-value')).toBeVisible();
}

/** The values the field currently shows, in order. */
async function fieldValues(page: Page): Promise<string[]> {
  return page.getByTestId('multi-values').locator('li').allTextContents();
}

/** The comma-joined state the route renders, which is the applied list. */
async function applied(page: Page): Promise<string> {
  return (await page.getByTestId('tag-state').textContent()) ?? '';
}

test.describe('right-click paste into a multi-value field', () => {
  test('a right-click opens the field menu and suppresses the browser one', async ({ page }) => {
    await openTags(page);
    const field = page.getByTestId('multi-field');

    // The claim is `defaultPrevented`, checked by dispatching the event and
    // reading the flag the browser will act on. Asserting the menu merely
    // appears would pass even if the browser's own menu also appeared, which
    // is the failure: two menus for one action.
    const prevented = await field.evaluate((el) => {
      const ev = new MouseEvent('contextmenu', { bubbles: true, cancelable: true });
      el.dispatchEvent(ev);
      return ev.defaultPrevented;
    });

    expect(prevented).toBe(true);
    await expect(page.getByTestId('multi-menu')).toBeVisible();
    await expect(page.getByTestId('multi-paste-item')).toBeVisible();
  });

  test('a paste previews without changing the field', async ({ page }) => {
    await openTags(page);

    // The real event, with real clipboard data, because a synthetic event with
    // an empty `clipboardData` is indistinguishable from an empty clipboard.
    await page.getByTestId('multi-field').click();
    await page.evaluate(() => {
      const field = document.querySelector('[data-testid="multi-field"]')!;
      const dt = new DataTransfer();
      dt.setData('text/plain', 'one\ntwo\nthree');
      field.dispatchEvent(new ClipboardEvent('paste', { clipboardData: dt, bubbles: true }));
    });

    // The preview states what will happen...
    await expect(page.getByTestId('multi-preview-text')).toHaveText('Will add 3 values.');
    // ...and the field has NOT changed. This is the claim: a paste that lands
    // immediately is not undoable by hand once there are 400 of them.
    expect(await fieldValues(page)).toEqual(['existing']);
    expect(await applied(page)).toBe('existing');
  });

  test('confirming the preview adds the values', async ({ page }) => {
    await openTags(page);
    await page.getByTestId('multi-field').click();
    await page.evaluate(() => {
      const field = document.querySelector('[data-testid="multi-field"]')!;
      const dt = new DataTransfer();
      dt.setData('text/plain', 'one\ntwo');
      field.dispatchEvent(new ClipboardEvent('paste', { clipboardData: dt, bubbles: true }));
    });

    await page.getByTestId('multi-preview-apply').click();

    expect(await fieldValues(page)).toEqual(['existing', 'one', 'two']);
    expect(await applied(page)).toBe('existing|one|two');
    // And the preview is gone, so a second click cannot add them again.
    await expect(page.getByTestId('multi-preview')).toHaveCount(0);
  });

  test('the preview names what was already in the field', async ({ page }) => {
    await openTags(page, ['one', 'two']);
    await page.getByTestId('multi-field').click();
    await page.evaluate(() => {
      const field = document.querySelector('[data-testid="multi-field"]')!;
      const dt = new DataTransfer();
      dt.setData('text/plain', 'one\nthree');
      field.dispatchEvent(new ClipboardEvent('paste', { clipboardData: dt, bubbles: true }));
    });

    // "Will add 1 value" for a two-row paste reads as a mistake unless the
    // other row is accounted for.
    await expect(page.getByTestId('multi-preview-text')).toHaveText(
      'Will add 1 value, 1 value already in the field.'
    );
  });

  test('a paste of only duplicates applies nothing and says so', async ({ page }) => {
    await openTags(page, ['one', 'two']);
    await page.getByTestId('multi-field').click();
    await page.evaluate(() => {
      const field = document.querySelector('[data-testid="multi-field"]')!;
      const dt = new DataTransfer();
      dt.setData('text/plain', 'one\ntwo');
      field.dispatchEvent(new ClipboardEvent('paste', { clipboardData: dt, bubbles: true }));
    });

    await expect(page.getByTestId('multi-preview-text')).toHaveText(
      'Nothing new — all 2 values already in the field.'
    );
    // The confirm button is DEAD rather than hidden: a button that says
    // "Nothing to add" and does nothing when pressed is a trap.
    await expect(page.getByTestId('multi-preview-apply')).toBeDisabled();
  });

  test('Esc dismisses the preview without applying it', async ({ page }) => {
    await openTags(page);
    await page.getByTestId('multi-field').click();
    await page.evaluate(() => {
      const field = document.querySelector('[data-testid="multi-field"]')!;
      const dt = new DataTransfer();
      dt.setData('text/plain', 'one\ntwo');
      field.dispatchEvent(new ClipboardEvent('paste', { clipboardData: dt, bubbles: true }));
    });
    await expect(page.getByTestId('multi-preview')).toBeVisible();

    await page.keyboard.press('Escape');

    await expect(page.getByTestId('multi-preview')).toHaveCount(0);
    expect(await fieldValues(page)).toEqual(['existing']);
  });

  test('a quoted value with a comma is one tag, not two', async ({ page }) => {
    // The parser's design decision, proved end to end: the browser test would
    // pass with a naive comma split if the field passed the text through
    // unchanged, and fail only if the parse actually happened.
    await openTags(page, []);
    await page.getByTestId('multi-field').click();
    await page.evaluate(() => {
      const field = document.querySelector('[data-testid="multi-field"]')!;
      const dt = new DataTransfer();
      dt.setData('text/plain', '"Smith, John", Jr');
      field.dispatchEvent(new ClipboardEvent('paste', { clipboardData: dt, bubbles: true }));
    });

    await page.getByTestId('multi-preview-apply').click();
    expect(await fieldValues(page)).toEqual(['Smith, John', 'Jr']);
  });

  test('a CRLF paste produces no carriage returns', async ({ page }) => {
    // Invisible in a UI and it breaks every later comparison against the stored
    // value, so it is worth a test that says so explicitly.
    await openTags(page, []);
    await page.getByTestId('multi-field').click();
    await page.evaluate(() => {
      const field = document.querySelector('[data-testid="multi-field"]')!;
      const dt = new DataTransfer();
      dt.setData('text/plain', 'one\r\ntwo\r\nthree');
      field.dispatchEvent(new ClipboardEvent('paste', { clipboardData: dt, bubbles: true }));
    });

    await page.getByTestId('multi-preview-apply').click();
    const values = await fieldValues(page);
    expect(values).toEqual(['one', 'two', 'three']);
    for (const v of values) expect(v).not.toContain('\r');
  });
});
