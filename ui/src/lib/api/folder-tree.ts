/**
 * The folder tree, as a view. Spec 9.5 and 5.14, plan T-P5-006 item 16;
 * #1586, #1723.
 *
 * # The store already decided what a folder is
 *
 * `crates/commons-store/src/folders.rs` is done and its header is worth reading
 * before this file: a folder is a name, a filter and a position, and it holds no
 * objects. Membership is recomputed on every open. So there is no membership to
 * compute here, no join table to mirror, and nothing to invalidate when a
 * filter changes — the view's whole job is to render a tree and turn a
 * selection into a filter.
 *
 * What this module adds is the part the store deliberately does not have: the
 * shape a tree takes in a browser, and the decisions about what a user sees
 * when the tree underneath is not well-formed.
 *
 * # A folder holds no objects, so a folder is not a filter of its own kind
 *
 * Opening a folder means "run this folder's filter". The one thing worth being
 * careful about is that this is *replacement*, not intersection: a folder whose
 * filter is `tagged x` shows everything tagged x, and the filter that led you
 * there is not still applied. Anything else means a user cannot get from a
 * folder to a result, because every drill-down narrows to nothing.
 *
 * # The tree is rendered from a FLAT list
 *
 * A flat `Folder[]` and a tree are different shapes, and the tree is the harder
 * one to keep correct: one orphan, one cycle, or one id that appears twice and
 * the whole listing vanishes. So `buildTree` takes the flat list and is total —
 * it puts every folder somewhere, and a folder it cannot place is reported
 * rather than dropped. `Tree.orphans` is that report, and the component shows
 * it, because a folder that silently never appears is the worst outcome a
 * navigator has.
 */

/** A folder, as the view receives it. Mirrors the store's `Folder`. */
export interface Folder {
  readonly id: string;
  readonly name: string;
  /** Never null: a folder whose filter is missing matches nothing. */
  readonly filter: string;
  readonly parentId: string | null;
  readonly position: number;
  readonly icon: string | null;
  /** §5.14: smart collections notify when new items match. Off by default. */
  readonly notify: boolean;
}

/** One node of the rendered tree. */
export interface FolderNode {
  readonly folder: Folder;
  readonly depth: number;
  /** 0-based position among its siblings, in display order. */
  readonly position: number;
  /** How many descendants, at any depth. */
  readonly descendantCount: number;
  /** True when this folder or anything under it has `notify` set. */
  readonly notifies: boolean;
  /** This folder's children, in display order. */
  readonly children: readonly FolderNode[];
}

/**
 * The whole tree, plus what could not be placed in it.
 *
 * `orphans` and `cycles` are reported rather than thrown. A tree view that
 * refuses to render because one folder is misplaced has turned a data problem
 * into a blank page, and the user has no way to get to the folders that are
 * fine.
 */
export interface FolderTree {
  readonly roots: readonly FolderNode[];
  /** Every node, flattened in display order. */
  readonly all: readonly FolderNode[];
  /**
   * Folders whose `parentId` names a folder that is not in the list.
   *
   * A deleted parent is an ordinary event, not corruption, so these are hoisted
   * to the roots rather than hidden: the user can still see and fix them.
   */
  readonly orphans: readonly Folder[];
  /**
   * Ids that took part in a parent cycle.
   *
   * Reported so a user can be told their tree has a loop, rather than the loop
   * silently eating a subtree.
   */
  readonly cycles: readonly string[];
}

/**
 * Build the tree from a flat list.
 *
 * # The order
 *
 * `position` first, id as the tiebreak — the same order the store's
 * `order_by_position_sql` produces, and the same reason: an order the user can
 * change and an order that re-derives itself when something is renamed are
 * different things, and a tree that re-sorts under the cursor is the second.
 * The id tiebreak is what makes it total, so two folders at the same position
 * have a defined order rather than repeating or skipping across a page.
 *
 * # Cycles
 *
 * A `parentId` cycle is reachable by dragging a folder into its own descendant.
 * The store's migration `0018` refuses it at the database, but the view cannot
 * assume the database it is handed is the one that migration protects, and a
 * `while` loop with no visited set is an infinite loop in a browser. The
 * folders in a cycle are reported in `cycles` and attached to the roots, so the
 * rest of the tree renders and the user is told what is wrong.
 */
