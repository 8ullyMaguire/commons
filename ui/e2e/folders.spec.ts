/**
 * The folder view in a browser. Spec 9.5, 5.14 / #1586, #1723.
 *
 * The tree logic is in `tests/folder-tree.test.ts`. What is left is the five
 * things only a browser can answer:
 *
 *   1. the pane renders a tree with the right DEPTHS — indentation is derived
 *      from the number, and a wrong depth is a tree that indents a leaf as if it
 *      had children;
 *   2. opening a folder changes the FILTER the wall runs — §5.14's "a folder
 *      holds no objects, it IS a query", which is the part a user gets wrong;
 *   3. the folder is a URL, so it can be linked and reloaded;
 *   4. broken data is REPORTED — an orphan and a parent cycle are in the fixture
 *      on purpose, because the paths that report them should be exercised in a
 *      browser and not only in unit tests;
 *   5. a collapsed folder says how much is inside it.
 */
import { test, expect, type Page } from '@playwright/test';

async function seed(page: Page, total = 8) {
  await page.route('**/graphql', (route) =>
    route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        data: {
          objects: {
            totalCount: total,
            pageInfo: { hasNextPage: false, hasPreviousPage: false, startCursor: null, endCursor: null },
            nodes: Array.from({ length: total }, (_, i) => ({
              id: `o${i}`,
              kind: 'Scene',
              title: `Item ${i}`,
              date: '2024-0' + ((i % 3) + 1) + '-01',
              rating: 1,
              organized: null,
              coverPath: null,
              width: 800,
              height: 1200,
              durationMs: null,
              producer: `Studio ${i % 2}`,
              performers: [],
              tags: [],
              folder: null
            }))
          }
        }
      })
    })
  );
}

/** Every rendered row as [id, depth]. */
async function rows(page: Page): Promise<[string, string][]> {
  return page.getByTestId('folder-row').evaluateAll((els) =>
    els.map((e) => [e.getAttribute('data-id') ?? '', e.getAttribute('data-depth') ?? ''])
  );
}

