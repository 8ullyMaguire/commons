//! Sort keys, and the keyset cursor that goes with them.
//!
//! T-P5-006 item 8. Spec §10.4.
//!
//! # A sort here is a tuple, and the tuple always ends in `id`
//!
//! The trailing `id` is not a sort key anybody chose. It is what makes the
//! order *total*, and a total order is the precondition for a cursor: without
//! it, two rows that tie on every key the user named have no defined relative
//! order, so a page boundary between them is a guess — the same two rows can
//! appear on both pages, or on neither. The symptom is a list that repeats and
//! skips, and it only shows up on a sort with ties *that span a boundary*, which
//! is why a test that sorts 10 distinct rows proves nothing.
//!
//! So `Sort` refuses an empty key list rather than defaulting: a sort with no
//! keys has no trailing `id` either, because the `id` is appended to whatever
//! the user named, and nothing is nothing.
//!
//! # Why the cursor predicate is spelled out longhand
//!
//! The obvious spelling of "after this tuple" is a row-value comparison —
//! `WHERE (k1, k2, id) > (?, ?, ?)`. Postgres has it. **SQLite does not have
//! row values at all**; `(a, b) > (?, ?)` there compares two blobs, which is
//! neither lexicographic nor correct and fails silently by returning the wrong
//! rows rather than erroring. The explicit three-way form is the only spelling
//! both engines agree on, so that is what [`Sort::after_sql`] emits.
//!
//! The second half of the trap is that `after_sql` and `order_by` are two
//! fragments of SQL that must describe *the same order*, and nothing in the
//! language ties them together. A predicate that forgets the last key, or spells
//! a direction the ORDER BY does not have, produces a query that runs, returns
//! rows, and pages wrongly. `Sort` builds both from one key list for that
//! reason, and the page-boundary test exists because the agreement is not
//! checkable by reading either fragment alone.

use crate::filter_ast::Value;
use std::fmt;

/// What a sort key's column holds.
///
/// Only for checking a decoded cursor. It is not a SQL type system and does not
/// need to be one: a sort names five columns, three of which are text and one
/// of which is an integer, and the point is to refuse a cursor whose value
/// cannot be that column's — not to model SQLite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyType {
    /// A TEXT column, or one that may be NULL.
    NullableText,
    Integer,
    /// A REAL column. **No `SortKey` produces one today.** It is here so that
    /// adding a float-valued sort key is not also a change to this module's
    /// public shape, and so `KeyType` and `Tagged` have the same variants.
    Real,
}

impl KeyType {
    /// In the words a refusal message uses, not a type name: the message is
    /// read by a person, and "expected an integer" beats "expected Integer".
    pub(crate) fn type_name(self) -> &'static str {
        match self {
            KeyType::NullableText => "a string or null",
            KeyType::Integer => "an integer",
            KeyType::Real => "a float",
        }
    }
}

/// Which way one key sorts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortOrder {
    Asc,
    Desc,
}

impl SortOrder {
    pub fn as_sql(self) -> &'static str {
        match self {
            SortOrder::Asc => "ASC",
            SortOrder::Desc => "DESC",
        }
    }

    /// The predicate meaning "strictly past the cursor on this key".
    fn after_sql(self) -> &'static str {
        match self {
            SortOrder::Asc => ">",
            SortOrder::Desc => "<",
        }
    }
}

impl fmt::Display for SortOrder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_sql())
    }
}

/// A column a user may sort on.
///
/// An enum rather than a `String` because the column name reaches SQL. A sort
/// taken as a string would need quoting, allow-listing, or both, and the
/// allow-list is the part that gets forgotten: one interpolation of a
/// caller-supplied sort name into an `ORDER BY` is an injection, and §5.16's
/// shareable URLs make the sort name attacker-controlled by construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Date,
    RatingSum,
    Title,
    Kind,
    AddedAt,
}

