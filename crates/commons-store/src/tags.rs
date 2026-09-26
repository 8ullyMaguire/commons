//! §5.15 and §9.4 — the tag system.
//!
//! Four things live here, and the order matters because the first one is what
//! makes the rest honest.
//!
//! **1. A namespace, and a tag without one cannot be written.** §5.15 calls the
//! namespace "the honesty mechanism for ML tagging": a tag a model proposed and
//! a tag a person typed are *different claims* about a body of work, and a
//! library that cannot tell them apart is lying to whoever is looking at it. So
//! this is not a column with a default that a caller may set to anything — the
//! constructor that takes a model name is the only way to make an ML tag, and it
//! writes the `ml:` prefix itself. There is no API that takes a raw namespace
//! string for a machine's opinion, because a caller that had one would eventually
//! pass `"canonical"` to it.
//!
//! **2. A tree, with cycles refused.** Tags nest (`parent_id`), and a cycle makes
//! a breadcrumb infinite and a recursive delete a stack overflow. The guard is
//! in [`TagTree::set_parent`] rather than in the database, because a database
//! check would have to be a recursive CTE — which is in neither engine's
//! portable subset and would make the two disagree.
//!
//! **3. Breadcrumbs** (#1723) — the path from the root to a tag, as a list, so
//! the UI can render `parent / child / grandchild` and so a caller can ask
//! "is this tag under that one" without walking the tree itself.
//!
//! **4. Typed attributes** (#3400) and **groups** (#3469) — a tag can say
//! something about its subject, and four value columns rather than one text
//! column so `between` and `is null` are real operations.

use crate::db::{Result, Store, StoreError};
use crate::search::Field;
use std::collections::{BTreeMap, HashSet};

/// Where a tag came from. §5.15's honesty mechanism.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Namespace {
    /// A person chose this name. The default, and the only namespace a
    /// [`Tag::new`] can produce.
    Canonical,
    /// Imported from a remote site: `site:stashbox.org`, `site:theporndb`.
    Site(String),
    /// Proposed by a model: `ml:tagger`. The model name is kept because
    /// "which model said this" is the question a person asks when an ML tag is
    /// wrong, and one `ml:` bucket cannot answer it.
    Ml(String),
    /// Added by a specific person: `user:<id>`.
    User(String),
}

impl Namespace {
    /// The stored form. `canonical` is bare, everything else is prefixed.
    ///
    /// Prefixing rather than a separate `kind` column so a namespace is one
    /// value in one place, and `namespace LIKE 'ml:%'` is the tagger's query
    /// (§8.2's "the confidence is visible" has a list to be visible in).
    pub fn as_str(&self) -> String {
        match self {
            Namespace::Canonical => "canonical".to_string(),
            Namespace::Site(host) => format!("site:{host}"),
            Namespace::Ml(model) => format!("ml:{model}"),
            Namespace::User(id) => format!("user:{id}"),
        }
    }

    /// Parse the stored form.
    ///
    /// An unrecognised prefix is `Canonical` rather than an error or a new
    /// variant. Two peers of one library must agree about what a tag is, and
    /// the failure mode of an unknown prefix is not "this tag is unreadable" —
    /// it is "this tag is silently promoted to something a person chose", which
    /// is precisely the dishonesty the namespace exists to prevent. So an
    /// unparseable namespace is *not* canonical: it keeps its raw form in a
    /// `Site`-shaped variant that cannot be mistaken for a person's choice, and
    /// [`Namespace::is_canonical`] is false for it.
    pub fn parse(s: &str) -> Namespace {
        match s.split_once(':') {
            Some(("ml", model)) => Namespace::Ml(model.to_string()),
            Some(("user", id)) => Namespace::User(id.to_string()),
            Some(("site", host)) => Namespace::Site(host.to_string()),
            // A prefix nobody has heard of. Kept verbatim under `Site` so it
            // is neither lost nor promoted; `as_str` round-trips it.
            Some((_, rest)) => Namespace::Site(rest.to_string()),
            None => Namespace::Canonical,
        }
    }

    /// Whether a person chose this name.
    ///
    /// The single predicate the UI needs to decide whether an ML tag needs
    /// marking, and the reason [`Namespace::parse`] does not collapse an
    /// unknown prefix into `Canonical`.
    pub fn is_canonical(&self) -> bool {
        matches!(self, Namespace::Canonical)
    }

    /// Whether a machine proposed this.
    pub fn is_machine(&self) -> bool {
        matches!(self, Namespace::Ml(_))
    }
}

/// A tag, as stored.
#[derive(Debug, Clone, PartialEq)]
pub struct Tag {
    pub id: String,
    pub name: String,
    pub parent_id: Option<String>,
    pub namespace: Namespace,
    pub color: Option<String>,
    /// #2973. `1.0` for a tag that says nothing more than its name, and
    /// whatever the person applying it thought — a tag that is true of a tenth
    /// of a library is a different claim from one that is true of all of it.
    pub importance: f64,
    /// `None` for a tag a person applied. A confidence of `1.0` would be a
    /// claim that a person's tag is certainly right, which is not a thing
    /// anybody knows.
    pub confidence: Option<f64>,
    pub created_at: String,
}

