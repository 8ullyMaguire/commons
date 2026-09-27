/**
 * Importing a CSV file into a multi-value field. Spec 10.10, plan T-P5-006
 * item 12; #1296 and #431.
 *
 * # Why this is not `paste.ts` with a different splitter
 *
 * `paste.ts` splits on newlines first and treats a line as a row, and that is
 * right for a paste: a spreadsheet column copied to the clipboard has one value
 * per line. A CSV is a *table*, and RFC 4180 allows a newline **inside** a
 * quoted field. An export from Postgres, MySQL or Excel puts a long description
 * in exactly such a field. A parser that reads line by line turns one value
 * into three and attributes the pieces to three different rows.
 *
 * So this module has its own state machine. It does reuse `ParsedValue`,
 * `preview` and `addCount` from `paste.ts`, which is the point: a CSV import and
 * a paste produce the *same shape*, so the preview and the Apply button in
 * `MultiValueField` work for both with no second code path in the component.
 *
 * # The decisions the CSV format does not make
 *
 * 1. **Delimiter** — sniffed, overridable, and reported. §2 of the format
 *    defines comma; the files users actually have come from European locales
 *    (semicolon) and from Excel (tab). See `sniffDelimiter`.
 * 2. **Header** — the caller's choice, never a guess. A column of tags whose
 *    first entry happens to be `name` is entirely plausible, and a parser that
 *    guessed would silently drop a tag. See `CsvImportOptions.hasHeader`.
 * 3. **Which column** — named or indexed, and the names are returned so a UI
 *    can offer the choice rather than making it.
 * 4. **Encoding** — BOM-detected, including the UTF-16 case with no BOM, which
 *    is what Windows tools write and which reads as NUL-interleaved garbage if
 *    assumed to be UTF-8. See `decodeCsvBytes`.
 * 5. **Malformed input** — split by severity. A truncated file (an unterminated
 *    quote) is an *error* because the last field swallowed the rest of the file
 *    and the user has lost data without being told. A stray quote after a
 *    closing quote is a *warning*: the value is recoverable and refusing the
 *    whole import over it is worse than importing it with a note.
 *
 * Everything here is pure and DOM-free, so each boundary is testable exactly.
 * See `tests/csv.test.ts`.
 */

import { type ParsedValue } from './paste.js';

// ---------------------------------------------------------------------------
// Encoding
// ---------------------------------------------------------------------------

export interface DecodedCsv {
  readonly text: string;
  /**
   * What was actually decoded. Reported rather than assumed, because "it
   * rendered as garbage" is a bug report and "it was UTF-16" is a fix.
   */
  readonly encoding: 'utf-8' | 'utf-16le' | 'utf-16be';
  /** A BOM was present and stripped. */
  readonly hadBom: boolean;
}

/**
 * Decode CSV bytes, detecting a byte-order mark and the BOM-less UTF-16 case.
 *
 * A BOM is the only unambiguous signal, so it is checked first and in full. The
 * NUL-position heuristic is the fallback for the files that have no BOM: UTF-16
 * ASCII text is `h\0i\0`, so the NULs land on odd byte offsets for little-endian
 * and even ones for big-endian. It is a heuristic and says so — it is only
 * consulted when the bytes are not valid UTF-8 in the first place, which is
 * exactly the case where the alternative is mojibake.
 */
export function decodeCsvBytes(bytes: Uint8Array): DecodedCsv {
  if (startsWith(bytes, [0xef, 0xbb, 0xbf])) {
    return { text: decode(bytes.subarray(3), 'utf-8'), encoding: 'utf-8', hadBom: true };
  }
  if (startsWith(bytes, [0xff, 0xfe])) {
    return { text: decode(bytes.subarray(2), 'utf-16le'), encoding: 'utf-16le', hadBom: true };
  }
  if (startsWith(bytes, [0xfe, 0xff])) {
    return { text: decode(bytes.subarray(2), 'utf-16be'), encoding: 'utf-16be', hadBom: true };
  }

  // No BOM. If the head has no NUL bytes, it is UTF-8 and the heuristic must
  // not fire.
  //
  // The test is for NUL, NOT for U+FFFD. NUL is a perfectly valid UTF-8
  // character, so a UTF-16 file decoded as UTF-8 comes out as
  // "a\0b\0c\0" with no replacement character at all -- the replacement-character
  // check passes, the heuristic below it is unreachable, and the file imports as
  // mojibake. Asking "is this valid UTF-8" cannot distinguish the two encodings
  // for ASCII text; asking "are there NULs where text should be" can, because
  // UTF-16 code units for ASCII are NUL-padded.
  const head = bytes.subarray(0, Math.min(bytes.length, 512));
  if (countNulsAt(head, 0) === 0 && countNulsAt(head, 1) === 0) {
    return { text: decode(bytes, 'utf-8'), encoding: 'utf-8', hadBom: false };
  }

  const odd = countNulsAt(head, 1);
  const even = countNulsAt(head, 0);
  if (odd > even) {
    return { text: decode(bytes, 'utf-16le'), encoding: 'utf-16le', hadBom: false };
  }
  if (even > 0) {
    return { text: decode(bytes, 'utf-16be'), encoding: 'utf-16be', hadBom: false };
  }
  return { text: decode(bytes, 'utf-8'), encoding: 'utf-8', hadBom: false };
}

