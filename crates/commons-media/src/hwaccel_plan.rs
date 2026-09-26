//! Turning a detected accelerator into ffmpeg arguments.
//!
//! # What acceleration can and cannot do to a thumbnail
//!
//! The thumbnail pipeline is `decode -> scale -> encode to WebP`. Probing
//! [`hwaccel`] tells you what hardware exists; this module decides what to do
//! about it, and the answer is not "add some flags".
//!
//! **`ffmpeg -encoders` on this project's own ffmpeg lists `h264_nvenc`,
//! `h264_vaapi`, `h264_qsv` and no `webp` variant of any of them.** WebP has no
//! hardware encoder in ffmpeg at all. So the encode step is *always* software,
//! no matter what the GPU can do, and any plan that swaps `-c:v libwebp` for a
//! hardware encoder is not an optimisation -- it is a plan that stops
//! producing WebP.
//!
//! What acceleration genuinely buys here is the two steps around it:
//!
//! | step | hardware | why it matters |
//! |---|---|---|
//! | decode | yes | H.264/HEVC are the expensive part; on a large scan this is the bulk of the wall clock |
//! | scale | yes | `scale_vaapi` avoids a round trip through system memory |
//! | encode to WebP | **no** | no hardware WebP encoder exists |
//!
//! So a plan names the *decode* method, the *scale* filter, and leaves the
//! encoder alone. The alpha guarantee in
//! [`super::thumbs`] (`-pix_fmt yuva420p`) is unaffected, because WebP is
//! still WebP.
//!
//! # Why the plan is a value and not a flag soup
//!
//! Because the combination has to be *coherent*. `-hwaccel vaapi` with
//! `-vf scale=...` runs, and silently ignores the hardware: ffmpeg allocates
//! the hardware device, uploads, then does the scale on the CPU anyway. The
//! user sees a slower machine with "VA-API: on" in the settings, which is a
//! worse failure than being off, because they have no reason to look.
//!
//! [`AccelPlan::args`] is therefore the only place that builds a flag list,
//! and it returns nothing at all for an unavailable accelerator. A caller
//! cannot accidentally pass half a plan.

use std::fmt;
use std::path::Path;

use super::hwaccel::{probe_all, Accel, FfmpegCaps, HwAccelConfig, HwAccelStatus, ProbePaths};
use super::thumbs::Kind;

/// What kind of input the pipeline is being asked to accelerate.
///
/// Only the distinction that changes the plan. The pixel format of the decoded
/// frames is a property of the codec, and every codec this project handles
/// decodes to a planar YUV format the hardware scaler accepts, so there is
/// nothing to parameterise beyond "is there a decoder to accelerate at all".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    /// An image. PNG, JPEG, WebP.
    ///
    /// There is no decode step to move to a GPU: a PNG is inflated by libpng.
    /// Passing `-hwaccel` to ffmpeg for a still is not a no-op, it is an error
    /// on some builds and a silent no-op on others.
    Still,
    /// A video. The case acceleration is for.
    Video,
}

/// What a plan will actually do.
///
/// Distinct from `Accel`: `Accel` names hardware, this names a coherent way to
/// use it, and a plan is only ever `Software` or one of the two real paths.
/// The four-to-two collapse is deliberate, because `NVENC without a scale
/// filter` and `NVENC with one` are not the same thing and pretending
/// otherwise is how the silent-fallback bug happens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccelPlan {
    /// No hardware. The flags the pipeline uses today, unchanged.
    Software,
    /// Frames are decoded on the GPU and the scale happens there too.
    ///
    /// Only sound for a real video pipeline; for a still image input the
    /// decode is already trivial and the scale is the only cost, so hardware
    /// can lose to a well-tuned CPU scale.
    Hardware {
        accel: Accel,
        /// The device the probe chose, carried rather than re-discovered.
        ///
        /// Re-probing at generation time would be a second, racy lookup: a
        /// machine with two render nodes can enumerate them in a different
        /// order, and a plan that names a different node than the status does
        /// is the "acceleration: on, machine getting slower" bug. The status
        /// and the plan are built from one probe, and this is the join.
        node: std::path::PathBuf,
        scale_on_device: bool,
    },
}

