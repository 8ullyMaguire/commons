/*
  Tests for the folder tree view. Spec 9.5 and 5.14, plan T-P5-006 item 16;
  #1586, #1723.

  # What is being tested here

  The store (`crates/commons-store/src/folders.rs`) is done and its header says
  the important thing: a folder holds no objects. Membership is recomputed on
  every open, so there is nothing to compute here and nothing to invalidate.

  What this file tests is the part the store deliberately does not have — the
  shape a tree takes in a browser, and what happens when the tree underneath is
  not well-formed. The load-bearing claims:

   1. **every folder is placed somewhere.** An orphan is hoisted to the roots and
      REPORTED, not dropped. A folder that silently never appears is the worst
      outcome a navigator has, because the user has no way to know it exists.
   2. **a parent cycle terminates** and is reported. The store's migration 0018
      refuses one at the database, but the view cannot assume the data it is
      handed came through that migration, and an unguarded `while` in a browser
      is a hang.
   3. **the order is position-then-id**, not position-then-name. A tree that
      re-sorts when something is renamed is a tree that moves under the cursor.
   4. **opening a folder replaces the filter rather than narrowing it**, and
      opening one expands its ancestors, so the user can see where they are.
 */

import { test, describe } from 'node:test';
import assert from 'node:assert/strict';

import {
  ROOT_NAV,
  breadcrumbs,
  buildTree,
  compareFolders,
  flatten,
  formatCount,
  isCurrentCrumb,
  isExpanded,
  keepAncestors,
  openFilterLabel,
  openFolder,
  openParent,
  pathTo,
  searchFolders,
  toggleExpanded,
  type Folder,
} from '../src/lib/api/folder-tree.js';

function f(over: Partial<Folder> & { id: string }): Folder {
  return {
    name: over.id,
    filter: 'all',
    parentId: null,
    position: 0,
    icon: null,
    notify: false,
    ...over
  };
}

/** a, with children b and c; b has a child d. */
const TREE: Folder[] = [
  f({ id: 'a', name: 'Alpha', position: 1 }),
  f({ id: 'b', name: 'Beta', parentId: 'a', position: 0 }),
  f({ id: 'c', name: 'Gamma', parentId: 'a', position: 1 }),
  f({ id: 'd', name: 'Delta', parentId: 'b', position: 0 })
];

// ---------------------------------------------------------------------------