export function buildTree(folders: readonly Folder[]): FolderTree {
  const byId = new Map<string, Folder>();
  for (const f of folders) byId.set(f.id, f);

  // Display order, computed once and reused: sorting at every level is the
  // thing that turns a 500-folder tree into a visible pause.
  const ordered = [...folders].sort(compareFolders);

  const roots: FolderNode[] = [];
  const orphans: Folder[] = [];
  const cycles: string[] = [];

  // A parent that is not in the list, or that is the folder itself, is not a
  // root -- it is misplaced, and it is reported.
  const parentOf = (f: Folder): Folder | null => {
    if (f.parentId === null) return null;
    if (f.parentId === f.id) {
      cycles.push(f.id);
      return null;
    }
    return byId.get(f.parentId) ?? null;
  };

  // Depth-first with a visited set, so a cycle terminates.
  const visited = new Set<string>();
  const inCycle = new Set<string>();

  const place = (f: Folder, depth: number, position: number): FolderNode | null => {
    if (visited.has(f.id)) return null;

    // Depth is bounded rather than only cycle-checked, for the same reason the
    // store is: a visited set makes a cycle terminate but a long legitimate
    // chain is fine and must not be cut. There is no upper bound the store
    // enforces on tree depth, so this only guards a pathological input.
    if (depth > 64) {
      inCycle.add(f.id);
      cycles.push(f.id);
      return null;
    }

    visited.add(f.id);

    const children = ordered.filter((c) => c.parentId === f.id);
    const childNodes: FolderNode[] = [];
    for (const [i, c] of children.entries()) {
      if (visited.has(c.id)) {
        // A child that is already placed is a cycle back into this subtree.
        inCycle.add(c.id);
        cycles.push(c.id);
        continue;
      }
      const node = place(c, depth + 1, i);
      if (node !== null) childNodes.push(node);
    }

    return {
      folder: f,
      depth,
      position,
      descendantCount: childNodes.reduce((n, c) => n + 1 + c.descendantCount, 0),
      notifies: f.notify || childNodes.some((c) => c.notifies),
      children: childNodes,
    };
  };

  // Only genuine roots are placed here. A folder whose parent resolved has
  // already been placed by its parent's `place` call, and placing it AGAIN from
  // this loop puts it at the top level as well -- which is how a tree ends up
  // rendering every folder twice, once nested and once loose.
  for (const [i, f] of ordered.entries()) {
    if (visited.has(f.id)) continue;
    const parent = parentOf(f);
    if (f.parentId !== null) {
      if (parent === null) orphans.push(f);
      // A parent that RESOLVED is reached through that parent, above -- unless
      // the parent is itself unreachable, which is the case of a cycle with no
      // root. `reachable` answers exactly that, and without the check a
      // two-folder cycle is placed by nobody: no folder is a root, so the loop
      // skips both, and both silently vanish from the navigator.
      if (parent !== null && reachable(parent.id)) continue;
      if (parent !== null) {
        // Unreachable, and we are standing on the cycle. Place it here so it is
        // visible, and record the whole cycle.
        for (const id of cycleFrom(f, byId)) cycles.push(id);
      }
    }
    const node = place(f, 0, i);
    if (node !== null) roots.push(node);
  }

  return { roots, all: flatten(roots), orphans, cycles: [...new Set(cycles)] };

  /**
   * Can this folder be reached from a root?
   *
   * Walking up the `parentId` chain, bounded by a visited set. A folder whose
   * chain never reaches a null parent is in a cycle, and a cycle has no root --
   * which is why this exists: without it, `a -> b -> a` is skipped by the root
   * loop on both iterations and both folders disappear.
   */
  function reachable(id: string): boolean {
    const seen = new Set<string>([id]);
    let cursor: string | null = byId.get(id)?.parentId ?? null;
    while (cursor !== null) {
      // Coming back to ANY folder already on this chain -- including the one we
      // started from -- means the chain loops, and a loop has no root, so
      // nothing above us is reachable. There is no case where returning to the
      // start is "fine": the chain from a folder to a root is a path, and a path
      // that returns to its start is a cycle.
      if (seen.has(cursor)) return false;
      seen.add(cursor);
      const next: Folder | undefined = byId.get(cursor);
      if (next === undefined) return false; // missing parent
      cursor = next.parentId;
    }
    return true; // reached a null parent, so a root is above us
  }
}