impl AccelPlan {
    /// The accelerator this plan uses, if any.
    pub fn accel(&self) -> Option<Accel> {
        match self {
            AccelPlan::Software => None,
            AccelPlan::Hardware { accel, .. } => Some(*accel),
        }
    }

    /// The device node this plan uses.
    pub fn node(&self) -> Option<&Path> {
        match self {
            AccelPlan::Software => None,
            AccelPlan::Hardware { node, .. } => Some(node),
        }
    }

    /// Will this plan actually put frames on the GPU?
    pub fn is_hardware(&self) -> bool {
        matches!(self, AccelPlan::Hardware { .. })
    }

    /// The ffmpeg arguments for this plan, in three groups.
    ///
    /// **Order matters and ffmpeg will not tell you when you get it wrong.**
    /// `-init_hw_device` and `-filter_hw_device` are *global* options: they go
    /// before `-hide_banner`. `-hwaccel*` are *per-input*: they go after the
    /// global block and before `-i`. Putting either group in the wrong place
    /// makes ffmpeg exit with `Error parsing global options: Invalid argument`,
    /// which says nothing about which option was misplaced -- so the grouping
    /// is produced here as three separate lists and a caller cannot interleave
    /// them wrongly.
    ///
    /// The third group is a suffix for the filter chain, because
    /// `scale_vaapi` hands its output back as a hardware frame and the encoder
    /// cannot consume one. See [`AccelPlan::filter_suffix`].
    pub fn global_args(&self) -> Vec<String> {
        let AccelPlan::Hardware { accel, .. } = *self else {
            return Vec::new();
        };
        let Some(node) = self.node() else {
            return Vec::new();
        };
        // The `va:` prefix names the *default* VA display for the node. Without
        // it ffmpeg picks device 0 of the whole DRM subsystem, which on a
        // multi-GPU machine is a different card than the render node the probe
        // looked at -- and on this machine it fails outright, because the
        // only DRM device is card1 and there is no VA display on index 0.
        vec![
            "-init_hw_device".to_string(),
            accel.device_spec(node),
            "-filter_hw_device".to_string(),
            accel.device_handle().to_string(),
        ]
    }

    /// Input options, to be inserted after [`AccelPlan::global_args`] and
    /// before `-i`.
    ///
    /// Empty for [`AccelPlan::Software`], which is the signal to a caller that
    /// there is nothing to add rather than something to remove.
    pub fn input_args(&self) -> Vec<String> {
        let AccelPlan::Hardware {
            accel,
            scale_on_device,
            ..
        } = self
        else {
            return Vec::new();
        };
        let Some(method) = accel.hwaccel_method() else {
            // NVENC is an encoder, not a decode method. `-hwaccel nvenc` is
            // an error, and the plan already declines to scale on the device,
            // so there is nothing to add.
            return Vec::new();
        };
        let mut args = vec![
            "-hwaccel".to_string(),
            method.to_string(),
            "-hwaccel_device".to_string(),
            accel.device_handle().to_string(),
        ];
        if *scale_on_device {
            // `vaapi`, not `nv12`. The device wants its own surface format; a
            // software pixel format means ffmpeg copies every frame back to
            // system memory and the "hardware" path is a slower software one.
            // The copy is undone by the `hwdownload` in `filter_suffix`.
            args.push("-hwaccel_output_format".to_string());
            args.push(accel.hw_surface_format().to_string());
        }
        args
    }

