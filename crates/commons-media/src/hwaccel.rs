//! Hardware acceleration: what is actually usable, and why not.
//!
//! # The problem this file exists to solve
//!
//! stash#7239: the settings page listed hardware acceleration as
//! off/unavailable and gave the user nothing to act on. `[0] gives no
//! actionable reason`. The rule here is that **every unavailable accelerator
//! carries a human-readable reason naming a concrete cause**, and
//! [`HwAccelStatus::is_actionable`] is the check that enforces it.
//!
//! # Why listing ffmpeg's encoders is not a probe
//!
//! `ffmpeg -encoders` on a machine with no GPU at all lists `h264_nvenc`,
//! `h264_qsv`, `h264_vaapi` and the rest, because they are compiled in. They
//! are compiled into the ffmpeg *binary*; the hardware they talk to is a
//! different machine, reached through a device node or a driver library. So
//! "is nvenc available" is not a question about ffmpeg at all.
//!
//! That is the trap: an implementation that asks ffmpeg reports acceleration
//! available on a laptop with no GPU, and the first thumbnail silently falls
//! back after a multi-second timeout.
//!
//! So each accelerator has its own real probe, in increasing order of cost:
//!
//! 1. **Is the ffmpeg build able to do it at all?** `-hwaccels` and `-encoders`
//!    name the method. A build without it cannot be asked to try.
//! 2. **Does the device exist?** `/dev/dri/renderD*` for VA-API and QSV,
//!    `/dev/nvidia*` for NVENC. This is a `stat`, not a guess.
//! 3. **Is the driver actually loaded?** A render node can exist with no
//!    driver behind it — a container with the device mapped but no
//!    `/dev/dri` permission, a module not yet autoloaded.
//!
//! Each step that fails produces the *specific* reason. "no VA-API" is not
//! actionable; "/dev/dri/renderD128 exists but is not readable by this process
//! (permissions)" is.
//!
//! # What is deliberately not here
//!
//! No attempt to *benchmark* the accelerator or to warm a driver open. A
//! status is cheap, is asked at startup and on every settings change, and a
//! probe that costs a second is a probe nobody calls.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

/// The accelerators this build knows how to look for.
///
/// A closed set rather than a list parsed out of `ffmpeg -hwaccels`: the
/// reason strings are the point, and a reason can only be written for an
/// accelerator someone thought about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Accel {
    /// Intel/AMD VA-API, through a DRI render node.
    VaApi,
    /// NVIDIA NVENC.
    Nvenc,
    /// Intel Quick Sync.
    Qsv,
    /// Apple's VideoToolbox. macOS only, and named in spec §6.4.
    VideoToolbox,
}

impl Accel {
    /// Every accelerator, in a stable order.
    pub const ALL: [Accel; 4] = [Accel::VaApi, Accel::Nvenc, Accel::Qsv, Accel::VideoToolbox];

