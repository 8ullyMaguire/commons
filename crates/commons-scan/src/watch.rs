//! Keeping the library index current: a debounced watcher, or a poll loop
//! when the volume cannot be trusted to deliver events.
//!
//! # The rule
//!
//! **A watcher is never trusted across a network boundary.** inotify events
//! are lost, coalesced, and reordered across NFS, SMB, and rclone, and a
//! silently missed `Create` event is a file that never appears in the
//! library -- no error, no log, just an object the user can see on disk and
//! not in the app. That is stash#7130, the "Loading forever" bug, and it is
//! why [`WatchMode::for_volume`] picks a poll loop from the volume's
//! filesystem type rather than from a setting.
//!
//! # The debounce
//!
//! A single file save is not one event. Editors write a temp file and rename
//! it, extractors write in chunks, and a copy is a create, several writes,
//! then a metadata change. Rescanning on every raw event means rescanning
//! the same path five times, and on a large library the scanner spends its
//! time re-stat'ing files it has already seen. So events are coalesced per
//! path over [`DEFAULT_DEBOUNCE`] and the set is drained on a tick.
//!
//! Coalescing per path, not globally, matters: a global debounce would hold
//! back an unrelated library's change behind a busy one.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use notify::event::ModifyKind;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};

use crate::walker::VolumePolicy;

/// How long to wait after the last event for a path before handing it over.
///
/// Two seconds is the plan's figure and it is not arbitrary: it is long
/// enough to swallow a chunked write and short enough that a user who drops
/// a file in and switches windows sees it already indexed.
pub const DEFAULT_DEBOUNCE: Duration = Duration::from_secs(2);

/// How often a remote volume is rescanned.
///
/// Remote is slow, so this is a deliberate compromise rather than a default.
/// Fifteen seconds is the shortest interval that does not turn a mounted
/// rclone into a busy loop, and it is configurable because a fast link
/// deserves better.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(15);

/// An explicit setting about how a root is watched.
///
/// Tri-state rather than `Option<Duration>`, because "poll every N" and "do
/// not poll, use a watcher" are both durations, and only one of them is a
/// remote mount's right answer. Making the override a tri-state keeps the
/// filesystem-type policy from quietly winning a disagreement with the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PollOverride {
    /// No opinion. The volume's policy decides.
    #[default]
    Auto,
    /// Always use a watcher, whatever the filesystem type. For a fast LAN
    /// mount where events have been verified to arrive.
    Never,
    /// Always poll, at this interval. For a local volume on FUSE, or a
    /// remote one the user wants checked more or less often.
    Always(Duration),
}

impl From<Option<Duration>> for PollOverride {
    fn from(d: Option<Duration>) -> Self {
        match d {
            Some(d) => PollOverride::Always(d),
            None => PollOverride::Auto,
        }
    }
}

/// How a library root is kept current.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchMode {
    /// inotify/FSEvents/ReadDirectoryChangesW, debounced.
    Evented,
    /// A periodic rescan. Used for remote volumes, and available anywhere as
    /// an override.
    Polled(Duration),
}

impl WatchMode {
    /// The mode for a volume, from its policy and the user's setting.
    ///
    /// The policy already decided local vs remote; this maps that to a
    /// mechanism and fills in the interval. The override always wins, because
    /// a user who has watched a volume misbehave knows something the
    /// filesystem type does not.
    pub fn for_volume(policy: &VolumePolicy, override_poll: PollOverride) -> Self {
        match override_poll {
            PollOverride::Never => WatchMode::Evented,
            PollOverride::Always(d) => WatchMode::Polled(d),
            PollOverride::Auto => match policy.poll_interval() {
                Some(d) => WatchMode::Polled(d),
                None => WatchMode::Evented,
            },
        }
    }

    /// Is this a poll loop? Used by tests and by the UI, which wants to say
    /// "polling every 15s" rather than "watching".
    pub fn is_polled(&self) -> bool {
        matches!(self, WatchMode::Polled(_))
    }

    /// Why this mode, for a settings screen.
    pub fn describe(&self) -> String {
        match self {
            WatchMode::Evented => "watching for changes".to_string(),
            WatchMode::Polled(d) => {
                format!("rechecking every {}", crate::progress::human_duration(*d))
            }
        }
    }
}

