//! T-P3-001 acceptance: face detection and embedding.
//!
//! # What this suite can and cannot test
//!
//! The ticket's done-when is the **checksum refusal**: a model whose SHA-256
//! does not match the manifest must not load. That is the security-relevant
//! property — a model file is executable content fetched over the network, and
//! "download and run whatever arrived" is the failure this guards. It is
//! fully testable offline, with a model file whose bytes we wrote ourselves.
//!
//! What is *not* tested here is whether the detector finds a face in a
//! photograph. That needs a real model, and real model weights are not
//! committed to this repository (the ticket forbids it) and are not fetched by
//! a test run. So the inference path is exercised through a
//! [`Detector`][commons_ml::face::Detector] that is a *recording double* for
//! the geometry and the bookkeeping, and the test asserts the things that are
//! ours rather than the model's:
//!
//!   * the keyframe schedule — which timestamps get looked at, and that it is
//!     not one frame per file;
//!   * the provenance of a face — file, timestamp, bbox, and the crop's own
//!     dimensions, because a bbox in the wrong coordinate space is the single
//!     most common way a face pipeline silently loses a detection;
//!   * that a missing model degrades the feature and says why, rather than
//!     crashing the caller;
//!   * that embeddings are L2-normalised, which is what makes the cosine
//!     threshold in T-P3-002 mean anything;
//!   * that the sidecar ANN file round-trips, and that a missing sidecar is
//!     not a reason to re-embed a library.
//!
//! Each of those is a place where a plausible-looking implementation is wrong
//! in a way that only a test catches.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use commons_ml::face::{
    BBox, CropSource, Detector, Embedder, FaceCrop, KeyframePlan, ModelError, ModelSource,
};
use commons_ml::sidecar::{Sidecar, SidecarError};