/**
 * Every folder in the cycle `from` sits in, `from` included.
 *
 * Only used to populate `cycles`, so the user can be told their tree has a loop
 * rather than the loop silently eating a subtree. Bounded, for the same reason
 * `breadcrumbs` is.
 */
function cycleFrom(from: Folder, byId: ReadonlyMap<string, Folder>): string[] {
  const out: string[] = [from.id];
  const seen = new Set<string>([from.id]);
  let cursor = from.parentId;
  while (cursor !== null && !seen.has(cursor)) {
    seen.add(cursor);
    out.push(cursor);
    const next: Folder | undefined = byId.get(cursor);
    if (next === undefined) break;
    cursor = next.parentId;
  }
  return seen.has(cursor ?? '') ? out : [from.id];
}

/**
 * Every node in display order, parents before children.
 *
 * Depth-first, so the order is the order the sidebar renders rows in. This is
 * what a search walks and what a keyboard traversal moves through, and it is the
 * reason `buildTree` can hand back a flat list: a consumer that only needs the
 * order should not have to re-derive it.
 */
export function flatten(roots: readonly FolderNode[]): FolderNode[] {
  const out: FolderNode[] = [];
  const walk = (nodes: readonly FolderNode[]): void => {
    for (const n of nodes) {
      out.push(n);
      walk(n.children);
    }
  };
  walk(roots);
  return out;
}

/** Only the nodes on the path to `id`, for auto-expanding a search hit. */
export function pathTo(id: string, roots: readonly FolderNode[]): FolderNode[] {
  const walk = (nodes: readonly FolderNode[]): FolderNode[] | null => {
    for (const n of nodes) {
      if (n.folder.id === id) return [n];
      const below = walk(n.children);
      if (below !== null) return [n, ...below];
    }
    return null;
  };
  return walk(roots) ?? [];
}

/** `position` first, id as the tiebreak. The store's order, on this side. */
export function compareFolders(a: Folder, b: Folder): number {
  if (a.position !== b.position) return a.position - b.position;
  return a.id < b.id ? -1 : a.id > b.id ? 1 : 0;
}

// ---------------------------------------------------------------------------
// Breadcrumbs (#1723)
// ---------------------------------------------------------------------------

/**
 * The trail from a folder up to its root, nearest first.
 *
 * Mirrors the store's `ancestry`, including the decision that a missing parent
 * is `Empty` rather than an error: a breadcrumb that cannot be completed is
 * still worth rendering as far as it goes, and a tree whose breadcrumbs fail to
 * appear because one ancestor was deleted is worse than one that stops early.
 */
export function breadcrumbs(folderId: string, folders: readonly Folder[]): (Folder | null)[] {
  const byId = new Map(folders.map((f) => [f.id, f]));
  const out: (Folder | null)[] = [];
  const seen = new Set<string>([folderId]);
  let cursor: string | null = folderId;

  while (cursor !== null) {
    const f = byId.get(cursor);
    if (f === undefined) {
      out.push(null);
      break;
    }
    out.push(f);
    cursor = f.parentId;
    if (cursor !== null) {
      // Belt and braces, exactly as the store does it: a tree cycle is not
      // something the filter resolver can see, because resolution walks
      // filters and not parents.
      if (seen.has(cursor)) break;
      seen.add(cursor);
    }
  }
  return out;
}

/**
 * Whether a breadcrumb is the one the user is currently in.
 *
 * The last crumb, and the only one that is a link back rather than a label.
 */
export function isCurrentCrumb(trail: readonly (Folder | null)[], i: number): boolean {
  return i === trail.length - 1;
}

// ---------------------------------------------------------------------------
// Navigation state
// ---------------------------------------------------------------------------

/** What the folder pane is showing. */
export interface FolderNav {
  /** The open folder, or `null` for the tree root. */
  readonly open: string | null;
  /** Which ancestors are expanded. A closed folder's children are not shown. */
  readonly expanded: readonly string[];
}

export const ROOT_NAV: FolderNav = { open: null, expanded: [] };

/** Is this folder's children shown? The root is always expanded. */
export function isExpanded(nav: FolderNav, id: string): boolean {
  if (nav.open === id) return true;
  if (id === nav.open) return true;
  return nav.expanded.includes(id);
}

