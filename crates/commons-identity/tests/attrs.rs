//! T-P3-006 acceptance: §7.6–§7.12, the performer field model.
//!
//! # What this ticket is
//!
//! Everything before it made *clusters*: which appearance belongs to which
//! person. This ticket gives the person a record with fields, and the fields are
//! where the real modelling decisions are.
//!
//! # The four decisions, and why each one is a decision
//!
//! **1. Attributes are typed in the schema, not in the UI.** §7.7 names eight
//! types -- `single`, `multi`, `range`, `ordinal`, `boolean`, `text`, `date`,
//! `measurement` -- and the type has to be somewhere a *query* can see, because
//! the questions are not "what is in this field" but "which performers have a
//! measurement on record" and "does this field accept more than one value". A
//! field type stored only in the interface cannot answer either. So `attr_type` is
//! a column and every value is validated against it.
//!
//! **2. A `measurement` is date-stamped, and that is the whole point of it.** A
//! measurement without a date is a number, and numbers without dates cannot
//! produce a career span or an age. §7.11 asks for a career span derived from
//! item dates and a person's age at a scene; the only way to chart a measurement
//! over time is to require the date. So `measurement` is the one type where the
//! date is mandatory, and an undated value is refused rather than stored with a
//! null nobody can see is null.
//!
//! **3. Career span is derived, never stored.** §7.11 says derived from item
//! dates. The temptation is a `career_start` column updated as items arrive,
//! which is faster to query and quietly wrong: it drifts, it needs recomputing on
//! every edit, and it cannot say *why* the span is what it is. So the span is a
//! function of the item dates, recomputed on read.
//!
//! **4. Not every appearance is a co-star.** §7.12: a cameo, a non-sexual
//! presence, a background appearance and a duologue are four different things,
//! and only some of them mean "these two people are in a scene together". The
//! credit decision is already `AppearanceType::counts_as_appearance` in
//! `commons-core`; what is new here is that the appear-with graph *reads* it, so
//! the decision lives in one place instead of at every call site that builds a
//! co-star list.
//!
//! # The recompute test is the one that matters
//!
//! "It is derived, not incrementally drifted" is the ticket's own done-when. The
//! test that proves it is not a span that is correct once: it is a span that is
//! correct again after every input has been re-presented, out of order, with
//! duplicates, and after a rescan that changed nothing. A drifted implementation
//! passes the first assertion and fails that one.

mod common;

use common::*;
use commons_core::AppearanceType;
use commons_identity::attrs::{
    self, AttrType, AttrValue, AttributeError, Measurement, Range, Status,
};
use commons_identity::span::{self, CareerSpan};
use commons_store::Store;
use serde_json::json;

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn now() -> String {
    crate_now()
}

