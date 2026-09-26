//! File state, volume state, and the rules that stop a missing volume from
//! turning into a high-CPU loop.
//!
//! # The problem this exists for
//!
//! stash#5683: a user unmounts a drive. The scanner notices the files are
//! gone, and reacts the way a naive scanner reacts -- by looking for them,
//! again, on every scan, forever. Each scan stats every file that used to be
//! there, logs a line per file, and burns a core. The drive is not coming
//! back on its own, so none of that work can ever succeed.
//!
//! The fix is two decisions, and the second is the one that matters:
//!
//! 1. Detect that the *volume* is gone, not that the files are gone. A file
//!    missing from a mounted volume is a deletion. A file missing because
//!    its volume is not mounted is a lie told by the mount table, and the
//!    difference matters enormously -- marking those files deleted cascades
//!    their metadata and thumbnails away, and when the volume comes back
//!    every file on it has to be re-extracted from scratch.
//!
//! 2. Once a volume is known to be missing, stop looking at it. Record that
//!    fact, and the next scan checks one thing -- is the mount point back? --
//!    rather than the number of files that vanished. That is the entire
//!    difference between O(files) per scan and O(1).
//!
//! The [`VolumeTracker`] is therefore the load-bearing type. [`FileState`]
//! is a straightforward enum that would have been fine on its own; it is
//! here because deciding *which* state a file is in requires knowing things
//! about the volume underneath it, which is state the file row does not hold.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

// The `FileState` enum itself is not here. It already exists in
// `commons-core`, defined against the same spec section this ticket cites, and
// a second copy is a second thing to keep in sync -- and a second thing whose
// `as_str` and `parse` will drift from the first, which is the kind of drift
// that shows up as "the API says present, the database says missing".
//
// What lives here is the *policy*: which state a file is in given what we know
// about the volume underneath it, which is not derivable from the enum.

use commons_core::FileState;

/// A state the scanner has never heard of, read from the database.
///
/// An unknown value is `Present`, not an error and never `Missing`. These
/// strings are in the database and in the GraphQL API, so a newer build can
/// write a state an older binary has never seen; the failure mode of guessing
/// wrong in the pessimistic direction is a user who downgrades and finds their
/// library empty.
pub fn parse_file_state(s: &str) -> FileState {
    FileState::parse(s).unwrap_or(FileState::Present)
}

/// Does a bulk operation act on this file by default?
///
/// #314 and the reason `Missing` is excluded: "delete everything in this
/// folder" run against an unmounted drive's worth of files would mark the
/// entire library absent. Skipping is the safe default, and the user is told
/// it happened.
pub fn actionable_by_default(state: FileState) -> bool {
    matches!(state, FileState::Present)
}

// ------------------------------------------------------------------ volumes

/// What the scanner believes about one mount point.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Volume {
    /// The configured root, normalised. This is the identity: a volume is a
    /// path the user configured, not a device name, because the same drive
    /// shows up under different device names and the same device name under
    /// different paths.
    pub root: PathBuf,
    /// The state as of the last scan.
    pub state: VolumeState,
    /// When we last confirmed the volume was there. A backoff timer runs from
    /// this, so it is the number that actually bounds the CPU cost.
    pub last_seen: Option<SystemTime>,
    /// Whether the user asked us to leave this volume's files alone
    /// (stash#314, hot-swap users).
    ///
    /// Opting a volume out of cleanup means a `Missing` volume never causes
    /// files on it to be marked absent at all, even once. Their state is
    /// left exactly as it was.
    pub cleanup_enabled: bool,
    /// Consecutive scans that found the volume missing.
    pub consecutive_misses: u32,
}

impl Volume {
    /// A volume configured and believed present.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Volume {
            root: root.into(),
            state: VolumeState::Mounted,
            last_seen: Some(SystemTime::now()),
            cleanup_enabled: true,
            consecutive_misses: 0,
        }
    }

    /// A volume the user has opted out of cleanup on (stash#314).
    pub fn opted_out(root: impl Into<PathBuf>) -> Self {
        Volume {
            cleanup_enabled: false,
            ..Self::new(root)
        }
    }
}

/// Whether a configured volume is there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VolumeState {
    Mounted,
    /// Not there. The files under it are `Missing`, not deleted.
    Unmounted,
    /// There, but not readable -- a network share that timed out, a drive
    /// reporting I/O errors, a permissions problem on the mount point.
    ///
    /// Separate from `Unmounted` because the right response differs: an
    /// unmounted volume should be left alone, but an unreadable one may be
    /// a mount that is up and not yet healthy, and retrying sooner is
    /// reasonable.
    Unreadable,
}

impl VolumeState {
    /// Can we read files under this volume right now?
    pub fn readable(self) -> bool {
        matches!(self, VolumeState::Mounted)
    }
}

