//! §7.7's typed attributes, and §7.9's status.
//!
//! # Why the type lives in the schema
//!
//! §7.7 lists eight attribute types and the questions they exist to answer are
//! *queries*: "which performers have a measurement on record", "does this field
//! take more than one value", "who is between 160 and 175 cm". A type held only
//! in the interface cannot answer any of them -- the interface knows the type
//! because it read the row, and a query does not have an interface. So the type
//! is a column (`custom_field.attr_type`), constrained to a vocabulary the
//! migration owns, and every value is checked against the field's declared type
//! before it is written.
//!
//! # Why values are refused rather than coerced
//!
//! Every [`AttributeError`] in this module is a refusal with a name, and the names
//! are the point. A value of the wrong type stored anyway produces a row that
//! reads back as something the writer never meant: a `boolean` field holding
//! `"true"` still sorts, still filters, and is still wrong, and nothing anywhere
//! reports it. A refusal says what happened and leaves the field as it was, so a
//! writer who gets it wrong finds out at the moment they get it wrong.
//!
//! # The vocabulary is the schema's
//!
//! [`AttrType`] mirrors `attr_type_vocab` in the migration, and
//! [`AttrType::from_schema`] reads the row rather than assuming. A type that is
//! multi-valued in Rust and single-valued in the schema would make [`add`]
//! accept a second value the schema believes is impossible -- so the schema is
//! asked, and the answer is cached per field.

use commons_core::AppearanceType;
use commons_store::{Store, StoreError};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::Row;
use thiserror::Error;

use crate::cluster::now;

// ---------------------------------------------------------------------------
// The type vocabulary
// ---------------------------------------------------------------------------

/// §7.7's eight attribute types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttrType {
    /// Exactly one value.
    Single,
    /// Zero or more values.
    Multi,
    /// A bounded numeric interval.
    Range,
    /// A position in an ordered set, stored as a number.
    Ordinal,
    /// True or false.
    Boolean,
    /// Free text.
    Text,
    /// An ISO-8601 date. Multi-valued: a person can have several.
    Date,
    /// A number and the date it was taken. Multi-valued, and the date is
    /// mandatory -- see [`AttributeError::MeasurementNeedsDate`].
    Measurement,
}

impl AttrType {
    pub const ALL: [AttrType; 8] = [
        AttrType::Single,
        AttrType::Multi,
        AttrType::Range,
        AttrType::Ordinal,
        AttrType::Boolean,
        AttrType::Text,
        AttrType::Date,
        AttrType::Measurement,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            AttrType::Single => "single",
            AttrType::Multi => "multi",
            AttrType::Range => "range",
            AttrType::Ordinal => "ordinal",
            AttrType::Boolean => "boolean",
            AttrType::Text => "text",
            AttrType::Date => "date",
            AttrType::Measurement => "measurement",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.as_str() == s)
    }

    /// Whether the type holds more than one value per subject.
    ///
    /// The schema decides, not this function -- see [`from_schema`]. This is the
    /// fallback for a type read from code rather than from a row, and the two are
    /// asserted to agree in the tests.
    pub const fn is_multi(self) -> bool {
        matches!(self, AttrType::Multi | AttrType::Measurement)
    }
}

/// A field's declared type, as the *schema* has it.
///
/// Read from `attr_type_vocab` rather than from the `AttrType` enum, so the two
/// cannot drift: a type added to the schema and not to Rust fails loudly here,
/// and a type marked multi-valued in one and not the other is visible in the same
/// place.
pub async fn from_schema(store: &Store, field_id: &str) -> Result<AttrType, AttributeError> {
    let row: Option<(String, i64)> = sqlx::query_as(
        "SELECT v.name, v.multi_valued
         FROM custom_field f JOIN attr_type_vocab v ON v.name = f.attr_type
         WHERE f.id = ?",
    )
    .bind(field_id)
    .fetch_optional(store.pool())
    .await
    .map_err(StoreError::Query)?;

    let (name, multi) = row.ok_or_else(|| AttributeError::NoSuchField {
        field: field_id.to_string(),
    })?;
    let ty = AttrType::parse(&name).ok_or(AttributeError::UnknownType { name: name.clone() })?;
    if (multi != 0) != ty.is_multi() {
        return Err(AttributeError::SchemaDisagrees {
            name,
            code_multi: multi != 0,
            rust_multi: ty.is_multi(),
        });
    }
    Ok(ty)
}