/// A tag application, as stored.
#[derive(Debug, Clone, PartialEq)]
pub struct TagApplication {
    pub object_id: String,
    pub tag_id: String,
    /// §8.2: ML tags are proposals and the confidence is visible. `None` for a
    /// person-applied tag.
    pub confidence: Option<f64>,
    /// Free text: the model name, the site, the user's id. Deliberately a
    /// string rather than the namespace, because the namespace says *what kind*
    /// of source and this says *which one*.
    pub source: Option<String>,
    /// When the application was written, ISO-8601 UTC. `None` for a row that
    /// predates migration 0016, which added the column to a table 0001 created
    /// without a timestamp of any kind. Not an empty string and not the epoch:
    /// those are two more answers that are not "the time is not known".
    pub created_at: Option<String>,
}

/// The errors a tag operation can report.
///
/// Distinct from [`StoreError`] because these are all *decisions* rather than
/// failures, and a caller that gets one has done something it should not have
/// and needs to know which thing.
/// `Eq` and `Hash` are deliberately absent: [`TagError::BadConfidence`] and
/// [`TagError::AttributeType`] carry an `f64`, and deriving `Eq` for a type
/// containing one is a lie the compiler will not catch and every
/// `HashSet<TagError>` will act on.
#[derive(Debug, Clone, PartialEq)]
pub enum TagError {
    /// A cycle: the proposed parent is this tag or already under it.
    Cycle { tag: String, parent: String },
    /// The name is empty or only whitespace. A blank tag is invisible in a
    /// sidebar and unremovable by clicking it, and `name` is the only
    /// human-facing part of the row.
    BlankName,
    /// The namespace is malformed — an `ml:` with no model, say. Refused at
    /// the boundary rather than stored, because a stored `ml:` with no model
    /// cannot answer "which model".
    BadNamespace(String),
    /// The tag does not exist.
    NotFound(String),
    /// The attribute's value does not match its declared type. Carries the
    /// attribute and the value so the caller can say which one.
    AttributeType { attribute: String, detail: String },
    /// The attribute's type is not declared.
    UndeclaredAttribute(String),
    /// The value is not one of the declared options.
    NotAnOption { attribute: String, value: String },
    /// A cycle in the group membership would make a group contain itself.
    /// Groups do not nest in this model, so this is unreachable through the
    /// API; it exists so the check is in the type rather than assumed.
    GroupCycle(String),
    /// The confidence is outside `0.0..=1.0`, or is `NaN`.
    BadConfidence(f64),
}

impl std::fmt::Display for TagError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TagError::Cycle { tag, parent } => {
                write!(
                    f,
                    "tag '{tag}' cannot be placed under '{parent}': that is a cycle"
                )
            }
            TagError::BlankName => write!(f, "a tag name may not be blank"),
            TagError::BadNamespace(ns) => {
                write!(f, "'{ns}' is not a namespace: it must be 'canonical', 'ml:<model>', 'site:<host>' or 'user:<id>'")
            }
            TagError::NotFound(id) => write!(f, "no such tag: '{id}'"),
            TagError::AttributeType { attribute, detail } => {
                write!(f, "attribute '{attribute}': {detail}")
            }
            TagError::UndeclaredAttribute(name) => {
                write!(f, "attribute '{name}' has no declared type")
            }
            TagError::NotAnOption { attribute, value } => {
                write!(f, "'{value}' is not a declared value for '{attribute}'")
            }
            TagError::GroupCycle(name) => write!(f, "group '{name}' would contain itself"),
            TagError::BadConfidence(c) => {
                write!(f, "confidence {c} is not a number in 0.0..=1.0")
            }
        }
    }
}

impl std::error::Error for TagError {}

impl From<TagError> for StoreError {
    fn from(e: TagError) -> StoreError {
        // `Invalid { what, why }` rather than a string, so a caller matching on
        // the store error can tell *which* kind of refusal this was. The `what`
        // is the variant name, which is the stable part; the `why` is the
        // human-readable detail. A caller that only has a `StoreError` and wants
        // to show "that tag already has that parent" can read the `why`, and a
        // caller matching programmatically has the `what`.
        let what: &'static str = match &e {
            TagError::Cycle { .. } => "tag cycle",
            TagError::BlankName => "blank tag name",
            TagError::BadNamespace(_) => "malformed namespace",
            TagError::NotFound(_) => "missing tag",
            TagError::AttributeType { .. } => "attribute type mismatch",
            TagError::UndeclaredAttribute(_) => "undeclared attribute",
            TagError::NotAnOption { .. } => "undeclared attribute value",
            TagError::GroupCycle(_) => "tag group cycle",
            TagError::BadConfidence(_) => "confidence out of range",
        };
        StoreError::Invalid {
            what,
            why: e.to_string(),
        }
    }
}

/// A tag as a tree node: the row plus its children.
///
/// Separate from [`Tag`] because a tree node knows its children and a tag
/// does not, and a `Tag` carrying an empty `Vec` most of the time is a field
/// that is only sometimes the answer.
#[derive(Debug, Clone, PartialEq)]
pub struct TagNode {
    pub tag: Tag,
    pub children: Vec<TagNode>,
}

