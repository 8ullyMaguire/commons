//! Segments: one file, many objects (T-P1-003, spec §5.2).
//!
//! This is the single primitive that answers stash#3530 (38 comments upstream),
//! #2276 (multi-part scenes), and #2511 (virtual compilations). All three feel
//! like separate features and are actually the same question — *can one file
//! back more than one object?* — asked three times.
//!
//! The shape:
//!
//! ```text
//! File ──┬── Segment(idx 0, 0ms..1800ms)   ──▶ Object A
//!        └── Segment(idx 1, 1800ms..5400ms) ──▶ Object B
//! ```
//!
//! A whole-file object has exactly one segment covering the file, so "one file,
//! one object" and "one file, four objects" are the same code path with a
//! different number of rows. There is no separate "multi-scene" concept to keep
//! consistent.
//!
//! What this module deliberately does **not** do is decide *where* the
//! boundaries are. That is a curation question — a human, or a marker-driven
//! proposal (Phase 4) — and this crate's job is to make a given set of
//! boundaries correct, checkable, and cheap to apply.

use std::collections::BTreeMap;

/// A `(file, start, end)` window backing one object.
///
/// `start_ms` and `end_ms` are offsets from the start of the *file*, not from
/// the start of the containing object. Every offset in the system is a file
/// offset: markers, chapters, and segments all agree, so a marker at 00:30 of
/// a four-scene file lands unambiguously in one segment without any
/// segment-relative arithmetic anywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Segment {
    /// Position within the file. Dense from 0: `UNIQUE (file_id, idx)` in the
    /// schema, so a gap would be legal but meaningless.
    pub idx: i32,
    pub start_ms: i64,
    pub end_ms: i64,
}

impl Segment {
    /// Build a segment, panicking on an inverted or zero-length range.
    ///
    /// Panicking rather than returning a `Result` is the right trade here: every
    /// caller derives these numbers from either user input already validated or
    /// a probe, and an inverted range is a bug in the caller rather than a
    /// condition to handle. [`Segment::try_new`] is there for the boundary
    /// case where zero length is legitimate.
    pub fn new(idx: i32, start_ms: i64, end_ms: i64) -> Self {
        Self::try_new(idx, start_ms, end_ms).expect("segment must be non-empty and ordered")
    }

    /// Build a segment, rejecting an inverted or zero-length range.
    pub fn try_new(idx: i32, start_ms: i64, end_ms: i64) -> Option<Self> {
        if end_ms <= start_ms || start_ms < 0 {
            return None;
        }
        Some(Self {
            idx,
            start_ms,
            end_ms,
        })
    }

    /// The whole file, given its duration.
    pub fn whole_file(duration_ms: i64) -> Self {
        Self::new(0, 0, duration_ms.max(1))
    }

    /// Length in milliseconds.
    pub fn duration_ms(&self) -> i64 {
        self.end_ms - self.start_ms
    }

    /// Does this segment contain `at_ms`? Half-open: `[start, end)`, so two
    /// adjacent segments do not both claim the boundary instant.
    pub fn contains(&self, at_ms: i64) -> bool {
        at_ms >= self.start_ms && at_ms < self.end_ms
    }

    /// Shift a marker into this segment's own coordinates.
    ///
    /// Markers are stored as file offsets, so this is only needed at the display
    /// or playback boundary, where a player needs "how far into *this* scene is
    /// the marker".
    pub fn local_ms(&self, file_offset_ms: i64) -> Option<i64> {
        if self.contains(file_offset_ms) {
            Some(file_offset_ms - self.start_ms)
        } else {
            None
        }
    }

    /// The local offset, clamped to the segment's bounds rather than rejected.
    ///
    /// For a *range* (a marker with a duration) whose start is in an earlier
    /// segment: the range still overlaps, and clamping the local start to 0
    /// keeps the visible portion rather than dropping the marker entirely.
    pub fn local_ms_clamped(&self, file_offset_ms: i64) -> i64 {
        (file_offset_ms - self.start_ms).clamp(0, self.duration_ms())
    }
}

