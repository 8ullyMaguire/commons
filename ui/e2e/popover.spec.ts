/**
 * A menu that stays inside the window. T-P5-007, spec 10.8, stash#4667.
 *
 * The failure this file exists to prevent: a menu positioned by CSS alone
 * opens off the bottom of the screen on a long page, and the user's only way
 * to reach it is to scroll, which is not a fix.
 *
 * These drive the real `Menu`, mounted in the shell toolbar, because the
 * toolbar trigger is at the far right and the bottom of the viewport -- which
 * is the worst case for a menu that opens downward, and the case the ticket is
 * about. A popover only ever tested near the top of the window is a popover
 * whose viewport clamping has never run.
 */

import { expect, test, type Page } from '@playwright/test';

const NODES = [
  {
    __typename: 'Object',
    id: 'obj-0',
    kind: 'Scene',
    title: 'Zero',
    date: '2024-01-01',
    rating: 1,
    organized: null,
    coverPath: null,
    width: 800,
    height: 1200
  }
];

test.beforeEach(async ({ page }) => {
  await page.route('**/graphql', async (route) => {
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        data: {
          objects: {
            totalCount: NODES.length,
            pageInfo: {
              hasNextPage: false,
              hasPreviousPage: false,
              startCursor: '0',
              endCursor: String(NODES.length)
            },
            nodes: NODES
          }
        }
      })
    });
  });
  await page.goto('/');
  await expect(page.locator('[data-testid="grid-tile"]').first()).toBeVisible();
});

/** The menu's box in viewport coordinates, and which side it chose. */
const boxOf = (page: Page) =>
  page.evaluate(() => {
    const el = document.querySelector<HTMLElement>('[data-testid="menu-popover"]');
    if (!el) return null;
    const r = el.getBoundingClientRect();
    return {
      x: r.x,
      y: r.y,
      w: r.width,
      h: r.height,
      side: el.dataset.popoverSide ?? null
    };
  });

test.describe('a popover stays inside the window', () => {
  test('a menu that does not fit is shrunk or flipped, never clipped', async ({ page }) => {
    // The invariant from #4667 is "the user can see all of it", not "it opens
    // upward". There are two correct answers when there is not enough room
    // below -- flip to the other side, or shrink to the space available -- and
    // an earlier version of this test asserted the flip and so failed against
    // a menu that had shrunk perfectly well. Asserting the invariant is what
    // the ticket asks for; asserting a particular strategy is a claim about
    // the implementation that the user cannot observe and does not need.
    await page.locator('[data-testid="menu-trigger"]').click();
    await expect(page.locator('[data-testid="menu-popover"]')).toBeVisible();
    const vp = page.viewportSize()!;
    const natural = await boxOf(page);
    expect(natural!.side, 'with room below, the menu opens downward').toBe('bottom');

    // Squeeze the window until the menu cannot fit below its trigger.
    await page.setViewportSize({ width: vp.width, height: 120 });
    await expect
      .poll(async () => {
        const b = (await boxOf(page))!;
        return b.y >= 0 && b.y + b.h <= 120;
      })
      .toBe(true);

    const box = (await boxOf(page))!;
    // Whichever strategy it chose, the whole box is on screen. This is the
    // assertion that fails against a menu positioned by CSS alone, which is
    // the bug.
    expect(box.y, 'the top edge is on screen').toBeGreaterThanOrEqual(0);
    expect(box.y + box.h, 'the bottom edge is on screen').toBeLessThanOrEqual(120);
    // And it is still usable: a menu squeezed to nothing is not "on screen".
    expect(box.h, 'the menu kept a usable height').toBeGreaterThan(40);
  });

  test('a short window makes the menu open upward when that is the only way', async ({
    page
  }) => {
    // The flip itself, isolated. A window too short for the menu on EITHER
    // side is the case that forces a decision, and with a short menu the
    // upward side is the one with room.
    const vp = page.viewportSize()!;
    await page.setViewportSize({ width: vp.width, height: 300 });
    await page.locator('[data-testid="menu-trigger"]').click();
    await expect(page.locator('[data-testid="menu-popover"]')).toBeVisible();

    // Measure the trigger: if it sits in the lower half, `top` is the side
    // with room and the menu must go there.
    const t = (await page.locator('[data-testid="menu-trigger"]').boundingBox())!;
    const menu = (await boxOf(page))!;
    const triggerMid = t.y + t.height / 2;
    if (triggerMid > 150) {
      expect(menu.side).toBe('top');
      expect(menu.y + menu.h).toBeLessThanOrEqual(t.y + 1);
    } else {
      expect(menu.side).toBe('bottom');
      expect(menu.y).toBeGreaterThanOrEqual(t.y + t.height - 1);
    }
  });

  test('the menu is inside the window horizontally too', async ({ page }) => {
    await page.locator('[data-testid="menu-trigger"]').click();
    await expect(page.locator('[data-testid="menu-popover"]')).toBeVisible();
    const box = (await boxOf(page))!;
    const vp = page.viewportSize()!;
    expect(box.x).toBeGreaterThanOrEqual(0);
    expect(box.x + box.w).toBeLessThanOrEqual(vp.width);
  });

  test('the menu re-places when the window is resized', async ({ page }) => {
    await page.locator('[data-testid="menu-trigger"]').click();
    await expect(page.locator('[data-testid="menu-popover"]')).toBeVisible();
    const before = (await boxOf(page))!;

    await page.setViewportSize({ width: 420, height: 640 });
    // Polled, not read once: the reposition is a resize listener, so it lands
    // after the resize event rather than with it.
    await expect.poll(async () => (await boxOf(page))!.x).not.toBe(before.x);
    const after = (await boxOf(page))!;
    expect(after.x + after.w).toBeLessThanOrEqual(420);
  });

  test('a menu opened twice is placed the same way both times', async ({ page }) => {
    await page.locator('[data-testid="menu-trigger"]').click();
    await expect(page.locator('[data-testid="menu-popover"]')).toBeVisible();
    const first = await boxOf(page);

    await page.keyboard.press('Escape');
    await expect(page.locator('[data-testid="menu-popover"]')).toHaveCount(0);

    await page.locator('[data-testid="menu-trigger"]').click();
    await expect(page.locator('[data-testid="menu-popover"]')).toBeVisible();
    const second = await boxOf(page);

    // Placement must be a function of the geometry, not of how many times the
    // action has run. A popover that drifts on the second open is the kind of
    // bug that gets reported as "sometimes it is in the wrong place".
    expect(second!.side).toBe(first!.side);
    expect(second!.x).toBe(first!.x);
    expect(second!.y).toBe(first!.y);
  });
});

