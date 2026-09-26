/*
  The command registry: one list, one scope stack, one resolver.

  T-P5-006 item 5. Spec 10.7; plan §T-P5-006 item 5; reasoning in
  docs/spec/t-p5-006-commands.md §3.1 and §3.3.

  # Why scopes, and not priorities

  The obvious model is a priority number per command: highest wins. It cannot
  express the case that actually happens. A lightbox is open, the user presses
  Escape, and both the lightbox and the app want it. Priority resolves this by
  accident — whoever was numbered higher wins — and the number is a global
  property of a command, so it has to be right for every combination of open
  surfaces, forever.

  Scopes nest instead. A scope is a frame on a stack; a command declares which
  frame it lives in; resolution walks from the top and takes the first frame
  with a binding for the key. "Who is on top" is then not a thing two
  components can disagree about, because there is one stack and both read it.

  The rule that makes it work, and that a priority model cannot express: **a
  scope either claims a key or it does not.** There is no "Escape, but only if
  the lightbox is dirty", because a conditional claim is a claim that sometimes
  does not fire, and that is how a modal becomes unclosable.

  # Why the same list backs the palette

  The palette filters this registry. A command that is not here cannot be found
  in the palette, and a command here cannot be missing from it — which is the
  only way to stop the two drifting apart. That drift is the actual content of
  #2542 and #6218: a shortcut that exists and is not discoverable, and a palette
  that lists things the keyboard cannot do.
 */

import {
  chordsEqual,
  formatChord,
  inTextField,
  normalizeKey,
  type Chord,
  type KeyLike
} from './keys.js';

/** A thing the user can do. */
export interface Command {
  /** Stable identity. The palette's selected index refers to this, not to a
   *  position — a list that reorders under the cursor selects the wrong command. */
  id: string;
  /** What the user reads. Also what the palette matches against. */
  title: string;
  /** A longer line, or `undefined`. The palette shows it; the keydown path
   *  never sees it. */
  detail?: string;
  /** Words the palette matches that are not in the title. Empty is normal. */
  keywords?: string[];
  /**
   * The frame this command lives in. `'app'` is the bottom frame and always
   * exists.
   */
  scope: string;
  /** The key that invokes it, or `null` for a command the palette can run and
   *  the keyboard cannot — which is a real state, not a stub: a command with no
   *  binding is discoverable and not mistypeable. */
  binding: Chord | null;
  /** Does invoking this need something selected? Read by the UI to disable a
   *  palette row; never read by the resolver, which must not decide whether a
   *  command is *allowed* — only which one the key names. */
  needsSelection?: boolean;
  /**
   * Resolved before the scope stack, whatever else is open.
   *
   * For the palette, and only for the palette. A palette you cannot open from
   * inside a modal is a palette that does not exist for half your users, and the
   * alternative — every surface re-binding `Ctrl+P` — is a thing that gets
   * forgotten in exactly one modal.
   *
   * It lives here, on the registration, rather than as a branch in the keydown
   * handler. The handler version recognised the chord and returned a magic
   * `'opened-palette'` outcome that the caller then discarded, so the keystroke
   * was correctly recognised, correctly prevented, and did nothing at all. A
   * special case the caller must remember to honour is a special case that will
   * not be honoured.
   */
  global?: boolean;
}

/** A frame on the scope stack. */
interface Frame {
  id: string;
  /** Commands this frame contributed, so popping removes exactly those. */
  owned: string[];
}

export type Outcome =
  /** A command ran. */
  | { kind: 'ran'; id: string }
  /** A key matched nothing in any frame. */
  | { kind: 'unbound' }
  /** A key was text, and stayed text. */
  | { kind: 'text' }
  /** A key matched a command that is currently disabled. */
  | { kind: 'disabled'; id: string }
  /** A key matched more than one command in the *same* frame. */
  | { kind: 'collision'; ids: string[] };

/**
 * The registry.
 *
 * A class rather than a module of functions because the palette and the
 * keydown handler must read the *same* list at the same time, and a pair of
 * module-level maps is a pair of things that can disagree.
 */
export class CommandRegistry {
  #commands = new Map<string, Command>();
  #stack: Frame[] = [{ id: 'app', owned: [] }];

  // ---------------------------------------------------------------- reading

  /** Every command, in registration order. */
  all(): Command[] {
    return [...this.#commands.values()];
  }

  /** Commands in the named frame. */
  inScope(scope: string): Command[] {
    return this.all().filter((c) => c.scope === scope);
  }

  /** The frame stack, bottom first. The e2e asserts against this. */
  scopes(): string[] {
    return this.#stack.map((f) => f.id);
  }

  get depth(): number {
    return this.#stack.length;
  }

  /**
   * Is this command currently on top of its own scope?
   *
   * A command in a frame that has been popped is not runnable, and this is how
   * that is expressed: the resolver does not consult the stack for membership
   * (the stack decides *which frame wins*, not whether a command exists), it
   * consults this.
   */
  #live(id: string): boolean {
    const c = this.#commands.get(id);
    if (c === undefined) return false;
    return this.#stack.some((f) => f.id === c.scope);
  }