/// What one probe of a volume concluded, and how much it cost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeProbe {
    /// The volume's state after the probe.
    pub state: VolumeState,
    /// `true` if this probe actually touched the filesystem.
    ///
    /// The acceptance test for stash#5683 is a count of filesystem calls, and
    /// the only way that count is meaningful is if backoff probes are marked
    /// as not touching it. So this field is load-bearing for the test, and the
    /// test is load-bearing for the ticket.
    pub touched: bool,
}

/// Decides which volumes to look at, and remembers the ones that are gone.
///
/// # The one-line version
///
/// A mounted volume is probed every scan. A missing one is probed with an
/// exponentially increasing delay, up to a ceiling, so the steady-state cost
/// of an absent drive is one `stat` per backoff period rather than one per
/// file per scan. That is the objective difference between the fixed and
/// broken behaviours, and [`VolumeTracker::stats`] reports it so the test
/// can assert on it.
///
/// # Why a ceiling
///
/// Pure exponential backoff without a ceiling eventually means "never check
/// again", which is correct for CPU and wrong for the user: they reattach the
/// drive and nothing notices for a week. The ceiling bounds the worst case
/// at one probe per period, so recovery is always within `max_backoff` of
/// coming back.
#[derive(Debug, Clone)]
pub struct VolumeTracker {
    volumes: Vec<Volume>,
    /// How long to wait before the first retry of a missing volume.
    pub base_backoff: Duration,
    /// The longest we will ever wait between probes.
    pub max_backoff: Duration,
    /// How many times we have touched the filesystem, in total.
    ///
    /// This is the stash#5683 counter. It is on the tracker rather than in
    /// the test so that production code and the test are counting the same
    /// thing -- a counter the test owns would only prove the test is right.
    probes: u64,
}

impl Default for VolumeTracker {
    fn default() -> Self {
        VolumeTracker::new()
    }
}

impl VolumeTracker {
    /// A tracker with the default backoff: 30 seconds doubling to 1 hour.
    ///
    /// Thirty seconds because a drive that was just unplugged and replugged
    /// should be noticed promptly, and an hour because a drive that is gone
    /// for a week is not coming back in the next minute either.
    pub fn new() -> Self {
        VolumeTracker {
            volumes: Vec::new(),
            base_backoff: Duration::from_secs(30),
            max_backoff: Duration::from_secs(3600),
            probes: 0,
        }
    }

    /// Backoff settings, for tests and for a user who wants faster recovery.
    pub fn with_backoff(mut self, base: Duration, max: Duration) -> Self {
        self.base_backoff = base;
        self.max_backoff = max;
        self
    }

    /// Configure a volume, or add one.
    ///
    /// On an existing volume only `cleanup_enabled` is taken from the
    /// argument. Everything else -- state, miss count, last-seen time -- is an
    /// *observation*, and a config reload is not an observation: the file
    /// saying "this path is configured" is not evidence the drive is plugged
    /// in. Taking the argument's `state` would let a config watcher reset the
    /// backoff every minute, which is stash#5683's loop with a different
    /// trigger.
    pub fn configure(&mut self, volume: Volume) {
        match self.volumes.iter_mut().find(|v| v.root == volume.root) {
            Some(existing) => {
                // The user's decision survives a reload; our observations do not.
                existing.cleanup_enabled = volume.cleanup_enabled;
            }
            None => self.volumes.push(volume),
        }
    }

    /// The configured volumes.
    pub fn volumes(&self) -> &[Volume] {
        &self.volumes
    }

    /// How many filesystem probes this tracker has made. The #5683 counter.
    pub fn stats(&self) -> u64 {
        self.probes
    }

    /// Look at a volume, honouring the backoff.
    ///
    /// Returns `None` when the backoff says not to look yet, which is the
    /// state that makes the whole thing cheap. The caller treats `None` as
    /// "use what we already knew", and so does every code path above it.
    pub fn should_probe(&self, root: &Path, now: SystemTime) -> bool {
        let Some(volume) = self.volumes.iter().find(|v| v.root == root) else {
            // An unconfigured root has no backoff to honour, so look. The
            // caller is expected to `configure` it, and the test that this
            // is a footgun lives in `an_unconfigured_volume_is_probed_at_once`.
            return true;
        };
        if volume.state.readable() {
            return true;
        }
        // Never successfully seen: there is no clock to run the backoff
        // from, so look. A volume that has never been there is a
        // configuration error, and hiding it behind backoff would make it
        // silent.
        let Some(last_seen) = volume.last_seen else {
            return true;
        };
        // A clock that moved backwards reads as a negative duration. Treating
        // that as "probe now" is the safe direction: it costs one stat, and
        // the alternative is a volume that is never checked again.
        now.duration_since(last_seen)
            .map(|since| since >= self.backoff_for(volume.consecutive_misses))
            .unwrap_or(true)
    }

