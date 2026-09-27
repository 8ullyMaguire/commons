/**
 * CSV import, in a browser. §10.10 / #1296, #431.
 *
 * The parsing decisions are in `tests/csv.test.ts`, where they can be tested at
 * their boundaries. What is left is the four things only a browser can answer,
 * and three of them are about bytes rather than values:
 *
 *   1. a real `<input type="file">` produces a real `File` with real BYTES, and
 *      the encoding is decided from those bytes — a UTF-16 file is a `File` in
 *      the browser and mojibake by the time it reaches a `.text()` call;
 *   2. the paste preview and the file preview are the SAME control, so a bug in
 *      the shared commit path is visible from both;
 *   3. Esc dismisses a file import, exactly as it dismisses a paste — a preview
 *      that only one of two entry points can cancel is a trap;
 *   4. a truncated file offers NO Apply button, not a disabled one.
 */
import { expect, test, type Page } from '@playwright/test';

/** The values the field currently shows, in order. */
async function fieldValues(page: Page): Promise<string[]> {
  return page.getByTestId('multi-values').locator('li').allTextContents();
}

/**
 * Hand a file to the hidden input.
 *
 * `setInputFiles` with a buffer rather than a path: the point of most of these
 * tests is the BYTES, and a fixture on disk cannot be UTF-16-without-a-BOM
 * without being a committed binary blob.
 */
async function chooseFile(page: Page, name: string, bytes: Buffer | string): Promise<void> {
  await page.getByTestId('multi-file-input').setInputFiles({
    name,
    mimeType: 'text/csv',
    buffer: typeof bytes === 'string' ? Buffer.from(bytes, 'utf8') : bytes,
  });
}