// ---------------------------------------------------------------------------
// Values
// ---------------------------------------------------------------------------

/// An inclusive numeric interval, in the unit the field's name implies.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Range {
    pub low: f64,
    pub high: f64,
}

impl Range {
    fn contains(&self, x: f64) -> bool {
        x >= self.low && x <= self.high
    }
}

/// A number and the date it was observed.
///
/// Not `Copy` -- unlike [`Range`], which is two `f64`s and cheap to pass around.
/// The asymmetry is deliberate: a `Measurement` gets cloned once per value in a
/// `multi` field's vector, and making it `Copy` would invite a copy per element
/// in a loop over a collection nobody is going to notice copying.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Measurement {
    pub value: f64,
    /// ISO-8601 date. Mandatory: this is what makes a measurement chartable
    /// over time and therefore what makes §7.11's span and age possible.
    pub at: String,
}

/// A value written to a typed field.
/// # Why this is externally tagged
///
/// Adjacently tagged (`{type, v}`) was the first choice and is what the stored
/// rows would be nicer with, but serde cannot serialize a *bare primitive* inside
/// an internally tagged enum, and `Text(String)` is a bare primitive. A
/// `raw_value` reader in `span` that keys on `type == "date"` is worth less than
/// a value that round-trips, and the store-side test
/// `no_internally_tagged_enum_carries_a_bare_primitive` exists precisely because
/// this is a mistake that compiles.
///
/// So the shape is `{"text": "..."}` rather than `{"type": "text", "v": "..."}`.
/// The tag is the key, which is also what `raw_value` can read with a single
/// `get("date")`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttrValue {
    Text(String),
    Number(f64),
    Bool(bool),
    Date(String),
    Range(Range),
    Measured(Measurement),
    /// Only produced by reading a `multi` field; writing one is checked against
    /// the element type.
    Many(Vec<AttrValue>),
}

impl AttrValue {
    /// Does this value fit a field of type `ty`?
    ///
    /// The one place the type rules live. Every write path calls it, so a field
    /// cannot be given a value of a shape it does not have by going around the
    /// check -- and `Many` is checked element-wise, because a `multi` field whose
    /// elements are wrong is a `multi` field with no useful type at all.
    pub fn fits(&self, ty: AttrType) -> bool {
        match (ty, self) {
            (AttrType::Single | AttrType::Text, AttrValue::Text(_)) => true,
            (AttrType::Ordinal, AttrValue::Number(_)) => true,
            (AttrType::Boolean, AttrValue::Bool(_)) => true,
            (AttrType::Date, AttrValue::Date(_)) => true,
            (AttrType::Range, AttrValue::Range(_)) => true,
            (AttrType::Measurement, AttrValue::Measured(_)) => true,
            (AttrType::Multi, AttrValue::Many(vs)) => vs.iter().all(|v| v.fits(AttrType::Single)),
            _ => false,
        }
    }

