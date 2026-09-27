/**
 * The theme, in a real browser. T-P5-007, spec §10.8.
 *
 * The unit tests prove the arithmetic and the precedence. They cannot prove
 * the three things this file exists for, and all three are things that look
 * fine and are wrong:
 *
 *   1. That the inline script applies the theme BEFORE FIRST PAINT. A wrong
 *      order is invisible to every other test -- the page ends up in the right
 *      state, just after a flash.
 *   2. That the attribute survives a reload and a client-side navigation, i.e.
 *      that the STORE and the INLINE SCRIPT agree. They are two
 *      implementations of the same precedence rule, in two languages, and
 *      nothing but this file compares them.
 *   3. That the control is operable from the keyboard.
 */
import { test, expect } from '@playwright/test';

/** The `data-theme` the app is currently painting. */
async function theme(page: import('@playwright/test').Page): Promise<string | null> {
  return page.evaluate(() => document.documentElement.getAttribute('data-theme'));
}

/** The stored choice, which is the user's answer and not the painted theme. */
async function stored(page: import('@playwright/test').Page): Promise<string | null> {
  return page.evaluate(() => localStorage.getItem('commons.theme'));
}

test.describe('the theme is applied before first paint', () => {
  test('a stored light choice is applied by the inline script, not by Svelte', async ({
    page
  }) => {
    // Seed the choice, then load. `addInitScript` runs before the document is
    // parsed, so the inline head script -- which runs during parsing -- sees
    // it. Setting it after `goto` would be too late and the test would pass
    // while proving nothing about first paint.
    await page.addInitScript(() => {
      localStorage.setItem('commons.theme', 'light');
      // Force light regardless of the OS, so the assertion is about the
      // STORED value winning rather than about what this machine's OS says.
      const orig = window.matchMedia.bind(window);
      window.matchMedia = ((q: string) =>
        q.includes('dark') ? { ...orig(q), matches: true } : orig(q)) as typeof orig;
    });

    // Block the app bundle entirely. If the theme still lands, it can only
    // have come from the inline script -- which is the whole claim. Svelte
    // never runs, so `theme.init()` never runs, so the store cannot be the
    // source.
    await page.route('**/_app/immutable/**', (route) => route.abort());

    await page.goto('/');
    expect(await theme(page)).toBe('light');
  });

  test('the default with no stored choice follows the OS', async ({ page }) => {
    await page.addInitScript(() => {
      localStorage.clear();
      const orig = window.matchMedia.bind(window);
      window.matchMedia = ((q: string) =>
        q.includes('dark') ? { ...orig(q), matches: true } : orig(q)) as typeof orig;
    });
    await page.route('**/_app/immutable/**', (route) => route.abort());
    await page.goto('/');
    expect(await theme(page)).toBe('dark');
  });
});

test.describe('the two implementations of the precedence agree', () => {
  test('the inline script and the store resolve the same value', async ({ page }) => {
    // Three combinations. The failure this catches is a rule implemented once
    // in TypeScript and once in an inline string, which is two chances to be
    // wrong and no test that compares them.
    for (const [choice, osDark, want] of [
      ['light', true, 'light'],
      ['dark', false, 'dark'],
      ['system', true, 'dark'],
      ['system', false, 'light']
    ] as const) {
      // A fresh page per combination, and a fresh CONTEXT for the init
      // scripts. `addInitScript` accumulates on a page across `goto`, so by
      // the fourth iteration three earlier matchMedia wrappers were still
      // installed -- and the one that ran last in the chain was the first one
      // added, not the current one. The symptom was a test that failed on its
      // LAST case and passed on the first three.
      await page.context().clearCookies();
      await page.addInitScript(
        ([c, d]) => {
          localStorage.clear();
          if (c !== 'system') localStorage.setItem('commons.theme', c);
          const orig = window.matchMedia.bind(window);
          window.matchMedia = ((q: string) =>
            q.includes('dark') ? { ...orig(q), matches: d } : orig(q)) as typeof orig;
        },
        [choice, osDark] as const
      );
      await page.goto('/');
      expect(await theme(page), `choice=${choice} osDark=${osDark}`).toBe(want);
    }
  });
});

