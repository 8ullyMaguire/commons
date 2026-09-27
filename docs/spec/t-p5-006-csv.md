# T-P5-006 item 12 — CSV import (`#1296`, `#431`)

**Plan entry:** `docs/plans/implementation-plan.md` §T-P5-006 item 12
**Spec:** §10.10 Bulk editing (C86) — "CSV **and** paste-parse import (#1296, #431)"
**Preceded by:** item 11 (`docs/spec/t-p5-006-paste.md`)
**Module:** `ui/src/lib/api/csv.ts`
**Component:** `ui/src/lib/components/MultiValueField.svelte` (extended)
**Tests:** `ui/tests/csv.test.ts` (76), `ui/e2e/csv-import.spec.ts` (14)
**Mutations:** `scripts/mutate-csv.mjs` — 34 applied, 30 killed, 4 exempt

---

## 1. A correction to item 11's spec

Item 11's spec closed with a §9 "What is deliberately not here" that said:

> **No CSV import.** That is `#1296`, its own item.

That was wrong, and it was wrong in a way worth recording. §10.10 reads
"CSV **and** paste-parse import (#1296, #431)" — one issue covering two
formats, not two issues. Item 11 delivered the paste-parse half and named the
CSV half as separate work. It is not separate; it is the rest of the same
issue, and it is item 12.

The cost of the error was one misleading sentence in a document whose whole
purpose is to tell a reader what is done. A "not here" list is a claim about
the future, and an unexamined claim about the future is how work quietly falls
through the gap between two items.

## 2. Why this is not `paste.ts` with a different splitter

`paste.ts` splits on newlines first and treats a line as a row. That is right
for a paste: a spreadsheet column copied to the clipboard has one value per
line.

A CSV is a *table*, and RFC 4180 allows a newline **inside** a quoted field. An
export from Postgres, MySQL or Excel puts a long description in exactly such a
field. A line-splitting parser turns one value into three and attributes the
pieces to three different rows.

So `csv.ts` is a character-by-character state machine. It reuses
`ParsedValue`, `preview` and `addCount` from `paste.ts`, which is the point: a
CSV import and a paste produce the *same shape*, so the preview, the Apply
button and Esc work for both with no second code path in the component. The
e2e asserts this directly — the file preview and the paste preview are the same
element, distinguished only by `data-source`.

## 3. The five decisions CSV does not make

**Delimiter — sniffed, overridable, reported.** The format's default is comma;
the files users have come from European locales (semicolon) and Excel (tab).
`sniffDelimiter` counts occurrences *outside quotes* — a comma inside a quoted
description is data, and a sniffer that counts it gets wrong exactly the files
that needed sniffing. The score is the **modal count across sampled lines**, so
one malformed row cannot flip the answer, and it prefers **agreement** over
magnitude, so a single row with six commas cannot outvote nine rows with one.
Ties break toward comma, the format default. The result is named in the UI
whenever a row was dropped, because that is when the user needs to check it.

**Header — the caller's choice, never a guess.** A column of tags whose first
entry happens to be `name` is entirely plausible. A parser that guessed would
drop a tag and call the import clean.

**Column — named or indexed, never guessed.** `columnNames` is returned so a UI
can offer the choice. An out-of-range index imports nothing rather than
throwing, because a UI that offered every column can have one deselected by the
time the file is read.

**Encoding — BOM first, then a NUL-position heuristic.** See §4.

**Malformed input — split by severity.** A truncated file (an unterminated
quote) is an **error**: the last field swallowed the rest of the file, and the
user has lost data without being told. A stray quote after a closing quote is a
**warning**: the value is recoverable, and refusing a whole import over it is
worse than importing it with a note.

## 4. A real bug the unit tests could not see

The first `decodeCsvBytes` decided the encoding by asking whether the bytes were
valid UTF-8:

```ts
if (!decode(head, 'utf-8').includes('\uFFFD')) return utf8;
```

NUL is a **valid UTF-8 character**. A UTF-16 file decoded as UTF-8 comes out as
`a\0b\0c\0` with no replacement character at all, so the check passes, the
UTF-16 heuristic below it is unreachable, and the file imports as mojibake.

Asking "is this valid UTF-8" cannot distinguish the two encodings for ASCII
text. Asking "are there NULs where text should be" can, because UTF-16 code
units for ASCII are NUL-padded. The fix is `countNulsAt(head, parity) === 0`.

The test that catches it asserts the decoded *text*, not the `encoding` field,
because the wrong answer is a string full of NULs rather than a wrong label.

## 5. Three tests that named a boundary without straddling it

This is now the sixth, seventh and eighth time in this project that a test
asserted a boundary and could not distinguish the two implementations. Each was
found by a surviving mutation, and each needed the case rebuilt from the
boundary's actual arithmetic.

**The sniffer.** The first test used `'"a,b,c",d\ne,f,g'` — a comma inside a
quoted field. A naive `line.count(',')` gives `[3, 2]` and the quote-aware scan
gives `[1, 2]`; both have a modal count that names the comma, so the test passed
with the quotes ignored. A second test putting a semicolon only inside quotes
still was not enough. The case that separates them puts the wrong delimiter
**both inside a quoted field and as a real separator**:

```
'"a;b;c",d\ne;f;g'    naive   ';' -> [2, 1]  modal 2, one line agrees
                     correct ';' -> [0, 1]  modal 1, one line agrees
```