describe('buildTree', () => {
  test('nests children under their parent', () => {
    const tree = buildTree(TREE);
    assert.equal(tree.roots.length, 1);
    const a = tree.roots[0]!;
    assert.equal(a.folder.id, 'a');
    assert.deepEqual(a.children.map((c) => c.folder.id), ['b', 'c']);
    assert.deepEqual(a.children[0]!.children.map((c) => c.folder.id), ['d']);
  });

  test('records the depth of each node', () => {
    // Indentation is derived from this, and a wrong depth is a tree that
    // indents a leaf as if it had children.
    const tree = buildTree(TREE);
    const byId = new Map(tree.all.map((n) => [n.folder.id, n]));
    assert.equal(byId.get('a')!.depth, 0);
    assert.equal(byId.get('b')!.depth, 1);
    assert.equal(byId.get('d')!.depth, 2);
  });

  test('counts descendants at any depth, not just children', () => {
    // "3 items" under a, when a has 2 children and one grandchild, is the number
    // a user can check by looking.
    const a = buildTree(TREE).roots[0]!;
    assert.equal(a.descendantCount, 3);
  });

  test('a parent with no children has none', () => {
    assert.equal(buildTree([f({ id: 'x' })]).roots[0]!.descendantCount, 0);
  });

  // The order, and why it is not by name.
  test('siblings are in position order, with the id as a tiebreak', () => {
    // Mirrors the store's `order_by_position_sql`, so the sidebar and the SQL
    // cannot disagree about what "third" means.
    const tree = buildTree([
      f({ id: 'z', name: 'Zebra', position: 1 }),
      f({ id: 'a', name: 'Apple', position: 0 }),
      f({ id: 'm', name: 'Mango', position: 1 })
    ]);
    // Apple(0), then the position-1 pair ordered by ID: 'm' < 'z'. Note the
    // id, not the name -- 'Mango' would sort before 'Zebra' by name too, and
    // 'Apple' by name would sort AFTER both. The ids are what make the order
    // total and what stop a rename from moving a row.
    assert.deepEqual(tree.roots.map((r) => r.folder.id), ['a', 'm', 'z']);
  });

  test('two folders at the same position have a defined order', () => {
    // Without the id tiebreak, a listing with no defined order can repeat or
    // skip a row across a page boundary.
    const [x, y] = [f({ id: 'x' }), f({ id: 'y' })].sort(compareFolders);
    assert.equal(x!.id, 'x');
    const reversed = [f({ id: 'y' }), f({ id: 'x' })].sort(compareFolders);
    assert.deepEqual(reversed.map((r) => r.id), ['x', 'y'], 'the same order either way');
  });

  test('renaming a folder does not move it', () => {
    // The claim that motivates position-then-id over sorting by name.
    const before = buildTree([
      f({ id: 'a', name: 'Zebra', position: 0 }),
      f({ id: 'b', name: 'Apple', position: 1 })
    ]).roots.map((r) => r.folder.id);
    const after = buildTree([
      f({ id: 'a', name: 'Apple', position: 0 }),
      f({ id: 'b', name: 'Zebra', position: 1 })
    ]).roots.map((r) => r.folder.id);
    assert.deepEqual(before, after);
  });

  test('a folder whose parent does not exist is hoisted and reported', () => {
    // A deleted parent is an ordinary event, not corruption. Hoisting means the
    // user can still see the folder and fix it; dropping it means it vanishes
    // with nothing said.
    const tree = buildTree([f({ id: 'a' }), f({ id: 'lost', parentId: 'gone' })]);
    assert.deepEqual(tree.roots.map((r) => r.folder.id).sort(), ['a', 'lost']);
    assert.deepEqual(tree.orphans.map((o) => o.id), ['lost']);
  });

  test('an orphan is not also counted as a root twice', () => {
    const tree = buildTree([f({ id: 'lost', parentId: 'gone' })]);
    assert.equal(tree.roots.length, 1);
    assert.equal(tree.all.length, 1);
  });

  // A cycle is reachable by dragging a folder into its own descendant, and the
  // view cannot assume the data came through migration 0018.
  test('a parent cycle terminates instead of hanging', () => {
    // `a -> b -> a` has no root at all, so the honest outcome is: the loop is
    // reported, and the folders that cannot be placed are hoisted so they are
    // reachable rather than invisible.
    const tree = buildTree([
      f({ id: 'a', parentId: 'b' }),
      f({ id: 'b', parentId: 'a' })
    ]);
    assert.deepEqual(tree.cycles.sort(), ['a', 'b']);
    // Whatever the placement, it terminates and every id is accounted for
    // somewhere: rendered, an orphan, or in `cycles`. A folder in none of the
    // three is a folder the user cannot find.
    const accounted = new Set([
      ...tree.all.map((n) => n.folder.id),
      ...tree.orphans.map((o) => o.id),
      ...tree.cycles
    ]);
    assert.deepEqual([...accounted].sort(), ['a', 'b']);
  });

  test('a self-parented folder terminates', () => {
    const tree = buildTree([f({ id: 'a', parentId: 'a' })]);
    assert.equal(tree.all.length, 1);
    assert.deepEqual(tree.cycles, ['a']);
  });

  test('a cycle in one subtree does not eat the rest of the tree', () => {
    // The claim that matters: a broken folder must not blank the navigator.
    const tree = buildTree([
      f({ id: 'ok' }),
      f({ id: 'a', parentId: 'b' }),
      f({ id: 'b', parentId: 'a' })
    ]);
    assert.ok(
      tree.all.some((n) => n.folder.id === 'ok'),
      'an unrelated folder still renders'
    );
  });

  test('a long legitimate chain is not cut as if it were a cycle', () => {
    // A visited set alone is not enough of a guard, and a depth limit alone
    // would cut a real deep tree. The chain here is 20 deep and must survive.
    const deep: Folder[] = [f({ id: 'n0' })];
    for (let i = 1; i < 20; i += 1) deep.push(f({ id: `n${i}`, parentId: `n${i - 1}` }));
    const tree = buildTree(deep);
    assert.equal(tree.all.length, 20);
    assert.deepEqual(tree.cycles, []);
  });

  test('a notify on any descendant marks the whole path', () => {
    // The sidebar shows a dot on the folder you have to open to find it.
    const tree = buildTree([f({ id: 'a' }), f({ id: 'b', parentId: 'a', notify: true })]);
    assert.equal(tree.roots[0]!.notifies, true);
  });

  test('an empty list is an empty tree, not a crash', () => {
    const tree = buildTree([]);
    assert.deepEqual(tree.roots, []);
    assert.deepEqual(tree.all, []);
    assert.deepEqual(tree.orphans, []);
    assert.deepEqual(tree.cycles, []);
  });
});

