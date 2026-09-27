/**
 * The two share pages, in a browser.
 *
 * T-P5-007 part 2B, spec §9.5 (#5612).
 *
 * # What needs a browser and what does not
 *
 * `tests/share.test.ts` already proves the client turns a denial into a
 * sentence and refuses to turn an unknown reason into anything else. What only a
 * real document can show is the thing that actually matters to a recipient: that
 * a dead link says *why* it is dead, in words, with no stack trace and no blank
 * page — and that an expired link does not read as "not found".
 *
 * The routes are stubbed at the network boundary rather than by mocking the
 * client module, because the thing under test is what a person sees after a
 * fetch, and a mocked module would make the fetch part of the test imaginary.
 */

import { expect, test, type Page, type Route } from '@playwright/test';

const TOKEN = 'shr_abc.defghijklmnopqrstuvwxyz0123456789ABCDE';

/** Answer `/api/s/:token` with one wire body. */
async function resolveAs(page: Page, status: number, body: unknown) {
  await page.route(/\/api\/s\/[^/]+$/, async (route: Route) =>
    route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) })
  );
}

const opened = {
  target_kind: 'object',
  target_id: 'obj-1',
  can_download: false,
  needs_password: false
};

test.describe('the recipient page', () => {
  test('a live link names what was shared', async ({ page }) => {
    await resolveAs(page, 200, opened);
    await page.goto(`/s/${TOKEN}`);
    await expect(page.getByTestId('share-opened')).toContainText('file');
    await expect(page.getByTestId('share-target')).toHaveAttribute('href', /obj-1/);
  });

  test('an expired link says expired, and does not say "not found"', async ({ page }) => {
    // The assertion with the most weight behind it. "Not found" sends the
    // recipient to their sender saying the link is broken, when the only true
    // fact is that it is old.
    await resolveAs(page, 403, { error: 'expired' });
    await page.goto(`/s/${TOKEN}`);
    const refusal = page.getByTestId('share-refused');
    await expect(refusal).toBeVisible();
    await expect(refusal).toContainText(/expired/i);
    await expect(refusal).not.toContainText(/not found/i);
  });

  test('a revoked link says somebody turned it off', async ({ page }) => {
    // Not "expired": somebody deliberately disabled this, and telling the
    // recipient otherwise sends them to argue with the wrong person.
    await resolveAs(page, 403, { error: 'revoked' });
    await page.goto(`/s/${TOKEN}`);
    const refusal = page.getByTestId('share-refused');
    await expect(refusal).toContainText(/turned off|disabled|no longer/i);
    await expect(refusal).not.toContainText(/expired/i);
  });

  test('a denial is announced, not only shown', async ({ page }) => {
    // On a refused link this page IS the message, so a message that is only
    // visible is a message a screen-reader user does not get.
    await resolveAs(page, 403, { error: 'expired' });
    await page.goto(`/s/${TOKEN}`);
    await expect(page.getByTestId('share-refused')).toHaveAttribute('role', 'alert');
  });

  test('a reason the client does not know still refuses, and never opens', async ({ page }) => {
    // The safety property. A server that grew a new denial reason must not make
    // an older client show a success.
    await resolveAs(page, 403, { error: 'some_new_reason' });
    await page.goto(`/s/${TOKEN}`);
    await expect(page.getByTestId('share-refused')).toBeVisible();
    await expect(page.getByTestId('share-opened')).toHaveCount(0);
  });

  test('a download button appears only when the server said so', async ({ page }) => {
    // A download control on a `view` link produces a 403 the recipient cannot
    // explain, which reads as "this link is broken".
    await resolveAs(page, 200, { ...opened, can_download: false });
    await page.goto(`/s/${TOKEN}`);
    await expect(page.getByTestId('share-opened')).toBeVisible();
    await expect(page.getByTestId('share-download')).toHaveCount(0);

    await resolveAs(page, 200, { ...opened, can_download: true });
    await page.goto(`/s/other-token-value-here`);
    await expect(page.getByTestId('share-download')).toBeVisible();
  });

  test('a 500 is not shown as a dead link', async ({ page }) => {
    // A 500 says nothing about the link. Rendering it as "this link has
    // expired" sends the recipient to their sender when the problem is here.
    await page.route(/\/api\/s\/[^/]+$/, (route: Route) =>
      route.fulfill({ status: 500, contentType: 'text/html', body: '<html>gateway</html>' })
    );
    await page.goto(`/s/${TOKEN}`);
    await expect(page.getByTestId('share-transport-error')).toBeVisible();
    await expect(page.getByTestId('share-refused')).toHaveCount(0);
  });

  test('a loading state exists and is not a refusal', async ({ page }) => {
    // A page that renders nothing in flight is indistinguishable from one that
    // rendered nothing because the link is dead.
    let release: (() => void) | null = null;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    await page.route(/\/api\/s\/[^/]+$/, async (route: Route) => {
      await gate;
      await route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(opened) });
    });
    await page.goto(`/s/${TOKEN}`);
    await expect(page.getByTestId('share-loading')).toBeVisible();
    await expect(page.getByTestId('share-refused')).toHaveCount(0);
    release?.();
    await expect(page.getByTestId('share-opened')).toBeVisible();
  });

  test('a smart collection reads as a collection, not a file', async ({ page }) => {
    // "One item or one smart collection" (§9.5). Calling a collection a file
    // makes the page a small lie about what was shared.
    await resolveAs(page, 200, { ...opened, target_kind: 'smart_collection', target_id: 'sc-1' });
    await page.goto(`/s/${TOKEN}`);
    await expect(page.getByTestId('share-opened')).toContainText(/collection/i);
  });

  test('two different tokens show two different answers', async ({ page }) => {
    // `/s/[token]` is one route with a parameter, so a fetch that runs once on
    // mount shows the first link's answer for every link after it.
    await page.route(/\/api\/s\/[^/]+$/, async (route: Route) => {
      const token = route.request().url().split('/').pop() ?? '';
      if (token === 'good') {
        return route.fulfill({
          status: 200,
          contentType: 'application/json',
          body: JSON.stringify({ ...opened, target_id: 'from-the-first-link' })
        });
      }
      return route.fulfill({
        status: 403,
        contentType: 'application/json',
        body: JSON.stringify({ error: 'expired' })
      });
    });
    await page.goto('/s/good');
    await expect(page.getByTestId('share-target')).toHaveAttribute(
      'href',
      /from-the-first-link/
    );

    await page.goto('/s/stale');
    await expect(page.getByTestId('share-refused')).toBeVisible();
    await expect(page.getByTestId('share-target')).toHaveCount(0);
  });
});

