/*
 * Folders: a saved query with a name, a parent, and an order.
 *
 * T-P5-006 item 6. Spec §5.14 and §9.5; plan §T-P5-006 item 6. The decisions
 * and their reasons are in docs/spec/t-p5-006-folders.md; this file is the
 * mechanism.
 *
 * # A folder holds no objects
 *
 * §5.14 is unusually firm: a smart collection is "the answer to #1029
 * (folder-like structure) *without pretending folders are the right model*". A
 * folder is a name, a filter, and a position in a tree. Membership is recomputed
 * every time the folder is opened, and two folders may match the same object.
 *
 * So there is no `folder_members` table, and no object column. An object is in a
 * folder because it matches that folder's filter and for no other reason. That
 * is what makes "re-evaluated on every open" true rather than aspirational: an
 * object tagged after the folder was created appears in it with no write of any
 * kind, and editing a filter takes effect on the next open rather than the next
 * time something happens to touch every member.
 *
 * # The expansion happens before the filter is compiled
 *
 * `Filter::Saved { id }` already exists in the AST. It used to compile to
 * `o.saved_filter_ids LIKE ?` — a column that no migration creates, so any
 * filter naming a folder failed at the database with "no such column". Nothing
 * caught it because the only test touching `Filter::Saved` asserted its serde
 * shape and never ran the SQL. It now returns
 * `FilterError::UnresolvedSavedReference`, which names the missing step rather
 * than a column that does not exist.
 *
 * The fix is not to add the column. That column is a cache of the filter's
 * answer stored on the row, which is the join-table problem again with the
 * invalidation cost moved somewhere less visible: every write path in the app
 * would have to keep it current, and a filter edit would leave stale membership
 * until each member was touched.
 *
 * Instead the reference is resolved *before* compilation, by a pass that has
 * store access and the AST does not. `Filter::compile` is deliberately a pure
 * function over the tree — it has no database handle and must keep none, since
 * a filter that could read the database would make every query's behaviour
 * depend on connection state. So resolution is a separate, earlier step:
 *
 *     resolve(filter, store) -> filter with no `Saved` nodes
 *     compile(resolved)      -> SQL
 *
 * The result is that the AST never grows a database dependency, and that
 * "a folder is re-evaluated on every open" is enforced by the shape of the code
 * rather than by remembering to invalidate something.
 *
 * # Cycles
 *
 * A folder may contain folders (§9.5's tree, #1723's breadcrumbs), so a filter
 * may reference a folder that transitively contains it. That is reachable by
 * editing a filter to name an ancestor — a user action, not corruption — and it
 * gets a defined answer rather than a stack overflow or a query the server
 * cannot finish: a reference already on the expansion path is cut, and the cut
 * term becomes `Or([])`.
 *
 * `Or([])` and not `And([])`, because the choice is load-bearing. `And([])`
 * compiles to `1 = 1`, so a cycle there means a folder that names itself
 * matches the *whole library* — every object the caller can see, the filter
 * ignored — and it looks fine, because a folder with a lot in it is not
 * obviously wrong. `Or([])` compiles to `1 = 0`, so the cyclic term contributes
 * nothing and the rest of the filter stands: `tagged x OR <cycle>` matches
 * exactly what `tagged x` does.
 *
 * "Match nothing" and "leave it alone" are different answers, and only the first
 * is safe. A cycle through a filter that says "tagged X or in this folder" has
 * to exclude the folder term, or the object satisfies the predicate by way of
 * itself.
 *
 * The tree side of the same rule lives in migration `0018`, as a trigger that
 * refuses a reparent which would put a folder inside its own descendant. That is
 * a separate failure from a filter cycle — one is a bad row, the other a bad
 * query — and they are enforced separately for that reason.
 */

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use commons_core::Role;

use crate::filter_ast::Filter;

/// How deep a chain of folder references may expand.
///
/// A depth limit, not a visited-set alone, for two reasons. A visited set makes
/// a *direct* cycle terminate but a diamond (two folders referencing the same
/// third) report a false cycle, and a diamond is ordinary. A depth limit
/// terminates everything, including a cycle the code cannot see because it
/// expanded through a filter the resolver did not rewrite.
pub const MAX_FOLDER_DEPTH: u8 = 16;

