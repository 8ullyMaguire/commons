use std::fmt;

use commons_core::{ObjectKind, Role, SubjectType};
use serde::{Deserialize, Serialize};

/// A reference to a field, built-in or user-defined.
// Externally tagged for the same reason as `Value`: an internally-tagged
// variant cannot carry a bare `String` payload.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldRef {
    /// One of the schema's declared fields.
    Builtin(BuiltinField),
    /// A custom field by name (stash#6795, stash-box#823).
    Custom(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuiltinField {
    Title,
    Description,
    Date,
    Kind,
    Organized,
    Rating,
    RatingCount,
    Studio,
    Circle,
    Tag,
    Performer,
    Cluster,
    Group,
    DurationMs,
    SizeBytes,
    Path,
    State,
    ConsentTier,
    CreatedAt,
    UpdatedAt,
    RatingStars,
    MarkerCount,
    ImageCount,
    /// `is null` and `is not null` as first-class operators (stash#6970).
    HasFingerprint,
    HasThumbnail,
    HasTranscript,
    /// Secondary-sort and hidden-field toggles (stash#3219, #7068).
    SceneIndex,
    IsMissing,
}

impl fmt::Display for FieldRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FieldRef::Builtin(b) => write!(f, "{b}"),
            FieldRef::Custom(name) => write!(f, "custom:{name}"),
        }
    }
}

impl fmt::Display for BuiltinField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            BuiltinField::Title => "title",
            BuiltinField::Description => "description",
            BuiltinField::Date => "date",
            BuiltinField::Kind => "kind",
            BuiltinField::Organized => "organized",
            BuiltinField::Rating => "rating",
            BuiltinField::RatingCount => "rating_count",
            BuiltinField::Studio => "studio",
            BuiltinField::Circle => "circle",
            BuiltinField::Tag => "tag",
            BuiltinField::Performer => "performer",
            BuiltinField::Cluster => "cluster",
            BuiltinField::Group => "group",
            BuiltinField::DurationMs => "duration_ms",
            BuiltinField::SizeBytes => "size_bytes",
            BuiltinField::Path => "path",
            BuiltinField::State => "state",
            BuiltinField::ConsentTier => "consent_tier",
            BuiltinField::CreatedAt => "created_at",
            BuiltinField::UpdatedAt => "updated_at",
            BuiltinField::RatingStars => "rating_stars",
            BuiltinField::MarkerCount => "marker_count",
            BuiltinField::ImageCount => "image_count",
            BuiltinField::HasFingerprint => "has_fingerprint",
            BuiltinField::HasThumbnail => "has_thumbnail",
            BuiltinField::HasTranscript => "has_transcript",
            BuiltinField::SceneIndex => "scene_index",
            BuiltinField::IsMissing => "is_missing",
        };
        f.write_str(s)
    }
}

/// The comparison operators. `IsNull`/`IsNotNull` are separate variants rather
/// than a value, because NULL is not a value: `is null` and `equals ""` must
/// not be confusable, and stash#3159 is a bug about exactly that confusion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CmpOp {
    Eq,
    Ne,
    In,
    NotIn,
    Contains,
    StartsWith,
    Gt,
    Gte,
    Lt,
    Lte,
    Between,
    IsNull,
    IsNotNull,
    MatchesRegex,
}

impl CmpOp {
    /// Whether this operator consumes the `values` list. The two NULL
    /// operators do not, and the SQL generator must not emit a placeholder for
    /// them — an extra bind is a silently wrong query, not a compile error.
    pub const fn takes_values(self) -> bool {
        !matches!(self, CmpOp::IsNull | CmpOp::IsNotNull)
    }

    pub const fn is_range(self) -> bool {
        matches!(self, CmpOp::Between)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
// Externally tagged: serde rejects a bare String / f64 / bool as the payload
// of an internally-tagged variant ("cannot serialize tagged newtype variant
// Value::Str containing a string"), so the internal tag was never actually
// usable. The wire shape is `{"str":"a"}`, which also disambiguates
// `Value::Str("t")` from `Value::List(vec![Value::Str("t")])` by structure
// rather than by convention.
#[serde(rename_all = "snake_case")]
pub enum Value {
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Null,
    List(Vec<Value>),
}

impl Value {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::Str(s.to_string())
    }
}

impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::Str(s)
    }
}

impl From<i64> for Value {
    fn from(i: i64) -> Self {
        Value::Int(i)
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::Bool(b)
    }
}

/// Who is asking. The consent filter is derived from this, which is what makes
/// rule 3 of the plan enforceable: there is no way to build a query for a
/// caller without also producing their visibility constraint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CallerId {
    pub account_id: Option<String>,
    pub role: Role,
    /// Per-account content filters, enforced at the query layer so a hidden
    /// category cannot leak through search or a recommendation (stash-box#643,
    /// #733).
    pub excluded_tag_ids: Vec<String>,
    pub excluded_studio_ids: Vec<String>,
    pub excluded_performer_ids: Vec<String>,
    /// Tiers this caller may see beyond the public set. Stewards see all.
    pub tier_allowlist: Vec<String>,
}