Agreement is 1 either way, so the tie-break is magnitude, and the naive count
has the larger modal. It answers `;`, and the file is read as
semicolon-separated when the real delimiter is the comma.

**The BOM.** `hadBom: true` is set by the same branch that strips the BOM, so a
flag-only test cannot see a mutation that keeps the flag and drops the strip.
The strip is now asserted through a value: a leading U+FEFF is not whitespace,
so no `trim()` rescues it, and it becomes a tag that can never be found again.

**`cell.line` vs `row.line`.** These are the same number for an ordinary row, so
the obvious test cannot tell them apart. They differ only when a quoted field
holds a newline — the cell being read starts later than the row does.

## 6. Four exempt mutations, each proved

**`TextDecoder` strips the BOM itself.** `ignoreBOM` defaults to `false`, which
means *do not ignore it*, so decoding `bytes` and decoding `bytes.subarray(3)`
produce the same string. Verified directly. The `subarray` is kept anyway — it
makes the intent explicit rather than depending on a platform default — and the
comment says that is the reason, which is a different claim from "a test covers
it". The same holds for the UTF-16LE BOM.

**The sniffer's escaped-quote branch.** This function only *counts*; it does not
emit. Skipping an escaped `""` as a pair and toggling `inside` twice leave the
state identical and consume the same two characters. Checked exhaustively over
all 1,093 strings of length ≤ 6 in `{a, ", ;}` against both `,` and `;`:
**zero differences**. The same handling *is* load-bearing in `splitLine` and
`parseCsv`, where the characters are kept, and those are covered by mutations
that kill.

**The UTF-16LE check widened to accept a UTF-8 BOM.** Unreachable, precisely
*because* the UTF-8 check runs first and returns. A file starting `EF BB BF` can
never reach that branch, so widening its condition changes nothing for any
input. The ordering is the thing under test and no reachable mutation can
express a swap as a textual edit, so a test asserts it directly: a UTF-8 BOM
decodes as `utf-8` and specifically **not** as `utf-16le`.

## 7. A bug only the browser could find

`Esc` did not dismiss a file import.

The `onkeydown` handler was bound to the field *wrapper*, so it fired only for
events bubbling through it. A paste works because clicking the field puts focus
inside. A file import leaves focus on the hidden file input, which is outside
the wrapper — so Esc did nothing. That is the one way a preview is opened
without the user touching the field, which makes it the one way a user who
opens a file, reads the warning, and changes their mind is left with no way out.

The handler now listens on the document while a preview is open, in the capture
phase so it beats an enclosing modal, and removes itself when the preview
closes. The wrapper handler stays for the menu.

The test is kept for the reason it exists: it is the only e2e that imports a
file *without* clicking the field first.

## 8. Results

### Unit — `ui/tests/csv.test.ts`, 76 tests

| group | count | the load-bearing claim |
|---|---|---|
| `decodeCsvBytes` | 9 | BOM-less UTF-16 LE and BE, both BOMs, plain UTF-8 left alone |
| `sniffDelimiter` | 9 | counts outside quotes; the in-quotes-and-as-a-separator case |
| `parseCsv` | 22 | newline in a quoted field; blank line is a gap; truncation keeps parsed rows |
| `importCsv` | 21 | header is the caller's; quoted empty is data; `trimValues` is a choice |
| `describeImport` | 9 | truncation first; no rows ≠ all rows empty |
| `delimiterName` | 5 | the four, plus an unknown shown literally |

### Browser — `ui/e2e/csv-import.spec.ts`, 14 tests

| claim | why only a browser |
|---|---|
| the menu offers both entry points | the menu contents |
| a file previews before it changes the field | a real `File` with real bytes |
| confirming applies | the DOM after the click |
| **the file preview IS the paste preview** | `data-source` on one element |
| semicolons sniffed | a real file |
| spaces trimmed | the component's `trimValues: true` |
| **BOM-less UTF-16** | only a `File` carries the raw bytes |
| UTF-8 BOM stripped from the value | same |
| newline in a quoted field | same |
| **truncated → no Apply at all** | `toHaveCount(0)`, not disabled |
| **Esc without clicking the field first** | the focus bug |
| blank line is not a value | the parser, end to end |
| duplicate not re-added | the sentence |
| warning shown, value still importable | both halves |

## 9. Mutations — `scripts/mutate-csv.mjs`

**34 applied, 30 killed, 4 exempt, 0 stale.** Full table in the script; the four
exemptions are §6.

Two mutations were **retargeted** during the work, and one was **added** after a
first run: a mutation that no longer matches its source is a script that has
stopped testing anything, which is what the `COULD NOT APPLY` line reports.

## 10. What is deliberately not here

- **No column picker in the UI.** `importCsv` takes `column` and returns
  `columnNames`; the component imports column 0. A picker is a control, and
  adding one without a file that needs it would be a control no user of this
  corpus has asked for. The parser supports it the moment a UI wants it.
- **No persistence.** The GraphQL server is T-P6-007.
- **No tag creation.** A pasted or imported name that matches nothing is added
  as text; deciding whether it *becomes* a tag is §9.4's tagger.

## 11. Remaining in §10.10

Per-field ignore lists for the tagger — `#2318` and `#2399` — and then the plan
is 13 of 17.