    /// The delay before the next probe of a volume that has missed `n` times.
    ///
    /// Shifting rather than doubling in a loop: a volume missing for a month
    /// has millions of consecutive misses, and `2u64.checked_shl(misses)` is
    /// one instruction where the loop is millions of them. `saturating_shl`
    /// because a shift of 64 or more is not a panic but a zero, and zero would
    /// mean "probe constantly" -- the exact opposite of the intent.
    pub fn backoff_for(&self, misses: u32) -> Duration {
        // `misses` counts the misses *already recorded*, so `backoff_for(1)`
        // is the base delay and the ramp doubles from there. Anchoring on the
        // count rather than a separate step counter means a remount -- which
        // zeroes the count -- puts the volume straight back at the base delay
        // with no second piece of state to keep in sync.
        let base = self.base_backoff.as_secs();
        // `1 << exponent`, not `2 << exponent`: the base already accounts for
        // the first doubling, and shifting a 2 up front made every delay one
        // step longer than documented -- invisible in a test that only checked
        // the ramp's shape, and a real doubling of the wait on a real volume.
        let exponent = misses.saturating_sub(1);
        // `checked_shl` returns None past 63, which is the "already
        // astronomically past the ceiling" case -- clamp to the ceiling
        // rather than treating it as zero, because zero would mean "probe
        // constantly", the exact opposite of the intent.
        let factor = 1u64.checked_shl(exponent).unwrap_or(u64::MAX);
        let shifted = base.saturating_mul(factor);
        Duration::from_secs(shifted.min(self.max_backoff.as_secs())).min(self.max_backoff)
    }

    /// Record a probe result, and return the volume's new state.
    ///
    /// Takes the current time rather than reading the clock. The first
    /// version called `SystemTime::now()` itself, which made the backoff
    /// impossible to test: the recorded time and the time the test asked
    /// about were always a few microseconds apart, so a boundary assertion
    /// passed or failed depending on how long the test took to run.
    pub fn record(&mut self, root: &Path, probe: VolumeProbe, now: SystemTime) -> VolumeState {
        self.probes += 1;
        let Some(volume) = self.volumes.iter_mut().find(|v| v.root == root) else {
            return probe.state;
        };
        volume.state = probe.state;
        // The backoff is measured from the last *probe*, not the last
        // success. Anchoring it to the last success is the obvious reading
        // and it is wrong: a volume that is permanently absent never has
        // another success, so the anchor never moves, `consecutive_misses`
        // grows but the elapsed time does not, and once the elapsed time
        // passes the ceiling every subsequent scan is allowed a probe. The
        // stash#5683's loop, with a one-minute period instead of a per-file
        // stat.
        volume.last_seen = Some(now);
        if probe.state.readable() {
            volume.consecutive_misses = 0;
        } else {
            volume.consecutive_misses = volume.consecutive_misses.saturating_add(1);
        }
        probe.state
    }

    /// Is this volume opted out of cleanup (stash#314)?
    pub fn cleanup_enabled(&self, root: &Path) -> bool {
        self.volumes
            .iter()
            .find(|v| v.root == root)
            .map(|v| v.cleanup_enabled)
            // Unconfigured means opted in, because the default behaviour of
            // a fresh install is to index what it finds.
            .unwrap_or(true)
    }

    /// The files to mark `Missing` because their volume is gone.
    ///
    /// #314: a volume opted out of cleanup yields nothing, even when it is
    /// unmounted. The user's files keep whatever state they had, and the
    /// volume is still tracked, so it still costs one probe per backoff --
    /// opting out of cleanup is not opting out of noticing the drive.
    pub fn files_to_mark_missing<S: AsRef<str>>(
        &self,
        root: &Path,
        file_paths: impl IntoIterator<Item = S>,
    ) -> Vec<String> {
        if !self.cleanup_enabled(root) {
            return Vec::new();
        }
        if self
            .volumes()
            .iter()
            .find(|v| v.root == root)
            .map(|v| v.state.readable())
            .unwrap_or(true)
        {
            return Vec::new();
        }
        file_paths
            .into_iter()
            .map(|p| p.as_ref().to_string())
            .collect()
    }
}

// ------------------------------------------------------------ bulk operations

/// The one-line explanation a bulk operation gives the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BulkOutcome {
    /// What the operation did.
    pub affected: usize,
    /// What it declined to touch, and why. One line, as the ticket asks.
    pub skipped: usize,
    /// The states that were skipped, so the message can name them.
    pub skipped_states: BTreeSet<FileState>,
}

