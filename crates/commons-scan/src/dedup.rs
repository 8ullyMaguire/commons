//! §9.7: deciding which relation a set of signals implies.
//!
//! The store half (`commons_store::relations`) knows how to *record* a
//! relation. This module decides what to record, from the four signals §9.7
//! ranks. Keeping them apart is the design: "these two files are a re-encode" is
//! a judgement about content and "store a `re_encode_of` row" is a fact about
//! the database, and a test that covers only the second proves nothing about
//! the first.
//!
//! # The four signals, and the one rule that orders them
//!
//! | signal | what it establishes |
//! |---|---|
//! | blake3 equality (§5.18) | the same bytes -- a **fact** |
//! | xxh128 equality | nothing, on its own; see below |
//! | phash Hamming distance (#1220) | the same picture, re-encoded -- an **estimate** |
//! | size, bitrate (#5067, #2397) | which copy to keep, never that there is a duplicate |
//!
//! The rule is one sentence: **only a fact may assert a relation on its own.**
//! A phash match proposes; it does not assert. That is the whole reason the
//! false-positive case in the plan's accept criterion is worth a test -- a
//! threshold-based feature with no fact underneath it is a feature that is
//! wrong sometimes and has no way to be told apart from being right.
//!
//! ## Why xxh128 is not consulted
//!
//! `file` carries two hashes and a reader will assume both are identity
//! signals. They are not: §6.2 uses xxh128 as a fast *change* detector and
//! blake3 as *content identity*, deliberately, because xxh128 is 128 bits of
//! xxHash and a library of a few million files will eventually collide it.
//! Dedup merges things, so a collision here deletes a file the user wanted. So
//! this module reads `blake3` only, and the comment is here so that "optimise
//! this to use the faster hash" is recognisable as the bug it would be.

use commons_core::RelationType;
use commons_store::relations::{RelationError, Relations};
use serde::{Deserialize, Serialize};

/// The perceptual-hash algorithm a hash was computed with.
///
/// Not decoration. 0009 stores the algorithm because pHash has three
/// incompatible definitions in common use, and two files whose 64 bits happen
/// to be equal under *different* algorithms are not a match -- they are
/// different numbers that collide. Treating them as comparable produces a
/// duplicate at exactly the Hamming distance that was supposed to mean
/// "confident".
pub const DEFAULT_ALGORITHM: &str = "phash64";

/// What two files are to each other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verdict {
    /// Identical bytes. §5.18 identity, and the only verdict that is a fact.
    Identical,
    /// Same picture, different bytes. An estimate, and the direction is a
    /// claim about which file is the original.
    ReEncode,
    /// Close enough to propose, far enough not to assert. `distance` says how
    /// close; a caller that wants to show the user a suggestion shows this.
    Possible { distance: u32 },
    /// Not related.
    Distinct,
}

impl Verdict {
    /// The relation this verdict implies, if any.
    ///
    /// `Possible` maps to `None` on purpose. It is the one verdict with a
    /// number attached and no relation behind it, because a phash distance is
    /// evidence and a relation is a claim, and the gap between them is where
    /// #1656 lives.
    pub fn relation(self) -> Option<RelationType> {
        match self {
            Verdict::Identical => Some(RelationType::SameSceneAs),
            Verdict::ReEncode => Some(RelationType::ReEncodeOf),
            _ => None,
        }
    }

    /// May this verdict be written without a human?
    ///
    /// `Possible` may not, and that is the whole reason this method exists
    /// rather than `relation().is_some()`: the difference between "propose" and
    /// "assert" is not visible in the relation type, because both are a row in
    /// the same table.
    pub fn is_assertable(self) -> bool {
        matches!(self, Verdict::Identical)
    }
}

/// How close is close enough?
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhashPolicy {
    /// At or below this Hamming distance, the two files are a re-encode.
    /// 10 of 64 bits is the common default and it is a *guess*: the number
    /// where a re-encode and an unrelated scene overlap. It belongs in a
    /// caller-supplied value rather than a constant for that reason.
    pub re_encode_at_or_below: u32,
    /// At or below this, and above the above, the pair is *offered* to a user.
    pub possible_at_or_below: u32,
    /// The algorithm both hashes must have been computed with.
    pub algorithm: &'static str,
}

impl Default for PhashPolicy {
    fn default() -> Self {
        Self {
            re_encode_at_or_below: 10,
            possible_at_or_below: 20,
            algorithm: DEFAULT_ALGORITHM,
        }
    }
}

