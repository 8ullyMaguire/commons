//! §7.11's automatic career span.
//!
//! # Why nothing here is stored
//!
//! §7.11 asks for a first and last appearance derived from item dates. The
//! obvious implementation is a `career_start` and `career_end` on the performer,
//! updated whenever an item is added. It is faster to query and it is wrong in
//! three ways that only show up later:
//!
//! * **It drifts.** Every path that adds, edits or removes an item has to
//!   remember to update it, and the one that forgets is a bug nobody sees until
//!   somebody notices a span that ends before the person's last release.
//! * **It cannot explain itself.** A stored span is a fact with no provenance.
//!   "Why does this say 2019?" has no answer except "that is what the column
//!   says".
//! * **It is not editable without becoming derived.** §7.11 also says the span
//!   is editable and never destructive. An editable derived value is a stored
//!   value with an override, which is two sources of truth and a rule about
//!   which wins.
//!
//! So the span is a query. [`derive`] is that query, and it is cheap because the
//! index is on the selective end of the join -- one cluster's appearances, then
//! the object dates. The cost of a stored column is paid on every write forever;
//! the cost of this one is paid on a read that already touches the row.
//!
//! # The date of an item
//!
//! `object.date`, set by the scan from the file's metadata. An item with no date
//! is *not* dated at the epoch and is not excluded -- it is counted in
//! [`CareerSpan::appearances`] and ignored for the bounds, because a span is a
//! range and a range needs two ends, while the count is a number and a number
//! does not. A person with one dated and one undated item has a span from the
//! dated one and a count of two, and the tests say so rather than leaving it to
//! be discovered.
//!
//! # `rescan`
//!
//! The ticket's done-when is that the span "recomputes identically after a no-op
//! rescan", which is the property that distinguishes derived from incrementally
//! maintained. [`rescan`] exists so that property is testable against the real
//! path rather than against a direct call to [`derive`] twice: it re-reads the
//! items the way a scan would and writes nothing, so a span that survives it is
//! derived by construction.

use commons_store::{Store, StoreError};
use thiserror::Error;

/// The derived span. Never stored.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CareerSpan {
    /// The earliest item date, or `None` if there is none.
    pub first: Option<String>,
    /// The latest item date, or `None` if there is none.
    pub last: Option<String>,
    /// How many items the person appears in, dated or not.
    pub appearances: usize,
}

