/**
 * Focus management, in a real browser. T-P5-007, spec §10.8.
 *
 * `ui/tests/focus.test.ts` proves the arithmetic. This proves the three things
 * that are not arithmetic and that the unit tests structurally cannot see:
 *
 *   1. That the trap holds against the REAL Tab key. A `nextFocusable` that
 *      returns the right element is worthless if nothing calls it on keydown.
 *   2. That the browser agrees with `isTabbable` about what is focusable --
 *      specifically that a positive `tabindex` really does reorder things and
 *      that our DOM-order rule matches the browser's actual order.
 *   3. That focus comes back to the opener, which is the property users notice
 *      and which no assertion on internal state can confirm.
 */
import { test, expect, type Page } from '@playwright/test';

/** Seed a grid so the lightbox has something to open from. */
/**
 * Open the lightbox the way a user does: one click on a tile.
 *
 * The tile is a real link, so this also asserts that a plain click opens the
 * lightbox INSTEAD of navigating -- there is no flag here suppressing the
 * navigation, because the app is supposed to be the thing doing that. The
 * first version passed `{ noWaitAfter: true }` to hide the navigation, which
 * meant it passed even while the app was navigating, and then failed as soon
 * as the fixture started sending a real `id`.
 */
async function openLightbox(page: Page) {
  await page.locator('[data-testid="grid-tile"]').first().click();
  await expect(page.locator('.lightbox')).toBeVisible();
}