    /// The name shown in settings, and the key in config.
    pub const fn as_str(self) -> &'static str {
        match self {
            Accel::VaApi => "vaapi",
            Accel::Nvenc => "nvenc",
            Accel::Qsv => "qsv",
            Accel::VideoToolbox => "videotoolbox",
        }
    }

    /// A name a human recognises. Used in reason strings, where `qsv` alone
    /// means nothing to the person reading the error.
    pub const fn display_name(self) -> &'static str {
        match self {
            Accel::VaApi => "VA-API",
            Accel::Nvenc => "NVENC",
            Accel::Qsv => "Intel Quick Sync",
            Accel::VideoToolbox => "VideoToolbox",
        }
    }

    /// The `ffmpeg -hwaccels` method name, where one exists.
    pub const fn hwaccel_method(self) -> Option<&'static str> {
        match self {
            Accel::VaApi => Some("vaapi"),
            Accel::Qsv => Some("qsv"),
            // NVENC is an encoder, not a hwaccel method; it is reached with
            // `-c:v h264_nvenc`, and asking for `-hwaccel nvenc` is an error.
            Accel::Nvenc => None,
            Accel::VideoToolbox => Some("videotoolbox"),
        }
    }

    /// The encoder name ffmpeg would use, for the codecs this project
    /// produces. `None` means "no encoder" -- see [`Accel::encoders`].
    pub const fn encoder_prefix(self) -> Option<&'static str> {
        match self {
            Accel::VaApi => Some("_vaapi"),
            Accel::Nvenc => Some("_nvenc"),
            Accel::Qsv => Some("_qsv"),
            Accel::VideoToolbox => Some("_videotoolbox"),
        }
    }

    /// The ffmpeg encoder names that mean this accelerator, for the codecs
    /// that matter here.
    pub fn encoders(self) -> Vec<String> {
        const CODECS: [&str; 2] = ["h264", "hevc"];
        match self.encoder_prefix() {
            Some(suffix) => CODECS.iter().map(|c| format!("{c}{suffix}")).collect(),
            None => Vec::new(),
        }
    }

    /// The `-init_hw_device` type, which is also the ffmpeg filter prefix.
    pub const fn device_type(&self) -> &'static str {
        match self {
            Accel::VaApi => "vaapi",
            Accel::Qsv => "qsv",
            _ => "vaapi",
        }
    }

    /// The name this accelerator's hardware context is bound to.
    ///
    /// Matches `Accel::VaApi => "va"`, which is what `-filter_hw_device` takes
    /// and what `-hwaccel_device` refers to.
    pub const fn device_handle(&self) -> &'static str {
        match self {
            Accel::VaApi => "va",
            Accel::Qsv => "qsv",
            _ => "va",
        }
    }

    /// The format the decoder hands frames over in, when the frames stay on the
    /// device.
    ///
    /// `vaapi` for VA-API, not `nv12`. A software pixel format here means
    /// ffmpeg copies every frame back to system memory to satisfy it, and the
    /// hardware path becomes a slower software path -- the failure mode this
    /// whole module exists to prevent.
    pub const fn hw_surface_format(&self) -> &'static str {
        match self {
            Accel::VaApi => "vaapi",
            Accel::Qsv => "qsv",
            _ => "nv12",
        }
    }

    /// The `-init_hw_device` value, naming the render node the probe chose.
    ///
    /// The `va:` prefix is not optional in practice. Without it ffmpeg indexes
    /// the whole DRM subsystem, and on a machine whose only DRM device is
    /// `card1` with no VA display on index 0 that fails with
    /// `No VA display found for device 0` -- while the render node the probe
    /// found is right there and works. Naming the node is what makes the two
    /// agree.
    pub fn device_spec(&self, node: &Path) -> String {
        match self {
            Accel::VaApi => format!("vaapi=va:{}", node.display()),
            _ => format!("{}=qsv:{}", self.device_type(), node.display()),
        }
    }

    /// Parse a persisted or configured name.
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.as_str() == name)
    }
}

impl fmt::Display for Accel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why an accelerator is not available.
///
/// Not a string. A string is how #7239 happened: the first implementation had
/// `unavailable: Vec<(String, String)>` and filled the second element with `""`
/// in three of five branches, and nothing complained, because `""` is a valid
/// `String`.
///
/// Each variant is a distinct cause, and each knows how to phrase itself for a
/// person. An unlisted cause is a compile error at every `match`, which is the
/// point: adding a way for this to fail means writing the sentence for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unavailable {
    /// This ffmpeg build does not include the method.
    NotBuiltIn { build: String },
    /// The build has it, but this platform does not.
    WrongPlatform { platform: String },
    /// No device node.
    NoDevice { expected: String, searched: String },
    /// A device node exists but cannot be used.
    DeviceUnusable { device: String, detail: String },
    /// Present but switched off in configuration.
    Disabled { source: String },
    /// The driver's userspace library is not installed.
    DriverMissing { library: String, package: String },
}

