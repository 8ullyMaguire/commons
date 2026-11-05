//! The trend rule — **which group is arriving, so an event or a bounty can be timed**.
//!
//! # The decision rule, fixed before the data
//!
//! A group is **emerging** when it gained at least [`MIN_RECENT`] arrivals in the recent window *and* its rate
//! is at least [`RATIO`] times its own baseline rate. Both halves matter: the ratio alone calls a group that
//! went 1 → 3 emerging, and the count alone calls the biggest group on the site emerging every week.
//!
//! A group whose baseline is zero and which has arrivals now is [`Verdict::New`], not infinitely emerging — the
//! verdict says so rather than dividing by zero.
//!
//! **No verdict is an answer.** Below [`MIN_RECENT`] arrivals the assessment says *why* it has none. An empty
//! verdict would be read as "not emerging", which is a claim this data cannot support at that sample size.
//!
//! # Why a crate
//!
//! The same rule fits any table with an arrival timestamp: a fiction archive's fandoms, a booru's tags, a video
//! library's studios, a directory's verticals. It is deliberately about *groups of arrivals*, not about any one
//! product's nouns — the caller decides what a group is and what an arrival is, and passes two counts.
//!
//! # The bug this rule exists to avoid
//!
//! Comparing `recent` to `baseline` as raw counts compares two windows of different lengths. Eight arrivals in
//! two weeks is four a week, not eight, and calling that a doubling is how a radar invents a trend. Both sides
//! are divided by their own window length, which is why [`Counts`] carries weeks rather than dates.

use serde::{Deserialize, Serialize};

/// The fewest arrivals in the recent window before any verdict is given.
pub const MIN_RECENT: i64 = 3;

/// How much faster than its own baseline a group must move to be called emerging.
pub const RATIO: f64 = 2.0;

/// What the radar says about one group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Arriving: enough arrivals, and at least [`RATIO`] times the baseline rate.
    Emerging,
    /// The baseline was empty and it has arrivals now.
    New,
    /// Between a half and [`RATIO`] times baseline.
    Steady,
    /// Under half its baseline rate.
    Fading,
    /// Not enough evidence, with the reason in [`Assessment::reason`].
    NoVerdict,
}

impl Verdict {
    /// The word the interface prints.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Emerging => "emerging",
            Self::New => "new",
            Self::Steady => "steady",
            Self::Fading => "fading",
            Self::NoVerdict => "no_verdict",
        }
    }
}

/// One group's counts, as the caller's store reported them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Counts {
    /// Arrivals inside the recent window.
    pub recent: i64,
    /// Arrivals inside the baseline window.
    pub baseline: i64,
    /// How many weeks the baseline covers, so the rate is per-week on both sides.
    pub baseline_weeks: f64,
    /// How many weeks the recent window covers. A ratio between two windows of different lengths is not a
    /// ratio.
    pub recent_weeks: f64,
}

/// The radar's answer for one group.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Assessment {
    /// Recent rate against the baseline rate; `None` when there is no baseline to divide by.
    pub ratio: Option<f64>,
    pub verdict: Verdict,
    /// A sentence for the interface, and the reason a verdict is absent.
    pub reason: String,
}

/// Assess one group.
///
/// # Examples
///
/// ```
/// use commons_trend::{assess, Counts, Verdict};
/// let counts = Counts { recent: 6, baseline: 2, baseline_weeks: 4.0, recent_weeks: 1.0 };
/// assert_eq!(assess(&counts).verdict, Verdict::Emerging);
/// ```
#[must_use]
pub fn assess(counts: &Counts) -> Assessment {
    if counts.recent < MIN_RECENT {
        return Assessment {
            ratio: None,
            verdict: Verdict::NoVerdict,
            reason: format!(
                "{} arrivals in the recent window; {MIN_RECENT} are needed before a ratio means anything",
                counts.recent
            ),
        };
    }
    if counts.baseline_weeks <= 0.0 || counts.recent_weeks <= 0.0 {
        return Assessment {
            ratio: None,
            verdict: Verdict::NoVerdict,
            reason: "no baseline window configured".to_owned(),
        };
    }
    let baseline_rate = counts.baseline as f64 / counts.baseline_weeks;
    if baseline_rate <= 0.0 {
        return Assessment {
            ratio: None,
            verdict: Verdict::New,
            reason: format!(
                "{} arrivals, none in the baseline window — this group did not exist here a window ago",
                counts.recent
            ),
        };
    }
    let recent_rate = counts.recent as f64 / counts.recent_weeks;
    let ratio = recent_rate / baseline_rate;
    let verdict = if ratio >= RATIO {
        Verdict::Emerging
    } else if ratio <= 1.0 / RATIO {
        Verdict::Fading
    } else {
        Verdict::Steady
    };
    Assessment {
        ratio: Some(ratio),
        verdict,
        reason: format!(
            "{ratio:.1}x its baseline rate: {recent_rate:.1} arrivals per week now against \
             {baseline_rate:.1} per baseline week"
        ),
    }
}