/// A folder, as stored.
///
/// `PartialEq` without `Eq`: `Filter` contains `Value`, which has a float
/// variant, and floats are not `Eq`. Deriving `Eq` here would be a lie the
/// compiler catches, and the alternative -- making `Value` `Eq` by dropping the
/// float or wrapping it in an ordered decimal -- is a much larger change to a
/// type that is not this file's to redesign.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Folder {
    pub id: String,
    pub name: String,
    /// Never null. A folder whose filter is missing is a folder that matches
    /// nothing, and `Option<Filter>` would let a NULL reach the query where
    /// "no filter" and "no constraint" are the same SQL — the one reading of a
    /// missing filter nobody would choose.
    pub filter: Filter,
    pub parent_id: Option<String>,
    pub position: i32,
    pub icon: Option<String>,
    /// Smart collections notify when new items match (§5.14). Off by default:
    /// a subscription that defaulted on would mean a background job per folder
    /// for every user who ever made one.
    #[serde(default)]
    pub notify: bool,
}

/// Where a folder's filter comes from.
///
/// The resolver's only input, and a trait rather than a concrete store so the
/// expansion rules can be tested against a literal map with no database — the
/// same reason `filter_ast` takes a `CallerId` and not a pool.
pub trait FolderSource {
    /// The filter stored for `id`, or `None` if there is no such folder.
    fn filter_of(&self, id: &str) -> Result<Option<Filter>, FolderError>;
}

/// The error cases resolution can hit. Distinct from `FilterError`, which is
/// about compilation; mixing them would mean a missing folder was reported as a
/// parse error, which is the kind of message that sends a user looking in the
/// wrong place.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum FolderError {
    #[error("no folder with id {0:?}")]
    NotFound(String),
    #[error("folder {id:?} references itself, directly or through a parent")]
    Cycle { id: String },
    #[error("folder references nested more than {MAX_FOLDER_DEPTH} deep")]
    TooDeep,
    #[error(transparent)]
    Filter(#[from] crate::filter_ast::FilterError),
}

/// What resolution did, for the log and for a caller's own reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ResolutionReport {
    /// References that became a folder's filter.
    pub expanded: usize,
    /// References that were already on the expansion path and became
    /// `Or([])`.
    ///
    /// Separate from `expanded` rather than folded into one number, because a
    /// single total hides exactly the thing worth knowing: a caller logging
    /// "resolved 3 references" when one was a cycle is reporting a number that
    /// makes a broken folder look routine. A non-zero `cut` is the signal.
    pub cut: usize,
}

impl ResolutionReport {
    /// Every reference encountered, expanded or cut.
    pub fn total(&self) -> usize {
        self.expanded + self.cut
    }

    /// Did anything get dropped? A caller can branch on this to log a warning
    /// rather than an info line.
    pub fn dropped_anything(&self) -> bool {
        self.cut > 0
    }
}

/// Replace every `Saved` reference with the folder's own filter.
///
/// `path` is the set of folder ids already on the expansion path. A reference to
/// one of them resolves to `match nothing` rather than to an error, because a
/// cycle is a thing a user can create by editing a filter and the enclosing view
/// should still open — see the module comment.
pub fn resolve_saved(
    filter: &Filter,
    source: &impl FolderSource,
    path: &mut HashSet<String>,
) -> Result<(Filter, ResolutionReport), FolderError> {
    let mut report = ResolutionReport::default();
    let out = rewrite(filter, source, path, 0, &mut report)?;
    Ok((out, report))
}

fn rewrite(
    filter: &Filter,
    source: &impl FolderSource,
    path: &mut HashSet<String>,
    depth: u8,
    report: &mut ResolutionReport,
) -> Result<Filter, FolderError> {
    if depth > MAX_FOLDER_DEPTH {
        return Err(FolderError::TooDeep);
    }
    Ok(match filter {
        Filter::And(children) => {
            let mut out = Vec::with_capacity(children.len());
            for c in children {
                out.push(rewrite(c, source, path, depth + 1, report)?);
            }
            Filter::And(out)
        }
        Filter::Or(children) => {
            let mut out = Vec::with_capacity(children.len());
            for c in children {
                out.push(rewrite(c, source, path, depth + 1, report)?);
            }
            Filter::Or(out)
        }
        Filter::Not(inner) => {
            Filter::Not(Box::new(rewrite(inner, source, path, depth + 1, report)?))
        }
        Filter::Facet {
            kind,
            field,
            op,
            values,
        } => Filter::Facet {
            kind: *kind,
            field: field.clone(),
            op: *op,
            values: values.clone(),
        },
        Filter::Text { q } => Filter::Text { q: q.clone() },
        Filter::All => Filter::All,

        Filter::Saved { id } => {
            // A cycle resolves to "matches nothing", not to an error. The
            // enclosing query still runs; the folder simply contributes no
            // members, which is a state a user can see and a developer can read
            // in the resolution count.
            if path.contains(id) {
                // `Or([])`, and the choice is load-bearing.
                //
                // `And([])` compiles to `1 = 1` and `Or([])` to `1 = 0` (see
                // `filter_ast`). A cycle returning `And([])` means a folder
                // whose filter names itself matches the *whole library* -- every
                // object the caller can see, the filter ignored -- and it looks
                // fine, because a folder with a lot in it is not obviously
                // wrong. Nobody finds out until they notice that a filter on
                // `tagged = "x"` is returning things not tagged x.
                //
                // `Or([])` is the honest answer: the cyclic term contributes
                // nothing and the rest of the filter stands, so
                // `tagged x OR <cycle>` matches exactly what `tagged x` does.
                report.cut += 1;
                return Ok(Filter::Or(Vec::new()));
            }
            let Some(inner) = source.filter_of(id)? else {
                return Err(FolderError::NotFound(id.clone()));
            };
            report.expanded += 1;
            path.insert(id.clone());
            // Expanded in place rather than appended, so `tagged X AND (folder)`
            // becomes `tagged X AND <the folder's filter>` and the user's
            // explicit terms keep their meaning.
            let expanded = rewrite(&inner, source, path, depth + 1, report)?;
            path.remove(id);
            expanded
        }
    })
}