/// The content facts about one file, as the classifier sees them.
///
/// A struct rather than reading the database directly, because the classifier
/// is then a pure function of its inputs and can be tested without a store at
/// all -- which is the only way to test the *thresholds*, since a threshold bug
/// needs a pair at exactly the boundary and building one through SQL is a
/// fixture that can be wrong in a way the assertion cannot see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileFacts {
    pub object_id: String,
    /// §5.18 content identity. `None` for a file that has not been hashed
    /// yet, which is a normal state during a scan and is why identity is a
    /// fallible question rather than a field.
    pub blake3: Option<String>,
    pub phash: Option<String>,
    pub phash_algorithm: Option<String>,
    pub size_bytes: i64,
}

/// Hamming distance between two hex strings, in bits.
///
/// Returns `None` when the two are not comparable -- different lengths, or
/// either one is not hex. The first version returned `u32::MAX` for a
/// mismatch, which reads as "infinitely far" and is right for a *ranking* and
/// wrong for a *query*: a phash stored under a different algorithm is not a
/// distant neighbour, it is not a neighbour at all, and ranking it last still
/// puts it in the candidate set. `Option` makes "not comparable" and "far
/// apart" different values, which is what the query needs.
///
/// The name is what §9.7 (#1220) calls it and what `commons-index` already
/// calls it; this is the one definition, and the copy there is deleted rather
/// than left to diverge.
pub fn hamming(a: &str, b: &str) -> Option<u32> {
    if a.len() != b.len() || a.is_empty() || !a.len().is_multiple_of(2) {
        return None;
    }
    let (ab, bb) = (hex_bytes(a)?, hex_bytes(b)?);
    Some(ab.iter().zip(&bb).map(|(x, y)| (x ^ y).count_ones()).sum())
}

fn hex_bytes(s: &str) -> Option<Vec<u8>> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

/// Classify one pair. Pure, and the only place a threshold is read.
pub fn classify(a: &FileFacts, b: &FileFacts, policy: &PhashPolicy) -> Verdict {
    // Identity first and unconditionally. A pair with identical bytes is
    // `Identical` whatever its phash says, because "these are the same file"
    // does not become less true because a perceptual hash disagrees -- and
    // phash is a summary, so a disagreement is evidence about the *summary*.
    if let (Some(x), Some(y)) = (&a.blake3, &b.blake3) {
        if x == y {
            return Verdict::Identical;
        }
    }
    let (Some(pa), Some(pb)) = (&a.phash, &b.phash) else {
        return Verdict::Distinct;
    };
    // A hash from another algorithm is not comparable, and is not a distant
    // neighbour. Skipping it here is what stops a foreign hash being ranked
    // last instead of being absent.
    if a.phash_algorithm.as_deref() != Some(policy.algorithm)
        || b.phash_algorithm.as_deref() != Some(policy.algorithm)
    {
        return Verdict::Distinct;
    }
    let Some(d) = hamming(pa, pb) else {
        return Verdict::Distinct;
    };
    if d <= policy.re_encode_at_or_below {
        Verdict::ReEncode
    } else if d <= policy.possible_at_or_below {
        Verdict::Possible { distance: d }
    } else {
        Verdict::Distinct
    }
}

/// Which of two files is the original, for #5067's "keep the highest
/// bitrate".
///
/// Size, because a bitrate is a function of a duration this layer does not
/// have. Stated in the doc because the spec says "bitrate" and a reader will
/// otherwise assume a timebase is hiding somewhere. The tie is the smaller
/// object id, so the answer is a function of the data and not of row order --
/// which differs between the two engines.
pub fn preferred_copy<'a>(a: &'a FileFacts, b: &'a FileFacts) -> &'a FileFacts {
    match a.size_bytes.cmp(&b.size_bytes) {
        std::cmp::Ordering::Greater => a,
        std::cmp::Ordering::Less => b,
        std::cmp::Ordering::Equal => {
            if a.object_id <= b.object_id {
                a
            } else {
                b
            }
        }
    }
}

/// A classified pair, with the evidence attached.
///
/// The evidence is kept because §9.7's UI is "side-by-side merge with the
/// direction made obvious" (#6429) and "100%-identical display" (#5823): a
/// merge screen that shows a verdict without showing *why* is asking the user
/// to approve a claim the system cannot justify.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub left: String,
    pub right: String,
    pub verdict: Verdict,
    /// The phash distance, when the two phashes were comparable. `None` for an
    /// identical-bytes pair that was decided before the phash was read, which
    /// is why it is not inside `Verdict`.
    pub distance: Option<u32>,
    /// The id of the file the system would keep, per #5067.
    pub preferred: String,
}

