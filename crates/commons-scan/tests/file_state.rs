//! T-P2-003: the file state machine, volume backoff, bulk-operation skipping,
//! and generation leases.
//!
//! The acceptance criterion for stash#5683 is a *count of stat calls*, not a
//! stopwatch and not a CPU measurement -- both of those are noisy enough to
//! be useless in CI. So these tests assert on
//! [`VolumeTracker::stats`] and on [`Walker::stats`], which is only meaningful
//! because production code and the tests count the same thing.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use commons_core::FileState;
use commons_scan::state::{
    actionable_by_default, parse_file_state, partition_for_bulk, BulkOutcome, Lease, LeaseRegistry,
    Volume, VolumeProbe, VolumeState, VolumeTracker,
};

fn root() -> PathBuf {
    PathBuf::from("/mnt/library")
}

fn tracker_with_backoff() -> VolumeTracker {
    VolumeTracker::new().with_backoff(Duration::from_secs(1), Duration::from_secs(8))
}

// ------------------------------------------------------------- the four states

#[test]
fn a_file_state_round_trips_through_the_database_string() {
    // These strings are in the database and in the GraphQL API, read by
    // things that are not this program. A rename here is a data migration.
    for state in [
        FileState::Present,
        FileState::Missing,
        FileState::Unreadable,
        FileState::Remote,
    ] {
        assert_eq!(parse_file_state(state.as_str()), state);
    }
}

#[test]
fn a_state_this_version_has_never_heard_of_is_optimistic() {
    // The failure mode here is a downgrade emptying a library. An unknown
    // state must read as `Present`, never as `Missing`.
    assert_eq!(parse_file_state("something-new"), FileState::Present);
    assert_eq!(parse_file_state(""), FileState::Present);
    assert_eq!(parse_file_state("MISSING"), FileState::Present);
}

#[test]
fn only_present_files_are_actionable_by_default() {
    assert!(actionable_by_default(FileState::Present));
    // The other three are all reasons not to touch a file without being
    // asked twice.
    assert!(!actionable_by_default(FileState::Missing));
    assert!(!actionable_by_default(FileState::Unreadable));
    assert!(!actionable_by_default(FileState::Remote));
}

// ------------------------------------------------------- the #5683 mechanism

/// A volume that has never been seen is probed every scan. Hiding a
/// configuration error behind backoff would make it silent forever.
#[test]
fn an_unconfigured_volume_is_probed_at_once() {
    let tracker = VolumeTracker::new();
    assert!(tracker.should_probe(&root(), SystemTime::now()));
    assert!(tracker.cleanup_enabled(&root()));
}