impl SortKey {
    /// The column, qualified with the table alias the query uses.
    fn column(self) -> &'static str {
        match self {
            SortKey::Date => "o.date",
            // There is no `rating` column. A rating is `rating_sum /
            // rating_count`, and the mean cannot be sorted on a nullable integer
            // column without a division in the ORDER BY -- which is legal SQL
            // but defeats the index and, worse, makes the cursor predicate
            // have to repeat the same expression or it compares something
            // different from what was ordered.
            //
            // So the sortable integer is `rating_sum`, and the honest answer is
            // that this key sorts by total stars, not by average. That is named
            // in the variant so no caller is misled: `RatingSum` is the truth.
            // An average-rating sort belongs in its own ticket, with an
            // expression index and a cursor that compares the same expression.
            SortKey::RatingSum => "o.rating_sum",
            SortKey::Title => "o.title",
            SortKey::Kind => "o.kind",
            SortKey::AddedAt => "o.created_at",
        }
    }
}

impl SortKey {
    /// What this column holds, for checking a decoded cursor against it.
    ///
    /// Beside [`Self::column`] rather than in a `match` inside `key_types`, so
    /// a new `SortKey` cannot get a column without also getting a type — the
    /// two are the same piece of knowledge about a column and splitting them is
    /// how one of them goes stale.
    fn key_type(self) -> KeyType {
        match self {
            // The one sortable key that is not text. `rating_sum` is an
            // INTEGER, so a cursor carrying a string in this slot is a cursor
            // that will bind text to an integer column.
            SortKey::RatingSum => KeyType::Integer,
            // `o.date` and `o.title` are nullable; `o.kind` and `o.created_at`
            // are not. `NullableText` covers both because a non-null column
            // simply never carries the `N` tag, and refusing `N` for a column
            // that cannot be NULL would be a check with no failure case.
            _ => KeyType::NullableText,
        }
    }
}

impl fmt::Display for SortKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.column())
    }
}

/// A sort: one or more keys, in order, always terminated by `id`.
///
/// Construct with [`Sort::new`] and never by hand — the trailing `id` is the
/// invariant, and it is maintained in one place so it cannot be forgotten at a
/// call site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sort {
    keys: Vec<(SortKey, SortOrder)>,
}

impl Sort {
    /// A sort over the given keys, with `id` appended as the final tiebreak.
    ///
    /// # Panics
    ///
    /// On an empty key list. There is no meaningful "sort by nothing": the
    /// point of the type is that the order is total, and an empty list is the
    /// one input that cannot produce one. A caller with a user-supplied list
    /// should check it before calling rather than catch a panic from here.
    pub fn new(keys: Vec<(SortKey, SortOrder)>) -> Self {
        assert!(
            !keys.is_empty(),
            "a sort needs at least one key: with none there is no order to be total"
        );
        Sort { keys }
    }

    /// Newest first — the default, matching every view's own default.
    pub fn date_desc() -> Self {
        Sort::new(vec![(SortKey::Date, SortOrder::Desc)])
    }

    /// The user-named keys, without the trailing `id`.
    pub fn keys(&self) -> &[(SortKey, SortOrder)] {
        &self.keys
    }

    /// The whole key list including the `id` tiebreak.
    ///
    /// `id` sorts ASC unconditionally. A user-chosen direction on the tiebreak
    /// would still be a total order, but it would make page 2 depend on a
    /// setting nobody thinks about, and the constant is one fewer thing to
    /// explain.
    fn all_keys(&self) -> Vec<(String, SortOrder)> {
        let mut v: Vec<(String, SortOrder)> = self
            .keys
            .iter()
            .map(|(k, o)| (k.column().to_string(), *o))
            .collect();
        v.push(("o.id".to_string(), SortOrder::Asc));
        v
    }