/// Classify every pair in a set and return the findings.
///
/// Deliberately O(n²) over the given set. A library-wide duplicate sweep is
/// driven by `hash_blake3`, which is indexed (0001), so the real query is a
/// join on equal hashes and not a scan; what is left here is the small,
/// candidate-shaped set a caller has already narrowed -- the items with
/// similar phashes, say. So the function takes what it is given and does not
/// pretend to be the thing that finds the candidates.
///
/// Pairs already ruled out are dropped. A suppression is the one input that
/// can veto, because it is the only one that came from a person.
pub fn classify_all(
    files: &[FileFacts],
    policy: &PhashPolicy,
    ruled_out: &dyn Fn(&str, &str) -> bool,
) -> Vec<Finding> {
    let mut out = Vec::new();
    for (i, a) in files.iter().enumerate() {
        for b in files.iter().skip(i + 1) {
            if ruled_out(&a.object_id, &b.object_id) {
                continue;
            }
            let verdict = classify(a, b, policy);
            if verdict == Verdict::Distinct {
                continue;
            }
            out.push(Finding {
                left: a.object_id.clone(),
                right: b.object_id.clone(),
                verdict,
                distance: match (&a.phash, &b.phash) {
                    (Some(x), Some(y)) => hamming(x, y),
                    _ => None,
                },
                preferred: preferred_copy(a, b).object_id.clone(),
            });
        }
    }
    out
}