    /// The filter-chain suffix needed after a hardware scaler.
    ///
    /// Empty unless [`AccelPlan::is_hardware`] with the scale on the device.
    /// Without it the encoder is handed a hardware frame it cannot read, and
    /// the whole run ends with `Nothing was written into output file, because
    /// at least one of its streams received no packets` -- an error that names
    /// the encoder when the fault is two filters earlier in the chain.
    pub fn filter_suffix(&self) -> &'static str {
        match self {
            AccelPlan::Hardware {
                scale_on_device: true,
                ..
            } => "hwdownload,format=nv12",
            _ => "",
        }
    }

    /// The `-vf` value for this plan.
    ///
    /// The hardware scaler replaces `scale` and *is* a filter chain element, so
    /// it is a drop-in for `scale=W:-2:flags=lanczos` -- but it does not accept
    /// `flags=`, because there is no software implementation to tune. A plan
    /// that emitted `scale_vaapi=W:-2:flags=lanczos` fails at runtime.
    pub fn scale_filter(&self, width: u32, rest: &str) -> String {
        let base = format!("scale={width}:{rest}");
        match self {
            AccelPlan::Software => format!("{base}:flags=lanczos"),
            AccelPlan::Hardware {
                scale_on_device: false,
                ..
            } => format!("{base}:flags=lanczos"),
            AccelPlan::Hardware {
                accel,
                scale_on_device: true,
                ..
            } => match accel {
                Accel::VaApi => format!("scale_vaapi={width}:{rest}"),
                Accel::Qsv => format!("scale_qsv={width}:{rest}"),
                // NVENC and VideoToolbox are encoders. There is no
                // `scale_nvenc`, and writing one would make ffmpeg exit with
                // "No such filter", so they decode on the GPU and scale on the
                // CPU. That is a real plan, not a degraded one.
                Accel::Nvenc | Accel::VideoToolbox => format!("{base}:flags=lanczos"),
            },
        }
    }

    /// The encoder name for this plan.
    ///
    /// Always `libwebp`, and deliberately not a parameter. See the module
    /// docs: there is no hardware WebP encoder, so a function taking an
    /// encoder would invite a caller to pass `h264_vaapi` and get a JPEG
    /// named `.webp`.
    pub fn encoder(&self) -> &'static str {
        "libwebp"
    }

    /// Build the full filter chain for a `Kind`.
    ///
    /// **The order is the whole of the correctness here.** Two rules, both
    /// learned from a filter chain that produced
    /// `Nothing was written into output file, because at least one of its
    /// streams received no packets`:
    ///
    /// 1. A hardware scaler returns a *hardware frame*. A software encoder
    ///    cannot read one, so `hwdownload` has to follow it.
    /// 2. A software filter that is not frame-format agnostic -- `tile` above
    ///    all -- cannot read a hardware frame either. So `hwdownload` has to
    ///    come *before* any software filter, not merely before the encoder.
    ///
    /// Rule 2 is why the suffix is inserted before the `tile` rather than
    /// appended at the end. Appending it works for a single frame, because
    /// there is no `tile`, and fails for a sprite, which is the artifact a
    /// scrubber reads while the user waits. A filter chain that works for
    /// thumbnails and silently produces nothing for sprites is the worst of
    /// the available outcomes.
    pub fn filter_chain(&self, kind: &Kind, width: u32, leading: &[&str]) -> String {
        let mut parts: Vec<String> = leading.iter().map(|s| s.to_string()).collect();
        parts.push(self.scale_filter(width, "-2"));
        // Back to system memory before the first software filter, not after
        // the last one.
        if !self.filter_suffix().is_empty() {
            parts.push(self.filter_suffix().to_string());
        }
        if let Kind::Sprite { frames, .. } = kind {
            parts.push(format!("tile={frames}x1"));
        }
        parts.join(",")
    }
}

impl fmt::Display for AccelPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AccelPlan::Software => f.write_str("software"),
            AccelPlan::Hardware {
                accel,
                node,
                scale_on_device,
            } => write!(
                f,
                "{} on {} (scale {})",
                accel.display_name(),
                node.display(),
                if *scale_on_device {
                    "on device"
                } else {
                    "on cpu"
                }
            ),
        }
    }
}

/// Choosing a plan.
///
/// Kept apart from [`AccelPlan`] because *choosing* consults configuration and
/// the *coherence* rules, while a plan is just a value. Every caller that has
/// a plan should have gone through here.
#[derive(Debug, Clone)]
pub struct Planner {
    status: HwAccelStatus,
    /// `report_only` is the Docker default (#7007): the image has the hooks
    /// wired, the process is unprivileged, and the useful thing is to *report*
    /// the status rather than to use it. A plan built under `report_only` is
    /// always [`AccelPlan::Software`].
    report_only: bool,
}

