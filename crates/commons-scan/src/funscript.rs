//! Funscript: discovery, parsing, and typed actions (T-P1-007, spec §5.6).
//!
//! # What this is and is not
//!
//! A funscript is a JSON file that describes a video's motion over time as a
//! list of timestamped actions. This module DISCOVERS it next to a video,
//! PARSES it into typed actions, and carries the script metadata
//! (stash-box #851).
//!
//! It does not do playback. Sync is Phase 6; a funscript that has been parsed
//! and stored is a funscript a Phase 6 player can use, and nothing here knows
//! about a video player.
//!
//! # Why the format is lenient on read and strict on write
//!
//! Real funscripts in the wild are not all the same shape:
//!
//! - the version field is sometimes a number, sometimes a string
//! - `actions` is sometimes absent, sometimes null, sometimes an empty array
//! - a timestamp is sometimes milliseconds, sometimes a float
//! - extra keys appear that the format does not define
//!
//! Refusing those is refusing a user's own library. So the parser accepts all
//! of them and reports what it found, including when it had to drop a
//! malformed action rather than failing the whole file. What it does NOT do is
//! silently accept a file and produce an empty timeline, because "this video
//! has no funscript" and "this funscript is unreadable" are different facts and
//! the user needs to be told which one is true.
//!
//! Every action is clamped to the video's duration when one is known, and the
//! clamp is counted: a script running past the end of the video is a real
//! authoring mistake and hiding it means the last few seconds of a video are
//! dead. [`Funscript::clamped_actions`] reports how many, so the UI can say so.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Path, PathBuf};

/// One action: a point on a 0..=1 axis at a point in time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Action {
    /// Milliseconds from the start. `u64`, so a negative timestamp from a
    /// corrupt file is a parse error rather than a wrapped enormous number
    /// that sorts to the end of the timeline.
    pub at_ms: u64,
    /// Position along the axis, 0.0 to 1.0 inclusive.
    pub position: f32,
}

impl Action {
    /// An action, with the position clamped into 0..=1.
    ///
    /// Clamping rather than rejecting: a script with a position of 1.02 is
    /// off by two hundredths and the user's intent is unambiguous, while a
    /// script with a position of 40 is corrupt and the caller should hear
    /// about it. The position is clamped and the caller can detect a far
    /// outlier with [`Action::is_wildly_out_of_range`].
    pub fn new(at_ms: u64, position: f32) -> Self {
        Self {
            at_ms,
            position: position.clamp(0.0, 1.0),
        }
    }

    /// Whether the position was far enough outside 0..=1 to suggest a corrupt
    /// file rather than a rounding slip.
    pub fn is_wildly_out_of_range(&self) -> bool {
        !(-0.5..=1.5).contains(&self.position)
    }

    /// Whether this action is at or past `ms`.
    pub fn is_at_or_after(&self, ms: u64) -> bool {
        self.at_ms >= ms
    }
}

/// A named axis of motion.
///
/// A funscript is not one-dimensional. Multi-axis interactive scenes (#6339) is
/// why: the platform models an item as having N named axes, and a two-axis
/// overlay and an N-axis controller are both renderings of the same thing. So
/// the axis set is data, not a fixed `position` field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Axis {
    /// The axis name as the script gives it, e.g. "stroke", "move", or a
    /// device-specific name. NOT normalised: device names are meaningful to
    /// the user and a mapping table is Phase 6's problem.
    pub name: String,
    pub actions: Vec<Action>,
}

impl Axis {
    /// The number of actions, which is what a UI shows as the script's size.
    pub fn len(&self) -> usize {
        self.actions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }

    /// The action at or after `ms`, for a player seeking to a moment.
    ///
    /// Binary search, because a player calls this every frame and a linear
    /// scan over a 20,000-action script at 60Hz is 1.2 million comparisons a
    /// second for no reason. `partition_point` gives the upper bound, and the
    /// action to apply is the one BEFORE it -- a script is a step function, and
    /// at t=0 with actions at 0 and 500, the value at t=0 is the first action.
    pub fn action_at(&self, ms: u64) -> Option<&Action> {
        let idx = self.actions.partition_point(|a| a.at_ms <= ms);
        idx.checked_sub(1).map(|i| &self.actions[i])
    }

    /// Drop actions whose timestamps are not strictly increasing.
    ///
    /// Required before any binary search: `partition_point` on an unsorted
    /// slice returns an arbitrary index rather than an error, so an unsorted
    /// script would seek to the wrong place with no diagnostic. This is
    /// [`Funscript::normalised`], and it is not optional.
    pub fn is_sorted(&self) -> bool {
        self.actions.windows(2).all(|w| w[0].at_ms < w[1].at_ms)
    }

    /// Remove actions at or past `duration_ms`, returning how many went.
    pub fn clamp_to_duration(&mut self, duration_ms: u64) -> usize {
        let before = self.actions.len();
        self.actions.retain(|a| a.at_ms < duration_ms);
        before - self.actions.len()
    }

    /// The time span the axis actually covers, or zero when it has no
    /// actions. A script that is all at t=0 covers nothing, and a UI showing
    /// "0:00 duration" for it is telling the truth.
    pub fn span_ms(&self) -> u64 {
        match (self.actions.first(), self.actions.last()) {
            (Some(f), Some(l)) => l.at_ms.saturating_sub(f.at_ms),
            _ => 0,
        }
    }
}