    /// The `ORDER BY` clause, no `ORDER BY` keyword.
    pub fn order_by(&self) -> String {
        self.all_keys()
            .iter()
            .map(|(c, o)| format!("{c} {}", o.as_sql()))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// The keyset predicate for "strictly after this cursor", with no `WHERE`
    /// keyword, or `None` when there is no cursor.
    ///
    /// The spelling is longhand on purpose. `WHERE (k1, k2, id) > (?, ?, ?)` is
    /// the one line that says what this does, and it is Postgres-only: SQLite
    /// has no row values, so the same text compares two blobs and returns the
    /// wrong rows without erroring. The three-way form below is the same
    /// predicate written out, and it is what both engines agree on.
    ///
    /// Read it against `order_by`: every `kN OR` opens a run of equality that
    /// the next key then narrows, and the final clause is a strict comparison
    /// on the last key. That is lexicographic order over the tuple, which is
    /// what `order_by` has to be describing for the two to agree.
    pub fn after_sql(&self) -> Option<String> {
        let keys = self.all_keys();
        let mut parts: Vec<String> = Vec::with_capacity(keys.len());
        for i in 0..keys.len() {
            let mut run = String::new();
            for (col, _) in &keys[..i] {
                run.push_str(&format!("{col} IS NOT DISTINCT FROM ? AND "));
            }
            parts.push(format!("({run}{} {} ?)", keys[i].0, keys[i].1.after_sql()));
        }
        Some(parts.join(" OR "))
    }

    /// How many bind values [`Self::after_sql`] expects.
    ///
    /// Not decoration: the predicate above mentions each key once per run, so
    /// the arity is `n(n+1)/2` and NOT `n`. Binding `n` values to a predicate
    /// that wants `n(n+1)/2` is an error, but binding them in the wrong ORDER
    /// is a query that runs and pages wrongly — so this count is derived from
    /// the same loop that builds the string rather than written beside it.
    pub fn after_bind_count(&self) -> usize {
        let n = self.all_keys().len();
        n * (n + 1) / 2
    }

    /// A short, stable identifier for *this sort*, as its `ORDER BY` text.
    ///
    /// This is what binds a cursor to the sort it was made for, and it is
    /// derived from `order_by()` rather than being a hand-written label per
    /// sort for one reason: `order_by()` is already the canonical description
    /// of the sort, and it changes automatically when a `SortKey` is added or a
    /// direction is flipped. A hand-written label is a second thing to forget,
    /// and forgetting it is *silent* — which is the one failure mode the
    /// cursor's whole design exists to prevent. See `cursor_wire.rs`.
    ///
    /// FNV-1a rather than `DefaultHasher`, for the reason `stable_usn` already
    /// records: `DefaultHasher`'s output is unspecified across releases, and
    /// this value ends up persisted in shareable URLs, where a toolchain bump
    /// that changed it would invalidate every cursor anyone had saved.
    ///
    /// **Eight hex characters is a fingerprint, not a security boundary.** 32
    /// bits takes a deliberate search to collide, and what this guards is
    /// *correctness* — a cursor used with the wrong sort — not authority.
    /// Written down because the next reader will otherwise assume it is
    /// load-bearing for security, and either strengthen it needlessly or
    /// weaken it deliberately.
    pub fn fingerprint(&self) -> String {
        /// FNV-1a, 64-bit. Written out rather than pulled in: it is four lines
        /// and a crate that every binary links is a bad place for a dependency
        /// that saves them.
        const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
        const PRIME: u64 = 0x0000_0100_0000_01b3;

        let mut h = OFFSET;
        for b in self.order_by().as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(PRIME);
        }
        // The LOW 32 bits, explicitly. `{h:08x}` is a *minimum* width, not a
        // truncation, so it printed all 16 digits of a 64-bit value -- which is
        // not wrong, just twice the length the spec and the doc comment here
        // both promise, and a URL parameter that is twice as long as it needs
        // to be for no benefit. `as u32` is the truncation, made visible.
        format!("{:08x}", h as u32)
    }