/**
 * Open a folder: it becomes `open`, and every ancestor expands so the user can
 * see where they are.
 *
 * Expanding the ancestors rather than only the folder is the difference between
 * a navigator and a jump. Opening a deep folder and having the pane collapse to
 * a single highlighted row is how a user gets lost.
 */
export function openFolder(
  nav: FolderNav,
  id: string,
  folders: readonly Folder[],
): FolderNav {
  const trail = breadcrumbs(id, folders);
  // The trail is nearest-first and includes the folder itself, so the
  // ancestors are everything but the first entry.
  const ancestors = trail.slice(1).filter((f): f is Folder => f !== null).map((f) => f.id);
  const merged = [...new Set([...nav.expanded, ...ancestors])];
  return { open: id, expanded: merged };
}

/** Toggle one folder's expansion, leaving `open` alone. */
export function toggleExpanded(nav: FolderNav, id: string): FolderNav {
  return {
    open: nav.open,
    expanded: nav.expanded.includes(id)
      ? nav.expanded.filter((x) => x !== id)
      : [...nav.expanded, id]
  };
}

/** Go up one level. The root is not above anything, so it stays put. */
export function openParent(nav: FolderNav, folders: readonly Folder[]): FolderNav {
  if (nav.open === null) return nav;
  const byId = new Map(folders.map((f) => [f.id, f]));
  const current = byId.get(nav.open);
  if (current === undefined || current.parentId === null) {
    // Either the folder is gone or it is a root: either way the tree root is
    // the nearest thing above it that exists.
    return ROOT_NAV;
  }
  return openFolder(nav, current.parentId, folders);
}

// ---------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------

/**
 * The folders whose name or filter mentions `q`.
 *
 * A substring match, case-insensitive, over the name and the filter text. The
 * filter is searched because a user who remembers a folder as "the one that
 * looks for tagged x" is searching for it by that, and a name-only search
 * answers "no results" for a folder that is right there.
 */
export function searchFolders(folders: readonly Folder[], q: string): Folder[] {
  const needle = q.trim().toLowerCase();
  if (needle === '') return [...folders].sort(compareFolders);
  return [...folders]
    .filter((f) => f.name.toLowerCase().includes(needle) || f.filter.toLowerCase().includes(needle))
    .sort(compareFolders);
}

/**
 * The folders that must be shown for a set of results to be reachable.
 *
 * A search that returns "Untagged" while hiding every folder above it produces
 * a list whose entries cannot be opened in context, so the user has to go and
 * find where it lives. `keepAncestors` returns the matches plus their ancestors,
 * which is what makes a hit navigable.
 */
export function keepAncestors(matches: readonly Folder[], folders: readonly Folder[]): Folder[] {
  const wanted = new Set(matches.map((f) => f.id));
  const byId = new Map(folders.map((f) => [f.id, f]));
  // Add each match's ancestors. Bounded by the same visited set as the
  // breadcrumbs, because a cycle would otherwise loop here too.
  for (const m of matches) {
    let cursor = m.parentId;
    const seen = new Set<string>([m.id]);
    while (cursor !== null && !seen.has(cursor)) {
      seen.add(cursor);
      wanted.add(cursor);
      const parent: Folder | undefined = byId.get(cursor);
      cursor = parent?.parentId ?? null;
    }
  }
  return folders.filter((f) => wanted.has(f.id)).sort(compareFolders);
}

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

/**
 * How many items a folder holds, for the sidebar.
 *
 * `null` is not "zero" — it is "not asked yet". The membership is recomputed on
 * every open, so counting it is a query per folder, and rendering a row of
 * zeroes before the counts land looks like a library that is empty. A dash is
 * the honest rendering of "unknown".
 */
export function formatCount(n: number | null): string {
  return n === null ? '—' : String(n);
}

/**
 * The filter of the folder the user is in, for the content pane's heading.
 *
 * `null` at the tree root, because "everything" is the answer and there is no
 * filter to name. The heading is what tells the user that opening a folder
 * REPLACED the filter rather than narrowing it, which is the part of §5.14 a
 * user gets wrong.
 */
export function openFilterLabel(folderId: string | null, folders: readonly Folder[]): string | null {
  if (folderId === null) return null;
  const f = folders.find((x) => x.id === folderId);
  return f === undefined ? null : f.filter;
}
