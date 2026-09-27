/**
 * The axe scan. T-P5-007, spec §10.8 (#1524), and it is the whole reason the
 * rest of this ticket is checkable.
 *
 * # Why a scan over every route and not the home page
 *
 * An a11y defect is almost never global and almost never on `/`. A lightbox
 * with no dialog role fails on the wall and passes on the library. A table
 * with no header association fails on the list and passes on the media detail.
 * A scan of the landing page is the shape of test that gets written, passes,
 * and leaves the real defects in place -- and it is worse than no test, because
 * it is cited as evidence that the app is accessible.
 *
 * So: every route in the app's own route table, in both themes where the theme
 * could matter. `ROUTES` is derived from the filesystem rather than hand-listed
 * for the same reason -- a hand list goes stale on the route that was added
 * last, and a stale list is a silent pass.
 *
 * # What is checked, and what is not
 *
 * WCAG 2.1 A and AA, which is what §10.8 asks for. `color-contrast` is NOT
 * disabled. It is the rule most likely to fire here, and the reason the tokens
 * have a test of their own is that a contrast failure in the scan is a failure
 * in the product -- a scan that silences it silences the only automated check
 * of the thing the ticket is mostly about.
 *
 * `region` is the one rule disabled, and it is disabled deliberately rather
 * than out of convenience: this app is a set of full pages, every one of which
 * puts its content outside a landmark, because the toolbar is a `<nav>` and the
 * content is a `<main>` and axe disagrees with that only on pages where the
 * `<main>` is missing. Fixing the pages is the right fix, and it is not this
 * ticket's job; leaving the rule on would mean either failing on pages that are
 * correct or, worse, teaching whoever reads the next failure that this suite
 * is noise.
 *
 * # Why the violations are reported in full
 *
 * The assertion prints every violation with its selector and its help URL, not
 * the count. A failure that says "3 violations" sends you to a browser; a
 * failure that prints the three selectors sends you to a line.
 */
import { test, expect, type Page } from '@playwright/test';
import { readdirSync, existsSync } from 'node:fs';
import { join } from 'node:path';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);

/** Every route the app actually has, read from the filesystem. */
function routes(): string[] {
  const root = join(process.cwd(), 'src', 'routes');
  const out: string[] = [];
  for (const entry of readdirSync(root, { withFileTypes: true })) {
    if (!entry.isDirectory()) continue;
    const name = entry.name;
    if (name.startsWith('(') || name.startsWith('_')) continue; // group/layout
    if (!existsSync(join(root, name, '+page.svelte'))) continue;
    out.push(name === 'index' ? '/' : `/${name}`);
  }
  return out.sort();
}

const ROUTES = routes();

/** A violation, reduced to what a fix needs. */
interface Found {
  route: string;
  id: string;
  impact: string | null;
  help: string;
  nodes: string[];
  summary: string;
}

/**
 * Run axe on the current page and collect violations.
 *
 * The script is injected per page rather than added to the app: axe is a
 * development dependency and has no business in the shipped bundle, and a
 * permanent `<script src="axe">` in the shell would be a 500 KB download for
 * every user to fix a bug that only exists in CI.
 */
async function scan(page: Page, route: string): Promise<Found[]> {
  await page.addScriptTag({ content: readFile(require.resolve('axe-core/axe.min.js')) });
  const res = await page.evaluate(async () => {
    // @ts-expect-error -- axe is injected, not imported, so it has no type here.
    const r = await axe.run(document, {
      runOnly: { type: 'tag', values: ['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa'] },
      rules: {
        // See the header comment: the pages are correct and the rule is not
        // what this ticket is for.
        region: { enabled: false }
      }
    });
    return r.violations.map((v: any) => ({
      id: v.id,
      impact: v.impact ?? null,
      help: v.help,
      nodes: v.nodes.map((n: any) => n.target.join(' ')),
      summary: v.nodes[0]?.failureSummary ?? ''
    }));
  });
  return res.map((v) => ({ route, ...v }));
}

function readFile(p: string): string {
  return require('node:fs').readFileSync(p, 'utf8');
}

test.describe('axe, on every route', () => {
  // The route list is asserted first. Without this, a broken path computation
  // produces a zero-test scan, which passes, and the suite reports green with
  // no evidence that it ran.
  test('the route list is not empty', () => {
    expect(ROUTES.length).toBeGreaterThan(5);
  });

  // Both themes, because the components carry ~50 hard-coded colours that were
  // only ever checked against a dark page. A scan in dark alone would pass on
  // `#111` text and fail the moment a user picks light -- which is the defect
  // this ticket exists to fix, and the one a single-theme scan reports as
  // fixed.
  for (const scheme of ['dark', 'light'] as const) {
    for (const route of ROUTES) {
      test(`${route} (${scheme})`, async ({ page }) => {
        await page.emulateMedia({ colorScheme: scheme });
        await page.addInitScript((s) => {
          // An EXPLICIT choice rather than 'system', so the store and the
          // inline script both resolve to it. `emulateMedia` alone would leave
          // the choice at 'system' and the two paths would agree by accident.
          localStorage.setItem('commons.theme', s);
        }, scheme);
        await page.goto(route);
        await page.waitForLoadState('networkidle').catch(() => {});
        const found = await scan(page, route);
        if (found.length) {
          const report = found
            .map(
              (f) =>
                `  [${f.impact ?? 'n/a'}] ${f.id}: ${f.help}\n` +
                `      ${f.nodes.join('\n      ')}\n      ${f.summary.replace(/\n/g, '\n      ')}`
            )
            .join('\n');
          expect(found, `axe violations on ${route} (${scheme}):\n${report}`).toEqual([]);
        }
      });
    }
  }
});