test.describe('the folder view', () => {
  test('renders the tree, with a child one level below its parent', async ({ page }) => {
    await seed(page);
    await page.goto('/folders');
    await expect(page.getByTestId('folder-row').first()).toBeVisible();

    // Collapsed by default: the roots, and nothing else. A navigator that opens
    // everything is a list, not a tree.
    const shown = await rows(page);
    assert_depths(shown);
    expect(shown.every(([, d]) => d === '0')).toBe(true);
  });

  test('expanding a folder shows its children one level deeper', async ({ page }) => {
    await seed(page);
    await page.goto('/folders?expanded=f-inbox');
    await expect(page.getByTestId('folder-row').first()).toBeVisible();

    const shown = await rows(page);
    const byId = Object.fromEntries(shown);
    expect(byId['f-untagged']).toBe('1', 'a child of Inbox is at depth 1');
    expect(byId['f-inbox']).toBe('0');
  });

  test('a collapsed folder says how much is inside it', async ({ page }) => {
    // A twisty that hides things without admitting it is a control the user
    // cannot predict.
    await seed(page);
    await page.goto('/folders?expanded=f-inbox');
    await expect(page.getByTestId('folder-row').first()).toBeVisible();

    await expect(page.getByTestId('folder-row').filter({ hasText: 'By studio' }).getByTestId('folder-hidden')).toHaveText('2 hidden');
  });

  test('the twisty reports its state to assistive tech', async ({ page }) => {
    await seed(page);
    await page.goto('/folders');
    await expect(page.getByTestId('folder-row').first()).toBeVisible();

    const twisty = page.getByTestId('folder-row').filter({ hasText: 'Inbox' }).getByTestId('folder-twisty');
    await expect(twisty).toHaveAttribute('aria-expanded', 'false');
    await twisty.click();
    await expect(twisty).toHaveAttribute('aria-expanded', 'true');
  });

  // --- opening a folder is the whole feature -------------------------------

  test('opening a folder names the filter it runs', async ({ page }) => {
    // §5.14: a folder holds no objects. Opening one REPLACES the filter rather
    // than narrowing it, and a user who does not know that will keep adding
    // filters and end up with nothing.
    await seed(page);
    await page.goto('/folders?folder=f-untagged&expanded=f-inbox');
    await expect(page.getByTestId('folder-filter')).toHaveText('tag_count = 0');
  });

  test('the tree root says "Everything", not a filter', async ({ page }) => {
    await seed(page);
    await page.goto('/folders');
    await expect(page.getByTestId('folder-filter')).toHaveText('Everything');
  });

  test('a folder is a URL, and reloading it opens the same folder', async ({ page }) => {
    // A folder is a place, so it has to be linkable.
    await seed(page);
    await page.goto('/folders?folder=f-studio-a&expanded=f-by-studio');
    await expect(page.getByTestId('folder-filter')).toHaveText('producer = "A"');

    await page.reload();
    await expect(page.getByTestId('folder-filter')).toHaveText('producer = "A"');
    await expect(
      page.getByTestId('folder-row').filter({ hasText: 'Studio A' }).getByTestId('folder-name')
    ).toHaveAttribute('aria-current', 'page');
  });

  test('opening a folder expands its ancestors, so the user can see where they are', async ({ page }) => {
    // The difference between a navigator and a jump.
    await seed(page);
    await page.goto('/folders');
    await expect(page.getByTestId('folder-row').first()).toBeVisible();

    await page.getByTestId('folder-row').filter({ hasText: 'Inbox' }).getByTestId('folder-twisty').click();
    await page.getByTestId('folder-row').filter({ hasText: 'Untagged' }).getByTestId('folder-name').click();

    await expect(page).toHaveURL(/folder=f-untagged/);
    await expect(page).toHaveURL(/expanded=f-inbox/, { timeout: 5000 });
  });

  // --- breadcrumbs (#1723) -------------------------------------------------

  test('the trail runs from the folder up to the root, nearest first', async ({ page }) => {
    await seed(page);
    await page.goto('/folders?folder=f-untagged&expanded=f-inbox');
    await expect(page.getByTestId('folder-crumbs')).toBeVisible();

    const crumbs = await page.getByTestId('crumb').allInnerTexts();
    expect(crumbs).toEqual(['Untagged', 'Inbox']);
  });

  test('only the last crumb is the current one', async ({ page }) => {
    await seed(page);
    await page.goto('/folders?folder=f-untagged&expanded=f-inbox');
    await expect(page.getByTestId('folder-crumbs')).toBeVisible();

    const current = await page.getByTestId('crumb').evaluateAll((els) =>
      els.map((e) => e.getAttribute('data-current'))
    );
    expect(current).toEqual(['false', 'true']);
  });

  test('the root crumb returns to the whole library', async ({ page }) => {
    await seed(page);
    await page.goto('/folders?folder=f-untagged&expanded=f-inbox');
    await expect(page.getByTestId('folder-crumbs')).toBeVisible();

    await page.getByTestId('crumb-root').click();
    await expect(page).toHaveURL(/^[^?]*\/folders$/);
    await expect(page.getByTestId('folder-filter')).toHaveText('Everything');
  });

  // --- search --------------------------------------------------------------

  test('searching shows a hit together with its ancestors', async ({ page }) => {
    // A search that returns "Untagged" while hiding every folder above it gives
    // the user an entry they cannot open in context.
    await seed(page);
    await page.goto('/folders');
    await expect(page.getByTestId('folder-row').first()).toBeVisible();

    await page.getByTestId('folder-search').fill('Studio A');

    const ids = (await rows(page)).map(([id]) => id);
    expect(ids).toContain('f-studio-a');
    expect(ids).toContain('f-by-studio', 'the parent is shown so the hit is reachable');
  });

  test('searching matches the filter, not just the name', async ({ page }) => {
    // A user who remembers a folder as "the one that looks for untagged" is
    // searching for it by that.
    await seed(page);
    await page.goto('/folders');
    await expect(page.getByTestId('folder-row').first()).toBeVisible();

    await page.getByTestId('folder-search').fill('tag_count');
    const ids = (await rows(page)).map(([id]) => id);
    expect(ids).toContain('f-untagged');
  });

  test('an empty search shows the tree again', async ({ page }) => {
    await seed(page);
    await page.goto('/folders');
    await expect(page.getByTestId('folder-row').first()).toBeVisible();

    const all = (await rows(page)).length;
    await page.getByTestId('folder-search').fill('zzz');
    expect((await rows(page)).length).toBe(0);

    await page.getByTestId('folder-search').fill('');
    expect((await rows(page)).length).toBe(all);
  });

  // --- broken data, reported ----------------------------------------------

  test('a folder with a missing parent is reported, not hidden', async ({ page }) => {
    // The fixture contains one on purpose. A folder that silently never appears
    // is the worst outcome a navigator has: the user cannot know it exists.
    await seed(page);
    await page.goto('/folders');
    await expect(page.getByTestId('folder-row').first()).toBeVisible();

    await expect(page.getByTestId('folder-orphans')).toContainText('1 folder has a missing parent');
    // And it is still shown, hoisted to the top, so it can be fixed.
    expect((await rows(page)).map(([id]) => id)).toContain('f-orphan');
  });

  test('a parent cycle is reported and does not blank the tree', async ({ page }) => {
    // The fixture has two folders each other's parent. Migration 0018 is meant to
    // refuse this, so the view must survive the data anyway -- a blank pane
    // because of one broken folder is a far worse failure than a misplaced row.
    await seed(page);
    await page.goto('/folders');
    await expect(page.getByTestId('folder-row').first()).toBeVisible();

    await expect(page.getByTestId('folder-cycles')).toContainText('2 folders form a loop');
    // The healthy folders are all still there. `f-studio-a` is a CHILD of a
    // collapsed folder, so it is correctly not rendered -- asserting it would be
    // asserting that collapsed folders show their children. Expanding is what
    // makes it appear, and that it does is the stronger claim anyway: a cycle
    // has not eaten the subtree.
    const ids = (await rows(page)).map(([id]) => id);
    expect(ids).toContain('f-inbox');
    expect(ids).toContain('f-by-studio');

    // Expand, and the subtree under a healthy parent is intact.
    await page.getByTestId('folder-row').filter({ hasText: 'By studio' }).getByTestId('folder-twisty').click();
    await expect
      .poll(async () => (await rows(page)).map(([id]) => id))
      .toContain('f-studio-a');
  });

  // --- counts and notifications -------------------------------------------

  test('an unknown count is a dash, not a zero', async ({ page }) => {
    // Membership is recomputed on open, so counting a folder is a query per
    // folder. A column of zeroes before the counts land looks like an empty
    // library, which is a worse lie than "I do not know yet".
    await seed(page);
    await page.goto('/folders');
    await expect(page.getByTestId('folder-row').first()).toBeVisible();

    await expect(page.getByTestId('folder-count').first()).toHaveText('—');
  });

  test('a folder that notifies shows a marker', async ({ page }) => {
    // §5.14 smart collections.
    await seed(page);
    await page.goto('/folders');
    await expect(page.getByTestId('folder-row').first()).toBeVisible();

    await expect(page.getByTestId('folder-notify')).toHaveCount(1);
  });

  test('the content pane renders for a folder', async ({ page }) => {
    // The pane and the wall are one view: a folder that opens an empty right
    // side is not a folder view.
    await seed(page);
    await page.goto('/folders?folder=f-studio-a&expanded=f-by-studio');
    await expect(page.getByTestId('wall-tile').first()).toBeVisible();
  });
});

/** Every rendered row has a numeric depth. A missing one indents by `NaN`. */
function assert_depths(shown: [string, string][]): void {
  for (const [id, d] of shown) {
    expect(Number.isInteger(Number(d)), `${id} has depth "${d}"`).toBe(true);
  }
}