function startsWith(haystack: Uint8Array, needle: readonly number[]): boolean {
  if (haystack.length < needle.length) return false;
  return needle.every((b, i) => haystack[i] === b);
}

function countNulsAt(bytes: Uint8Array, parity: 0 | 1): number {
  let n = 0;
  for (let i = parity; i < Math.min(bytes.length, 512); i += 2) {
    if (bytes[i] === 0) n += 1;
  }
  return n;
}

function decode(bytes: Uint8Array, encoding: 'utf-8' | 'utf-16le' | 'utf-16be'): string {
  return new TextDecoder(encoding).decode(bytes);
}

// ---------------------------------------------------------------------------
// Delimiter sniffing
// ---------------------------------------------------------------------------

/**
 * The delimiters worth trying, in tie-break order.
 *
 * The order IS the tie-break policy and it is deliberate: comma first, because
 * it is the format's own default and a file that uses both equally is far more
 * likely to be a comma file with a semicolon in a value than the reverse. Tab
 * is last because a tab inside a free-text value is more common than a pipe.
 */
const CANDIDATE_DELIMITERS = [',', ';', '\t', '|'] as const;

/** How many leading lines to look at. Ten covers a header plus a sample. */
const SNIFF_LINES = 10;

/**
 * Guess the delimiter.
 *
 * The count is taken **outside quotes** — a comma inside a quoted description
 * is data, and a sniffer that counts it will report the wrong delimiter for
 * exactly the files that needed the sniffer. Delimiters inside a quoted field
 * are invisible to this scan by construction, which is correct: they are
 * invisible to the parser too.
 *
 * A ragged file (rows with differing column counts) is normal in exports, so
 * the score is the **modal** count across the sampled lines rather than the
 * first or the total. A single malformed row then cannot flip the answer.
 */
export function sniffDelimiter(text: string): string {
  const lines = text.split(/\r\n|\r|\n/).filter((l) => l.trim() !== '').slice(0, SNIFF_LINES);
  if (lines.length === 0) return ',';

  let best = ',';
  let bestScore = -1;
  for (const candidate of CANDIDATE_DELIMITERS) {
    const counts = lines.map((line) => countOutsideQuotes(line, candidate));
    const nonZero = counts.filter((c) => c > 0);
    if (nonZero.length === 0) continue;

    // Modal count: the value that appears most often, ties broken toward more
    // lines agreeing. A delimiter that splits only one of ten lines is noise.
    const tally = new Map<number, number>();
    for (const c of nonZero) tally.set(c, (tally.get(c) ?? 0) + 1);
    let mode = 0;
    let modeSeen = 0;
    for (const [count, seen] of tally) {
      if (seen > modeSeen || (seen === modeSeen && count > mode)) {
        mode = count;
        modeSeen = seen;
      }
    }
    // Agreement is the score: how many lines carry the modal count. Preferring
    // agreement over magnitude means a single row with six commas cannot beat
    // nine rows with one.
    if (modeSeen > bestScore) {
      bestScore = modeSeen;
      best = candidate;
    }
  }
  return best;
}

/** Count occurrences of `needle` that are not inside a quoted run. */
function countOutsideQuotes(line: string, needle: string): number {
  let inside = false;
  let n = 0;
  for (let i = 0; i < line.length; i += 1) {
    if (line[i] === '"') {
      if (inside && line[i + 1] === '"') {
        i += 1;
        continue;
      }
      inside = !inside;
      continue;
    }
    if (!inside && line.startsWith(needle, i)) {
      n += 1;
      i += needle.length - 1;
    }
  }
  return n;
}

