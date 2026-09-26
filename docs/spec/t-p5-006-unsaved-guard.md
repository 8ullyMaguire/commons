# T-P5-006 item 4 — Unsaved-entry protection

**Plan entry:** `docs/plans/implementation-plan.md` §T-P5-006 item 4
**Spec:** §10.7 (all destructive actions confirm, long tasks confirm with scope
spelled out), §10.10 (unsaved-entry protection)
**References:** stash#6466 (unsaved entries intermittently lost), stash#3253,
stash-box#594 (confirmation on cancel), stash#7154 (back button surfaces an
unsaved modal — T-P5-005's own edge, still open)

---

## 1. The plan's floor, and why it is not the target

The plan's "done when" is *"the unsaved-protection test must attempt
navigation with an unsaved edit and assert a confirm appears. Done when: that
test exists."*

That is a floor. A test that exists is not a test that passes, and a confirm
dialog that appears on every navigation regardless of whether anything is
unsaved would satisfy the letter of it while being worse than nothing — a guard
that always fires is a guard users learn to dismiss without reading, and then it
protects nothing on the one navigation that mattered.

So: the test must exist **and pass**, and the second half of this document is
the part the floor does not mention — what happens after the user clicks away.

## 2. What "unsaved" means here, and what it does not

`selection.ts` already has `isUnsaved(pendingEdits, hasFailedSave)`. It is
tested, and it is the right rule: a *save state*, not an edit count. A counter
is wrong twice — it does not fall back to zero when the last pending edit is
saved, and it cannot represent "edited, saved, edited again" without a second
piece of state to subtract the save from.

Item 4 is not that function. Item 4 is everything around it:

| Concern | Item 1 did | Item 4 does |
|---|---|---|
| the rule | `isUnsaved()` | — |
| who holds the state | nothing | a registry any surface can write to |
| who asks | nothing | the router, on every navigation |
| what the user sees | nothing | a confirm naming what is at risk |
| what happens if they confirm | nothing | the edit is genuinely discarded |
| what happens if they cancel | nothing | nothing moved, and the edit is intact |
| closing the tab | nothing | `beforeunload`, because a navigation guard cannot see it |
| a *failed* save | modelled, not reachable | reachable, and it blocks |

## 3. The three things that are actually hard

### 3.1 Intercept, never undo

The obvious implementation watches `page.url`, sees the navigation, and calls
`goto()` back to put the user where they were. It is wrong, and the failure is
quiet enough to survive a review: **SvelteKit destroys the outgoing page
component when a navigation commits**, so undoing a navigation does not return
the user to their page, it *remounts* a fresh one. Every piece of component state
is rebuilt, and an in-progress edit is destroyed by the very navigation the guard
performed to protect it. The confirm then renders over a now-clean page and
reports nothing to save.

So a navigation is intercepted **before it commits** and nothing is ever
destroyed. A capture-phase `click` listener on the document runs before the
browser follows the link, so `preventDefault()` there stops the unmount outright.
The decision half is held as a thunk, and the thunk runs only after the user
answers.

### 3.2 Two registries, and why

A click-intercepted navigation happens while the page is still mounted, so the
live registry is the whole truth. A navigation with no click behind it — the back
button, a form submit, a `goto()` from code — has already unmounted every
surface, and each has already deregistered, so the live registry is empty and the
only remaining evidence of what was at risk is a record the store keeps:
`guard.last`, written by `add` and by nothing else.

`drop` deliberately does not write `last`. A surface unmounting is a
*consequence* of the navigation, so letting it erase the record would make the
guard destroy its own evidence one step before reading it. `consume` does clear
it, because that is the user saying yes, lose it — and a user who abandoned an
edit must not be asked about it again.

The watcher tells the two cases apart by comparing the URL change against the URL
the click handler last recorded, not by reading a boolean the handler set. A flag
written by an event handler and read by an `$effect` is read at the wrong moment:
the effect is scheduled by `page.url`, so its body may run before the handler's
write lands.

### 3.3 What the platform will not give us

For a history navigation the page is already gone when the guard notices.
`beforeunload` does not fire for same-document navigations, and there is no hook
that runs earlier. "Stay" therefore means *go back to the page you were on*, and
the unsaved edit does not survive the round trip. The confirm still names what
was at risk, which is what the spec asks for. A tab close is worse still: only
`beforeunload` sees it and only the browser may render a prompt, so that case
gets `beforeunload` plus a visible indicator and nothing more.

## Decisions