impl CallerId {
    /// An anonymous visitor: no account, lowest role, no filters beyond the
    /// defaults. The Phase 9 negative tests run against exactly this.
    pub fn anonymous() -> Self {
        Self {
            account_id: None,
            role: Role::Public,
            excluded_tag_ids: vec![],
            excluded_studio_ids: vec![],
            excluded_performer_ids: vec![],
            tier_allowlist: vec![],
        }
    }

    /// A steward, for moderation and export tooling.
    ///
    /// The allowlist is [`ConsentTiers::MODERATION`], not
    /// [`ConsentTiers::ALL`]. It used to be `ALL`, which meant this helper --
    /// not the clause -- decided what a steward sees, and it decided to grant
    /// `denied`: a takedown accepted, which §14.1 calls "permanently blocked by
    /// hash across all peers". The consent clause was tightened and this kept
    /// handing out the old grant, which is the shape of bug that survives a fix
    /// applied to one of two places.
    ///
    /// Moderation needs `quarantined`; an export tool that needs the rest has to
    /// say so, in a place a reviewer reads.
    pub fn steward(account_id: impl Into<String>) -> Self {
        Self {
            account_id: Some(account_id.into()),
            role: Role::Steward,
            excluded_tag_ids: vec![],
            excluded_studio_ids: vec![],
            excluded_performer_ids: vec![],
            tier_allowlist: ConsentTiers::MODERATION
                .iter()
                .map(|s| s.to_string())
                .collect(),
        }
    }
}

/// The tier strings a non-steward caller may see, as a constant so the SQL
/// generator and the tests cannot drift from `commons-core`'s enum.
pub struct ConsentTiers;

impl ConsentTiers {
    /// Tiers visible to a remote viewer, matching
    /// `ConsentTier::visible_to_public`. `Unverified` is deliberately absent:
    /// consent not established means consent not established for anyone but
    /// the holder.
    pub const PUBLIC: [&'static str; 3] = [
        "self_published",
        "performer_claimed",
        "third_party_permitted",
    ];

    /// Tiers visible to the library's own operator.
    ///
    /// A *superset* of [`Self::PUBLIC`], deliberately. §14.1 is explicit that a
    /// licensed item is one "the user may watch and keep", so an operator
    /// holding a `third_party_permitted` file must be able to find it in their
    /// own library. This set was previously
    /// `["unverified", "self_published", "performer_claimed"]`, which answered
    /// the question "may the owner see their own unverified scans?" and silently
    /// answered the larger one wrong. `unverified` is present because a freshly
    /// scanned file is unverified and must be visible in the app holding it.
    ///
    /// Not a superset of the moderation tiers, also deliberately: a contested
    /// item is hidden from its own uploader too. §14.1 says quarantined is
    /// "hidden everywhere, pending review", and *everywhere* includes the
    /// uploader.
    pub const OWNER: [&'static str; 4] = [
        "unverified",
        "self_published",
        "performer_claimed",
        "third_party_permitted",
    ];

    /// Tiers a steward or admin may see: the moderation tiers plus the
    /// publishable ones.
    ///
    /// Not [`Self::ALL`]. `may_see_restricted()` is `Steward | Admin`, and the
    /// clause it used to guard returned `1 = 1` -- which granted not moderation
    /// but *everything*, putting every `denied` row (a takedown accepted, §14.1
    /// "permanently blocked by hash across all peers") into the ordinary browse
    /// surface of a steward who had a takedown accepted against them.
    ///
    /// The publishable tiers are here for a specific reason: a quarantine is
    /// raised against an otherwise ordinary row, so a moderator who cannot see
    /// the ordinary tiers cannot see the object they are moderating.
    pub const MODERATION: [&'static str; 4] = [
        "self_published",
        "performer_claimed",
        "third_party_permitted",
        "quarantined",
    ];

    pub const ALL: [&'static str; 6] = [
        "unverified",
        "self_published",
        "performer_claimed",
        "third_party_permitted",
        "quarantined",
        "denied",
    ];
}

