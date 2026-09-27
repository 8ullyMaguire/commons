# T-P5-006 item 11 — Right-click paste (`#7139`)

**Plan entry:** `docs/plans/implementation-plan.md` §T-P5-006 item 11
**Spec:** §10.10 Bulk editing (C86), right-click paste; §9.4 tag organization
**Preceded by:** item 10 (`docs/spec/t-p5-006-create.md`).
**Module:** `ui/src/lib/api/paste.ts`
**Component:** `ui/src/lib/components/MultiValueField.svelte`
**Route:** `ui/src/routes/tags/+page.svelte`
**Tests:** `ui/tests/paste.test.ts` (39), `ui/e2e/multi-value-paste.spec.ts` (8)
**Mutations:** `scripts/mutate-paste.mjs` — 14, 13 killed, 1 exempt

---

## 1. The problem, in one sentence

§9.4 is about tag organization, and its edit boxes take many values. Copying a
list of tags out of a spreadsheet and pasting it into one of those boxes did
nothing, because the box was a single `<input>` and a paste is a single string.

The spec gives one sentence for this feature. Everything below is a decision
that sentence does not make.

## 2. The decision: a separator only counts outside quotes

A paste is one string that must become N values, and the text carries no
information about how it should be split. Three candidate rules:

1. **split on newlines only** — misses a spreadsheet column, which arrives
   comma-separated in `text/plain`;
2. **split on commas** — breaks the common case, because a person's name
   contains a comma and "Smith, John" is one tag;
3. **a separator counts unless it is inside quotes** — the chosen rule.

Rule 3 is a judgement call and is documented as one rather than presented as
something the format dictates. Its justification: the quoted form is
*unambiguous* and the unquoted form is not, so when the paste carries a quote
the quote is the only evidence available and it is trusted. A user who pastes a
genuine CSV with quoted commas gets the quoted reading, which is the right
default. A user who pastes `Smith, John` with no quotes gets two values, and
the fix is to add quotes — which is a thing they can see how to do, unlike a
parser that silently merged two of their tags.

`hasOpenQuote` counts quotes and treats an odd count as open, with `""` inside
a quoted field read as one escaped quote. That is the whole difference between
`"Smith, John", Jr` (two values) and `"Smith, John"` (one).

## 3. The decision: the paste does not land until it is confirmed

A `paste` event produces a **preview** — one sentence, an Add button, and
Cancel — rather than changing the field. Esc dismisses it.

The reason is volume: a paste of 400 tags into a field that already has 380 is
not obviously reversible by hand, and §10.10's rule is that an action states its
scope before the user commits to it. This is the same rule the bulk modal
follows with its scope line, applied to the smallest unit it has.

The preview says *"Will add 5 values, 35 already in the field"* rather than a
single number, for the reason item 10 established: one count silently chooses
between "you added five things" and "you asked about forty".

## 4. What the parser deliberately does not do

**It does not persist.** `onchange` hands the list to the parent; the parent
owns the draft and the save. The component has no store and no idea what a tag
is. The GraphQL server is T-P6-007, so nothing is written anywhere.

**It does not normalise case.** `parsePaste` is case-sensitive, and a test
asserts that pasting `best` into a field holding `Best` adds it. §9.4's tags
are case-insensitive for *display*, but two tags differing only in case are two
entities, and a parser that folded them would merge them silently. Normalisation
is a store decision, in a place the user can see it.

**It does not read the clipboard directly.** `navigator.clipboard.readText()`
is async, permission-gated, and can be denied — a denied read leaves a menu item
that closed and did nothing. The `paste` event carries the text with no
permission at all, so the field's `onpaste` is the only read path, and the
menu's Paste item dispatches a user-gesture `execCommand('paste')`. When the
browser refuses, the field is focused and the menu closes: the path is one
keystroke away and the item did not claim a success it did not have.

## 5. A real bug the mutation pass found

`splitLine` originally accumulated quote characters into the value buffer and
stripped them at the end. That reads `"A, B"` as one value — correct — and
`"A", B` as **one** value with stray quotes in it, which is wrong: a quote only
protects a separator that sits next to it, and once the closing quote has
passed, a space is a separator like any other.