/// Write the assertable findings, and return all of them.
///
/// The split is the design: `classify_all` returns everything including the
/// `Possible` ones, and this writes only what a fact supports. A caller that
/// wants to write a `Possible` has to say so explicitly, which is the only
/// honest way for an estimate to reach a table that also holds decisions.
pub async fn store_findings(
    relations: &Relations<'_>,
    findings: &[Finding],
) -> Result<Vec<RelationType>, RelationError> {
    let mut written = Vec::new();
    for f in findings.iter().filter(|f| f.verdict.is_assertable()) {
        if let Some(relation) = f.verdict.relation() {
            relations
                .assert_relation(&f.left, &f.right, relation)
                .await?;
            written.push(relation);
        }
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(id: &str, blake3: Option<&str>, phash: Option<&str>) -> FileFacts {
        FileFacts {
            object_id: id.to_string(),
            blake3: blake3.map(str::to_string),
            phash: phash.map(str::to_string),
            phash_algorithm: phash.map(|_| DEFAULT_ALGORITHM.to_string()),
            size_bytes: 1_000,
        }
    }

    #[test]
    fn hamming_counts_differing_bits() {
        assert_eq!(hamming("ffff0000ffff0000", "ffff0000ffff0000"), Some(0));
        assert_eq!(hamming("0000ffff0000ffff", "ffff0000ffff0000"), Some(64));
    }

    #[test]
    fn hamming_refuses_rather_than_guessing() {
        // Different lengths, and not hex. Both are "not comparable", which is
        // a different answer from "very far apart" -- and the first version
        // returned `u32::MAX` for both, so a foreign hash ranked last in a
        // candidate set instead of being absent from it.
        assert_eq!(hamming("ffff", "ffffffff"), None);
        assert_eq!(hamming("zzzz", "0000"), None);
        assert_eq!(hamming("", "0000"), None);
        assert_eq!(hamming("fff", "fff"), None); // odd nibble count
    }

    #[test]
    fn identical_bytes_beat_a_disagreeing_phash() {
        // A phash is a summary, so a disagreement is evidence about the
        // summary, not about the file.
        let a = facts("a", Some("same"), Some("0000000000000000"));
        let b = facts("b", Some("same"), Some("ffffffffffffffff"));
        assert_eq!(
            classify(&a, &b, &PhashPolicy::default()),
            Verdict::Identical
        );
    }

    #[test]
    fn a_close_phash_is_a_re_encode_and_a_distant_one_is_not() {
        let policy = PhashPolicy::default();
        // One bit apart: a re-encode, under both engines, every time.
        let a = facts("a", Some("one"), Some("0000000000000000"));
        let b = facts("b", Some("two"), Some("0000000000000001"));
        assert_eq!(classify(&a, &b, &policy), Verdict::ReEncode);

        // Thirty bits apart: nothing.
        let c = facts("c", Some("three"), Some("ffffffffffffffff"));
        assert_eq!(classify(&a, &c, &policy), Verdict::Distinct);
    }

    #[test]
    fn a_foreign_algorithm_is_not_a_neighbour_at_all() {
        let policy = PhashPolicy::default();
        let mut a = facts("a", Some("one"), Some("0000000000000000"));
        let b = facts("b", Some("two"), Some("0000000000000000"));
        a.phash_algorithm = Some("phash64-v2".to_string());
        // Identical bits, so every distance calculation says "these are the
        // same picture". The algorithms differ, so they are not the same
        // picture -- they are two different numbers that happen to be equal.
        assert_eq!(classify(&a, &b, &policy), Verdict::Distinct);
    }

    #[test]
    fn only_identity_may_assert() {
        assert!(Verdict::Identical.is_assertable());
        assert!(!Verdict::ReEncode.is_assertable());
        assert!(!Verdict::Possible { distance: 4 }.is_assertable());
        assert!(!Verdict::Distinct.is_assertable());
    }

    #[test]
    fn a_verdict_without_evidence_has_no_relation() {
        assert_eq!(
            Verdict::Identical.relation(),
            Some(RelationType::SameSceneAs)
        );
        assert_eq!(Verdict::ReEncode.relation(), Some(RelationType::ReEncodeOf));
        assert_eq!(Verdict::Possible { distance: 4 }.relation(), None);
    }

    #[test]
    fn the_larger_copy_is_preferred_and_a_tie_is_not_row_order() {
        let mut a = facts("a", Some("one"), None);
        a.size_bytes = 900;
        let mut b = facts("b", Some("two"), None);
        b.size_bytes = 700;
        assert_eq!(preferred_copy(&a, &b).object_id, "a");

        // Equal sizes: the id decides, so both engines agree.
        a.size_bytes = 900;
        b.size_bytes = 900;
        assert_eq!(preferred_copy(&a, &b).object_id, "a");
        assert_eq!(preferred_copy(&b, &a).object_id, "a");
    }

    #[test]
    fn a_ruled_out_pair_is_not_even_reported() {
        let a = facts("a", Some("same"), None);
        let b = facts("b", Some("same"), None);
        let c = facts("c", Some("same"), None);
        let all = classify_all(
            &[a.clone(), b.clone(), c.clone()],
            &PhashPolicy::default(),
            &|_, _| false,
        );
        assert_eq!(all.len(), 3, "three pairs, three findings");

        // A person ruled out one pair. The other two are still there, and that
        // is the point: the suppression is about the pair, not about the
        // objects, and a veto that generalised would hide real duplicates.
        let all = classify_all(&[a, b, c], &PhashPolicy::default(), &|x, y| {
            (x == "a" && y == "b") || (x == "b" && y == "a")
        });
        assert_eq!(all.len(), 2, "one pair vetoed, two left: {all:?}");
    }

    #[test]
    fn a_finding_carries_the_distance_it_was_decided_on() {
        let a = facts("a", Some("one"), Some("0000000000000000"));
        let b = facts("b", Some("two"), Some("000000000000000f"));
        let all = classify_all(&[a, b], &PhashPolicy::default(), &|_, _| false);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].distance, Some(4));
        assert_eq!(all[0].verdict, Verdict::ReEncode);
    }

    #[test]
    fn a_missing_content_hash_means_never_identical() {
        // `blake3: None` is the normal state of a file the scanner has not
        // hashed yet. The property that matters is not "unrelated" -- it is
        // that an absent hash can never make two files *identical*, because
        // `Identical` is the only verdict that may assert and a fact needs both
        // sides of it. This test asserted `Distinct` and got `ReEncode`, which
        // is correct: the phashes are identical and that is real evidence. The
        // assertion was wrong about the question, not about the answer.
        let a = facts("a", None, Some("0000000000000000"));
        let b = facts("b", Some("two"), Some("0000000000000000"));
        let v = classify(&a, &b, &PhashPolicy::default());
        assert_ne!(
            v,
            Verdict::Identical,
            "no content hash, so no identity claim"
        );
        assert!(
            !v.is_assertable(),
            "and therefore nothing may be written for it: {v:?}"
        );
    }

    #[test]
    fn a_file_with_no_signals_at_all_is_distinct() {
        let a = facts("a", None, None);
        let b = facts("b", Some("two"), Some("0000000000000000"));
        assert_eq!(classify(&a, &b, &PhashPolicy::default()), Verdict::Distinct);
    }
}