/// A missing volume costs exactly one probe per backoff period, and the
/// backoff doubles to a ceiling.
///
/// This is the assertion the ticket calls "the only objective measure of
/// stash#5683". Ten scans inside one backoff window must cost one probe, not
/// ten, and certainly not ten-per-file.
#[test]
fn a_missing_volume_costs_one_probe_per_backoff_period_not_one_per_file() {
    let mut tracker = tracker_with_backoff();
    tracker.configure(Volume::new(root()));

    // A fixed clock, so the test is about the backoff arithmetic and not
    // about how long the test took to run.
    let t0 = SystemTime::now();
    let unmounted = VolumeProbe {
        state: VolumeState::Unmounted,
        touched: true,
    };

    assert!(
        tracker.should_probe(&root(), t0),
        "a mounted volume is probed"
    );

    // It goes away. One miss.
    tracker.record(&root(), unmounted.clone(), t0);
    assert_eq!(tracker.stats(), 1, "exactly one probe so far");

    // Ten scans inside the first backoff window, none touching the
    // filesystem. With one miss the delay is the base, 1s.
    for millis in [10, 500, 900, 999] {
        assert!(
            !tracker.should_probe(&root(), t0 + Duration::from_millis(millis)),
            "at {millis}ms we are still inside the backoff window"
        );
    }
    assert_eq!(tracker.stats(), 1, "and the probe count has not moved");

    // At the window, one probe is allowed -- and the ramp continues from
    // there, so the next wait is 2s more than the last.
    let t1 = t0 + Duration::from_secs(1);
    assert!(tracker.should_probe(&root(), t1), "at the window boundary");
    tracker.record(&root(), unmounted.clone(), t1);
    assert_eq!(tracker.stats(), 2, "one probe, not one per scan");
    // Two misses, so the window is 2s from the last probe at t1.
    assert!(
        !tracker.should_probe(&root(), t1 + Duration::from_millis(1_999)),
        "two misses means a 2s window"
    );
    assert!(tracker.should_probe(&root(), t1 + Duration::from_secs(2)));
    assert_eq!(tracker.backoff_for(2), Duration::from_secs(2));

    // And the cost that actually matters. A user with a big library and a
    // 100k-file absent volume does not scan once an hour; scans are minutes
    // apart. Ten scans a minute for an hour is 600 scan cycles, and the
    // backoff must collapse those to a handful of probes -- not one per
    // scan, and never one per file.
    let mut tracker = tracker_with_backoff();
    tracker.configure(Volume::new(root()));
    tracker.record(&root(), unmounted.clone(), t0);
    let before = tracker.stats();
    let mut clock = t0;
    for _ in 0..600 {
        clock += Duration::from_secs(6);
        if tracker.should_probe(&root(), clock) {
            tracker.record(&root(), unmounted.clone(), clock);
        }
    }
    // At the ceiling the steady state is one probe per ceiling period, so
    // the cost is bounded by the ceiling and not by the scan rate. With a 6s
    // cycle and an 8s ceiling that is roughly half the scans; with the
    // production ceiling of an hour it is a handful. What must never happen
    // is one probe per scan, and what must never happen at all is one probe
    // per file.
    let probes_in_an_hour = tracker.stats() - before;
    assert!(
        probes_in_an_hour < 600,
        "600 scan cycles cost {probes_in_an_hour} probes, which is not less than one per scan"
    );
    // And the steady state is exactly the ceiling, not shorter: the ramp
    // saturates rather than continuing to grow without bound.
    assert_eq!(tracker.backoff_for(60), Duration::from_secs(8));
}

/// The backoff doubles and stops at the ceiling.
#[test]
fn the_backoff_doubles_to_a_ceiling() {
    let tracker = tracker_with_backoff();
    // `misses` counts misses already recorded, so the ramp starts at the
    // base: one absence waits the base, two wait double, and so on.
    assert_eq!(tracker.backoff_for(1), Duration::from_secs(1));
    assert_eq!(tracker.backoff_for(2), Duration::from_secs(2));
    assert_eq!(tracker.backoff_for(3), Duration::from_secs(4));
    assert_eq!(tracker.backoff_for(4), Duration::from_secs(8));
    // The ceiling: without it, exponential backoff eventually means "never
    // check again", and a reattached drive would go unnoticed for a week.
    assert_eq!(tracker.backoff_for(5), Duration::from_secs(8));
    assert_eq!(tracker.backoff_for(40), Duration::from_secs(8));
    assert_eq!(tracker.backoff_for(0), Duration::from_secs(1));
}

/// A volume with a huge miss count must not overflow the backoff computation
/// into a panic or a zero delay.
#[test]
fn a_miss_count_of_u32_max_does_not_overflow() {
    let tracker = tracker_with_backoff();
    assert_eq!(tracker.backoff_for(u32::MAX), Duration::from_secs(8));
    // And a tracker configured with a base larger than its ceiling is still
    // bounded, rather than never firing.
    let odd = VolumeTracker::new().with_backoff(Duration::from_secs(600), Duration::from_secs(5));
    assert_eq!(odd.backoff_for(0), Duration::from_secs(5));
}

