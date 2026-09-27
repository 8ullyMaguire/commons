/**
 * Parsing a paste into a multi-value field. Spec 10.10, plan T-P5-006 item 11;
 * #7139.
 *
 * # The problem this exists to solve
 *
 * A user copies a list of tags from somewhere — a spreadsheet column, a wiki
 * page, another tool's export — and pastes it into a multi-value box. The box
 * has to decide what the user meant, because the pasted text is *one string*
 * and the field wants *N values*.
 *
 * The decision has three parts, and each is a guess that can be wrong:
 *
 * 1. **What separates the values?** Newlines obviously. But a spreadsheet
 *    column copied to the clipboard arrives comma-separated in `text/plain`,
 *    and semicolons because that is what a European locale's CSV uses. Guessing
 *    wrong turns one tag called "Smith, John" into two.
 * 2. **Do the values already exist?** Pasting `A, B, C` where `B` is already in
 *    the field must not produce `B (2)`.
 * 3. **Is a pasted value an existing entity or a new one?** §9.4 is about tag
 *    organization, and the distinction matters: a name that matches a
 *    performer is that performer, and a name that matches nothing is a
 *    suggestion, not an error.
 *
 * # The rule that decides (1): a separator only counts inside a quoted field
 *
 * A CSV is the obvious answer and it is the wrong one for a *paste*, because a
 * CSV cell can contain its own delimiter and the paste carries no information
 * about which parts were quoted. Guessing "split on comma" breaks the common
 * case of a person's name.
 *
 * So the rule is: split on newlines first, and treat a comma or semicolon as a
 * separator only when the line has no unbalanced quote — a line that reads
 * `"Smith, John", Jr` is one value with an embedded comma, and a line that
 * reads `A, B, C` is three.
 *
 * This is a judgement call, and it is documented as one rather than presented
 * as a rule the format dictates. A user who pastes a genuine CSV with quoted
 * commas gets the quoted reading, which is the right default: the quoted form
 * is unambiguous and the unquoted form is not.
 *
 * Everything here is pure and has no DOM dependency, so the boundaries can be
 * tested exactly — see `tests/paste.test.ts`. The component holds the
 * `paste` event and nothing else.
 */

/** One pasted value and where it came from. */
export interface ParsedValue {
  readonly value: string;
  /** `true` if this was already in the field before the paste. */
  readonly duplicate: boolean;
  /**
   * The line the value came from, 1-indexed, for a message that says which row
   * is wrong. A parser that cannot point at a line cannot be argued with.
   */
  readonly line: number;
}

/** What a paste produced, and what was thrown away. */
export interface PasteResult {
  readonly values: readonly ParsedValue[];
  /** Lines that produced nothing — blank, or only separators. */
  readonly skipped: number;
  /** Values dropped because the field already had them. */
  readonly duplicates: number;
}

/** The characters that separate values, longest first so `;;` is not `;` twice. */
const SEPARATORS = [';', ','] as const;

/**
 * Does this line open a quote that it does not close?
 *
 * A double quote inside a quoted field is an escaped quote, so the scan counts
 * quotes and treats an odd count as open. This is the whole difference between
 * `"Smith, John", Jr` (one value) and `A, B` (two), and it is why the function
 * exists rather than a `.includes('"')` at the call site.
 */
export function hasOpenQuote(line: string): boolean {
  let inside = false;
  for (let i = 0; i < line.length; i += 1) {
    if (line[i] !== '"') continue;
    // `""` inside a quoted field is one escaped quote, not a close-then-open.
    if (inside && line[i + 1] === '"') {
      i += 1;
      continue;
    }
    inside = !inside;
  }
  return inside;
}

/** Strip one layer of surrounding quotes, and unescape `""`. */
export function unquote(value: string): string {
  const trimmed = value.trim();
  if (trimmed.length < 2) return trimmed;
  if (!trimmed.startsWith('"') || !trimmed.endsWith('"')) return trimmed;
  return trimmed.slice(1, -1).replace(/""/g, '"');
}

/**
 * Split one line into values.
 *
 * Newline-separated text reaches here one line at a time. A line with an open
 * quote is ONE value even if it contains separators, because the quote is the
 * only evidence available that the separator is data.
 */