/// Script-level metadata (stash-box #851).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct FunscriptMetadata {
    /// The title from the script, if it has one.
    pub title: Option<String>,
    /// Free-form author credit.
    pub author: Option<String>,
    /// The script's own version field, kept as text because it arrives as both
    /// a number and a string and the caller should not have to care.
    pub version: Option<String>,
    /// Where the script came from, so a user can tell a sidecar they dropped
    /// next to the video from one a plugin fetched.
    pub source: FunscriptSource,
}

/// Provenance of a funscript. Stored, because a user debugging "why is this
/// moving like that" needs to know whether Commons is reading their file or a
/// downloaded one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum FunscriptSource {
    /// A file sitting next to the video, the common case.
    #[default]
    Sidecar,
    /// A file inside a `*.funscript/` directory next to the video.
    Directory,
    /// Supplied by a plugin or the user rather than found on disk.
    Provided,
}

impl fmt::Display for FunscriptSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            FunscriptSource::Sidecar => "sidecar",
            FunscriptSource::Directory => "directory",
            FunscriptSource::Provided => "provided",
        })
    }
}

/// A parsed funscript.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Funscript {
    pub metadata: FunscriptMetadata,
    pub axes: Vec<Axis>,
    /// What the parser had to do to make sense of the file, so a user is
    /// told rather than silently handed a short timeline.
    pub warnings: Vec<FunscriptWarning>,
}

/// Something wrong with a script that did not stop it being usable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FunscriptWarning {
    /// No `actions` key at all. The file parsed; it is just not a script.
    NoActionsKey,
    /// `actions` was present and null. Same outcome, different cause, and the
    /// cause is what a user needs in order to fix their exporter.
    NullActions,
    /// An action was dropped because it was malformed.
    MalformedActions(usize),
    /// Actions were found but not in increasing time order, so they were
    /// sorted. This one matters: an unsorted script makes a binary search
    /// return an arbitrary index.
    ReorderedActions,
    /// Actions past the end of the video were dropped.
    ClampedToDuration(usize),
    /// A position was far outside 0..=1 and was clamped, which suggests the
    /// file is not really a funscript.
    WildPositions(usize),
    /// More than one axis, which is a multi-axis script (#6339) and is not an
    /// error but changes what the UI can render.
    MultiAxis(usize),
    /// Keys the format does not define, which are preserved and ignored.
    UnknownKeys(usize),
}

impl fmt::Display for FunscriptWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FunscriptWarning::NoActionsKey => f.write_str("the script has no actions key"),
            FunscriptWarning::NullActions => f.write_str("the script's actions are null"),
            FunscriptWarning::MalformedActions(n) => {
                write!(f, "{n} actions were malformed and were dropped")
            }
            FunscriptWarning::ReorderedActions => {
                f.write_str("the actions were not in time order and were sorted")
            }
            FunscriptWarning::ClampedToDuration(n) => {
                write!(f, "{n} actions past the end of the video were dropped")
            }
            FunscriptWarning::WildPositions(n) => write!(
                f,
                "{n} actions had a position outside the normal range and were clamped"
            ),
            FunscriptWarning::MultiAxis(n) => {
                write!(f, "the script has {n} axes, so it is a multi-axis script")
            }
            FunscriptWarning::UnknownKeys(n) => {
                write!(f, "{n} keys are not part of the format and were ignored")
            }
        }
    }
}

impl Funscript {
    /// Parse a funscript from JSON text.
    ///
    /// Returns an error only when the text is not JSON at all or is not an
    /// object. Everything else becomes a [`FunscriptWarning`], because a
    /// script with a bad action is still a script and a user with one malformed
    /// action should not lose the other nineteen thousand.
    pub fn parse(text: &str) -> Result<Self, FunscriptError> {
        let value: serde_json::Value =
            serde_json::from_str(text).map_err(|e| FunscriptError::NotJson(e.to_string()))?;
        let obj = value.as_object().ok_or(FunscriptError::NotAnObject)?;
        Self::from_value(obj)
    }

    fn from_value(
        obj: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Self, FunscriptError> {
        let mut warnings = Vec::new();

        // version: number or string or absent.
        let version = match obj.get("version") {
            Some(serde_json::Value::String(s)) => Some(s.clone()),
            Some(serde_json::Value::Number(n)) => Some(n.to_string()),
            _ => None,
        };

        let mut axes = Vec::new();

        // A multi-axis script puts its axes under "axes"; a single-axis one
        // puts actions at the top level. Both are real, and the difference is
        // the shape, not the version.
        if let Some(serde_json::Value::Array(list)) = obj.get("axes") {
            warnings.push(FunscriptWarning::MultiAxis(list.len()));
            for (i, entry) in list.iter().enumerate() {
                let name = entry
                    .get("name")
                    .and_then(|n| n.as_str())
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("axis{i}"));
                let (actions, mut w) = parse_actions(entry.get("actions"));
                warnings.append(&mut w);
                axes.push(Axis { name, actions });
            }
        } else {
            let (actions, mut w) = parse_actions(obj.get("actions"));
            warnings.append(&mut w);
            if !actions.is_empty() {
                axes.push(Axis {
                    name: "position".to_string(),
                    actions,
                });
            }
        }

        // Count keys the format does not define.
        let known = [
            "version",
            "actions",
            "axes",
            "title",
            "author",
            "description",
            "name",
        ];
        let unknown = obj.keys().filter(|k| !known.contains(&k.as_str())).count();
        if unknown > 0 {
            warnings.push(FunscriptWarning::UnknownKeys(unknown));
        }

        let mut script = Funscript {
            metadata: FunscriptMetadata {
                title: obj
                    .get("title")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                author: obj
                    .get("author")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                version,
                source: FunscriptSource::Sidecar,
            },
            axes,
            warnings,
        };
        script.normalise();
        Ok(script)
    }