/// The order siblings are shown in.
///
/// `position` alone, with the id as a tiebreak, rather than by name. An order
/// the user can change and an order that re-derives itself when something is
/// renamed are different things, and a tree that re-sorts under the cursor is
/// the second. The id tiebreak is what makes the order total: two folders at the
/// same position otherwise have no defined order, and a listing with no defined
/// order can repeat or skip a row across a page boundary.
pub fn order_by_position_sql() -> &'static str {
    "ORDER BY f.position ASC, f.id ASC"
}

/// `parent_id = ?`, or the roots when `None`.
///
/// Written here rather than inlined at each call site because the roots case is
/// the one that gets forgotten, and a missing root clause does not return the
/// wrong rows — it returns every row.
pub fn parent_predicate_sql() -> &'static str {
    "f.parent_id IS NULL"
}

/// One step of a breadcrumb trail.
///
/// No `Eq`: the payload is a `Folder`, which contains a `Filter` with a float
/// variant, so it is not `Eq`. Nothing needs the stronger bound here -- a
/// breadcrumb trail is compared for rendering, not put in a `HashSet`.
#[derive(Debug, Clone, PartialEq)]
pub enum FolderRef {
    /// A folder that exists.
    Expanded(Folder),
    /// A parent id that no longer resolves. A folder whose ancestor was deleted
    /// still gets a trail, just a short one.
    Empty,
}

/// Walk from a folder to its root, for breadcrumbs (#1723).
///
/// `None` for a missing parent rather than a `Err`: a breadcrumb that cannot be
/// completed is still worth rendering as far as it goes, and a tree whose
/// breadcrumbs fail to appear because one ancestor was deleted is worse than one
/// that stops early.
pub fn ancestry(folder: &Folder, parent_of: &impl Fn(&str) -> Option<Folder>) -> Vec<FolderRef> {
    let mut out = vec![FolderRef::Expanded(folder.clone())];
    let mut seen = HashSet::from([folder.id.clone()]);
    let mut cursor = folder.parent_id.clone();
    let mut steps = 0u8;
    while let Some(id) = cursor {
        // Belt and braces: a `parent_id` cycle in the *tree* (as opposed to in a
        // filter) is not something the resolver can see, because resolution
        // walks filters and not parents. A user can create one by dragging a
        // folder into its own descendant.
        if steps > MAX_FOLDER_DEPTH || !seen.insert(id.clone()) {
            break;
        }
        match parent_of(&id) {
            Some(p) => {
                // Read the cursor before the move: the folder goes into the
                // trail by value, and asking it anything afterwards is a
                // use-after-move.
                cursor = p.parent_id.clone();
                out.push(FolderRef::Expanded(p));
            }
            None => {
                out.push(FolderRef::Empty);
                break;
            }
        }
        steps += 1;
    }
    out
}

/// Whether a caller may create or edit a folder.
///
/// `may_curate`, not `may_see_restricted`. The two are unrelated axes: the first
/// is what a caller may *change* and the second what they may *view*. Making a
/// folder is curation -- a name and a query, both of which are the user's own
/// and neither of which touches anyone's content -- and a Contributor curates.
/// Gating folder creation on moderation privileges hid the button for every
/// contributor with no message saying why.
///
/// Read is not filtered at all, and deliberately so: the consent conjunction
/// already applies to every object query, so a folder's *members* are filtered
/// without reference to the folder. Filtering the folder's visibility on its
/// contents would make the tree change as objects are tagged, which is the
/// opposite of what a stable navigation is for.
pub fn may_write(role: Role) -> bool {
    role.may_curate()
}