/// Why a split was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SplitError {
    /// The boundaries, sorted, are not strictly increasing.
    NotIncreasing { at: Vec<i64> },
    /// A boundary is negative, or beyond the file's duration.
    OutOfRange { boundary: i64, duration_ms: i64 },
    /// Fewer than two boundaries — that is not a split.
    TooFewBoundaries { given: usize },
    /// The file's duration is unknown, so boundaries cannot be validated.
    UnknownDuration,
}

impl std::fmt::Display for SplitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SplitError::NotIncreasing { at } => {
                write!(f, "segment boundaries must strictly increase, got {at:?}")
            }
            SplitError::OutOfRange {
                boundary,
                duration_ms,
            } => write!(
                f,
                "boundary {boundary}ms is outside the file (0..{duration_ms}ms)"
            ),
            SplitError::TooFewBoundaries { given } => {
                write!(f, "a split needs at least 2 boundaries, got {given}")
            }
            SplitError::UnknownDuration => {
                write!(f, "cannot split a file whose duration is unknown")
            }
        }
    }
}

impl std::error::Error for SplitError {}

/// The result of splitting a file.
#[derive(Debug, Clone, PartialEq)]
pub struct Split {
    pub segments: Vec<Segment>,
    /// Markers that were re-homed into a new object, keyed by segment index.
    /// The caller performs the database writes; this is the plan.
    pub marker_moves: BTreeMap<i32, Vec<MarkerMove>>,
    /// Objects created for segments after the first, in order. The first
    /// segment keeps the original object, so an existing object id, its tags,
    /// and its rating history are not thrown away by a split.
    pub new_object_titles: Vec<String>,
}

/// A marker being moved to a new object, with its offset already rebased.
#[derive(Debug, Clone, PartialEq)]
pub struct MarkerMove {
    /// Index of the destination segment.
    pub segment_idx: i32,
    /// Offset within the destination object (not the file).
    pub local_start_ms: i64,
    /// Duration preserved, so a 5-second marker stays 5 seconds long.
    pub duration_ms: i64,
    pub title: String,
}

/// The minimum information a split needs about an existing marker.
#[derive(Debug, Clone, PartialEq)]
pub struct ExistingMarker {
    pub start_ms: i64,
    /// `None` for a point marker.
    pub duration_ms: Option<i64>,
    pub title: String,
}