// ---------------------------------------------------------------------------
// The parser
// ---------------------------------------------------------------------------

export interface CsvCell {
  readonly value: string;
  /**
   * 1-indexed source line this cell **starts** on. Not where it ends, because
   * a message about a bad value should point at where the user would look for
   * it in their editor, which is the start.
   */
  readonly line: number;
  /** `true` when the cell was quoted. A quoted blank is data; see below. */
  readonly quoted: boolean;
}

export interface CsvRow {
  readonly cells: readonly CsvCell[];
  /** 1-indexed source line this row starts on. */
  readonly line: number;
}

export interface CsvWarning {
  readonly line: number;
  readonly message: string;
}

export interface CsvParse {
  readonly rows: readonly CsvRow[];
  readonly delimiter: string;
  /**
   * Set when a quoted field was never closed. This is the one hard error: the
   * final field has swallowed the remainder of the file, so `rows` is
   * incomplete and the caller must say so rather than import a partial file as
   * if it were whole.
   */
  readonly truncatedAtLine: number | null;
  readonly warnings: readonly CsvWarning[];
  /**
   * Blank lines that were dropped as gaps.
   *
   * They are not rows -- `a\n\nb` is two values -- but they are also not
   * nothing. A file with 40 blank lines in it is a file the user probably wants
   * to know about, and a count of 0 for "we discarded 40 lines" would be a lie.
   * They are reported separately from `CsvImport.skipped`, which counts rows
   * that existed and held no value, because the two mean different things to
   * the person who exported the file.
   */
  readonly blankLines: number;
}

/**
 * Parse CSV text into rows and cells.
 *
 * A field is quoted only if a `"` is its **first** character. Anywhere else a
 * quote is literal data, which is the lenient reading and the right one for
 * files people edited by hand: `He said "hi", ok` is two cells, not a syntax
 * error the user has to go and fix before their tags will import.
 */
export function parseCsv(text: string, delimiter?: string | null): CsvParse {
  const delim = delimiter && delimiter !== '' ? delimiter : sniffDelimiter(text);
  const rows: CsvRow[] = [];
  const warnings: CsvWarning[] = [];
  let truncatedAtLine: number | null = null;
  let blankLines = 0;

  let cells: CsvCell[] = [];
  let current = '';
  let cellQuoted = false;
  let inQuotes = false;
  let fieldStarted = false;
  // Whether the current field began with a quote. A quote appearing later is
  // data, and must not be able to open a quoted run.
  let openedByQuote = false;
  let cellLine = 1;
  let rowLine = 1;
  let line = 1;

  const endField = (): void => {
    cells.push({ value: current, line: cellLine, quoted: openedByQuote || cellQuoted });
    current = '';
    cellQuoted = false;
    openedByQuote = false;
    fieldStarted = false;
  };

  const endRow = (isBlank: boolean): void => {
    // A row that is a single empty cell is a BLANK LINE, not a row with one
    // empty value. Spreadsheets and text editors both leave blank lines in the
    // middle of an export, and `a\n\nb` is two values, not three -- the middle
    // "row" is a gap in the file, and counting it as a row is how a 500-row
    // export reports 503 rows.
    //
    // The quoted-empty case is excluded: `""` is a value, and it is the one
    // empty cell that is data. The check has to happen BEFORE `endField` clears
    // the flags, which is why it is computed here rather than after the push.
    //
    // The discard also has to CLEAR the pending cells. Skipping the push while
    // leaving `cells` alone does not drop the blank row -- it merges the next
    // real row into the previous one, so `a\n\nb` came out as one row with two
    // cells, `a` and `b`, and `a` looked like it had swallowed a column. That
    // is how the blank line became invisible AND the data wrong, and it is
    // invisible in a test that only counts rows.
    const isGap = isBlank && current === '' && !openedByQuote && !cellQuoted;

    endField();
    if (isGap && rows.length > 0) {
      cells = [];
      blankLines += 1;
      rowLine = line + 1;
      return;
    }
    rows.push({ cells, line: rowLine });
    cells = [];
    rowLine = line + 1;
  };

  for (let i = 0; i < text.length; i += 1) {
    const ch = text[i]!;

    if (inQuotes) {
      if (ch === '"') {
        if (text[i + 1] === '"') {
          current += '"';
          i += 1;
          continue;
        }
        inQuotes = false;
        continue;
      }
      if (ch === '\r') {
        // A newline inside a quoted field is DATA, and inside a quoted field a
        // CRLF is one newline, not two. This is the whole reason the parser is
        // a state machine rather than a line split.
        if (text[i + 1] === '\n') i += 1;
        current += '\n';
        line += 1;
        continue;
      }
      if (ch === '\n') {
        current += '\n';
        line += 1;
        continue;
      }
      current += ch;
      continue;
    }

    if (!fieldStarted) {
      // First character of a field.
      if (ch === '"') {
        inQuotes = true;
        openedByQuote = true;
        fieldStarted = true;
        continue;
      }
      fieldStarted = true;
      if (ch === delim) {
        endField();
        cellLine = line;
        continue;
      }
      if (ch === '\r' || ch === '\n') {
        if (ch === '\r' && text[i + 1] === '\n') i += 1;
        endRow(current.trim() === '');
        line += 1;
        cellLine = line;
        rowLine = line;
        continue;
      }
      current += ch;
      continue;
    }

    // Mid-field, outside quotes.
    if (text.startsWith(delim, i)) {
      endField();
      cellLine = line;
      i += delim.length - 1;
      continue;
    }
    if (ch === '\r' || ch === '\n') {
      if (ch === '\r' && text[i + 1] === '\n') i += 1;
      endRow(current.trim() === '');
      line += 1;
      cellLine = line;
      rowLine = line;
      continue;
    }
    if (ch === '"') {
      // A quote after a closing one, or in the middle of a bare field. The
      // value is recoverable, so this is a warning and not a rejection.
      cellQuoted = true;
      warnings.push({
        line,
        message: `stray quote in an unquoted field; kept as data (row ${rowLine})`,
      });
    }
    current += ch;
  }

  if (inQuotes) {
    // The file ended inside a quoted field. The field is still pushed, because
    // discarding it would return ZERO rows for a file whose first rows parsed
    // fine -- and the caller is told `truncatedAtLine` so it can refuse the
    // import anyway. A file that reads as empty and a file that reads as
    // complete-but-wrong are very different bug reports.
    truncatedAtLine = cellLine;
    endRow(false);
  } else if (fieldStarted || current !== '' || cells.length > 0) {
    // A final row with no trailing newline.
    endRow(false);
  }

  return { rows, delimiter: delim, truncatedAtLine, warnings, blankLines };
}