test.describe('CSV import into a multi-value field', () => {
  test.beforeEach(async ({ page }) => {
    await page.goto('/tags?tag=existing');
    await expect(page.getByTestId('multi-value')).toBeVisible();
  });

  test('the menu offers a file item as well as a paste item', async ({ page }) => {
    await page.getByTestId('multi-field').click({ button: 'right' });
    await expect(page.getByTestId('multi-paste-item')).toBeVisible();
    await expect(page.getByTestId('multi-file-item')).toBeVisible();
  });

  test('imports one column, previewing before it changes the field', async ({ page }) => {
    await chooseFile(page, 'tags.csv', 'one\ntwo\nthree');

    await expect(page.getByTestId('multi-preview-text')).toHaveText('Will add 3 values.');
    // The claim: the file has NOT been applied yet.
    expect(await fieldValues(page)).toEqual(['existing']);
  });

  test('confirming the preview applies the values', async ({ page }) => {
    await chooseFile(page, 'tags.csv', 'one\ntwo');
    await page.getByTestId('multi-preview-apply').click();

    expect(await fieldValues(page)).toEqual(['existing', 'one', 'two']);
    await expect(page.getByTestId('multi-preview')).toHaveCount(0);
  });

  // The whole reason `importCsv` returns `parsePaste`'s result type: one
  // preview, one Apply, one Esc, for both entry points.
  test('the file preview is the same control as the paste preview', async ({ page }) => {
    await chooseFile(page, 'tags.csv', 'one');
    await expect(page.getByTestId('multi-preview')).toHaveAttribute('data-source', 'file');
    await expect(page.getByTestId('multi-preview-apply')).toHaveText('Add 1');

    await page.getByTestId('multi-preview-cancel').click();

    await page.getByTestId('multi-field').click();
    await page.evaluate(() => {
      const field = document.querySelector('[data-testid="multi-field"]')!;
      const dt = new DataTransfer();
      dt.setData('text/plain', 'two');
      field.dispatchEvent(new ClipboardEvent('paste', { clipboardData: dt, bubbles: true }));
    });
    await expect(page.getByTestId('multi-preview')).toHaveAttribute('data-source', 'paste');
  });

  test('sniffs a semicolon-delimited file', async ({ page }) => {
    await chooseFile(page, 'tags.csv', 'one;two\nthree;four');
    await page.getByTestId('multi-preview-apply').click();
    expect(await fieldValues(page)).toEqual(['existing', 'one', 'three']);
  });

  // One VALUE per line, each padded with the spaces a spreadsheet leaves after
  // its separators. Written as one line per value rather than as three columns
  // on one line, because `importCsv` reads column 0 by default and a
  // three-column row is three values only if the user picks the column -- which
  // is a UI this component does not have yet.
  test('trims the spaces a spreadsheet puts after its separators', async ({ page }) => {
    // The component passes `trimValues: true`, unlike the default. A tag with a
    // leading space cannot be found again, and the space came from the export
    // rather than from the user.
    await chooseFile(page, 'tags.csv', 'one \n two \nthree ');
    await page.getByTestId('multi-preview-apply').click();
    expect(await fieldValues(page)).toEqual(['existing', 'one', 'two', 'three']);
  });

  // A UTF-16 file with no BOM. `File.text()` would decode it as UTF-8 and
  // produce NUL-interleaved mojibake, and the parser could not tell, because the
  // damage would already be done to the bytes.
  test('reads a BOM-less UTF-16 file', async ({ page }) => {
    const bytes = Buffer.alloc('one\ntwo\nthree'.length * 2);
    for (let i = 0; i < 'one\ntwo\nthree'.length; i += 1) {
      bytes[i * 2] = 'one\ntwo\nthree'.charCodeAt(i);
    }
    await chooseFile(page, 'tags.csv', bytes);

    await page.getByTestId('multi-preview-apply').click();
    const values = await fieldValues(page);
    expect(values).toEqual(['existing', 'one', 'two', 'three']);
    for (const v of values) expect(v).not.toContain('\\u0000');
  });

  test('strips a UTF-8 BOM from the first value', async ({ page }) => {
    await chooseFile(page, 'tags.csv', Buffer.concat([
      Buffer.from([0xef, 0xbb, 0xbf]),
      Buffer.from('one\ntwo', 'utf8'),
    ]));
    await page.getByTestId('multi-preview-apply').click();
    // A leading U+FEFF in a tag is not whitespace, so no trim() rescues it.
    expect(await fieldValues(page)).toEqual(['existing', 'one', 'two']);
  });

  test('keeps a newline inside a quoted field as one value', async ({ page }) => {
    await chooseFile(page, 'tags.csv', '"line one\nline two"\nplain');
    await page.getByTestId('multi-preview-apply').click();
    expect(await fieldValues(page)).toEqual(['existing', 'line one\nline two', 'plain']);
  });

  // The truncation state. A greyed-out Apply next to "Add 3" is a control the
  // user cannot use and cannot understand, so there is no button at all.
  test('a truncated file offers no Apply button at all', async ({ page }) => {
    await chooseFile(page, 'tags.csv', 'one\n"never closed\nstill going');

    await expect(page.getByTestId('multi-preview-text')).toContainText('File is truncated');
    await expect(page.getByTestId('multi-preview-apply')).toHaveCount(0);
    // And the field is untouched.
    expect(await fieldValues(page)).toEqual(['existing']);
  });

  // Esc is the documented way out of a preview, and the handler is bound to the
  // field WRAPPER. A wrapper is not focusable, so a file import -- which leaves
  // focus on the hidden file input, outside the wrapper -- never receives the
  // key. The paste tests passed because clicking the field put focus inside.
  //
  // Fixed by listening on the document while a preview is open, and the test is
  // worth keeping for the reason it fails loudly: it is the only one that
  // imports a file without touching the field first.
  test('Esc dismisses a file import, without the field having been clicked', async ({ page }) => {
    await chooseFile(page, 'tags.csv', 'one\ntwo');
    await expect(page.getByTestId('multi-preview')).toBeVisible();

    await page.keyboard.press('Escape');

    await expect(page.getByTestId('multi-preview')).toHaveCount(0);
    expect(await fieldValues(page)).toEqual(['existing']);
  });

  test('a blank line in the file is not a value', async ({ page }) => {
    await chooseFile(page, 'tags.csv', 'one\n\n\ntwo');
    await page.getByTestId('multi-preview-apply').click();
    expect(await fieldValues(page)).toEqual(['existing', 'one', 'two']);
  });

  test('a value already in the field is not added twice', async ({ page }) => {
    await chooseFile(page, 'tags.csv', 'existing\nnew');
    await expect(page.getByTestId('multi-preview-text')).toHaveText(
      'Will add 1 value, 1 value already in the field.',
    );
    await page.getByTestId('multi-preview-apply').click();
    expect(await fieldValues(page)).toEqual(['existing', 'new']);
  });

  test('shows a warning for a file that parsed with a stray quote', async ({ page }) => {
    await chooseFile(page, 'tags.csv', 'he said "hi"');
    await expect(page.getByTestId('multi-file-warnings')).toBeVisible();
    // And the value is still importable, because it was recoverable: a quote
    // in the middle of a field is a character, not a syntax error, and refusing
    // the file over one would be worse than importing it with a note.
    await page.getByTestId('multi-preview-apply').click();
    expect(await fieldValues(page)).toEqual(['existing', 'he said "hi"']);
  });
});