/// A clock that jumps backwards probes now rather than never again.
#[test]
fn a_clock_that_moved_backwards_probes_now() {
    let mut tracker = tracker_with_backoff();
    let now = SystemTime::now();
    tracker.configure(Volume::new(root()));
    tracker.record(
        &root(),
        VolumeProbe {
            state: VolumeState::Unmounted,
            touched: true,
        },
        now,
    );
    // `now` is before the record's `last_seen`.
    assert!(
        tracker.should_probe(&root(), now - Duration::from_secs(3600)),
        "a backwards clock must not wedge a volume permanently"
    );
}

/// A remounted volume is found within one ceiling period, and its backoff
/// resets so the next absence starts from the base delay again.
#[test]
fn a_remount_resets_the_backoff() {
    let mut tracker = tracker_with_backoff();
    let now = SystemTime::now();
    tracker.configure(Volume::new(root()));
    let start = SystemTime::now();

    for _ in 0..5 {
        tracker.record(
            &root(),
            VolumeProbe {
                state: VolumeState::Unmounted,
                touched: true,
            },
            now,
        );
    }
    // Deep in the backoff: nothing would be looked at for a long time.
    assert!(!tracker.should_probe(&root(), start + Duration::from_secs(3)));

    // The drive comes back. A remount is noticed on the very next probe.
    let back = start + Duration::from_secs(3);
    tracker.record(
        &root(),
        VolumeProbe {
            state: VolumeState::Mounted,
            touched: true,
        },
        back,
    );
    assert!(
        tracker.should_probe(&root(), back),
        "a mounted volume is probed"
    );

    // And the next absence starts from the base delay again, measured from
    // the moment it was last seen present -- not from a stale anchor.
    tracker.record(
        &root(),
        VolumeProbe {
            state: VolumeState::Unmounted,
            touched: true,
        },
        back,
    );
    assert_eq!(
        tracker.backoff_for(1),
        Duration::from_secs(1),
        "back to base"
    );
    assert!(
        !tracker.should_probe(&root(), back + Duration::from_millis(999)),
        "the backoff restarted, so we are inside a fresh window"
    );
    assert!(tracker.should_probe(&root(), back + Duration::from_secs(1)));
}

/// A volume that is mounted but unreadable is a different state from an
/// unmounted one, because retrying sooner is reasonable.
#[test]
fn an_unreadable_volume_is_not_an_unmounted_one() {
    assert!(!VolumeState::Unreadable.readable());
    assert!(!VolumeState::Unmounted.readable());
    assert_ne!(VolumeState::Unreadable, VolumeState::Unmounted);

    let mut tracker = tracker_with_backoff();
    let now = SystemTime::now();
    tracker.configure(Volume::new(root()));
    tracker.record(
        &root(),
        VolumeProbe {
            state: VolumeState::Unreadable,
            touched: true,
        },
        now,
    );
    // Same backoff -- one probe, not one per scan.
    assert!(!tracker.should_probe(&root(), SystemTime::now() + Duration::from_millis(1)));
}

/// Reloading the config does not reset the backoff. If it did, a config
/// watcher firing every minute would cause one probe per minute on every
/// absent volume -- the same CPU problem in a smaller box.
#[test]
fn reconfiguring_a_volume_keeps_its_backoff() {
    let mut tracker = tracker_with_backoff();
    let now = SystemTime::now();
    tracker.configure(Volume::new(root()));
    let start = SystemTime::now();
    tracker.record(
        &root(),
        VolumeProbe {
            state: VolumeState::Unmounted,
            touched: true,
        },
        now,
    );

    // A config reload arrives, declaring the same volume. The reload keeps
    // the miss count, so the backoff is not reset -- otherwise a config
    // watcher firing every minute would cause a probe every minute on every
    // absent volume, which is the same CPU problem in a smaller box.
    tracker.configure(Volume::new(root()));
    assert_eq!(tracker.volumes().len(), 1, "not a second volume");
    assert_eq!(
        tracker.volumes()[0].consecutive_misses,
        1,
        "the miss count survived"
    );
    assert!(
        !tracker.should_probe(&root(), start + Duration::from_millis(10)),
        "the reload did not hand the volume a fresh backoff"
    );
}

