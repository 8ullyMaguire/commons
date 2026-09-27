# T-P5-007 — Theming, accessibility, deep links

Spec §10.8 (C61), §15.10 (C90). Plan T-P5-007.

Owner-added scope, all of it mapped to a stash issue: dark/light, contrast,
focus management, screen-reader labels, hit-target sizes (#3322, #6383, #6823,
#6816), safe-area insets (#5979), viewport-constrained popovers (#4667), a
lightbox animation toggle (#6864), mobile web as a supported layout (#771,
#6335), and every view state as a resolvable URL (#337, #5612, #185).

## What already exists, and what this ticket therefore does not do again

| Concern | Where it is | Note |
|---|---|---|
| filters, sorts, view mode, density as a URL | `ui/src/lib/api/view.ts` | `encodeView` / `decodeView` / `viewToHref` / `viewFromLocation`, 1 test file. **§15.10 is done.** |
| the page request type | `ui/src/lib/api/client.ts` | `PageInput` with the keyset cursor, no offset. |
| safe-area insets, in one place | `ui/src/lib/components/Lightbox.svelte` | `env(safe-area-inset-*)`, already correct there. |
| `aria-label` and `role` | 13 components | Present where a control needed a name. Not audited. |

So this ticket is **theming, the a11y primitives, and the two responsive
fixes** — not a second URL layer, and not a re-audit of labels that are already
there.

## The four decisions that are not obvious

### A theme is a set of CUSTOM PROPERTIES, and `app.css` was already waiting for it

`ui/src/app.css` carries `--bg`, `--fg`, `--muted` and the comment *"a future
theme is a variable change and not a component rewrite."* That is the whole
design, already made. So the theme work is: define the full token set once,
declare a `[data-theme="light"]` override, and change **no component**.

`prefers-color-scheme` is the default and `data-theme` is the override, in that
order of authority, because a user who set a preference means it and a user
who set nothing should follow the OS. The three-way choice (system / light /
dark) is state in a store and *nothing else* — no class on `<html>` written
from a component, because `document.documentElement` written from a component is
the one part of a theme that cannot be tested without a DOM.

### Contrast is asserted as a NUMBER, in a test, against the token pairs

WCAG contrast is the whole reason a dark/light toggle is dangerous: a token
pair that reads well on `--bg` at #111 can be unreadable on #fff, and a
screenshot proves nothing. So `contrast.ts` computes the WCAG 2.1 ratio and the
test asserts **every declared pair clears its threshold in both themes** —
4.5:1 for body text, 3:1 for large text and UI boundaries (§1.4.3, §1.4.11).

A theme that ships without this is a theme whose contrast is whatever the last
author's monitor showed. The numbers are in the test, not in a comment.

### `focus-visible`, and why it is one global rule rather than per-component

There is currently no `focus-visible` anywhere. The naive fix is a
`:focus` style on each control, which produces the two bad outcomes: a ring on
mouse clicks (the "accessibility feature nobody can turn off" complaint) and no
ring at all on a control somebody forgot. One rule in `app.css` —

```css
:focus-visible { outline: 2px solid var(--focus); outline-offset: 2px; }
:focus:not(:focus-visible) { outline: none; }
```

— gets both right for every present and future control, including any in a
plugin. **A control that removes its own outline without replacing it is a
violation**, and the axe scan plus a focus-order test are what hold that line.

### A popover is positioned by a pure function, so the flip is unit-tested

`popover.ts` takes a rect and the viewport and returns the placement. The rule:
prefer the requested side, **flip** when it would overflow, and **shift** along
the cross axis when neither side fits, clamping to the viewport rather than
letting the popover hang off the edge (#4667). Pure, so "a popover near the
bottom-right corner opens upward and leftward" is a unit test rather than a
screenshot — and the same function serves the tagger, the filter menu and the
command palette.

## What is claimed, and how it is verified

| Claim | Verified by |
|---|---|
| dark/light/system, OS default respected, choice persisted | `tests/theme.test.ts` (pure) + `e2e/theme.spec.ts` (applies) |
| every token pair clears its WCAG threshold in both themes | `tests/contrast.test.ts` — the numbers are the test |
| a keyboard user sees a focus ring, a mouse user does not | `e2e/a11y.spec.ts` |
| hit targets are ≥44px on touch viewports (#3322) | `e2e/a11y.spec.ts`, measured |
| safe-area insets applied at the shell, not per-component | `e2e/a11y.spec.ts` |
| popover flips and shifts rather than overflowing | `tests/popover.test.ts` |
| lightbox animation toggle (#6864) | `tests/theme.test.ts` |
| **zero critical axe violations** | `e2e/a11y.spec.ts`, the ticket's own bar |

## The e2e that must be run twice

`scripts/verify.sh` builds the app between the unit and e2e stages. A theme
test that passes against a stale `build/` proves nothing, because the token
values it reads are in the built CSS. Run it twice — once after a build, once
immediately again — and if the two disagree, the build is stale, not the test.

## Not claimed here

- **Time-limited share links** (#5612). That is T-P9-003, which owns expiring
  links and the share-token model; a theme is not where a token lives.
- **Full WCAG conformance.** axe-core with zero *critical* violations is the
  ticket's stated bar and is what is claimed. An audit of every rule is a
  different claim and is not made.
- **A component library.** The spec forbids a UI framework, so the tokens are
  CSS custom properties and the components stay hand-written.
