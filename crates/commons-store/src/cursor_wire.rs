//! The wire form of a keyset cursor. T-P6-009, spec
//! `docs/spec/t-p6-009-cursor-encoding.md` §2–§4.
//!
//! # Why this file exists at all
//!
//! Keyset paging worked inside one process from the day `Sort` landed, and has
//! never left it. `Cursor`'s only constructor is `#[cfg(test)] pub(crate)`, and
//! `Cursor::from_row` is `pub(crate)`, so nothing outside the store can build
//! one — which is the point. `Filter` has `to_url`/`from_url` and crossed a
//! process boundary long ago; the cursor had no equivalent, and no surface had
//! ever needed one until GraphQL's `PageInput.after`.
//!
//! # The one thing this encoding has to get right
//!
//! **A cursor is not self-describing, and a filter is.** `Filter::from_url`
//! decodes a value that says what it is. A cursor is a bare `Vec<Value>` in
//! `Sort::all_keys()` order, and *which sort that is* is not in the value.
//!
//! The consequence is specific and it is not a crash. `after_binds` checks
//! only `cursor.len() == all_keys().len()` — a **length** check. So a
//! `date_desc` cursor handed to a `title ASC` sort:
//!
//! - has the same arity (2 + the `id` tiebreak), so the assert passes;
//! - is `[Str(date), Str(id)]` against `[Str(title), Str(id)]`, so the values
//!   are type-compatible;
//! - produces `o.title < '2026-01-01'`, which is a perfectly valid text
//!   comparison that **returns rows**.
//!
//! You get a page, computed with the wrong column, and nothing anywhere errors.
//! That is precisely the failure `Cursor`'s own doc comment says the type
//! exists to prevent — "a cursor cannot be hand-assembled with the wrong arity,
//! which is the failure that turns into a silently wrong page rather than an
//! error" — reintroduced at the wire, by an encoding that carried the values
//! and dropped the sort.
//!
//! **So the wire form carries a fingerprint of the sort it was made for, and
//! `from_url` is given the sort it is being decoded *for*.** That is the whole
//! design. Everything else here is detail.
//!
//! (Cross-*type* mismatches are the benign case and worth naming so the
//! distinction is not lost: a `date` cursor seeked in a `rating_sum` sort binds
//! a string to an integer column, the comparison is never true, and the result
//! is an **empty page**. Visible, if unhelpful. The dangerous case is
//! cross-*sort* among the four text-valued keys — `Date`, `Title`, `Kind`,
//! `AddedAt` are all `Value::Str` — which is why the test that pins this uses
//! two *text* sorts and not a text/int pair.)

use serde::{Deserialize, Serialize};

use crate::filter_ast::{base64url_decode, base64url_encode, Value};
use crate::sort::{Cursor, KeyType, Sort};

/// The encoding this module writes.
///
/// A `from_url` that sees any other number **refuses**. It does not assume 1,
/// for the same reason `Filter::from_url` refuses a malformed filter rather
/// than defaulting: a decoder that guesses is a decoder that will one day guess
/// wrong, and here "wrong" is a wrong page rather than an error.
pub const CURSOR_WIRE_VERSION: u8 = 1;