    /// Sort the axes' actions, clamp wild positions, and record what it did.
    ///
    /// Not optional and not separate: a script that has not been through this
    /// makes [`Axis::action_at`] return an arbitrary result, and the caller
    /// cannot tell. So every path that produces a `Funscript` calls it.
    pub fn normalise(&mut self) {
        let mut wild = 0;
        let mut reordered = false;
        for axis in &mut self.axes {
            for a in &mut axis.actions {
                // Clamp UNCONDITIONALLY: the axis is defined as 0..=1 and a
                // value outside it is not a position a device can use. The
                // warning is conditional, because a 1.02 is a rounding slip
                // and a 40 is a broken file, and conflating them means one of
                // the two goes unreported.
                let wild_here = a.is_wildly_out_of_range();
                if wild_here {
                    wild += 1;
                }
                a.position = a.position.clamp(0.0, 1.0);
            }
            if !axis.is_sorted() {
                reordered = true;
                axis.actions.sort_by_key(|a| a.at_ms);
                // A stable sort leaves equal timestamps in file order, which
                // is the only sensible choice: two actions at the same instant
                // are the script contradicting itself, and which wins is
                // arbitrary either way.
            }
        }
        if reordered {
            self.warnings.push(FunscriptWarning::ReorderedActions);
        }
        if wild > 0 {
            self.warnings.push(FunscriptWarning::WildPositions(wild));
        }
    }

    /// Drop actions past the video's end, recording how many.
    ///
    /// A `duration_ms` of zero is treated as UNKNOWN and does nothing. Zero is
    /// how a kind with no runtime -- a comic, an image, a video whose duration
    /// was never probed -- reports itself, and clamping to it would erase the
    /// whole script. Treating "unknown" as "empty" is the failure mode worth
    /// naming: the user would find their funscript silently gone.
    pub fn clamp_to_duration(&mut self, duration_ms: u64) {
        if duration_ms == 0 {
            return;
        }
        let mut dropped = 0;
        for axis in &mut self.axes {
            dropped += axis.clamp_to_duration(duration_ms);
        }
        if dropped > 0 {
            self.warnings
                .push(FunscriptWarning::ClampedToDuration(dropped));
        }
    }

    /// The actions clamped to a duration, as a fresh script. This is the
    /// function a Phase 6 player calls, and the fact that it does NOT mutate
    /// is the point: the stored script keeps the author's full timeline, and
    /// the player gets a view of it.
    pub fn clamped_actions(&self, duration_ms: u64) -> Vec<(&str, Vec<Action>)> {
        self.axes
            .iter()
            .map(|a| {
                (
                    a.name.as_str(),
                    a.actions
                        .iter()
                        .filter(|x| x.at_ms < duration_ms)
                        .cloned()
                        .collect(),
                )
            })
            .collect()
    }

    /// The first axis, which is the one a single-axis player uses.
    pub fn primary(&self) -> Option<&Axis> {
        self.axes.first()
    }

    /// The number of axes, for the N-axis controller of #6339.
    pub fn axis_count(&self) -> usize {
        self.axes.len()
    }

    /// The total number of actions across every axis.
    pub fn action_count(&self) -> usize {
        self.axes.iter().map(Axis::len).sum()
    }

    /// The time the script covers, or zero when it covers nothing.
    pub fn span_ms(&self) -> u64 {
        self.axes.iter().map(Axis::span_ms).max().unwrap_or(0)
    }

    /// Whether the script is usable: it has at least one action somewhere.
    pub fn is_usable(&self) -> bool {
        self.action_count() > 0
    }
}

/// Parse one `actions` value, tolerating the shapes real files use.
fn parse_actions(v: Option<&serde_json::Value>) -> (Vec<Action>, Vec<FunscriptWarning>) {
    let mut warnings = Vec::new();
    let mut out = Vec::new();
    match v {
        None => {
            warnings.push(FunscriptWarning::NoActionsKey);
            (out, warnings)
        }
        Some(serde_json::Value::Null) => {
            warnings.push(FunscriptWarning::NullActions);
            (out, warnings)
        }
        Some(serde_json::Value::Array(list)) => {
            let mut malformed = 0;
            for entry in list {
                match parse_action(entry) {
                    Some(a) => out.push(a),
                    None => malformed += 1,
                }
            }
            if malformed > 0 {
                warnings.push(FunscriptWarning::MalformedActions(malformed));
            }
            (out, warnings)
        }
        // A string where an array belongs: not a script. Report the same way as
        // null rather than panicking on the wrong variant.
        Some(_) => {
            warnings.push(FunscriptWarning::NullActions);
            (out, warnings)
        }
    }
}