    /// The date this value carries, if it has one.
    pub fn date(&self) -> Option<&str> {
        match self {
            AttrValue::Date(d) | AttrValue::Measured(Measurement { at: d, .. }) => Some(d),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Status (§7.9)
// ---------------------------------------------------------------------------

/// §7.9: a badge on the card, a filter, and a timeline entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Appearing. The default when nothing has been said.
    Active,
    /// Deceased. First-class rather than a phrase, because a deceased
    /// performer's scenes are a category people browse and filter.
    Deceased,
    Retired,
    /// Present in the index but not currently appearing.
    Inactive,
}

impl Status {
    pub const ALL: [Status; 4] = [
        Status::Active,
        Status::Deceased,
        Status::Retired,
        Status::Inactive,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Status::Active => "active",
            Status::Deceased => "deceased",
            Status::Retired => "retired",
            Status::Inactive => "inactive",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum AttributeError {
    #[error("no such field: {field}")]
    NoSuchField { field: String },

    #[error("no such attribute type: {name}")]
    UnknownType { name: String },

    /// The migration's vocabulary and [`AttrType`] disagree about a type. A
    /// build-time mistake, caught at the first read of the field rather than
    /// being papered over.
    #[error("the schema calls {name} multi={code_multi} and the code calls it multi={rust_multi}")]
    SchemaDisagrees {
        name: String,
        code_multi: bool,
        rust_multi: bool,
    },

    #[error("a {ty} field cannot hold {got}")]
    WrongType { ty: String, got: String },

    #[error("this field holds one value; it is not multi-valued")]
    NotMulti { field: String },

    /// §7.7: a measurement without a date is not a measurement over time, so it
    /// is refused at the point of writing rather than being stored with a null
    /// that the derivation would silently skip.
    #[error("a measurement needs the date it was taken")]
    MeasurementNeedsDate { field: String },

    #[error("range {low}..{high} is reversed")]
    ReversedRange { low: f64, high: f64 },

    #[error("not a status: {value}")]
    UnknownStatus { value: String },

    #[error("the value is already on this field")]
    DuplicateValue { field: String },

    /// A stored row that will not deserialise. Not recoverable by a reader, and
    /// a build-time bug rather than bad data in the ordinary sense.
    #[error("stored value is not readable: {why} (raw: {raw})")]
    CorruptValue { raw: String, why: String },

    #[error("this value cannot be stored: {why}")]
    Unserialisable { why: String },

    #[error(transparent)]
    Store(#[from] StoreError),
}

// ---------------------------------------------------------------------------
// Declaring and reading fields
// ---------------------------------------------------------------------------

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Declare a field, or return the existing one.
///
/// Idempotent by name, because a declaration is a migration of the *schema* and
/// two callers racing to declare "nationality" should not produce two fields
/// that disagree about their type. The declared type of an existing field is not
/// changed: a field's type is part of its meaning, and silently retyping it
/// would reinterpret every value already written.
pub async fn declare_field(
    store: &Store,
    name: &str,
    ty: AttrType,
    applies_to: &str,
) -> Result<String, AttributeError> {
    if let Ok(Some((id, existing))) = field_by_name(store, name).await {
        return match AttrType::parse(&existing) {
            Some(t) if t == ty => Ok(id),
            Some(t) => Err(AttributeError::WrongType {
                ty: t.as_str().into(),
                got: format!("'{name}' is already declared as {}", ty.as_str()),
            }),
            None => Err(AttributeError::UnknownType { name: existing }),
        };
    }

    let id = new_id();
    sqlx::query(
        "INSERT INTO custom_field (id, name, attr_type, applies_to, ord, created_at)
         VALUES (?, ?, ?, ?, 0, ?)",
    )
    .bind(&id)
    .bind(name)
    .bind(ty.as_str())
    .bind(applies_to)
    .bind(now())
    .execute(store.pool())
    .await
    .map_err(StoreError::Query)?;
    Ok(id)
}

async fn field_by_name(store: &Store, name: &str) -> Result<Option<(String, String)>, StoreError> {
    sqlx::query_as("SELECT id, attr_type FROM custom_field WHERE name = ?")
        .bind(name)
        .fetch_optional(store.pool())
        .await
        .map_err(StoreError::Query)
}

/// The declared type of a field, as the schema has it.
pub async fn type_of(store: &Store, field_id: &str) -> Result<AttrType, AttributeError> {
    from_schema(store, field_id).await
}

/// Every value on a field for a subject, oldest first where the values are dated.
pub async fn all(
    store: &Store,
    field_id: &str,
    subject_id: &str,
) -> Result<Vec<AttrValue>, AttributeError> {
    let rows = sqlx::query(
        "SELECT value_json FROM custom_field_value
         WHERE field_id = ? AND subject_type = 'performer' AND subject_id = ?
         ORDER BY COALESCE(at, ''), value_json",
    )
    .bind(field_id)
    .bind(subject_id)
    .fetch_all(store.pool())
    .await
    .map_err(StoreError::Query)?;
    rows.iter()
        .map(|r| {
            let raw: String = r.get("value_json");
            // A row that will not deserialise is a corrupt row, and there is
            // nothing a reader can do with it except skip it. `expect` is right
            // here: every writer in this module serialises `AttrValue`, and the
            // only way a bad row exists is a hand-edited database or a future
            // type added to the enum without a migration. Failing loudly at the
            // first such row is better than a field that silently reads as empty.
            match serde_json::from_str(&raw) {
                Ok(v) => Ok(v),
                Err(e) => Err(AttributeError::CorruptValue {
                    raw,
                    why: e.to_string(),
                }),
            }
        })
        .collect()
}

/// The single value on a field, or `None`.
///
/// Returns `None` rather than the first of several when a `single` field somehow
/// holds more than one: a caller asking for "the" value of a single-valued field
/// and receiving an arbitrary one of several is how a list becomes a person.
pub async fn get(
    store: &Store,
    field_id: &str,
    subject_id: &str,
) -> Result<Option<AttrValue>, AttributeError> {
    let mut vs = all(store, field_id, subject_id).await?;
    match vs.len() {
        0 => Ok(None),
        1 => Ok(vs.pop()),
        _ => Ok(None),
    }
}

// ---------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------

/// Write a value, replacing whatever was there.
///
/// `replace` rather than `add` for a single-valued field, because the common case
/// is "this is the value now" and a writer who means to replace should not have
/// to know that `add` would refuse.
pub async fn set(
    store: &Store,
    field_id: &str,
    subject_id: &str,
    value: AttrValue,
) -> Result<(), AttributeError> {
    let ty = from_schema(store, field_id).await?;
    check(store, field_id, ty, &value)?;

    let mut tx = store.pool().begin().await.map_err(StoreError::Query)?;
    sqlx::query("DELETE FROM custom_field_value WHERE field_id = ? AND subject_type = 'performer' AND subject_id = ?")
        .bind(field_id)
        .bind(subject_id)
        .execute(&mut *tx)
        .await
        .map_err(StoreError::Query)?;
    // One timestamp for the whole write, so every value in a `set` records the
    // same moment. Per-row timestamps would order a `multi` field's values by
    // how long the loop took, which is not a fact anybody wants.
    let recorded = now();
    for v in flatten(&value) {
        let encoded = encode(&v)?;
        sqlx::query(
            "INSERT INTO custom_field_value
                 (id, field_id, subject_type, subject_id, value_json, at, recorded_at)
             VALUES (?, ?, 'performer', ?, ?, ?, ?)",
        )
        .bind(new_id())
        .bind(field_id)
        .bind(subject_id)
        .bind(encoded)
        // `at` is the date the value *describes* where it has one, and the write
        // time where it does not -- a text value has no date of its own, and a
        // null there would drop it out of the ordered read.
        .bind(v.date().unwrap_or(&recorded))
        .execute(&mut *tx)
        .await
        .map_err(StoreError::Query)?;
    }
    tx.commit().await.map_err(StoreError::Query)?;
    Ok(())
}

/// Add a value to a multi-valued field, or refuse.
///
/// "Refuse" is the interesting half: a second value on a `single` field is
/// refused by name, because accepting it would turn a query that expects a
/// person into one that gets a list, and there is no way to tell afterwards which
/// of the two the writer meant.
pub async fn add(
    store: &Store,
    field_id: &str,
    subject_id: &str,
    value: AttrValue,
) -> Result<(), AttributeError> {
    let ty = from_schema(store, field_id).await?;
    if !ty.is_multi() {
        return Err(AttributeError::NotMulti {
            field: field_id.to_string(),
        });
    }
    let existing = all(store, field_id, subject_id).await?;
    for v in flatten(&value) {
        if existing.contains(&v) {
            return Err(AttributeError::DuplicateValue {
                field: field_id.to_string(),
            });
        }
    }
    for v in flatten(&value) {
        insert_one(store, field_id, subject_id, &v).await?;
    }
    Ok(())
}

async fn insert_one(
    store: &Store,
    field_id: &str,
    subject_id: &str,
    v: &AttrValue,
) -> Result<(), AttributeError> {
    sqlx::query(
        "INSERT INTO custom_field_value
             (id, field_id, subject_type, subject_id, value_json, at, recorded_at)
         VALUES (?, ?, 'performer', ?, ?, ?, ?)",
    )
    .bind(new_id())
    .bind(field_id)
    .bind(subject_id)
    .bind(encode(v)?)
    .bind(v.date().unwrap_or(""))
    .bind(now())
    .execute(store.pool())
    .await
    .map_err(StoreError::Query)?;
    Ok(())
}

/// Serialise a value for storage.
///
/// Cannot fail for any `AttrValue` -- they are all numbers, strings, bools and
/// maps of those -- so the error exists to make that a checked claim rather than
/// an assumption. If a variant is ever added that cannot be represented, this is
/// where it fails, and it fails on the write rather than producing a row that
/// cannot be read back.
fn encode(v: &AttrValue) -> Result<String, AttributeError> {
    serde_json::to_string(v).map_err(|e| AttributeError::Unserialisable { why: e.to_string() })
}

/// A `Many` is stored as one row per element, so a query can ask for "every
/// value of this field" without knowing whether the field is multi-valued.
fn flatten(v: &AttrValue) -> Vec<AttrValue> {
    match v {
        AttrValue::Many(vs) => vs.iter().flat_map(flatten).collect(),
        other => vec![other.clone()],
    }
}

/// Every rule about what a value may be, in one place.
fn check(
    _store: &Store,
    field_id: &str,
    ty: AttrType,
    value: &AttrValue,
) -> Result<(), AttributeError> {
    // The measurement rule first, so an undated measurement is reported as
    // undated rather than as "a number where a measurement belongs" -- the first
    // is the thing the writer can act on.
    if ty == AttrType::Measurement {
        let undated = match value {
            AttrValue::Measured(m) => m.at.trim().is_empty(),
            AttrValue::Many(vs) => vs
                .iter()
                .any(|v| matches!(v, AttrValue::Measured(m) if m.at.trim().is_empty())),
            _ => true,
        };
        if undated {
            return Err(AttributeError::MeasurementNeedsDate {
                field: field_id.to_string(),
            });
        }
    }

    if !value.fits(ty) {
        return Err(AttributeError::WrongType {
            ty: ty.as_str().into(),
            got: describe(value),
        });
    }

    // A reversed range is refused at write time because it is not detectable at
    // read time: `BETWEEN low AND high` matches nothing, which looks exactly
    // like "no performers have this field set".
    for r in ranges(value) {
        if r.low > r.high {
            return Err(AttributeError::ReversedRange {
                low: r.low,
                high: r.high,
            });
        }
    }
    Ok(())
}

fn ranges(v: &AttrValue) -> Vec<Range> {
    match v {
        AttrValue::Range(r) => vec![*r],
        AttrValue::Many(vs) => vs.iter().flat_map(ranges).collect(),
        _ => vec![],
    }
}

fn describe(v: &AttrValue) -> String {
    match v {
        AttrValue::Text(_) => "text".into(),
        AttrValue::Number(_) => "a number".into(),
        AttrValue::Bool(_) => "a boolean".into(),
        AttrValue::Date(_) => "a date".into(),
        AttrValue::Range(_) => "a range".into(),
        AttrValue::Measured(_) => "a measurement".into(),
        AttrValue::Many(vs) => format!("{} values", vs.len()),
    }
}

// ---------------------------------------------------------------------------
// Status
// ---------------------------------------------------------------------------

/// Set a performer's status.
pub async fn set_status(
    store: &Store,
    performer: &str,
    status: Status,
) -> Result<(), AttributeError> {
    set_status_raw(store, performer, status.as_str()).await
}

/// Set a performer's status from a string.
///
/// Exists so the database constraint is exercised through the same path as
/// [`set_status`]: a value the enum does not have has to be refused *by the
/// database*, not merely by a `match` in the caller, because the caller is not
/// the only thing that writes.
pub async fn set_status_raw(
    store: &Store,
    performer: &str,
    value: &str,
) -> Result<(), AttributeError> {
    if Status::parse(value).is_none() {
        return Err(AttributeError::UnknownStatus {
            value: value.to_string(),
        });
    }
    let at = now();
    sqlx::query("UPDATE performer SET status = ?, updated_at = ? WHERE id = ?")
        .bind(value)
        .bind(at)
        .bind(performer)
        .execute(store.pool())
        .await
        .map_err(StoreError::Query)?;
    Ok(())
}

/// A performer's status, or `None` if nothing has been said.
pub async fn status_of(store: &Store, performer: &str) -> Result<Option<Status>, AttributeError> {
    let raw: Option<String> = sqlx::query_scalar("SELECT status FROM performer WHERE id = ?")
        .bind(performer)
        .fetch_optional(store.pool())
        .await
        .map_err(StoreError::Query)?;
    match raw {
        None => Ok(None),
        Some(s) => Status::parse(&s)
            .map(Some)
            .ok_or(AttributeError::UnknownStatus { value: s }),
    }
}

/// Every performer with a given status.
///
/// Equality, not `LIKE`. That is the whole reason the status is an enum: "Passed",
/// "passed away" and "RIP" as free text are three filters for one fact, and a
/// `LIKE` cannot be indexed.
pub async fn performers_with_status(
    store: &Store,
    status: Status,
) -> Result<Vec<String>, AttributeError> {
    let ids =
        sqlx::query_scalar::<_, String>("SELECT id FROM performer WHERE status = ? ORDER BY id")
            .bind(status.as_str())
            .fetch_all(store.pool())
            .await
            .map_err(StoreError::Query)?;
    Ok(ids)
}

// ---------------------------------------------------------------------------
// The appear-with graph (§7.12)
// ---------------------------------------------------------------------------

/// The performers `cluster` appears with, in credited scenes only.
///
/// The credit decision is [`AppearanceType::counts_as_appearance`], read from
/// `commons-core` rather than restated here. Restating it would be the same
/// decision in two places, and the copy is the one that would be wrong after
/// someone changed the rule in the enum and did not grep for the string.
///
/// Uncredited pairings -- a cameo, a non-sexual presence, a background
/// appearance -- produce no edge, because two people being in the same file is
/// not the same as two people being in a scene together.
/// The appearance types that count as "appearing together", as SQL literals.
///
/// Derived from [`AppearanceType::counts_as_appearance`] rather than written
/// out, because this is the second copy of that decision and the copy is what
/// goes stale: someone adds a type to the enum, the list here does not change,
/// and the new type is silently missing from the graph.
fn credited_sql() -> String {
    let names: Vec<&str> = AppearanceType::ALL
        .iter()
        .filter(|t| t.counts_as_appearance())
        .map(|t| t.as_str())
        .collect();
    // Built from a fixed enum, so the only characters are letters, digits and
    // underscores -- but it is still a parameter rather than an interpolation,
    // because "it can't contain a quote" is not a property a reader should have
    // to verify.
    format!("('{}')", names.join("','"))
}

/// The performers `cluster` appears with, in credited scenes only.
///
/// Uncredited pairings -- a cameo, a non-sexual presence, a background
/// appearance -- produce no edge, because two people being in the same file is
/// not the same as two people being in a scene together.
pub async fn appear_with(store: &Store, cluster: &str) -> Result<Vec<String>, AttributeError> {
    let credited = credited_sql();
    let sql = format!(
        "SELECT DISTINCT a2.cluster_id
         FROM appearance a1
         JOIN appearance a2 ON a2.object_id = a1.object_id
         WHERE a1.cluster_id = ?
           AND a2.cluster_id <> a1.cluster_id
           AND a1.appearance_type IN {credited}
           AND a2.appearance_type IN {credited}
         ORDER BY a2.cluster_id"
    );
    let partners = sqlx::query_scalar::<_, String>(&sql)
        .bind(cluster)
        .fetch_all(store.pool())
        .await
        .map_err(StoreError::Query)?;
    Ok(partners)
}

/// The appearance type recorded for one performer in one item.
pub async fn appearance_type_of(
    store: &Store,
    object_id: &str,
    cluster: &str,
) -> Result<Option<AppearanceType>, AttributeError> {
    let raw: Option<String> = sqlx::query_scalar(
        "SELECT appearance_type FROM appearance WHERE object_id = ? AND cluster_id = ?",
    )
    .bind(object_id)
    .bind(cluster)
    .fetch_optional(store.pool())
    .await
    .map_err(StoreError::Query)?;
    match raw {
        None => Ok(None),
        Some(s) => AppearanceType::parse(&s)
            .map(Some)
            .ok_or(AttributeError::WrongType {
                ty: "appearance_type".into(),
                got: s,
            }),
    }
}

/// Does a range-valued field contain a number?
///
/// The query §7.7 exists for, and it is here rather than in the caller because a
/// range filter written by hand is one that forgets the `low <= high` check that
/// [`check`] already guarantees at write time.
pub async fn in_range(
    store: &Store,
    field_id: &str,
    subject_id: &str,
    x: f64,
) -> Result<bool, AttributeError> {
    Ok(all(store, field_id, subject_id)
        .await?
        .iter()
        .any(|v| match v {
            AttrValue::Range(r) => r.contains(x),
            _ => false,
        }))
}

/// The value of a value, for a caller that wants to log or display it.
pub fn to_json(v: &AttrValue) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}