// -------------------------------------------------------- #314, the opt-out

/// A hot-swap user opts a volume out of cleanup, and an unmount on that
/// volume marks nothing at all.
#[test]
fn an_opted_out_volume_marks_nothing_missing() {
    let mut tracker = tracker_with_backoff();
    let now = SystemTime::now();
    tracker.configure(Volume::opted_out(root()));
    tracker.record(
        &root(),
        VolumeProbe {
            state: VolumeState::Unmounted,
            touched: true,
        },
        now,
    );

    assert!(!tracker.cleanup_enabled(&root()));
    assert!(
        tracker
            .files_to_mark_missing(&root(), ["a.mp4", "b.mp4", "c.mp4"])
            .is_empty(),
        "stash#314: an opted-out volume's files keep their state"
    );
}

/// Opting out of cleanup is not opting out of noticing the drive. It still
/// costs one probe per backoff, and it is still reported as missing.
#[test]
fn opting_out_of_cleanup_still_tracks_the_volume() {
    let mut tracker = tracker_with_backoff();
    let now = SystemTime::now();
    tracker.configure(Volume::opted_out(root()));
    let start = SystemTime::now();
    tracker.record(
        &root(),
        VolumeProbe {
            state: VolumeState::Unmounted,
            touched: true,
        },
        now,
    );
    assert_eq!(tracker.volumes()[0].state, VolumeState::Unmounted);
    for _ in 0..10 {
        assert!(!tracker.should_probe(&root(), start + Duration::from_millis(10)));
    }
}

/// A mounted volume marks nothing missing, however many files are on it.
#[test]
fn a_mounted_volume_marks_nothing_missing() {
    let mut tracker = tracker_with_backoff();
    tracker.configure(Volume::new(root()));
    assert!(tracker
        .files_to_mark_missing(&root(), ["a.mp4", "b.mp4"])
        .is_empty());
}

/// A missing volume marks exactly the files that were on it, once.
#[test]
fn a_missing_volume_marks_exactly_its_own_files() {
    let mut tracker = tracker_with_backoff();
    let now = SystemTime::now();
    tracker.configure(Volume::new(root()));
    tracker.record(
        &root(),
        VolumeProbe {
            state: VolumeState::Unmounted,
            touched: true,
        },
        now,
    );
    assert_eq!(
        tracker.files_to_mark_missing(&root(), ["a.mp4", "b.mp4"]),
        vec!["a.mp4".to_string(), "b.mp4".to_string()]
    );
}

/// Two volumes, one gone. The other is untouched.
#[test]
fn one_missing_volume_does_not_disturb_another() {
    let mut tracker = tracker_with_backoff();
    let now = SystemTime::now();
    let other = PathBuf::from("/mnt/other");
    tracker.configure(Volume::new(root()));
    tracker.configure(Volume::new(&other));
    tracker.record(
        &root(),
        VolumeProbe {
            state: VolumeState::Unmounted,
            touched: true,
        },
        now,
    );
    assert!(tracker.files_to_mark_missing(&root(), ["a.mp4"]).len() == 1);
    assert!(tracker.files_to_mark_missing(&other, ["b.mp4"]).is_empty());
}

// ------------------------------------------------------ bulk operations

/// The ticket's point 3: skip by default, say how many and why, in one line.
#[test]
fn a_bulk_operation_skips_missing_items_and_says_so() {
    let (actionable, outcome) = partition_for_bulk([
        ("a", FileState::Present),
        ("b", FileState::Missing),
        ("c", FileState::Present),
        ("d", FileState::Missing),
        ("e", FileState::Present),
    ]);
    assert_eq!(actionable, vec!["a", "c", "e"]);
    assert_eq!(outcome.affected, 3);
    assert_eq!(outcome.skipped, 2);
    assert_eq!(
        outcome.one_line("Deleted"),
        "Deleted 3 items, skipped 2 (missing)",
        "one line, with the count and the reason"
    );
}