  // ------------------------------------------------------------- registering

  /**
   * Add or replace a command.
   *
   * Replace-by-id rather than reject, because a component re-renders and
   * re-registers, and a registry that throws on the second registration of the
   * same id is a registry that only works on first mount.
   *
   * `pushScope` defaults to true for a command whose scope is not yet on the
   * stack: registering a command is how a surface declares that it exists, and
   * requiring the two to be done separately is a pairing that will be forgotten
   * in exactly one place.
   */
  register(command: Command, pushScope = true): void {
    if (pushScope && !this.#stack.some((f) => f.id === command.scope)) {
      this.#stack.push({ id: command.scope, owned: [] });
    }
    this.#commands.set(command.id, command);
    const frame = this.#stack.find((f) => f.id === command.scope);
    if (frame && !frame.owned.includes(command.id)) frame.owned.push(command.id);
  }

  /**
   * Pop the topmost frame, or the named one if it is on top.
   *
   * Named because a surface closing out of order — two modals, the older one
   * dismissed first — must not pop the wrong frame. Returns whether anything was
   * popped, so a caller can tell "I closed" from "I was not open".
   *
   * `app` is never popped: it is the floor, and a registry with no floor has
   * nothing to fall back to.
   */
  popScope(id?: string): boolean {
    if (this.#stack.length <= 1) return false;
    if (id === undefined) {
      this.#stack.pop();
      return true;
    }
    const top = this.#stack[this.#stack.length - 1];
    if (top.id !== id) return false;
    this.#stack.pop();
    return true;
  }

  // ---------------------------------------------------------------- resolving

  /**
   * Which command, if any, does this keystroke name?
   *
   * Returns an `Outcome` and never runs anything, so the caller decides what
   * running means — which is what lets the palette, the keydown handler, and the
   * e2e all ask the same question and get the same answer.
   *
   * A `disabled` outcome is returned rather than thrown: a command that needs a
   * selection and has none is a normal state, and the palette needs to show it
   * disabled rather than hide it.
   */
  resolve(e: KeyLike, selected = 0): Outcome {
    const chord = normalizeKey(e);
    if (chord === null) return { kind: 'unbound' };

    // Text wins over everything except a modifier chord. A bare letter in an
    // input is a letter; `Ctrl+K` in an input is still ours, because the user
    // cannot type a control character. See `inTextField`.
    if (inTextField(e.target) && chord.mods.length === 0) return { kind: 'text' };

    // Global bindings, before the stack. Deliberately a separate pass rather than
    // a synthetic bottom frame, because a frame on the stack can be popped and a
    // global command must not be poppable by a modal that happens to be named
    // `app`.
    for (const c of this.all()) {
      if (c.global !== true || c.binding == null) continue;
      if (!chordsEqual(c.binding, chord)) continue;
      return this.#finish(c, selected);
    }

    // Innermost frame first. `reverse()` over a small stack, so a copy is not
    // worth it.
    for (let i = this.#stack.length - 1; i >= 0; i -= 1) {
      const frame = this.#stack[i];
      const hits = this.inScope(frame.id).filter(
        (c) => c.binding != null && chordsEqual(c.binding as Chord, chord)
      );
      if (hits.length === 0) continue;
      if (hits.length > 1) return { kind: 'collision', ids: hits.map((c) => c.id) };
      const c = hits[0];
      if (!this.#live(c.id)) continue;
      return this.#finish(c, selected);
    }
    return { kind: 'unbound' };
  }

  /**
   * The liveness and gating checks, shared by the global pass and the stack
   * pass.
   *
   * Split out because the two passes must agree: a global command and a scoped
   * one that resolve the same way is the point, and two copies of three
   * conditions is two places for them to drift.
   */
  #finish(c: Command, selected: number): Outcome {
    if (c.needsSelection && selected === 0) return { kind: 'disabled', id: c.id };
    return { kind: 'ran', id: c.id };
  }

  /**
   * Reorder a step through a list. The #6218/#5587 shortcuts.
   *
   * A command rather than a handler, so it appears in the palette with its
   * binding and so "move selection down" is one implementation rather than one
   * per surface.
   *
   * The list is a *copy*: returning a new array rather than sorting in place is
   * what stops a reorder from being visible to a caller holding the old one, and
   * a selection reorder that mutates its input is a selection reorder that
   * changes the sort order when the input was a sorted view.
   */
  static reorder<T>(items: readonly T[], from: number, to: number): T[] {
    const out = [...items];
    if (from < 0 || from >= out.length) return out;
    const clamped = Math.max(0, Math.min(out.length - 1, to));
    const [moved] = out.splice(from, 1);
    out.splice(clamped, 0, moved);
    return out;
  }
}

/**
 * The palette's filter.
 *
 * Subsequence matching, not substring: a user typing "bte" means "bulk tag
 * edit", and a substring match finds nothing. Scored so the *title* matches
 * outrank a keyword match, because the title is what the user can see.
 *
 * Returns the matched commands and their scores, best first. A score of 0 is
 * dropped, so an unmatched command never appears as a result with no
 * explanation -- an empty palette with "no matches" is better than a palette
 * showing every command because the query was nonsense.
 */
export function searchCommands(
  commands: readonly Command[],
  query: string
): { command: Command; score: number }[] {
  const q = query.trim().toLowerCase();
  if (q === '') return commands.map((command) => ({ command, score: 1 }));

  const scored: { command: Command; score: number }[] = [];
  for (const command of commands) {
    const title = command.title.toLowerCase();
    const words = (command.keywords ?? []).map((k) => k.toLowerCase());

    let score = subsequenceScore(title, q);
    if (score > 0) {
      // A title match is the user finding what they can see. A keyword match is
      // the user finding something they cannot, so it counts for less.
      score += 10;
    } else {
      for (const w of words) {
        const s = subsequenceScore(w, q);
        if (s > 0) {
          score = Math.max(score, s);
          break;
        }
      }
    }
    if (score > 0) scored.push({ command, score });
  }
  return scored.sort((a, b) => {
    if (b.score !== a.score) return b.score - a.score;
    // A tie is not a neutral outcome. For `bt`, "Bulk tag selected" and "Bring
    // the tagger up" both match on two word-initial letters and score
    // identically, and alphabetical order hands the user "Bring" -- which is
    // neither more likely nor more relevant.
    //
    // The user who typed `bt` typed the initials of a command, and the shortest
    // command whose words start with those letters is the one they meant:
    // "bulk tag" is two words, "bring the tagger up" is four. Fewer words wins,
    // then the shorter title, then alphabetical purely so the order is stable
    // and a test can assert it.
    const aw = wordsOf(a.command.title);
    const bw = wordsOf(b.command.title);
    if (aw !== bw) return aw - bw;
    if (a.command.title.length !== b.command.title.length) {
      return a.command.title.length - b.command.title.length;
    }
    return a.command.title.localeCompare(b.command.title);
  });
}

/** How many words a title has. The tiebreak prefers fewer. */
function wordsOf(title: string): number {
  return title.trim().split(/[\s\-_/]+/).filter((w) => w !== '').length;
}

/**
 * How well does `q` match `text` as a subsequence?
 *
 * Higher is better; 0 is no match. A prefix scores highest, then a match that
 * starts at a word boundary, then a contiguous run, then plain subsequence --
 * so `bt` ranks "Bulk tag" above "Bring the tagger up".
 *
 * Word boundaries matter more than they look: a user typing `bt` wants the
 * command whose *words* start with b and t, not one that happens to contain
 * those letters somewhere.
 */
function subsequenceScore(text: string, q: string): number {
  if (q.length === 0) return 1;
  if (text.startsWith(q)) return 100;
  if (text.includes(q)) return 60;

  let ti = 0;
  let score = 0;
  // A hit that starts a word is worth more than one in the middle of a word,
  // and this is what separates "Bulk tag selected" from "Bring the tagger up"
  // for the query `bt`. The counter has to be a *count* and not a boolean: with
  // a boolean, both candidates scored identically and the tie fell through to
  // alphabetical order, so the wrong command ranked first.
  let boundaryHits = 0;
  for (const ch of q) {
    const at = text.indexOf(ch, ti);
    if (at < 0) return 0;
    if (at === 0 || /[\s\-_/]/.test(text[at - 1] ?? '')) boundaryHits += 1;
    // A contiguous run is better than a scattered one.
    if (at === ti) score += 2;
    ti = at + 1;
  }
  // Weighted well above the run bonus so it can never be outvoted: a user
  // typing `bt` means "bulk tag", and both of those are word-initial, while
  // the `t` in "tagger" is not.
  return 20 + boundaryHits * 10 + score;
}

/**
 * The display string for a command's binding, or `''` when it has none.
 *
 * `== null` and not `=== null`: `binding` is optional, so "no binding" arrives
 * as `undefined` from a command that omits it and as `null` from one that sets
 * it explicitly. A strict check catches the second and misses the first, and
 * the miss is a `TypeError` inside the palette's row loop — one command with no
 * binding takes the whole palette down, and only for a user who typed a query
 * that surfaced it.
 */
export function bindingLabel(c: Command): string {
  return c.binding == null ? '' : formatChord(c.binding);
}
