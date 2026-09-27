/**
 * Per-field ignore lists for the tagger. Spec 10.10, plan T-P5-006 item 13;
 * #2318 and #2399.
 *
 * # What the two issues are
 *
 * #2318 — "Add ability to ignore more fields when using scene tagger."
 * #2399 — "Scene Tagger — add option to exclude specific metadata fields from
 *        search query."
 *
 * Both are the same mechanism at two different points, and the difference is
 * load-bearing rather than cosmetic:
 *
 *   - an **applied** ignore list decides what a scrape *writes*. A field the
 *     user has ignored does not get a proposal, so the tagger's "Accept all"
 *     cannot silently overwrite a field the user curates by hand.
 *   - a **search** ignore list decides what a *query* looks at. #2399 is about
 *     a scraper that searches a site by metadata, and a field excluded from
 *     the search changes the QUERY, not the result. Nothing is written.
 *
 * Conflating them produces the worst of both: a user who excludes a field from
 * the search silently stops getting its value, with nothing on screen saying
 * why. So they are two lists over one shared vocabulary, and they are never
 * merged.
 *
 * # The decisions this module makes
 *
 * 1. **What is a field name?** A bare string, because `FieldProposal.field` is
 *    one. But an *unvalidated* string means a typo -- `perfromer` -- produces an
 *    ignore list that appears to work and never matches. §2 validates the
 *    shape and warns about the ones that match nothing, rather than rejecting
 *    them, because a scraper with a field this module has not heard of is
 *    normal and must still be ignorable.
 * 2. **Case.** Field names are normalised to lower case on both sides. Two
 *    entries differing only in case would otherwise be one list that looks
 *    like two, and a user who cannot see why only one took effect will assume
 *    the feature is broken.
 * 3. **"All fields" is not a name.** It is a mode, held separately, because a
 *    list containing a magic string is a list that can be half-populated with
 *    it. See `Scope`.
 * 4. **Order does not matter, and the list is stored sorted**, so two users who
 *    build the same list by hand have the same list and a diff of two settings
 *    is readable.
 *
 * Everything here is pure. Nothing here persists; the tagger reads the result.
 */

import { KNOWN_TAGGER_FIELDS } from './tagger-fields.js';

// ---------------------------------------------------------------------------
// Vocabulary
// ---------------------------------------------------------------------------

/**
 * A field name, normalised.
 *
 * `normaliseField` is exported because every entry point has to go through it:
 * a name read from a URL, from a saved setting, or from a scraped proposal is
 * only the same field as another if it went through the same normalisation. One
 * place does that, and everything else calls it.
 */
export function normaliseField(field: string): string {
  return field.trim().toLowerCase();
}

/**
 * Whether a string is shaped like a field name.
 *
 * Deliberately permissive: letters, digits, `_`, `-` and `.`. A field named
 * `studio.alias` or `performer-fav` is plausible, and rejecting it would mean a
 * scraper with an unusual name could not be silenced. What is rejected is what
 * would break a query or a selector: whitespace inside, a `/`, a quote, and the
 * empty string.
 */
export function isValidFieldName(field: string): boolean {
  if (field === '') return false;
  return /^[a-z0-9_.-]+$/.test(normaliseField(field));
}

// ---------------------------------------------------------------------------
// Scope
// ---------------------------------------------------------------------------

/**
 * Which fields are being ignored, and what happens to a field not on the list.
 *
 * A `Set` would do, and a `Set` alone is the bug: with a set, "ignore nothing"
 * and "ignore everything" are both just an empty set, and a mode has to be
 * stored somewhere. `ignoreAll` makes "ignore every field except those listed"
 * expressible without a magic string in the list — which is what #2399 needs,
 * because the useful case there is "search on title and date, ignore the other
 * forty fields", and that is an allow-list wearing an ignore-list's clothes.
 */
export interface Scope {
  /** `true` means every field is ignored EXCEPT those in `fields`. */
  readonly ignoreAll: boolean;
  /** The explicit entries. Sorted and normalised; never contains duplicates. */
  readonly fields: readonly string[];
}

/** An empty scope: nothing is ignored. */
export const NO_IGNORES: Scope = { ignoreAll: false, fields: [] };

/** Every field ignored, with no exceptions. */
export function ignoreEverything(): Scope {
  return { ignoreAll: true, fields: [] };
}

/** Every field ignored except the named ones — an allow-list. */
export function ignoreAllExcept(fields: readonly string[]): Scope {
  return { ignoreAll: true, fields: normaliseAll(fields) };
}

/** Normalise, drop empties and duplicates, and sort. */
export function normaliseAll(fields: readonly string[]): string[] {
  const set = new Set<string>();
  for (const f of fields) {
    const n = normaliseField(f);
    if (n !== '') set.add(n);
  }
  return [...set].sort();
}

// ---------------------------------------------------------------------------
// The decision
// ---------------------------------------------------------------------------

/**
 * Is this field ignored under this scope?
 *
 * The whole feature, and it is three lines because the scope has already been
 * normalised. `ignoreAll` inverts the test, which is the only place the
 * inversion happens — a caller that gets this backwards would be a bug in
 * every caller at once rather than one.
 */
export function isIgnored(scope: Scope, field: string): boolean {
  const n = normaliseField(field);
  if (n === '') return false;
  return scope.ignoreAll ? !scope.fields.includes(n) : scope.fields.includes(n);
}