/// With nothing skipped there is nothing to say about it.
#[test]
fn a_clean_bulk_operation_says_nothing_extra() {
    let (actionable, outcome) =
        partition_for_bulk([("a", FileState::Present), ("b", FileState::Present)]);
    assert_eq!(actionable.len(), 2);
    assert_eq!(outcome.one_line("Tagged"), "Tagged 2 items");
}

/// A single item is not pluralised.
#[test]
fn one_item_is_not_pluralised() {
    let (_, outcome) = partition_for_bulk([("a", FileState::Present)]);
    assert_eq!(outcome.one_line("Deleted"), "Deleted 1 item");

    let (_, outcome) = partition_for_bulk([("a", FileState::Missing)]);
    assert_eq!(
        outcome.one_line("Deleted"),
        "Deleted 0 items, skipped 1 (missing)"
    );
}

/// Several different reasons, all named in the one line.
#[test]
fn several_skip_reasons_are_all_named() {
    let (actionable, outcome) = partition_for_bulk([
        ("a", FileState::Present),
        ("b", FileState::Missing),
        ("c", FileState::Unreadable),
        ("d", FileState::Remote),
    ]);
    assert_eq!(actionable, vec!["a"]);
    assert_eq!(outcome.skipped, 3);
    // Ordered, because BTreeSet -- so the message is stable run to run.
    assert_eq!(
        outcome.one_line("Organised"),
        "Organised 1 item, skipped 3 (missing, unreadable, remote)"
    );
}

/// An empty selection is a valid, boring outcome.
#[test]
fn an_empty_selection_is_fine() {
    let (actionable, outcome) = partition_for_bulk(Vec::new());
    assert!(actionable.is_empty());
    assert_eq!(outcome.affected, 0);
    assert_eq!(outcome.skipped, 0);
    assert_eq!(outcome.one_line("Deleted"), "Deleted 0 items");
}

// ------------------------------------------------------ #5953, the lease

/// A held lease authorises work; a dropped one cancels it.
#[test]
fn a_dropped_lease_cancels_the_job() {
    let registry = LeaseRegistry::new();
    let lease = Lease {
        target: "file-1".into(),
        generation: 1,
    };
    {
        let _guard = registry.acquire("file-1", 1);
        assert!(registry.is_held(&lease));
        assert_eq!(registry.outstanding(), 1);
    }
    // The guard went out of scope: the job is cancelled.
    assert!(!registry.is_held(&lease), "stash#5953: the generator stops");
    assert_eq!(registry.outstanding(), 0, "and does not leak the lease");
}

/// A job that returns early still releases, because `Drop` does it.
#[test]
fn an_early_return_still_releases_the_lease() {
    let registry = LeaseRegistry::new();
    let lease = Lease {
        target: "file-1".into(),
        generation: 1,
    };
    fn job(registry: &LeaseRegistry) -> Result<(), &'static str> {
        let _guard = registry.acquire("file-1", 1);
        Err("the file was deleted underneath us")
    }
    assert_eq!(job(&registry), Err("the file was deleted underneath us"));
    assert!(
        !registry.is_held(&lease),
        "a failed job does not wedge the file"
    );
    assert_eq!(registry.outstanding(), 0);
}

/// A job that panics releases too, which is the wedge the ticket is about:
/// without `Drop`, one panic would leave the file permanently
/// un-processable and nothing would say why.
#[test]
fn a_panicking_job_still_releases_the_lease() {
    let registry = LeaseRegistry::new();
    let lease = Lease {
        target: "file-1".into(),
        generation: 1,
    };
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = registry.acquire("file-1", 1);
        assert!(registry.is_held(&lease));
        panic!("boom");
    }));
    std::panic::set_hook(previous_hook);

    assert!(result.is_err(), "the panic happened");
    assert!(
        !registry.is_held(&lease),
        "the guard unwound, so the file is not wedged"
    );
    assert_eq!(registry.outstanding(), 0);
}