impl TagNode {
    /// Breadcrumbs (#1723): the path from the root to this node, root first.
    ///
    /// Returns the full path rather than the direct parent, because the two
    /// questions a caller actually has are "render `a / b / c`" and "is this tag
    /// anywhere under that one", and both want the whole path — the second
    /// because "under" in a tree is a question about the path, not about
    /// `parent_id`.
    pub fn breadcrumbs(&self) -> Vec<&Tag> {
        // The path, which is the left spine: follow the first child while there
        // is one, and stop.
        //
        // The first version walked the *whole* subtree pre-order and reversed
        // the result, which is the root-first order for a list of siblings but
        // not for a path: reversing a pre-order walk of a 4-deep chain gives
        // `d a b c`, and reversing a 3-deep one gives `c b a` — so it was right
        // for the root and wrong for everything below it, and a test that
        // asserted on a 2-deep tree passed.
        let mut out: Vec<&Tag> = Vec::new();
        let mut cursor = Some(self);
        while let Some(n) = cursor {
            out.push(&n.tag);
            cursor = n.children.first();
        }
        out
    }

    /// This node and everything under it, as a flat list in depth-first order.
    ///
    /// The shape a rename or a delete needs, and the shape a "count everything
    /// under here" number needs. Iterative rather than recursive so a deep
    /// tree cannot overflow the stack — the depth is a person's doing, and a
    /// hundred-deep taxonomy is a taxonomy somebody would build.
    pub fn flatten(&self) -> Vec<&Tag> {
        let mut out: Vec<&Tag> = Vec::new();
        let mut stack: Vec<&TagNode> = vec![self];
        while let Some(n) = stack.pop() {
            out.push(&n.tag);
            // Reversed, so the pop order comes out left-to-right.
            for c in n.children.iter().rev() {
                stack.push(c);
            }
        }
        out
    }

    /// Depth, root at zero.
    pub fn depth(&self) -> usize {
        self.breadcrumbs().len() - 1
    }

    /// The same node, re-rooted under a path of ancestors.
    ///
    /// The ancestors are attached as a *left spine* — each becomes a single-child
    /// parent of the next — so the tree shape is unchanged and
    /// [`TagNode::breadcrumbs`] needs no separate path field that could disagree
    /// with the structure. A node with an ancestors list is a node whose root
    /// chain has been made explicit; a node without one is a subtree cut at its
    /// own root, which is what [`crate::Store::tag_tree`] hands back and what
    /// makes `breadcrumbs` on a whole-tree node correct already.
    ///
    /// The ancestors are consumed root-first, so the spine is built from the
    /// root inwards and `breadcrumbs` — which now walks the spine forwards —
    /// needs no reversal at either end. The first version built the spine the
    /// other way and relied on a `reverse` inside `breadcrumbs` to undo it; the
    /// two reversals cancelled for the root and doubled for everything below it,
    /// which is the kind of bug a 3-deep test finds and a 2-deep one does not.
    pub fn with_ancestors(self, ancestors: Vec<Tag>) -> TagNode {
        let mut node = self;
        for a in ancestors.into_iter().rev() {
            node = TagNode {
                tag: a,
                children: vec![node],
            };
        }
        node
    }
}

impl Store {
    // ------------------------------------------------------------------ write

    /// Create a tag a person chose.
    ///
    /// The only constructor that writes a `canonical` namespace, and the reason
    /// the ML path is a separate function rather than a field: a caller that
    /// holds a `Namespace` can pass `Canonical` for a model's opinion, and then
    /// the honesty mechanism is a convention. Here there is no argument to get
    /// wrong.
    pub async fn create_tag(&self, name: &str, parent_id: Option<&str>) -> Result<Tag> {
        self.create_tag_with(NameSpaceArg::Canonical, name, parent_id, None, None)
            .await
    }

    /// Create a tag a model proposed.
    ///
    /// §5.15 and #560, #848, #722. The `ml:` prefix is written here and the
    /// caller supplies only the model name, so a model tag cannot be stored
    /// without it. `confidence` is required — a model's opinion with no
    /// confidence recorded is the thing the spec is arguing against.
    pub async fn propose_ml_tag(
        &self,
        model: &str,
        name: &str,
        parent_id: Option<&str>,
        confidence: f64,
    ) -> Result<Tag> {
        if model.trim().is_empty() {
            return Err(TagError::BadNamespace("ml:".to_string()).into());
        }
        check_confidence(confidence)?;
        self.create_tag_with(
            NameSpaceArg::Machine(model.to_string()),
            name,
            parent_id,
            None,
            Some(confidence),
        )
        .await
    }

