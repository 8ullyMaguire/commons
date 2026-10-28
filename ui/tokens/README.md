# Estate design tokens

One palette, one type scale, one spacing scale, one trust vocabulary — consumed by every project, whatever
its stack. This is Tier 1 of the stack recommendation: it is what makes eleven sites look like they were
built by one team, and it costs no shared database, no shared session and no rewrite.

```
tokens.json     the source of truth (semantic roles, with a note on what each is for)
tokens.css      GENERATED — do not edit; run build.py
build.py        render, and --check to fail when the two disagree
```

## Why this exists — measured, not asserted

```
distinct 6-digit hex colours across 8 front ends (ascend, streakforge, tessera, lorebook,
whitebois, darkhold, ficnexus, polaris):  812
most repeated:  #616161 ×245   #000000 ×159   #007acc ×136   #ffffff ×133   #3794ff ×115
                #f0f6fc ×92    #f14c4c ×90    #1a85ff ×86    #808080 ×85    #e51400 ×80
```

812 distinct colours is not eleven designs; it is nobody's design. Half of them come from embedded theme and
vendor CSS, which is exactly why the answer is *semantic roles* rather than a longer palette: a project says
`var(--text-muted)`, not `#9aa4ad`, and the meaning survives the next redesign.

## The rule

1. **Consume roles.** `var(--text)`, `var(--surface)`, `var(--accent)`, `var(--space-4)`, `var(--radius)`.
   Never a literal hex or pixel value in a shared surface.
2. **Map, do not restyle.** Existing colours are mapped onto the nearest role during adoption. A project
   that looks different today should look *the same kind of different* tomorrow — the point is agreement,
   not a redesign in the same commit.
3. **Trust and moderation vocabulary is part of the token set** (`--trust-0` … `--trust-4`). If two projects
   show a trust level in different colours, users read a different ladder; that is a correctness bug wearing
   a design costume.
4. **Light mode is defined here, not per project** — one `prefers-color-scheme` block, so a site cannot
   half-implement it.
5. **`build.py --check` belongs in CI.** A token edited in the JSON and not regenerated is a build failure,
   not a mystery on someone's screen six weeks later.

## Verified behaviour

```
$ build.py --check          -> tokens.css matches tokens.json (58 declarations)     rc 0
append one stray line       -> tokens.css is STALE against tokens.json …           rc 1
$ build.py && build.py --check -> matches (58 declarations)                        rc 0
```

## Consuming it

* **SvelteKit** — `@import "../../rust/commons/ui/tokens/tokens.css";` in the root layout until a published
  package exists (item 5.3 decides the packaging: npm workspace vs vendored copy per repo).
* **Go templates / plain HTML** — the same file, copied at build time.
* **Rust-rendered** (askama/maud) — include the CSS file at startup or serve it as a static asset; the
  token crate (`--tokens` in the crate split, item 5.4) exposes the values for inline styles and emails.

## What is deliberately not here

No components (item 5.2), no icon set, no font files. Tokens decide *values*; components decide
*structure*. Mixing them is how a design system becomes a framework nobody can adopt piecemeal.