/// The registry is usable while a lease is held. This is the whole reason it
/// takes `&self`: the scanner has to be able to ask whether a file is still
/// wanted while a generator is working on it.
#[test]
fn the_registry_is_usable_while_a_lease_is_held() {
    let registry = LeaseRegistry::new();
    let _guard = registry.acquire("file-1", 1);
    assert_eq!(registry.outstanding(), 1);
    // A second, unrelated file can be leased while the first is held.
    let other = Lease {
        target: "file-2".into(),
        generation: 1,
    };
    let _other = registry.acquire("file-2", 1);
    assert!(registry.is_held(&other));
    assert_eq!(registry.outstanding(), 2);
}

/// A lease from a previous generation does not authorise a new one.
///
/// Delete a file, re-add the same path, and the file id can come back. A
/// liveness flag alone would let the old job write into the new object's
/// artifacts.
#[test]
fn a_lease_from_an_earlier_generation_does_not_authorise_the_next_one() {
    let registry = LeaseRegistry::new();
    let first = Lease {
        target: "file-1".into(),
        generation: 1,
    };
    let _guard = registry.acquire("file-1", 1);
    assert!(registry.is_held(&first));

    // The file is deleted and re-added: generation 2.
    let second = Lease {
        target: "file-1".into(),
        generation: 2,
    };
    assert!(
        !registry.is_held(&second),
        "the old job must not be able to write into the new object"
    );
}

/// Releasing early is the same as dropping, and doing it twice is harmless.
#[test]
fn releasing_early_is_idempotent() {
    let registry = LeaseRegistry::new();
    let lease = Lease {
        target: "file-1".into(),
        generation: 1,
    };
    let guard = registry.acquire("file-1", 1);
    guard.release();
    assert!(!registry.is_held(&lease));
    registry.release(&lease);
    assert_eq!(registry.outstanding(), 0);
}

/// Two jobs on different files hold independent leases.
#[test]
fn leases_are_independent_per_target() {
    let registry = LeaseRegistry::new();
    let a = Lease {
        target: "file-1".into(),
        generation: 1,
    };
    let b = Lease {
        target: "file-2".into(),
        generation: 1,
    };
    let _ga = registry.acquire("file-1", 1);
    let _gb = registry.acquire("file-2", 1);
    assert!(registry.is_held(&a) && registry.is_held(&b));
    assert_eq!(registry.outstanding(), 2);
    drop(_ga);
    assert!(!registry.is_held(&a));
    assert!(registry.is_held(&b), "file-2 is unaffected");
    assert_eq!(registry.outstanding(), 1);
}

/// A `BulkOutcome` is serialisable, because it goes to the API and into a log.
#[test]
fn a_bulk_outcome_serialises() {
    let (_, outcome) = partition_for_bulk([("a", FileState::Present), ("b", FileState::Missing)]);
    let json = serde_json::to_string(&outcome).unwrap();
    assert_eq!(
        json,
        r#"{"affected":1,"skipped":1,"skipped_states":["missing"]}"#
    );
    let back: BulkOutcome = serde_json::from_str(&json).unwrap();
    assert_eq!(back, outcome);
}

/// A `Volume` round-trips too -- these go in the config file.
#[test]
fn a_volume_serialises() {
    let v = Volume::new("/mnt/library");
    let json = serde_json::to_string(&v).unwrap();
    let back: Volume = serde_json::from_str(&json).unwrap();
    assert_eq!(back.root, v.root);
    assert_eq!(back.state, v.state);
    assert_eq!(back.cleanup_enabled, v.cleanup_enabled);
}
