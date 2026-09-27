/**
 * The per-field ignore-list settings, in a browser. §10.10 / #2318, #2399.
 *
 * The decisions are in `tests/ignore-list.test.ts`, where they can be tested at
 * their boundaries. What is left is the four things only a browser can answer:
 *
 *   1. ticking a box produces the scope the summary claims — the summary and
 *      the state are computed from the same object, so a mismatch means one of
 *      them is lying;
 *   2. the two lists are INDEPENDENT — #2318 and #2399 are different actions and
 *      a user must be able to set one without touching the other;
 *   3. the inverted mode reads as an allow-list, in the checkbox states and in
 *      the words, not just in the query string;
 *   4. the scope round-trips through the URL, because a configured tagger is
 *      something a user links to.
 */
import { expect, test, type Page } from '@playwright/test';

/** Open the settings for one subject. */
async function open(page: Page, query = ''): Promise<void> {
  await page.goto(`/tagger-settings${query}`);
  await expect(page.getByTestId('ignore-settings')).toBeVisible();
}

/** The rendered state: which list, and which fields. */
async function state(page: Page): Promise<string> {
  return ((await page.getByTestId('ignore-state').textContent()) ?? '').replace(/\s+/g, ' ').trim();
}

test.describe('per-field ignore lists', () => {
  test('shows both lists, each with its own wording', async ({ page }) => {
    await open(page);
    // Two different actions, so two different headings. Conflating them is the
    // failure: a user who excludes a field from the SEARCH would silently stop
    // getting its value.
    await expect(page.getByTestId('ignore-list-apply')).toContainText('A scrape may not write');
    await expect(page.getByTestId('ignore-list-search')).toContainText('A search may not look at');
  });

  test('nothing is ignored by default', async ({ page }) => {
    await open(page);
    await expect(page.getByTestId('ignore-summary-apply')).toHaveText('No fields are ignored.');
    await expect(page.getByTestId('ignore-apply-title')).not.toBeChecked();
  });

  test('ticking a box ignores that field and says so', async ({ page }) => {
    await open(page);

    await page.getByTestId('ignore-apply-title').check();

    await expect(page.getByTestId('ignore-apply-title')).toBeChecked();
    await expect(page.getByTestId('ignore-summary-apply')).toHaveText('title is ignored.');
    expect(await state(page)).toContain('apply=list:title');
  });

  test('the two lists are independent', async ({ page }) => {
    await open(page);

    await page.getByTestId('ignore-apply-title').check();

    // The claim: the search list is untouched.
    await expect(page.getByTestId('ignore-search-title')).not.toBeChecked();
    await expect(page.getByTestId('ignore-summary-search')).toHaveText('No fields are ignored.');
    // The rendered state names the mode and then the list, and the space after
    // `search=` is real -- the template puts the mode on one side of the `=` and
    // the list on the other.
    expect(await state(page)).toContain('search=');
    expect(await state(page)).toMatch(/search=\s*list:\s*$/);
  });

  test('the summary names several fields, not a count', async ({ page }) => {
    await open(page);
    await page.getByTestId('ignore-apply-title').check();
    await page.getByTestId('ignore-apply-date').check();

    // A row that says "2 fields ignored" tells a user nothing they can check.
    await expect(page.getByTestId('ignore-summary-apply')).toHaveText('date and title are ignored.');
  });

  // The inversion, in the words and in the checkbox states. A checkbox labelled
  // "ignore all" beside a list of ticked fields is ambiguous about which way
  // round it goes, so the label changes with the state.
  test('the inverted mode reads as an allow-list', async ({ page }) => {
    await open(page);
    await page.getByTestId('ignore-apply-title').check();

    await page.getByTestId('ignore-invert-apply').check();

    await expect(page.getByTestId('ignore-list-apply')).toContainText('Only the ticked fields are used');
    // `title` was on the ignore list and is now on the KEEP list, so it is
    // ticked — and the tick means "used", not "ignored". A checkbox that
    // unticked itself the moment the inversion went on would leave the user
    // looking at an allow-list in which nothing is allowed.
    await expect(page.getByTestId('ignore-apply-title')).toBeChecked();
    await expect(page.getByTestId('ignore-apply-date')).not.toBeChecked();
    await expect(page.getByTestId('ignore-summary-apply')).toHaveText(
      'Every field is ignored except title.',
    );
  });

  test('turning the inversion off means ignore nothing', async ({ page }) => {
    // The other direction, and the one that is easy to get wrong: a keep-list
    // of zero entries would silently mean "write nothing", which is a different
    // action from the one the user is performing.
    await open(page, '?ignore=title&ignore-invert=1');

    await page.getByTestId('ignore-invert-apply').uncheck();

    await expect(page.getByTestId('ignore-summary-apply')).toHaveText('No fields are ignored.');
    expect(await state(page)).toContain('apply=list:');
  });

  test('an unrecognised field is reported and still ignored', async ({ page }) => {
    // A scraper with a field this build has never seen is a normal thing, and
    // the entry is still doing its job. Removing it would make the setting
    // quietly wrong.
    await open(page, '?ignore=from_the_year_3000');

    await expect(page.getByTestId('ignore-unknown-apply')).toContainText(
      'from_the_year_3000 is not a field this build knows',
    );
    await expect(page.getByTestId('ignore-unknown-apply')).toContainText('It is still ignored.');
    await expect(page.getByTestId('ignore-summary-apply')).toHaveText(
      'from_the_year_3000 is ignored.',
    );
  });

  test('a field can be added by name, for a scraper ahead of the vocabulary', async ({ page }) => {
    // Without this the feature is unusable against a scraper this build has not
    // caught up with, which is the case the open field design exists to allow.
    await open(page);
    const field = page.getByTestId('ignore-custom');
    await field.fill('studio_alias');
    await field.press('Enter');

    await expect(page.getByTestId('ignore-unknown-apply')).toBeVisible();
    expect(await state(page)).toContain('apply=list:studio_alias');
    // And the input is cleared, so a second entry does not append to the first.
    await expect(field).toHaveValue('');
  });

  test('an invalid name is not accepted', async ({ page }) => {
    await open(page);
    const field = page.getByTestId('ignore-custom');
    await field.fill('performer name');
    await field.press('Enter');

    expect(await state(page)).toContain('apply=list:');
    await expect(field).toHaveValue('performer name');
  });

  test('the scope round-trips through the URL', async ({ page }) => {
    // A configured tagger is something a user links to, and the two modes have
    // to stay distinguishable in a URL — the same distinction `scopeToQuery`
    // makes on the way out.
    await open(page);
    await page.getByTestId('ignore-apply-title').check();
    await page.getByTestId('ignore-search-description').check();

    const url = new URL(page.url());
    expect(url.searchParams.getAll('ignore')).toEqual(['title']);
    expect(url.searchParams.getAll('search')).toEqual(['description']);
    expect(url.searchParams.get('ignore-invert')).toBeNull();

    // A RELOAD, not a fresh goto: the state has to survive the browser
    // re-reading the URL, which is the whole point of putting it there. Ticking
    // a box and reading the state back would pass with no URL at all.
    await page.reload();
    await expect(page.getByTestId('ignore-apply-title')).toBeChecked();
    await expect(page.getByTestId('ignore-search-description')).toBeChecked();
  });

  test('an inverted scope round-trips as inverted, not as an empty list', async ({ page }) => {
    // Without the `invert=1` marker, an inverted scope with one field and a
    // plain scope with one field serialise identically and reload as the wrong
    // one.
    await open(page, '?ignore=title&ignore-invert=1');

    await expect(page.getByTestId('ignore-invert-apply')).toBeChecked();
    await expect(page.getByTestId('ignore-apply-title')).toBeChecked();
    await expect(page.getByTestId('ignore-summary-apply')).toHaveText(
      'Every field is ignored except title.',
    );
  });

  test('the object subject does not offer the derived producer columns', async ({ page }) => {
    // §7.11 says career_start is computed and never authored. A checkbox for it
    // would be a control that does nothing.
    await open(page, '?subject=object');
    await expect(page.getByTestId('ignore-apply-title')).toBeVisible();
    await expect(page.getByTestId('ignore-apply-career_start')).toHaveCount(0);
  });

  // The pair a flat exclusion list cannot express: the same NAME, opposite
  // verdicts, because `Object.kind` is the §5.1 discriminator and `Producer.kind`
  // is metadata a tagger should propose.
  test('"kind" is offered for a producer and not for an object', async ({ page }) => {
    await open(page, '?subject=object');
    await expect(page.getByTestId('ignore-apply-kind')).toHaveCount(0);

    await open(page, '?subject=producer');
    await expect(page.getByTestId('ignore-apply-kind')).toBeVisible();
  });

  test('the query each list would use is shown', async ({ page }) => {
    await open(page);
    await page.getByTestId('ignore-apply-title').check();

    // The scope is visible as the thing it will be applied as, not only as a set
    // of ticks — the two lists serialise differently, and a user debugging a
    // tagger needs to see which.
    await expect(page.getByTestId('ignore-query-apply')).toHaveText('ignore=title');
    await expect(page.getByTestId('ignore-query-search')).toHaveText('(no filter)');
  });
});