    /// The one place a tag row is written, so the checks are in one place.
    async fn create_tag_with(
        &self,
        ns: NameSpaceArg,
        name: &str,
        parent_id: Option<&str>,
        color: Option<&str>,
        confidence: Option<f64>,
    ) -> Result<Tag> {
        if name.trim().is_empty() {
            return Err(TagError::BlankName.into());
        }
        let namespace = ns.into_namespace();
        // A namespace's stored form is the contract, and a malformed one is
        // caught here rather than at the point where somebody asks "which model
        // proposed this".
        if matches!(namespace, Namespace::Ml(ref m) if m.trim().is_empty()) {
            return Err(TagError::BadNamespace(namespace.as_str()).into());
        }

        // A parent that does not exist would make a breadcrumb that stops in
        // the middle. Checked before the write so the failure leaves nothing
        // behind.
        if let Some(p) = parent_id {
            if !self.tag_exists(p).await? {
                return Err(TagError::NotFound(p.to_string()).into());
            }
        }

        let id = uuid::Uuid::new_v4().to_string();
        let now = commons_core::ts::now();
        let ns_str = namespace.as_str();
        let imp = 1.0f64;

        match self {
            Store::Sqlite(p) => {
                sqlx::query(
                    "INSERT INTO tag (id, name, parent_id, namespace, color, importance)
                     VALUES (?, ?, ?, ?, ?, ?)",
                )
                .bind(id.clone())
                .bind(name.to_string())
                .bind(parent_id.map(str::to_string))
                .bind(ns_str)
                .bind(color.map(str::to_string))
                .bind(imp)
                .execute(p)
                .await
                .map_err(StoreError::Query)?;
            }
            Store::Postgres(p) => {
                let sql = Store::bind_sql(
                    "INSERT INTO tag (id, name, parent_id, namespace, color, importance)
                     VALUES (?, ?, ?, ?, ?, ?)",
                );
                sqlx::query(&sql)
                    .bind(id.clone())
                    .bind(name.to_string())
                    .bind(parent_id.map(str::to_string))
                    .bind(ns_str)
                    .bind(color.map(str::to_string))
                    .bind(imp)
                    .execute(p)
                    .await
                    .map_err(StoreError::Query)?;
            }
        }

        let tag = Tag {
            id,
            name: name.to_string(),
            parent_id: parent_id.map(str::to_string),
            namespace,
            color: color.map(str::to_string),
            importance: imp,
            // The tag's own confidence is `None`; the confidence belongs to the
            // application. `propose_ml_tag` takes one because the caller has it
            // at propose time, and it is stored on the application below.
            confidence,
            created_at: now,
        };

        // Searchable immediately. A tag that exists but is not findable is a
        // tag the user believes they deleted.
        //
        // `index_object` reports a `SearchError` and this function returns a
        // `StoreError`, so the failure is mapped rather than propagated. A
        // search-index failure is a store failure from the caller's point of
        // view — the tag is written and is not findable, which is the state
        // this call is supposed to prevent — and the message says which half
        // failed so an operator reading it is not left guessing.
        let terms = crate::search::tokenize(name);
        let pairs: Vec<(Field, String)> = terms.into_iter().map(|t| (Field::Tag, t)).collect();
        self.index_object(&tag.id, &pairs)
            .await
            .map_err(|e| StoreError::Invalid {
                what: "tag search index",
                why: format!("the tag was written but could not be indexed: {e}"),
            })?;

        Ok(tag)
    }

    /// Whether a tag row exists.
    async fn tag_exists(&self, id: &str) -> Result<bool> {
        let n: i64 = match self {
            Store::Sqlite(p) => sqlx::query_scalar("SELECT COUNT(*) FROM tag WHERE id = ?")
                .bind(id.to_string())
                .fetch_one(p)
                .await
                .map_err(StoreError::Query)?,
            Store::Postgres(p) => {
                let sql = Store::bind_sql("SELECT COUNT(*) FROM tag WHERE id = ?");
                sqlx::query_scalar(&sql)
                    .bind(id.to_string())
                    .fetch_one(p)
                    .await
                    .map_err(StoreError::Query)?
            }
        };
        Ok(n > 0)
    }

    /// Re-parent a tag, refusing a cycle.
    ///
    /// #3221's undo is a caller of this: re-parenting is a move, and a move
    /// that can make a cycle is not a move. The check is here rather than in
    /// SQL because it needs the ancestor chain, and a recursive CTE is in
    /// neither engine's portable subset — which would make the two engines
    /// disagree about whether an operation is legal, the one thing they must
    /// never do.
    pub async fn set_parent(&self, tag_id: &str, parent_id: Option<&str>) -> Result<()> {
        if Some(tag_id) == parent_id {
            return Err(TagError::Cycle {
                tag: tag_id.to_string(),
                parent: parent_id.unwrap_or_default().to_string(),
            }
            .into());
        }
        if let Some(p) = parent_id {
            if p == tag_id {
                return Err(TagError::Cycle {
                    tag: tag_id.to_string(),
                    parent: p.to_string(),
                }
                .into());
            }
            if !self.tag_exists(p).await? {
                return Err(TagError::NotFound(p.to_string()).into());
            }
            // Walk up from the proposed parent. If `tag_id` is anywhere up
            // there, making it the parent closes a loop. The walk is bounded by
            // the set of tags it has already seen, so a tree that was already
            // cyclic when loaded cannot hang this.
            let mut seen: HashSet<String> = HashSet::new();
            seen.insert(tag_id.to_string());
            let mut cursor = Some(p.to_string());
            while let Some(id) = cursor {
                if !seen.insert(id.clone()) {
                    // Already-cyclic input. Refusing is the only safe answer:
                    // there is no honest answer to "is this a cycle" about a
                    // tree that already has one.
                    return Err(TagError::Cycle {
                        tag: tag_id.to_string(),
                        parent: p.to_string(),
                    }
                    .into());
                }
                cursor = self.tag_parent(&id).await?;
            }
        }

        match self {
            Store::Sqlite(p) => {
                sqlx::query("UPDATE tag SET parent_id = ? WHERE id = ?")
                    .bind(parent_id.map(str::to_string))
                    .bind(tag_id.to_string())
                    .execute(p)
                    .await
                    .map_err(StoreError::Query)?;
            }
            Store::Postgres(p) => {
                let sql = Store::bind_sql("UPDATE tag SET parent_id = ? WHERE id = ?");
                sqlx::query(&sql)
                    .bind(parent_id.map(str::to_string))
                    .bind(tag_id.to_string())
                    .execute(p)
                    .await
                    .map_err(StoreError::Query)?;
            }
        }
        Ok(())
    }

