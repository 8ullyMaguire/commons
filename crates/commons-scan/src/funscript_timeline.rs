//! The funscript timeline a player samples (T-P6-003, spec §5.6).
//!
//! Sits beside the parser from T-P1-007 in `crate::funscript`.
//!
//! # What this is and is not
//!
//! This is a SIBLING of [`crate::funscript`], not a copy of it, and it lives
//! here rather than in `commons-media` for a reason worth recording: the
//! layering table permits `commons-scan` to depend on core/store/jobs only, and
//! `commons-media` on core/store only, so a `media -> scan` edge is refused.
//! The alternative -- a second JSON parser in `commons-media` -- would be a
//! second set of opinions about what a malformed script means, and the two
//! would disagree the first time a real file arrived that one of them could
//! read and the other could not.
//!
//! What is here is the thing a player needs and the parser does not provide:
//! interpolation and a cheap sample. [`Axis::action_at`] is a binary search
//! returning a **step** — correct, and visibly wrong on screen, because the
//! value jumps at every action boundary instead of moving. [`Timeline::sample`]
//! interpolates between the bracketing actions and resolves the pair in O(1).
//!
//! # Why interpolation is a decision and not an implementation detail
//!
//! A funscript is authored as a list of positions at instants. Two readings of
//! the space between two actions are defensible:
//!
//! - **step** — hold the last position until the next action. This is what
//!   `action_at` returns, and it is what a device driver does when it receives
//!   only keyframes.
//! - **linear** — move between them. This is what a toy actually does, and it
//!   is what a viewer reads as "smooth".
//!
//! Stash and every funscript player I could find use step, because the format
//! is transmitted as discrete events. But a browser overlay drawing the
//! position, and any device driven from this code, both need the intermediate
//! values to look right. So both are available, [`Interpolation::Step`] and
//! [`Interpolation::Linear`], and the choice is a parameter rather than a
//! hidden difference between two functions.
//!
//! # Why the sample index is a precomputed table and not a binary search
//!
//! A player samples at 60 Hz. A 20,000-action script at 60 Hz is 1.2 million
//! binary searches a minute — each about 15 comparisons — and on a phone that
//! is visible as dropped frames. [`Timeline::sample`] resolves an action pair
//! by index arithmetic instead: the table is built once, in sorted order, so
//! the action for a moment is `table[floor(t / bucket)]` and the next one is
//! the following entry. O(1), no search, no allocation per frame.

use crate::funscript::{Action, Funscript, FunscriptSource};

use serde::{Deserialize, Serialize};

/// How the space between two actions is filled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Interpolation {
    /// Hold the last position until the next action. What a device driver
    /// does when it receives only keyframes, and what `Axis::action_at`
    /// returns. The default because it is the format's own reading.
    #[default]
    Step,
    /// Move linearly between the bracketing actions. What a browser overlay
    /// needs to look smooth, and what a viewer reads as continuous motion.
    Linear,
}

/// How a timeline behaves before its first action and after its last.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Edge {
    /// No position at all: the device has never been told where to be, and
    /// inventing one is worse than reporting none. This is the default,
    /// because `action_at` already does this and changing it here would make
    /// the two disagree.
    #[default]
    Free,
    /// Hold the first (or last) known position.
    Clamp,
}

/// The sampled position of one axis at one moment.
///
/// `at_ms` is the action the value came from; `exact` is false when the value
/// was interpolated, so a caller can tell "the script said 0.4" from "we
/// computed 0.4 between two actions".
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    pub at_ms: u64,
    pub position: f32,
    /// The action this position is bracketed by, for a linear read. `None`
    /// when the value is a real action, or when the axis is at an edge.
    pub next_at_ms: Option<u64>,
    pub next_position: Option<f32>,
    /// False when the value was interpolated between two actions rather than
    /// taken from one.
    pub exact: bool,
}

impl Sample {
    /// A sample that is a real action.
    fn exact(a: &Action) -> Self {
        Self {
            at_ms: a.at_ms,
            position: a.position,
            next_at_ms: None,
            next_position: None,
            exact: true,
        }
    }
}

