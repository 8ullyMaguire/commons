//! T-P3-005, the HTTP half: the error mapping and the wire shapes.
//!
//! # What is worth testing in a translation layer
//!
//! Almost nothing, which is why this file is short. The handlers delegate every
//! rule to `commons_identity::claim`; a test that re-ran those rules through the
//! handlers would be testing the same code twice and would break for the wrong
//! reason whenever the domain changed.
//!
//! What *is* worth testing is the part that has no counterpart in the domain:
//!
//! * the status-code mapping, because it is a judgement call written down once
//!   and then never revisited, and
//! * the round trip through serde, because a field that silently stops being
//!   serialised looks exactly like a field that was never there.

use commons_api::claim::{
    self, ApiError, ClaimView, DashboardView, OpenTakedown, SubmitClaim, TakedownView,
};
use commons_identity::claim::ClaimError;

/// Every domain error has a status, and none of them is 200.
///
/// The exhaustiveness is the point: a new `ClaimError` variant will not compile
/// against [`claim::status_for`] until someone has decided what a client sees.
#[test]
fn every_claim_error_maps_to_a_status() {
    let cases: Vec<(ClaimError, u16)> = vec![
        (ClaimError::NoSuchCluster("c".into()), 404),
        (
            ClaimError::AlreadyClaimed {
                cluster: "c".into(),
            },
            409,
        ),
        (ClaimError::TooManyPending, 409),
        (ClaimError::NotPending("x".into()), 409),
        (ClaimError::NoSuchClaim("x".into()), 404),
        (
            ClaimError::NotYourRecord {
                performer: "p".into(),
            },
            403,
        ),
        (
            ClaimError::FieldNotCorrectable {
                field: "consent".into(),
            },
            422,
        ),
    ];
    for (err, want) in cases {
        let got = claim::status_for(&err);
        assert_eq!(
            got.status, want,
            "{err:?} maps to {} but the mapping says {want}: {got:?}",
            got.status
        );
        assert!(
            !got.message.is_empty(),
            "{err:?} maps to a status with no message"
        );
    }
}

/// The two mappings that are not the obvious one.
///
/// `NotYourRecord` is 403 and not 404: the caller is authenticated and asking
/// about a record that exists, and 404 would promise that it does not. That is
/// a promise §7.5 should not be making about another person's identity.
///
/// `FieldNotCorrectable` is 422 and not 403: the caller *may* edit their own
/// record and this particular field is not one of them. Answering 403 would tell
/// a performer they have no permission on their own name.
#[test]
fn the_two_awkward_mappings_are_the_ones_documented() {
    let not_yours = claim::status_for(&ClaimError::NotYourRecord {
        performer: "p".into(),
    });
    assert_eq!(not_yours.status, 403, "authenticated, so not 404");
    assert_ne!(
        not_yours.status, 404,
        "404 would claim the record does not exist; it does"
    );

    let wrong_field = claim::status_for(&ClaimError::FieldNotCorrectable {
        field: "consent".into(),
    });
    assert_eq!(
        wrong_field.status, 422,
        "the permission is fine, the field is not"
    );
    assert_ne!(
        wrong_field.status, 403,
        "403 would tell the performer they cannot edit their own record"
    );
}

/// A claim survives the round trip, evidence included.
///
/// The evidence field is the one most likely to be dropped by accident, because
/// it is the one a non-steward route has no business returning. If it vanishes
/// from the wire form, the steward queue silently stops working and nothing
/// fails until a moderator notices the queue is empty.
#[test]
fn a_claim_survives_the_wire_round_trip_with_its_evidence() {
    let original = ClaimView {
        id: "claim-1".into(),
        cluster_id: "cluster-1".into(),
        account: "anna".into(),
        evidence: "a letter from the publisher".into(),
        state: "queued".into(),
        decided_by: None,
        decided_at: None,
        created_at: "2026-09-27T00:00:00Z".into(),
    };
    let json = serde_json::to_string(&original).unwrap();
    assert!(
        json.contains("a letter from the publisher"),
        "the evidence is on the wire: {json}"
    );
    let back: ClaimView = serde_json::from_str(&json).unwrap();
    assert_eq!(back, original, "and it comes back the same");
}