/// One action, tolerating the field names real exporters use.
fn parse_action(v: &serde_json::Value) -> Option<Action> {
    let obj = v.as_object()?;
    // `at` is the funscript field. Some exporters write `at_ms`, and some
    // write `pos`. All three mean the same thing and refusing two of them
    // refuses a user's own library.
    let at = obj
        .get("at")
        .or_else(|| obj.get("at_ms"))
        .or_else(|| obj.get("time"))?;
    // A negative or non-numeric timestamp is a parse failure, not a clamp:
    // -1 ms is not "just before zero", it is a broken file, and coercing it
    // to 0 would put the action at the wrong end of the timeline.
    let at_ms = at.as_f64()?;
    if !at_ms.is_finite() || at_ms < 0.0 {
        return None;
    }
    let pos = obj
        .get("pos")
        .or_else(|| obj.get("position"))
        .and_then(|p| p.as_f64())?;
    if !pos.is_finite() {
        return None;
    }
    // Note the position is NOT clamped here. parse_action keeps the raw value
    // so normalise() can see how many were wild and report it; clamping at
    // construction would make the count always zero and the warning a lie.
    Some(Action {
        at_ms: at_ms.round() as u64,
        position: pos as f32,
    })
}

/// Why a funscript could not be read at all.
#[derive(Debug, thiserror::Error)]
pub enum FunscriptError {
    #[error("the funscript is not valid JSON: {0}")]
    NotJson(String),
    #[error("the funscript is valid JSON but not an object")]
    NotAnObject,
}

/// Find the funscript for a video, if there is one.
///
/// Two conventions, both real, checked in order:
///
/// 1. `<video>.funscript` next to the video.
/// 2. `<video-basename>.funscript/` as a DIRECTORY next to the video, which
///    is what multi-axis scripts use. Any file inside counts, the first in
///    sorted order, because a directory of axes has no canonical single file
///    and picking by name is at least predictable.
///
/// The extension match is case-insensitive: `.Funscript` and `.funscript` are
/// the same thing to a user, and the filesystem is case-preserving but not
/// case-guaranteed across the platforms Commons runs on.
pub fn discover(video: &Path) -> Option<(PathBuf, FunscriptSource)> {
    // The sidecar check is case-insensitive on both the extension and the
    // stem, because the filesystem is case-preserving but not case-guaranteed
    // across the platforms Commons runs on, and `.FUNSCRIPT` is the same file
    // to the user who made it.
    if let Some(p) = find_case_insensitive(video, ".funscript") {
        return Some((p, FunscriptSource::Sidecar));
    }

    // The directory form strips the video's own extension first, so
    // `clip.mp4` -> `clip.funscript/`, and only then tries a case-insensitive
    // match for the directory name.
    let stem = video.file_stem()?;
    let parent = video.parent().unwrap_or(Path::new("."));
    let want = format!("{}.funscript", stem.to_string_lossy());

    // Exact directory first, then case-insensitive.
    let exact = parent.join(&want);
    if exact.is_dir() {
        return first_in_dir(&exact).map(|f| (f, FunscriptSource::Directory));
    }
    let entries = std::fs::read_dir(parent).ok()?;
    for e in entries.flatten() {
        let p = e.path();
        if !p.is_dir() {
            continue;
        }
        let name = p.file_name()?.to_string_lossy();
        if name.eq_ignore_ascii_case(&want) {
            return first_in_dir(&p).map(|f| (f, FunscriptSource::Directory));
        }
    }
    None
}

/// Find `<video><suffix>` next to the video, matching case-insensitively.
///
/// Returns the real path on disk, not the reconstructed one, so the caller
/// opens the file the user actually has rather than a path that only differs
/// in case and may not exist on a case-sensitive filesystem.
fn find_case_insensitive(video: &Path, suffix: &str) -> Option<PathBuf> {
    // The exact match first: it is the overwhelmingly common case and it
    // avoids a readdir on every scan.
    let mut exact = video.as_os_str().to_os_string();
    exact.push(suffix);
    let exact = PathBuf::from(exact);
    if exact.is_file() {
        return Some(exact);
    }

    let (dir, want) = match (video.parent(), video.file_name()) {
        (Some(d), Some(n)) => (d, format!("{}{suffix}", n.to_string_lossy())),
        _ => return None,
    };
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        if !p.is_file() {
            continue;
        }
        if p.file_name()
            .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(&want))
        {
            return Some(p);
        }
    }
    None
}

