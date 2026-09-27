/**
 * The live theme, as a Svelte store.
 *
 * T-P5-007, spec §10.8. The `.svelte.ts` extension is not decoration: Svelte's
 * compiler only treats `$state` in a *Svelte module* as reactive, so this in a
 * plain `.ts` would compile, build, and report `$state is not defined` at
 * runtime. The store would then never update and the toggle would be inert.
 */

import {
  applyTo,
  currentChoice,
  isThemeChoice,
  osPrefersDark,
  readChoice,
  resolve,
  watchSystem,
  writeChoice,
  type ResolvedTheme,
  type ThemeChoice
} from './theme.js';

/** The `localStorage` surface, or null where there is none (SSR, private mode). */
function store(): Storage | null {
  try {
    return typeof localStorage === 'undefined' ? null : localStorage;
  } catch {
    return null;
  }
}

class ThemeStore {
  /** What the user picked. The thing a settings UI binds to. */
  #choice = $state<ThemeChoice>('system');

  /** What is painted. Derived, and the only thing the DOM reads. */
  #resolved = $state<ResolvedTheme>('dark');

  #stop: (() => void) | null = null;

  /**
   * Read the stored choice and the OS, and start following the OS.
   *
   * Called once from the shell. The document already has `data-theme` set by
   * the inline script in `app.html` -- this adopts that rather than
   * recomputing it, so a value applied before first paint is the value the
   * store holds even if the two disagree about the OS.
   */
  init(): void {
    const doc = typeof document === 'undefined' ? null : document;
    const pre = currentChoice(doc as never);
    // The attribute is a RESOLVED theme, not a choice: the inline script
    // resolves it. So reading it back gives "light" or "dark" and tells us
    // nothing about whether the user chose it. The choice comes from storage
    // and the resolved value from the document, and each is read from the
    // place that actually knows it.
    this.#choice = readChoice(store());
    this.#resolved = (pre === 'system' ? 'dark' : pre) as ResolvedTheme;
    this.#apply();

    if (typeof window === 'undefined') return;
    this.#stop?.();
    const mq =
      typeof window.matchMedia === 'function'
        ? window.matchMedia('(prefers-color-scheme: dark)')
        : null;
    this.#stop = watchSystem(mq as never, () => {
      this.#apply();
    });
  }

  /** Detach the OS listener. The shell calls it on destroy. */
  destroy(): void {
    this.#stop?.();
    this.#stop = null;
  }

  #apply(): void {
    const dark = osPrefersDark(
      typeof window !== 'undefined' && typeof window.matchMedia === 'function'
        ? (q: string) => window.matchMedia(q)
        : null
    );
    this.#resolved = resolve(this.#choice, dark);
    if (typeof document !== 'undefined') applyTo(document as never, this.#resolved);
  }

  /** Set the choice, persist it, and repaint. */
  set(choice: unknown): void {
    if (!isThemeChoice(choice)) return;
    this.#choice = choice;
    writeChoice(store(), choice);
    this.#apply();
  }

  /** What the user picked. */
  get choice(): ThemeChoice {
    return this.#choice;
  }

  /** What is painted. */
  get resolved(): ResolvedTheme {
    return this.#resolved;
  }

  /** Step to the next choice, for a single-button toggle. */
  cycle(): void {
    const order: ThemeChoice[] = ['system', 'light', 'dark'];
    const next = order[(order.indexOf(this.#choice) + 1) % order.length];
    this.set(next);
  }
}

/**
 * One store for the app.
 *
 * Module-level rather than created in the shell, so the settings control and
 * the shell read the SAME instance. Two stores means a control that shows
 * "system" while the page is light, which is the sort of bug that survives
 * because each half works.
 */
export const theme = new ThemeStore();