/// Split a file at the given boundaries (milliseconds from the file start).
///
/// `boundaries` need not be sorted; they are sorted and de-duplicated here, so
/// a caller that collected them from a marker list in arbitrary order gets the
/// right answer. Boundaries that are equal after sorting collapse, because two
/// scenes cannot begin at the same instant.
///
/// The first segment keeps the original object. Everything after it becomes a
/// new object, and markers are re-homed with their offsets rebased into the new
/// object's own timeline (stash#5089: a marker at 00:30 of a split file must end
/// up at the right place in the right scene, not 30 minutes into a 2-minute
/// scene or dropped).
///
/// # Example
///
/// ```
/// use commons_scan::segment::split_file;
///
/// let markers = vec![
///     commons_scan::segment::ExistingMarker { start_ms: 30_000, duration_ms: None, title: "Opening".into() },
/// ];
/// let split = split_file(300_000, vec![90_000, 180_000, 240_000], &markers, |i| format!("Part {}", i + 1)).unwrap();
///
/// assert_eq!(split.segments.len(), 4);
/// // 00:30 is inside segment 0 (0..90s), rebased to 30s of local time.
/// assert_eq!(split.marker_moves[&0][0].local_start_ms, 30_000);
/// ```
pub fn split_file<F>(
    duration_ms: i64,
    boundaries_ms: Vec<i64>,
    markers: &[ExistingMarker],
    title_for: F,
) -> Result<Split, SplitError>
where
    F: Fn(usize) -> String,
{
    if duration_ms <= 0 {
        return Err(SplitError::UnknownDuration);
    }

    // Sort, then dedupe: two boundaries at the same instant would produce a
    // zero-length segment, which cannot exist. `len` is taken before the move
    // because the "too few" error reports how many the caller supplied.
    let supplied = boundaries_ms.len();
    let mut bounds: Vec<i64> = boundaries_ms.into_iter().filter(|b| *b >= 0).collect();
    bounds.sort_unstable();
    bounds.dedup();

    // Drop anything at or past the end, which would leave a zero-length tail.
    bounds.retain(|b| *b < duration_ms);

    if bounds.is_empty() {
        return Err(SplitError::TooFewBoundaries { given: supplied });
    }

    // After sorting and deduping, the sequence is already strictly increasing,
    // so this can only fire if a caller reaches `split_file` through a future
    // path that skips that normalisation. Cheap to keep as a guard.
    for w in bounds.windows(2) {
        if w[0] >= w[1] {
            return Err(SplitError::NotIncreasing { at: bounds });
        }
    }

    // Build edges: 0, then each boundary, then the duration.
    let mut edges: Vec<i64> = Vec::with_capacity(bounds.len() + 2);
    edges.push(0);
    edges.extend_from_slice(&bounds);
    edges.push(duration_ms);

    let segments: Vec<Segment> = edges
        .windows(2)
        .enumerate()
        .map(|(idx, w)| Segment::new(idx as i32, w[0], w[1]))
        .collect();

    // Re-home markers. A marker belongs to the segment containing its start; a
    // ranged marker that starts in an earlier segment but overlaps this one is
    // also carried, clamped, so the visible part is not lost.
    let mut marker_moves: BTreeMap<i32, Vec<MarkerMove>> = BTreeMap::new();
    for seg in &segments {
        let mut here: Vec<MarkerMove> = Vec::new();
        for m in markers {
            let dur = m.duration_ms.unwrap_or(0);
            // A zero-length marker (a point) has an empty half-open range
            // [start, start), so the general overlap test below would exclude
            // it from every segment -- including the one it sits inside. Points
            // are therefore tested for containment, and ranges for overlap.
            let overlaps = if dur == 0 {
                seg.contains(m.start_ms)
            } else {
                m.start_ms < seg.end_ms && (m.start_ms + dur) > seg.start_ms
            };
            if !overlaps {
                continue;
            }
            here.push(MarkerMove {
                segment_idx: seg.idx,
                local_start_ms: seg.local_ms_clamped(m.start_ms),
                duration_ms: dur,
                title: m.title.clone(),
            });
        }
        if !here.is_empty() {
            marker_moves.insert(seg.idx, here);
        }
    }

    let new_object_titles = (1..segments.len()).map(title_for).collect();

    Ok(Split {
        segments,
        marker_moves,
        new_object_titles,
    })
}

/// Merge a file's segments back into one. The inverse of a split, used when a
/// user undoes one.
///
/// Markers are re-based onto the file timeline; a caller that wants to restore
/// exactly should prefer a soft-delete over this, but the arithmetic is here
/// for the case where the merge is genuinely wanted.
pub fn merge_segments(segments: &[Segment]) -> Segment {
    let start = segments.iter().map(|s| s.start_ms).min().unwrap_or(0);
    let end = segments.iter().map(|s| s.end_ms).max().unwrap_or(start + 1);
    Segment::new(0, start, end.max(start + 1))
}

/// Re-base a local offset from a segment back onto the file timeline.
pub fn to_file_offset(seg: &Segment, local_ms: i64) -> i64 {
    (seg.start_ms + local_ms).clamp(seg.start_ms, seg.end_ms)
}

/// A compilation (stash#2511, #2085) is not a new type. It is an object whose
/// segments point at parts held by other files, expressed as relations.
///
/// This returns the relations to create; the relation kind is `PartOf` with
/// `from` = the compilation and `to` = each part, which is what lets a
/// compilation participate in tagging, rating, and search exactly as any other
/// object does.
pub fn compilation_relations(
    compilation_id: &str,
    part_object_ids: &[String],
) -> Vec<CompilationRelation> {
    part_object_ids
        .iter()
        .map(|part| CompilationRelation {
            from_id: compilation_id.to_string(),
            to_id: part.clone(),
            relation: "part_of".to_string(),
        })
        .collect()
}