The mutation that removed `unquote`'s `endsWith` check **survived**, and chasing
it is what turned up the real defect: keeping the quotes in the buffer makes the
two cases indistinguishable, because both have balanced quotes. The fix is to
strip quotes during the scan and track whether a part *opened* with one, so a
quoted blank (`" "`) survives as data while an unquoted blank is dropped.

Three tests were added for it, and one of them — `a quoted blank is dropped
like an unquoted one` — guards a case that the first fix broke: trimming before
filtering reduced `" "` to the empty string and silently discarded it.

## 6. An exempt mutation, and why

`a doubled quote is read as a close, then an open` **survives**, and is
recorded rather than chased. The mutation emits `""` instead of `"` and consumes
one character instead of two; both paths consume exactly two quote characters
and leave `inside` unchanged, and the doubled form is undone by `unquote`'s
`.replace(/""/g, '"')`.

An exhaustive check over every string of length ≤ 8 in `{a, "}` found **zero**
differences between the two implementations. It is an equivalent mutant, not a
weak test, and the exemption says so next to the mutation rather than deleting
the entry to improve the number.

## 7. Test results

### Unit — `ui/tests/paste.test.ts`, 39 tests

| claim | test |
|---|---|
| quote balance | `hasOpenQuote` × 4 |
| one layer of quoting, `""` | `unquote` × 4 |
| separator rules | `splitLine` × 13 |
| newline, CRLF, lone CR | `parsePaste` × 3 |
| no CR survives | `does not split a CRLF paste into values with a CR` |
| duplicates from the field | `drops a value the field already has` |
| duplicates within one paste | `drops a duplicate that appears twice in the paste itself` |
| case is not folded | `is case-sensitive, because two tags differing in case are two entities` |
| blank lines are not values | `counts a blank line as skipped, not as a value` |
| line provenance | `reports the line each value came from` |
| the preview sentence | `preview` × 5 |

### Browser — `ui/e2e/multi-value-paste.spec.ts`, 8 tests

| claim | why only a browser |
|---|---|
| right-click suppresses the browser menu | asserts `defaultPrevented` |
| a paste previews without changing the field | a real `ClipboardEvent` with real `DataTransfer` |
| confirming applies | the DOM after the click |
| duplicates are named | the rendered sentence |
| an all-duplicate paste has a **disabled** confirm | `toBeDisabled()`, not visibility |
| Esc dismisses without applying | the key event |
| a quoted comma is one tag | end to end, so a no-op parser cannot pass |
| CRLF leaves no `\r` | asserted on the rendered values |

The right-click test dispatches the event and reads `defaultPrevented` rather
than clicking. An earlier version asserted on `navigator.userAgent` — a line
that was both meaningless and false, since HeadlessChrome contains an `x`.

## 8. Mutations — `scripts/mutate-paste.mjs`

14 mutations: **13 killed, 1 exempt** (§6).

| mutation | result |
|---|---|
| a separator inside quotes splits anyway | killed |
| the quote state is not tracked at all | killed |
| a value already in the field is added again | killed |
| a value repeated inside one paste is added twice | killed |
| a blank line becomes an empty value | killed |
| only LF splits, so a CRLF paste leaves a CR | killed |
| case is folded | killed |
| a trailing separator produces an empty value | killed |
| the duplicate count is dropped from the preview | killed |
| the preview names the pasted count, not what will be added | killed |
| a split value is not unquoted | killed |
| `unquote` strips only the first character | killed |
| a quoted blank is dropped like an unquoted one | killed |
| a doubled quote read as close-then-open | **exempt — equivalent** |

Four of these needed retargeting after the §5 fix, because the code they named
had been rewritten. A mutation script whose replacements no longer match is not
a suite that got weaker; it is a script that has stopped testing anything, and
the `COULD NOT APPLY` line is what says so.

## 9. What is deliberately not here

- **No CSV import.** That is `#1296`, its own item, and it owns column order,
  delimiter sniffing and encoding. This parser reads *values*, not a table.
  The two are different problems and conflating them would produce a parser that
  guesses at a header row.
- **No tag creation.** A pasted name that matches no existing tag is added as
  text. Deciding whether it *becomes* a tag is §9.4's tagger, and the
  per-field ignore lists are `#2318`/`#2399`, also later items.
- **No persistence.** T-P6-007.