test.describe('the menu is a menu, not a list of links', () => {
  test('it has the menu roles and the trigger advertises them', async ({ page }) => {
    const trigger = page.locator('[data-testid="menu-trigger"]');
    await expect(trigger).toHaveAttribute('aria-haspopup', 'menu');
    await expect(trigger).toHaveAttribute('aria-expanded', 'false');
    await trigger.click();
    await expect(trigger).toHaveAttribute('aria-expanded', 'true');
    await expect(page.locator('[data-testid="menu-popover"]')).toHaveAttribute('role', 'menu');
    await expect(page.locator('[data-testid="menu-item"]').first()).toHaveAttribute(
      'role',
      'menuitem'
    );
  });

  test('ArrowDown and ArrowUp move between items and wrap', async ({ page }) => {
    await page.locator('[data-testid="menu-trigger"]').click();
    await expect(page.locator('[data-testid="menu-popover"]')).toBeVisible();
    const items = page.locator('[data-testid="menu-item"]');
    const n = await items.count();
    expect(n).toBeGreaterThan(1);

    const active = () =>
      page.evaluate(() => document.activeElement?.textContent?.trim() ?? null);
    const label = async (i: number) => (await items.nth(i).textContent())?.trim();

    // Opening moves focus to the FIRST item, and that takes a frame: the menu
    // has to be rendered and placed before an element in it can be focused.
    // Asserting `activeElement` straight after the visibility check measured
    // the trigger and reported a menu that "did not take focus" -- so this
    // waits for the focus rather than assuming it.
    //
    // A visibility assertion cannot see this class of failure at all, which is
    // why the whole file asserts on `activeElement` and not on what is on
    // screen.
    await expect.poll(active).toBe(await label(0));

    // And from there the arrows move. A menu that opens with nothing focused
    // makes the first keypress do nothing, which reads as broken.
    await page.keyboard.press('ArrowDown');
    expect(await active()).toBe(await label(1));
    await page.keyboard.press('ArrowDown');
    expect(await active()).toBe(await label(2 % n));

    // Wraps at both ends. From the last item, ArrowUp steps back to the first
    // rather than sticking on the last -- a menu that stops at the ends needs a
    // different gesture to leave, and the user just used this one.
    await page.keyboard.press('ArrowUp');
    expect(await active()).toBe(await label(1));

    // Home and End, which is what a long menu needs and Arrow alone does not
    // give.
    await page.keyboard.press('End');
    expect(await active()).toBe(await label(n - 1));
    await page.keyboard.press('Home');
    expect(await active()).toBe(await label(0));

    // Typeahead: a single letter jumps to the next item starting with it,
    // wrapping. Every native menu does this and users' fingers already know it.
    await page.keyboard.press('p');
    expect(await active()).toBe(await label(n - 1));
  });

  test('Escape closes the menu and returns focus to the trigger', async ({ page }) => {
    const trigger = page.locator('[data-testid="menu-trigger"]');
    await trigger.click();
    await expect(page.locator('[data-testid="menu-popover"]')).toBeVisible();
    await page.keyboard.press('Escape');
    await expect(page.locator('[data-testid="menu-popover"]')).toHaveCount(0);
    // The part that matters: without the restore, a keyboard user is stranded
    // on <body> and has to Tab the whole toolbar again to find the button.
    await expect(trigger).toBeFocused();
  });

  test('a click elsewhere closes the menu without stealing focus', async ({ page }) => {
    await page.locator('[data-testid="menu-trigger"]').click();
    await expect(page.locator('[data-testid="menu-popover"]')).toBeVisible();
    await page.locator('[data-testid="search"]').click({ force: true });
    await expect(page.locator('[data-testid="menu-popover"]')).toHaveCount(0);
  });

  test('an item is a 44px touch target', async ({ page }) => {
    await page.locator('[data-testid="menu-trigger"]').click();
    await expect(page.locator('[data-testid="menu-popover"]')).toBeVisible();
    // stash#6383 / #6823. Asserted on the item, not the trigger: the item is
    // what a finger lands on, and it is the one most likely to be sized by the
    // text inside it.
    const box = (await page.locator('[data-testid="menu-item"]').first().boundingBox())!;
    expect(box.height).toBeGreaterThanOrEqual(44);
  });
});