/// The relation row for a compilation edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompilationRelation {
    pub from_id: String,
    pub to_id: String,
    /// Always `part_of`; a string rather than an enum because the schema stores
    /// the relation as text and `commons-core` is the crate that owns
    /// `RelationType`.
    pub relation: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marker(start: i64, dur: Option<i64>, title: &str) -> ExistingMarker {
        ExistingMarker {
            start_ms: start,
            duration_ms: dur,
            title: title.to_string(),
        }
    }

    fn title(i: usize) -> String {
        format!("Part {}", i + 1)
    }

    /// The ticket's acceptance criterion, verbatim: three boundaries, four
    /// segments, and a marker at 00:30 landed in the right one.
    #[test]
    fn three_boundaries_make_four_segments_and_route_the_marker() {
        let markers = vec![marker(30_000, None, "Opening")];
        let split = split_file(300_000, vec![90_000, 180_000, 240_000], &markers, title).unwrap();

        assert_eq!(split.segments.len(), 4);
        assert_eq!(
            split.segments[0],
            Segment::new(0, 0, 90_000),
            "segment 0 covers the first 90 seconds"
        );
        assert_eq!(
            split.segments[3].end_ms, 300_000,
            "the tail reaches the end"
        );

        // 00:30 is inside segment 0, so it stays with the first object -- and
        // the point of the test is that it is *rebased*, not carried at 30s of
        // a timeline that happens to start at zero.
        let moved = &split.marker_moves[&0][0];
        assert_eq!(moved.local_start_ms, 30_000);
        assert_eq!(moved.title, "Opening");
    }

    /// stash#5089, and the reason the test above only proves half the ticket:
    /// a marker in a LATER segment has a non-zero segment start, so carrying
    /// its file offset through un-rebased would place it 180 seconds into a
    /// 60-second scene.
    #[test]
    fn a_marker_in_a_later_segment_is_rebased_not_carried() {
        let markers = vec![marker(200_000, None, "Late")];
        let split = split_file(300_000, vec![90_000, 180_000, 240_000], &markers, title).unwrap();

        assert!(
            !split.marker_moves.contains_key(&0),
            "the marker must not leak into segment 0"
        );
        let moved = &split.marker_moves[&2][0];
        assert_eq!(moved.segment_idx, 2, "200s is inside 180s..240s");
        assert_eq!(
            moved.local_start_ms, 20_000,
            "20s into a segment that starts at 180s, not 200s"
        );
    }

    #[test]
    fn a_marker_exactly_on_a_boundary_belongs_to_the_later_segment() {
        // Half-open intervals: the instant a new scene begins is the new
        // scene's, not the previous one's. Otherwise the last frame of scene 1
        // and the first frame of scene 2 are both claimed.
        let markers = vec![marker(90_000, None, "Boundary")];
        let split = split_file(300_000, vec![90_000, 180_000], &markers, title).unwrap();
        assert!(!split.marker_moves.contains_key(&0));
        assert_eq!(split.marker_moves[&1][0].local_start_ms, 0);
    }

    #[test]
    fn a_ranged_marker_spanning_a_boundary_appears_in_both_segments_clamped() {
        // A 30s marker from 75s to 105s crosses the 90s boundary. Dropping it
        // from scene 1 would silently lose 15 seconds of a marker the user can
        // see in the UI.
        let markers = vec![marker(75_000, Some(30_000), "Long")];
        let split = split_file(300_000, vec![90_000, 180_000], &markers, title).unwrap();

        let in0 = &split.marker_moves[&0][0];
        assert_eq!(in0.local_start_ms, 75_000, "whole marker is in scene 1");
        assert_eq!(in0.duration_ms, 30_000, "duration preserved");

        let in1 = &split.marker_moves[&1][0];
        assert_eq!(
            in1.local_start_ms, 0,
            "the part inside scene 2 is clamped to its start"
        );
    }

    #[test]
    fn boundaries_may_arrive_unsorted_and_duplicate() {
        let split =
            split_file(300_000, vec![240_000, 90_000, 90_000, 180_000], &[], title).unwrap();
        assert_eq!(
            split.segments.len(),
            4,
            "duplicates collapse, order is fixed"
        );
        assert_eq!(split.segments[1].start_ms, 90_000);
    }

    #[test]
    fn a_boundary_at_or_past_the_end_is_dropped_rather_than_making_an_empty_tail() {
        let split = split_file(100_000, vec![50_000, 100_000, 150_000], &[], title).unwrap();
        assert_eq!(split.segments.len(), 2);
        assert_eq!(split.segments[1].end_ms, 100_000);
    }

    #[test]
    fn a_single_boundary_is_a_valid_two_part_split() {
        // stash#2276 multi-part: two parts from one cut.
        let split = split_file(100_000, vec![50_000], &[], title).unwrap();
        assert_eq!(split.segments.len(), 2);
    }

    #[test]
    fn no_boundaries_is_an_error_not_a_one_segment_no_op() {
        assert_eq!(
            split_file(100_000, vec![], &[], title),
            Err(SplitError::TooFewBoundaries { given: 0 })
        );
    }

    #[test]
    fn an_unknown_duration_is_refused_rather_than_guessed() {
        // Splitting a file we could not probe would produce boundaries measured
        // against nothing.
        assert_eq!(
            split_file(0, vec![1000, 2000], &[], title),
            Err(SplitError::UnknownDuration)
        );
    }

    #[test]
    fn the_first_segment_keeps_the_original_object_and_the_rest_are_new() {
        // A split that threw away the object's tags, rating, and history would
        // be a data-loss event dressed as a feature.
        let split = split_file(300_000, vec![100_000, 200_000], &[], title).unwrap();
        assert_eq!(
            split.new_object_titles,
            vec!["Part 2".to_string(), "Part 3".to_string()],
            "only segments 1..n create objects; segment 0 keeps the original"
        );
    }

    #[test]
    fn segments_tile_the_file_with_no_gap_and_no_overlap() {
        let split = split_file(300_000, vec![90_000, 90_001, 200_000], &[], title).unwrap();
        let segs = &split.segments;
        assert_eq!(segs[0].start_ms, 0);
        assert_eq!(segs.last().unwrap().end_ms, 300_000);
        for pair in segs.windows(2) {
            assert_eq!(
                pair[0].end_ms, pair[1].start_ms,
                "segments must abut exactly, or part of the file has no object"
            );
        }
        let total: i64 = segs.iter().map(Segment::duration_ms).sum();
        assert_eq!(total, 300_000, "durations must sum to the file's length");
    }

    #[test]
    fn a_whole_file_segment_covers_everything() {
        let seg = Segment::whole_file(5_400_000);
        assert_eq!(seg.idx, 0);
        assert!(seg.contains(0));
        assert!(seg.contains(5_399_999));
        assert!(!seg.contains(5_400_000), "the end is exclusive");
        assert_eq!(seg.duration_ms(), 5_400_000);
    }

    #[test]
    fn an_inverted_or_empty_segment_is_rejected() {
        assert!(Segment::try_new(0, 500, 500).is_none(), "zero length");
        assert!(Segment::try_new(0, 500, 400).is_none(), "inverted");
        assert!(Segment::try_new(0, -1, 400).is_none(), "negative start");
        assert!(Segment::try_new(0, 0, 1).is_some());
    }

    #[test]
    fn merging_restores_the_original_span() {
        let segs = vec![
            Segment::new(0, 0, 90_000),
            Segment::new(1, 90_000, 180_000),
            Segment::new(2, 180_000, 300_000),
        ];
        let merged = merge_segments(&segs);
        assert_eq!(merged, Segment::new(0, 0, 300_000));
    }

    #[test]
    fn a_local_offset_round_trips_back_to_the_file_offset() {
        let seg = Segment::new(1, 180_000, 240_000);
        let local = seg.local_ms(200_000).unwrap();
        assert_eq!(local, 20_000);
        assert_eq!(to_file_offset(&seg, local), 200_000);
    }

    #[test]
    fn an_offset_outside_the_segment_has_no_local_value() {
        let seg = Segment::new(1, 180_000, 240_000);
        assert_eq!(seg.local_ms(10_000), None, "before");
        assert_eq!(seg.local_ms(300_000), None, "after");
        assert_eq!(seg.local_ms_clamped(10_000), 0, "clamped instead");
        assert_eq!(seg.local_ms_clamped(300_000), 60_000);
    }

    /// stash#2511 and #2085: a compilation is a relation, not a type.
    #[test]
    fn a_compilation_is_a_part_of_relation_per_part() {
        let rels = compilation_relations("comp-1", &["part-a".into(), "part-b".into()]);
        assert_eq!(rels.len(), 2);
        assert!(rels.iter().all(|r| r.from_id == "comp-1"));
        assert!(rels.iter().all(|r| r.relation == "part_of"));
        assert_eq!(rels[1].to_id, "part-b");
    }
}