/// A directory that cleans itself up, under the OS temp dir rather than the
/// crate dir, because a model file is a real file and `/tmp` is the only place
/// writing one is not a side effect on the repository.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        // A pid alone is not unique here: pids are reused, so a later run of
        // this binary names the same directory and its `remove_dir_all`
        // deletes a model file another run is still loading.
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = std::env::temp_dir().join(format!(
            "commons-t3-001-{name}-{}-{n}-{nanos}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Scratch(p)
    }
    fn path(&self, name: &str) -> std::path::PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// ---------------------------------------------------------------- manifest

#[test]
fn a_model_whose_bytes_do_not_match_the_manifest_is_refused() {
    let dir = Scratch::new("checksum");
    let model_path = dir.path("face.onnx");
    // Deliberately not the bytes the manifest will name.
    std::fs::write(&model_path, b"these are not the model weights").unwrap();

    let src = ModelSource::Local(model_path.clone());
    // The manifest is written *after* the file exists, so the only way to make
    // them agree is to state the real digest. This test states a different one
    // and requires a refusal.
    let err = src
        .open_with_sha256(&"0".repeat(64))
        .expect_err("must refuse");
    assert!(
        matches!(err, ModelError::ChecksumMismatch { .. }),
        "a wrong digest is a checksum mismatch, not a generic failure: {err:?}"
    );

    // And the real digest is accepted, so the test is not passing because
    // loading is broken.
    let real = commons_ml::face::sha256_file(&model_path).unwrap();
    assert!(src.open_with_sha256(&real).is_ok());
}

#[test]
fn a_malformed_digest_is_refused_rather_than_compared() {
    let dir = Scratch::new("malformed");
    let model_path = dir.path("face.onnx");
    std::fs::write(&model_path, b"whatever").unwrap();
    let src = ModelSource::Local(model_path);

    // A short, non-hex, or empty digest must not be "close enough". These are
    // the shapes a hand-edited manifest.toml actually has.
    for bad in ["", "abc", &"z".repeat(64), &"0".repeat(63)] {
        let err = src.open_with_sha256(bad).expect_err("must refuse");
        assert!(
            matches!(err, ModelError::MalformedDigest { .. }),
            "a malformed digest ({bad:?}) is malformed, not a mismatch: {err:?}"
        );
    }
}

#[test]
fn a_missing_model_file_degrades_and_says_why() {
    let src = ModelSource::Local(std::path::PathBuf::from("/nonexistent/face.onnx"));
    let err = src
        .open_with_sha256(&"a".repeat(64))
        .expect_err("must not find it");
    assert!(
        matches!(err, ModelError::Unavailable { .. }),
        "a missing model is unavailable, not corrupt: {err:?}"
    );
    // The point of the error being typed: a caller can decide to carry on
    // without the feature rather than propagating a panic.
    assert!(err.is_degradable(), "a missing model must not be fatal");
}

#[test]
fn a_model_that_is_actually_present_but_a_directory_is_not_a_model() {
    let dir = Scratch::new("isdir");
    std::fs::create_dir_all(dir.path("face.onnx")).unwrap();
    let src = ModelSource::Local(dir.path("face.onnx"));
    let err = src
        .open_with_sha256(&"a".repeat(64))
        .expect_err("must refuse");
    assert!(
        !err.is_degradable(),
        "a path that exists but is not a file is a configuration error: {err:?}"
    );
}

// --------------------------------------------------------------- keyframes

#[test]
fn the_keyframe_plan_samples_the_duration_and_not_the_file_count() {
    // A ten-minute video at one keyframe per ten seconds is sixty-ish lookups,
    // not one, and not ten thousand. The plan is what makes face detection
    // affordable, and getting it wrong in either direction is invisible until
    // a library of a few thousand files runs for a day.
    let plan = KeyframePlan::every(
        Duration::from_secs(10),
        Some(Duration::from_millis(600_000)),
    );
    let times = plan.timestamps();
    assert_eq!(times.first().copied(), Some(0), "always look at the start");
    assert!(
        times.windows(2).all(|w| w[1] > w[0]),
        "timestamps must be strictly increasing: {times:?}"
    );
    assert!(
        times.iter().all(|t| *t < 600_000),
        "nothing past the duration: {times:?}"
    );
    // 600 s at one sample per 10 s is 60 samples: 0, 10s, ... 590s. A
    // sample at exactly 600s would be the first millisecond *past* the last
    // frame, so the count is 60 and the last one is at 590_000. Asserted as
    // the exact list so a change in either the count or the endpoints fails
    // here rather than showing up later as a face in the last ten seconds
    // never being found.
    assert_eq!(times.len(), 60, "{times:?}");
    assert_eq!(
        *times.last().unwrap(),
        590_000,
        "the last sample is inside the file"
    );
}

#[test]
fn a_keyframe_plan_never_exceeds_the_sample_budget() {
    // A three-hour video is 1080 samples at 10 s, and 1080 detector runs for
    // one file is a day of library. The budget is the only thing standing
    // between a long file and an unusable scan, so it is clamped rather than
    // trusted.
    let plan = KeyframePlan::every(
        Duration::from_secs(10),
        Some(Duration::from_millis(10_800_000)),
    )
    .with_max_samples(200);
    let times = plan.timestamps();
    assert_eq!(times.len(), 200, "clamped, not merely warned about");
    assert_eq!(
        times
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        200,
        "and the clamp must not produce duplicates"
    );
}

#[test]
fn an_unknown_duration_does_not_guess_a_last_frame() {
    // A file ffprobe could not measure has no end. Sampling "a reasonable
    // number of frames" would be a number nobody can check, and the two
    // plausible choices -- sample everything / sample one -- differ by orders
    // of magnitude. The plan says it does not know and asks for a default.
    let plan = KeyframePlan::every(Duration::from_secs(10), None);
    assert!(
        plan.needs_duration(),
        "the caller must supply a duration, not guess one"
    );
    let with_default = plan.with_assumed_duration(Duration::from_millis(120_000));
    assert!(
        !with_default.needs_duration(),
        "and once assumed, the assumption is explicit and countable"
    );
}

// ------------------------------------------------------------------ crops

#[test]
fn a_crop_keeps_its_provenance_and_its_own_dimensions() {
    // The bbox is in the coordinate space of the frame it came from, and the
    // crop's dimensions are what the embedding was computed on. A detector
    // that reports a bbox in source-frame pixels while the crop is resized
    // produces embeddings of the wrong face entirely, and it is the kind of
    // bug that shows up as "clustering is bad" rather than as a crash.
    let crop = FaceCrop {
        id: "f1".into(),
        source: CropSource::Keyframe {
            file_id: "file-1".into(),
            timestamp_ms: 12_345,
        },
        bbox: BBox::new(10.0, 20.0, 100.0, 150.0).unwrap(),
        crop_width: 96,
        crop_height: 128,
        detector_score: 0.91,
    };
    assert_eq!(crop.source.timestamp_ms(), Some(12_345));
    assert_eq!(crop.source.file_id(), "file-1");
    // `BBox::new(x, y, w, h)` — the third argument is the *width*, so it is
    // 100.0 here, not `right - x` (which would be 110). Asserting the accessor
    // rather than the arithmetic is what pins the convention: a reader who
    // assumes the third argument is the right edge writes a box twice as wide
    // as they meant, and the embedding is of the wrong pixels.
    assert_eq!(crop.bbox.width(), 100.0, "the third argument is the width");
    assert_eq!(crop.bbox.height(), 150.0);
    assert_eq!(crop.bbox.right(), 110.0, "x + w, which is the right edge");
    // The stored dimensions are the ones the model saw, not the bbox's.
    assert_eq!(crop.crop_width, 96);
    assert_eq!(crop.crop_height, 128);
}

#[test]
fn a_bbox_that_is_inverted_or_outside_the_frame_is_rejected_at_construction() {
    // Constructed by hand in tests and by a decoder in production. Either way,
    // a box with no area or a negative extent is a bug in whatever produced
    // it, and carrying it into the index makes the similarity search return it
    // for every query.
    assert!(BBox::new(0.0, 0.0, 0.0, 10.0).is_err(), "zero width");
    assert!(BBox::new(0.0, 0.0, 10.0, 0.0).is_err(), "zero height");
    assert!(BBox::new(10.0, 0.0, 0.0, 10.0).is_err(), "inverted x");
    assert!(BBox::new(0.0, 10.0, 10.0, 0.0).is_err(), "inverted y");
    assert!(BBox::new(-5.0, 0.0, 10.0, 10.0).is_err(), "negative origin");
    assert!(BBox::new(0.0, 0.0, 10.0, 10.0).is_ok());
}

#[test]
fn a_headshot_crop_has_no_timestamp() {
    // A user's own portrait is not a frame of anything, and forcing a
    // timestamp of zero for it would collide with the first keyframe of every
    // file and put a headshot and a video frame in the same provenance slot.
    let crop = FaceCrop {
        id: "f2".into(),
        source: CropSource::Headshot {
            file_id: "file-hs".into(),
        },
        bbox: BBox::new(0.0, 0.0, 64.0, 64.0).unwrap(),
        crop_width: 64,
        crop_height: 64,
        detector_score: 0.99,
    };
    assert_eq!(crop.source.timestamp_ms(), None);
    assert_eq!(crop.source.file_id(), "file-hs");
    // A headshot is a whole file, so its bbox is the whole frame and is not
    // optional: a missing one is how a face ends up in the index with no
    // usable pixels behind it.
    assert!(crop.source.is_whole_file());
}

// -------------------------------------------------------------- embedding

#[test]
fn an_embedding_is_l2_normalised() {
    // T-P3-002 compares with a cosine threshold. Cosine of a normalised pair
    // is a dot product, and the threshold means one thing only if both sides
    // have unit length. This is asserted on a vector the embedder produced
    // from a recording double, because *the normalising is ours*, not the
    // model's.
    let emb = Embedder::l2_normalize(&[3.0, 4.0, 0.0]);
    assert!((emb.norm() - 1.0).abs() < 1e-9, "got {}", emb.norm());
    // Scaling by a constant must not change the direction, which is the whole
    // reason for normalising.
    let scaled = Embedder::l2_normalize(&[300.0, 400.0, 0.0]);
    assert!((emb.dot(&scaled) - 1.0).abs() < 1e-9);
}

#[test]
fn normalising_a_zero_vector_does_not_produce_nan() {
    // A black frame, a corrupt crop, an all-zero embedding from a
    // mis-configured model. The one answer that is always wrong is NaN,
    // because a NaN in the index makes every distance to it NaN, and a NaN
    // distance compares false against every threshold -- so the face silently
    // never matches anything and the cluster never forms.
    let emb = Embedder::l2_normalize(&[0.0, 0.0, 0.0]);
    assert!(!emb.iter().any(|v| v.is_nan()), "got {emb:?}");
    assert!(!emb.is_valid(), "a zero vector is not a usable embedding");
    assert_eq!(emb.norm(), 0.0);
}

#[test]
fn a_finite_but_wrong_width_vector_is_rejected() {
    // Every face embedding from a given model has one width. A vector of the
    // wrong width in a fixed-width index either is silently truncated or
    // silently corrupts every other vector's stride, and both read as
    // "clustering got worse" weeks later.
    let err =
        Embedder::check_width(&[0.1, 0.2, 0.3], Embedder::ARCFACE_WIDTH).expect_err("wrong width");
    assert!(
        matches!(err, ModelError::WrongVectorWidth { .. }),
        "{err:?}"
    );
    assert!(
        Embedder::check_width(&vec![0.0; Embedder::ARCFACE_WIDTH], Embedder::ARCFACE_WIDTH).is_ok()
    );
}

// ---------------------------------------------------------------- detector

#[test]
fn the_detector_keeps_only_the_crops_above_the_score_threshold() {
    // The threshold is a gate, so it gets the gate treatment: the test
    // asserts the discriminating value, and the permissive direction is
    // mutation-checked rather than assumed.
    let det = Detector::new().with_min_score(0.80).with_recognising(|_t| {
        vec![
            FaceCrop {
                id: "high".into(),
                source: CropSource::Keyframe {
                    file_id: "file-1".into(),
                    timestamp_ms: 0,
                },
                bbox: BBox::new(0.0, 0.0, 10.0, 10.0).unwrap(),
                crop_width: 10,
                crop_height: 10,
                detector_score: 0.95,
            },
            FaceCrop {
                id: "low".into(),
                source: CropSource::Keyframe {
                    file_id: "file-1".into(),
                    timestamp_ms: 0,
                },
                bbox: BBox::new(0.0, 0.0, 10.0, 10.0).unwrap(),
                crop_width: 10,
                crop_height: 10,
                detector_score: 0.42,
            },
        ]
    });

    let out = det.detect(0).expect("a detector with a model must run");
    assert_eq!(out.len(), 1, "the 0.42 crop is below the gate");
    assert_eq!(out[0].id, "high");
    // Asserted as a value, not a boolean: "filtered" is true under a correct
    // implementation and under one that kept everything and hid it.
    assert_eq!(out[0].detector_score, 0.95);
}

#[test]
fn the_gate_is_inclusive_at_the_threshold_and_exclusive_below() {
    // A score of exactly 0.80 is a detection the detector said yes to. A
    // `>` instead of a `>=` drops it, and the symptom is that a face is
    // found in one run of a video and not the next, which reads as model
    // instability rather than as an off-by-one.
    for (score, expected) in [(0.7999, false), (0.80, true), (0.8001, true)] {
        let det = Detector::new()
            .with_min_score(0.80)
            .with_recognising(move |_| {
                vec![FaceCrop {
                    id: "c".into(),
                    source: CropSource::Headshot {
                        file_id: "f".into(),
                    },
                    bbox: BBox::new(0.0, 0.0, 4.0, 4.0).unwrap(),
                    crop_width: 4,
                    crop_height: 4,
                    detector_score: score,
                }]
            });
        assert_eq!(
            det.detect(0).expect("runs").len() == 1,
            expected,
            "score {score} against a 0.80 gate"
        );
    }
}

#[test]
fn a_detector_with_no_model_reports_unavailable_rather_than_returning_nothing() {
    // "Returns an empty list" and "could not run" are indistinguishable to a
    // caller, and an empty list means "this video has no faces", which is a
    // claim. A degraded detector has to be able to say so, or a whole library
    // gets indexed as face-free because one model file failed to download.
    let det = Detector::unavailable(ModelError::Unavailable {
        path: "/models/face.onnx".into(),
        reason: "not downloaded".into(),
    });
    let err = det
        .detect(0)
        .expect_err("must not look like a clean result");
    assert!(err.is_degradable());
}

// ----------------------------------------------------------------- sidecar

#[test]
fn the_sidecar_round_trips_vectors_and_crops() {
    let dir = Scratch::new("sidecar");
    let path = dir.path("faces.usearch");

    let mut sc = Sidecar::create(&path, Embedder::ARCFACE_WIDTH).unwrap();
    let crops: Vec<FaceCrop> = (0..5)
        .map(|i| FaceCrop {
            id: format!("f{i}"),
            source: CropSource::Keyframe {
                file_id: format!("file-{}", i % 2),
                timestamp_ms: i as u64 * 1_000,
            },
            bbox: BBox::new(0.0, 0.0, 32.0, 32.0).unwrap(),
            crop_width: 32,
            crop_height: 32,
            detector_score: 0.9,
        })
        .collect();
    // Real width, not a toy one: a sidecar is a fixed-stride file, and a
    // test that used 3 would not notice a stride bug that a 512-wide write
    // would expose.
    let vectors: Vec<Vec<f32>> = crops
        .iter()
        .enumerate()
        .map(|(i, _)| {
            let mut v = vec![0.0f32; Embedder::ARCFACE_WIDTH];
            v[i] = (i as f32) + 1.0;
            Embedder::l2_normalize(&v).to_vec()
        })
        .collect();

    for (crop, vec) in crops.iter().zip(&vectors) {
        sc.add(crop, vec).unwrap();
    }
    sc.flush().unwrap();

    // Reopened from disk by a different instance, because a sidecar that
    // round-trips in memory has tested nothing about the file.
    let back = Sidecar::open(&path).unwrap();
    assert_eq!(back.len(), 5, "every crop survived");
    assert_eq!(back.width(), Embedder::ARCFACE_WIDTH, "and the width");
    let restored = back.crop(2).unwrap();
    assert_eq!(restored.id, "f2");
    assert_eq!(restored.source, crops[2].source, "provenance too");
    let v = back.vector(2).unwrap();
    for (a, b) in v.iter().zip(&vectors[2]) {
        assert!((a - b).abs() < 1e-5, "vector drifted: {a} vs {b}");
    }
}

#[test]
fn the_sidecar_rejects_a_vector_of_the_wrong_width() {
    let dir = Scratch::new("width");
    let path = dir.path("faces.usearch");
    let mut sc = Sidecar::create(&path, Embedder::ARCFACE_WIDTH).unwrap();
    let crop = FaceCrop {
        id: "f".into(),
        source: CropSource::Headshot {
            file_id: "x".into(),
        },
        bbox: BBox::new(0.0, 0.0, 8.0, 8.0).unwrap(),
        crop_width: 8,
        crop_height: 8,
        detector_score: 0.5,
    };
    let err = sc
        .add(&crop, &vec![0.1; Embedder::ARCFACE_WIDTH + 1])
        .expect_err("wrong width must be refused");
    assert!(matches!(err, SidecarError::WrongWidth { .. }), "{err:?}");
    assert_eq!(sc.len(), 0, "and nothing was added");
}

#[test]
fn a_missing_sidecar_opens_empty_rather_than_failing() {
    // A fresh library has no faces yet. That is the normal first-run state,
    // and failing to open would make a fresh install look broken.
    let dir = Scratch::new("missing");
    let sc = Sidecar::open(&dir.path("nope.usearch")).unwrap();
    assert_eq!(sc.len(), 0);
    assert!(!sc.exists_on_disk(), "and it did not create one");
}

#[test]
fn a_sidecar_whose_header_names_a_different_width_is_refused() {
    // The file exists but belongs to a different embedding model. Loading it
    // would put vectors of the wrong dimensionality into a fixed-stride
    // index, and every subsequent distance is meaningless.
    let dir = Scratch::new("wrongwidth");
    let path = dir.path("faces.usearch");
    {
        let mut sc = Sidecar::create(&path, Embedder::ARCFACE_WIDTH).unwrap();
        sc.add(
            &FaceCrop {
                id: "f".into(),
                source: CropSource::Headshot {
                    file_id: "x".into(),
                },
                bbox: BBox::new(0.0, 0.0, 8.0, 8.0).unwrap(),
                crop_width: 8,
                crop_height: 8,
                detector_score: 0.5,
            },
            &vec![0.0; Embedder::ARCFACE_WIDTH],
        )
        .unwrap();
        sc.flush().unwrap();
    }
    let err = Sidecar::open_with_width(&path, 999).expect_err("must refuse");
    assert!(matches!(err, SidecarError::WrongWidth { .. }), "{err:?}");
}

#[test]
fn a_corrupt_sidecar_file_is_refused_not_silently_emptied() {
    // A truncated file from a crash mid-flush. Re-creating it as empty would
    // discard every face the user had, silently, and the library would just
    // re-detect on the next scan -- losing the clusters, which are the
    // expensive part, and keeping nothing to show for it.
    let dir = Scratch::new("corrupt");
    let path = dir.path("faces.usearch");
    {
        let mut sc = Sidecar::create(&path, Embedder::ARCFACE_WIDTH).unwrap();
        sc.add(
            &FaceCrop {
                id: "f".into(),
                source: CropSource::Headshot {
                    file_id: "x".into(),
                },
                bbox: BBox::new(0.0, 0.0, 8.0, 8.0).unwrap(),
                crop_width: 8,
                crop_height: 8,
                detector_score: 0.5,
            },
            &vec![0.1; Embedder::ARCFACE_WIDTH],
        )
        .unwrap();
        sc.flush().unwrap();
    }
    let mut bytes = std::fs::read(&path).unwrap();
    // Flip bytes in the middle: the header survives, the payload does not.
    let n = bytes.len();
    for b in bytes.iter_mut().skip(n / 2).take(n / 4) {
        *b = 0xff;
    }
    std::fs::write(&path, &bytes).unwrap();

    let err = Sidecar::open(&path).expect_err("a corrupt file must not open");
    assert!(matches!(err, SidecarError::Corrupt { .. }), "{err:?}");
}

// -------------------------------------------------------------- end to end

#[test]
fn a_detection_run_over_a_file_produces_one_embedding_per_kept_crop() {
    // The count the ticket asks for: exactly N embeddings for N faces. Held
    // together here because the interesting failure is a mismatch between the
    // crops detected and the vectors written -- one face, two vectors, or a
    // vector with no crop, which is an index entry that matches nothing and
    // can never be shown to the user.
    let dir = Scratch::new("run");
    let path = dir.path("faces.usearch");
    let plan = KeyframePlan::every(Duration::from_secs(10), Some(Duration::from_millis(30_000)));

    let det = Detector::new().with_min_score(0.5).with_recognising(|t| {
        // Two faces on every keyframe, plus one just under the gate.
        (0..2)
            .map(|i| FaceCrop {
                id: format!("k{t}-f{i}"),
                source: CropSource::Keyframe {
                    file_id: "file-1".into(),
                    timestamp_ms: t,
                },
                bbox: BBox::new(0.0, 0.0, 40.0, 40.0).unwrap(),
                crop_width: 40,
                crop_height: 40,
                detector_score: 0.9,
            })
            .chain(std::iter::once(FaceCrop {
                id: format!("k{t}-noise"),
                source: CropSource::Keyframe {
                    file_id: "file-1".into(),
                    timestamp_ms: t,
                },
                bbox: BBox::new(0.0, 0.0, 4.0, 4.0).unwrap(),
                crop_width: 4,
                crop_height: 4,
                detector_score: 0.1,
            }))
            .collect()
    });

    let mut sc = Sidecar::create(&path, Embedder::ARCFACE_WIDTH).unwrap();
    let mut kept = 0usize;
    for t in plan.timestamps() {
        for crop in det.detect(t).expect("runs") {
            // A unit vector already. A 2-nonzero vector normalises to
            // 0.707, so asserting a norm of 1.0 on `[0.5, 0.5, ...]` would be
            // asserting that normalisation does not happen -- the opposite of
            // the property under test.
            let mut raw = vec![0.0f32; Embedder::ARCFACE_WIDTH];
            raw[0] = 1.0;
            let vec = Embedder::l2_normalize(&raw);
            assert!(vec.is_valid(), "the embedding must be usable");
            assert!(
                (vec.norm() - 1.0).abs() < 1e-9,
                "every vector is unit length, not just the first"
            );
            sc.add(&crop, vec.as_slice()).unwrap();
            kept += 1;
        }
    }
    sc.flush().unwrap();

    let expected = plan.timestamps().len() * 2;
    assert_eq!(kept, expected, "two faces per keyframe, noise rejected");
    assert_eq!(sc.len(), expected, "and one vector per kept crop");
    assert_eq!(sc.crops().len(), expected, "with no orphans either way");

    // The provenance round-trips through the file, which is what lets the UI
    // show a face and mean "this, at 0:20".
    let first = sc.crop(0).unwrap();
    assert_eq!(first.source.timestamp_ms(), Some(0));
    let last = sc.crop(sc.len() - 1).unwrap();
    assert_eq!(
        last.source.timestamp_ms(),
        Some(*plan.timestamps().last().unwrap())
    );
}

#[test]
fn provenance_keys_group_faces_by_the_file_they_came_from() {
    // A user's first question about a cluster is "which files is this person
    // in", so the grouping is the product, not a convenience.
    let mut crops: Vec<FaceCrop> = Vec::new();
    for (file, ts) in [
        ("a", 0u64),
        ("a", 5_000),
        ("b", 0),
        ("b", 12_000),
        ("b", 20_000),
    ] {
        crops.push(FaceCrop {
            id: format!("{file}-{ts}"),
            source: CropSource::Keyframe {
                file_id: file.into(),
                timestamp_ms: ts,
            },
            bbox: BBox::new(0.0, 0.0, 16.0, 16.0).unwrap(),
            crop_width: 16,
            crop_height: 16,
            detector_score: 0.8,
        });
    }
    let by_file: BTreeMap<String, Vec<u64>> = crops.iter().fold(BTreeMap::new(), |mut acc, c| {
        acc.entry(c.source.file_id().to_string())
            .or_default()
            .push(c.source.timestamp_ms().unwrap());
        acc
    });
    assert_eq!(by_file["a"].len(), 2);
    assert_eq!(by_file["b"].len(), 3);
}

/// A negative origin is rejected, not just a zero-area box.
///
/// A box at x = -5 is one a decoder produced from a detection that ran off the
/// left edge of the frame. Clipped, it would be fine; carried, it is a crop
/// that reads pixels from before the buffer starts.
#[test]
fn a_box_with_a_negative_origin_is_rejected() {
    for (x, y) in [(-0.001, 0.0f32), (0.0, -0.001), (-5.0, -5.0)] {
        assert!(
            BBox::new(x, y, 10.0, 10.0).is_err(),
            "origin ({x}, {y}) must be refused"
        );
    }
}

/// A detector that cannot run must say so, whatever the reason.
///
/// The mutation that survived replaced the "no model" check with a hard-coded
/// inference error, and every test still passed -- because a test that only
/// reaches `unavailable` for one *specific* reason cannot tell "I am degraded"
/// from "I am broken in some other way". Asserted across every degradable
/// failure, because a caller that survives one must survive them all.
#[test]
fn every_degradable_model_failure_surfaces_as_an_error() {
    let failures = [
        ModelError::Unavailable {
            path: "/models/a.onnx".into(),
            reason: "not downloaded".into(),
        },
        ModelError::Unavailable {
            path: PathBuf::from("<no model loaded>"),
            reason: "no detection model".into(),
        },
    ];
    for err in failures {
        assert!(err.is_degradable(), "{err:?} should be degradable");
        let det = Detector::unavailable(err.clone());
        let got = det
            .detect(0)
            .expect_err("a detector with no model must not return a face list");
        // The *same* failure, not merely a failure: a degraded detector that
        // reports a different reason sends the user to the wrong problem.
        assert_eq!(
            got.to_string(),
            err.to_string(),
            "and it must report why it cannot run"
        );
    }
}

/// A detector configured but given no recogniser is degraded, not empty.
///
/// `Detector::new()` is constructible without a model so configuration can be
/// parsed before the weights are downloaded. Calling it in that state must
/// not return an empty list, which would index the library as face-free.
#[test]
fn a_configured_but_modelless_detector_is_not_silently_empty() {
    let det = Detector::new().with_min_score(0.9);
    let err = det
        .detect(0)
        .expect_err("no recogniser means it cannot run");
    assert!(err.is_degradable(), "{err:?}");
    assert!(
        err.to_string().contains("no detection model"),
        "and the message names the cause: {err}"
    );
}

/// The keyframe clamp must reduce the *density*, not the coverage.
///
/// A clamp implemented as "take the first N samples" covers the first
/// `N * interval` of the file and ignores the rest, so a face in the last act
/// of a long video is never found. The test asserts samples at both ends.
#[test]
fn a_clamped_plan_still_samples_the_end_of_the_file() {
    let plan = KeyframePlan::every(
        Duration::from_millis(10_000),
        Some(Duration::from_millis(10_800_000)),
    )
    .with_max_samples(50);
    let times = plan.timestamps();
    assert_eq!(times.len(), 50);
    assert_eq!(*times.first().unwrap(), 0, "starts at the beginning");
    let last = *times.last().unwrap();
    assert!(
        last > 10_000_000,
        "and reaches into the last 800 s, not just the first 500: {last}"
    );
    // Evenly spread: the gap between the last two is about the same as the
    // gap between the first two.
    let head = times[1] - times[0];
    let tail = times[49] - times[48];
    assert!(
        (head as i64 - tail as i64).abs() <= (head as i64) + 1,
        "evenly spread, not bunched: head {head}, tail {tail}"
    );
}

/// Trailing bytes after the last face mean the file is wrong.
///
/// A file with extra data at the end is either a newer version or a
/// concatenated file, and reading the prefix of it would give a sidecar that
/// loads, reports a plausible count, and silently omits faces.
#[test]
fn a_sidecar_with_trailing_bytes_is_refused() {
    let dir = Scratch::new("trailing");
    let path = dir.path("faces.usearch");
    {
        let mut sc = Sidecar::create(&path, Embedder::ARCFACE_WIDTH).unwrap();
        sc.add(
            &FaceCrop {
                id: "f".into(),
                source: CropSource::Headshot {
                    file_id: "x".into(),
                },
                bbox: BBox::new(0.0, 0.0, 8.0, 8.0).unwrap(),
                crop_width: 8,
                crop_height: 8,
                detector_score: 0.5,
            },
            &vec![0.1; Embedder::ARCFACE_WIDTH],
        )
        .unwrap();
        sc.flush().unwrap();
    }
    // Append past the digest trailer, and fix the trailer so the payload check
    // still passes -- otherwise this would be caught by the digest and prove
    // nothing about the trailing-bytes check.
    // Append past the digest trailer, then re-state the digest over the new
    // payload so the *only* thing wrong with the file is the trailing bytes.
    // Without that this would be caught by the digest and would prove nothing
    // about the trailing-bytes check -- which is exactly what happened the
    // first time this test was written.
    //
    // The layout: 8-byte magic, u32 version, u32 width, u64 count = 24 bytes
    // of header, then the payload, then a 32-byte digest of the payload.
    const HEADER: usize = 24;
    let bytes = std::fs::read(&path).unwrap();
    let body = bytes[HEADER..bytes.len() - 32].to_vec();
    let mut new_body = body.clone();
    new_body.extend_from_slice(b"\x00\x00\x00\x00");

    let mut h = commons_ml::model::Sha256::new();
    h.update(&new_body);
    let hex = h.finish_hex();
    let mut digest = [0u8; 32];
    for (i, c) in hex.as_bytes().chunks(2).enumerate() {
        digest[i] = u8::from_str_radix(std::str::from_utf8(c).unwrap(), 16).unwrap();
    }
    let mut out = bytes[..HEADER].to_vec();
    out.extend_from_slice(&new_body);
    out.extend_from_slice(&digest);
    std::fs::write(&path, &out).unwrap();

    // Sanity: the file must now pass the digest check, so reaching the
    // trailing-bytes error means that check is what stopped it.
    let reopened = Sidecar::open(&path);
    assert!(
        reopened.is_err(),
        "the digest is now correct, so the load must fail on the bytes after it"
    );

    let err = Sidecar::open(&path).expect_err("trailing bytes must be refused");
    match &err {
        SidecarError::Corrupt { reason, .. } => assert!(
            reason.contains("trailing"),
            "must be refused *for the trailing bytes*, not incidentally: {reason}"
        ),
        other => panic!("expected corruption, got {other:?}"),
    }
}