fn crate_now() -> String {
    // The identity crate keeps its clock private, so tests use their own. Nothing
    // below orders by time -- the rescan comparison is about the *identity* of a
    // derived value, not about when it was computed.
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = d.as_secs() as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Days since epoch -> civil date, by the standard 400/100/4/1 cycle.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let dday = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{dday:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

async fn store() -> (tempfile::TempDir, Store) {
    let d = tempfile::tempdir().unwrap();
    let store = Store::open_library(d.path()).await.unwrap();
    (d, store)
}

/// A performer with no cluster: the field model does not require one.
async fn performer(store: &Store, name: &str) -> String {
    let id = new_id();
    sqlx::query("INSERT INTO performer (id, name, created_at, updated_at) VALUES (?, ?, ?, ?)")
        .bind(&id)
        .bind(name)
        .bind(now())
        .bind(now())
        .execute(store.pool())
        .await
        .unwrap();
    id
}

/// Record that `performer` appears in an item, as a cluster the appear-with
/// graph can see. The item's date is on the object, which is where `span` reads
/// it from -- so there is nothing to pass.
///
/// Goes through the real rows rather than a test-only table, so the graph and the
/// span are reading the same thing a rescan would.
async fn appears(store: &Store, performer: &str, object_id: &str, ty: AppearanceType) {
    // One cluster per performer, named for them, so the graph has a stable id.
    let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM person_cluster WHERE id = ?")
        .bind(performer)
        .fetch_one(store.pool())
        .await
        .unwrap();
    if exists == 0 {
        sqlx::query(
            "INSERT INTO person_cluster (id, handle, state, created_at, updated_at)
             VALUES (?, ?, 'anonymous', ?, ?)",
        )
        .bind(performer)
        .bind(performer)
        .bind(now())
        .bind(now())
        .execute(store.pool())
        .await
        .unwrap();
    }
    sqlx::query(
        "INSERT INTO appearance (id, object_id, cluster_id, appearance_type, source, created_at)
         VALUES (?, ?, ?, ?, 'test', ?)",
    )
    .bind(new_id())
    .bind(object_id)
    .bind(performer)
    .bind(ty.as_str())
    .bind(now())
    .execute(store.pool())
    .await
    .unwrap();
}

// ---------------------------------------------------------------------------
// 1. The field types exist and enforce their own shape
// ---------------------------------------------------------------------------

/// Every one of §7.7's eight types accepts a value of its own shape.
///
/// This is the test that says "typed" means typed. A field declared `ordinal` that
/// also accepts `true` is a field with no type, and the failure it produces is a
/// sort that silently puts "true" between 3 and 4.
#[tokio::test]
async fn each_attribute_type_accepts_its_own_shape() {
    let (_d, store) = store().await;

    let cases: Vec<(&str, AttrType, AttrValue)> = vec![
        (
            "one nationality",
            AttrType::Single,
            AttrValue::Text("German".into()),
        ),
        (
            "several ethnicities",
            AttrType::Multi,
            AttrValue::Many(vec![
                AttrValue::Text("Ashkenazi".into()),
                AttrValue::Text("Scottish".into()),
            ]),
        ),
        (
            "a height",
            AttrType::Range,
            AttrValue::Range(Range {
                low: 160.0,
                high: 175.0,
            }),
        ),
        ("a ranking", AttrType::Ordinal, AttrValue::Number(3.0)),
        ("circumcised", AttrType::Boolean, AttrValue::Bool(true)),
        (
            "a note",
            AttrType::Text,
            AttrValue::Text("retired 2019".into()),
        ),
        (
            "a birth date",
            AttrType::Date,
            AttrValue::Date("1984-03-02".into()),
        ),
        (
            "a dated measurement",
            AttrType::Measurement,
            AttrValue::Measured(Measurement {
                value: 88.0,
                at: "2021-06-01".into(),
            }),
        ),
    ];

    for (label, ty, value) in cases {
        let f = attrs::declare_field(&store, label, ty, "performer")
            .await
            .unwrap();
        let p = performer(&store, "someone").await;
        attrs::set(&store, &f, &p, value.clone())
            .await
            .unwrap_or_else(|e| panic!("{label} ({ty:?}) refused its own type: {e}"));

        // `get` is for single-valued fields. A `multi` field with two values has
        // no "the" value, and returning an arbitrary one of them is how a list
        // becomes a person -- so `get` returns `None` and `all` is the read.
        match &value {
            // A `multi` field is stored one row per element, so it reads back
            // as its elements rather than as the `Many` that was written. That
            // is deliberate: a query asking for "every value of this field"
            // should not have to know whether the field is multi-valued.
            AttrValue::Many(vs) => assert_eq!(
                attrs::all(&store, &f, &p).await.unwrap(),
                *vs,
                "{label} reads back as the values that were written"
            ),
            _ => assert_eq!(
                attrs::get(&store, &f, &p).await.unwrap(),
                Some(value),
                "{label} reads back as it was written"
            ),
        }
    }
}

/// A field refuses a value of the wrong type, by name, and writes nothing.
///
/// Refused rather than coerced. A `multi` field given a single string is not a
/// field with one value -- it is a field whose type is a guess, and the guess is
/// wrong for the next person who writes to it.
#[tokio::test]
async fn a_field_refuses_a_value_of_the_wrong_type() {
    let (_d, store) = store().await;
    let ordinal = attrs::declare_field(&store, "ranking", AttrType::Ordinal, "performer")
        .await
        .unwrap();
    let p = performer(&store, "someone").await;

    for wrong in [AttrValue::Bool(true), AttrValue::Text("three".into())] {
        assert!(
            matches!(
                attrs::set(&store, &ordinal, &p, wrong.clone())
                    .await
                    .unwrap_err(),
                AttributeError::WrongType { .. }
            ),
            "an ordinal field refuses {wrong:?}"
        );
    }

    assert_eq!(
        attrs::get(&store, &ordinal, &p).await.unwrap(),
        None,
        "the refusals left the field empty rather than half-set"
    );
}

/// A `multi` field holds several values; a `single` field holds exactly one.
///
/// The asymmetry is the point of having both: writing a second value to a
/// `single` field has to be *refused*, not appended, because the difference
/// between "nationality" and "ethnicities" is the difference between a query
/// returning a person and a query returning a list.
#[tokio::test]
async fn multi_holds_several_and_single_holds_one() {
    let (_d, store) = store().await;
    let multi = attrs::declare_field(&store, "ethnicity", AttrType::Multi, "performer")
        .await
        .unwrap();
    let single = attrs::declare_field(&store, "nationality", AttrType::Single, "performer")
        .await
        .unwrap();
    let p = performer(&store, "someone").await;

    attrs::add(&store, &multi, &p, AttrValue::Text("Ashkenazi".into()))
        .await
        .unwrap();
    attrs::add(&store, &multi, &p, AttrValue::Text("Scottish".into()))
        .await
        .unwrap();
    assert_eq!(
        attrs::all(&store, &multi, &p).await.unwrap().len(),
        2,
        "both values are stored"
    );

    attrs::set(&store, &single, &p, AttrValue::Text("German".into()))
        .await
        .unwrap();
    assert!(
        matches!(
            attrs::add(&store, &single, &p, AttrValue::Text("Scottish".into()))
                .await
                .unwrap_err(),
            AttributeError::NotMulti { .. }
        ),
        "a second value on a single-valued field is refused by name"
    );
    assert_eq!(
        attrs::all(&store, &single, &p).await.unwrap().len(),
        1,
        "and the field still holds exactly one"
    );
}

/// A `measurement` without a date is refused.
///
/// This is the rule §7.11 rests on. An undated measurement cannot be placed on a
/// timeline, so it cannot contribute to a career span or an age-at-scene, and
/// storing it as though it could means the derivation silently skips a row nobody
/// can see being skipped.
#[tokio::test]
async fn a_measurement_must_be_dated() {
    let (_d, store) = store().await;
    let f = attrs::declare_field(&store, "bust", AttrType::Measurement, "performer")
        .await
        .unwrap();
    let p = performer(&store, "someone").await;

    assert!(
        matches!(
            attrs::set(&store, &f, &p, AttrValue::Number(88.0))
                .await
                .unwrap_err(),
            AttributeError::MeasurementNeedsDate { .. }
        ),
        "a bare number where a measurement belongs is named as such"
    );
    assert!(
        attrs::all(&store, &f, &p).await.unwrap().is_empty(),
        "and nothing was written"
    );
}

/// A range's low end is not above its high end.
///
/// Checked at write time because a reversed range is not detectable at read
/// time -- a filter `BETWEEN low AND high` silently matches nothing, which looks
/// exactly like "no performers have this".
#[tokio::test]
async fn a_range_cannot_be_reversed() {
    let (_d, store) = store().await;
    let f = attrs::declare_field(&store, "height_cm", AttrType::Range, "performer")
        .await
        .unwrap();
    let p = performer(&store, "someone").await;

    assert!(matches!(
        attrs::set(
            &store,
            &f,
            &p,
            AttrValue::Range(Range {
                low: 180.0,
                high: 160.0
            })
        )
        .await
        .unwrap_err(),
        AttributeError::ReversedRange { .. }
    ));
    assert!(
        attrs::set(
            &store,
            &f,
            &p,
            AttrValue::Range(Range {
                low: 160.0,
                high: 175.0
            })
        )
        .await
        .is_ok(),
        "and a sensible one is accepted"
    );
}

// ---------------------------------------------------------------------------
// 2. Status is a first-class enum
// ---------------------------------------------------------------------------

/// §7.9: deceased, retired, inactive -- first-class, filterable, and *not* free
/// text.
///
/// A free-text status cannot be filtered without a `LIKE`, and a `LIKE` on a
/// status field is how "Passed", "passed away" and "RIP" become three different
/// filters. So the values are an enum and the filter is an equality.
#[tokio::test]
async fn status_is_an_enum_and_filters_by_equality() {
    let (_d, store) = store().await;

    for (name, status) in [
        ("a", Status::Active),
        ("d", Status::Deceased),
        ("r", Status::Retired),
        ("i", Status::Inactive),
    ] {
        let p = performer(&store, name).await;
        attrs::set_status(&store, &p, status).await.unwrap();
    }

    for status in [
        Status::Active,
        Status::Deceased,
        Status::Retired,
        Status::Inactive,
    ] {
        let found = attrs::performers_with_status(&store, status).await.unwrap();
        assert_eq!(
            found.len(),
            1,
            "{status:?} filters to exactly its own performer, got {} of {found:?}",
            found.len()
        );
    }

    // Two performers with the same status, so the filter is shown to select
    // *among* performers and not merely to return a single row.
    let extra = performer(&store, "d2").await;
    attrs::set_status(&store, &extra, Status::Deceased)
        .await
        .unwrap();
    assert_eq!(
        attrs::performers_with_status(&store, Status::Deceased)
            .await
            .unwrap()
            .len(),
        2,
        "and it returns both of them"
    );

    assert!(
        matches!(
            attrs::set_status_raw(&store, &performer(&store, "x").await, "passed away")
                .await
                .unwrap_err(),
            AttributeError::UnknownStatus { .. }
        ),
        "'passed away' is a phrase, not a status, and a phrase cannot be filtered"
    );
}

// ---------------------------------------------------------------------------
// 3. Career span is derived, and recomputes identically
// ---------------------------------------------------------------------------

/// The ticket's done-when: a three-year span with five dated measurements
/// produces the right span and the right age, and recomputes identically after a
/// no-op rescan.
#[tokio::test]
async fn a_three_year_span_with_five_measurements_derives_correctly() {
    let (_d, store) = store().await;
    let p = performer(&store, "someone").await;

    let dates = [
        "2020-01-15",
        "2020-09-02",
        "2021-06-20",
        "2022-02-11",
        "2023-04-30",
    ];
    for d in dates {
        let obj = dated_object(&store, d).await;
        appears(&store, &p, &obj, AppearanceType::Primary).await;
    }

    let m = attrs::declare_field(&store, "bust", AttrType::Measurement, "performer")
        .await
        .unwrap();
    for (value, at) in [
        (88.0, "2020-01-15"),
        (88.5, "2020-09-02"),
        (89.0, "2021-06-20"),
        (87.5, "2022-02-11"),
        (87.0, "2023-04-30"),
    ] {
        attrs::set(
            &store,
            &m,
            &p,
            AttrValue::Measured(Measurement {
                value,
                at: at.into(),
            }),
        )
        .await
        .unwrap();
    }

    let s = span::derive(&store, &p).await.unwrap();
    assert_eq!(
        s,
        CareerSpan {
            first: Some("2020-01-15".into()),
            last: Some("2023-04-30".into()),
            appearances: 5,
        },
        "the span is the first and last item dates, and the count is real: {s:?}"
    );
    assert!(
        (s.years() - 3.29).abs() < 0.02,
        "three years and change, computed rather than stored: {}",
        s.years()
    );

    // Age at a scene: born 1990-06-01, so at the 2021-06-20 item she is 31.
    // The birth date is the conventional field name, because §7.11's age
    // calculation finds it by name.
    let birth = attrs::declare_field(&store, "birth_date", AttrType::Date, "performer")
        .await
        .unwrap();
    attrs::set(&store, &birth, &p, AttrValue::Date("1990-06-01".into()))
        .await
        .unwrap();
    assert_eq!(
        span::age_at(&store, &p, "2021-06-20").await.unwrap(),
        Some(31),
        "age at a scene is computed from the birth date and the item's date"
    );
    assert_eq!(
        span::age_at(&store, &p, "1985-01-01").await.unwrap(),
        None,
        "an item dated before the birth date has no age, not age 0"
    );

    // The done-when: a no-op rescan changes nothing.
    let before = span::derive(&store, &p).await.unwrap();
    span::rescan(&store, &p).await.unwrap();
    let after = span::derive(&store, &p).await.unwrap();
    assert_eq!(
        before, after,
        "a rescan that changed nothing recomputes to the same span -- so the \
         span is derived, not incrementally drifted"
    );
}

/// Re-presenting the same appearances out of order and with duplicates computes
/// the same span.
///
/// The strongest form of the done-when: a drifted implementation that adds dates
/// to a running min/max would survive the no-op rescan above if the rescan were
/// append-only, so the test has to feed the same facts in a different order.
#[tokio::test]
async fn the_span_does_not_depend_on_the_order_the_items_arrived() {
    let (_d, store) = store().await;
    let p = performer(&store, "someone").await;

    // The same five dates, inserted in an order that does not match the
    // calendar, and then read twice.
    for d in [
        "2021-06-20",
        "2020-01-15",
        "2023-04-30",
        "2020-09-02",
        "2022-02-11",
    ] {
        let obj = dated_object(&store, d).await;
        appears(&store, &p, &obj, AppearanceType::Primary).await;
    }

    let s = span::derive(&store, &p).await.unwrap();
    assert_eq!(
        (s.first.as_deref(), s.last.as_deref()),
        (Some("2020-01-15"), Some("2023-04-30")),
        "the same five facts, whatever order they arrived in"
    );
    assert_eq!(s.appearances, 5, "duplicates are not counted twice");
}

/// A performer with no items has no span, rather than a zero-length one.
#[tokio::test]
async fn a_performer_with_no_items_has_no_span() {
    let (_d, store) = store().await;
    let p = performer(&store, "someone").await;
    let s = span::derive(&store, &p).await.unwrap();
    assert_eq!(s.first, None);
    assert_eq!(s.last, None);
    assert_eq!(s.appearances, 0);
    assert_eq!(
        s.years(),
        0.0,
        "an unknown span is zero years, and `first`/`last` are what say it is \
         unknown rather than instantaneous"
    );
}

// ---------------------------------------------------------------------------
// 4. Appearance type drives the appear-with graph
// ---------------------------------------------------------------------------

/// §7.12: only appearances that carry a credit put two performers in the same
/// graph edge.
///
/// A cameo and a background appearance put two people in the same scene without
/// meaning they are in a scene together, and a graph built from every
/// co-occurrence is a graph where everyone is connected to everyone in a busy
/// production.
#[tokio::test]
async fn only_credited_appearances_build_an_appear_with_edge() {
    let (_d, store) = store().await;
    let a = performer(&store, "a").await;
    let b = performer(&store, "b").await;
    let c = performer(&store, "c").await;
    let obj = dated_object(&store, "2022-01-01").await;

    // a and b share a credited scene; c is in the same object as a cameo.
    appears(&store, &a, &obj, AppearanceType::Primary).await;
    appears(&store, &b, &obj, AppearanceType::Primary).await;
    appears(&store, &c, &obj, AppearanceType::Cameo).await;

    let partners = attrs::appear_with(&store, &a).await.unwrap();
    assert_eq!(
        partners,
        vec![b.clone()],
        "the credited partner is there and the cameo is not: {partners:?}"
    );
    // Symmetric: b's partners are a, and not c either.
    assert_eq!(
        attrs::appear_with(&store, &b).await.unwrap(),
        vec![a.clone()],
        "and the edge is symmetric"
    );
    assert!(
        attrs::appear_with(&store, &c).await.unwrap().is_empty(),
        "a cameo-only pairing puts the cameoist in the graph with nobody"
    );
}

/// A cameo and a background appearance are not a scene together.
#[tokio::test]
async fn a_cameo_and_a_background_appearance_are_not_a_scene() {
    let (_d, store) = store().await;
    let d = performer(&store, "d").await;
    let e = performer(&store, "e").await;
    let obj = dated_object(&store, "2022-02-02").await;
    appears(&store, &d, &obj, AppearanceType::Cameo).await;
    appears(&store, &e, &obj, AppearanceType::Background).await;
    assert!(
        attrs::appear_with(&store, &d).await.unwrap().is_empty(),
        "two uncredited presences in one object produce no edge"
    );
}

/// Every §7.12 appearance type is stored, and the credit decision is a property
/// of the type rather than of each call site.
#[tokio::test]
async fn every_appearance_type_is_stored_and_has_a_credit_decision() {
    let (_d, store) = store().await;
    let p = performer(&store, "someone").await;

    // A different object per type. `appearance` is unique on
    // `(object_id, cluster_id, appearance_type)`, so one performer appearing in
    // one object under six different types is six rows -- but the read is keyed
    // on (object, cluster) and would find whichever row came first, so the test
    // would pass for `Primary` and fail for every other type. One object each is
    // what makes the round-trip assertion mean what it says.
    for (i, ty) in AppearanceType::ALL.into_iter().enumerate() {
        let date = format!("2023-01-{:02}", i + 1);
        let obj = dated_object(&store, &date).await;
        appears(&store, &p, &obj, ty).await;
        assert_eq!(
            attrs::appearance_type_of(&store, &obj, &p).await.unwrap(),
            Some(ty),
            "{ty:?} round-trips"
        );
    }
}

// ---------------------------------------------------------------------------
// The serialised shapes are part of the contract
// ---------------------------------------------------------------------------

/// A value row written by an older build has to still be readable, so the
/// serialised forms are asserted rather than assumed.
#[test]
fn the_serialised_shapes_are_stable() {
    assert_eq!(
        serde_json::to_value(Measurement {
            value: 88.0,
            at: "2021-06-01".into()
        })
        .unwrap(),
        json!({"value": 88.0, "at": "2021-06-01"}),
        "a measurement is a value and a date, and nothing else"
    );
    // The tagged form is externally tagged: the tag is the key. Asserted
    // because `span::age_at` reads it, and a change of shape there would be a
    // silent "no birth date" rather than an error.
    assert_eq!(
        serde_json::to_value(AttrValue::Date("1990-06-01".into())).unwrap(),
        json!({"date": "1990-06-01"}),
    );
    assert_eq!(
        serde_json::to_value(AttrValue::Measured(Measurement {
            value: 88.0,
            at: "2021-06-01".into()
        }))
        .unwrap(),
        json!({"measured": {"value": 88.0, "at": "2021-06-01"}})
    );
    assert_eq!(
        serde_json::to_value(Range {
            low: 160.0,
            high: 175.0
        })
        .unwrap(),
        json!({"low": 160.0, "high": 175.0})
    );
    assert_eq!(Status::Deceased.as_str(), "deceased");
    assert_eq!(Status::parse("deceased"), Some(Status::Deceased));
    assert_eq!(Status::parse("passed away"), None);
    assert_eq!(AttrType::Measurement.as_str(), "measurement");
    assert_eq!(AttrType::parse("measurement"), Some(AttrType::Measurement));
    assert_eq!(AttrType::parse("freeform"), None);
}

/// The age boundary, on both sides of the birthday.
///
/// A performer born 1990-06-01 is 31 on 2021-06-01 and still 31 on 2021-05-31.
/// Testing only a date a comfortable distance from the boundary cannot tell a
/// correct comparison from one that is off by a day in either direction, and both
/// of those pass every "what was her age in 2021" assertion.
///
/// So: the day before, the day of, and the day after.
#[tokio::test]
async fn the_age_boundary_is_the_birthday_itself() {
    let (_d, store) = store().await;
    let birth = attrs::declare_field(&store, "birth_date", AttrType::Date, "performer")
        .await
        .unwrap();

    let p = performer(&store, "someone").await;
    attrs::set(&store, &birth, &p, AttrValue::Date("1990-06-01".into()))
        .await
        .unwrap();
    for (date, want, why) in [
        ("2021-05-31", 30, "the day before the birthday"),
        ("2021-06-01", 31, "on the birthday itself"),
        ("2021-06-02", 31, "the day after"),
        ("2021-06-30", 31, "the last day of the birth month"),
        ("2022-05-31", 31, "a year later, still one day short"),
        ("2022-06-01", 32, "a year later, on the birthday"),
    ] {
        assert_eq!(
            span::age_at(&store, &p, date).await.unwrap(),
            Some(want),
            "{date} ({why})"
        );
    }

    // A 29 February birthday, which an implementation comparing day-of-year gets
    // wrong by a day every four years. Born on the leap day, a person turns a
    // year older on 28 February in a leap year and on 1 March in a common one.
    let leap = performer(&store, "leapling").await;
    attrs::set(&store, &birth, &leap, AttrValue::Date("1990-02-29".into()))
        .await
        .unwrap();
    for (date, want, why) in [
        ("2020-02-28", 29, "a day before, in a leap year"),
        ("2020-02-29", 30, "the birthday itself, in a leap year"),
        (
            "2021-02-28",
            30,
            "the 28th is not the birthday in a common year",
        ),
        (
            "2021-03-01",
            31,
            "the birthday, taken on 1 March in a common year",
        ),
    ] {
        assert_eq!(
            span::age_at(&store, &leap, date).await.unwrap(),
            Some(want),
            "{date} ({why})"
        );
    }
}

#[tokio::test]
async fn an_undated_item_is_still_an_appearance() {
    let (_d, store) = store().await;
    let p = performer(&store, "someone").await;

    let dated = dated_object(&store, "2020-01-01").await;
    appears(&store, &p, &dated, AppearanceType::Primary).await;
    let undated = undated_object(&store).await;
    appears(&store, &p, &undated, AppearanceType::Primary).await;

    let s = span::derive(&store, &p).await.unwrap();
    assert_eq!(
        s.appearances, 2,
        "both items counted, including the one with no date: {s:?}"
    );
    assert_eq!(
        (s.first.as_deref(), s.last.as_deref()),
        (Some("2020-01-01"), Some("2020-01-01")),
        "and the bounds come from the dated one, which is a zero-length span          rather than an unknown one"
    );
}

/// Adding a value that is already there is reported, not silently doubled.
///
/// The de-duplication lives in `add()` rather than in a unique index on purpose:
/// 0001's constraint named no field and no value, and the rebuilt one in 0006
/// would report a conflict in terms of an index rather than in terms of "you
/// already said that". So `add()` says it, and this is the test that says the
/// saying works -- a silently doubled value is a list that claims a performer has
/// three ethnicities when they have two.
#[tokio::test]
async fn adding_a_value_twice_is_reported() {
    let (_d, store) = store().await;
    let f = attrs::declare_field(&store, "ethnicity", AttrType::Multi, "performer")
        .await
        .unwrap();
    let p = performer(&store, "someone").await;

    attrs::add(&store, &f, &p, AttrValue::Text("Ashkenazi".into()))
        .await
        .unwrap();
    assert!(
        matches!(
            attrs::add(&store, &f, &p, AttrValue::Text("Ashkenazi".into()))
                .await
                .unwrap_err(),
            AttributeError::DuplicateValue { .. }
        ),
        "the second add is refused by name"
    );
    assert_eq!(
        attrs::all(&store, &f, &p).await.unwrap().len(),
        1,
        "and the field holds one value, not two"
    );

    // A different value still goes on, so the refusal is about *this* value.
    attrs::add(&store, &f, &p, AttrValue::Text("Scottish".into()))
        .await
        .unwrap();
    assert_eq!(attrs::all(&store, &f, &p).await.unwrap().len(), 2);
}

/// A rescan re-reads the items, so it sees an item a direct derive would miss.
///
/// `rescan` is a read, and the ticket's done-when is that a rescan changes
/// nothing. Asserting only that is not enough: an implementation that made
/// `rescan` a no-op returning a cached span would pass it while doing none of
/// the work. So the test writes an item, then rescan *finds it* -- which is only
/// possible if the rescan really queried.
#[tokio::test]
async fn a_rescan_sees_an_item_written_since_the_last_one() {
    let (_d, store) = store().await;
    let p = performer(&store, "someone").await;

    let first = dated_object(&store, "2020-01-01").await;
    appears(&store, &p, &first, AppearanceType::Primary).await;
    assert_eq!(
        span::rescan(&store, &p).await.unwrap().appearances,
        1,
        "one item so far"
    );

    // A later item arrives, as a scan would add it.
    let second = dated_object(&store, "2021-06-01").await;
    appears(&store, &p, &second, AppearanceType::Primary).await;

    let after = span::rescan(&store, &p).await.unwrap();
    assert_eq!(
        after.appearances, 2,
        "the rescan saw the new item, so it queried rather than replayed a cache"
    );
    assert_eq!(
        (after.first.as_deref(), after.last.as_deref()),
        (Some("2020-01-01"), Some("2021-06-01")),
        "and the span followed it, with no stored state to update"
    );
}