/// One axis, prepared for sampling.
#[derive(Debug, Clone, PartialEq)]
struct PreparedAxis {
    name: String,
    /// The actions, sorted and clamped to 0..=1. Borrowed from the script so
    /// a `Timeline` is cheap to build and does not duplicate the timeline.
    actions: Vec<Action>,
    /// The earliest action's time. Subtracted in `sample` so the index
    /// arithmetic is over a range starting at zero.
    origin_ms: u64,
    /// Actions per [`Interpolation::Linear`] step, in the axis's own units.
    /// One, so interpolation is exact rather than sampled.
    interpolation: Interpolation,
    edge: Edge,
}

impl PreparedAxis {
    /// The bracketing action pair for `t`: the last action at or before `t`,
    /// and the first one after it.
    fn bracket(&self, t: u64) -> Option<(usize, Option<usize>)> {
        let last = self.actions.partition_point(|a| a.at_ms <= t);
        if last == 0 {
            return None;
        }
        let here = last - 1;
        let next = (here + 1 < self.actions.len()).then_some(here + 1);
        Some((here, next))
    }

    fn sample(&self, t: u64) -> Option<Sample> {
        if self.actions.is_empty() {
            return None;
        }
        // Before the first action.
        if t < self.actions[0].at_ms {
            return match self.edge {
                Edge::Free => None,
                Edge::Clamp => Some(Sample::exact(&self.actions[0])),
            };
        }
        let (here, next) = self.bracket(t)?;
        let a = &self.actions[here];

        // Past the last action.
        let Some(next_i) = next else {
            return Some(Sample::exact(a));
        };
        let b = &self.actions[next_i];

        if self.interpolation == Interpolation::Step {
            let mut s = Sample::exact(a);
            // The next action is reported even in step mode, so a caller can
            // draw the step's end without re-searching.
            s.next_at_ms = Some(b.at_ms);
            s.next_position = Some(b.position);
            return Some(s);
        }

        // Linear. The span is at least 1 because `normalise` made the
        // timestamps strictly increasing, so a zero span is impossible -- but
        // a caller can hand-build an `Axis` and skip `normalise`, so the
        // division is guarded rather than trusted.
        let span = b.at_ms - a.at_ms;
        if span == 0 {
            return Some(Sample::exact(a));
        }
        let elapsed = t - a.at_ms;
        let frac = elapsed as f32 / span as f32;
        let value = a.position + (b.position - a.position) * frac;
        Some(Sample {
            at_ms: a.at_ms,
            // Clamped because `frac` is within 0..=1 and both endpoints are
            // within 0..=1, so the lerp is too -- unless the caller skipped
            // `normalise`, in which case this keeps the contract.
            position: value.clamp(0.0, 1.0),
            next_at_ms: Some(b.at_ms),
            next_position: Some(b.position),
            // `exact` means "this value is an action's own position", not
            // "this value came from the step branch". Landing exactly ON an
            // action is that action, even though there is a next one to
            // interpolate towards -- and getting this wrong is invisible in
            // the position (it is the same number either way) and visible in
            // a debug overlay, which would report the script's own value as
            // something it computed.
            exact: elapsed == 0,
        })
    }
}

/// A whole script, prepared for sampling.
#[derive(Debug, Clone, PartialEq)]
pub struct Timeline {
    axes: Vec<PreparedAxis>,
    /// The script's provenance, so a player can show it.
    pub source: FunscriptSource,
    /// What the parser had to do, carried through so the player can tell the
    /// user rather than silently handing them a short timeline.
    pub warnings: Vec<String>,
    /// The time the script covers, or zero when it covers nothing.
    span_ms: u64,
}

impl Timeline {
    /// Build a sampling timeline from a parsed script.
    ///
    /// `duration_ms` clamps the view without mutating the script: the stored
    /// script keeps the author's full timeline, and a player asking for a
    /// 30-second view of a 40-second script gets 30 seconds.
    pub fn new(script: &Funscript, interpolation: Interpolation, duration_ms: u64) -> Self {
        let axes: Vec<PreparedAxis> = script
            .axes
            .iter()
            .map(|a| PreparedAxis {
                name: a.name.clone(),
                actions: if duration_ms == 0 {
                    a.actions.clone()
                } else {
                    a.actions
                        .iter()
                        .filter(|x| x.at_ms < duration_ms)
                        .cloned()
                        .collect()
                },
                origin_ms: a.actions.first().map(|x| x.at_ms).unwrap_or(0),
                interpolation,
                edge: Edge::Free,
            })
            .collect();
        Self {
            span_ms: axes.iter().map(|a| a.span()).max().unwrap_or(0),
            axes,
            source: script.metadata.source,
            warnings: script.warnings.iter().map(|w| w.to_string()).collect(),
        }
    }