/// The first file in a directory, in name order. Name order rather than
/// readdir order because readdir order is filesystem-dependent and a script
/// that resolves to a different axis on ext4 than on btrfs is a bug.
fn first_in_dir(dir: &Path) -> Option<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    files.sort();
    files.into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIMPLE: &str = r#"{
        "version": "1.0",
        "actions": [
            {"at": 0, "pos": 0.0},
            {"at": 1000, "pos": 1.0},
            {"at": 2000, "pos": 0.0}
        ]
    }"#;

    // ---- parsing ----

    #[test]
    fn a_simple_script_parses_to_one_axis_of_three_actions() {
        let s = Funscript::parse(SIMPLE).unwrap();
        assert_eq!(s.axis_count(), 1);
        assert_eq!(s.action_count(), 3);
        assert_eq!(s.primary().unwrap().name, "position");
        assert!(s.is_usable());
        assert!(s.warnings.is_empty(), "{:?}", s.warnings);
    }

    #[test]
    fn metadata_is_read_from_the_top_level() {
        let s = Funscript::parse(
            r#"{"version": 2, "title": "A title", "author": "someone",
                "actions": [{"at":0,"pos":0}]}"#,
        )
        .unwrap();
        assert_eq!(s.metadata.title.as_deref(), Some("A title"));
        assert_eq!(s.metadata.author.as_deref(), Some("someone"));
        // A version can arrive as a number or a string; both become text so a
        // caller does not have to handle two types for one field.
        assert_eq!(s.metadata.version.as_deref(), Some("2"));
    }

    // ---- leniency: the shapes real files have ----

    #[test]
    fn a_missing_actions_key_is_a_warning_not_an_error() {
        let s = Funscript::parse(r#"{"version":"1.0"}"#).unwrap();
        assert!(!s.is_usable());
        assert_eq!(s.warnings, vec![FunscriptWarning::NoActionsKey]);
    }

    /// null actions and a missing key are the same OUTCOME and different
    /// CAUSES, and the cause is what tells a user their exporter is broken.
    #[test]
    fn null_actions_is_distinguished_from_a_missing_key() {
        let s = Funscript::parse(r#"{"actions": null}"#).unwrap();
        assert_eq!(s.warnings, vec![FunscriptWarning::NullActions]);
    }

    #[test]
    fn an_empty_actions_array_is_a_valid_empty_script_with_no_warning() {
        let s = Funscript::parse(r#"{"actions": []}"#).unwrap();
        assert!(!s.is_usable());
        assert!(
            s.warnings.is_empty(),
            "an empty script is not a malformed one: {:?}",
            s.warnings
        );
    }

    #[test]
    fn the_alternative_field_names_are_all_accepted() {
        let s = Funscript::parse(
            r#"{"actions":[{"at":100,"pos":0.5},{"at_ms":200,"position":0.6},{"time":300,"pos":0.7}]}"#,
        )
        .unwrap();
        assert_eq!(s.action_count(), 3, "{:?}", s.warnings);
        assert_eq!(s.primary().unwrap().actions[0].at_ms, 100);
        assert_eq!(s.primary().unwrap().actions[2].at_ms, 300);
    }

    #[test]
    fn a_float_timestamp_rounds_to_the_nearest_millisecond() {
        let s = Funscript::parse(r#"{"actions":[{"at":100.4,"pos":0.0},{"at":100.6,"pos":1.0}]}"#)
            .unwrap();
        let a = &s.primary().unwrap().actions;
        assert_eq!(a[0].at_ms, 100);
        assert_eq!(a[1].at_ms, 101);
    }

    /// One bad action must not cost the user the other nineteen thousand. The
    /// whole point of the warning is that the script is still usable.
    #[test]
    fn a_malformed_action_is_dropped_and_the_rest_survive() {
        let s = Funscript::parse(
            r#"{"actions":[
                {"at":0,"pos":0.0},
                {"at":-5,"pos":0.5},
                {"at":2000,"pos":1.0},
                {"at":"soon","pos":0.5},
                {"pos":0.5},
                {"at":3000}
            ]}"#,
        )
        .unwrap();
        assert_eq!(s.action_count(), 2, "the two good ones survived");
        assert!(
            s.warnings.contains(&FunscriptWarning::MalformedActions(4)),
            "{:?}",
            s.warnings
        );
    }

    /// A negative timestamp is a broken file, not "just before zero".
    #[test]
    fn a_negative_timestamp_is_rejected_rather_than_wrapped() {
        let s = Funscript::parse(r#"{"actions":[{"at":-1,"pos":0.0}]}"#).unwrap();
        assert_eq!(s.action_count(), 0, "it must not become 0 or a huge number");
        assert!(s.warnings.contains(&FunscriptWarning::MalformedActions(1)));
    }

    #[test]
    fn unknown_keys_are_counted_and_the_script_still_parses() {
        let s = Funscript::parse(r#"{"actions":[{"at":0,"pos":0.0}],"weird":1,"alsoWeird":2}"#)
            .unwrap();
        assert_eq!(s.action_count(), 1);
        assert!(s.warnings.contains(&FunscriptWarning::UnknownKeys(2)));
    }

    // ---- the shape that is not JSON ----

    #[test]
    fn text_that_is_not_json_is_an_error() {
        assert!(matches!(
            Funscript::parse("not json at all"),
            Err(FunscriptError::NotJson(_))
        ));
    }

    #[test]
    fn json_that_is_not_an_object_is_an_error() {
        assert!(matches!(
            Funscript::parse("[1,2,3]"),
            Err(FunscriptError::NotAnObject)
        ));
        assert!(matches!(
            Funscript::parse("42"),
            Err(FunscriptError::NotAnObject)
        ));
    }

    /// Actions as a bare string is a wrong shape, not a null. It is reported
    /// rather than panicking on the wrong variant.
    #[test]
    fn actions_of_the_wrong_type_do_not_panic() {
        let s = Funscript::parse(r#"{"actions": "nope"}"#).unwrap();
        assert!(!s.is_usable());
        assert_eq!(s.warnings, vec![FunscriptWarning::NullActions]);
    }

    // ---- multi-axis (#6339) ----

    #[test]
    fn a_multi_axis_script_parses_to_named_axes() {
        let s = Funscript::parse(
            r#"{"axes":[
                {"name":"stroke","actions":[{"at":0,"pos":0.0},{"at":500,"pos":1.0}]},
                {"name":"move","actions":[{"at":0,"pos":0.5}]}
            ]}"#,
        )
        .unwrap();
        assert_eq!(s.axis_count(), 2);
        assert_eq!(s.axes[0].name, "stroke");
        assert_eq!(s.axes[0].len(), 2);
        assert_eq!(s.axes[1].name, "move");
        assert!(s.warnings.contains(&FunscriptWarning::MultiAxis(2)));
    }

    /// An axis with no name gets a positional one rather than being dropped,
    /// because a nameless axis is still an axis a device may use.
    #[test]
    fn an_unnamed_axis_gets_a_positional_name() {
        let s = Funscript::parse(r#"{"axes":[{"actions":[{"at":0,"pos":0.0}]}]}"#).unwrap();
        assert_eq!(s.axis_count(), 1);
        assert_eq!(s.axes[0].name, "axis0");
    }

    /// A multi-axis script with one empty axis is still two axes. Dropping it
    /// would make the N-axis controller show a device the script has.
    #[test]
    fn an_empty_axis_is_kept_because_it_is_still_an_axis() {
        let s = Funscript::parse(
            r#"{"axes":[{"name":"a","actions":[{"at":0,"pos":0.0}]},{"name":"b","actions":[]}]}"#,
        )
        .unwrap();
        assert_eq!(s.axis_count(), 2);
        assert!(s.axes[1].is_empty());
    }

    // ---- normalisation: the property that makes seeking correct ----

    /// The reason `normalise` is not optional. `partition_point` on an unsorted
    /// slice returns an arbitrary index with no error, so a player seeking
    /// would land on the wrong action and nothing would say so.
    #[test]
    fn out_of_order_actions_are_sorted_so_seeking_works() {
        let s = Funscript::parse(
            r#"{"actions":[{"at":2000,"pos":0.0},{"at":0,"pos":1.0},{"at":1000,"pos":0.5}]}"#,
        )
        .unwrap();
        let a = &s.primary().unwrap().actions;
        assert!(s.primary().unwrap().is_sorted(), "{a:?}");
        assert_eq!(
            a.iter().map(|x| x.at_ms).collect::<Vec<_>>(),
            vec![0, 1000, 2000]
        );
        assert!(s.warnings.contains(&FunscriptWarning::ReorderedActions));
    }

    #[test]
    fn an_already_sorted_script_is_not_reported_as_reordered() {
        let s = Funscript::parse(SIMPLE).unwrap();
        assert!(!s.warnings.contains(&FunscriptWarning::ReorderedActions));
    }

    /// A position of 40 is a corrupt file, not a rounding slip, and is
    /// clamped so the script is still usable -- with a warning, because silent
    /// clamping of a wild value hides a real problem.
    #[test]
    fn a_wild_position_is_clamped_and_reported() {
        let s =
            Funscript::parse(r#"{"actions":[{"at":0,"pos":40.0},{"at":1,"pos":-30.0}]}"#).unwrap();
        let a = &s.primary().unwrap().actions;
        assert_eq!(a[0].position, 1.0);
        assert_eq!(a[1].position, 0.0);
        assert!(s.warnings.contains(&FunscriptWarning::WildPositions(2)));
    }

    #[test]
    fn a_position_just_outside_the_range_is_clamped_without_a_warning() {
        // 1.02 is a rounding slip and the intent is unambiguous.
        let s = Funscript::parse(r#"{"actions":[{"at":0,"pos":1.02}]}"#).unwrap();
        assert_eq!(s.primary().unwrap().actions[0].position, 1.0);
        assert!(!s.warnings.contains(&FunscriptWarning::WildPositions(1)));
    }

    // ---- seeking ----

    /// A script is a step function: the value at t is the LAST action at or
    /// before t, and before the first action there is no value. The second
    /// half is the part a naive implementation gets wrong by clamping to index
    /// 0, which makes a script start moving before it says to.
    #[test]
    fn a_seek_returns_the_last_action_at_or_before_the_moment() {
        let s = Funscript::parse(SIMPLE).unwrap();
        let a = s.primary().unwrap();
        assert_eq!(a.action_at(0).unwrap().at_ms, 0);
        assert_eq!(a.action_at(999).unwrap().at_ms, 0, "still the first action");
        assert_eq!(a.action_at(1000).unwrap().at_ms, 1000);
        assert_eq!(a.action_at(1999).unwrap().at_ms, 1000);
        assert_eq!(a.action_at(2000).unwrap().at_ms, 2000);
        assert_eq!(a.action_at(999_999).unwrap().at_ms, 2000, "held to the end");
    }

    #[test]
    fn a_seek_before_the_first_action_has_no_value() {
        let s = Funscript::parse(r#"{"actions":[{"at":500,"pos":1.0}]}"#).unwrap();
        let a = s.primary().unwrap();
        assert!(a.action_at(0).is_none(), "the script has not started yet");
        assert!(a.action_at(499).is_none());
        assert!(a.action_at(500).is_some());
    }

    #[test]
    fn seeking_an_empty_axis_returns_nothing() {
        let s = Funscript::parse(r#"{"axes":[{"name":"a","actions":[]}]}"#).unwrap();
        assert!(s.axes[0].action_at(0).is_none());
        assert!(s.axes[0].action_at(u64::MAX).is_none());
    }

    /// The property that makes the binary search correct, over a large
    /// generated script: a seek must return the same answer as a linear scan.
    #[test]
    fn seeking_agrees_with_a_linear_scan_over_a_large_script() {
        let actions: Vec<String> = (0..5_000)
            .map(|i| format!(r#"{{"at":{},"pos":{}}}"#, i * 10, (i % 100) as f32 / 100.0))
            .collect();
        let text = format!(r#"{{"actions":[{}]}}"#, actions.join(","));
        let s = Funscript::parse(&text).unwrap();
        let a = s.primary().unwrap();
        assert_eq!(a.len(), 5_000);

        for probe in [0u64, 1, 9, 10, 11, 123, 4_999, 49_990, 49_999, 1_000_000] {
            let expect = a
                .actions
                .iter()
                .rev()
                .find(|x| x.at_ms <= probe)
                .map(|x| x.at_ms);
            assert_eq!(
                a.action_at(probe).map(|x| x.at_ms),
                expect,
                "seek at {probe}"
            );
        }
    }

    // ---- duration clamping ----

    /// A script running past the end of the video is a real authoring mistake,
    /// and the count is what lets the UI say so rather than the last few
    /// seconds of the video being silently dead.
    #[test]
    fn actions_past_the_end_of_the_video_are_dropped_and_counted() {
        let mut s = Funscript::parse(SIMPLE).unwrap();
        s.clamp_to_duration(1_500);
        assert_eq!(s.action_count(), 2, "the 2000ms action is gone");
        assert!(s.warnings.contains(&FunscriptWarning::ClampedToDuration(1)));
    }

    /// The function a Phase 6 player calls, and the reason it does not mutate:
    /// the stored script keeps the author's full timeline.
    #[test]
    fn the_clamped_view_does_not_mutate_the_stored_script() {
        let s = Funscript::parse(SIMPLE).unwrap();
        let before = s.action_count();
        let view = s.clamped_actions(1_500);
        assert_eq!(view[0].1.len(), 2, "the view is clamped");
        assert_eq!(s.action_count(), before, "the stored script is not");
        assert!(!s.warnings.contains(&FunscriptWarning::ClampedToDuration(1)));
    }

    #[test]
    fn clamping_to_a_duration_past_the_end_drops_nothing() {
        let mut s = Funscript::parse(SIMPLE).unwrap();
        s.clamp_to_duration(10_000);
        assert_eq!(s.action_count(), 3);
        assert!(!s.warnings.contains(&FunscriptWarning::ClampedToDuration(1)));
    }

    /// A zero duration is "not applicable" and would drop every action. That is
    /// the wrong answer for a video whose duration was not probed, so a zero
    /// clamp is a no-op rather than a wipe.
    #[test]
    fn clamping_to_zero_duration_is_a_no_op_rather_than_a_wipe() {
        let mut s = Funscript::parse(SIMPLE).unwrap();
        s.clamp_to_duration(0);
        assert_eq!(
            s.action_count(),
            3,
            "an unknown duration must not erase the script"
        );
    }

    // ---- spans and metadata ----

    #[test]
    fn a_span_is_the_time_the_script_actually_covers() {
        let s = Funscript::parse(SIMPLE).unwrap();
        assert_eq!(s.span_ms(), 2_000);
    }

    /// A script whose actions are all at t=0 covers nothing, and reporting
    /// zero is the truth rather than a bug.
    #[test]
    fn a_script_with_every_action_at_zero_covers_nothing() {
        let s = Funscript::parse(r#"{"actions":[{"at":0,"pos":0.0},{"at":0,"pos":1.0}]}"#).unwrap();
        assert_eq!(s.span_ms(), 0);
    }

    #[test]
    fn an_empty_script_covers_nothing() {
        let s = Funscript::parse(r#"{"actions":[]}"#).unwrap();
        assert_eq!(s.span_ms(), 0);
        // No actions means no axis: there is no timeline to seek in, and
        // inventing an empty one would make `primary()` return something a
        // player would then have to special-case.
        assert!(s.primary().is_none());
        assert_eq!(s.action_count(), 0);
        assert_eq!(s.axis_count(), 0);
    }

    #[test]
    fn warnings_render_as_sentences() {
        let all = [
            FunscriptWarning::NoActionsKey,
            FunscriptWarning::NullActions,
            FunscriptWarning::MalformedActions(3),
            FunscriptWarning::ReorderedActions,
            FunscriptWarning::ClampedToDuration(2),
            FunscriptWarning::WildPositions(1),
            FunscriptWarning::MultiAxis(2),
            FunscriptWarning::UnknownKeys(4),
        ];
        let mut seen = Vec::new();
        for w in all {
            let s = w.to_string();
            assert!(!s.is_empty());
            assert!(!seen.contains(&s), "two warnings render the same: {s:?}");
            seen.push(s);
        }
    }

    #[test]
    fn a_source_renders_for_display() {
        assert_eq!(FunscriptSource::Sidecar.to_string(), "sidecar");
        assert_eq!(FunscriptSource::Directory.to_string(), "directory");
        assert_eq!(FunscriptSource::Provided.to_string(), "provided");
    }

    // ---- round trip ----

    #[test]
    fn a_script_round_trips_through_json() {
        let s = Funscript::parse(SIMPLE).unwrap();
        let json = serde_json::to_string(&s).unwrap();
        let back: Funscript = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    // ---- discovery ----

    #[test]
    fn a_sidecar_next_to_the_video_is_found() {
        let dir = tempfile::tempdir().unwrap();
        let video = dir.path().join("clip.mp4");
        std::fs::write(&video, b"x").unwrap();
        std::fs::write(dir.path().join("clip.mp4.funscript"), SIMPLE).unwrap();

        let (found, source) = discover(&video).expect("the sidecar is right there");
        assert_eq!(found, dir.path().join("clip.mp4.funscript"));
        assert_eq!(source, FunscriptSource::Sidecar);
    }

    #[test]
    fn a_funscript_directory_is_found_when_there_is_no_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let video = dir.path().join("clip.mp4");
        std::fs::write(&video, b"x").unwrap();
        let d = dir.path().join("clip.funscript");
        std::fs::create_dir(&d).unwrap();
        std::fs::write(d.join("a.json"), SIMPLE).unwrap();

        let (found, source) = discover(&video).unwrap();
        assert_eq!(source, FunscriptSource::Directory);
        assert_eq!(found, d.join("a.json"));
    }

    /// The sidecar wins over the directory when both exist: the file is the
    /// more specific statement of intent.
    #[test]
    fn the_sidecar_wins_over_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        let video = dir.path().join("clip.mp4");
        std::fs::write(&video, b"x").unwrap();
        std::fs::write(dir.path().join("clip.mp4.funscript"), SIMPLE).unwrap();
        let d = dir.path().join("clip.funscript");
        std::fs::create_dir(&d).unwrap();
        std::fs::write(d.join("a.json"), SIMPLE).unwrap();

        let (_, source) = discover(&video).unwrap();
        assert_eq!(source, FunscriptSource::Sidecar);
    }

    /// Readdir order is filesystem-dependent, so a directory of axes must
    /// resolve by NAME. A script that picks a different axis on ext4 than on
    /// btrfs is a bug, and the test cannot see the bug without checking.
    #[test]
    fn a_directory_resolves_by_name_not_by_readdir_order() {
        let dir = tempfile::tempdir().unwrap();
        let video = dir.path().join("clip.mp4");
        std::fs::write(&video, b"x").unwrap();
        let d = dir.path().join("clip.funscript");
        std::fs::create_dir(&d).unwrap();
        for n in ["z.json", "a.json", "m.json"] {
            std::fs::write(d.join(n), SIMPLE).unwrap();
        }
        let (found, _) = discover(&video).unwrap();
        assert_eq!(found, d.join("a.json"), "first in name order");
    }

    /// `.Funscript` and `.funscript` are the same thing to a user, and the
    /// filesystem is case-preserving but not case-guaranteed.
    #[test]
    fn the_sidecar_match_is_case_insensitive() {
        let dir = tempfile::tempdir().unwrap();
        let video = dir.path().join("clip.mp4");
        std::fs::write(&video, b"x").unwrap();
        std::fs::write(dir.path().join("clip.mp4.FUNSCRIPT"), SIMPLE).unwrap();
        let (found, source) = discover(&video).expect("found case-insensitively");
        assert_eq!(source, FunscriptSource::Sidecar);
        assert!(found.to_string_lossy().contains("FUNSCRIPT"));
    }

    #[test]
    fn a_video_with_no_funscript_finds_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let video = dir.path().join("clip.mp4");
        std::fs::write(&video, b"x").unwrap();
        assert!(discover(&video).is_none());
    }

    /// An empty `*.funscript/` directory is not a script. Reporting one would
    /// put a "no funscript" row in with an attached empty script.
    #[test]
    fn an_empty_funscript_directory_is_not_a_script() {
        let dir = tempfile::tempdir().unwrap();
        let video = dir.path().join("clip.mp4");
        std::fs::write(&video, b"x").unwrap();
        std::fs::create_dir(dir.path().join("clip.funscript")).unwrap();
        assert!(discover(&video).is_none());
    }

    /// A file named `clip.funscript.mp4` next to `clip.mp4` must not be
    /// mistaken for a sidecar: the sidecar is `clip.mp4.funscript`.
    #[test]
    fn a_differently_named_file_is_not_a_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let video = dir.path().join("clip.mp4");
        std::fs::write(&video, b"x").unwrap();
        std::fs::write(dir.path().join("clip.funscript.mp4"), SIMPLE).unwrap();
        assert!(discover(&video).is_none());
    }
}