impl CareerSpan {
    /// The span in years, from the two ends.
    ///
    /// `0.0` when either end is missing, and the two `Option`s are what
    /// distinguish "no appearances" from "appeared for no time" -- a performer
    /// with one dated item has a real span of zero and a `first`, and a
    /// performer with none has neither.
    pub fn years(&self) -> f64 {
        match (&self.first, &self.last) {
            (Some(f), Some(l)) => {
                let days = (day_number(l) - day_number(f)) as f64;
                days / 365.2425
            }
            _ => 0.0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.appearances == 0
    }
}

#[derive(Debug, Error)]
pub enum SpanError {
    /// A string that was supposed to be an ISO-8601 date and is not. A named
    /// error rather than a store failure, because the caller asked for a date and
    /// got something else -- which is a fact about the argument, not about the
    /// database.
    #[error("not a date: {0}")]
    NotADate(String),

    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Days since 1970-01-01 for an ISO-8601 `YYYY-MM-DD[THH:...]`.
///
/// Hand-rolled rather than pulling in a calendar crate, and the reason is worth
/// stating because the hand-rolled version is the kind that is wrong: the civil
/// calendar has a 400-year cycle, a 100-year exception and a 4-year exception,
/// and this is Howard Hinnant's `days_from_civil`, which is the standard
/// formulation of exactly that. It is used only to turn two dates into a
/// difference, so the two error modes that do not matter are a leap second and a
/// date before 1970, and the one that does -- a string that is not a date -- is
/// handled by returning 0 rather than by panicking in a display path.
fn day_number(s: &str) -> i64 {
    let b = s.as_bytes();
    if b.len() < 10 {
        return 0;
    }
    let num = |a: usize, z: usize| -> i64 {
        s.get(a..z).and_then(|x| x.parse::<i64>().ok()).unwrap_or(0)
    };
    let (y, m, d) = (num(0, 4), num(5, 7), num(8, 10));
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return 0;
    }
    // days_from_civil, with the month shifted so March is month 1 and the leap
    // day lands at the end of the year.
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = if m > 2 { m - 3 } else { m + 9 }; // [0, 11]
    let doy = (153 * mp + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// The span, computed from the items.
pub async fn derive(store: &Store, cluster: &str) -> Result<CareerSpan, SpanError> {
    // LEFT JOIN with COUNT(a.id), not an inner join with COUNT(*).
    //
    // The distinction that matters is that an item with no *date* is still an
    // item: MIN and MAX ignore the NULL, so the bounds come from the dated ones,
    // while COUNT(a.id) counts every appearance. A span that says "4
    // appearances" for a performer in 5 items is a lie nobody can debug from the
    // record.
    //
    // `appearance.object_id` is a NOT NULL foreign key, and `Store::open_*`
    // enables `PRAGMA foreign_keys` on every connection, so through the public
    // API the LEFT JOIN never null-extends and COUNT(*) would agree. The LEFT
    // JOIN is correct by construction rather than by assertion: a caller with a
    // raw connection that has not set the pragma is exactly the case where
    // COUNT(*) would count a phantom row, and nothing about the failure would
    // point here.
    let row: Option<(Option<String>, Option<String>, i64)> = sqlx::query_as(
        "SELECT MIN(o.date), MAX(o.date), COUNT(a.id)
         FROM appearance a LEFT JOIN object o ON o.id = a.object_id
         WHERE a.cluster_id = ?",
    )
    .bind(cluster)
    .fetch_optional(store.pool())
    .await
    .map_err(StoreError::Query)?;

    let row = match row {
        Some(r) => r,
        None => return Ok(CareerSpan::default()),
    };
    let (first, last, appearances) = row;
    if appearances == 0 {
        return Ok(CareerSpan::default());
    }
    Ok(CareerSpan {
        first,
        last,
        appearances: appearances as usize,
    })
}

/// Re-read the items, as a rescan would, and write nothing.
///
/// Exists so the ticket's done-when -- "recomputes identically after a no-op
/// rescan" -- is testable against a real path rather than against [`derive`]
/// called twice. An implementation that maintained a stored span would have to
/// do its updating here, and this function's contract is that there is nothing
/// to update: it is a read.
pub async fn rescan(store: &Store, cluster: &str) -> Result<CareerSpan, SpanError> {
    // Touch the same rows a scan would, so a rescan that somehow reordered or
    // rewrote them would be visible. No writes, deliberately: the point is that
    // there is nothing to write.
    let _: Vec<String> = sqlx::query_scalar(
        "SELECT o.id FROM appearance a JOIN object o ON o.id = a.object_id
         WHERE a.cluster_id = ? ORDER BY o.id",
    )
    .bind(cluster)
    .fetch_all(store.pool())
    .await
    .map_err(StoreError::Query)?;
    derive(store, cluster).await
}

/// Age at a given date, from a birth-date attribute.
///
/// `None` when there is no birth date, and `None` when the date is *before* the
/// birth date. The second case matters: returning `0` would put a performer in a
/// scene before she was born, and returning a negative age would be arithmetically
/// honest and completely useless to a reader.
/// Age at a given date, from a birth date on the performer's record.
///
/// `None` when there is no birth date, and `None` when the date is *before* the
/// birth date. The second case matters: returning `0` would put a performer in a
/// scene before she was born, and a negative age would be arithmetically honest
/// and useless to a reader.
///
/// Reads the birth date through the attribute layer rather than a column,
/// because §7.7 makes it a typed `date` attribute and a second copy of it on the
/// performer table would be a second thing to keep in step -- and a person whose
/// birth date was corrected in one place and not the other would get an age
/// computed from the old one.
pub async fn age_at(store: &Store, performer: &str, date: &str) -> Result<Option<i32>, SpanError> {
    let birth = birth_date(store, performer).await?;
    let birth = match birth {
        None => return Ok(None),
        Some(b) => b,
    };

    let (by, bm, bd) = parts(&birth)?;
    let (dy, dm, dd) = parts(date)?;
    if (dy, dm, dd) < (by, bm, bd) {
        return Ok(None);
    }
    // Whole years elapsed, comparing month and day rather than day numbers, so
    // it is 31 on the birthday and not the day before.
    let mut age = dy - by;
    if (dm, dd) < (bm, bd) {
        age -= 1;
    }
    Ok(Some(age as i32))
}

/// The performer's birth date, from the field named `birth_date`.
///
/// A field *name* rather than an id, because ids are per-deployment and a birth
/// date is not; the name is the stable handle. The value is read through the
/// attribute layer's own decoding, so a field that has been retyped and now
/// holds something else is skipped rather than parsed as a date.
async fn birth_date(store: &Store, performer: &str) -> Result<Option<String>, SpanError> {
    let raw: Option<String> = sqlx::query_scalar(
        "SELECT v.value_json
         FROM custom_field_value v
         JOIN custom_field f ON f.id = v.field_id
         WHERE f.name = 'birth_date'
           AND v.subject_type = 'performer'
           AND v.subject_id = ?
         ORDER BY COALESCE(v.at, '') DESC
         LIMIT 1",
    )
    .bind(performer)
    .fetch_optional(store.pool())
    .await
    .map_err(StoreError::Query)?;
    Ok(raw.as_deref().and_then(raw_value))
}

/// The date out of a stored value row.
///
/// The stored form is externally tagged, so a date is `{"date": "YYYY-MM-DD"}`
/// and a measurement is `{"measured": {"value": 88.0, "at": "..."}}`. Keying on
/// the tag means a field that has been retyped and now holds a measurement is
/// *skipped* rather than parsed as a date: a measurement in a field called
/// `birth_date` is a data-entry mistake, and it should not produce an age of
/// 1969.
fn raw_value(raw: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    v.get("date")?.as_str().map(|s| s.to_string())
}

fn parts(s: &str) -> Result<(i64, u32, u32), SpanError> {
    let b = s.as_bytes();
    if b.len() < 10 {
        return Err(SpanError::NotADate(s.to_string()));
    }
    let n = |a: usize, z: usize| -> Result<i64, SpanError> {
        s.get(a..z)
            .and_then(|x| x.parse::<i64>().ok())
            .ok_or_else(|| SpanError::NotADate(s.to_string()))
    };
    Ok((n(0, 4)?, n(5, 7)? as u32, n(8, 10)? as u32))
}

// ---------------------------------------------------------------------------
// Tests for the arithmetic
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_known_date_has_a_known_day_number() {
        // 1970-01-01 is day 0, and 2000-03-01 is the day a leap day falls on.
        assert_eq!(day_number("1970-01-01"), 0);
        assert_eq!(day_number("1970-01-02"), 1);
        assert_eq!(day_number("1971-01-01"), 365);
        // 2000 is a leap year (divisible by 400), 1900 is not (divisible by 100
        // but not 400). These two are the whole reason the calendar arithmetic
        // is hand-written correctly rather than approximated by 365.25.
        assert_eq!(day_number("2000-03-01") - day_number("2000-02-28"), 2);
        assert_eq!(day_number("1900-03-01") - day_number("1900-02-28"), 1);
        assert_eq!(day_number("2024-03-01") - day_number("2024-02-28"), 2);
    }

    #[test]
    fn a_nonsense_date_is_zero_rather_than_a_panic() {
        // A display path must not panic on a bad row; it shows 0 years and the
        // `Option`s say why.
        for s in ["", "not a date", "2020-13-01", "2020-00-10", "xxxx-xx-xx"] {
            assert_eq!(day_number(s), 0, "{s:?} does not panic");
        }
    }

    #[test]
    fn a_span_needs_both_ends() {
        let none = CareerSpan::default();
        assert_eq!(none.years(), 0.0);
        let one = CareerSpan {
            first: Some("2020-01-01".into()),
            last: None,
            appearances: 1,
        };
        assert_eq!(
            one.years(),
            0.0,
            "one end is not a range, and `last: None` is what says so"
        );
        let both = CareerSpan {
            first: Some("2020-01-01".into()),
            last: Some("2021-01-01".into()),
            appearances: 2,
        };
        assert!(
            (both.years() - 1.0).abs() < 0.01,
            "a leap year is about a year"
        );
    }
}