/// A filter tree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
// Externally tagged, deliberately. An internal tag compiles in isolation but
// overflows the trait solver once `Facet` carries `Vec<Value>` and `Value` is
// itself a recursively-tagged enum: the two tag buffers compose into a solver
// search that does not terminate (E0275 on `&mut Vec<u8>: std::io::Write`),
// and raising `recursion_limit` does not help -- it was measured at 19s of
// compile time per doubling and still failing. Externally tagged needs no tag
// buffer and compiles immediately. The wire shape becomes `{"and":[...]}`, which
// is also easier to read in a shareable URL than an inline tag.
#[serde(rename_all = "snake_case")]
pub enum Filter {
    And(Vec<Filter>),
    Or(Vec<Filter>),
    Not(Box<Filter>),
    /// A single facet comparison.
    Facet {
        kind: Option<ObjectKind>,
        field: FieldRef,
        op: CmpOp,
        #[serde(default)]
        values: Vec<Value>,
    },
    /// Free-text search across the configured fields.
    Text {
        q: String,
    },
    /// A stored filter, expanded at query time.
    Saved {
        id: String,
    },
    /// Matches every row. Used as the base for the consent conjunction.
    All,
}

// Written out rather than derived: `#[derive(Default)]` needs a `#[default]`
// attribute on a unit variant, which serde's external tagging puts out of
// reach in the derive's view of the enum.
impl Default for Filter {
    /// `All` -- match everything, before the consent conjunction is applied.
    /// This is the base a UI starts from, never a filter that grants access.
    fn default() -> Self {
        Filter::All
    }
}

/// A compiled fragment: SQL text with `?` placeholders plus the bound values,
/// in order. The two engines differ only in the placeholder style, which the
/// store crate handles.
#[derive(Debug, Clone, PartialEq)]
pub struct SqlFragment {
    pub sql: String,
    pub params: Vec<Value>,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum FilterError {
    #[error("operator {0:?} does not accept a value list")]
    UnexpectedValues(CmpOp),
    #[error("operator {0:?} requires at least one value")]
    MissingValues(CmpOp),
    #[error("operator between requires exactly 2 values, got {0}")]
    BadRange(usize),
    #[error("cannot order by facet {0} on subject {1:?}")]
    UnsupportedSort(String, SubjectType),
    #[error("parse error at position {pos}: {msg}")]
    Parse { pos: usize, msg: String },
}

/// Which engine the SQL is being generated for.
///
/// The AST always emits `?` markers regardless of engine. The store's bind
/// layer rewrites them to `$1..$n` for Postgres on the way out. Keeping one
/// marker style means the AST is tested once, not once per engine, and the
/// only engine-specific knowledge lives in one function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    Postgres,
    Sqlite,
}

impl std::fmt::Display for Engine {
    /// The engine's own name, lowercase.
    ///
    /// Present because every test in this repository runs the same assertion
    /// twice and reports which engine failed, and `{:?}` on a two-variant enum
    /// prints `Sqlite` where a test message wants `sqlite`. The name is written
    /// down once, here, rather than spelled at each call site.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Engine::Postgres => "postgres",
            Engine::Sqlite => "sqlite",
        })
    }
}

impl Filter {
    /// Compile to SQL and bound parameters.
    ///
    /// Every value goes into `params`; the returned SQL contains only `?`
    /// markers. This is the invariant the security test asserts.
    pub fn to_sql(&self, engine: Engine, caller: &CallerId) -> Result<SqlFragment, FilterError> {
        let mut params: Vec<Value> = Vec::new();
        let body = self.compile(engine, caller, &mut params, 0)?;
        Ok(SqlFragment {
            sql: format!("({body})"),
            params,
        })
    }

    /// The consent conjunction that every object query must include.
    ///
    /// This is rule 3 of the plan expressed in code: there is no API that
    /// builds an object query without it, because `to_sql` takes the caller and
    /// always ANDs this in.
    pub fn consent_clause(
        caller: &CallerId,
        params: &mut Vec<Value>,
    ) -> Result<String, FilterError> {
        // A steward or admin is granted the *moderation* tiers, not everything.
        // See `ConsentTiers::MODERATION`: `denied` is a takedown rather than a
        // moderation state, and the previous `1 = 1` put it in every browse
        // surface. Everything below still applies.
        if caller.role.may_see_restricted() && caller.tier_allowlist.is_empty() {
            for t in ConsentTiers::MODERATION {
                params.push(Value::Str(t.to_string()));
            }
            return Ok(format!(
                "c.tier IN ({})",
                placeholders(ConsentTiers::MODERATION.len())
            ));
        }

        let visible: Vec<&str> = if caller.account_id.is_some() && caller.tier_allowlist.is_empty()
        {
            // A logged-in local user with no explicit allowlist is the library
            // owner: they must see their own unverified scans.
            ConsentTiers::OWNER.to_vec()
        } else if caller.tier_allowlist.is_empty() {
            ConsentTiers::PUBLIC.to_vec()
        } else {
            caller.tier_allowlist.iter().map(|s| s.as_str()).collect()
        };

        for t in &visible {
            params.push(Value::Str((*t).to_string()));
        }
        Ok(format!("c.tier IN ({})", placeholders(visible.len())))
    }