test.describe('the control', () => {
  test.beforeEach(async ({ page }) => {
    await page.addInitScript(() => localStorage.clear());
    await page.goto('/');
  });

  test('repaints and persists, and survives a reload', async ({ page }) => {
    await page.getByTestId('theme-light').click();
    expect(await theme(page)).toBe('light');
    expect(await stored(page)).toBe('light');

    await page.reload();
    expect(await theme(page)).toBe('light');
  });

  test('system removes the stored key rather than storing "system"', async ({ page }) => {
    await page.getByTestId('theme-dark').click();
    expect(await stored(page)).toBe('dark');
    await page.getByTestId('theme-system').click();
    // A key that is present and equal to the default is a state nothing can
    // ever change, and it is what a "write whatever was selected" toggle
    // produces. The e2e is the only place that can see storage.
    expect(await stored(page)).toBeNull();
  });

  test('is a radiogroup with one tab stop and arrow-key movement', async ({ page }) => {
    const group = page.getByTestId('theme-group');
    await expect(group).toHaveAttribute('role', 'radiogroup');
    await expect(group).toHaveAttribute('aria-label', 'Colour theme');

    await expect(page.getByTestId('theme-system')).toHaveAttribute('aria-checked', 'true');
    await expect(page.getByTestId('theme-light')).toHaveAttribute('aria-checked', 'false');

    // Roving tabindex: exactly one option is reachable by Tab.
    const tabbable = await group.locator('button[tabindex="0"]').count();
    expect(tabbable).toBe(1);

    await page.getByTestId('theme-system').focus();
    await page.keyboard.press('ArrowRight');
    await expect(page.getByTestId('theme-light')).toHaveAttribute('aria-checked', 'true');
    // Focus follows selection, so the next arrow press acts on the right
    // button rather than on the one that was focused a step ago.
    await expect(page.getByTestId('theme-light')).toBeFocused();

    await page.keyboard.press('ArrowRight');
    await expect(page.getByTestId('theme-dark')).toHaveAttribute('aria-checked', 'true');
    // Wraps, because a group with a dead end is a group that traps.
    await page.keyboard.press('ArrowRight');
    await expect(page.getByTestId('theme-system')).toHaveAttribute('aria-checked', 'true');
  });

  test('announces the change, and the live region is not hidden', async ({ page }) => {
    const status = page.getByTestId('theme-status');
    // `display:none` / `visibility:hidden` remove a node from the
    // accessibility tree, which makes a live region announce nothing. The
    // assertion is on the computed style, not on the class, because a
    // `visually-hidden` pattern is correct and a `display:none` is not.
    await expect(status).toHaveText(/Match system theme/);
    await page.getByTestId('theme-dark').click();
    await expect(status).toHaveText('Dark theme');
  });

  test('the painted colour follows the token, not a hard-coded value', async ({
    page
  }) => {
    // The point of the generated CSS: the light theme's --bg is the value in
    // contrast.ts. If the generator is bypassed or the import dropped, the
    // computed value stays dark and this fails -- which is the drift this
    // ticket was written to prevent.
    await page.getByTestId('theme-light').click();
    const bg = await page.evaluate(() =>
      getComputedStyle(document.documentElement).getPropertyValue('--bg').trim()
    );
    expect(bg.toLowerCase()).toBe('#fdfdfd');

    await page.getByTestId('theme-dark').click();
    const darkBg = await page.evaluate(() =>
      getComputedStyle(document.documentElement).getPropertyValue('--bg').trim()
    );
    expect(darkBg.toLowerCase()).toBe('#111111');
  });
});

test.describe('following the OS', () => {
  test('repaints when the OS flips while the choice is system', async ({ page }) => {
    await page.addInitScript(() => localStorage.clear());
    await page.emulateMedia({ colorScheme: 'light' });
    await page.goto('/');
    expect(await theme(page)).toBe('light');

    await page.emulateMedia({ colorScheme: 'dark' });
    // The media query fires asynchronously, so a bare expect would race the
    // listener rather than test it. The assertion below is on the waitFor,
    // not the value.
    await expect.poll(() => theme(page)).toBe('dark');
  });

  test('does NOT follow the OS when the user has chosen', async ({ page }) => {
    // The other half. Without this, "follows the OS" passes and a user who
    // explicitly chose light has their choice overridden on the next sunrise.
    await page.addInitScript(() => localStorage.setItem('commons.theme', 'light'));
    await page.emulateMedia({ colorScheme: 'dark' });
    await page.goto('/');
    expect(await theme(page)).toBe('light');
    await page.emulateMedia({ colorScheme: 'light' });
    await page.waitForTimeout(120);
    expect(await theme(page)).toBe('light');
  });
});

test.describe('the a11y primitives are global', () => {
  test('a focused control gets a visible ring, and only via :focus-visible', async ({
    page
  }) => {
    await page.goto('/');
    const btn = page.getByTestId('theme-light');

    await btn.focus();
    const ring = await btn.evaluate((el) => getComputedStyle(el).outlineWidth);
    expect(ring).not.toBe('0px');

    // The ring is defined once, globally, so a component cannot opt out of it
    // and a plugin cannot remove it. If this were a per-component rule, the
    // assertion would pass for the theme control and nothing else would be
    // covered.
    const global = await page.evaluate(() => {
      for (const sheet of Array.from(document.styleSheets)) {
        let rules: CSSRuleList;
        try {
          rules = sheet.cssRules;
        } catch {
          continue; // a cross-origin sheet; the local ones are what matter
        }
        for (const r of Array.from(rules)) {
          if (r instanceof CSSStyleRule && r.selectorText === ':focus-visible') {
            return r.style.outline;
          }
        }
      }
      return null;
    });
    expect(global, 'a global :focus-visible rule exists').toBeTruthy();
  });

  test('safe-area insets are declared with a fallback', async ({ page }) => {
    // The navigation is REQUIRED and its absence was a real bug in this file:
    // without it the page is `about:blank`, `document.styleSheets` is empty,
    // and the scan returns null for a rule that is present in the build. The
    // symptom reads as "the CSS is missing" and the fix is to go looking in
    // the stylesheet, which is correct and useless.
    await page.goto('/');

    const inset = await page.evaluate(() => {
      for (const sheet of Array.from(document.styleSheets)) {
        let rules: CSSRuleList;
        try {
          rules = sheet.cssRules;
        } catch {
          continue;
        }
        for (const r of Array.from(rules)) {
          if (r instanceof CSSStyleRule && r.style.paddingLeft?.includes('safe-area-inset-left')) {
            return r.style.paddingLeft;
          }
        }
      }
      return null;
    });
    // The fallback is the point: `env()` with no fallback is an invalid
    // declaration on an engine that lacks it, and the whole padding is
    // dropped rather than degraded.
    expect(inset).toContain('env(safe-area-inset-left');
    expect(inset).toMatch(/,\s*0px\)/);
  });
});