/// Why a cursor could not be decoded.
///
/// Every variant names **what was wrong and what was expected**. The GraphQL
/// resolver turns these straight into a message the user reads, and a refusal
/// that cannot explain itself is a refusal nobody can act on.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CursorError {
    #[error("not a cursor: the value is not valid base64url")]
    NotBase64,

    #[error("not a cursor: the value is not the expected JSON shape ({0})")]
    Shape(String),

    #[error(
        "unsupported cursor version {found}; this server writes version {CURSOR_WIRE_VERSION}"
    )]
    Version { found: u8 },

    /// The mismatch this module exists to catch. See the module docs.
    #[error(
        "this cursor was made for a different sort (its fingerprint is {found}, this query's is {expected})"
    )]
    SortMismatch { found: String, expected: String },

    #[error("wrong number of keys: this cursor carries {found}, this sort has {expected}")]
    Arity { found: usize, expected: usize },

    #[error("key {index} is {got}, but this sort's {column} column holds {expected}")]
    KeyType {
        index: usize,
        column: &'static str,
        got: &'static str,
        expected: &'static str,
    },

    /// Not the same as [`CursorError::KeyType`]: a `List` has no `SortKey` whose
    /// column it could match, so there is no "expected" column to name. Two
    /// variants because the sentences differ — "this should have been an
    /// integer" versus "this should have been a scalar at all".
    #[error("key {index} is {got}, and a sort key is always a scalar")]
    NotScalar { index: usize, got: &'static str },
}

/// One cursor key, with its `Value` variant spelled out.
///
/// Serialises as `["s", "…"]`, `["i", 42]`, `["n"]`.
///
/// **The tag is load-bearing, not decoration.** Without it `Value::Int(42)`
/// and `Value::Str("42")` are the same JSON, and a cursor that decoded an
/// integer where the column holds text binds a number to a string and the
/// comparison silently does the wrong thing. `Value::Null` and `Value::Str("")`
/// have the same problem from the other side — which is why `o.date`, which is
/// nullable, needs a spelling for "no value" that is not "empty string".
///
/// # Why both directions are hand-written
///
/// The format is a two-element array: `["s", "2026-01-01"]`. **Serde cannot
/// derive that shape for a newtype enum.** Its options are all wrong for this:
///
/// - the default serialises `S(String)` as the bare string `"x"`, with no room
///   for a tag at all — which is the thing the tag exists to prevent;
/// - `#[serde(tag = "t", content = "v")]` produces `{"t":"s","v":"x"}`, an
///   object, not an array. It is the closest derive available and it is still
///   the wrong shape, and it was tried: `from_url` rejected the array form
///   outright, which is how the mismatch was found.
///
/// An array is smaller than an object for the same information (no repeated
/// key names), and this value goes in a URL. The cost is ~30 lines of impl and
/// the loss of a derive that would have caught a variant being added without a
/// case here — so `Tagged::from_value` and `to_value` are the two functions a
/// new `Value` variant would have to touch, and both are exhaustive matches
/// with no `_ =>` arm, so the compiler names the omission.
#[derive(Debug, Clone, PartialEq)]
enum Tagged {
    S(String),
    I(i64),
    /// No `SortKey` produces a float today. The variant exists so adding one
    /// is not also a wire change, and so a cursor carrying one is refused by
    /// the type check with a real message rather than by a JSON parse error.
    F(f64),
    B(bool),
    /// A NULL column — `["n"]`, a one-element array.
    N,
}

impl Serialize for Tagged {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        // `use_ser::serialize_tuple` with a FIXED arity per variant, so the
        // output is a JSON array and not an object, and `N` is a one-element
        // array rather than a zero-element one. A zero-length tuple would
        // serialise as `[]`, which is indistinguishable from "an empty key
        // list" at the envelope level -- so NULL carries an explicit slot.
        use serde::ser::SerializeTuple;
        let mut t = s.serialize_tuple(arity(self))?;
        t.serialize_element(tag_of(self))?;
        match self {
            Tagged::S(v) => t.serialize_element(v)?,
            Tagged::I(v) => t.serialize_element(v)?,
            Tagged::F(v) => t.serialize_element(v)?,
            Tagged::B(v) => t.serialize_element(v)?,
            Tagged::N => {}
        }
        t.end()
    }
}