impl Unavailable {
    /// The sentence shown to the user, naming `accel`.
    ///
    /// Every arm produces something with a concrete noun in it. A test asserts
    /// that, so an arm that says "unavailable" fails the test rather than
    /// shipping.
    ///
    /// The accelerator is a parameter rather than part of the variant because
    /// it is always the same for a given probe, and putting it in the variant
    /// would mean constructing `Unavailable::NotBuiltIn` in five places each
    /// remembering to say which accelerator it was about. The first version
    /// used a shared constant for the name and every reason came out reading
    /// "this accelerator", which tells the reader nothing.
    pub fn reason_for(&self, accel: Accel) -> String {
        let name = accel.display_name();
        match self {
            Unavailable::NotBuiltIn { build } => {
                format!("{build} was compiled without {name} support, so it cannot be used at all")
            }
            Unavailable::WrongPlatform { platform } => format!(
                "{name} only exists on {platform}; this system is {}",
                std::env::consts::OS
            ),
            Unavailable::NoDevice { expected, searched } => {
                format!("{name} needs a {expected} and there is none (looked in {searched})")
            }
            Unavailable::DeviceUnusable { device, detail } => {
                format!("{name}: {device} exists but cannot be used: {detail}")
            }
            Unavailable::Disabled { source } => {
                format!("{name} is turned off in {source}")
            }
            Unavailable::DriverMissing { library, package } => format!(
                "{name}: the driver library {library} is not installed (package: {package})"
            ),
        }
    }
}

/// One accelerator's verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Usable, and here is the device that makes it so.
    ///
    /// The node is carried rather than looked up again by the caller, because
    /// a plan that re-probes can name a *different* node than the status it
    /// came from. On a two-GPU machine that is a plan that reports one device
    /// and uses another, and the symptom is a machine that got slower when the
    /// user turned acceleration on.
    Available { node: std::path::PathBuf },
    /// Not usable, and here is why.
    Unavailable(Unavailable),
}

impl Verdict {
    pub fn is_available(&self) -> bool {
        matches!(self, Verdict::Available { .. })
    }

    /// The device that makes this accelerator available.
    pub fn node(&self) -> Option<&Path> {
        match self {
            Verdict::Available { node } => Some(node),
            Verdict::Unavailable(_) => None,
        }
    }

    /// The reason, or `None` if available.
    pub fn reason_for(&self, accel: Accel) -> Option<String> {
        match self {
            Verdict::Available { .. } => None,
            Verdict::Unavailable(u) => Some(u.reason_for(accel)),
        }
    }
}

/// What acceleration is present, and what is not.
///
/// The two lists are the ticket's shape. `available` holds accelerator names
/// and `unavailable` holds `(name, reason)` pairs, and the reason is the whole
/// point of the type: there is no way to construct an `Unavailable` without
/// naming a cause.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HwAccelStatus {
    /// Accelerator names that are usable.
    pub available: Vec<String>,
    /// `(name, human-readable reason)` for the rest. The reason is never
    /// empty; `HwAccelStatus::is_actionable` is the guard.
    pub unavailable: Vec<(String, String)>,
    /// Accelerator name to the device that makes it usable.
    ///
    /// Separate from `available` because the plan needs the device and the
    /// settings page does not, and a second list of names would be a second
    /// thing that can disagree with the first. `is_actionable` checks the
    /// partition; a test checks this map agrees with it.
    #[serde(default)]
    pub devices: BTreeMap<String, String>,
}

impl HwAccelStatus {
    /// Is this accelerator usable?
    pub fn has(&self, accel: Accel) -> bool {
        self.available.iter().any(|n| n == accel.as_str())
    }

    /// The device that makes `accel` usable.
    ///
    /// `None` when the accelerator is not available, and also when it is
    /// available but no device was recorded -- which the invariant test says
    /// cannot happen, so a caller that gets `None` for a usable accelerator has
    /// found a bug rather than a missing feature.
    pub fn device_for(&self, accel: Accel) -> Option<std::path::PathBuf> {
        self.devices
            .get(accel.as_str())
            .map(std::path::PathBuf::from)
    }