    /// How many keys a cursor for this sort must carry: the named keys plus
    /// the `id` tiebreak.
    ///
    /// A separate accessor because the *cursor* needs the arity to check a
    /// decoded value against, and `all_keys()` is private precisely because
    /// callers were never meant to reproduce the tiebreak themselves.
    ///
    /// **No `is_empty`, and clippy is right to ask.** A `Sort` with no keys
    /// would order by nothing, so it cannot be constructed: the value is
    /// checked non-empty at construction. An `is_empty` that can only ever
    /// return `false` is a lie with a doc comment, and the honest fix is to
    /// say here why there is no such method.
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        self.all_keys().len()
    }

    /// What each of this sort's keys holds, in `all_keys()` order, paired with
    /// the column it came from.
    ///
    /// `(column, kind)` per key, so a decoded cursor can be checked against the
    /// sort it claims: a `date_desc` cursor is `[Text, Text]` and a
    /// `rating_sum` cursor is `[Integer, Text]`, and a cursor carrying the
    /// wrong tag in slot 0 is refused rather than bound to a column of the
    /// other type.
    ///
    /// **The `id` tiebreak is always `Text`.** It is a TEXT primary key, so a
    /// cursor whose last value is not a string is wrong regardless of the sort
    /// — and the check is worth having precisely because that is the slot no
    /// sort-specific reasoning reaches.
    pub fn key_types(&self) -> Vec<(&'static str, KeyType)> {
        let mut out: Vec<(&'static str, KeyType)> = self
            .keys
            .iter()
            .map(|(k, _)| (k.column(), k.key_type()))
            .collect();
        // The tiebreak, always TEXT: it is the `id` primary key.
        out.push(("o.id", KeyType::NullableText));
        out
    }

    /// The cursor's values, repeated into the order `after_sql` binds them.
    ///
    /// The repetition is the point. `after_sql` mentions key 0 once, key 1
    /// twice and key 2 three times, so the bind list is the cursor values
    /// *concatenated* in key order with each prefix repeated -- which is why
    /// this is written as a loop over keys rather than a map.
    pub fn after_binds(&self, cursor: &Cursor) -> Vec<Value> {
        let keys = self.all_keys();
        assert_eq!(
            cursor.len(),
            keys.len(),
            "a cursor from a different sort cannot seek in this one"
        );
        let mut out: Vec<Value> = Vec::with_capacity(self.after_bind_count());
        for i in 0..keys.len() {
            for j in 0..=i {
                out.push(cursor.values[j].clone());
            }
        }
        out
    }
}

/// A position in a sorted result set: the value of every key, including the
/// trailing `id`.
///
/// Opaque on the outside by construction — there is no way to build one except
/// from a page, so a cursor cannot be hand-assembled with the wrong arity, which
/// is the failure that turns into a silently wrong page rather than an error.
#[derive(Debug, Clone, PartialEq)]
pub struct Cursor {
    values: Vec<Value>,
}

impl Cursor {
    /// Build from a position. `values` is in [`Sort::all_keys`] order.
    ///
    /// `#[cfg(test)]` because the only honest way to make one is from a real
    /// row, and production gets there through [`Self::from_row`]. A public
    /// constructor would let a caller assemble a cursor of the wrong arity and
    /// bind it to a query that wants a different one -- an error, but one that
    /// reads as a sqlx problem rather than as the call site that mixed two
    /// sorts.
    #[cfg(test)]
    pub(crate) fn new(values: Vec<Value>) -> Self {
        Cursor { values }
    }