/// What changed under a library root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// Path relative to the watched root. The scanner wants this, not an
    /// absolute path, because the root moves when a library is remounted.
    pub path: PathBuf,
    pub kind: ChangeKind,
    /// When the change was first seen, before debouncing.
    pub first_seen: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ChangeKind {
    /// A file appeared, or its contents changed.
    Touched,
    /// A file went away.
    Removed,
    /// A directory appeared or vanished. Directories are reported so the
    /// scanner can re-enumerate what is under them, not because the scanner
    /// has a row for a directory.
    Directory,
}

impl ChangeKind {
    /// Does this change need the file's contents re-hashed, or is its
    /// existence enough?
    pub fn needs_hash(&self) -> bool {
        matches!(self, ChangeKind::Touched)
    }
}

/// Collects debounced changes and hands them out on a tick.
///
/// Separated from the OS watcher so the debounce logic is testable without a
/// real filesystem event, and so a poll loop and an evented watch can share
/// it. The first version fused the two, which meant the debounce could only
/// be tested by racing a real `touch` against a timer -- a test that passes
/// on a fast machine and fails on a loaded one.
#[derive(Debug)]
pub struct Debouncer {
    /// Path and kind to when it was FIRST seen. One map, because the
    /// first version kept a set of pending keys plus a set of keys plus a
    /// lookup into a third map -- three structures that had to agree, and
    /// which a removal from one of them could desynchronise.
    ///
    /// The first-seen time is what makes the debounce correct: the clock
    /// starts when a change begins, not when it was last touched.
    pending: BTreeMap<(PathBuf, ChangeKind), Instant>,
    debounce: Duration,
}

impl Default for Debouncer {
    fn default() -> Self {
        Debouncer::new(DEFAULT_DEBOUNCE)
    }
}

impl Debouncer {
    pub fn new(debounce: Duration) -> Self {
        Debouncer {
            pending: BTreeMap::new(),
            debounce,
        }
    }

    /// Record a change. Repeated records of the same (path, kind) collapse
    /// into one, keeping the *first* sighting time.
    pub fn record(&mut self, path: PathBuf, kind: ChangeKind, now: Instant) {
        self.pending.entry((path, kind)).or_insert(now);
    }

    /// Changes whose quiet period has elapsed since they were first seen, and
    /// only those. Sorted, so the consumer's order is deterministic.
    pub fn ready(&mut self, now: Instant) -> Vec<Change> {
        let debounce = self.debounce;
        let ready: Vec<_> = self
            .pending
            .iter()
            .filter(|(_, first)| now.saturating_duration_since(**first) >= debounce)
            .map(|(k, first)| (k.clone(), *first))
            .collect();
        ready
            .into_iter()
            .map(|((path, kind), first_seen)| {
                self.pending.remove(&(path.clone(), kind));
                Change {
                    path,
                    kind,
                    first_seen,
                }
            })
            .collect()
    }

    /// How many changes are waiting. For tests and for a status line.
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}

/// Sends [`Change`]s to a consumer on a channel.
///
/// The consumer is a plain `Receiver`, so the scanner side is a `for` loop
/// and a test can be a `recv_timeout`.
#[derive(Clone)]
pub struct ChangeSink {
    tx: Sender<Change>,
}

impl ChangeSink {
    pub fn channel() -> (ChangeSink, Receiver<Change>) {
        let (tx, rx) = mpsc::channel();
        (ChangeSink { tx }, rx)
    }

    /// Hand over a change. `false` means the receiver is gone, which is the
    /// normal shutdown path and not an error.
    pub fn send(&self, change: Change) -> bool {
        self.tx.send(change).is_ok()
    }
}

/// A live watcher. Dropping it stops the watch.
pub struct VolumeWatcher {
    mode: WatchMode,
    root: PathBuf,
    /// Kept alive because dropping the `notify` watcher stops it.
    _inner: Option<RecommendedWatcher>,
    rx: Receiver<Change>,
}

impl VolumeWatcher {
    /// Start watching a root with the mode the volume's policy implies.
    pub fn start(root: impl Into<PathBuf>, policy: &VolumePolicy) -> notify::Result<Self> {
        Self::start_with(root, policy, PollOverride::Auto, DEFAULT_DEBOUNCE)
    }