/// A rejected claim keeps its decider and its timestamp; a queued one has
/// neither.
#[test]
fn the_decision_fields_are_present_and_optional() {
    let queued: ClaimView = serde_json::from_str(
        r#"{"id":"c","cluster_id":"k","account":"a","evidence":"e",
            "state":"queued","decided_by":null,"decided_at":null,"created_at":"t"}"#,
    )
    .unwrap();
    assert_eq!(queued.state, "queued");
    assert!(queued.decided_by.is_none(), "nothing has decided it yet");

    let decided: ClaimView = serde_json::from_str(
        r#"{"id":"c","cluster_id":"k","account":"a","evidence":"e",
            "state":"approved","decided_by":"steward-1","decided_at":"later","created_at":"t"}"#,
    )
    .unwrap();
    assert_eq!(decided.state, "approved");
    assert_eq!(decided.decided_by.as_deref(), Some("steward-1"));
    assert_eq!(decided.decided_at.as_deref(), Some("later"));
}

/// The takedown response tells the complainant where their request went.
///
/// `to_stewards` is the field that matters: a takedown that silently goes
/// nowhere is the one outcome §7.5's third clause exists to prevent, so the
/// response has to say which of the two happened.
#[test]
fn a_takedown_response_says_where_the_request_went() {
    let escalated = TakedownView {
        id: "t1".into(),
        cluster_id: "k".into(),
        recipients: vec![],
        to_stewards: true,
    };
    let json = serde_json::to_string(&escalated).unwrap();
    assert!(
        json.contains("\"to_stewards\":true"),
        "and it is on the wire: {json}"
    );
    let back: TakedownView = serde_json::from_str(&json).unwrap();
    assert_eq!(back, escalated);
    assert!(
        back.recipients.is_empty(),
        "a request with no verified performer has no recipients -- and \
         `to_stewards` is what says it went somewhere"
    );

    let addressed = TakedownView {
        id: "t2".into(),
        cluster_id: "k".into(),
        recipients: vec!["anna".into()],
        to_stewards: false,
    };
    let back: TakedownView =
        serde_json::from_str(&serde_json::to_string(&addressed).unwrap()).unwrap();
    assert_eq!(back.recipients, vec!["anna".to_string()]);
    assert!(!back.to_stewards);
}

/// The submit body converts into the domain type without losing anything.
#[test]
fn the_submit_body_converts_into_the_domain_submit() {
    let body = SubmitClaim {
        cluster_id: "k".into(),
        account: "anna".into(),
        evidence: "because".into(),
    };
    let domain: commons_identity::claim::Submit = body.into();
    assert_eq!(domain.cluster_id, "k");
    assert_eq!(domain.account, "anna");
    assert_eq!(domain.evidence, "because");
}

/// The dashboard view is two string lists, and an account with nothing verified
/// gets empty ones rather than an error.
#[test]
fn an_empty_dashboard_is_empty_not_absent() {
    let dash = DashboardView {
        clusters: vec![],
        appearances: vec![],
    };
    let json = serde_json::to_string(&dash).unwrap();
    assert_eq!(
        json, r#"{"clusters":[],"appearances":[]}"#,
        "an account with no claims gets an empty dashboard, not a 404 -- the \
         page is loaded by every visitor and the UI should not have to tell the \
         two apart"
    );
}

/// The takedown body carries a complainant who need not be the person in the
/// content.
#[test]
fn a_takedown_names_a_complainant_and_a_cluster() {
    let body = OpenTakedown {
        cluster_id: "k".into(),
        requested_by: "a-viewer".into(),
        reason: "not mine".into(),
    };
    assert_eq!(body.cluster_id, "k");
    assert_eq!(
        body.requested_by, "a-viewer",
        "the complainant is whoever reported it, which is not necessarily the \
         verified performer"
    );
}

/// An `ApiError` renders as something a client can be shown.
#[test]
fn an_api_error_is_a_status_and_a_sentence() {
    let e: ApiError = claim::status_for(&ClaimError::NoSuchCluster("c".into()));
    assert_eq!(e.status, 404);
    assert!(!e.message.is_empty());
    assert!(
        e.message.chars().all(|c| !c.is_ascii_uppercase()),
        "a message shown to a person is a sentence, not an identifier: {:?}",
        e.message
    );
}