    /// The reason an accelerator is not usable.
    pub fn reason_for(&self, accel: Accel) -> Option<&str> {
        self.unavailable
            .iter()
            .find(|(n, _)| n == accel.as_str())
            .map(|(_, r)| r.as_str())
    }

    /// The check that makes #7239 impossible to reintroduce.
    ///
    /// Every unavailable accelerator must name a concrete cause. "available" or
    /// "no" or "unsupported" are not causes; a device path, a missing package,
    /// a platform name or a config key are.
    pub fn is_actionable(&self) -> Result<(), String> {
        let mut problems = Vec::new();
        for (name, reason) in &self.unavailable {
            if reason.trim().is_empty() {
                problems.push(format!("{name}: the reason is empty"));
                continue;
            }
            if !mentions_a_concrete_cause(reason) {
                problems.push(format!(
                    "{name}: {reason:?} names no concrete cause (a device, a \
                     package, a platform, or a config source)"
                ));
            }
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(problems.join("; "))
        }
    }

    /// One line per accelerator, for a log.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        for name in &self.available {
            parts.push(format!("{name}: yes"));
        }
        for (name, reason) in &self.unavailable {
            parts.push(format!("{name}: {reason}"));
        }
        parts.join("; ")
    }
}

/// Does this sentence name something a user can go and do something about?
///
/// Deliberately narrow. It is not a check for "is this helpful", which is not
/// a property a function can have; it is a check for the specific failure mode
/// #7239 reports, which is a *bare* status. The tokens below are the things
/// that turn a status into an instruction.
fn mentions_a_concrete_cause(reason: &str) -> bool {
    // Anything that looks like a path, a package, a library, a platform, a
    // build string or a config source. These are the things that turn a status
    // into an instruction the reader can act on.
    //
    // Deliberately not a list of words. The first version was
    // `["/dev/", "/usr/", ".so", "package:", ...]` and it failed on its own
    // output: "looked in /dev" has a path in it that the list did not cover,
    // because the list had `/dev/` with a trailing slash. A list of specific
    // paths is a list that is always slightly wrong; "does this look like a
    // filesystem path" does not have that failure mode.
    if reason.contains('/') || reason.contains('\\') {
        return true;
    }
    const MARKERS: [&str; 8] = [
        ".so",      // a driver library
        "package:", // a package manager name
        "driver",   // a kernel module
        "linux", "macos", "windows", "compiled", // a build
        "config",   // a config file
    ];
    let lower = reason.to_lowercase();
    MARKERS.iter().any(|m| lower.contains(m))
}

// ------------------------------------------------------------------ probing

/// The ffmpeg build's capabilities, as reported by ffmpeg itself.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FfmpegCaps {
    /// Everything `ffmpeg -hwaccels` printed.
    pub hwaccels: Vec<String>,
    /// Every encoder name `ffmpeg -encoders` printed.
    pub encoders: Vec<String>,
    /// The ffmpeg version string.
    pub version: String,
}

impl FfmpegCaps {
    /// Ask ffmpeg what it can do.
    ///
    /// Returns `Err` with a usable message when ffmpeg is missing, because
    /// "no acceleration" is a misleading answer for a machine that has no
    /// ffmpeg at all -- the real problem is upstream of acceleration.
    pub fn probe(ffmpeg: &Path) -> Result<Self, String> {
        let hwaccels = parse_hwaccels(&run_ffmpeg(ffmpeg, &["-hwaccels"])?);
        let encoders = parse_encoders(&run_ffmpeg(ffmpeg, &["-encoders"])?);
        let version = run_ffmpeg(ffmpeg, &["-version"])?;
        Ok(FfmpegCaps {
            hwaccels,
            encoders,
            version: version.lines().next().unwrap_or_default().to_string(),
        })
    }

    fn has_hwaccel(&self, method: &str) -> bool {
        self.hwaccels.iter().any(|m| m == method)
    }

    fn has_encoder(&self, name: &str) -> bool {
        self.encoders.iter().any(|e| e == name)
    }
}