    /// Start watching, with the knobs exposed.
    pub fn start_with(
        root: impl Into<PathBuf>,
        policy: &VolumePolicy,
        override_poll: PollOverride,
        debounce: Duration,
    ) -> notify::Result<Self> {
        let root = root.into();
        let mode = WatchMode::for_volume(policy, override_poll);
        let (sink, rx) = ChangeSink::channel();

        let inner = match mode {
            WatchMode::Polled(interval) => {
                std::thread::Builder::new()
                    .name("commons-poll".into())
                    .spawn({
                        let root = root.clone();
                        move || poll_loop(root, interval, sink)
                    })
                    .expect("spawn poll loop");
                None
            }
            WatchMode::Evented => {
                let debouncer = Arc::new(Mutex::new(Debouncer::new(debounce)));
                // Two holders: the notify callback records into it, the relay
                // drains it. Cloning an Arc here rather than moving the value
                // twice is the whole reason it is an Arc.
                let for_events = Arc::clone(&debouncer);
                let for_relay = sink.clone();
                let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
                    let Ok(ev) = res else { return };
                    let mut d = for_events.lock().expect("debouncer mutex poisoned");
                    let now = Instant::now();
                    for (path, kind) in classify(&ev) {
                        d.record(path, kind, now);
                    }
                })
                .map_err(|e| notify::Error::generic(&format!("watcher: {e}")))?;
                watcher
                    .watch(&root, RecursiveMode::Recursive)
                    .map_err(|e| {
                        notify::Error::generic(&format!("watch {}: {e}", root.display()))
                    })?;
                std::thread::Builder::new()
                    .name("commons-watch".into())
                    .spawn(move || relay(debouncer, debounce, for_relay))
                    .expect("spawn watch relay");
                Some(watcher)
            }
        };

        Ok(VolumeWatcher {
            mode,
            root,
            _inner: inner,
            rx,
        })
    }

    /// The next settled change, or `None` on timeout or shutdown.
    pub fn next(&mut self, timeout: Duration) -> Option<Change> {
        match self.rx.recv_timeout(timeout) {
            Ok(c) => Some(c),
            Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => None,
        }
    }

    /// Every settled change, blocking until the stream ends.
    pub fn drain(&mut self) -> impl Iterator<Item = Change> + '_ {
        self.rx.iter()
    }

    pub fn mode(&self) -> WatchMode {
        self.mode
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

/// Rescan a remote volume on an interval and emit the difference.
///
/// The listing is a set of relative paths, so a root that gets remounted at
/// a different absolute path does not look like "everything was deleted".
fn poll_loop(root: PathBuf, interval: Duration, sink: ChangeSink) {
    let mut previous: BTreeSet<PathBuf> = listing(&root);
    loop {
        std::thread::sleep(interval);
        let current = listing(&root);
        if current == previous {
            continue;
        }
        let now = Instant::now();
        for path in current.difference(&previous) {
            if sink.send(Change {
                path: path.clone(),
                kind: ChangeKind::Touched,
                first_seen: now,
            }) {
                continue;
            } else {
                return;
            }
        }
        for path in previous.difference(&current) {
            if !sink.send(Change {
                path: path.clone(),
                kind: ChangeKind::Removed,
                first_seen: now,
            }) {
                return;
            }
        }
        previous = current;
    }
}

/// Every file under `root`, as paths relative to it.
fn listing(root: &Path) -> BTreeSet<PathBuf> {
    crate::walker::walk(root)
        .files
        .into_iter()
        .map(|f| f.relative)
        .collect()
}

/// Bridge a [`Debouncer`] into a channel, ticking at `tick`.
fn relay(debouncer: Arc<Mutex<Debouncer>>, debounce: Duration, out: ChangeSink) {
    // Tick at a quarter of the debounce so a change is released within about
    // a quarter of its settle time, rather than up to a full debounce late.
    let tick = (debounce / 4).max(Duration::from_millis(50));
    loop {
        std::thread::sleep(tick);
        let ready = {
            let Ok(mut d) = debouncer.lock() else { return };
            d.ready(Instant::now())
        };
        for change in ready {
            if !out.send(change) {
                return;
            }
        }
    }
}

/// Turn a notify event into (path, kind) pairs.
///
/// A `rename` carries the old and new path and needs a *removal* for the
/// first; every other kind treats each path as a change. The result is
/// deduplicated because notify is entitled to report the same path twice in
/// one event -- a rename whose two paths are equal, for instance, which is
/// not a thing a user did and is not a change the index needs.
fn classify(ev: &Event) -> Vec<(PathBuf, ChangeKind)> {
    let is_rename = matches!(ev.kind, EventKind::Modify(ModifyKind::Name(_)));
    let mut out: Vec<(PathBuf, ChangeKind)> = Vec::new();

    for path in &ev.paths {
        let kind = if ev.kind.is_remove() {
            ChangeKind::Removed
        } else {
            ChangeKind::Touched
        };
        out.push((path.clone(), kind));
    }

    if is_rename {
        if let (Some(from), Some(to)) = (ev.paths.first(), ev.paths.get(1)) {
            if from != to {
                out.push((from.clone(), ChangeKind::Removed));
                out.push((to.clone(), ChangeKind::Touched));
            }
        }
    }

    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    // ------------------------------------------------------------- WatchMode

    #[test]
    fn a_local_volume_is_watched() {
        let p = VolumePolicy::for_fs_type("ext4");
        assert_eq!(
            WatchMode::for_volume(&p, PollOverride::Auto),
            WatchMode::Evented
        );
        assert!(!WatchMode::for_volume(&p, PollOverride::Auto).is_polled());
    }

    #[test]
    fn a_remote_volume_is_polled() {
        let p = VolumePolicy::for_fs_type("fuse.rclone");
        let m = WatchMode::for_volume(&p, PollOverride::Auto);
        assert!(m.is_polled(), "a watcher cannot be trusted across NFS");
        assert!(
            m.describe().contains("rechecking"),
            "the UI has to be able to say so: {}",
            m.describe()
        );
    }

    #[test]
    fn an_override_beats_the_policy_in_both_directions() {
        let local = VolumePolicy::for_fs_type("ext4");
        let remote = VolumePolicy::for_fs_type("fuse.rclone");
        // Force polling a local volume: a user watching a btrfs snapshot
        // mounted over FUSE has a reason.
        assert!(
            WatchMode::for_volume(&local, PollOverride::Always(Duration::from_secs(5))).is_polled()
        );
        // Force watching a remote one: a fast LAN mount where the user has
        // verified events do arrive.
        assert!(
            WatchMode::for_volume(&remote, PollOverride::Auto).is_polled(),
            "the default stays polled"
        );
        assert_eq!(
            WatchMode::for_volume(&remote, PollOverride::Never),
            WatchMode::Evented,
            "the user's setting beats the filesystem type"
        );
    }

    // ------------------------------------------------------------- Debouncer

    #[test]
    fn a_change_is_held_for_the_debounce() {
        let t0 = Instant::now();
        let mut d = Debouncer::new(Duration::from_secs(2));
        d.record(PathBuf::from("a.mp4"), ChangeKind::Touched, t0);

        assert!(d.ready(at(t0, 0)).is_empty(), "not ready immediately");
        assert!(d.ready(at(t0, 1_999)).is_empty(), "not ready one ms early");
        assert_eq!(d.ready(at(t0, 2_000)).len(), 1, "ready at the boundary");
        assert!(d.is_empty(), "and released");
    }

    #[test]
    fn repeated_events_collapse_into_one_change() {
        let t0 = Instant::now();
        let mut d = Debouncer::new(Duration::from_secs(2));
        // A chunked write: 50 events for the same path.
        for i in 0..50 {
            d.record(PathBuf::from("a.mp4"), ChangeKind::Touched, at(t0, i * 10));
        }
        assert_eq!(d.len(), 1, "one path, one change");
        assert_eq!(d.ready(at(t0, 3_000)).len(), 1);
    }

    /// The clock starts at the FIRST event, not the last. If it started at the
    /// last, a file being written continuously would never be released --
    /// the "Loading forever" bug in miniature.
    #[test]
    fn the_settle_clock_starts_at_the_first_event() {
        let t0 = Instant::now();
        let mut d = Debouncer::new(Duration::from_secs(2));
        d.record(PathBuf::from("a.mp4"), ChangeKind::Touched, at(t0, 0));
        for i in 1..10 {
            // Still being written, one second apart.
            d.record(
                PathBuf::from("a.mp4"),
                ChangeKind::Touched,
                at(t0, i * 1_000),
            );
        }
        // Two seconds after the FIRST event, with nothing for a second.
        let ready = d.ready(at(t0, 2_100));
        assert_eq!(ready.len(), 1, "released, not starved by the writer");
    }

    #[test]
    fn different_paths_do_not_block_each_other() {
        let t0 = Instant::now();
        let mut d = Debouncer::new(Duration::from_secs(2));
        d.record(PathBuf::from("a.mp4"), ChangeKind::Touched, at(t0, 0));
        d.record(PathBuf::from("b.mp4"), ChangeKind::Touched, at(t0, 1_500));
        let ready = d.ready(at(t0, 2_100));
        // A global debounce would hold b until 3.5s for no reason.
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].path, PathBuf::from("a.mp4"), "a is ready first");
    }

    #[test]
    fn a_touch_and_a_removal_of_the_same_path_are_separate() {
        let t0 = Instant::now();
        let mut d = Debouncer::new(Duration::from_secs(1));
        d.record(PathBuf::from("a.mp4"), ChangeKind::Touched, at(t0, 0));
        d.record(PathBuf::from("a.mp4"), ChangeKind::Removed, at(t0, 0));
        let ready = d.ready(at(t0, 1_100));
        assert_eq!(ready.len(), 2, "coalescing must not lose the removal");
    }

    #[test]
    fn draining_twice_yields_nothing_the_second_time() {
        let t0 = Instant::now();
        let mut d = Debouncer::new(Duration::from_secs(1));
        d.record(PathBuf::from("a.mp4"), ChangeKind::Touched, at(t0, 0));
        assert_eq!(d.ready(at(t0, 1_100)).len(), 1);
        assert!(
            d.ready(at(t0, 5_000)).is_empty(),
            "a change is delivered once"
        );
    }

    #[test]
    fn ready_is_sorted_so_the_consumer_is_deterministic() {
        let t0 = Instant::now();
        let mut d = Debouncer::new(Duration::from_secs(1));
        for p in ["c.mp4", "a.mp4", "b.mp4"] {
            d.record(PathBuf::from(p), ChangeKind::Touched, at(t0, 0));
        }
        let got: Vec<String> = d
            .ready(at(t0, 1_100))
            .iter()
            .map(|c| c.path.to_string_lossy().into_owned())
            .collect();
        assert_eq!(got, ["a.mp4", "b.mp4", "c.mp4"]);
    }

    // -------------------------------------------------------------- classify

    #[test]
    fn a_rename_reports_a_removal_and_a_touch() {
        use notify::event::{ModifyKind, RenameMode};
        use notify::Event;
        let ev = Event {
            kind: EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
            paths: vec![PathBuf::from("old.mp4"), PathBuf::from("new.mp4")],
            attrs: Default::default(),
        };
        let got = classify(&ev);
        assert!(
            got.contains(&(PathBuf::from("old.mp4"), ChangeKind::Removed)),
            "the old row must be retired: {got:?}"
        );
        assert!(got.contains(&(PathBuf::from("new.mp4"), ChangeKind::Touched)));
    }

    #[test]
    fn a_rename_onto_itself_is_not_two_changes() {
        use notify::event::{ModifyKind, RenameMode};
        use notify::Event;
        let ev = Event {
            kind: EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
            paths: vec![PathBuf::from("same.mp4"), PathBuf::from("same.mp4")],
            attrs: Default::default(),
        };
        let got = classify(&ev);
        assert_eq!(got.len(), 1, "one path means one change: {got:?}");
    }

    #[test]
    fn a_removal_is_a_removal() {
        use notify::event::{EventKind, RemoveKind};
        use notify::Event;
        let ev = Event {
            kind: EventKind::Remove(RemoveKind::File),
            paths: vec![PathBuf::from("gone.mp4")],
            attrs: Default::default(),
        };
        assert_eq!(
            classify(&ev),
            vec![(PathBuf::from("gone.mp4"), ChangeKind::Removed)]
        );
    }

    #[test]
    fn a_creation_is_a_touch_that_needs_hashing() {
        use notify::event::{CreateKind, EventKind};
        use notify::Event;
        let ev = Event {
            kind: EventKind::Create(CreateKind::File),
            paths: vec![PathBuf::from("new.mp4")],
            attrs: Default::default(),
        };
        let got = classify(&ev);
        assert_eq!(got[0].1, ChangeKind::Touched);
        assert!(got[0].1.needs_hash(), "a new file must be hashed");
        assert!(
            !ChangeKind::Removed.needs_hash(),
            "a removed file has no content"
        );
    }

    // ------------------------------------------------------- end to end

    /// A real file appearing in a real watched directory comes out the other
    /// end, debounced.
    ///
    /// This is the only test that exercises notify itself; everything above
    /// tests the debounce and the classification, which is the right split
    /// because those are the parts that can be wrong in ways notify's
    /// behaviour is not. The debounce here is 200ms rather than 2s so the
    /// suite stays fast -- it is the mechanism under test, not the constant.
    #[test]
    fn a_real_file_change_arrives_debounced() {
        use tempfile::TempDir;
        let dir = TempDir::new().unwrap();
        let policy = VolumePolicy::for_fs_type("ext4");
        let mut w = VolumeWatcher::start_with(
            dir.path(),
            &policy,
            PollOverride::Auto,
            Duration::from_millis(200),
        )
        .expect("start watcher");

        std::fs::write(dir.path().join("a.mp4"), b"x").unwrap();

        let change = w
            .next(Duration::from_secs(5))
            .expect("a change should arrive");
        assert!(change.path.ends_with("a.mp4"), "got {:?}", change.path);
        assert_eq!(change.kind, ChangeKind::Touched);
        assert!(change.kind.needs_hash());
    }

    /// Two writes to the same file inside the debounce window come out once.
    /// This is the property that makes the debounce worth having.
    #[test]
    fn a_chunked_write_arrives_once() {
        use tempfile::TempDir;
        let dir = TempDir::new().unwrap();
        let policy = VolumePolicy::for_fs_type("ext4");
        let mut w = VolumeWatcher::start_with(
            dir.path(),
            &policy,
            PollOverride::Auto,
            Duration::from_millis(300),
        )
        .expect("start watcher");

        let f = dir.path().join("big.mp4");
        for i in 0..10 {
            std::fs::write(&f, vec![b'x'; 1024 * (i + 1)]).unwrap();
            std::thread::sleep(Duration::from_millis(20));
        }

        // Collect until the stream goes quiet. A fixed window is the wrong
        // shape here: on a loaded machine (a full-workspace test run) the ten
        // writes settle later than any short window, and the assertion then
        // fails not because the debounce is wrong but because the test stopped
        // looking. So: drain until a window the length of the debounce passes
        // with nothing more arriving, which is the definition of settled.
        //
        // A second, later write would still be caught, because a quiet window
        // this long after the last observed event means the debounce actually
        // fired. The property under test -- ten writes inside the debounce
        // window produce one change -- is unchanged.
        let mut got: Vec<Change> = Vec::new();
        while let Some(c) = w.next(Duration::from_millis(400)) {
            got.push(c);
        }
        assert_eq!(
            got.len(),
            1,
            "ten writes, one settled change: {:?}",
            got.iter()
                .map(|c| (c.path.clone(), c.kind))
                .collect::<Vec<_>>()
        );
    }

    /// A remote volume polls, and the poll emits only what changed. Without
    /// the diff this is a full rescan every interval, which is the reason a
    /// rclone mount appeared to hang forever (stash#7130).
    #[test]
    fn a_polled_remote_volume_emits_only_the_difference() {
        use tempfile::TempDir;
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/old.mp4"), b"x").unwrap();

        let mut w = VolumeWatcher::start_with(
            dir.path(),
            &VolumePolicy::for_fs_type("fuse.rclone"),
            PollOverride::Always(Duration::from_millis(150)),
            DEFAULT_DEBOUNCE,
        )
        .expect("start poller");
        assert!(w.mode().is_polled(), "a remote volume polls");
        // Give the first listing time to be taken.
        std::thread::sleep(Duration::from_millis(400));
        assert!(
            w.next(Duration::from_millis(300)).is_none(),
            "an unchanged volume produces nothing"
        );

        std::fs::write(dir.path().join("sub/new.mp4"), b"x").unwrap();
        let change = w
            .next(Duration::from_secs(5))
            .expect("the new file should be noticed");
        assert!(change.path.ends_with("new.mp4"), "got {:?}", change.path);
        assert_eq!(change.kind, ChangeKind::Touched);
        // The pre-existing file must NOT be re-reported.
        assert!(
            w.next(Duration::from_millis(400)).is_none(),
            "only the delta, not a full rescan"
        );

        std::fs::remove_file(dir.path().join("sub/old.mp4")).unwrap();
        let gone = w.next(Duration::from_secs(5)).expect("a removal");
        assert!(gone.path.ends_with("old.mp4"), "got {:?}", gone.path);
        assert_eq!(gone.kind, ChangeKind::Removed);
        assert!(!gone.kind.needs_hash());
    }
}