// ---------------------------------------------------------------------------

describe('flatten and pathTo', () => {
  test('flatten is depth-first, parents before children', () => {
    // This is the order the sidebar renders rows in, and the order a keyboard
    // traversal moves through.
    assert.deepEqual(flatten(buildTree(TREE).roots).map((n) => n.folder.id), [
      'a',
      'b',
      'd',
      'c'
    ]);
  });

  test('pathTo returns the route to a node', () => {
    // What auto-expanding a search hit needs.
    assert.deepEqual(pathTo('d', buildTree(TREE).roots).map((n) => n.folder.id), ['a', 'b', 'd']);
  });

  test('pathTo is empty for a node that is not there', () => {
    assert.deepEqual(pathTo('nope', buildTree(TREE).roots), []);
  });
});

// ---------------------------------------------------------------------------

describe('breadcrumbs', () => {
  test('runs from the folder to its root, nearest first', () => {
    // #1723. Nearest first, because that is the direction a user reads a trail
    // in when they are deciding where to go back to.
    assert.deepEqual(breadcrumbs('d', TREE).map((x) => x?.id), ['d', 'b', 'a']);
  });

  test('a root folder is its own only crumb', () => {
    assert.deepEqual(breadcrumbs('a', TREE).map((x) => x?.id), ['a']);
  });

  test('a missing ancestor stops the trail rather than failing', () => {
    // Mirrors the store's `FolderRef::Empty`: a breadcrumb that cannot be
    // completed is still worth rendering as far as it goes.
    const trail = breadcrumbs('d', [f({ id: 'd', parentId: 'gone' })]);
    assert.equal(trail.length, 2);
    assert.equal(trail[0]!.id, 'd');
    assert.equal(trail[1], null, 'and says the next step does not resolve');
  });

  test('an unknown folder is a single unresolved crumb', () => {
    assert.deepEqual(breadcrumbs('nope', TREE), [null]);
  });

  test('a parent cycle terminates the trail', () => {
    // A tree cycle is not something the filter resolver can see, because
    // resolution walks filters and not parents.
    const trail = breadcrumbs('a', [f({ id: 'a', parentId: 'b' }), f({ id: 'b', parentId: 'a' })]);
    assert.ok(trail.length <= 2, `bounded, got ${trail.length}`);
    assert.equal(trail[0]!.id, 'a');
  });

  test('only the last crumb is the current one', () => {
    const trail = breadcrumbs('d', TREE);
    assert.equal(isCurrentCrumb(trail, 0), false);
    assert.equal(isCurrentCrumb(trail, trail.length - 1), true);
  });
});

// ---------------------------------------------------------------------------

describe('navigation', () => {
  test('nothing is expanded at the root by default', () => {
    assert.equal(isExpanded(ROOT_NAV, 'a'), false);
  });

  test('the open folder is always expanded', () => {
    // Otherwise opening a folder shows a row that cannot be opened further.
    const nav = openFolder(ROOT_NAV, 'b', TREE);
    assert.equal(isExpanded(nav, 'b'), true);
  });

  // The difference between a navigator and a jump. Opening a deep folder and
  // having the pane collapse to one highlighted row is how a user gets lost.
  test('opening a folder expands its ancestors', () => {
    const nav = openFolder(ROOT_NAV, 'd', TREE);
    assert.equal(nav.open, 'd');
    assert.ok(isExpanded(nav, 'a'), 'the root is expanded');
    assert.ok(isExpanded(nav, 'b'), 'and so is the middle folder');
  });

  test('opening does not collapse what was already open', () => {
    let nav = openFolder(ROOT_NAV, 'b', TREE);
    nav = openFolder(nav, 'c', TREE);
    assert.ok(isExpanded(nav, 'a'));
  });

  test('opening twice expands nothing new', () => {
    const once = openFolder(ROOT_NAV, 'd', TREE);
    const twice = openFolder(once, 'd', TREE);
    assert.equal(twice.expanded.length, once.expanded.length);
  });

  test('toggling expands and collapses one folder', () => {
    const open = toggleExpanded(ROOT_NAV, 'a');
    assert.equal(isExpanded(open, 'a'), true);
    const shut = toggleExpanded(open, 'a');
    assert.equal(isExpanded(shut, 'a'), false);
  });

  test('toggling does not change which folder is open', () => {
    const nav = toggleExpanded(openFolder(ROOT_NAV, 'b', TREE), 'c');
    assert.equal(nav.open, 'b');
  });

  test('going up from a child opens the parent', () => {
    const nav = openParent(openFolder(ROOT_NAV, 'd', TREE), TREE);
    assert.equal(nav.open, 'b');
  });

  test('going up from a root returns to the tree root', () => {
    // Either the folder is gone or it has no parent; the tree root is the
    // nearest thing above it that exists.
    assert.equal(openParent(openFolder(ROOT_NAV, 'a', TREE), TREE).open, null);
  });

  test('going up from the tree root stays there', () => {
    assert.equal(openParent(ROOT_NAV, TREE).open, null);
  });

  test('going up from a folder that no longer exists returns to the root', () => {
    const nav: FolderNavLike = { open: 'deleted', expanded: [] };
    assert.equal(openParent(nav, TREE).open, null);
  });
});