    fn compile(
        &self,
        engine: Engine,
        caller: &CallerId,
        params: &mut Vec<Value>,
        depth: usize,
    ) -> Result<String, FilterError> {
        // A bound on nesting depth: a hand-crafted URL must not be able to
        // build a 10,000-deep tree and blow the stack.
        const MAX_DEPTH: usize = 32;
        if depth > MAX_DEPTH {
            return Err(FilterError::Parse {
                pos: depth,
                msg: format!("filter nesting exceeds {MAX_DEPTH} levels"),
            });
        }

        Ok(match self {
            Filter::All => "1 = 1".to_string(),

            Filter::And(children) if children.is_empty() => "1 = 1".to_string(),
            Filter::Or(children) if children.is_empty() => "1 = 0".to_string(),

            Filter::And(children) => {
                let parts = self.compile_children(children, engine, caller, params, depth)?;
                format!("({})", parts.join(" AND "))
            }
            Filter::Or(children) => {
                let parts = self.compile_children(children, engine, caller, params, depth)?;
                format!("({})", parts.join(" OR "))
            }
            Filter::Not(inner) => {
                let part = inner.compile(engine, caller, params, depth + 1)?;
                format!("(NOT {part})")
            }

            Filter::Text { q } => {
                // Parameterised LIKE. The wildcards are added to the *value*,
                // never to the SQL text, so a search for "100%" cannot inject.
                params.push(Value::Str(format!("%{}%", escape_like(q))));
                "o.search_blob LIKE ? ESCAPE '\\'".to_string()
            }

            Filter::Saved { id } => {
                // Expanded by the caller that owns stored-filter lookup; the
                // AST records the reference so a URL stays short and stable.
                params.push(Value::Str(id.clone()));
                "(o.saved_filter_ids LIKE ?)".to_string()
            }

            Filter::Facet {
                kind,
                field,
                op,
                values,
            } => self.compile_facet(*kind, field, *op, values, engine, params)?,
        })
    }

    fn compile_children(
        &self,
        children: &[Filter],
        engine: Engine,
        caller: &CallerId,
        params: &mut Vec<Value>,
        depth: usize,
    ) -> Result<Vec<String>, FilterError> {
        children
            .iter()
            .map(|c| c.compile(engine, caller, params, depth + 1))
            .collect()
    }

    fn compile_facet(
        &self,
        kind: Option<ObjectKind>,
        field: &FieldRef,
        op: CmpOp,
        values: &[Value],
        _engine: Engine,
        params: &mut Vec<Value>,
    ) -> Result<String, FilterError> {
        let column = column_for(field, kind).ok_or_else(|| FilterError::Parse {
            pos: 0,
            msg: format!("field `{field}` is not filterable"),
        })?;

        match op {
            CmpOp::IsNull => return Ok(format!("{column} IS NULL")),
            CmpOp::IsNotNull => return Ok(format!("{column} IS NOT NULL")),
            _ => {}
        }

        if values.is_empty() {
            return Err(FilterError::MissingValues(op));
        }

        Ok(match op {
            CmpOp::Eq | CmpOp::Ne => {
                let joiner = if op == CmpOp::Eq { "=" } else { "<>" };
                // `ne` with a list is the same as `not in`; handling it as a
                // list keeps the UI's multi-select honest.
                if values.len() == 1 {
                    params.push(values[0].clone());
                    format!("{column} {joiner} ?")
                } else {
                    for v in values {
                        params.push(v.clone());
                    }
                    let word = if op == CmpOp::Eq { "IN" } else { "NOT IN" };
                    format!("{column} {word} ({})", placeholders(values.len()))
                }
            }
            CmpOp::In | CmpOp::NotIn => {
                for v in values {
                    params.push(v.clone());
                }
                let word = if op == CmpOp::In { "IN" } else { "NOT IN" };
                format!("{column} {word} ({})", placeholders(values.len()))
            }
            CmpOp::Contains => {
                params.push(Value::Str(format!(
                    "%{}%",
                    escape_like(values[0].as_str().unwrap_or_default(),)
                )));
                format!("{column} LIKE ? ESCAPE '\\'")
            }
            CmpOp::StartsWith => {
                params.push(Value::Str(format!(
                    "{}%",
                    escape_like(values[0].as_str().unwrap_or_default())
                )));
                format!("{column} LIKE ? ESCAPE '\\'")
            }
            CmpOp::Gt => {
                params.push(values[0].clone());
                format!("{column} > ?")
            }
            CmpOp::Gte => {
                params.push(values[0].clone());
                format!("{column} >= ?")
            }
            CmpOp::Lt => {
                params.push(values[0].clone());
                format!("{column} < ?")
            }
            CmpOp::Lte => {
                params.push(values[0].clone());
                format!("{column} <= ?")
            }
            CmpOp::Between => {
                if values.len() != 2 {
                    return Err(FilterError::BadRange(values.len()));
                }
                params.push(values[0].clone());
                params.push(values[1].clone());
                format!("{column} BETWEEN ? AND ?")
            }
            CmpOp::MatchesRegex => {
                // Regex is accepted by the query language (stash#1111 adds a
                // case-insensitivity toggle) but the engine-specific syntax
                // is delegated, so the value is still only a parameter.
                params.push(values[0].clone());
                format!("{column} REGEXP ?")
            }
            CmpOp::IsNull | CmpOp::IsNotNull => unreachable!("handled above"),
        })
    }