// ---------------------------------------------------------------------------
// Import
// ---------------------------------------------------------------------------

export interface CsvImportOptions {
  /** `null` or omitted sniffs. Pass a string to override. */
  readonly delimiter?: string | null;
  /**
   * Whether row 1 holds column names. The caller's decision, never a guess: a
   * column of tags whose first tag is `name` is a real possibility, and a
   * parser that guessed would drop a tag and call the import clean.
   */
  readonly hasHeader?: boolean;
  /** Which column holds the values, by name or by index. Defaults to the first. */
  readonly column?: string | number;
  /** What the field already holds, for duplicate detection. */
  readonly existing?: readonly string[];
  /**
   * Trim surrounding whitespace from every value. Off by default.
   *
   * RFC 4180 says a space after a comma is part of the field, and that is
   * correct for a general CSV reader -- trimming silently changes data. It is
   * wrong for a *tag* import, where " ok" is a typo the user made in the
   * spreadsheet and would want fixed rather than stored.
   *
   * So it is a choice, and the choice belongs to the caller: a tool importing
   * free text keeps the space, and this component turns it on because a tag
   * with a leading space cannot be found again.
   */
  readonly trimValues?: boolean;
}

export interface CsvImport {
  /** In the same shape a paste produces, so one preview serves both. */
  readonly values: readonly ParsedValue[];
  readonly skipped: number;
  readonly duplicates: number;
  /** Column names from the header row, for a UI to offer the choice. */
  readonly columnNames: readonly string[];
  readonly delimiter: string;
  readonly truncatedAtLine: number | null;
  readonly warnings: readonly CsvWarning[];
  /** Data rows, excluding the header when one was declared. */
  readonly rowCount: number;
  /** Blank lines dropped from the file. See `CsvParse.blankLines`. */
  readonly blankLines: number;
}

/**
 * Import a CSV file into values.
 *
 * The duplicate rule matches `parsePaste` exactly — exact match, no case
 * folding — so that pasting and importing the same list of names cannot
 * produce two different sets. That consistency is the reason the two paths
 * share `ParsedValue` rather than having their own result types.
 */