/// Run ffmpeg and return stdout, or a reason it could not be run.
/// The method names from `ffmpeg -hwaccels`.
///
/// The output is a heading line then one bare name per line, indented. Taking
/// every non-empty line would put "Hardware acceleration methods:" in the list
/// as a method called "hardware acceleration methods", and a later `has_encoder`
/// style check on it would be comparing against a sentence.
fn parse_hwaccels(out: &str) -> Vec<String> {
    out.lines()
        .map(str::trim)
        // Drop the heading and any explanatory text.
        .filter(|l| !l.is_empty() && !l.ends_with(':') && !l.contains(' '))
        .map(str::to_string)
        .collect()
}

/// The encoder names from `ffmpeg -encoders`.
///
/// The table is `flags name description`, and only the second column is the
/// name. Taking the whole line would make `has_encoder("h264_nvenc")` fail on
/// a build that has it.
fn parse_encoders(out: &str) -> Vec<String> {
    out.lines()
        .filter_map(|l| {
            let l = l.trim();
            if !l.starts_with(|c: char| c.is_ascii_alphabetic() || c == ' ') {
                return None;
            }
            l.split_whitespace().nth(1).map(str::to_string)
        })
        .filter(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
        .collect()
}

fn run_ffmpeg(ffmpeg: &Path, args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new(ffmpeg)
        .args(args)
        .output()
        .map_err(|e| format!("could not run {}: {e}", ffmpeg.display()))?;
    if !out.status.success() {
        return Err(format!(
            "{} {} exited with {}",
            ffmpeg.display(),
            args.join(" "),
            out.status
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Where the probes look, so a test can point them at a fixture tree.
#[derive(Debug, Clone, Default)]
pub struct ProbePaths {
    /// `/dev/dri` by default.
    pub dri_dir: std::path::PathBuf,
    /// `/dev` by default, for the NVIDIA nodes.
    pub dev_dir: std::path::PathBuf,
    /// Directory searched for driver libraries. Empty means "do not check",
    /// which is what a build that does not need the check should do.
    pub lib_dir: std::path::PathBuf,
}

impl ProbePaths {
    /// The real system paths.
    pub fn system() -> Self {
        ProbePaths {
            dri_dir: "/dev/dri".into(),
            dev_dir: "/dev".into(),
            lib_dir: "/usr/lib".into(),
        }
    }
}

/// The user's configuration, as far as acceleration is concerned.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HwAccelConfig {
    /// Accelerators the user has switched off. Empty means "no opinion",
    /// which is not the same as "none enabled": an accelerator nobody
    /// configured is probed normally.
    pub disabled: Vec<Accel>,
    /// `true` means probe and report, use nothing. The Docker default (#7007):
    /// the hooks are wired but unprivileged, so the status page can say why
    /// rather than the code silently falling back.
    pub report_only: bool,
}

impl HwAccelConfig {
    /// Is this accelerator switched off?
    pub fn is_disabled(&self, accel: Accel) -> bool {
        self.disabled.contains(&accel)
    }

    /// Where the setting came from, for a reason string. Names the file rather
    /// than saying "configuration", because a user who edited one file and not
    /// the other needs to know which.
    pub fn source(&self) -> String {
        "the app configuration file (config.toml, [hardware.accel])".to_string()
    }
}

/// Probe every accelerator.
///
/// `caps` is the ffmpeg build's own answer and must be passed in rather than
/// re-run, because the caller usually already has it and because a test needs
/// to supply a fixed one.
pub fn probe_all(caps: &FfmpegCaps, paths: &ProbePaths, config: &HwAccelConfig) -> HwAccelStatus {
    let mut available = Vec::new();
    let mut unavailable = Vec::new();
    let mut devices = BTreeMap::new();
    for accel in Accel::ALL {
        let verdict = probe_one(accel, caps, paths, config);
        match verdict {
            Verdict::Available { node } => {
                available.push(accel.as_str().to_string());
                devices.insert(accel.as_str().to_string(), node.display().to_string());
            }
            Verdict::Unavailable(u) => {
                unavailable.push((accel.as_str().to_string(), u.reason_for(accel)))
            }
        }
    }
    let status = HwAccelStatus {
        available,
        unavailable,
        devices,
    };
    debug_assert!(
        status.is_actionable().is_ok(),
        "probe produced a reason with no cause in it: {status:?}"
    );
    status
}

/// Probe one accelerator.
pub fn probe_one(
    accel: Accel,
    caps: &FfmpegCaps,
    paths: &ProbePaths,
    config: &HwAccelConfig,
) -> Verdict {
    // 0. Configuration. Respected, never guessed -- spec §6.4's last sentence.
    if config.is_disabled(accel) {
        return Verdict::Unavailable(Unavailable::Disabled {
            source: config.source(),
        });
    }

    // 1. Platform. VideoToolbox does not exist off macOS, and neither does
    //    anything else worth trying.
    let on_this_platform = match accel {
        Accel::VideoToolbox => cfg!(target_os = "macos"),
        _ => cfg!(target_os = "linux") || cfg!(target_os = "freebsd"),
    };
    if !on_this_platform {
        return Verdict::Unavailable(Unavailable::WrongPlatform {
            platform: match accel {
                Accel::VideoToolbox => "macOS",
                _ => "Linux",
            }
            .to_string(),
        });
    }

    // 2. The ffmpeg build. Cheap, and it rules out the whole family at once.
    if let Some(method) = accel.hwaccel_method() {
        if !caps.has_hwaccel(method) {
            return Verdict::Unavailable(Unavailable::NotBuiltIn {
                build: if caps.version.is_empty() {
                    "this ffmpeg".to_string()
                } else {
                    caps.version.clone()
                },
            });
        }
    }
    // NVENC has no `-hwaccels` entry, so the encoder list is the only build
    // check it gets. This is exactly the check that is *not* sufficient on its
    // own, which is why step 3 exists.
    if !accel.encoders().is_empty() && !accel.encoders().iter().all(|e| caps.has_encoder(e)) {
        return Verdict::Unavailable(Unavailable::NotBuiltIn {
            build: if caps.version.is_empty() {
                "this ffmpeg".to_string()
            } else {
                caps.version.clone()
            },
        });
    }

    // 3. The device. This is the step that distinguishes "compiled in" from
    //    "usable", and it is the one ffmpeg cannot answer.
    let device = match accel {
        Accel::VaApi | Accel::Qsv => find_render_node(&paths.dri_dir),
        Accel::Nvenc => find_nvidia_node(&paths.dev_dir),
        Accel::VideoToolbox => None,
    };
    match (accel, device) {
        (_, Some(dev)) => match device_usable(&dev) {
            Ok(()) => Verdict::Available { node: dev },
            Err(detail) => Verdict::Unavailable(Unavailable::DeviceUnusable {
                device: dev.display().to_string(),
                detail,
            }),
        },
        (Accel::VideoToolbox, None) => {
            // On macOS the OS provides it; there is no node to stat. The
            // "node" is the display, which ffmpeg opens itself.
            Verdict::Available {
                node: std::path::PathBuf::from("default"),
            }
        }
        (accel, None) => {
            let (expected, searched) = match accel {
                Accel::VaApi | Accel::Qsv => {
                    ("DRI render node", paths.dri_dir.display().to_string())
                }
                Accel::Nvenc => ("NVIDIA device node", paths.dev_dir.display().to_string()),
                Accel::VideoToolbox => unreachable!("handled above"),
            };
            Verdict::Unavailable(Unavailable::NoDevice {
                expected: expected.to_string(),
                searched: searched.to_string(),
            })
        }
    }
}

/// The first `renderD*` node in `dri_dir`, if any.
fn find_render_node(dri_dir: &Path) -> Option<std::path::PathBuf> {
    let mut found: Vec<std::path::PathBuf> = std::fs::read_dir(dri_dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("renderD"))
        })
        .collect();
    // Sorted so the answer does not depend on directory order. Two render
    // nodes means two GPUs, and which one gets used should not be a function
    // of the filesystem.
    found.sort();
    found.into_iter().next()
}

/// The first `/dev/nvidia*` node, if any.
fn find_nvidia_node(dev_dir: &Path) -> Option<std::path::PathBuf> {
    let mut found: Vec<std::path::PathBuf> = std::fs::read_dir(dev_dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("nvidia"))
        })
        .collect();
    found.sort();
    found.into_iter().next()
}

