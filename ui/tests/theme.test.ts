/**
 * Theme resolution and popover placement, without a browser.
 *
 * T-P5-007, spec §10.8. Two pure modules, and the tests are pure too -- which
 * is the reason they are in `.ts` and not `.svelte`. The DOM half is four
 * lines in `applyTo` and the e2e covers it.
 */
import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import {
  CHOICES,
  DEFAULT_CHOICE,
  STORAGE_KEY,
  THEME_ATTR,
  applyTo,
  currentChoice,
  isThemeChoice,
  osPrefersDark,
  readChoice,
  resolve,
  tokensFor,
  watchSystem,
  writeChoice,
  type ThemeStorage
} from '../src/lib/theme/theme.js';
import {
  fits,
  opposite,
  place,
  sideForCorner,
  sideForPoint,
  type Corner
} from '../src/lib/theme/popover.js';

/** A `localStorage` in a variable, so the tests need no browser. */
function memStore(initial: Record<string, string> = {}): ThemeStorage & { map: Map<string, string> } {
  const map = new Map(Object.entries(initial));
  return {
    map,
    getItem: (k) => map.get(k) ?? null,
    setItem: (k, v) => void map.set(k, v),
    removeItem: (k) => void map.delete(k)
  };
}

/** A `documentElement` that records what was written to it. */
function fakeDoc(start: string | null = null) {
  const attrs = new Map<string, string>();
  if (start !== null) attrs.set(THEME_ATTR, start);
  return {
    attrs,
    documentElement: {
      getAttribute: (n: string) => attrs.get(n) ?? null,
      setAttribute: (n: string, v: string) => void attrs.set(n, v)
    }
  };
}

describe('theme precedence', () => {
  it('an explicit choice beats the OS in both directions', () => {
    // The direction that matters: a user who chose light on a dark OS must
    // get light. The reverse is the same rule, and is what makes the rule a
    // precedence rather than a special case for one side.
    assert.equal(resolve('light', true), 'light');
    assert.equal(resolve('dark', false), 'dark');
  });

  it('system follows the OS', () => {
    assert.equal(resolve('system', true), 'dark');
    assert.equal(resolve('system', false), 'light');
  });

  it('defaults to system, not to dark', () => {
    // The app shipped dark, so a hardcoded dark default would be the
    // "obvious" choice. It is wrong: it makes a light-OS user who has
    // expressed no preference get the app's historical colour.
    assert.equal(DEFAULT_CHOICE, 'system');
    assert.equal(readChoice(memStore()), 'system');
    assert.equal(readChoice(null), 'system');
  });

  it('recognises exactly three choices', () => {
    assert.deepEqual([...CHOICES], ['system', 'light', 'dark']);
    for (const c of CHOICES) assert.ok(isThemeChoice(c));
    for (const c of ['', 'DARK', 'auto', null, undefined, 1, {}]) {
      assert.ok(!isThemeChoice(c), `${JSON.stringify(c)} is not a choice`);
    }
  });
});

describe('the stored choice survives nonsense', () => {
  // A value from a previous version, or from someone editing storage. The
  // choice is "boot anyway", because refusing to start because of a stale
  // preference is a far worse failure than ignoring it.
  it('ignores a stored value that is not a choice', () => {
    assert.equal(readChoice(memStore({ [STORAGE_KEY]: 'sepia' })), 'system');
    assert.equal(readChoice(memStore({ [STORAGE_KEY]: '' })), 'system');
  });

  it('reads back what it wrote', () => {
    const s = memStore();
    for (const c of CHOICES) {
      writeChoice(s, c);
      assert.equal(readChoice(s), c);
    }
  });

  it('clears the key when the choice goes back to system', () => {
    const s = memStore();
    writeChoice(s, 'dark');
    assert.ok(s.map.has(STORAGE_KEY));
    writeChoice(s, 'system');
    // Absent rather than the string "system": a key that is present and
    // equal to the default is a state nothing can ever change.
    assert.ok(!s.map.has(STORAGE_KEY));
  });

  it('degrades to a working app when storage throws', () => {
    // Safari in private mode throws on localStorage access. A theme must not
    // be the thing that stops the app rendering.
    const hostile: ThemeStorage = {
      getItem: () => {
        throw new Error('SecurityError');
      },
      setItem: () => {
        throw new Error('QuotaExceededError');
      },
      removeItem: () => {
        throw new Error('SecurityError');
      }
    };
    assert.equal(readChoice(hostile), 'system');
    assert.doesNotThrow(() => writeChoice(hostile, 'light'));
  });
});

