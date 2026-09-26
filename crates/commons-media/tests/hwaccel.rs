//! T-P2-005: hardware acceleration, and the reason string that is the ticket.
//!
//! stash#7239 is the whole of this file. The report was that acceleration shows
//! as unavailable and the user is told nothing they can act on — `[0] gives
//! no actionable reason`. So:
//!
//!   * for every unavailable accelerator, the reason is non-empty **and** names
//!     a concrete cause. An empty reason fails. A bare "not supported" fails.
//!   * a build that *compiles in* an encoder is not the same as a machine that
//!     can *use* it, and the probe has to tell those apart.
//!   * configuration is respected, never guessed (spec §6.4).

use std::path::{Path, PathBuf};

use commons_media::hwaccel::{
    detect, probe_all, probe_one, Accel, FfmpegCaps, HwAccelConfig, HwAccelStatus, ProbePaths,
    Unavailable, Verdict,
};

/// An ffmpeg build with everything compiled in — which is what a real one is.
fn full_build() -> FfmpegCaps {
    FfmpegCaps {
        hwaccels: ["cuda", "vaapi", "qsv", "drm"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        encoders: [
            "h264_nvenc",
            "hevc_nvenc",
            "h264_vaapi",
            "hevc_vaapi",
            "h264_qsv",
            "hevc_qsv",
            "libwebp",
            "mjpeg",
            "libx264",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect(),
        version: "ffmpeg version 9.0.1 Copyright (c) 2000-2026".to_string(),
    }
}

/// A build with none of it, which is what a minimal distro ships.
fn bare_build() -> FfmpegCaps {
    FfmpegCaps {
        hwaccels: Vec::new(),
        encoders: ["libwebp", "mjpeg", "libx264"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        version: "ffmpeg version 4.2 minimal".to_string(),
    }
}

/// A fixture `/dev` tree.
struct Fixture {
    _dir: tempfile::TempDir,
    paths: ProbePaths,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let dri = dir.path().join("dri");
        let dev = dir.path().join("dev");
        std::fs::create_dir_all(&dri).unwrap();
        std::fs::create_dir_all(&dev).unwrap();
        Fixture {
            paths: ProbePaths {
                dri_dir: dri,
                dev_dir: dev,
                lib_dir: dir.path().join("lib"),
            },
            _dir: dir,
        }
    }

    fn with_render_node(&self, name: &str) -> PathBuf {
        let p = self.paths.dri_dir.join(name);
        std::fs::write(&p, b"").unwrap();
        // A render node is a character device in real life. A mode of 0600 on
        // a regular file reproduces the *permission* case, which is what the
        // Docker path (#7007) runs into.
        set_mode(&p, 0o600);
        p
    }

    fn with_nvidia_node(&self) -> PathBuf {
        let p = self.paths.dev_dir.join("nvidia0");
        std::fs::write(&p, b"").unwrap();
        set_mode(&p, 0o666);
        p
    }
}

fn set_mode(p: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).unwrap();
}

// ============================================================
// The acceptance test the ticket names
// ============================================================

/// **Accept:** for each unavailable accelerator, the reason is non-empty and
/// names a concrete cause.
///
/// The regression guard for #7239. If someone adds a branch that produces `""`,
/// or "unavailable", or "not supported", this fails.
#[test]
fn every_unavailable_accelerator_gives_an_actionable_reason() {
    // Every combination the probe can reach, not just one.
    let mut statuses = Vec::new();

    // The real machine, whatever it is.
    statuses.push(detect(Path::new("ffmpeg"), &HwAccelConfig::default()));

    // A full build with no devices at all.
    let fixture = Fixture::new();
    statuses.push(probe_all(
        &full_build(),
        &fixture.paths,
        &HwAccelConfig::default(),
    ));

    // A full build with every device present.
    let with_all = Fixture::new();
    with_all.with_render_node("renderD128");
    with_all.with_nvidia_node();
    statuses.push(probe_all(
        &full_build(),
        &with_all.paths,
        &HwAccelConfig::default(),
    ));

    // A build with nothing compiled in.
    statuses.push(probe_all(
        &bare_build(),
        &fixture.paths,
        &HwAccelConfig::default(),
    ));

    // Everything switched off.
    let all_off = HwAccelConfig {
        disabled: Accel::ALL.to_vec(),
        report_only: false,
    };
    statuses.push(probe_all(&full_build(), &fixture.paths, &all_off));

    // A missing ffmpeg.
    statuses.push(detect(
        Path::new("/nonexistent/ffmpeg"),
        &HwAccelConfig::default(),
    ));

    let mut checked = 0usize;
    for status in &statuses {
        for (name, reason) in &status.unavailable {
            assert!(
                !reason.trim().is_empty(),
                "{name} is unavailable with an EMPTY reason. This is exactly #7239."
            );
            assert!(
                reason.chars().any(|c| c.is_ascii_alphanumeric()),
                "{name}: {reason:?} has no words in it"
            );
            assert!(
                reason.len() > 10,
                "{name}: {reason:?} is too short to be actionable"
            );
            checked += 1;
        }
        status
            .is_actionable()
            .unwrap_or_else(|e| panic!("{e}\n  status: {}", status.summary()));
    }
    assert!(
        checked >= 8,
        "only {checked} reasons were checked; the fixtures are not reaching the branches"
    );
}

// ============================================================
// "Compiled in" is not "usable"
// ============================================================

/// The trap. A build with `h264_nvenc` on a machine with no GPU reports
/// acceleration *available*, and the first thumbnail silently falls back after
/// a multi-second timeout.
#[test]
fn a_built_in_encoder_without_a_device_is_unavailable_with_a_reason() {
    let fixture = Fixture::new();
    let status = probe_all(&full_build(), &fixture.paths, &HwAccelConfig::default());

    assert!(
        !status.has(Accel::Nvenc),
        "nvenc cannot be available: there is no /dev/nvidia* in the fixture"
    );
    let reason = status.reason_for(Accel::Nvenc).expect("a reason");
    assert!(
        reason.contains("/dev"),
        "{reason:?} does not say where it looked"
    );
    assert!(status.is_actionable().is_ok(), "{}", status.summary());
}

/// With a device, it is available.
#[test]
fn a_usable_device_makes_it_available() {
    let fixture = Fixture::new();
    fixture.with_nvidia_node();
    let status = probe_all(&full_build(), &fixture.paths, &HwAccelConfig::default());
    assert!(status.has(Accel::Nvenc), "{}", status.summary());
    assert_eq!(status.reason_for(Accel::Nvenc), None);
}

/// A render node makes both VA-API and QSV available.
#[test]
fn a_render_node_makes_vaapi_and_qsv_available() {
    let fixture = Fixture::new();
    fixture.with_render_node("renderD128");
    let status = probe_all(&full_build(), &fixture.paths, &HwAccelConfig::default());
    assert!(status.has(Accel::VaApi), "{}", status.summary());
    assert!(status.has(Accel::Qsv));
}

/// A device this process cannot open is a distinct failure from a missing one.
///
/// This is the Docker case #7007: the image ships with the hooks wired, the
/// node is mapped in, and without `--group-add render` the process cannot open
/// it. Saying "no device found" would send the user looking in the wrong place.
#[test]
fn an_unreadable_device_says_so_and_names_the_node() {
    let fixture = Fixture::new();
    let node = fixture.with_render_node("renderD128");
    set_mode(&node, 0o000);

    let verdict = probe_one(
        Accel::VaApi,
        &full_build(),
        &fixture.paths,
        &HwAccelConfig::default(),
    );
    let reason = verdict.reason_for(Accel::VaApi).expect("a reason");
    assert!(
        reason.contains("renderD128"),
        "{reason:?} does not name the node that exists"
    );
    assert!(
        reason.contains("permission") || reason.contains("mode"),
        "{reason:?} does not say the problem is permissions: {}",
        verdict_repr(&verdict)
    );
}

fn verdict_repr(v: &Verdict) -> String {
    format!("{v:?}")
}

// ============================================================
// The build, and the platform
// ============================================================

/// A build without the method says so, and names the build.
#[test]
fn a_build_without_the_method_says_which_build() {
    let fixture = Fixture::new();
    fixture.with_render_node("renderD128");
    let status = probe_all(&bare_build(), &fixture.paths, &HwAccelConfig::default());

    assert!(!status.has(Accel::VaApi));
    let reason = status.reason_for(Accel::VaApi).unwrap();
    assert!(reason.contains("ffmpeg version 4.2 minimal"), "{reason:?}");
    assert!(reason.contains("compiled"), "{reason:?}");

    // A *partial* build: the method is listed but the encoder is not. That is
    // a real packaging state, and it is different from either extreme.
    let mut partial = full_build();
    partial.encoders.retain(|e| e != "hevc_vaapi");
    let verdict = probe_one(
        Accel::VaApi,
        &partial,
        &fixture.paths,
        &HwAccelConfig::default(),
    );
    assert!(!verdict.is_available());
    assert!(status.is_actionable().is_ok(), "{}", status.summary());
}

/// VideoToolbox is macOS-only and says so, on any platform.
#[test]
fn videotoolbox_is_reported_as_platform_specific() {
    let fixture = Fixture::new();
    let status = probe_all(&full_build(), &fixture.paths, &HwAccelConfig::default());
    let reason = status.reason_for(Accel::VideoToolbox);
    if cfg!(target_os = "macos") {
        assert!(status.has(Accel::VideoToolbox), "{}", status.summary());
    } else {
        let reason = reason.expect("a reason on a non-mac");
        assert!(reason.contains("macOS"), "{reason:?}");
        assert!(reason.contains(std::env::consts::OS), "{reason:?}");
    }
    status.is_actionable().expect("actionable");
}

// ============================================================
// Configuration is respected, not guessed
// ============================================================

/// An accelerator the user switched off stays off, even with a device present,
/// and the reason names the config file.
#[test]
fn a_disabled_accelerator_stays_off_with_the_hardware_present() {
    let fixture = Fixture::new();
    fixture.with_render_node("renderD128");
    fixture.with_nvidia_node();

    let config = HwAccelConfig {
        disabled: vec![Accel::Nvenc, Accel::VaApi],
        report_only: false,
    };
    let status = probe_all(&full_build(), &fixture.paths, &config);

    assert!(!status.has(Accel::Nvenc), "nvenc is switched off");
    assert!(!status.has(Accel::VaApi), "vaapi is switched off");
    let reason = status.reason_for(Accel::Nvenc).unwrap();
    assert!(
        reason.contains("config.toml"),
        "{reason:?} does not name the file"
    );
    // And the ones that were not switched off are still reported normally, so
    // the user can see what they *could* turn on.
    assert!(status.has(Accel::Qsv), "{}", status.summary());
}

/// An empty `disabled` list is "no opinion", not "nothing enabled".
#[test]
fn an_empty_disabled_list_probes_normally() {
    let fixture = Fixture::new();
    fixture.with_nvidia_node();
    let status = probe_all(&full_build(), &fixture.paths, &HwAccelConfig::default());
    assert!(status.has(Accel::Nvenc), "{}", status.summary());
}

// ============================================================
// The shapes
// ============================================================

/// Available and unavailable partition the accelerators: exactly one reason
/// per accelerator, and none in both lists.
#[test]
fn every_accelerator_appears_exactly_once() {
    let fixture = Fixture::new();
    let status = probe_all(&full_build(), &fixture.paths, &HwAccelConfig::default());
    let total = status.available.len() + status.unavailable.len();
    assert_eq!(total, Accel::ALL.len(), "{status:?}");
    for a in Accel::ALL {
        let here = status.has(a);
        let there = status.reason_for(a).is_some();
        assert!(
            here != there,
            "{a}: available={here} has_reason={there} -- exactly one must be true"
        );
    }
}

/// Every available accelerator records the device that makes it available.
///
/// Without this, the planner has nothing to build a plan from and silently
/// declines -- which reads as "the probe found nothing" and is indistinguishable
/// from a machine with no GPU.
#[test]
fn every_available_accelerator_records_its_device() {
    let fixture = Fixture::new();
    fixture.with_render_node("renderD128");
    fixture.with_nvidia_node();
    let status = probe_all(&full_build(), &fixture.paths, &HwAccelConfig::default());

    for name in &status.available {
        assert!(
            status.devices.contains_key(name),
            "{name} is listed as available but no device was recorded: {status:?}"
        );
        let node = status.device_for(Accel::parse(name).unwrap()).unwrap();
        assert!(node.exists(), "{name} names {node:?}, which does not exist");
    }
    for (name, _) in &status.unavailable {
        assert!(
            !status.devices.contains_key(name),
            "{name} is unavailable yet has a device recorded"
        );
    }
    assert!(
        status
            .device_for(Accel::VaApi)
            .unwrap()
            .to_string_lossy()
            .contains("renderD"),
        "{}",
        status.summary()
    );
}

/// The status survives a JSON round trip, because it crosses the API boundary.
#[test]
fn the_status_round_trips_through_json() {
    let fixture = Fixture::new();
    let status = probe_all(&full_build(), &fixture.paths, &HwAccelConfig::default());
    let json = serde_json::to_string(&status).unwrap();
    let back: HwAccelStatus = serde_json::from_str(&json).unwrap();
    assert_eq!(status, back);
    // And the reasons survive, which is the part that matters.
    for (name, reason) in &status.unavailable {
        assert_eq!(
            back.reason_for(Accel::parse(name).unwrap()),
            Some(reason.as_str())
        );
    }
}

/// Accelerator names round trip, because they go in a config file.
#[test]
fn accelerator_names_round_trip() {
    for a in Accel::ALL {
        assert_eq!(Accel::parse(a.as_str()), Some(a));
    }
    assert_eq!(Accel::parse("vaapi"), Some(Accel::VaApi));
    assert_eq!(Accel::parse("nonsense"), None);
    assert_eq!(
        Accel::ALL.len(),
        4,
        "spec §6.4 names VA-API, NVENC, QSV, VideoToolbox"
    );
}

/// `Unavailable` names its cause in its own type, so an empty reason is not
/// constructible.
#[test]
fn the_reason_type_cannot_be_empty() {
    // Every variant carries the data needed to phrase itself. This test exists
    // to fail if a variant is ever added that does not.
    let cases = [
        Unavailable::NotBuiltIn { build: "x".into() },
        Unavailable::WrongPlatform {
            platform: "macOS".into(),
        },
        Unavailable::NoDevice {
            expected: "node".into(),
            searched: "/dev".into(),
        },
        Unavailable::DeviceUnusable {
            device: "/dev/x".into(),
            detail: "d".into(),
        },
        Unavailable::Disabled {
            source: "config.toml".into(),
        },
        Unavailable::DriverMissing {
            library: "l.so".into(),
            package: "p".into(),
        },
    ];
    for c in cases {
        for accel in Accel::ALL {
            let reason = c.reason_for(accel);
            assert!(
                reason.contains(accel.display_name()),
                "{reason:?} does not name {accel}"
            );
            assert!(reason.len() > 15, "{reason:?} is too short");
        }
    }
}

/// The summary is one line and names every accelerator.
#[test]
fn the_summary_is_one_line_and_complete() {
    let fixture = Fixture::new();
    let status = probe_all(&full_build(), &fixture.paths, &HwAccelConfig::default());
    let summary = status.summary();
    assert!(!summary.contains('\n'), "{summary:?}");
    for a in Accel::ALL {
        assert!(summary.contains(a.as_str()), "{summary:?} omits {a}");
    }
}