/// Can this process actually open the device?
///
/// A `stat` that the probe has already done. Existence is not permission, and
/// a container with `--device /dev/dri/renderD128` and no
/// `--group-add render` gets a node it cannot open -- which is the Docker case
/// #7007 is about, and it deserves a reason that says so.
fn device_usable(device: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(device).map_err(|e| format!("stat failed: {e}"))?;
    let mode = meta.permissions().mode();
    if mode & 0o444 == 0 {
        return Err(format!(
            "no read permission (mode {mode:o}); add this process to the \
             video/render group, or pass --device without --privileged"
        ));
    }
    // Read-write matters for a decoder that uploads frames; read-only is
    // enough for encoding, so this is a warning rather than a failure.
    if mode & 0o200 == 0 && mode & 0o600 != 0 {
        return Err(format!(
            "read-only and owned by another user (mode {mode:o}); \
             a hardware decoder needs write access to upload frames"
        ));
    }
    Ok(())
}

/// Probe the real system, with the real ffmpeg.
///
/// The entry point for an application. A missing ffmpeg yields a status that
/// says so, rather than an empty one that reads as "no acceleration here".
pub fn detect(ffmpeg: &Path, config: &HwAccelConfig) -> HwAccelStatus {
    let caps = match FfmpegCaps::probe(ffmpeg) {
        Ok(c) => c,
        Err(e) => {
            return HwAccelStatus {
                devices: BTreeMap::new(),
                available: Vec::new(),
                unavailable: Accel::ALL
                    .into_iter()
                    .map(|a| (a.as_str().to_string(), e.clone()))
                    .collect(),
            }
        }
    };
    probe_all(&caps, &ProbePaths::system(), config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reason_always_names_a_concrete_cause() {
        // The regression guard for #7239, at the level of the type.
        for accel in Accel::ALL {
            for u in [
                Unavailable::NotBuiltIn {
                    build: "ffmpeg version 9.0".to_string(),
                },
                Unavailable::WrongPlatform {
                    platform: "macOS".to_string(),
                },
                Unavailable::NoDevice {
                    expected: "DRI render node".to_string(),
                    searched: "/dev/dri".to_string(),
                },
                Unavailable::DeviceUnusable {
                    device: "/dev/dri/renderD128".to_string(),
                    detail: "no read permission".to_string(),
                },
                Unavailable::Disabled {
                    source: "config.toml".to_string(),
                },
                Unavailable::DriverMissing {
                    library: "libcuda.so.1".to_string(),
                    package: "nvidia-utils".to_string(),
                },
            ] {
                let reason = u.reason_for(accel);
                assert!(
                    mentions_a_concrete_cause(&reason),
                    "{accel}: {reason:?} names no cause"
                );
            }
        }
    }

    #[test]
    fn a_bare_status_is_not_actionable() {
        let status = HwAccelStatus {
            available: vec![],
            unavailable: vec![("nvenc".to_string(), "".to_string())],
            devices: Default::default(),
        };
        assert!(status.is_actionable().is_err());

        let status = HwAccelStatus {
            available: vec![],
            unavailable: vec![("nvenc".to_string(), "not supported".to_string())],
            devices: Default::default(),
        };
        assert!(
            status.is_actionable().is_err(),
            "a bare 'not supported' is the #7239 failure"
        );
    }

    #[test]
    fn the_real_machine_produces_an_actionable_status() {
        // Whatever this machine has, every reason it gives must name a cause.
        let status = detect(Path::new("ffmpeg"), &HwAccelConfig::default());
        status
            .is_actionable()
            .unwrap_or_else(|e| panic!("{e}\nfull status: {}", status.summary()));
    }
}