impl BulkOutcome {
    /// A short sentence for a toast or a log line.
    ///
    /// One line, no trailing punctuation games, and it names the reason
    /// because "skipped 40" on its own leaves the user guessing whether they
    /// should be worried.
    pub fn one_line(&self, verb: &str) -> String {
        if self.skipped == 0 {
            return format!("{verb} {} item{}", self.affected, plural(self.affected));
        }
        let reasons = self
            .skipped_states
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "{verb} {} item{}, skipped {} ({reasons})",
            self.affected,
            plural(self.affected),
            self.skipped,
        )
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// Split a selection into the part to act on and the part to skip.
///
/// This is the single place the "skip `Missing` by default" rule is
/// implemented, so every bulk operation gets the same behaviour and the same
/// message rather than each one re-deriving it.
pub fn partition_for_bulk<'a>(
    items: impl IntoIterator<Item = (&'a str, FileState)>,
) -> (Vec<&'a str>, BulkOutcome) {
    let mut actionable = Vec::new();
    let mut skipped_states = BTreeSet::new();
    let mut skipped = 0;
    for (id, state) in items {
        if actionable_by_default(state) {
            actionable.push(id);
        } else {
            skipped += 1;
            skipped_states.insert(state);
        }
    }
    let affected = actionable.len();
    (
        actionable,
        BulkOutcome {
            affected,
            skipped,
            skipped_states,
        },
    )
}

// -------------------------------------------------------------------- leases

/// A claim that a job is still wanted.
///
/// stash#5953: a user deletes a file while the generator is working on it.
/// Without a lease the generator finishes, writes its output, and either
/// resurrects the deleted file's artifacts or wedges waiting for a row that
/// no longer exists. The lease is the answer to "is anyone still asking for
/// this?", and it is checked at the points where a generator would otherwise
/// write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    /// What the lease is for. A file id, or an object id.
    pub target: String,
    /// Which generation of the target this lease was taken against.
    ///
    /// A plain liveness flag is not enough: a delete followed by a re-add of
    /// the same path produces the same file id in a new generation, and a
    /// lease from the old one must not authorise work on the new one.
    pub generation: u64,
}

/// A registry of outstanding leases.
///
/// Deliberately in-memory and deliberately not persisted: a lease is
/// permission to finish work that is already in flight, and a lease that
/// survived a crash would be permission to write to a database nobody is
/// looking at any more.
///
/// Takes `&self` and uses interior mutability. An `&mut self` registry would
/// be unusable for the thing leases are for -- while a generator holds a
/// lease on a file, the scanner is *supposed* to be able to ask whether the
/// file is still wanted, and an exclusive borrow would make that a compile
/// error at every call site.
#[derive(Debug, Default)]
pub struct LeaseRegistry {
    leases: RefCell<BTreeSet<(String, u64)>>,
}

impl LeaseRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        LeaseRegistry::default()
    }

    /// Take a lease, returning a guard that releases it on drop.
    ///
    /// The guard's `Drop` is the release, so a job that returns early, fails,
    /// or panics does not leak the lease and wedge the target forever. That
    /// is the wedge in stash#5953: without it, one panic leaves the file
    /// permanently un-processable and nothing says why.
    pub fn acquire(&self, target: impl Into<String>, generation: u64) -> LeaseGuard<'_> {
        let lease = Lease {
            target: target.into(),
            generation,
        };
        self.leases
            .borrow_mut()
            .insert((lease.target.clone(), lease.generation));
        LeaseGuard {
            registry: self,
            lease,
            live: true,
        }
    }

    /// Is the lease still held, and is it for this generation?
    pub fn is_held(&self, lease: &Lease) -> bool {
        self.leases
            .borrow()
            .contains(&(lease.target.clone(), lease.generation))
    }

    /// Drop a lease early. The guard's `Drop` makes this idempotent.
    pub fn release(&self, lease: &Lease) {
        self.leases
            .borrow_mut()
            .remove(&(lease.target.clone(), lease.generation));
    }

    /// How many leases are outstanding. A leak shows up here.
    pub fn outstanding(&self) -> usize {
        self.leases.borrow().len()
    }
}

/// Holds a lease until dropped. Dropping it is what cancels the job.
#[derive(Debug)]
pub struct LeaseGuard<'a> {
    registry: &'a LeaseRegistry,
    lease: Lease,
    live: bool,
}

impl LeaseGuard<'_> {
    /// The lease being held.
    pub fn lease(&self) -> &Lease {
        &self.lease
    }

    /// Give the lease up now, rather than at the end of the scope.
    pub fn release(mut self) {
        self.live = false;
        self.registry.release(&self.lease);
    }
}

impl Drop for LeaseGuard<'_> {
    fn drop(&mut self) {
        if self.live {
            self.registry.release(&self.lease);
        }
    }
}