/** Every field of `all` that is ignored, in the order given. */
export function ignoredFields(scope: Scope, all: readonly string[]): string[] {
  return all.filter((f) => isIgnored(scope, f));
}

/** Every field of `all` that is NOT ignored, in the order given. */
export function keptFields(scope: Scope, all: readonly string[]): string[] {
  return all.filter((f) => !isIgnored(scope, f));
}

/**
 * Split a list of scraped proposals by whether their field is ignored.
 *
 * Both halves are returned, not just the kept one. A tagger that shows only the
 * kept proposals cannot tell the user that a field they expected was dropped —
 * and "why did my studio not come through" is the question this feature exists
 * to make answerable.
 */
export interface FieldSplit<T> {
  readonly kept: readonly T[];
  readonly ignored: readonly T[];
}

export function splitByField<T>(
  scope: Scope,
  items: readonly T[],
  fieldOf: (item: T) => string,
): FieldSplit<T> {
  const kept: T[] = [];
  const ignored: T[] = [];
  for (const item of items) {
    (isIgnored(scope, fieldOf(item)) ? ignored : kept).push(item);
  }
  return { kept, ignored };
}

// ---------------------------------------------------------------------------
// Applying and reading a scope
// ---------------------------------------------------------------------------

/**
 * Turn a list of field names into a scope, dropping the ones that are not valid
 * field names.
 *
 * Dropping rather than throwing is a judgement call. A settings file is user-
 * editable and a bad entry should cost the user that one entry, not the whole
 * setting; the reason it is dropped is reported by `rejectedFields` so a UI can
 * say so rather than silently losing it.
 */
export function scopeFrom(fields: readonly string[], ignoreAll = false): Scope {
  return {
    ignoreAll,
    fields: normaliseAll(fields.filter(isValidFieldName)),
  };
}

/** Entries of `fields` that `scopeFrom` would drop, unchanged, for reporting. */
export function rejectedFields(fields: readonly string[]): string[] {
  return fields.filter((f) => !isValidFieldName(f));
}

/**
 * The query string for a search whose scope is `scope`.
 *
 * #2399 is about the QUERY, so the scope has to be able to produce one. A
 * repeated `ignore=` parameter rather than a joined string, for the same reason
 * the tag route uses repeated `tag=`: a field name is not going to contain a
 * comma, but the separator a caller would choose if it were is exactly the
 * thing that eventually splits a setting in the wrong place.
 */
export function scopeToQuery(scope: Scope, all?: readonly string[]): string {
  if (!scope.ignoreAll) {
    return scope.fields.map((f) => `ignore=${encodeURIComponent(f)}`).join('&');
  }
  // Inverted: the query needs the KEEP list, because "ignore everything except
  // X, Y" cannot be expressed as an ignore list without a second parameter
  // meaning two things.
  //
  // With nothing kept, the query would be the empty string — which is exactly
  // what an EMPTY IGNORE LIST produces, and means the opposite thing: one means
  // "no filter at all" and the other means "ignore every field". Two settings
  // with opposite effects serialising to the same query is the kind of bug that
  // is invisible until someone's tagger starts returning nothing.
  //
  // So the two cases are spelled differently. `keep=` with no value after it is
  // the empty keep-list, which is distinct from omitting the parameter entirely.
  const keep = all === undefined ? scope.fields : keptFields(scope, all);
  const prefix = 'keep=';
  if (keep.length === 0) return prefix;
  return keep.map((f) => `${prefix}${encodeURIComponent(f)}`).join('&');
}

// ---------------------------------------------------------------------------
// Warnings
// ---------------------------------------------------------------------------

/**
 * Entries that match no field the tagger knows about.
 *
 * Reported, never removed. A scraper with a field this build has never seen is
 * a normal thing, and the entry is still doing its job — it is silencing that
 * field. Removing it would make the setting quietly wrong, and a setting that is
 * quietly wrong is worse than a setting that is loudly incomplete.
 */
export function unknownEntries(scope: Scope): string[] {
  const known = new Set(KNOWN_TAGGER_FIELDS.map(normaliseField));
  return scope.fields.filter((f) => !known.has(f));
}

/**
 * A sentence about a scope, for a settings row.
 *
 * Says how many fields and, when the list is short enough to be read, which
 * ones. §10.7's rule is that an action states its scope: a settings row that
 * says "3 fields ignored" tells a user nothing they can check.
 */
export function describeScope(scope: Scope, all?: readonly string[]): string {
  if (scope.ignoreAll) {
    const keep = all === undefined ? scope.fields : keptFields(scope, all);
    if (keep.length === 0) return 'Every field is ignored.';
    return `Every field is ignored except ${list(keep)}.`;
  }
  if (scope.fields.length === 0) return 'No fields are ignored.';
  return `${list(scope.fields)} ${scope.fields.length === 1 ? 'is' : 'are'} ignored.`;
}

/** Inline-join with a trailing "and", the way a sentence reads. */
function list(items: readonly string[]): string {
  if (items.length === 1) return items[0]!;
  if (items.length === 2) return `${items[0]} and ${items[1]}`;
  return `${items.slice(0, -1).join(', ')} and ${items[items.length - 1]}`;
}