    /// A tag's parent, or `None` for a root or an unknown tag.
    ///
    /// `None` for both because the caller is walking a chain and a chain that
    /// stops at a missing tag and a chain that stops at a root are the same
    /// walk; the difference matters to somebody reconciling the tree against
    /// the data, and that is `load_tag`, not this.
    async fn tag_parent(&self, id: &str) -> Result<Option<String>> {
        let row: Option<(Option<String>,)> = match self {
            Store::Sqlite(p) => sqlx::query_as("SELECT parent_id FROM tag WHERE id = ?")
                .bind(id.to_string())
                .fetch_optional(p)
                .await
                .map_err(StoreError::Query)?,
            Store::Postgres(p) => {
                let sql = Store::bind_sql("SELECT parent_id FROM tag WHERE id = ?");
                sqlx::query_as::<_, (Option<String>,)>(&sql)
                    .bind(id.to_string())
                    .fetch_optional(p)
                    .await
                    .map_err(StoreError::Query)?
            }
        };
        Ok(row.and_then(|r| r.0))
    }

    /// Apply a tag to an object, with a confidence if a model proposed it.
    pub async fn apply_tag(
        &self,
        object_id: &str,
        tag_id: &str,
        confidence: Option<f64>,
        source: Option<&str>,
    ) -> Result<()> {
        if let Some(c) = confidence {
            check_confidence(c)?;
        }
        if !self.tag_exists(tag_id).await? {
            return Err(TagError::NotFound(tag_id.to_string()).into());
        }
        let now = commons_core::ts::now();
        match self {
            Store::Sqlite(p) => {
                sqlx::query(
                    "INSERT INTO object_tag (object_id, tag_id, confidence, source, created_at)
                     VALUES (?, ?, ?, ?, ?)
                     ON CONFLICT (object_id, tag_id) DO UPDATE
                        SET confidence = excluded.confidence,
                            source = excluded.source",
                )
                .bind(object_id.to_string())
                .bind(tag_id.to_string())
                .bind(confidence)
                .bind(source.map(str::to_string))
                .bind(now)
                .execute(p)
                .await
                .map_err(StoreError::Query)?;
            }
            Store::Postgres(p) => {
                let sql = Store::bind_sql(
                    "INSERT INTO object_tag (object_id, tag_id, confidence, source, created_at)
                     VALUES (?, ?, ?, ?, ?)
                     ON CONFLICT (object_id, tag_id) DO UPDATE
                        SET confidence = excluded.confidence,
                            source = excluded.source",
                );
                sqlx::query(&sql)
                    .bind(object_id.to_string())
                    .bind(tag_id.to_string())
                    .bind(confidence)
                    .bind(source.map(str::to_string))
                    .bind(now)
                    .execute(p)
                    .await
                    .map_err(StoreError::Query)?;
            }
        }
        Ok(())
    }