| Decision | Why |
| --- | --- |
| Capture-phase listener on `document`, not on the toolbar | The back button is not a link and lives outside the toolbar; a bubble-phase document listener can be too late. |
| Thunk, not URL, for the held navigation | The page must never unmount, so there is nothing to undo and nothing to reconstruct. |
| `guard.last` written only by `add` | `drop` fires *because of* the navigation; letting it clear the record loses the evidence. |
| `consume` clears `last` too | The only caller entitled to: the user has answered. |
| `role="alertdialog"` | It blocks a navigation in flight; a passive `dialog` is not announced as an interruption. |
| `onDestroy` for deregistration | Unmounting is not a change to `dirty`, so the registration effect never re-runs to clear it. |
| Neutral colour on the indicator | A pending edit is ordinary work. An alarm-coloured one teaches users to ignore the failed save, which is the one that matters. |

## Verification

- `ui/tests/guard.test.ts` — the rules, in isolation: 24 cases.
- `ui/e2e/guard.spec.ts` — the wiring, 9 cases, including the three the plan's
  floor does not name (a guard that never goes quiet, a save that leaves a
  phantom registration, and a back-button navigation with no click to intercept).
- `scripts/mutate-guard-ui.py` — 10 mutants, 10 killed.

Two bugs in the tests themselves were found by the mutation script and are worth
recording, because both would have shipped:

- The provenance-style test for `summary` only ever ran against a registry with
  something in it, so the empty case — where a wrong answer renders "You have ."
  into the dialog — was untested. That in turn exposed a *dead* `isDirty` guard
  in `summary`: replacing it with a size check changed nothing observable, which
  is the mutation script's way of saying "this line is not load-bearing". The
  guard is gone and the empty case is decided where it is used.
- The first back-button fixture used `page.goto` to set up history. A `goto` is a
  full document load, so it destroys the surface and there is nothing to guard
  when the history entry lands. The failure looked like a missing guard rather
  than a broken fixture, which is what made it expensive.
## 4. Acceptance

The plan's floor, plus the cases it does not name. The plan says only "the
unsaved-protection test must attempt navigation with an unsaved edit and assert
a confirm appears" — case 1. A test that exists is not a test that passes, and a
guard that prompts on *every* navigation satisfies the letter of it while being
worse than nothing, so case 3 is the one that decides whether this is a feature
or wallpaper.

1. **The plan's test.** Navigate with an unsaved edit; a confirm appears.
2. **Cancelling leaves everything alone.** Not "the dialog closed" — the edit is
   still pending, the registry is unchanged, and the URL has not moved. A guard
   that resolves `false` and then clears state has silently discarded the edit
   *and* left the user where they were, which is the worst of both.
3. **A guard that does not fire when nothing is unsaved.** Navigating with a
   clean registry must not prompt. A test written only against the floor never
   checks this.
4. **Leaving does not ask twice.** "The whole mechanism", per §10.10: the user
   who chose to lose an edit must not be asked about it again on the way out, or
   they learn to click through — and a guard that can be clicked through protects
   nothing on the one navigation that mattered.
5. **Saving clears the registration.** A surface that sets a boolean and forgets
   to clear it on save gives a phantom prompt forever with nothing to save.
6. **A failed save blocks with nothing pending,** and says *which* it is. A user
   told only "you have unsaved changes" assumes their work is held somewhere and
   leaves.
7. **The prompt names the surface.** §10.7 spells out the scope; "are you sure?"
   does not, and does not let one edit be told from three.
8. **The back button is guarded too.** Not just the links — a guard that only
   intercepts clicks is bypassed by the one gesture every browser offers.
9. **A blocking dialog is announced as one.** `alertdialog`, not `dialog`.

Plus, for the tab-close case: `beforeunload` is registered exactly when the
registry is dirty, and not when it is clean. A permanently-registered handler
prompts on every reload forever.

## 5. Mutation coverage

`scripts/mutate-guard-ui.py`, because the rules here are pure and live in
`src/lib/api/guard.ts` — the registry, the summary, and the decision. The router
wiring is the part a mutation cannot reach, so the Playwright spec carries that,
which is the same split as items 1 to 3.

Ten mutants, all killed. Both survivors the first run found were gaps in the
*tests*, and one of them was a gap in the *code* — see the end of §3.

## 6. What is deliberately not in this item

- **Undo (#3221).** §5 of the scoping doc deferred it to after the first
  mutation. Item 3 delivered the first mutation, so undo is unblocked now — but
  it is a separate item with its own spec, and bolting it on here would make
  both worse.
- **The `#7154` back-button case.** That is a lightbox edge (T-P5-005), and the
  guard here is what makes it *survivable* rather than what fixes it. It gets
  its own test when the lightbox's own state is in scope.