export function importCsv(text: string, options: CsvImportOptions = {}): CsvImport {
  const parsed = parseCsv(text, options.delimiter ?? null);
  const { rows, delimiter, truncatedAtLine, warnings, blankLines } = parsed;

  const hasHeader = options.hasHeader === true;
  const headerRow = hasHeader ? rows[0] : undefined;
  const columnNames = headerRow ? headerRow.cells.map((c) => c.value) : [];
  const dataRows = hasHeader ? rows.slice(1) : rows;

  // Column selection: by name when asked, by index otherwise, first as the
  // default. An out-of-range index is an empty import rather than a throw, so
  // that a UI can offer every column and a stale choice cannot crash the import.
  let index = 0;
  if (typeof options.column === 'number') {
    index = options.column;
  } else if (typeof options.column === 'string' && headerRow) {
    const found = columnNames.indexOf(options.column);
    index = found === -1 ? 0 : found;
  }

  const seen = new Set<string>(options.existing ?? []);
  const values: ParsedValue[] = [];
  let skipped = 0;
  let duplicates = 0;

  for (const row of dataRows) {
    const cell = row.cells[index];
    if (cell === undefined) {
      // A row with fewer cells than the column being read. Skipped and
      // counted, not defaulted to the empty string: a short row is a malformed
      // row, and importing a blank tag from it would put an untaggable value in
      // the field.
      skipped += 1;
      continue;
    }
    const value = options.trimValues === true ? cell.value.trim() : cell.value;
    if (value === '' && !cell.quoted) {
      // A missing cell and an empty one are both "no value here". A *quoted*
      // empty is data, for the same reason it is in `splitLine`: the user
      // quoted it, and silently dropping it is worse than keeping it.
      skipped += 1;
      continue;
    }
    if (seen.has(value)) {
      duplicates += 1;
      continue;
    }
    seen.add(value);
    values.push({ value, duplicate: false, line: cell.line });
  }

  return {
    values,
    skipped,
    duplicates,
    columnNames,
    delimiter,
    truncatedAtLine,
    warnings,
    rowCount: dataRows.length,
    blankLines,
  };
}

/** A sentence about what a file will do, before it does it. */
export function describeImport(result: CsvImport): string {
  const { values, duplicates, skipped, truncatedAtLine, delimiter, rowCount, blankLines } = result;
  const named = delimiterName(delimiter);

  if (truncatedAtLine !== null) {
    return `File is truncated — a quoted value starting on line ${truncatedAtLine} is never closed, so the rest of the file is inside it. Fix the file and import again.`;
  }
  if (rowCount === 0) {
    // A file with no rows at all is a different thing from a file whose rows
    // are all empty, and "No values in 0 rows" is a sentence about a file that
    // does not exist. The user picked a file; saying so plainly is the only
    // honest answer.
    return 'Nothing to import — the file is empty.';
  }
  if (values.length === 0) {
    if (duplicates > 0) return `Nothing new — all ${count(duplicates, 'value')} already in the field.`;
    const dropped = [
      skipped > 0 ? `${count(skipped, 'empty row')}` : '',
      blankLines > 0 ? `${count(blankLines, 'blank line')}` : '',
    ].filter(Boolean);
    const how = dropped.length > 0 ? ` (${dropped.join(', ')} dropped, ${named} separated)` : ` (${named} separated)`;
    return `No values in ${count(rowCount, 'row')}${how}.`;
    return 'Nothing to import.';
  }

  const parts = [`Will add ${count(values.length, 'value')}`];
  if (duplicates > 0) parts.push(`${count(duplicates, 'value')} already in the field`);
  if (skipped > 0) {
    // The delimiter is named only when a row was dropped. On a clean import it
    // is noise, and a sentence that always mentions the delimiter is a sentence
    // nobody reads -- but when rows are being thrown away, which separator was
    // assumed is the first thing to check, and guessing wrong is how every
    // value in the file ends up in one column.
    parts.push(`${count(skipped, 'row')} empty (read as ${named})`);
  }
  return `${parts.join(', ')}.`;
}

/** How a user would name the delimiter, not its escape sequence. */
export function delimiterName(delimiter: string): string {
  switch (delimiter) {
    case ',':
      return 'comma';
    case ';':
      return 'semicolon';
    case '\t':
      return 'tab';
    case '|':
      return 'pipe';
    default:
      return `"${delimiter}"`;
  }
}

function count(n: number, word: string): string {
  return `${n} ${word}${n === 1 ? '' : 's'}`;
}