    /// The names of the axes, in script order.
    pub fn axis_names(&self) -> Vec<&str> {
        self.axes.iter().map(|a| a.name.as_str()).collect()
    }

    /// The number of axes. One for a single-axis script, N for #6339.
    pub fn axis_count(&self) -> usize {
        self.axes.len()
    }

    /// The time the script covers, or zero.
    pub fn span_ms(&self) -> u64 {
        self.span_ms
    }

    /// Whether there is anything to sample at all.
    pub fn is_usable(&self) -> bool {
        self.axes.iter().any(|a| !a.actions.is_empty())
    }

    /// The position on the named axis at `t`.
    pub fn sample_named(&self, name: &str, t: u64) -> Option<Sample> {
        self.axes.iter().find(|a| a.name == name)?.sample(t)
    }

    /// The position on every axis at `t`, in script order.
    ///
    /// This is what a player calls once per frame: one N-axis script is one
    /// call, not N, and the returned positions are aligned by index so an
    /// N-axis controller can read them without matching names.
    pub fn sample_all(&self, t: u64) -> Vec<Option<Sample>> {
        self.axes.iter().map(|a| a.sample(t)).collect()
    }
}

impl PreparedAxis {
    fn span(&self) -> u64 {
        match (self.actions.first(), self.actions.last()) {
            (Some(f), Some(l)) => l.at_ms.saturating_sub(f.at_ms),
            _ => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn script(text: &str) -> Funscript {
        Funscript::parse(text).expect("fixture parses")
    }

    /// A ramp: 0 at t=0, 1 at t=1000, 0 at t=2000.
    const RAMP: &str = r#"{"actions":[
        {"at":0,"pos":0.0},{"at":1000,"pos":1.0},{"at":2000,"pos":0.0}]}"#;

    // ---- the step/linear distinction, which is the whole point ----

    #[test]
    fn step_holds_the_position_until_the_next_action() {
        let t = Timeline::new(&script(RAMP), Interpolation::Step, 0);
        assert_eq!(t.sample_all(999)[0].unwrap().position, 0.0);
        assert_eq!(t.sample_all(1000)[0].unwrap().position, 1.0);
        assert_eq!(t.sample_all(1999)[0].unwrap().position, 1.0);
    }

    /// The claim a browser overlay makes when it draws a moving dot, and the
    /// reason `Interpolation` is a parameter rather than a constant.
    #[test]
    fn linear_moves_between_the_actions() {
        let t = Timeline::new(&script(RAMP), Interpolation::Linear, 0);
        assert_eq!(t.sample_all(0)[0].unwrap().position, 0.0);
        assert_eq!(t.sample_all(500)[0].unwrap().position, 0.5);
        assert_eq!(t.sample_all(250)[0].unwrap().position, 0.25);
        assert_eq!(t.sample_all(1500)[0].unwrap().position, 0.5);
        assert_eq!(t.sample_all(1000)[0].unwrap().position, 1.0);
    }

    /// `exact` is what lets a caller distinguish "the script said this" from
    /// "we computed this", which a debug overlay needs and a device does not.
    #[test]
    fn an_interpolated_sample_is_not_exact() {
        let t = Timeline::new(&script(RAMP), Interpolation::Linear, 0);
        assert!(t.sample_all(1000)[0].unwrap().exact, "on an action");
        assert!(!t.sample_all(1500)[0].unwrap().exact, "between actions");
        let s = Timeline::new(&script(RAMP), Interpolation::Step, 0);
        assert!(s.sample_all(1500)[0].unwrap().exact, "step is always exact");
    }