    /// Canonical JSON, base64url-encoded, for a shareable URL (§15.10).
    ///
    /// Infallible by construction: `Filter` holds no interior mutability and no
    /// non-serializable state, so encoding cannot fail at runtime. A failure
    /// here would be a bug in this crate, and it is reported as one rather than
    /// swallowed -- an earlier version fell back to `Filter::All`, which turns
    /// a serialization bug into a user silently seeing the entire library.
    pub fn to_url(&self) -> String {
        let mut buf: Vec<u8> = Vec::new();
        serde_json::to_writer(&mut buf, self)
            .expect("Filter is always serializable; this is a crate bug, not bad input");
        base64url_encode(&buf)
    }

    /// Parse a filter from a shareable URL. Untrusted input: every error path
    /// returns `Err` rather than defaulting, so a malformed link cannot widen
    /// a query into "everything".
    pub fn from_url(s: &str) -> Result<Self, FilterError> {
        let json = base64url_decode(s).ok_or_else(|| FilterError::Parse {
            pos: 0,
            msg: "filter is not valid base64url".into(),
        })?;
        serde_json::from_slice(&json).map_err(|e| FilterError::Parse {
            pos: 0,
            msg: e.to_string(),
        })
    }
}

/// `n` comma-separated placeholders.
/// `n` bind placeholders, for an `IN (…)` list.
///
/// `pub(crate)` and not `pub`: it produces SQL text, so a downstream caller
/// holding one is holding a fragment of a query it did not compile. Within the
/// crate it is a shared primitive — `bulk.rs` needs it for the same reason
/// `to_sql` does, and a second copy of `(1..=n).map(|_| "?")` is a second thing
/// that can disagree about what a list of n binds looks like.
pub(crate) fn placeholders(n: usize) -> String {
    (1..=n).map(|_| "?").collect::<Vec<_>>().join(", ")
}

/// Escape LIKE metacharacters so a search for "100%" is a literal search.
fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// Map a field to its SQL column. `None` means "not filterable", which is an
/// error rather than a silent always-true.
fn column_for(field: &FieldRef, kind: Option<ObjectKind>) -> Option<String> {
    let base = match field {
        FieldRef::Custom(name) => {
            // Custom fields live in a side table and are resolved by the
            // caller; the AST records the intent, the store joins.
            return Some(format!("cf.value_for_custom(o.id, '{name}')"));
        }
        FieldRef::Builtin(b) => match b {
            BuiltinField::Title => "o.title",
            BuiltinField::Description => "o.description",
            BuiltinField::Date => "o.date",
            BuiltinField::Kind => "o.kind",
            BuiltinField::Organized => "o.organized",
            BuiltinField::Rating => "(CASE WHEN o.rating_count > 0 THEN CAST(o.rating_sum AS REAL) / o.rating_count END)",
            BuiltinField::RatingCount => "o.rating_count",
            BuiltinField::RatingStars => "o.rating_sum",
            BuiltinField::Studio => "o.studio_name",
            BuiltinField::Circle => "o.circle_name",
            BuiltinField::Performer => "o.performer_ids",
            BuiltinField::Cluster => "o.cluster_ids",
            BuiltinField::Tag => "o.tag_ids",
            BuiltinField::Group => "o.group_ids",
            BuiltinField::DurationMs => "o.duration_ms",
            BuiltinField::SizeBytes => "o.size_bytes",
            BuiltinField::Path => "o.paths",
            BuiltinField::State => "o.file_state",
            BuiltinField::ConsentTier => "c.tier",
            BuiltinField::CreatedAt => "o.created_at",
            BuiltinField::UpdatedAt => "o.updated_at",
            BuiltinField::MarkerCount => "o.marker_count",
            BuiltinField::ImageCount => "o.image_count",
            BuiltinField::HasFingerprint => "o.fingerprint_count",
            BuiltinField::HasThumbnail => "o.thumbnail_count",
            BuiltinField::HasTranscript => "o.transcript_count",
            BuiltinField::SceneIndex => "o.scene_index",
            BuiltinField::IsMissing => "(o.missing_file_count > 0)",
        },
    };
    // A kind restriction is a predicate on the same alias, so it composes
    // with the field predicate rather than replacing it.
    let _ = kind;
    Some(base.to_string())
}

// --- base64url, no dependency, ~20 lines ---------------------------------

const B64URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