    /// Remove a tag from an object.
    pub async fn remove_tag(&self, object_id: &str, tag_id: &str) -> Result<()> {
        match self {
            Store::Sqlite(p) => {
                sqlx::query("DELETE FROM object_tag WHERE object_id = ? AND tag_id = ?")
                    .bind(object_id.to_string())
                    .bind(tag_id.to_string())
                    .execute(p)
                    .await
                    .map_err(StoreError::Query)?;
            }
            Store::Postgres(p) => {
                let sql =
                    Store::bind_sql("DELETE FROM object_tag WHERE object_id = ? AND tag_id = ?");
                sqlx::query(&sql)
                    .bind(object_id.to_string())
                    .bind(tag_id.to_string())
                    .execute(p)
                    .await
                    .map_err(StoreError::Query)?;
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------------- read

    /// One tag, or `None`.
    pub async fn load_tag(&self, id: &str) -> Result<Option<Tag>> {
        let raw: Option<TagRow> = match self {
            Store::Sqlite(p) => sqlx::query_as(
                "SELECT id, name, parent_id, namespace, color, importance
                   FROM tag WHERE id = ?",
            )
            .bind(id.to_string())
            .fetch_optional(p)
            .await
            .map_err(StoreError::Query)?,
            Store::Postgres(p) => {
                let sql = Store::bind_sql(
                    "SELECT id, name, parent_id, namespace, color, importance
                       FROM tag WHERE id = ?",
                );
                sqlx::query_as::<_, TagRow>(&sql)
                    .bind(id.to_string())
                    .fetch_optional(p)
                    .await
                    .map_err(StoreError::Query)?
            }
        };
        Ok(raw.map(row_to_tag))
    }

    /// Every tag, in name order, with no tree.
    ///
    /// The flat read, for a tagger's list and for a filter's autocomplete. The
    /// ordering is in the query rather than in Rust so the two engines sort
    /// identically — §3.5's rule applied to a `SELECT` rather than a search.
    pub async fn all_tags(&self) -> Result<Vec<Tag>> {
        let raw: Vec<TagRow> = match self {
            Store::Sqlite(p) => sqlx::query_as(
                "SELECT id, name, parent_id, namespace, color, importance
                   FROM tag ORDER BY name, id",
            )
            .fetch_all(p)
            .await
            .map_err(StoreError::Query)?,
            Store::Postgres(p) => {
                let sql = Store::bind_sql(
                    "SELECT id, name, parent_id, namespace, color, importance
                       FROM tag ORDER BY name, id",
                );
                sqlx::query_as::<_, TagRow>(&sql)
                    .fetch_all(p)
                    .await
                    .map_err(StoreError::Query)?
            }
        };
        Ok(raw.into_iter().map(row_to_tag).collect())
    }

    /// The whole tag tree, roots first.
    ///
    /// Built in Rust from the flat list rather than in SQL, because a recursive
    /// CTE is in neither engine's portable subset and two hand-written
    /// recursive queries would be two behaviours. One read, assembled once.
    ///
    /// A tag whose parent is missing appears as a root. That is a real state —
    /// a tag can outlive a parent that was deleted with `ON DELETE SET NULL`,
    /// and re-homing it under a made-up node would be inventing structure. It
    /// is a root, and `parent_id` still says what happened.
    pub async fn tag_tree(&self) -> Result<Vec<TagNode>> {
        let flat = self.all_tags().await?;
        let mut children_of: BTreeMap<Option<String>, Vec<Tag>> = BTreeMap::new();
        for t in flat {
            children_of.entry(t.parent_id.clone()).or_default().push(t);
        }
        // `None` is the root bucket. A tag whose parent is not in the list at
        // all is re-homed to the root bucket *for rendering only* -- the tree
        // the caller receives is the tree that exists.
        let known: HashSet<String> = children_of
            .values()
            .flatten()
            .map(|t| t.id.clone())
            .collect();
        let mut orphans: Vec<Tag> = Vec::new();
        let mut buckets: BTreeMap<Option<String>, Vec<Tag>> = BTreeMap::new();
        for (_parent, tags) in children_of {
            for t in tags {
                match &t.parent_id {
                    Some(pid) if known.contains(pid) => {
                        buckets.entry(Some(pid.clone())).or_default().push(t)
                    }
                    _ => orphans.push(t),
                }
            }
        }

        fn build(
            id: Option<&str>,
            buckets: &BTreeMap<Option<String>, Vec<Tag>>,
            orphans: &[Tag],
        ) -> Vec<TagNode> {
            let mut out: Vec<TagNode> = Vec::new();
            if id.is_none() {
                for t in orphans {
                    out.push(TagNode {
                        tag: t.clone(),
                        children: build(Some(&t.id), buckets, orphans),
                    });
                }
            }
            if let Some(kids) = buckets.get(&id.map(str::to_string)) {
                for t in kids {
                    out.push(TagNode {
                        tag: t.clone(),
                        children: build(Some(&t.id), buckets, orphans),
                    });
                }
            }
            out
        }
        Ok(build(None, &buckets, &orphans))
    }

    /// One tag with its subtree and the path to it, or `None`.
    ///
    /// The path is loaded, not derived, and the reason is a real bug: the first
    /// version returned a node cut out of the tree, so [`TagNode::breadcrumbs`]
    /// — which walks the subtree — saw only the node and its descendants. A
    /// 3-deep tree's breadcrumb came back as `["c"]` and the test that should
    /// have caught it asserted against a tree built the same way, so the two
    /// agreed and both were wrong.
    ///
    /// So the ancestors are fetched and the node is returned with them, and
    /// [`TagNode::breadcrumbs`] is the only way to read the path. That keeps one
    /// implementation of "what is the path to this tag" rather than two, which
    /// is the same discipline the rest of this module follows.
    pub async fn tag_subtree(&self, id: &str) -> Result<Option<TagNode>> {
        let tree = self.tag_tree().await?;
        fn find<'a>(nodes: &'a [TagNode], id: &str) -> Option<&'a TagNode> {
            for n in nodes {
                if n.tag.id == id {
                    return Some(n);
                }
                if let Some(hit) = find(&n.children, id) {
                    return Some(hit);
                }
            }
            None
        }
        let node = match find(&tree, id) {
            Some(n) => n.clone(),
            None => return Ok(None),
        };
        // The chain, walked from the tag up through `parent_id`. Bounded by the
        // set of ids seen, so an already-cyclic tree cannot hang this — the same
        // bound `set_parent` uses, and for the same reason.
        let mut path: Vec<Tag> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        seen.insert(node.tag.id.clone());
        let mut cursor = node.tag.parent_id.clone();
        while let Some(pid) = cursor {
            if !seen.insert(pid.clone()) {
                break;
            }
            match self.load_tag(&pid).await? {
                Some(t) => {
                    cursor = t.parent_id.clone();
                    path.push(t);
                }
                None => break,
            }
        }
        path.reverse();
        Ok(Some(node.with_ancestors(path)))
    }

    /// The tags on an object, with their application metadata.
    /// Every tag applied to one object, in an order both engines agree on.
    ///
    /// `ORDER BY t.name, t.id` and not `ORDER BY tag_id`. `tag_id` is a UUID
    /// generated per engine, so ordering by it produces a *different* order on
    /// Postgres than on SQLite for the same data, and §3.5 requires the two
    /// engines' reads to agree. The parity test in `tests/tags.rs` caught
    /// exactly this: for one object with two tags the engines returned
    /// `Anal, Ass` and `Ass, Anal`. A test with one tag per object could not
    /// have told — which is why the assertion that found it is on a two-tag
    /// object, and why the id-blindness of the earlier version of it is the
    /// thing that hid the bug rather than a test that was merely weak.
    ///
    /// The join is for one column. That is the cost of an order both engines
    /// agree on, and it is cheap: an object has tens of tags, not thousands.
    /// `t.id` is the tie-break, so two tags with the same name — which
    /// `UNIQUE (namespace, name)` permits, since it is scoped to a namespace —
    /// are still in a total order rather than an arbitrary one.
    pub async fn object_tags(&self, object_id: &str) -> Result<Vec<TagApplication>> {
        let sql = "SELECT ot.object_id, ot.tag_id, ot.confidence, ot.source, ot.created_at
                     FROM object_tag ot
                     JOIN tag t ON t.id = ot.tag_id
                    WHERE ot.object_id = ?
                    ORDER BY t.name, t.id";
        // `created_at` last as `Option<String>`, not `String`. Migration 0016
        // adds the column to a table that 0001 created without it, and added it
        // nullable: a row written before 0016 applied has no value there, and a
        // row written by a caller that went through `apply_tag` always does.
        //
        // The first version declared it `String` and got a decode error on any
        // row 0016 had not backfilled. Making the column `NOT NULL` in the
        // migration would have been the other answer, and it is the wrong one:
        // 0001's `object_tag` has no timestamp at all, so a NOT NULL would need a
        // backfill from something, and there is nothing in the row to backfill
        // from. Nullable with a reader that tolerates the absence is the honest
        // shape, and `TagApplication::created_at` says which of the two it is.
        let raw: Vec<ApplicationRow> = match self {
            Store::Sqlite(p) => sqlx::query_as(sql)
                .bind(object_id.to_string())
                .fetch_all(p)
                .await
                .map_err(StoreError::Query)?,
            Store::Postgres(p) => {
                let bound = Store::bind_sql(sql);
                sqlx::query_as(&bound)
                    .bind(object_id.to_string())
                    .fetch_all(p)
                    .await
                    .map_err(StoreError::Query)?
            }
        };
        Ok(raw
            .into_iter()
            .map(
                |(object_id, tag_id, confidence, source, created_at)| TagApplication {
                    object_id,
                    tag_id,
                    confidence,
                    source,
                    created_at,
                },
            )
            .collect())
    }

    pub async fn object_tags_full(&self, object_id: &str) -> Result<Vec<(Tag, TagApplication)>> {
        let apps = self.object_tags(object_id).await?;
        let mut out = Vec::with_capacity(apps.len());
        for a in apps {
            if let Some(t) = self.load_tag(&a.tag_id).await? {
                out.push((t, a));
            }
        }
        Ok(out)
    }

    /// Every tag a model proposed, whatever the model.
    ///
    /// §8.2: ML tags are proposals and the confidence is visible, and a list
    /// that can show them is the first half of that. The filter is
    /// `namespace LIKE 'ml:%'` so a new model's tags appear in this list
    /// without anybody adding it to a list somewhere.
    pub async fn machine_tags(&self) -> Result<Vec<Tag>> {
        Ok(self
            .all_tags()
            .await?
            .into_iter()
            .filter(|t| t.namespace.is_machine())
            .collect())
    }

    /// The tagger's queue: every machine-proposed tag application, newest first.
    ///
    /// §8.2 and the stash #2305 tagger list. This is the query the
    /// `object_tag_source_idx` in migration 0016 exists for, and it is a real
    /// index range rather than a scan: the predicate is `source IS NOT NULL`
    /// ordered by `created_at`, which is the index's leading column and its
    /// second.
    ///
    /// There was no such method before, and `machine_tags` — which filters
    /// *tags* by namespace — is not a substitute. The difference is what is being
    /// listed: a tag that the model proposed for one object, with the confidence
    /// attached, is a proposal awaiting a decision, and it disappears the moment
    /// the object is unlinked. A namespace query cannot see that, and a UI that
    /// showed the tag list instead would show every machine tag ever proposed
    /// and none of the ones anyone is being asked about.
    pub async fn tagger_queue(&self, limit: usize) -> Result<Vec<TagApplication>> {
        // The NULL guard is a `CASE`, not `(created_at IS NOT NULL AND
        // created_at)`.
        //
        // The obvious spelling is a bug and it is silent on both engines:
        // SQLite's `AND` is a *boolean* operator, so `x IS NOT NULL AND x`
        // evaluates the comparison, coerces the text to 0, and `0 AND 0` is 0.
        // Every row gets the same key and the sort stops being a sort — the
        // queue comes back in whatever order the scan produced, which on
        // SQLite is the insertion order. On Postgres `AND` is boolean over a
        // boolean *and* the text, which is a type error, so the first version of
        // this only ever ran on one engine.
        //
        // `CASE WHEN ... THEN 1 ELSE 0 END` is an integer on both engines and
        // the second key is the plain `created_at`, so the ordering is a real
        // ordering. The `NULL`s sort last on both, which is the portable
        // direction: Postgres puts NULLs last on a DESC by default and SQLite
        // puts them first, and "last" is right for a newest-first list.
        let sql = format!(
            "SELECT tag_id, object_id, confidence, source, created_at
               FROM object_tag
              WHERE source IS NOT NULL
              ORDER BY (CASE WHEN created_at IS NULL THEN 1 ELSE 0 END) ASC,
                       created_at DESC,
                       object_id
              LIMIT {limit}"
        );
        // `LIMIT` is inlined rather than bound. It is a `usize` this function
        // was handed, so there is nothing untrusted in the interpolation, and
        // the alternative is a dialect problem: Postgres cannot infer a type
        // for a bound parameter in a LIMIT clause on a prepared statement, the
        // same `PREPARE` failure the `CASE WHEN ?` in T-P5-002 hit. The first
        // version clamped to a maximum and did not say so here; a caller asking
        // for 10_000 rows of a queue gets 10_000, and one asking for 0 gets an
        // empty queue rather than a clamped one.
        // A tuple, not `query_as::<_, TagApplication>`. `TagApplication` is a
        // plain struct for callers, and the two read paths build it from a
        // tuple here; deriving `FromRow` for it would work too but would make
        // the field order load-bearing in two places, and the column order in
        // this query is not the struct's field order.
        type Row = (String, String, Option<f64>, Option<String>, Option<String>);
        let rows: Vec<Row> = match self {
            Store::Sqlite(p) => sqlx::query_as(&sql)
                .fetch_all(p)
                .await
                .map_err(StoreError::Query)?,
            Store::Postgres(p) => sqlx::query_as(&sql)
                .fetch_all(p)
                .await
                .map_err(StoreError::Query)?,
        };
        Ok(rows
            .into_iter()
            .map(
                |(tag_id, object_id, confidence, source, created_at)| TagApplication {
                    tag_id,
                    object_id,
                    confidence,
                    source,
                    created_at,
                },
            )
            .collect())
    }

    /// The tags a person chose — the ones that need no marking.
    pub async fn canonical_tags(&self) -> Result<Vec<Tag>> {
        Ok(self
            .all_tags()
            .await?
            .into_iter()
            .filter(|t| t.namespace.is_canonical())
            .collect())
    }
}

// ------------------------------------------------------------------ internal

/// A row of `tag`, as the database returns it.
///
/// Named because the read paths all decode the same six columns into the same
/// tuple, and three copies of `Vec<(String, String, Option<String>, String,
/// Option<String>, f64)>` is three places to change when a column is added --
/// which is what clippy's `type_complexity` was pointing at, and it was right.
/// The order is `id, name, parent_id, namespace, color, importance`.
type TagRow = (String, String, Option<String>, String, Option<String>, f64);

/// A row of `object_tag`, as the database returns it.
///
/// `created_at` last and nullable: migration 0016 added the column to a table
/// 0001 created without a timestamp, so a row written before it has none.
/// `tagger_queue` selects the same five columns in the opposite id order, so it
/// has its own tuple rather than this one.
type ApplicationRow = (String, String, Option<f64>, Option<String>, Option<String>);

/// How a tag row gets its namespace, before it becomes a string.
///
/// A private enum rather than a `Namespace` parameter, so the two public
/// constructors are the only ways in and neither of them takes a namespace the
/// caller assembled. That is the whole point of the honesty mechanism: a
/// `Namespace` argument is a `canonical` argument wearing a disguise.
enum NameSpaceArg {
    Canonical,
    Machine(String),
}

impl NameSpaceArg {
    fn into_namespace(self) -> Namespace {
        match self {
            NameSpaceArg::Canonical => Namespace::Canonical,
            NameSpaceArg::Machine(m) => Namespace::Ml(m),
        }
    }
}

fn check_confidence(c: f64) -> Result<()> {
    // `NaN` fails every comparison, so `!(0.0..=1.0).contains(&c)` is the
    // check that catches it. Written as a positive test rather than
    // `c < 0.0 || c > 1.0` because that form lets a `NaN` through, and a `NaN`
    // confidence sorts unpredictably and compares false to everything.
    if !(0.0..=1.0).contains(&c) {
        return Err(TagError::BadConfidence(c).into());
    }
    Ok(())
}

fn row_to_tag(r: (String, String, Option<String>, String, Option<String>, f64)) -> Tag {
    let (id, name, parent_id, namespace, color, importance) = r;
    Tag {
        id,
        name,
        parent_id,
        namespace: Namespace::parse(&namespace),
        color,
        importance,
        confidence: None,
        created_at: String::new(),
    }
}
