# T-P5-006 item 5 — Command palette and shortcut map

**Plan entry:** `docs/plans/implementation-plan.md` §T-P5-006 item 5
**Spec:** §10.7 — a complete shortcut map, a command palette, undo, #2833
collision handling, #2542 Esc closes modals, #6218/#5587 ordering shortcuts
**Preceded by:** item 4 (`docs/spec/t-p5-006-unsaved-guard.md`), whose registry
is the model this one follows.

---

## 1. What this is

One registry of commands, one place that decides which of them a keystroke
invokes, and one palette that lists the same registry. The keyboard map, the
palette, and every future menu item read from the same list — so a shortcut
cannot exist without a command behind it, and a command cannot be invisible.

**This is the item that makes the other sixteen composable.** Right-click paste,
undo, and CSV import are all "a command someone can invoke". Building the
registry once means those are a registration each, not a keyboard handler each.

## 2. What exists today

Measured, not assumed:

| Thing | State |
|---|---|
| Any command registry | **does not exist** |
| Any global key handler | **does not exist** |
| A command palette | **does not exist** |
| A documented shortcut map | **does not exist** |
| Per-component key handling | 3 sites, independent: `Lightbox`, `BulkEditModal`, `ListTable` |
| Collision resolution (#2833) | **impossible today** — nothing knows what else is listening |

The three existing handlers are the reason this is not just a palette. They
each call `window.addEventListener('keydown', …)` with no coordination, so
pressing Escape in the lightbox closes the lightbox *and* any parent handler
also sees it. #2833 is not a feature to add; it is a consequence of having a
registry, and it is impossible to add one at a time.

## 3. The three things that are actually hard

### 3.1 Collision resolution is about *who is asking*, not who wins

The naive model is a priority number: highest priority wins. That is wrong for
the case that actually happens. The lightbox is open, the user presses Escape,
and both the lightbox and the app want it. Priority resolves this by
consequence: the lightbox is on top, so the lightbox gets it — which is right,
but only because "on top" is knowable.

A priority number cannot express that. So instead: **scopes nest, and the
innermost scope that claims a key wins and the event stops there.** A scope is a
stack; a command declares which scope it lives in; resolution walks the stack
from the top and takes the first scope that has a binding for the key. No
priority numbers, and no two components can disagree about who is on top,
because "on top" is literally the top of one stack that both of them read.

The consequence that matters: a scope claims a key, or it does not. A scope
cannot claim "Escape but only sometimes", because that is how a modal ends up
unclosable.

### 3.2 A keystroke is not a key, and conflating them is why maps are wrong

`event.key` is layout-dependent — on a French keyboard `q` and `a` swap
positions, and `event.key` follows the *character*, so `Ctrl+A` means "home" on
one layout and "select all" on another. A shortcut map has to be about physical
keys or it is wrong for every user who is not on a US layout.

So bindings are matched on `event.code` (physical position) with `event.key` as
a fallback for keys that have no meaningful `code` (Escape, F-keys, arrows).
Both are stored, and a binding is satisfied by either, which is permissive in
the safe direction: a binding that fires when it shouldn't is a bug, and a
binding that fails to fire on an AZERTY user is a worse one.

### 3.3 The palette filters on the same registry it invokes

Not a second list. The palette's fuzzy match runs over the registry, so a
command that is not in the registry cannot be found in the palette, and a
command in the registry cannot be missing from it. The alternative — a
palette-specific list — is how a palette and a keyboard map drift apart, which
is the bug #2542 and #6218 are both really about.

`Ctrl+P` is the only chord that opens the palette, and it is handled *before*
scope resolution, because the palette has to be reachable from anywhere
including from inside a scope that would otherwise claim it.

## 4. Acceptance

The plan says only "Playwright spec per surface". For this item the surfaces
are the keyboard itself, so:

1. **A key invokes its command.** With a registry holding one command bound to
   `KeyX`, pressing `x` runs it.
2. **The innermost scope wins.** A command in the lightbox scope and one in the
   app scope both bound to Escape: with the lightbox open, only the lightbox's
   runs. The outer one does not also run.
3. **A claimed key stops there.** Two handlers, one key, the inner scope's runs
   exactly once — not twice, which is what independent `addEventListener` calls
   produce today.
4. **Unbinding removes it.** A command with no binding is in the palette and
   not on the keyboard, and the palette says so.
5. **The palette finds what the keyboard can do.** Every command in the
   registry appears in the palette; a palette query that matches a command's
   title runs that command.
6. **`Ctrl+P` opens the palette from inside a scope**, because a palette you
   cannot reach is a palette that does not exist.
7. **Typing in an input does not invoke commands.** A keystroke in a text field
   is text. The one exception is a chord with a modifier, and that exception is
   the interesting case, not a footnote.
8. **Layout independence.** A binding matched on `code` fires on a layout where
   the character differs — testable without a second keyboard, because the
   matcher is pure and takes the two fields separately.
9. **Every shortcut is discoverable in the app**, without leaving it: the
   palette is the map. That is what "a complete shortcut map" means when there
   is no manual to put it in.

## 5. Mutation coverage

`scripts/mutate-commands-ui.py` over the pure parts: key normalisation, scope
resolution, and the palette's filter. The parts a mutation cannot reach are the
DOM wiring — a keydown listener and an input — which is the same split as items
3 and 4.

## 6. Deliberately not here

- **Undo (#3221).** §5 of the scoping doc deferred it until after the first
  mutation, which item 3 delivered. It is now unblocked, and it belongs here:
  undo is a command, and the first thing a command registry is *for* is having
  several. It gets its own spec because the reversal store is a data structure
  with its own invariants, and bolting it onto the palette would make both
  worse.
- **Right-click paste (#7139).** Deferred to after this item, per §5. It needs
  the registry, which is what this item delivers.
- **Bindings being user-configurable.** The spec says "a complete shortcut map",
  not "an editable one". A persisted user-keymap needs a settings store, a
  conflict UI, and a migration story for the stored shape — three more items.
  The registry shape is designed so that adding one is additive, but writing it
  now would be a stub with a schema.