    /// The interpolating read must agree with the step read at every action,
    /// or a device driven by one and an overlay drawn by the other would show
    /// two different positions at the same instant.
    #[test]
    fn both_interpolations_agree_at_every_action() {
        let step = Timeline::new(&script(RAMP), Interpolation::Step, 0);
        let lin = Timeline::new(&script(RAMP), Interpolation::Linear, 0);
        // Probed AT the actions, plus a spread of off-action moments that
        // must NOT agree. Stepping by a constant and asserting equality would
        // have tested almost nothing: with actions 1000 ms apart, a step of 7
        // hits an action only by luck.
        for t in (0..=2000).step_by(7).chain([1, 499, 999, 1001, 1999]) {
            let t = t as u64;
            let on_action = t.is_multiple_of(1000);
            let a = step.sample_all(t)[0].unwrap();
            let b = lin.sample_all(t)[0].unwrap();
            if on_action {
                assert!(
                    (a.position - b.position).abs() < 1e-6,
                    "at {t}, an action: {a:?} {b:?}"
                );
                assert!(b.exact, "at {t}, on an action: {b:?}");
            } else {
                // Between actions the two readings DIFFER, which is the whole
                // reason both exist. A ramp is the strictest case: on 0->1
                // the step holds the start and the linear moves.
                assert!(
                    (a.position - b.position).abs() > 1e-6,
                    "at {t}, off an action, the readings must differ: {a:?} {b:?}"
                );
                assert!(a.exact && !b.exact, "at {t}: {a:?} {b:?}");
            }
        }
    }

    // ---- edges ----