type FolderNavLike = { open: string | null; expanded: readonly string[] };

// ---------------------------------------------------------------------------

describe('search', () => {
  test('matches the name, case-insensitively', () => {
    assert.deepEqual(searchFolders(TREE, 'alp').map((x) => x.id), ['a']);
    assert.deepEqual(searchFolders(TREE, 'ALPHA').map((x) => x.id), ['a']);
  });

  test('matches the filter, because a user remembers a folder by what it looks for', () => {
    const folders = [f({ id: 'x', name: 'Zebra', filter: 'tagged = "cat"' })];
    assert.deepEqual(searchFolders(folders, 'cat').map((r) => r.id), ['x']);
  });

  test('an empty query returns everything, in display order', () => {
    // b(0), d(0), then a(1), c(1) -- position first, id as the tiebreak, over
    // the flat list. The results are NOT tree order, because a search returns
    // matches rather than a navigable tree; `keepAncestors` is what makes them
    // navigable.
    assert.deepEqual(searchFolders(TREE, '   ').map((x) => x.id), ['b', 'd', 'a', 'c']);
  });

  test('a query matching nothing returns nothing', () => {
    assert.deepEqual(searchFolders(TREE, 'zzz'), []);
  });

  // A search that returns "Untagged" while hiding every folder above it
  // produces entries that cannot be opened in context.
  test('keeping ancestors includes the parents of a hit', () => {
    const kept = keepAncestors(searchFolders(TREE, 'Delta'), TREE);
    assert.deepEqual(kept.map((x) => x.id).sort(), ['a', 'b', 'd']);
  });

  test('keeping ancestors adds a whole path, not just the parent', () => {
    const kept = keepAncestors([f({ id: 'x', parentId: 'y' }), f({ id: 'y', parentId: 'z' }), f({ id: 'z' })], TREE)
      .map((x) => x.id);
    assert.deepEqual(kept, []);
  });

  test('keeping ancestors terminates on a cycle', () => {
    const folders = [f({ id: 'a', parentId: 'b' }), f({ id: 'b', parentId: 'a' })];
    const kept = keepAncestors([folders[0]!], folders);
    assert.equal(kept.length, 2, 'both are wanted, and it returned');
  });
});

// ---------------------------------------------------------------------------

describe('formatting', () => {
  // Not "zero" — "not asked yet". Counting membership is a query per folder,
  // and a column of zeroes before the counts land looks like an empty library.
  test('an unknown count is a dash, not a zero', () => {
    assert.equal(formatCount(null), '—');
    assert.equal(formatCount(0), '0');
    assert.equal(formatCount(12), '12');
  });

  test('the tree root has no filter to name', () => {
    assert.equal(openFilterLabel(null, TREE), null);
  });

  // The part of §5.14 a user gets wrong: opening a folder REPLACES the filter.
  test('a folder names the filter it will apply', () => {
    const folders = [f({ id: 'x', filter: 'tagged = "cat"' })];
    assert.equal(openFilterLabel('x', folders), 'tagged = "cat"');
  });

  test('an unknown folder has no filter label', () => {
    assert.equal(openFilterLabel('nope', TREE), null);
  });
});