impl Planner {
    /// Build a planner from an already-probed status.
    pub fn new(status: HwAccelStatus, config: &HwAccelConfig) -> Self {
        Planner {
            status,
            report_only: config.report_only,
        }
    }

    /// Probe the real machine and build a planner.
    pub fn detect(ffmpeg: &std::path::Path, config: &HwAccelConfig) -> Self {
        let caps = FfmpegCaps::probe(ffmpeg).unwrap_or_default();
        let status = probe_all(&caps, &ProbePaths::system(), config);
        Planner::new(status, config)
    }

    /// The status this planner was built from. The settings page reads this.
    pub fn status(&self) -> &HwAccelStatus {
        &self.status
    }

    /// The plan for a `Kind` of a source with known `duration_ms`.
    ///
    /// Returns [`AccelPlan::Software`] unless the accelerator is available,
    /// configuration allows it, and the source can benefit. Each of those is a
    /// separate reason to decline, and declining is the normal case: this
    /// function says "no" far more often than "yes".
    pub fn plan(&self, _kind: &Kind, media: &super::probe::MediaInfo) -> AccelPlan {
        self.plan_for_source(if media.duration_ms == 0 {
            SourceKind::Still
        } else {
            SourceKind::Video
        })
    }

    /// The plan for a known source kind.
    pub fn plan_for_source(&self, source: SourceKind) -> AccelPlan {
        // Configuration, first. A user who switched acceleration off gets a
        // software plan even on a machine where it would work, and that is the
        // point of the setting.
        if self.report_only {
            return AccelPlan::Software;
        }
        // A still cannot be accelerated. Declining here rather than emitting
        // flags ffmpeg will reject is the difference between a status page
        // that works and one that reports a filter error.
        if source == SourceKind::Still {
            return AccelPlan::Software;
        }
        // The preference order. VA-API first: it is the one with a real
        // hardware *scaler*, so the scale moves onto the device too, which is
        // most of the point.
        const PREFERENCE: [Accel; 3] = [Accel::VaApi, Accel::Qsv, Accel::Nvenc];
        for accel in PREFERENCE {
            if !self.status.has(accel) {
                continue;
            }
            // The device comes from the same probe as the status. A plan
            // without one would have to re-discover it, and could pick a
            // different node than the status named.
            let Some(node) = self.status.device_for(accel) else {
                continue;
            };
            return AccelPlan::Hardware {
                accel,
                node,
                // Only VA-API and QSV have a hardware scaler. NVENC has no
                // `scale_nvenc`, and this is the flag that has to be right for
                // the plan to be coherent.
                scale_on_device: matches!(accel, Accel::VaApi | Accel::Qsv),
            };
        }
        AccelPlan::Software
    }