impl<'de> Deserialize<'de> for Tagged {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::{Error as _, SeqAccess, Visitor};
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Tagged;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                // The message a malformed cursor's user reads. It names the
                // shape, because "invalid type" does not.
                f.write_str(r#"a cursor key: ["s", <string>], ["i", <integer>], ["f", <float>], ["b", <bool>], or ["n"]"#)
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Tagged, A::Error> {
                let tag: String = seq.next_element()?.ok_or_else(|| {
                    A::Error::custom("a cursor key needs a tag as its first element")
                })?;
                // A macro rather than a helper function: the four arms differ
                // only in the type they produce, and a generic `next_value`
                // needs an `Fn(&str) -> A::Error` closure that serde's
                // higher-ranked Error bound cannot satisfy (it is
                // "implementation of Fn is not general enough"). Writing the
                // four arms out is not duplication worth removing -- it is
                // four lines, and each one's error message names its own tag.
                macro_rules! value {
                    ($tag:literal) => {
                        seq.next_element()?.ok_or_else(|| {
                            A::Error::custom(concat!(
                                "a cursor key tagged ",
                                $tag,
                                " needs a value after its tag"
                            ))
                        })
                    };
                }
                Ok(match tag.as_str() {
                    "s" => Tagged::S(value!("s")?),
                    "i" => Tagged::I(value!("i")?),
                    "f" => Tagged::F(value!("f")?),
                    "b" => Tagged::B(value!("b")?),
                    "n" => Tagged::N,
                    other => {
                        return Err(A::Error::custom(format!(
                            "{other:?} is not a cursor key tag (s, i, f, b or n)"
                        )))
                    }
                })
            }
        }
        d.deserialize_seq(V)
    }
}

fn tag_of(t: &Tagged) -> &'static str {
    match t {
        Tagged::S(_) => "s",
        Tagged::I(_) => "i",
        Tagged::F(_) => "f",
        Tagged::B(_) => "b",
        Tagged::N => "n",
    }
}

fn arity(t: &Tagged) -> usize {
    match t {
        Tagged::N => 1,
        _ => 2,
    }
}

impl Tagged {
    /// `None` for a `Value::List`: a sort key is a column value and always
    /// scalar, so a list has no `SortKey` it could legitimately be.
    fn from_value(v: &Value) -> Option<Self> {
        Some(match v {
            Value::Str(s) => Tagged::S(s.clone()),
            Value::Int(i) => Tagged::I(*i),
            Value::Float(f) => Tagged::F(*f),
            Value::Bool(b) => Tagged::B(*b),
            Value::Null => Tagged::N,
            Value::List(_) => return None,
        })
    }

    /// Consumes, deliberately: `S` owns a `String` and this moves it rather
    /// than cloning it, which is the whole point of taking `self` here.
    #[allow(clippy::wrong_self_convention)]
    fn to_value(self) -> Value {
        match self {
            Tagged::S(s) => Value::Str(s),
            Tagged::I(i) => Value::Int(i),
            Tagged::F(f) => Value::Float(f),
            Tagged::B(b) => Value::Bool(b),
            Tagged::N => Value::Null,
        }
    }

    fn type_name(&self) -> &'static str {
        match self {
            Tagged::S(_) => "a string",
            Tagged::I(_) => "an integer",
            Tagged::F(_) => "a float",
            Tagged::B(_) => "a boolean",
            Tagged::N => "null",
        }
    }
}

/// The envelope. Canonical JSON, then base64url — the same two steps
/// `Filter::to_url` takes, and for the same reason: §15.10 shareable URLs, and
/// the alphabet is already implemented and tested in `filter_ast.rs`.
///
/// # Field order is part of the format, and that is a serde trap
///
/// The three fields are declared `v`, `f`, `k` and that order is what
/// `to_url` writes. **serde's derived `Deserialize` for a struct consumes fields
/// in DECLARATION order**, so a document with the same three keys in a
/// different order does not deserialize — and `serde_json::json!` sorts its
/// keys alphabetically, so a test (or any other producer) building the envelope
/// through it emits `f`, `k`, `v` and gets a `Shape` error pointing at a
/// column in the middle of the fingerprint.
///
/// The first version of the test fixture did exactly that and every test in the
/// file failed at `date_cursor()`, which is a fixture bug wearing the costume
/// of a decoder bug.
///
/// The fix has two halves, and the second is the one that keeps this from
/// happening again:
/// 1. `#[serde(deny_unknown_fields)]` is **not** the answer — the problem is
///    order, not extra keys — so the decoder is made order-independent.
/// 2. The decoder reads the envelope as a `serde_json::Value` first and pulls
///    the three fields out by name. Slightly more code, and it means a future
///    field can be added without anyone having to remember that a producer
///    somewhere might be emitting them in a different order.