    /// A cursor positioned at a specific row's key values, in the order
    /// `sort` reads them.
    ///
    /// The argument list is every column the query selects, not this sort's
    /// subset, so the mapping is a function of the *column* and not of the sort
    /// -- which is what makes a cursor assembled for a date-descending sort
    /// still mean "this row" when handed to a rating-descending one.
    pub(crate) fn from_row(
        sort: &Sort,
        id: &str,
        date: &Option<String>,
        rating_sum: &i64,
        title: &Option<String>,
        kind: &str,
        added_at: &str,
    ) -> Self {
        let value_for = |col: &str| -> Value {
            match col {
                "o.date" => date.clone().map(Value::Str).unwrap_or(Value::Null),
                "o.rating_sum" => Value::Int(*rating_sum),
                "o.title" => title.clone().map(Value::Str).unwrap_or(Value::Null),
                "o.kind" => Value::Str(kind.to_string()),
                "o.created_at" => Value::Str(added_at.to_string()),
                "o.id" => Value::Str(id.to_string()),
                other => unreachable!("a sort key has no column mapping: {other}"),
            }
        };
        let values = sort.all_keys().iter().map(|(c, _)| value_for(c)).collect();
        Cursor { values }
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// The cursor's values, in `all_keys()` order for the sort it was made for.
    ///
    /// `pub(crate)` and not public: a caller outside the store needs to *seek*
    /// with a cursor, not to read one out and build another, and a public
    /// accessor is a step on the way to a public constructor.
    pub(crate) fn values(&self) -> &[Value] {
        &self.values
    }

    /// A cursor from values that have already been checked against a sort.
    ///
    /// `pub(crate)` for the same reason [`Self::values`] is, and this is the
    /// reason it exists at all: `cursor_wire.rs` decodes a wire value into a
    /// `Vec<Value>` and needs to put it in a `Cursor`, and making the `values`
    /// field `pub` would open it to every module in the crate. One named
    /// constructor says "these were validated" at the call site, which a field
    /// assignment does not.
    ///
    /// The validation is real and is the caller's job: every path here comes
    /// from [`crate::cursor_wire::Cursor::from_url`], which has already checked
    /// the arity, the sort fingerprint and every key's type. Nothing in the
    /// store can reach this with an unvalidated vector.
    pub(crate) fn from_validated(values: Vec<Value>) -> Self {
        debug_assert!(
            !values.is_empty(),
            "a cursor with no values has no position; the wire decoder refuses it"
        );
        Cursor { values }
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cursor_wire::CursorError;

    #[test]
    fn the_order_always_ends_in_id() {
        let s = Sort::new(vec![
            (SortKey::Date, SortOrder::Desc),
            (SortKey::RatingSum, SortOrder::Desc),
        ]);
        assert_eq!(s.order_by(), "o.date DESC, o.rating_sum DESC, o.id ASC");
    }

    #[test]
    fn a_single_key_still_gets_the_tiebreak() {
        assert_eq!(Sort::date_desc().order_by(), "o.date DESC, o.id ASC");
    }

    #[test]
    fn the_tiebreak_direction_is_not_the_caller_s_to_choose() {
        // A user-chosen id direction is still a total order, so this is not a
        // correctness fix -- it is a "nothing surprising" rule, and the comment
        // says so rather than pretending otherwise.
        let s = Sort::new(vec![(SortKey::Date, SortOrder::Desc)]);
        assert!(s.order_by().ends_with("o.id ASC"));
    }

    #[test]
    #[should_panic(expected = "at least one key")]
    fn a_sort_with_no_keys_is_rejected() {
        Sort::new(vec![]);
    }

    #[test]
    fn the_predicate_is_lexicographic_over_the_whole_tuple() {
        // (a, b, id) > (?, ?, ?) is
        //   a < ?a
        //   OR (a = ?a AND b < ?b)
        //   OR (a = ?a AND b = ?b AND id < ?id)
        let s = Sort::new(vec![
            (SortKey::Date, SortOrder::Desc),
            (SortKey::RatingSum, SortOrder::Desc),
        ]);
        let after = s.after_sql().expect("a sort always has a predicate shape");
        assert_eq!(
            after,
            "(o.date < ?) \
             OR (o.date IS NOT DISTINCT FROM ? AND o.rating_sum < ?) \
             OR (o.date IS NOT DISTINCT FROM ? \
                 AND o.rating_sum IS NOT DISTINCT FROM ? AND o.id > ?)"
        );
    }

    #[test]
    fn the_predicate_mentions_each_key_once_per_run_so_the_arity_is_triangular() {
        // The count that is easy to get wrong by hand. THREE named keys, plus
        // the trailing `id`, is FOUR keys -- and four keys bind 1 + 2 + 3 + 4 =
        // 10 values, not 6. I wrote 6 first and the test caught it: the `id` is
        // a key like any other and I had been counting only the ones a user
        // named. Getting this wrong by a small amount is a bind-count error;
        // getting it wrong by exactly the tiebreak is a query that runs and
        // pages wrongly.
        let s = Sort::new(vec![
            (SortKey::Date, SortOrder::Desc),
            (SortKey::RatingSum, SortOrder::Desc),
            (SortKey::Title, SortOrder::Asc),
        ]);
        assert_eq!(
            s.order_by().matches(',').count() + 1,
            4,
            "4 keys with the id"
        );
        assert_eq!(s.after_bind_count(), 10, "4 keys -> 10 binds");
        // And the honest error is one the caller can see: three named keys
        // alone would be 6, and the difference between the two numbers IS the
        // tiebreak.
        let two = Sort::new(vec![
            (SortKey::Date, SortOrder::Desc),
            (SortKey::RatingSum, SortOrder::Desc),
        ]);
        assert_eq!(two.after_bind_count(), 6, "2 keys + id -> 6");
    }

    #[test]
    fn the_binds_repeat_each_prefix_in_the_order_the_predicate_reads_them() {
        let s = Sort::new(vec![
            (SortKey::Date, SortOrder::Desc),
            (SortKey::RatingSum, SortOrder::Desc),
        ]);
        // A cursor holding date=D, rating=R, id=I. The predicate's six bind
        // slots are: D; D,R; D,R,I.
        let cursor = Cursor::new(vec![
            Value::Str("2026-01-01".to_string()),
            Value::Int(3),
            Value::Str("o-7".to_string()),
        ]);
        let binds = s.after_binds(&cursor);
        assert_eq!(binds.len(), 6, "the arity the predicate needs");
        assert_eq!(binds[0], Value::Str("2026-01-01".to_string()));
        assert_eq!(binds[1], Value::Str("2026-01-01".to_string()));
        assert_eq!(binds[2], Value::Int(3));
        assert_eq!(binds[3], Value::Str("2026-01-01".to_string()));
        assert_eq!(binds[4], Value::Int(3));
        assert_eq!(binds[5], Value::Str("o-7".to_string()));
    }

    #[test]
    #[should_panic(expected = "different sort")]
    fn a_cursor_from_another_sort_cannot_seek_in_this_one() {
        // Two keys here, and a cursor carrying three. A wrong arity that is not
        // caught here is a bind list that is short by one, which is a query
        // error rather than a wrong page -- but the error message would point at
        // sqlx, not at the call site that mixed two sorts.
        let s = Sort::date_desc();
        let wrong = Cursor::new(vec![
            Value::Str("2026-01-01".to_string()),
            Value::Int(3),
            Value::Str("o-7".to_string()),
        ]);
        s.after_binds(&wrong);
    }

    #[test]
    fn a_cursor_from_another_text_sort_is_refused_rather_than_seeking() {
        // THE test for this module. Both sorts are [Str, Str] and arity 2.
        //
        // Step 0 of the plan measured why that matters: `Date`, `Title`, `Kind`
        // and `AddedAt` are ALL `Value::Str`, and `after_binds` checks only
        // LENGTH. So a date cursor seeked in a title sort binds cleanly, runs,
        // and returns rows -- `o.title < '2026-01-01'` is a perfectly valid text
        // comparison. The page is wrong and nothing errors.
        //
        // A test using a text/int pair (Date vs RatingSum) would PASS WITH NO
        // FINGERPRINT AT ALL, because that mismatch yields an empty page rather
        // than a wrong one. This one cannot.
        let made_for = Sort::date_desc();
        let asked_for = Sort::new(vec![(SortKey::Title, SortOrder::Asc)]);

        let wire = Cursor::new(vec![
            Value::Str("2026-01-01".to_string()),
            Value::Str("o-7".to_string()),
        ])
        .to_url(&made_for);

        let err = Cursor::from_url(&wire, &asked_for)
            .expect_err("a date cursor seeked in a title sort must be refused, not accepted");
        assert!(
            matches!(err, CursorError::SortMismatch { .. }),
            "expected a SortMismatch, got {err:?}",
        );
        // And the message names both, because a caller showing it to a user
        // cannot otherwise say what went wrong.
        assert!(err.to_string().contains("different sort"), "{err}");
        assert!(
            err.to_string().contains(&made_for.fingerprint()),
            "the message must name the cursor's own fingerprint: {err}"
        );
    }

    #[test]
    fn the_fingerprint_changes_when_a_direction_flips() {
        // The silent one: same keys, same arity, same value types. Only
        // `order_by()` differs, which is exactly why the fingerprint is derived
        // from it rather than from the key list.
        let desc = Sort::new(vec![(SortKey::Date, SortOrder::Desc)]);
        let asc = Sort::new(vec![(SortKey::Date, SortOrder::Asc)]);
        assert_ne!(desc.fingerprint(), asc.fingerprint());
    }

    #[test]
    fn the_fingerprint_changes_when_a_key_is_added() {
        let one = Sort::new(vec![(SortKey::Date, SortOrder::Desc)]);
        let two = Sort::new(vec![
            (SortKey::Date, SortOrder::Desc),
            (SortKey::RatingSum, SortOrder::Desc),
        ]);
        assert_ne!(one.fingerprint(), two.fingerprint());
        // Arity alone would catch THIS pair, so the assertion above is not
        // load-bearing on its own -- the direction test is. Both are here so a
        // future change that made the fingerprint arity-only would be caught by
        // the first and not noticed by the second.
        assert_eq!(one.len(), 2);
        assert_eq!(two.len(), 3);
    }

    #[test]
    fn the_fingerprint_is_stable_and_pinned_to_a_literal() {
        // Stability across calls, and then a HARD-CODED value.
        //
        // The literal is the cost of deriving the fingerprint from `order_by()`:
        // changing that text invalidates every cursor in every saved URL. That
        // is a real cost, and the mitigation is not avoiding it -- it is making
        // it VISIBLE here, so the change that causes it fails a test instead of
        // silently invalidating saved links.
        let s = Sort::date_desc();
        assert_eq!(s.fingerprint(), s.fingerprint());
        assert_eq!(
            s.fingerprint(),
            "82c4706c",
            "the fingerprint is derived from order_by(), so changing that text \
             invalidates every saved cursor. Update this literal deliberately."
        );
    }

    #[test]
    fn a_cursor_round_trips_through_its_wire_form() {
        let sort = Sort::new(vec![
            (SortKey::Date, SortOrder::Desc),
            (SortKey::RatingSum, SortOrder::Desc),
            (SortKey::Title, SortOrder::Asc),
        ]);
        let original = Cursor::new(vec![
            Value::Str("2026-01-01".to_string()),
            Value::Int(3),
            Value::Str("A Title".to_string()),
            Value::Str("o-7".to_string()),
        ]);
        let wire = original.to_url(&sort);
        let back = Cursor::from_url(&wire, &sort).expect("a cursor we just wrote decodes");
        assert_eq!(back, original);
        // And it still SEEKS: the decoded cursor binds the same values the
        // original would have. A round trip that decodes but binds differently
        // is the failure this whole module is about, so the bind list is
        // compared and not just the struct.
        assert_eq!(sort.after_binds(&back), sort.after_binds(&original));
    }

    #[test]
    fn a_null_date_survives_the_wire_rather_than_becoming_an_empty_string() {
        // `o.date` is nullable, and `Value::Null` and `Value::Str("")` are the
        // same thing to a naive JSON encoding. A NULL that decodes as "" seeks
        // to the wrong place: "" IS NOT DISTINCT FROM a real date is false, so
        // the first run of the predicate never matches and the page boundary
        // moves. This is why `N` is its own tag.
        let sort = Sort::date_desc();
        let original = Cursor::new(vec![Value::Null, Value::Str("o-7".to_string())]);
        let back = Cursor::from_url(&original.to_url(&sort), &sort).expect("decodes");
        assert_eq!(back.values()[0], Value::Null, "NULL stayed NULL");
        assert_ne!(back.values()[0], Value::Str(String::new()));
    }

    #[test]
    fn a_desc_key_predicates_backwards_and_the_tiebreak_always_forward() {
        // Under DESC, "after" means smaller -- except for the id, which is
        // ASC, so its clause is `>`. Getting this backwards on the tiebreak is
        // the bug that repeats the first row of every page forever.
        let after = Sort::date_desc().after_sql().unwrap();
        assert!(after.contains("o.date < ?"), "DESC seeks backwards");
        assert!(
            after.contains("o.id > ?"),
            "the ASC tiebreak seeks forwards"
        );
    }
}