describe('the OS preference', () => {
  it('reads the media query', () => {
    assert.equal(osPrefersDark(() => ({ matches: true })), true);
    assert.equal(osPrefersDark(() => ({ matches: false })), false);
  });

  it('assumes dark when it cannot ask', () => {
    // A browser that cannot answer the query is more likely old than dark,
    // and this preserves the app's historical appearance either way. The
    // alternative -- assuming light -- repaints every user on such an engine.
    assert.equal(osPrefersDark(null), true);
    assert.equal(
      osPrefersDark(() => {
        throw new Error('unsupported');
      }),
      true
    );
  });
});

describe('following the OS while on system', () => {
  it('subscribes and unsubscribes on a modern engine', () => {
    let fired = 0;
    const listeners: (() => void)[] = [];
    const off = watchSystem(
      {
        matches: false,
        addEventListener: (_t, cb) => void listeners.push(cb),
        removeEventListener: (_t, cb) => {
          const i = listeners.indexOf(cb);
          if (i >= 0) listeners.splice(i, 1);
        }
      },
      () => void fired++
    );
    assert.equal(listeners.length, 1);
    listeners[0]();
    assert.equal(fired, 1);
    off();
    assert.equal(listeners.length, 0);
  });

  it('falls back to addListener on an old Safari', () => {
    // Safari < 14 has no addEventListener on a MediaQueryList. The failure
    // this prevents is not a stale theme, it is a THROW on mount, because the
    // optional call would be on undefined.
    let fired = 0;
    const old: (() => void)[] = [];
    const off = watchSystem(
      {
        matches: false,
        addListener: (cb) => void old.push(cb),
        removeListener: (cb) => {
          const i = old.indexOf(cb);
          if (i >= 0) old.splice(i, 1);
        }
      },
      () => void fired++
    );
    old[0]();
    assert.equal(fired, 1);
    off();
    assert.equal(old.length, 0);
  });

  it('returns a no-op rather than throwing when neither API exists', () => {
    const off = watchSystem({ matches: true }, () => {});
    assert.doesNotThrow(off);
    assert.doesNotThrow(() => watchSystem(null, () => {})());
  });
});

describe('applying the theme', () => {
  it('writes the resolved theme, not the choice', () => {
    // The attribute holds "light" or "dark" so the CSS has ONE selector. If it
    // held "system" the stylesheet would need a media query AND an override,
    // and the two could disagree -- which is how a flash of the wrong theme
    // happens.
    const d = fakeDoc();
    applyTo(d, 'light');
    assert.equal(d.documentElement.getAttribute(THEME_ATTR), 'light');
    applyTo(d, 'dark');
    assert.equal(d.documentElement.getAttribute(THEME_ATTR), 'dark');
  });

  it('reads the choice back off the document', () => {
    assert.equal(currentChoice(fakeDoc('light')), 'light');
    assert.equal(currentChoice(fakeDoc('nonsense')), 'system');
    assert.equal(currentChoice(fakeDoc(null)), 'system');
    assert.equal(currentChoice(null), 'system');
  });

  it('is a no-op without a document', () => {
    assert.doesNotThrow(() => applyTo(null, 'dark'));
  });

  it('hands back the token set for each resolved theme', () => {
    assert.equal(tokensFor('dark').name, 'dark');
    assert.equal(tokensFor('light').name, 'light');
  });
});