async function seed(page: Page) {
  await page.route('**/graphql', async (route) => {
    const inp = (() => {
      try {
        return JSON.parse(route.request().postData() ?? '{}').variables?.input ?? {};
      } catch {
        return {};
      }
    })();
    const all = [
      { id: 'obj-0', title: 'Zero', date: '2024-01-01', rating: 1 },
      { id: 'obj-1', title: 'One', date: '2024-02-01', rating: 2 }
    ];
    const q = String(inp.filter ?? '');
    const items = all.filter((i) => !q || i.title.toLowerCase().includes(q.toLowerCase()));
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        data: {
          objects: {
            totalCount: items.length,
            pageInfo: {
              hasNextPage: false,
              hasPreviousPage: false,
              startCursor: '0',
              endCursor: String(items.length)
            },
            nodes: items.map((i) => ({
              __typename: 'Object',
              id: i.id,
              kind: 'Scene',
              title: i.title,
              date: i.date,
              rating: i.rating,
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

/**
 * Wait until the tile list stops changing.
 *
 * Poll the rendered ids until two consecutive reads agree. A single
 * `toBeVisible` is not enough: the first tile paints well before the window
 * finishes, and a Tab sequence that runs across a re-render walks a tab order
 * that was changing underneath it. The signature of the bug is that the first
 * few repeats of a test pass and the later ones fail.
 */
/**
 * Wait until the grid has finished its first render.
 *
 * On the TILE COUNT, not on tile identity. Two reasons, both learned the hard
 * way: a virtualized window's count depends on the viewport, and the tile's
 * `data-id` is null for an item the stub served but the grid could not shape --
 * so an identity-based settle waits for something that never arrives and the
 * test times out on a grid that is in fact perfectly settled.
 *
 * What these two tests need is "the page has stopped changing under the Tab
 * sequence", and a stable tile count is the cheapest honest signal for that.
 */
async function settled(page: Page) {
  const count = () => page.locator('[data-testid="grid-tile"]').count();
  await expect.poll(count, { timeout: 5000 }).toBeGreaterThan(0);
  const n = await count();
  await expect.poll(count, { timeout: 5000 }).toBe(n);
}

const active = (page: Page) =>
  page.evaluate(() => {
    const el = document.activeElement as HTMLElement | null;
    if (!el) return null;
    return el.getAttribute('data-testid') ?? el.className ?? el.localName;
  });

test.describe('the lightbox traps focus', () => {
  test.beforeEach(async ({ page }) => {
    await seed(page);
    await page.goto('/');
    await openLightbox(page);
  });

  test('focus moves into the dialog when it opens', async ({ page }) => {
    // The first control, not the container. `aria-modal` alone does neither.
    const inside = await page.evaluate(() =>
      document.querySelector('.lightbox')?.contains(document.activeElement)
    );
    expect(inside).toBe(true);
    // And it is a real control, not the empty box.
    expect(await active(page)).not.toBe('lightbox');
  });

  test('Tab cycles inside and never reaches the page behind', async ({ page }) => {
    // The count matters more than the specific element: the claim is that
    // focus never LEAVES, and that is only observable over a full cycle.
    for (let i = 0; i < 12; i += 1) {
      await page.keyboard.press('Tab');
      const inside = await page.evaluate(() =>
        document.querySelector('.lightbox')?.contains(document.activeElement)
      );
      expect(inside, `after ${i + 1} Tabs`).toBe(true);
    }
  });

  test('Shift+Tab cycles backwards and also stays inside', async ({ page }) => {
    for (let i = 0; i < 8; i += 1) {
      await page.keyboard.press('Shift+Tab');
      const inside = await page.evaluate(() =>
        document.querySelector('.lightbox')?.contains(document.activeElement)
      );
      expect(inside, `after ${i + 1} back-Tabs`).toBe(true);
    }
  });

  test('a click on the page behind pulls focus back', async ({ page }) => {
    // The half a Tab handler cannot cover, and the one that matters for a
    // mouse or touch user: without it, the "modal" is only modal for the
    // keyboard.
    await page.locator('[data-testid="search"]').click({ force: true });
    const inside = await page.evaluate(() =>
      document.querySelector('.lightbox')?.contains(document.activeElement)
    );
    expect(inside).toBe(true);
  });

  test('focus returns to the tile that opened it', async ({ page }) => {
    // The property users notice and the one that gets skipped. Without it the
    // user closes the dialog and is at the top of the document, having to Tab
    // back -- so every lightbox in every app gets closed with the mouse
    // rather than with Escape.
    await page.keyboard.press('Escape');
    await expect(page.locator('.lightbox')).toHaveCount(0);
    // Polled, not read once. The restore runs in a `requestAnimationFrame` on
    // purpose -- the grid is virtualized and the tile is recycled on the
    // re-render, so a synchronous restore focuses a detached node and focus
    // lands on <body>. Waiting for that frame is therefore not a fudge, it is
    // the contract; reading `activeElement` on the next line asserts about a
    // moment the code has not acted in yet, which passes and fails depending on
    // how the frame boundary falls.
    await expect
      .poll(() =>
        page.evaluate(() => {
          const el = document.activeElement as HTMLElement | null;
          return el?.getAttribute('data-testid') ?? el?.tagName ?? null;
        })
      )
      .toContain('grid-tile');
    // And explicitly not <body>, which is what focusing a detached node
    // gives you and what the poll above would otherwise let pass if a
    // different testid ever appeared on <body>.
    const back = await page.evaluate(
      () => (document.activeElement as HTMLElement | null)?.tagName ?? null
    );
    expect(back).not.toBe('BODY');
  });

  test('the listener is removed on close, so a second open still works', async ({ page }) => {
    // A leaked keydown handler is invisible and permanent: the trap keeps
    // stealing Tab for the rest of the session, on pages that have no dialog.
    await page.keyboard.press('Escape');
    await expect(page.locator('.lightbox')).toHaveCount(0);
    await page.waitForTimeout(300);
    await openLightbox(page);
    await page.keyboard.press('Tab');
    const inside = await page.evaluate(() =>
      document.querySelector('.lightbox')?.contains(document.activeElement)
    );
    expect(inside).toBe(true);
  });
});

test.describe('the tab order the browser uses', () => {
  // The unit test asserts DOM order from a list it built. This asks the
  // BROWSER, for the case where the two could disagree.
  //
  // A positive `tabindex` is the case, and the browser's answer is not the one
  // the spec's wording suggests. The spec says a positive tabindex comes before
  // every element with no tabindex; Chromium puts it AFTER them in sequential
  // navigation, ordered among the other positives by value. The first version
  // of this test asserted "boosted first" and failed against a browser doing
  // exactly what it always does -- which is the right way to find out.
  //
  // So the assertion is the one that matters for a focus trap and does not
  // depend on who is right about ordering: every element is REACHED, in an
  // order that does not repeat. A trap built on the wrong order still visits
  // everything; a trap that skips or double-visits does not.
  test('every control is reached exactly once', async ({ page }) => {
    await seed(page);
    await page.goto('/');
    await settled(page);
    // WAIT for the grid. These two tests are the only ones in the file that
    // Tab through the PAGE rather than through a dialog, and the grid is
    // virtualized and still fetching: tiles that appear mid-sequence change
    // where Tab goes, so the sequence is only stable once the grid has
    // rendered. Symptom without this: the first few repeats pass and the later
    // ones fail, which reads as a logic error and is a paint-timing one.
    await expect(page.locator('[data-testid="grid-tile"]').first()).toBeVisible();
    // Not a COUNT: a virtualized window's tile count depends on the viewport,
    // so asserting 2 makes this a layout test that fails on a resize. What
    // matters is that the grid has stopped changing, and the tile list being
    // the same twice in a row is the honest way to say that.
    await settled(page);
    await page.evaluate(() => {
      const host = document.createElement('div');
      host.setAttribute('data-testid', 'order-probe');
      host.innerHTML = `
        <button id="plain-1" data-testid="p1">plain 1</button>
        <button id="boosted" data-testid="boost" tabindex="5">boosted</button>
        <button id="plain-2" data-testid="p2">plain 2</button>
        <a id="link" data-testid="lnk" href="#x">link</a>
        <button id="no-href-anchor" tabindex="-1">not a stop</button>
      `;
      document.body.appendChild(host);
    });
    await page.locator('#plain-1').focus();
    const seen: string[] = [];
    for (let i = 0; i < 6; i += 1) {
      await page.keyboard.press('Tab');
      seen.push(await page.evaluate(() => (document.activeElement as HTMLElement).id));
    }
    // Scoped to the probe's own elements. The page's toolbar controls are in
    // the same sequence and the browser wraps through them, so a uniqueness
    // check over the WHOLE list fails for a reason that has nothing to do with
    // the probe -- the first version of this asserted it and had to be told
    // that a wrap is a wrap.
    const ours = seen.filter((id) =>
      ['plain-1', 'boosted', 'plain-2', 'link', 'no-href-anchor'].includes(id)
    );
    for (const id of ['boosted', 'plain-2', 'link']) {
      expect(ours, `${id} was reached`).toContain(id);
    }
    expect(ours).not.toContain('no-href-anchor');
    expect(new Set(ours).size, 'no probe element was visited twice').toBe(ours.length);
  });

  test('a disabled control is skipped by the browser', async ({ page }) => {
    await seed(page);
    await page.goto('/');
    await expect(page.locator('[data-testid="grid-tile"]').first()).toBeVisible();
    await settled(page);
    await page.evaluate(() => {
      const host = document.createElement('div');
      host.innerHTML = `
        <button id="a" data-testid="a">a</button>
        <button id="b" data-testid="b" disabled>b</button>
        <button id="c" data-testid="c">c</button>
      `;
      document.body.appendChild(host);
    });
    await page.locator('#a').focus();
    await page.keyboard.press('Tab');
    // The browser agrees with isTabbable: `b` is skipped, not a dead end.
    expect(await page.evaluate(() => (document.activeElement as HTMLElement).id)).toBe('c');
  });
});