test.describe('the owner page', () => {
  const one = {
    id: 'shr_1',
    target_kind: 'object',
    target_id: 'obj-1',
    scope: 'view',
    expires_at: '2030-01-01T00:00:00.000Z',
    revoked_at: null,
    created_at: '2026-01-01T00:00:00.000Z',
    access_count: 3,
    last_accessed_at: '2026-01-02T00:00:00.000Z',
    state: 'live'
  };

  test('the list shows a link and its state, with no copy button', async ({ page }) => {
    // The load-bearing assertion on this page. The server keeps only a hash, so
    // a "copy link" control here would be a button that cannot work, and a
    // user would learn that by clicking it.
    await page.route(/\/api\/share/, async (route: Route) => {
      if (route.request().method() === 'GET') {
        return route.fulfill({
          status: 200,
          contentType: 'application/json',
          body: JSON.stringify([one])
        });
      }
      return route.fulfill({ status: 201, contentType: 'application/json', body: '{}' });
    });
    await page.goto('/s');
    const row = page.getByTestId('share-row');
    await expect(row).toHaveCount(1);
    await expect(row).toContainText('obj-1');
    await expect(page.getByTestId('share-state')).toHaveText('Active');
    await expect(page.getByTestId('share-uses')).toHaveText('3');

    // Scoped to the LIST, not the whole document. `page.content()` returns the
    // entire rendered app, and the owner page legitimately contains the
    // one-time URL field elsewhere -- so a document-wide check fails on the
    // feature that is supposed to exist. What matters is that the list itself
    // offers no way to read a link it cannot re-read.
    const list = page.getByTestId('share-list');
    const listHtml = await list.innerHTML();
    expect(listHtml, 'the list must offer no way to copy a link').not.toMatch(
      /share-created-url|clipboard|copy/i
    );
    // And no button in the row but the one that turns it off.
    await expect(row.getByRole('button')).toHaveCount(1);
  });

  test('a dead link is listed with a word that is not "Active"', async ({ page }) => {
    await page.route(/\/api\/share/, async (route: Route) => {
      if (route.request().method() === 'GET') {
        return route.fulfill({
          status: 200,
          contentType: 'application/json',
          body: JSON.stringify([{ ...one, id: 'shr_2', state: 'revoked' }])
        });
      }
      return route.fulfill({ status: 201, contentType: 'application/json', body: '{}' });
    });
    await page.goto('/s');
    // "Where is the link I sent on Tuesday" is a question about a dead link.
    await expect(page.getByTestId('share-state')).toHaveText('Turned off');
  });

  test('an empty list says so rather than showing a blank table', async ({ page }) => {
    await page.route(/\/api\/share/, (route: Route) =>
      route.fulfill({ status: 200, contentType: 'application/json', body: '[]' })
    );
    await page.goto('/s');
    await expect(page.getByTestId('share-empty')).toBeVisible();
  });

  test('a created link is shown once and says it cannot be shown again', async ({ page }) => {
    await page.route(/\/api\/share/, async (route: Route) => {
      if (route.request().method() === 'GET') {
        return route.fulfill({ status: 200, contentType: 'application/json', body: '[]' });
      }
      return route.fulfill({
        status: 201,
        contentType: 'application/json',
        body: JSON.stringify({
          id: 'shr_9',
          url: 'http://127.0.0.1:4173/s/shr_9.aaaabbbbcccc',
          expires_at: '2030-01-01T00:00:00.000Z'
        })
      });
    });
    await page.goto('/s');
    await page.getByTestId('share-target').fill('obj-7');
    await page.getByTestId('share-submit').click();

    const shown = page.getByTestId('share-created-url');
    await expect(shown).toHaveValue(/shr_9/);
    // The sentence is the design stated to the user: a link you cannot re-read
    // cannot leak from your screen, and the price is that a lost link is
    // reissued rather than recovered.
    await expect(page.getByTestId('share-created')).toContainText(/only time/i);

    // And it goes away when dismissed, so it is not sitting in the page for a
    // shoulder-surfer or a screenshot to pick up later.
    await page.getByTestId('share-created-dismiss').click();
    await expect(page.getByTestId('share-created')).toHaveCount(0);
  });

  test('the created link is not put in the URL', async ({ page }) => {
    // A token in the address bar is a token in the browser history, in a
    // bookmark, and in anything that reads the address bar. The hashing in
    // migration 0022 is undone by a history entry.
    await page.route(/\/api\/share/, async (route: Route) => {
      if (route.request().method() === 'GET') {
        return route.fulfill({ status: 200, contentType: 'application/json', body: '[]' });
      }
      return route.fulfill({
        status: 201,
        contentType: 'application/json',
        body: JSON.stringify({
          id: 'shr_9',
          url: 'http://127.0.0.1:4173/s/shr_9.aaaabbbbcccc',
          expires_at: '2030-01-01T00:00:00.000Z'
        })
      });
    });
    await page.goto('/s');
    await page.getByTestId('share-target').fill('obj-7');
    await page.getByTestId('share-submit').click();
    await expect(page.getByTestId('share-created-url')).toHaveValue(/shr_9/);
    expect(page.url(), 'the token must not enter the address bar').not.toContain('shr_9');
  });

  test("the server's 422 message is shown verbatim", async ({ page }) => {
    // The difference between a UI that says which field is wrong and one that
    // says "could not create link", which is a bug report rather than a fix.
    //
    // A deliberately LONG password rather than an out-of-range expiry: the
    // expiry input carries `min`/`max`, so the browser blocks that submit
    // before it is sent, and a 422 for it would be testing a path no user can
    // reach. The password has no length hint on the client, so this is a real
    // request the server answers.
    await page.route(/\/api\/share/, async (route: Route) => {
      if (route.request().method() === 'GET') {
        return route.fulfill({ status: 200, contentType: 'application/json', body: '[]' });
      }
      return route.fulfill({
        status: 422,
        contentType: 'application/json',
        body: JSON.stringify({ error: 'password must be 64 bytes or fewer' })
      });
    });
    await page.goto('/s');
    await page.getByTestId('share-target').fill('obj-7');
    await page.getByTestId('share-password').fill('x'.repeat(200));
    await page.getByTestId('share-submit').click();
    await expect(page.getByTestId('share-admin-error')).toContainText(
      'password must be 64 bytes or fewer'
    );
  });

  test('turning a link off refreshes the list', async ({ page }) => {
    let live = true;
    // A regex, not a glob. `**/api/share*` matches the LIST path only: the
    // `*` stops at the slash, so the DELETE to `/api/share/shr_1` went to the
    // real preview server and 404'd, and the row never changed. A regex
    // matches the whole path family, so one handler covers all three verbs.
    await page.route(/\/api\/share/, async (route: Route) => {
      const req = route.request();
      if (req.method() === 'DELETE') {
        live = false;
        return route.fulfill({ status: 200, contentType: 'application/json', body: '{"revoked":true}' });
      }
      if (req.method() === 'GET') {
        return route.fulfill({
          status: 200,
          contentType: 'application/json',
          body: JSON.stringify([{ ...one, state: live ? 'live' : 'revoked' }])
        });
      }
      return route.fulfill({ status: 201, contentType: 'application/json', body: '{}' });
    });
    await page.goto('/s');
    await expect(page.getByTestId('share-state')).toHaveText('Active');
    await page.getByTestId('share-revoke').first().click();
    await expect(page.getByTestId('share-state')).toHaveText('Turned off');
  });

  test('scope is chosen explicitly, with both options visible', async ({ page }) => {
    // A radio group rather than a select: a select hides the other option, and
    // "what does this allow" is the question a person is answering when they
    // open a share dialog.
    await page.route(/\/api\/share/, (route: Route) =>
      route.fulfill({ status: 200, contentType: 'application/json', body: '[]' })
    );
    await page.goto('/s');
    const group = page.getByTestId('share-scope');
    await expect(group.getByRole('radio')).toHaveCount(2);
    await expect(group).toContainText('View only');
    await expect(group).toContainText('View and download');
  });
});