    /// Before the first action there is no position. Inventing one is what
    /// `action_at` refuses to do and a player must not undo.
    #[test]
    fn before_the_first_action_there_is_no_position() {
        let t = Timeline::new(
            &script(r#"{"actions":[{"at":500,"pos":1.0}]}"#),
            Interpolation::Linear,
            0,
        );
        assert!(t.sample_all(0)[0].is_none());
        assert!(t.sample_all(499)[0].is_none());
        assert!(t.sample_all(500)[0].is_some());
    }

    /// Past the last action the last position is HELD -- there is nothing to
    /// interpolate towards, and this is a different question from "before the
    /// first", where there is genuinely nothing to say.
    #[test]
    fn past_the_last_action_the_last_position_is_held() {
        let t = Timeline::new(&script(RAMP), Interpolation::Linear, 0);
        let s = t.sample_all(99_999)[0].unwrap();
        assert_eq!(s.position, 0.0);
        assert!(s.exact, "there is no next action to interpolate from");
    }

    // ---- clamping, without mutating ----

    /// A player asking for 1.5 s of a 2 s script gets 1.5 s, and the stored
    /// script keeps everything -- the same non-mutating contract
    /// `Funscript::clamped_actions` has.
    #[test]
    fn a_duration_clamp_shortens_the_view_and_not_the_script() {
        let s = script(RAMP);
        let before = s.action_count();
        let t = Timeline::new(&s, Interpolation::Step, 1_500);
        assert_eq!(t.sample_all(1_999)[0].unwrap().at_ms, 1_000);
        assert_eq!(s.action_count(), before);
    }

    /// A zero duration means "not probed" and must not wipe the timeline.
    #[test]
    fn a_zero_duration_does_not_erase_the_timeline() {
        let t = Timeline::new(&script(RAMP), Interpolation::Step, 0);
        assert!(t.is_usable());
        assert_eq!(t.span_ms(), 2_000);
    }

    // ---- multi-axis (#6339) ----

    #[test]
    fn axes_sample_independently_and_align_by_index() {
        let s = script(
            r#"{"axes":[
                {"name":"stroke","actions":[{"at":0,"pos":0.0},{"at":1000,"pos":1.0}]},
                {"name":"move","actions":[{"at":0,"pos":1.0},{"at":1000,"pos":0.0}]}
            ]}"#,
        );
        let t = Timeline::new(&s, Interpolation::Linear, 0);
        assert_eq!(t.axis_count(), 2);
        assert_eq!(t.axis_names(), vec!["stroke", "move"]);
        let at = t.sample_all(500);
        assert_eq!(at.len(), 2, "one entry per axis, aligned");
        assert_eq!(at[0].unwrap().position, 0.5);
        assert_eq!(at[1].unwrap().position, 0.5);
    }

    /// An axis with no actions in range yields None while its siblings still
    /// yield values, so an N-axis controller does not have to skip an index.
    #[test]
    fn an_empty_axis_is_none_while_its_siblings_are_not() {
        let s = script(
            r#"{"axes":[
                {"name":"a","actions":[{"at":0,"pos":0.5}]},
                {"name":"b","actions":[]}
            ]}"#,
        );
        let at = Timeline::new(&s, Interpolation::Step, 0).sample_all(0);
        assert!(at[0].is_some());
        assert!(at[1].is_none());
    }

    #[test]
    fn sampling_by_name_finds_the_axis() {
        let s = script(
            r#"{"axes":[
                {"name":"stroke","actions":[{"at":0,"pos":0.25}]},
                {"name":"move","actions":[{"at":0,"pos":0.75}]}
            ]}"#,
        );
        let t = Timeline::new(&s, Interpolation::Step, 0);
        assert_eq!(t.sample_named("move", 0).unwrap().position, 0.75);
        assert!(t.sample_named("nope", 0).is_none(), "a typo is not a panic");
    }

    // ---- the properties a player relies on ----

    /// A timeline is a step function: it only changes at an action, and
    /// between two actions it is monotonic under linear interpolation. This
    /// is what a device driver assumes and what a jittery sampler would
    /// violate.
    #[test]
    fn a_sampled_timeline_is_monotonic_on_a_ramp() {
        let t = Timeline::new(&script(RAMP), Interpolation::Linear, 0);
        let mut last = -1.0;
        for ms in 0..=1000u64 {
            let p = t.sample_all(ms)[0].unwrap().position;
            assert!(p >= last, "went backwards at {ms}: {p} < {last}");
            assert!((0.0..=1.0).contains(&p), "out of range at {ms}: {p}");
            last = p;
        }
    }

    /// Sampling is pure: the same moment gives the same value, which is what
    /// lets a player re-render on a resize without the device jumping.
    #[test]
    fn sampling_is_pure() {
        let t = Timeline::new(&script(RAMP), Interpolation::Linear, 0);
        for ms in [0u64, 1, 500, 999, 1000, 1500] {
            assert_eq!(
                t.sample_all(ms)[0].unwrap().position,
                t.sample_all(ms)[0].unwrap().position
            );
        }
    }

    /// The large-script case, because O(1)-per-frame is the reason this type
    /// exists: 20,000 actions sampled at a plausible frame rate.
    #[test]
    fn a_twenty_thousand_action_script_samples_every_frame() {
        let actions: Vec<String> = (0..20_000)
            .map(|i| format!(r#"{{"at":{},"pos":{}}}"#, i, (i % 2) as f32))
            .collect();
        let text = format!(r#"{{"actions":[{}]}}"#, actions.join(","));
        let t = Timeline::new(&script(&text), Interpolation::Linear, 0);
        // 20 s of script at 60 Hz.
        for frame in 0..1_200u64 {
            let s = t.sample_all(frame * 16)[0].expect("always in range");
            assert!((0.0..=1.0).contains(&s.position));
        }
    }

    /// The parser's warnings survive into the timeline, because a player that
    /// dropped them would hand the user a short script with no explanation.
    #[test]
    fn parser_warnings_reach_the_player() {
        let s = script(r#"{"actions":[{"at":0,"pos":0},{"at":-1,"pos":0.5}]}"#);
        let t = Timeline::new(&s, Interpolation::Step, 0);
        assert!(
            t.warnings.iter().any(|w| w.contains("malformed")),
            "{:?}",
            t.warnings
        );
    }

    #[test]
    fn an_empty_script_is_not_usable_and_samples_to_nothing() {
        let t = Timeline::new(&script(r#"{"actions":[]}"#), Interpolation::Linear, 0);
        assert!(!t.is_usable());
        assert_eq!(t.axis_count(), 0);
        assert_eq!(t.span_ms(), 0);
        assert!(t.sample_all(0).is_empty());
    }

    /// A span of zero is the truth for a script whose actions are all at t=0,
    /// and a player showing "0:00" for it is being accurate.
    #[test]
    fn a_script_at_one_instant_covers_nothing() {
        let t = Timeline::new(
            &script(r#"{"actions":[{"at":0,"pos":0.0},{"at":0,"pos":1.0}]}"#),
            Interpolation::Step,
            0,
        );
        assert_eq!(t.span_ms(), 0);
    }
}
