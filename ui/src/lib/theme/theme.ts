/**
 * Which theme applies, and where that choice is stored.
 *
 * T-P5-007, spec §10.8. The three-way choice (system / light / dark) and the
 * rule for resolving it.
 *
 * # Why this is not a Svelte component
 *
 * The tempting implementation is a `$state` in the app shell plus an effect
 * that writes `document.documentElement.dataset.theme`. That works, and it
 * moves the one decision worth testing — *which theme applies* — into the one
 * place it cannot be tested without a DOM. The precedence below is the whole
 * feature, and it is a function of three inputs.
 *
 * So the decision is here and the shell applies it. The one thing that needs a
 * DOM is `applyTo()`, at the bottom, and it is four lines.
 *
 * # The precedence, and why the OS is the BASE and not an override
 *
 *   explicit user choice  >  OS preference  >  dark
 *
 * The order matters in one direction only. A user who set light and is on a
 * dark OS must get light: their choice is about this app, the OS setting is
 * about everything, and a person who has said "light" once and been shown
 * dark anyway will conclude the toggle is broken. So the stored choice wins.
 *
 * Dark is the base rather than light because the app shipped dark and an
 * operator upgrading should not get a white flash. That is a real cost --
 * light is the more common OS default -- and it is paid knowingly.
 */

import { DARK, LIGHT, type ThemeTokens } from './contrast.js';

/** What the user picked. Not the same as what is showing. */
export type ThemeChoice = 'system' | 'light' | 'dark';

/** What is actually painted. */
export type ResolvedTheme = 'light' | 'dark';

/** The three choices, in the order a settings UI lists them. */
export const CHOICES: readonly ThemeChoice[] = ['system', 'light', 'dark'];

/** The fallback when nothing is known: the OS is assumed dark-capable. */
export const DEFAULT_CHOICE: ThemeChoice = 'system';

/** Where the choice is kept. Namespaced, and versioned by the key. */
export const STORAGE_KEY = 'commons.theme';

/** The attribute on `<html>` that the CSS keys off. */
export const THEME_ATTR = 'data-theme';

export function isThemeChoice(v: unknown): v is ThemeChoice {
  return v === 'system' || v === 'light' || v === 'dark';
}

/**
 * Resolve a choice against the OS preference.
 *
 * `osDark` is a parameter, not a read of `matchMedia`, so the whole decision
 * is testable. A caller reads the media query and passes the result.
 */
export function resolve(choice: ThemeChoice, osDark: boolean): ResolvedTheme {
  if (choice === 'light') return 'light';
  if (choice === 'dark') return 'dark';
  return osDark ? 'dark' : 'light';
}

/** The token set for a resolved theme. */
export function tokensFor(resolved: ResolvedTheme): ThemeTokens {
  return resolved === 'light' ? LIGHT : DARK;
}

/**
 * The minimal storage surface this module needs.
 *
 * Declared rather than reached for globally so the tests need no `localStorage`
 * shim, and so a caller that wants the choice in a cookie or a profile can
 * pass that instead. `localThemeStorage()` at the bottom is the browser one.
 */
export interface ThemeStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

/**
 * Read the stored choice.
 *
 * A stored value that is not one of the three is treated as absent rather than
 * thrown on. The value came from a previous version of the app or from someone
 * editing local storage, and refusing to boot because of it is a worse failure
 * than falling back to the OS.
 */
export function readChoice(store: ThemeStorage | null | undefined): ThemeChoice {
  if (!store) return DEFAULT_CHOICE;
  let raw: string | null;
  try {
    raw = store.getItem(STORAGE_KEY);
  } catch {
    // Safari in private mode throws on localStorage access. A theme is not
    // worth a failed render, so this is a fallback rather than an error.
    return DEFAULT_CHOICE;
  }
  return isThemeChoice(raw) ? raw : DEFAULT_CHOICE;
}

/** Persist a choice, clearing the key when it is back to `system`. */
export function writeChoice(store: ThemeStorage | null | undefined, choice: ThemeChoice): void {
  if (!store) return;
  try {
    if (choice === DEFAULT_CHOICE) store.removeItem(STORAGE_KEY);
    else store.setItem(STORAGE_KEY, choice);
  } catch {
    // Same reasoning as `readChoice`: a full or unavailable store degrades to
    // "this session's choice only", which is a working app.
  }
}

/**
 * The initial theme, read from the document.
 *
 * There is a reason this is `auto` and not an explicit value in the markup: a
 * page that hardcodes `data-theme="dark"` and then corrects itself after
 * hydration shows a light flash to every user whose choice is light. The
 * inline script in the shell applies the resolved theme before first paint,
 * and this is how it reads what it should apply.
 */
export interface ThemeDocument {
  documentElement: {
    getAttribute(name: string): string | null;
    setAttribute(name: string, value: string): void;
  };
}

export function currentChoice(doc: ThemeDocument | null | undefined): ThemeChoice {
  const attr = doc?.documentElement.getAttribute(THEME_ATTR);
  return isThemeChoice(attr) ? attr : DEFAULT_CHOICE;
}

/**
 * Read the OS preference.
 *
 * `true` when the query cannot be asked, because a browser that cannot answer
 * is far more likely to be light (an old engine, a print context) than dark,
 * and the previous behaviour was dark-by-default.
 */
export function osPrefersDark(matchMedia: ((q: string) => { matches: boolean }) | null | undefined): boolean {
  if (!matchMedia) return true;
  try {
    return matchMedia('(prefers-color-scheme: dark)').matches;
  } catch {
    return true;
  }
}

/**
 * A media-query LISTENER that also tolerates an engine without
 * `addEventListener` on the query object.
 *
 * Safari < 14 only has `addListener`. A theme toggle that stops following the
 * OS on an old Safari is a small thing; a theme toggle that THROWS on mount is
 * a white screen, so the unsupported path returns a no-op unsubscribe rather
 * than being absent.
 */
export function watchSystem(
  mq: {
    matches: boolean;
    addEventListener?: (t: string, cb: () => void) => void;
    removeEventListener?: (t: string, cb: () => void) => void;
    addListener?: (cb: () => void) => void;
    removeListener?: (cb: () => void) => void;
  } | null | undefined,
  cb: () => void
): () => void {
  if (!mq) return () => {};
  if (typeof mq.addEventListener === 'function') {
    mq.addEventListener('change', cb);
    return () => mq.removeEventListener?.('change', cb);
  }
  if (typeof mq.addListener === 'function') {
    mq.addListener(cb);
    return () => mq.removeListener?.(cb);
  }
  return () => {};
}

/**
 * Write the resolved theme onto the document.
 *
 * The only DOM in the module, and it is three lines because everything that
 * could be decided has already been decided. The attribute is set even for
 * `system`, so the CSS has one selector to match and no `:not()` to get wrong.
 */
export function applyTo(doc: ThemeDocument | null | undefined, resolved: ResolvedTheme): void {
  doc?.documentElement.setAttribute(THEME_ATTR, resolved);
}
