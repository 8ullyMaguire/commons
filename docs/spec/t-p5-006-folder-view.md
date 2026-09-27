# T-P5-006 item 16 — The folder view

**Plan entry:** `docs/plans/implementation-plan.md` §T-P5-006 item 16
**Spec:** §9.5 (nested folders, breadcrumbs, #1029, #1723), §5.14
**Closes:** #1586 (folder view), #1723 (breadcrumbs)
**Model already done:** item 6, `docs/spec/t-p5-006-folders.md`,
`crates/commons-store/src/folders.rs`

---

## 1. What item 6 left, and why it decides this one

`crates/commons-store/src/folders.rs` is complete, and its header states the
design: a folder is a name, a filter and a position; it **holds no objects**;
membership is recomputed on every open. So the model layer needed nothing here,
and the view has no membership to render and nothing to invalidate when a filter
changes.

What was missing is the thing a person touches: a navigator, and a URL.

The useful consequence of reading item 6's spec first is §1.1 there — a folder is
a **query, not a container**. That single fact drives the rest of this file.

## 2. Opening a folder REPLACES the filter, not narrows it

A folder is a query, so opening it is not "add a filter". It *is* the filter.

`ui/src/routes/folders/+page.svelte` implements it literally: the folder's filter
replaces the URL's `q` in the derived `ViewState`, with no intersection.

This is the part a user gets wrong if it is left implicit, and it is why the pane
shows the running filter in a visible slot rather than only reflecting the change
in the results. Someone who arrives with `?q=cat` in hand, opens "Untagged", and
gets a column of tagged items would file a bug. The pane saying `tag_count = 0`
makes the semantics visible instead of surprising.

## 3. Files

| File | Role |
|---|---|
| `ui/src/lib/api/folder-tree.ts` | The pure tree. No DOM, no network. |
| `ui/src/lib/components/FolderPane.svelte` | Pane: search, trail, tree, broken-data reports. |
| `ui/src/routes/folders/+page.svelte` | Pane plus wall; all state in the query string. |
| `ui/tests/folder-tree.test.ts` | 46 unit tests. |
| `ui/e2e/folders.spec.ts` | 19 browser tests. |
| `scripts/mutate-folder-tree.mjs` | 25 mutants. |

## 4. The tree, and what it guarantees

`buildTree(folders)` returns `{roots, all, orphans, cycles}`.

### 4.1 Every folder is placed somewhere

Three outcomes, and only three — a folder in none of them is a folder the user
cannot find:

- **nested** under its parent, when the parent exists and is reachable;
- **hoisted** to the roots, when the parent is missing (`orphans`);
- **reported** when it sits in a parent cycle (`cycles`).

An orphan is hoisted rather than dropped, and both broken cases render a status
line in the pane. A folder that silently never appears is the worst outcome a
navigator has, because the user has no way to know it exists.

### 4.2 Order is position-then-id, not position-then-name

Mirrors the store's `order_by_position_sql`, so the sidebar and the SQL cannot
disagree about what "third" means. The id tiebreak makes the order total: a
listing with no defined order can repeat or skip a row across a page boundary.

The choice that matters: **renaming a folder does not move it.** A tree that
re-sorts on rename moves rows out from under the cursor.

### 4.3 A parent cycle terminates, and is reported

Reachable by dragging a folder into its own descendant. Item 6's migration 0018
refuses it at the database, but the view cannot assume the data it is handed came
through that migration, and an unguarded `while` in a browser is a hang.

The guards are threefold and each answers a different question:

- a **visited set**, so placement terminates;
- a **`depth > 64` bound** as a backstop, at a limit well above the 20-deep
  legitimate chain that must survive;
- **`reachable(id)`** — "is there a root above this?" — and this is the one that
  was actually missing.

`reachable` is load-bearing for a reason worth recording. In `a → b → a` **no
folder is a root**, so the root loop skips both and both silently vanish. Writing
`reachable` as "returning to the starting id is fine" is tempting and wrong: a
path that returns to its start is a cycle, and a cycle has no root.

### 4.4 Notifications are inherited

A folder with a descendant that notifies marks the whole path, so the dot is on
the folder the user has to open to find the notification.

### 4.5 Counts are deliberately absent

Membership is recomputed on open, so counting a folder is a query per folder.
`formatCount(null)` is a **dash, not a zero** — a column of zeroes before the
counts land looks like an empty library, which is a worse lie than "I do not know
yet". Wiring counts is a follow-up once the server can return them cheaply.

## 5. Navigation

`FolderNav` is `{open, expanded}` — pure data, and URL-serialisable.

- `openFolder` **expands the ancestors** of what it opens. Opening a deep folder
  and having the pane collapse to a single highlighted row is how a user gets
  lost; the path is what tells them where they are.
- `openParent` from a root returns the tree root, and going up from the tree root
  stays put. An "up" that does nothing is a button that looks broken.
- `breadcrumbs(id, folders)` runs **nearest first** (#1723) — the direction a user
  reads a trail in when deciding where to go back to. A step that cannot be
  resolved renders as a disabled `missing` crumb rather than being dropped, so
  the trail's length still matches the depth.

## 6. Search

Replaces the tree with a flat list of matches **plus their ancestors**
(`keepAncestors`), rather than filtering the tree in place. Filtering in place is
where a hit three levels down ends up under a collapsed, unopenable path.

Matches name **or filter**, because a user remembers a folder as "the one that
looks for untagged" as often as by its name.

## 7. Memoization

`tree` is a `$derived` of `p.folders` and nothing else, so a keystroke in the
search box does **not** rebuild a 500-folder tree. The `rows` computation depends
on the search text, and folding the tree build into it would have made the pane
rebuild the tree per character.

## 8. Verification

```
targets ok: 76, tests passed: 1310, tests failed: 0
ℹ tests 595  ℹ pass 595  ℹ fail 0        # 46 new
127 passed (30.9s)                        # e2e, 19 new in folders.spec.ts
25 killed, 0 survived, 0 stale (of 25)
```

Two of the 25 mutants found the real bugs, and both produced a *plausible*
navigator rather than a crash — which is why they needed tests rather than a
reading of the code:

| Mutant | What went wrong |
|---|---|
| `if (parent !== null) continue` → `if (false) continue` | Every folder rendered twice, once nested and once loose. |
| `reachable` returns `true` on a cycle | `a → b → a` placed nobody; both folders silently vanished. |

## 9. Known follow-ups

1. **The `FOLDERS` array in the route is a fixture.** The GraphQL server is
   T-P6-007 and `Folders` is not in the transport yet. The fixture deliberately
   contains an orphan and a parent cycle, so the reporting paths are exercised in
   a browser and not only in unit tests. When the query lands, that array is the
   only thing that changes — the shape and the tree logic are already final.
2. **Counts are not wired** (§4.5), pending a cheap server-side count.
3. **Create / rename / delete / drag-to-reparent** are store-level and untested
   from the UI; the pane currently only opens and expands.