export function splitLine(line: string): string[] {
  const trimmed = line.trim();
  if (trimmed === '') return [];
  if (hasOpenQuote(trimmed)) return [unquote(trimmed)];

  // A left-to-right scan that tracks the quote state, so a separator outside a
  // quoted run splits and one inside it does not. `"a,b", c` is two values;
  // `"a,b"` is one.
  //
  // The `current` buffer accumulates WITHOUT its quote characters. That is the
  // fix for `"A" B`, which the first version read as a single value with a
  // stray quote in it: a quote only protects a separator that sits next to it,
  // and once the closing quote has passed, a space is just a space. Keeping the
  // quotes in the buffer and stripping them at the end cannot tell `"A, B"`
  // (one value) from `"A" B` (two), because both have balanced quotes.
  const parts: string[] = [];
  let current = '';
  let inside = false;
  // Whether the part being built opened with a quote. Needed because the scan
  // strips quote characters as it goes, so a blank part's origin is not visible
  // from the buffer -- and a quoted blank is data while an unquoted one is not.
  let quoted = false;
  for (let i = 0; i < trimmed.length; i += 1) {
    const ch = trimmed[i]!;
    if (ch === '"') {
      if (inside && trimmed[i + 1] === '"') {
        // An escaped quote: one literal `"`, and still inside.
        current += '"';
        i += 1;
        continue;
      }
      if (!inside && current.trim() === '') quoted = true;
      inside = !inside;
      continue;
    }
    if (!inside && (ch === ';' || ch === ',')) {
      parts.push(quoted ? `"${current}"` : current);
      current = '';
      quoted = false;
      continue;
    }
    current += ch;
  }
  parts.push(quoted ? `"${current}"` : current);

  // Trimming after the split, not before: a quoted blank is data the user
  // asked for, and trimming first would reduce `" "` to the empty string and
  // then drop it. `unquote` is what turns a quoted part back into its value.
  return parts
    .map((raw) => {
      const value = raw.trim();
      return value.startsWith('"') ? unquote(value) : value;
    })
    .filter((v) => v !== '');
}

/**
 * Parse a whole paste.
 *
 * `existing` is what the field already holds, and it is a parameter rather than
 * read from the field so the function stays pure — and so a caller can ask
 * "what would this paste do?" without committing to it, which is what the
 * preview in the dialog needs.
 *
 * Case sensitivity is the caller's choice (`existing.some(e => e === v)`), and
 * the default here is exact: §9.4's tags are case-insensitive for *display* but
 * two tags differing only in case are two entities, and a parser that folded
 * them would silently merge them. Normalisation is a decision for the store,
 * and one that belongs in a place where the user can see it.
 */
export function parsePaste(text: string, existing: readonly string[] = []): PasteResult {
  const seen = new Set<string>(existing);
  const values: ParsedValue[] = [];
  let skipped = 0;
  let duplicates = 0;

  const lines = text.split(/\r\n|\r|\n/);
  for (const [index, line] of lines.entries()) {
    const parts = splitLine(line);
    if (parts.length === 0) {
      // A blank line is not a value and is not an error; counting it as a
      // value would put an empty string in a tag field, and counting it as
      // "skipped" is only useful if the number is shown to someone.
      if (line.trim() === '') skipped += 1;
      continue;
    }
    for (const part of parts) {
      const duplicate = seen.has(part);
      if (duplicate) {
        duplicates += 1;
        continue;
      }
      seen.add(part);
      values.push({ value: part, duplicate: false, line: index + 1 });
    }
  }

  return { values, skipped, duplicates };
}

/**
 * The one-line preview, before anything is committed.
 *
 * "Will add 3, 2 already there" is the sentence a user needs BEFORE the paste
 * lands, because a paste into a 400-tag field is not obviously reversible by
 * hand. The plural split is here because "will add 1 tags" is the kind of thing
 * that ships.
 */
export function preview(result: PasteResult): string {
  const { values, duplicates } = result;
  if (values.length === 0) {
    return duplicates > 0
      ? `Nothing new — all ${plural(duplicates, 'value')} already in the field.`
      : 'Nothing to paste.';
  }
  const add = `Will add ${plural(values.length, 'value')}`;
  if (duplicates === 0) return `${add}.`;
  return `${add}, ${plural(duplicates, 'value')} already in the field.`;
}

/** The count of values a paste would add, for a button label. */
export function addCount(result: PasteResult): number {
  return result.values.length;
}

function plural(n: number, word: string): string {
  return `${n} ${word}${n === 1 ? '' : 's'}`;
}