    /// Why a plan was not the fastest available, for the settings page.
    ///
    /// The same contract as the status reasons: a reason, never an empty
    /// string. `None` when the plan *is* the best one, so a caller can show
    /// nothing rather than showing a blank row.
    pub fn declined_reason(&self, source: SourceKind) -> Option<String> {
        if self.report_only {
            return Some(
                "hardware acceleration is set to report-only; no jobs use it (Docker images \
                 run unprivileged, so the settings page can explain rather than the code \
                 failing silently)"
                    .to_string(),
            );
        }
        if source == SourceKind::Still {
            return Some(
                "stills are not accelerated: there is no decode step to move to the GPU"
                    .to_string(),
            );
        }
        if self.status.available.is_empty() {
            return Some(format!(
                "no accelerator on this machine -- {}",
                self.status
                    .unavailable
                    .iter()
                    .map(|(n, r)| format!("{n}: {r}"))
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::super::hwaccel::Unavailable;
    use super::*;

    fn vaapi() -> AccelPlan {
        AccelPlan::Hardware {
            accel: Accel::VaApi,
            node: "/dev/dri/renderD128".into(),
            scale_on_device: true,
        }
    }

    fn nvenc() -> AccelPlan {
        AccelPlan::Hardware {
            accel: Accel::Nvenc,
            node: "/dev/nvidia0".into(),
            scale_on_device: false,
        }
    }

    fn status_with(available: &[&str], devices: &[(&str, &str)]) -> HwAccelStatus {
        let mut m = std::collections::BTreeMap::new();
        for (k, v) in devices {
            m.insert(k.to_string(), v.to_string());
        }
        HwAccelStatus {
            available: available.iter().map(|s| s.to_string()).collect(),
            unavailable: vec![],
            devices: m,
        }
    }

    #[test]
    fn a_software_plan_emits_nothing_anywhere() {
        let p = AccelPlan::Software;
        assert!(p.global_args().is_empty());
        assert!(p.input_args().is_empty());
        assert_eq!(p.filter_suffix(), "");
        assert!(!p.is_hardware());
        assert_eq!(p.accel(), None);
        assert_eq!(p.node(), None);
        assert_eq!(p.encoder(), "libwebp");
    }

    /// The three groups ffmpeg needs, in the order ffmpeg needs them.
    ///
    /// This is the shape the whole module exists to get right: a misplaced
    /// global option gives `Error parsing global options: Invalid argument`,
    /// which names neither the option nor the position.
    #[test]
    fn a_vaapi_plan_is_three_coherent_groups() {
        let p = vaapi();
        assert_eq!(
            p.global_args(),
            vec![
                "-init_hw_device",
                "vaapi=va:/dev/dri/renderD128",
                "-filter_hw_device",
                "va"
            ]
        );
        assert_eq!(
            p.input_args(),
            vec![
                "-hwaccel",
                "vaapi",
                "-hwaccel_device",
                "va",
                "-hwaccel_output_format",
                "vaapi"
            ]
        );
        assert_eq!(p.filter_suffix(), "hwdownload,format=nv12");
    }

    /// The hardware scaler rejects `flags=`, so the filter must not carry it.
    #[test]
    fn a_hardware_scale_filter_carries_no_flags() {
        let f = vaapi().scale_filter(120, "-2");
        assert_eq!(f, "scale_vaapi=120:-2");
        assert!(!f.contains("flags"), "scale_vaapi rejects flags=: {f}");
    }

    /// The software filter keeps its flags, and is otherwise unchanged from
    /// what the pipeline used before acceleration existed.
    #[test]
    fn a_software_scale_filter_is_unchanged() {
        assert_eq!(
            AccelPlan::Software.scale_filter(120, "-2"),
            "scale=120:-2:flags=lanczos"
        );
        // A hardware plan whose *scale* is on the CPU keeps the CPU filter.
        assert_eq!(
            nvenc().scale_filter(120, "-2"),
            "scale=120:-2:flags=lanczos"
        );
    }

    /// NVENC is an encoder, not a decode method. `-hwaccel nvenc` is an error,
    /// and `scale_nvenc` does not exist as a filter.
    ///
    /// Asserted over *every* plan rather than for the one accelerator this
    /// machine happens to have. The acceptance tests drive real ffmpeg, and on a
    /// machine with no NVIDIA card the NVENC arm is never reached -- which is
    /// exactly how `scale_nvenc` would ship: a filter that does not exist,
    /// reached only on hardware nobody tested.
    #[test]
    fn an_nvenc_plan_invents_neither_a_decode_method_nor_a_scaler() {
        let p = nvenc();
        assert!(
            p.input_args().is_empty(),
            "-hwaccel nvenc would be rejected: {:?}",
            p.input_args()
        );
        assert_eq!(p.filter_suffix(), "");

        // No accelerator may produce a filter ffmpeg does not have. The
        // complete set of hardware scalers ffmpeg ships is vaapi and qsv.
        const REAL_HW_SCALERS: [&str; 2] = ["scale_vaapi", "scale_qsv"];
        for accel in Accel::ALL {
            for scale_on_device in [true, false] {
                let plan = AccelPlan::Hardware {
                    accel,
                    node: "/dev/dri/renderD128".into(),
                    scale_on_device,
                };
                let filter = plan.scale_filter(120, "-2");
                let invented = filter.starts_with("scale_") && filter.contains(accel.as_str());
                if invented {
                    assert!(
                        REAL_HW_SCALERS.contains(&filter.split('=').next().unwrap()),
                        "{accel} produced {filter:?}, which is not a real ffmpeg filter"
                    );
                }
                // And no accelerator may claim a decode method ffmpeg lacks.
                if accel.hwaccel_method().is_none() {
                    assert!(
                        plan.input_args().is_empty(),
                        "{accel} has no decode method but emitted input args"
                    );
                }
            }
        }
    }

    /// There is no hardware WebP encoder, so the encoder never changes.
    #[test]
    fn the_encoder_is_never_a_hardware_one() {
        for accel in Accel::ALL {
            for scale_on_device in [true, false] {
                let p = AccelPlan::Hardware {
                    accel,
                    node: "/dev/dri/renderD128".into(),
                    scale_on_device,
                };
                assert_eq!(
                    p.encoder(),
                    "libwebp",
                    "{accel} must not change the output format"
                );
            }
        }
    }

    /// The download goes *before* the first software filter, not at the end.
    ///
    /// Appending it is the intuitive thing to write and it is wrong: it works
    /// for a single frame and produces an empty file for a sprite, because
    /// `tile` cannot read a hardware frame. That is the worst outcome, since
    /// the thumbnail path looks fine.
    #[test]
    fn the_download_precedes_every_software_filter() {
        let sprite = Kind::Sprite {
            width: 100,
            frames: 4,
        };
        let chain = vaapi().filter_chain(&sprite, 100, &["fps=0.5"]);
        assert!(chain.starts_with("fps=0.5,"), "{chain}");
        assert!(chain.contains("scale_vaapi=100:-2"), "{chain}");

        let download = chain.find("hwdownload").expect("a download step");
        let tile = chain.find("tile=4x1").expect("a tile step");
        assert!(
            download < tile,
            "hwdownload must precede tile, or tile reads a hardware frame: {chain}"
        );

        // And with no software filter after the scale, the download is last.
        let plain = vaapi().filter_chain(&Kind::Thumbnail { width: 120 }, 120, &[]);
        assert_eq!(plain, "scale_vaapi=120:-2,hwdownload,format=nv12");
    }

    #[test]
    fn a_software_chain_is_unchanged() {
        let sprite = Kind::Sprite {
            width: 100,
            frames: 4,
        };
        let sw = AccelPlan::Software.filter_chain(&sprite, 100, &["fps=0.5"]);
        assert_eq!(
            sw, "fps=0.5,scale=100:-2:flags=lanczos,tile=4x1",
            "the software chain must be byte-identical to the pre-acceleration one"
        );
        assert!(!sw.contains("hwdownload"), "{sw}");
    }

    /// The device the probe found is the device the plan names.
    ///
    /// Re-probing at generation time is the bug this prevents: a machine with
    /// two render nodes can enumerate them in a different order, and a plan
    /// that names a different node than the status is a machine that got slower
    /// when the user turned acceleration on.
    #[test]
    fn the_plan_names_the_node_the_status_recorded() {
        let status = status_with(&["vaapi"], &[("vaapi", "/dev/dri/renderD128")]);
        let planner = Planner::new(status, &HwAccelConfig::default());
        let plan = planner.plan_for_source(SourceKind::Video);
        assert_eq!(plan.node().unwrap(), Path::new("/dev/dri/renderD128"));
        assert!(plan
            .global_args()
            .iter()
            .any(|a| a.contains("/dev/dri/renderD128")));
    }

    #[test]
    fn report_only_always_declines_and_says_why() {
        let status = status_with(&["vaapi"], &[("vaapi", "/dev/dri/renderD128")]);
        let config = HwAccelConfig {
            disabled: vec![],
            report_only: true,
        };
        let planner = Planner::new(status, &config);
        assert_eq!(
            planner.plan_for_source(SourceKind::Video),
            AccelPlan::Software
        );
        let reason = planner.declined_reason(SourceKind::Video).unwrap();
        assert!(reason.contains("report-only"), "{reason:?}");
    }

    #[test]
    fn a_declined_plan_always_has_a_reason() {
        let config = HwAccelConfig::default();
        let empty = HwAccelStatus {
            available: vec![],
            unavailable: vec![("vaapi".to_string(), "needs a DRI render node".to_string())],
            devices: Default::default(),
        };
        let planner = Planner::new(empty, &config);
        let reason = planner
            .declined_reason(SourceKind::Video)
            .expect("a reason");
        assert!(reason.contains("render node"), "{reason:?}");
        let reason = planner.declined_reason(SourceKind::Still).unwrap();
        assert!(reason.contains("stills"), "{reason:?}");
    }

    /// An accelerator with no recorded device is skipped, not planned with a
    /// missing path. A plan naming no device cannot be run.
    #[test]
    fn an_available_accelerator_with_no_device_is_not_planned() {
        let status = status_with(&["vaapi"], &[]);
        let planner = Planner::new(status, &HwAccelConfig::default());
        assert_eq!(
            planner.plan_for_source(SourceKind::Video),
            AccelPlan::Software
        );
    }

    #[test]
    fn vaapi_is_preferred_over_qsv_and_nvenc() {
        let status = status_with(
            &["nvenc", "qsv", "vaapi"],
            &[
                ("nvenc", "/dev/nvidia0"),
                ("qsv", "/dev/dri/renderD129"),
                ("vaapi", "/dev/dri/renderD128"),
            ],
        );
        let planner = Planner::new(status, &HwAccelConfig::default());
        assert_eq!(
            planner.plan_for_source(SourceKind::Video).accel(),
            Some(Accel::VaApi)
        );
    }

    #[test]
    fn qsv_is_used_when_vaapi_is_not() {
        let status = status_with(
            &["nvenc", "qsv"],
            &[("nvenc", "/dev/nvidia0"), ("qsv", "/dev/dri/renderD129")],
        );
        let planner = Planner::new(status, &HwAccelConfig::default());
        let plan = planner.plan_for_source(SourceKind::Video);
        assert_eq!(plan.accel(), Some(Accel::Qsv));
        // QSV has a hardware scaler, so the scale goes with it.
        assert!(plan.scale_filter(120, "-2").contains("scale_qsv"));
    }

    #[test]
    fn nvenc_is_used_and_does_not_claim_a_hardware_scaler() {
        let status = status_with(&["nvenc"], &[("nvenc", "/dev/nvidia0")]);
        let planner = Planner::new(status, &HwAccelConfig::default());
        let plan = planner.plan_for_source(SourceKind::Video);
        assert_eq!(plan.accel(), Some(Accel::Nvenc));
        assert!(!plan.scale_filter(120, "-2").contains("nvenc"));
    }

    #[test]
    fn a_still_never_gets_a_hardware_plan() {
        let status = status_with(&["vaapi"], &[("vaapi", "/dev/dri/renderD128")]);
        let planner = Planner::new(status, &HwAccelConfig::default());
        assert_eq!(
            planner.plan_for_source(SourceKind::Still),
            AccelPlan::Software
        );
    }

    #[test]
    fn a_display_names_its_accelerator_and_device() {
        assert_eq!(
            vaapi().to_string(),
            "VA-API on /dev/dri/renderD128 (scale on device)"
        );
        assert_eq!(nvenc().to_string(), "NVENC on /dev/nvidia0 (scale on cpu)");
        assert_eq!(AccelPlan::Software.to_string(), "software");
    }

    #[test]
    fn a_driver_missing_reason_reaches_the_declined_explanation() {
        let status = HwAccelStatus {
            available: vec![],
            unavailable: vec![(
                "nvenc".to_string(),
                Unavailable::DriverMissing {
                    library: "libcuda.so.1".into(),
                    package: "nvidia-utils".into(),
                }
                .reason_for(Accel::Nvenc),
            )],
            devices: Default::default(),
        };
        let planner = Planner::new(status, &HwAccelConfig::default());
        let reason = planner.declined_reason(SourceKind::Video).unwrap();
        assert!(reason.contains("libcuda.so.1"), "{reason:?}");
        assert!(reason.contains("nvidia-utils"), "{reason:?}");
    }
}