impl Wire {
    /// Read an envelope, whichever order its keys arrive in.
    ///
    /// Order-independent because this reads the three fields by name; writing
    /// still emits declaration order, because that is the canonical form and
    /// the golden test pins it.
    fn parse(json: &[u8]) -> Result<Self, CursorError> {
        let v: serde_json::Value =
            serde_json::from_slice(json).map_err(|e| CursorError::Shape(e.to_string()))?;
        let obj = v
            .as_object()
            .ok_or_else(|| CursorError::Shape("not a JSON object".into()))?;
        let get = |name: &str| -> Result<serde_json::Value, CursorError> {
            obj.get(name)
                .cloned()
                .ok_or_else(|| CursorError::Shape(format!("no `{name}` field")))
        };
        // Each field is deserialized from its own `Value`, so its own error
        // names the field rather than a byte column.
        let v: u8 = serde_json::from_value(get("v")?)
            .map_err(|e| CursorError::Shape(format!("`v`: {e}")))?;
        let f: String = serde_json::from_value(get("f")?)
            .map_err(|e| CursorError::Shape(format!("`f`: {e}")))?;
        let k: Vec<Tagged> = serde_json::from_value(get("k")?)
            .map_err(|e| CursorError::Shape(format!("`k`: {e}")))?;
        Ok(Wire { v, f, k })
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Wire {
    /// Encoding version. See [`CURSOR_WIRE_VERSION`].
    v: u8,
    /// [`Sort::fingerprint`] of the sort this cursor was made for.
    f: String,
    k: Vec<Tagged>,
}

impl Cursor {
    /// The wire form: a URL-safe string safe to put in a query parameter.
    ///
    /// # Why this takes the sort as well
    ///
    /// Because **a cursor does not know which sort it belongs to.** It is a
    /// `Vec<Value>` and nothing more; the values are only meaningful in the
    /// order `all_keys()` gave them, and two sorts of the same arity are
    /// indistinguishable from the values alone. So the sort is supplied here as
    /// well as to [`Self::from_url`], and the pair of them is the whole
    /// contract: *this is where you are, in this order.*
    ///
    /// A `to_url` that could work without the sort would be encoding a cursor
    /// that cannot be decoded safely, which is the failure the module docs
    /// describe. Taking the sort on both sides is the fix, and it is why the
    /// signature is `to_url(&self, sort)` rather than the more convenient
    /// `to_url(&self)`.
    ///
    /// **Infallible by construction**, and it panics rather than returning a
    /// `Result` for the reason `Filter::to_url` does: the payload is a `u8`, a
    /// `String` and a `Vec` of scalars, none of which can fail to serialize. A
    /// failure here is a bug in this crate, so it is reported as one.
    ///
    /// The alternative is worse than a panic and worth naming, because the
    /// codebase has already done it once: `Filter::to_url`'s comment records
    /// that an earlier version fell back to `Filter::All` on a serialization
    /// error, "which turns a serialization bug into a user silently seeing the
    /// entire library."
    pub fn to_url(&self, sort: &Sort) -> String {
        let wire = Wire {
            v: CURSOR_WIRE_VERSION,
            f: sort.fingerprint(),
            k: self
                .values()
                .iter()
                .map(|v| {
                    Tagged::from_value(v)
                        // The only `Value` that cannot be tagged is a `List`,
                        // and `Cursor` cannot hold one: `from_row` builds values
                        // from columns, and `from_url` refuses a list before it
                        // gets here. So this is a crate invariant, not input.
                        .expect("a Cursor's values are columns, and a column is scalar")
                })
                .collect(),
        };
        let mut buf: Vec<u8> = Vec::new();
        serde_json::to_writer(&mut buf, &wire)
            .expect("a cursor is always serializable; this is a crate bug, not bad input");
        base64url_encode(&buf)
    }

    /// Decode a cursor **for a specific sort**.
    ///
    /// # Why the sort is an argument
    ///
    /// Because a cursor does not say which sort it belongs to, and the only way
    /// to know is to be told. Passing the sort is what makes the fingerprint
    /// checkable: without it, `f` is a string nobody compares against anything.
    /// See the module docs for the failure this prevents.
    ///
    /// # Untrusted input
    ///
    /// Every path returns `Err`. Nothing defaults, nothing repairs, nothing
    /// "does the best it can" — the same rule `Filter::from_url` follows,
    /// because a malformed cursor that decodes to *something* is a wrong page
    /// rather than an error, and a wrong page is the failure this whole module
    /// is about.
    ///
    /// The checks run in a fixed order, and the order matters: each one
    /// narrows what the next can assume, and reporting a version or
    /// fingerprint mismatch as a type error would send the reader after the
    /// wrong thing.
    pub fn from_url(s: &str, sort: &Sort) -> Result<Self, CursorError> {
        // 1. Transport.
        let json = base64url_decode(s).ok_or(CursorError::NotBase64)?;

        // 2. Shape.
        let wire = Wire::parse(&json)?;

        // 3. Version. Refuse, never assume.
        if wire.v != CURSOR_WIRE_VERSION {
            return Err(CursorError::Version { found: wire.v });
        }

        // 4. The sort. THE point of the module.
        let expected = sort.fingerprint();
        if wire.f != expected {
            return Err(CursorError::SortMismatch {
                found: wire.f,
                expected,
            });
        }

        // 5. Arity. `after_binds` asserts this too, but as a PANIC, and a panic
        //    in a request handler is a 500 for what is a client error. Reaching
        //    it from the wire makes it an error with a message instead.
        let arity = sort.len();
        if wire.k.len() != arity {
            return Err(CursorError::Arity {
                found: wire.k.len(),
                expected: arity,
            });
        }

        // 6. Per-key type, against the sort's own declared key types.
        let mut values = Vec::with_capacity(arity);
        // `wire.k` is Vec<Tagged>, `sort.key_types()` is Vec<(&str, KeyType)>,
        // and `zip` pairs them positionally -- which is safe only because the
        // arity check above already proved the two are the same length. An
        // earlier `zip` here truncated to the shorter, so a short cursor would
        // have had its tail dropped and become a Cursor of the wrong arity.
        for (index, (tagged, (column, want))) in
            wire.k.into_iter().zip(sort.key_types()).enumerate()
        {
            if !tagged.matches(want) {
                return Err(CursorError::KeyType {
                    index,
                    column,
                    got: tagged.type_name(),
                    expected: want.type_name(),
                });
            }
            values.push(tagged.to_value());
        }

        Ok(Cursor::from_validated(values))
    }
}

impl Tagged {
    /// Whether this tag is a variant a column of type `want` could hold.
    ///
    /// `NullableText` accepts a string *and* `N`, because it stands for a
    /// column that may be NULL and NULL is carried by `N` alone. A non-null tag
    /// against a nullable column is fine too — the row simply had a value — so
    /// both directions pass and only a genuine type clash is refused.
    fn matches(&self, want: KeyType) -> bool {
        match want {
            KeyType::NullableText => matches!(self, Tagged::S(_) | Tagged::N),
            KeyType::Integer => matches!(self, Tagged::I(_)),
            KeyType::Real => matches!(self, Tagged::F(_)),
        }
    }
}