pub fn base64url_encode(input: &[u8]) -> String {
    // A full group of 3 bytes is 4 characters with no padding. A 2-byte tail is
    // 3 characters plus 1 pad; a 1-byte tail is 2 characters plus 2 pads.
    //
    // The pad count must be exact: '-' is index 62 of the URL alphabet, so a
    // pad cannot be confused with data, and a *missing* pad leaves the decoder
    // reading real characters as the tail. The first version of this encoder
    // emitted one pad character too few for 1- and 2-byte tails, which decoded
    // to a trailing byte of garbage rather than failing.
    const PAD: char = '-';
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64URL[(n >> 18) as usize & 63] as char);
        out.push(B64URL[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(B64URL[(n >> 6) as usize & 63] as char);
        } else {
            out.push(PAD);
        }
        if chunk.len() > 2 {
            out.push(B64URL[n as usize & 63] as char);
        } else {
            out.push(PAD);
        }
    }
    out
}

/// How many pad characters the final 4-character group carries. Padding is
/// always trailing, so counting from the end of the string is sufficient.
fn chunk_pad_count(bytes: &[u8]) -> usize {
    bytes.iter().rev().take_while(|b| **b == b'-').count()
}

pub fn base64url_decode(s: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        B64URL.iter().position(|&x| x == c).map(|p| p as u32)
    }
    // This encoder always emits a whole number of 4-character groups, using
    // '-' for the unused tail positions. So the output length is simply the
    // number of groups times 3, minus however many of those bytes the trailing
    // group padded out. Concretely: 2 significant characters in the last group
    // means 1 byte, 3 means 2 bytes, 4 means 3.
    //
    // Counting significant characters (not pad characters) is what makes this
    // correct: an earlier version truncated by the pad count, which is 3 for a
    // 1-byte input and therefore removed the one real byte along with the junk.
    let bytes: Vec<u8> = s.bytes().filter(|b| *b != b'=').collect();
    if bytes.is_empty() {
        return Some(Vec::new());
    }
    if !bytes.len().is_multiple_of(4) {
        // Not a well-formed length. Reject rather than guess, so a truncated
        // share link is an error and not a differently-filtered query.
        return None;
    }

    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        let mut n = 0u32;
        for (i, &c) in chunk.iter().enumerate() {
            n |= val(c)? << (18 - 6 * i);
        }
        out.push((n >> 16) as u8);
        if chunk.len() > 2 {
            out.push((n >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(n as u8);
        }
    }

    // The final group decoded 3 candidate bytes; as many of those as there were
    // pad characters in it are not real data. `YQ--` has 2 pads so 1 byte
    // survives, `YWI-` has 1 pad so 2 survive, `YWJj` has none so all 3 do.
    let pads = chunk_pad_count(&bytes);
    out.truncate(out.len().saturating_sub(pads));
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn title_eq(v: &str) -> Filter {
        Filter::Facet {
            kind: None,
            field: FieldRef::Builtin(BuiltinField::Title),
            op: CmpOp::Eq,
            values: vec![Value::Str(v.into())],
        }
    }

    #[test]
    fn values_are_always_bound_never_interpolated() {
        // The security property: a value containing SQL appears in `params`
        // and nowhere in the SQL text. If this ever fails, the store is
        // injectable.
        let evil = "'; DROP TABLE object; --";
        let f = Filter::And(vec![
            title_eq(evil),
            Filter::Facet {
                kind: None,
                field: FieldRef::Builtin(BuiltinField::Rating),
                op: CmpOp::Gte,
                values: vec![Value::Float(4.0)],
            },
        ]);
        let out = f.to_sql(Engine::Postgres, &CallerId::anonymous()).unwrap();
        assert!(
            !out.sql.contains("DROP"),
            "user value leaked into SQL text: {}",
            out.sql
        );
        assert!(!out.sql.contains("'"));
        assert_eq!(out.params.len(), 2);
        assert_eq!(out.params[0], Value::Str(evil.into()));
    }

    #[test]
    fn like_metacharacters_are_escaped_in_the_value() {
        let f = Filter::Facet {
            kind: None,
            field: FieldRef::Builtin(BuiltinField::Title),
            op: CmpOp::Contains,
            values: vec![Value::Str("100%".into())],
        };
        let out = f.to_sql(Engine::Sqlite, &CallerId::anonymous()).unwrap();
        assert!(out.params[0].as_str().unwrap().contains("\\%"));
        assert!(!out.sql.contains("100%"));
    }

    #[test]
    fn null_operators_emit_no_placeholder() {
        for (op, expected) in [
            (CmpOp::IsNull, "IS NULL"),
            (CmpOp::IsNotNull, "IS NOT NULL"),
        ] {
            let f = Filter::Facet {
                kind: None,
                field: FieldRef::Builtin(BuiltinField::Description),
                op,
                values: vec![],
            };
            let out = f.to_sql(Engine::Postgres, &CallerId::anonymous()).unwrap();
            assert!(out.sql.contains(expected), "{}", out.sql);
            assert!(out.params.is_empty(), "{op:?} must not bind a value");
            assert!(!out.sql.contains('?'), "{op:?} must not emit a marker");
        }
    }

    #[test]
    fn between_requires_exactly_two_values() {
        let mk = |n: usize| Filter::Facet {
            kind: None,
            field: FieldRef::Builtin(BuiltinField::Date),
            op: CmpOp::Between,
            values: (0..n as i64).map(Value::Int).collect(),
        };
        assert!(mk(2)
            .to_sql(Engine::Postgres, &CallerId::anonymous())
            .is_ok());
        assert_eq!(
            mk(1).to_sql(Engine::Postgres, &CallerId::anonymous()),
            Err(FilterError::BadRange(1))
        );
        assert_eq!(
            mk(3).to_sql(Engine::Postgres, &CallerId::anonymous()),
            Err(FilterError::BadRange(3))
        );
    }

    #[test]
    fn missing_values_is_an_error_not_an_empty_match() {
        let f = Filter::Facet {
            kind: None,
            field: FieldRef::Builtin(BuiltinField::Title),
            op: CmpOp::Eq,
            values: vec![],
        };
        assert_eq!(
            f.to_sql(Engine::Postgres, &CallerId::anonymous()),
            Err(FilterError::MissingValues(CmpOp::Eq))
        );
    }

    #[test]
    fn multi_select_ne_becomes_not_in() {
        let f = Filter::Facet {
            kind: None,
            field: FieldRef::Builtin(BuiltinField::Kind),
            op: CmpOp::Ne,
            values: vec![Value::Str("text".into()), Value::Str("comic".into())],
        };
        let out = f.to_sql(Engine::Postgres, &CallerId::anonymous()).unwrap();
        assert!(out.sql.contains("NOT IN (?, ?)"), "{}", out.sql);
        assert_eq!(out.params.len(), 2);
    }

    #[test]
    fn url_round_trip_is_stable() {
        let f = Filter::And(vec![
            title_eq("a title"),
            Filter::Text { q: "query".into() },
            Filter::Facet {
                kind: Some(ObjectKind::Scene),
                field: FieldRef::Custom("my_field".into()),
                op: CmpOp::In,
                values: vec![Value::Str("a".into()), Value::Int(2)],
            },
        ]);
        let url = f.to_url();
        assert!(
            !url.contains('+') && !url.contains('/') && !url.contains('='),
            "url alphabet must be base64url: {url}"
        );
        let back = Filter::from_url(&url).unwrap();
        assert_eq!(f, back);
        // And encoding twice yields the same string, so a shared URL is stable.
        assert_eq!(url, back.to_url());
    }

    #[test]
    fn from_url_rejects_garbage_without_panicking() {
        assert!(Filter::from_url("!!!not base64!!!").is_err());
        assert!(Filter::from_url("").is_err());
        assert!(Filter::from_url(&base64url_encode(b"not json")).is_err());
    }

    #[test]
    fn deep_nesting_is_rejected() {
        // A hand-crafted URL must not be able to blow the stack.
        let mut f = title_eq("x");
        for _ in 0..64 {
            f = Filter::Not(Box::new(f));
        }
        let err = f
            .to_sql(Engine::Postgres, &CallerId::anonymous())
            .unwrap_err();
        assert!(matches!(err, FilterError::Parse { .. }), "{err:?}");
    }

    #[test]
    fn consent_clause_hides_unverified_from_anonymous() {
        let mut params = Vec::new();
        let sql = Filter::consent_clause(&CallerId::anonymous(), &mut params).unwrap();
        assert!(sql.contains("IN"), "{sql}");
        let tiers: Vec<&str> = params.iter().filter_map(|v| v.as_str()).collect();
        assert_eq!(tiers, ConsentTiers::PUBLIC.to_vec());
        assert!(
            !tiers.contains(&"unverified"),
            "an anonymous viewer must never be granted unverified rows"
        );
    }

    #[test]
    fn consent_clause_shows_unverified_to_the_local_owner() {
        // A freshly scanned file is unverified; the local app must show it.
        let owner = CallerId {
            account_id: Some("acct-1".into()),
            ..CallerId::anonymous()
        };
        let mut params = Vec::new();
        let _ = Filter::consent_clause(&owner, &mut params).unwrap();
        let tiers: Vec<&str> = params.iter().filter_map(|v| v.as_str()).collect();
        assert!(
            tiers.contains(&"unverified"),
            "the local owner must see their own unverified scans: {tiers:?}"
        );
    }

    #[test]
    fn consent_clause_is_a_no_op_only_for_stewards() {
        for role in [Role::Public, Role::Subscriber, Role::Contributor] {
            let c = CallerId {
                role,
                ..CallerId::anonymous()
            };
            let mut params = Vec::new();
            let sql = Filter::consent_clause(&c, &mut params).unwrap();
            assert_ne!(sql, "1 = 1", "{role:?} must not bypass consent");
        }
        // And a steward is *not* waved through either. This assertion used to be
        // `assert_eq!(sql, "1 = 1")` -- it encoded the bug this ticket fixed.
        // A steward is bound to the moderation tiers: the exemption exists
        // because moderation must see contested material, and `1 = 1` granted
        // that by also granting `denied`, which §14.1 calls "permanently
        // blocked by hash across all peers".
        let mut params = Vec::new();
        Filter::consent_clause(&CallerId::steward("acct-2"), &mut params).unwrap();
        let tiers: Vec<&str> = params.iter().filter_map(|v| v.as_str()).collect();
        assert_eq!(tiers, ConsentTiers::MODERATION);
        assert!(
            !tiers.contains(&"denied"),
            "a takedown is not a moderation state"
        );
    }

    #[test]
    fn empty_and_or_are_identity_elements() {
        let f = Filter::And(vec![
            title_eq("kept"),
            Filter::And(vec![]),
            Filter::Or(vec![]),
        ]);
        let out = f.to_sql(Engine::Postgres, &CallerId::anonymous()).unwrap();
        assert!(out.sql.contains("1 = 1"), "{}", out.sql);
        assert!(out.sql.contains("1 = 0"), "{}", out.sql);
    }

    #[test]
    fn base64url_round_trips_every_length_up_to_two_groups() {
        // The first version of this test only checked that decoding did not
        // crash, and it passed while the decoder appended a NUL for every pad
        // character -- so a filter URL decoded to valid-looking bytes followed
        // by trailing garbage. Every length from 0 to 8 must now round-trip to
        // exactly the original bytes.
        for len in 0..=8usize {
            let input: Vec<u8> = (0..len).map(|i| b'a' + (i as u8 % 26)).collect();
            let encoded = base64url_encode(&input);
            assert!(
                !encoded.contains('+') && !encoded.contains('/') && !encoded.contains('='),
                "len {len}: wrong alphabet: {encoded}"
            );
            let decoded = base64url_decode(&encoded)
                .unwrap_or_else(|| panic!("len {len}: decode failed for {encoded}"));
            assert_eq!(
                decoded, input,
                "len {len}: round trip changed the bytes ({encoded})"
            );
        }
    }

    #[test]
    fn base64url_never_emits_a_nul_byte() {
        // The specific symptom of the padding bug: a 1- or 4-byte input decoded
        // with trailing NULs, which is what made `from_url` report "trailing
        // characters" instead of a parse error.
        for len in 1..=8usize {
            let input = vec![b'x'; len];
            let decoded = base64url_decode(&base64url_encode(&input)).unwrap();
            assert!(
                !decoded.contains(&0),
                "len {len}: decoder produced a NUL: {decoded:?}"
            );
            assert_eq!(decoded.len(), len, "len {len}: wrong output length");
        }
    }

    #[test]
    fn base64url_rejects_characters_outside_the_alphabet() {
        assert!(base64url_decode("!!!!").is_none());
        assert!(base64url_decode("ab+c").is_none());
        assert!(base64url_decode("ab/c").is_none());
    }

    #[test]
    fn all_operators_emit_bind_parameters_not_literals() {
        // The plan's acceptance criterion for T-P0-005, asserted over every
        // operator rather than a sample.
        let ops = [
            CmpOp::Eq,
            CmpOp::Ne,
            CmpOp::In,
            CmpOp::NotIn,
            CmpOp::Contains,
            CmpOp::StartsWith,
            CmpOp::Gt,
            CmpOp::Gte,
            CmpOp::Lt,
            CmpOp::Lte,
            CmpOp::Between,
            CmpOp::MatchesRegex,
        ];
        for op in ops {
            let values = match op {
                CmpOp::Between => vec![Value::Int(1), Value::Int(2)],
                CmpOp::In | CmpOp::NotIn => vec![Value::Int(1), Value::Int(2)],
                _ => vec![Value::Str("x".into())],
            };
            let f = Filter::Facet {
                kind: None,
                field: FieldRef::Builtin(BuiltinField::Title),
                op,
                values,
            };
            let out = f
                .to_sql(Engine::Postgres, &CallerId::anonymous())
                .unwrap_or_else(|e| panic!("{op:?} failed to compile: {e}"));
            // The ESCAPE clause in LIKE/StartsWith legitimately contains a
            // quote, so the check is that no *bound value* reached the SQL
            // text, not that the text is quote-free.
            for v in &out.params {
                if let Value::Str(s) = v {
                    assert!(
                        !out.sql.contains(s.as_str()),
                        "{op:?} leaked the value {s:?} into SQL: {}",
                        out.sql
                    );
                }
            }
            assert!(out.sql.contains('?'), "{op:?} emitted no bind marker");
        }
    }
}