describe('popover placement', () => {
  const VP = { width: 1000, height: 800 };

  it('opens on the requested side when there is room', () => {
    const p = place({
      anchor: { x: 400, y: 400, width: 20, height: 20 },
      size: { width: 100, height: 50 },
      viewport: VP,
      prefer: 'bottom'
    });
    assert.equal(p.side, 'bottom');
    assert.equal(p.x, 400);
    assert.equal(p.y, 428, 'below the anchor, plus the gap');
  });

  it('flips when the requested side does not fit', () => {
    // The #4667 case: a menu at the bottom of the screen must not hang off it.
    const p = place({
      anchor: { x: 400, y: 740, width: 20, height: 20 },
      size: { width: 100, height: 50 },
      viewport: VP,
      prefer: 'bottom'
    });
    assert.equal(p.side, 'top');
    assert.ok(p.y + p.height <= VP.height, 'it is on screen');
  });

  it('flips horizontally too, and reports the side so an arrow can follow', () => {
    const p = place({
      anchor: { x: 950, y: 400, width: 20, height: 20 },
      size: { width: 100, height: 50 },
      viewport: VP,
      prefer: 'right'
    });
    assert.equal(p.side, 'left');
    assert.ok(p.x + p.width <= VP.width);
  });

  it('does NOT flip when neither side fits', () => {
    // A popover taller than the viewport fits nowhere. Flipping anyway puts
    // it off the top, and a user sees the bottom edge of a menu and no top --
    // worse than either placement. So the requested side is kept and the clamp
    // does the work.
    const p = place({
      anchor: { x: 400, y: 400, width: 20, height: 20 },
      size: { width: 100, height: 2000 },
      viewport: VP,
      prefer: 'bottom'
    });
    assert.equal(p.side, 'bottom');
    assert.ok(p.y >= 0 && p.y + p.height <= VP.height, 'clamped into the viewport');
  });

  it('shifts along the cross axis rather than hanging off the end', () => {
    const p = place({
      anchor: { x: 960, y: 400, width: 20, height: 20 },
      size: { width: 200, height: 50 },
      viewport: VP,
      prefer: 'bottom'
    });
    assert.ok(p.x + p.width <= VP.width, `x=${p.x} w=${p.width}`);
    assert.ok(p.x >= 0);
  });

  it('keeps a margin from the viewport edge', () => {
    const p = place({
      anchor: { x: 0, y: 0, width: 10, height: 10 },
      size: { width: 50, height: 50 },
      viewport: VP,
      prefer: 'top',
      margin: 12
    });
    assert.ok(p.x >= 12, `x=${p.x}`);
    assert.ok(p.y >= 12, `y=${p.y}`);
  });

  it('reports the SHRUNK size, so the caller can reflow the content', () => {
    // The box cannot fit, so it is narrowed. A caller that is not told will lay
    // out content at the original width and the text will overflow sideways.
    const p = place({
      anchor: { x: 0, y: 0, width: 10, height: 10 },
      size: { width: 5000, height: 50 },
      viewport: VP,
      prefer: 'bottom'
    });
    assert.equal(p.width, VP.width - 16, 'narrowed to the viewport less its margins');
    assert.ok(p.x + p.width <= VP.width);
  });

  it('never places a box that is larger than the viewport at a negative x', () => {
    // A popover wider than the viewport: the clamp range is empty, and
    // `Math.min(Math.max(v, lo), hi)` with hi < lo returns lo. Without that
    // branch this returns a negative x and the menu is unreachable.
    const p = place({
      anchor: { x: 0, y: 0, width: 10, height: 10 },
      size: { width: 5000, height: 50 },
      viewport: VP,
      prefer: 'bottom'
    });
    assert.ok(p.x >= 0, `x=${p.x}`);
  });

  it('reports the opposite side for every side', () => {
    assert.equal(opposite('top'), 'bottom');
    assert.equal(opposite('bottom'), 'top');
    assert.equal(opposite('left'), 'right');
    assert.equal(opposite('right'), 'left');
  });

  it('checks the size as well as the position when deciding a fit', () => {
    // A position-only check says a 5000px-wide popover "fits" at x=0.
    const tooWide: Rect = { x: 0, y: 0, width: 5000, height: 10 };
    assert.equal(fits(tooWide, VP, 8), false);
    assert.equal(fits({ x: 0, y: 0, width: 100, height: 10 }, VP, 8), false, 'the margin still applies');
    assert.equal(fits({ x: 10, y: 10, width: 100, height: 10 }, VP, 8), true);
  });
});

describe('a context menu at the pointer', () => {
  it('opens into the larger half of the screen, on both axes', () => {
    // Each corner, and the two axes read independently -- a menu opened in the
    // bottom-LEFT quadrant goes LEFT as well as up, which is what collapsing
    // to a dominant axis gets wrong.
    assert.equal(sideForPoint({ x: 10, y: 10 }, { width: 1000, height: 800 }), 'up-left');
    assert.equal(sideForPoint({ x: 990, y: 10 }, { width: 1000, height: 800 }), 'up-right');
    assert.equal(sideForPoint({ x: 10, y: 790 }, { width: 1000, height: 800 }), 'down-left');
    assert.equal(sideForPoint({ x: 990, y: 790 }, { width: 1000, height: 800 }), 'down-right');
    // The middle of each edge, which is the case a corner test alone misses.
    assert.equal(sideForPoint({ x: 500, y: 5 }, { width: 1000, height: 800 }), 'up-right');
    assert.equal(sideForPoint({ x: 5, y: 400 }, { width: 1000, height: 800 }), 'down-left');
  });

  it('uses the vertical axis for the placement side', () => {
    // A menu is tall and a viewport is not: a menu that flipped up with 400 px
    // below it and 100 px above reads as broken even though it is on screen.
    const cases: [Corner, 'top' | 'bottom'][] = [
      ['up-left', 'top'],
      ['up-right', 'top'],
      ['down-left', 'bottom'],
      ['down-right', 'bottom']
    ];
    for (const [c, want] of cases) {
      assert.equal(sideForCorner(c, { width: 1000, height: 800 }), want, c);
    }
  });
});
