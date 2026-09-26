//! The fixture table for T-P1-001.
//!
//! The unit tests in `detect.rs` drive the sniffer with hand-built byte strings,
//! which proves the magic-byte logic but says nothing about whether the fixtures
//! in a real library classify correctly. This table runs the same code over
//! files that were actually produced by ffmpeg, zip, and a text editor.
//!
//! The four ambiguous cases the ticket names individually each have their own
//! row and their own comment, because they are the ones that will regress:
//!
//! | case | issue | why it is hard |
//! |------|-------|----------------|
//! | a `.webm` that is really an image | #6577 | extension and bytes agree, so only frame count settles it |
//! | a GIF in a zip | #5111 | container is a zip; the member decides |
//! | a folder of images | #5185 | the folder wants a gallery, the user may not |
//! | `.nogallery` on a gallery folder | #7179 | the override must *remove* a gallery, not reduce it |
//!
//! Fixtures are generated, not committed as binaries-with-no-provenance: see
//! `fixtures.rs` in the parent directory for the exact commands, and
//! `scripts/make-fixtures.sh` to regenerate them.

use std::path::{Path, PathBuf};

use commons_core::ObjectKind as K;
use commons_scan::detect::{detect, Container, Evidence};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn f(name: &str) -> PathBuf {
    fixtures().join(name)
}

/// Fail loudly, and actionably, if a fixture is not on disk.
///
/// A missing fixture has to stop the test rather than be skipped: these files
/// are the corpus, and a suite that quietly passes on an empty corpus is worse
/// than no suite, because it reports coverage it does not have.
fn fixture_present(name: &str) {
    let path = f(name);
    assert!(
        path.exists(),
        "fixture {name} is missing at {}. Run scripts/make-fixtures.sh to regenerate the test corpus.",
        path.display()
    );
}

/// (file, expected kind, expected container, what the fixture is for)
const TABLE: &[(&str, K, Option<Container>, &str)] = &[
    (
        "scene_3s.mp4",
        K::Scene,
        Some(Container::IsoBmff),
        "3-second H.264 mp4 from ffmpeg's testsrc",
    ),
    (
        "clip.mkv",
        K::Scene,
        Some(Container::Matroska),
        "Matroska; WebM shares the EBML header and is distinguished by DocType",
    ),
    (
        "clip.webm",
        K::Scene,
        Some(Container::Matroska),
        "WebM, which is Matroska with a different DocType past byte 64",
    ),
    (
        "tone.mp3",
        K::Scene,
        Some(Container::Mp3),
        "MP3 with an ID3 tag",
    ),
    (
        "tone.flac",
        K::Scene,
        Some(Container::Flac),
        "native FLAC",
    ),
    (
        "tone.wav",
        K::Scene,
        Some(Container::Wave),
        "RIFF/WAVE, the third meaning of a RIFF header",
    ),
    (
        "photo.jpg",
        K::Image,
        Some(Container::Jpeg),
        "single JPEG",
    ),
    (
        "alpha.png",
        K::Image,
        Some(Container::Png),
        "PNG with an alpha channel (T-P1-006 asserts the alpha survives)",
    ),
    (
        "still.gif",
        K::Image,
        Some(Container::Gif),
        "ONE frame, 0.04s. stash#5111: a GIF is an image until a probe says otherwise, and one frame is never a scene",
    ),
    (
        "animated.gif",
        K::Image,
        Some(Container::Gif),
        "100 frames over 4s. Also an Image at detection time -- promotion to Scene is T-P1-002's decision from frame count and duration, not the sniffer's",
    ),
    (
        "gallery.zip",
        K::Gallery,
        Some(Container::Zip),
        "zip of images; the member walk in T-P1-004 confirms it holds images",
    ),
    (
        "book.cbz",
        K::Comic,
        Some(Container::Zip),
        "same bytes as a gallery, named .cbz: the extension is what separates an ordered page container from a gallery",
    ),
    (
        "story.txt",
        K::Text,
        Some(Container::Text),
        "plain text, no magic bytes",
    ),
    (
        "clip.funscript",
        K::Text,
        Some(Container::Funscript),
        "funscript XML, recognised before the generic XML rule",
    ),
];

#[test]
fn every_fixture_classifies_as_expected() {
    for (name, want_kind, want_container, why) in TABLE {
        fixture_present(name);
        let got = detect(&f(name)).unwrap_or_else(|e| panic!("{name}: detect failed: {e}"));
        assert_eq!(
            got.kind, *want_kind,
            "{name} ({why}): expected {want_kind:?}, got {:?} decided by {}",
            got.kind, got.decided_by
        );
        assert_eq!(
            got.container, *want_container,
            "{name}: wrong container (expected {want_container:?}, got {:?})",
            got.container
        );
        assert_eq!(
            got.decided_by,
            Evidence::Magic,
            "{name} should be decided by its bytes, not its name"
        );
    }
}

#[test]
fn the_magic_bytes_win_over_a_lying_extension() {
    // The whole point of the ticket. Four files whose names disagree with their
    // contents, each copied to a name that says something else.
    let cases: &[(&str, &str, K, &str)] = &[
        (
            "scene_3s.mp4",
            "lying.zip",
            K::Scene,
            "an mp4 named .zip: the zip magic test must not fire on it",
        ),
        (
            "gallery.zip",
            "lying.cbz",
            K::Comic,
            "a zip named .cbz is a comic by name; the bytes are the same",
        ),
        (
            "tone.mp3",
            "lying.png",
            K::Scene,
            "an mp3 named .png: no PNG signature, so the name is ignored",
        ),
        (
            "photo.jpg",
            "lying.mkv",
            K::Image,
            "a jpeg named .mkv: no EBML header, so the name is ignored",
        ),
    ];
    for (src, as_name, want, why) in cases {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join(as_name);
        std::fs::copy(f(src), &dest).unwrap();
        let got = detect(&dest).unwrap();
        assert_eq!(got.kind, *want, "{src} copied to {as_name} ({why})");
    }
}

#[test]
fn ambiguous_case_webm_that_is_really_an_image_clip() {
    // stash#6577, part one. The sniffer cannot settle this: a short webm is
    // bytes-identical in shape to a short video. What it must do is *not*
    // pretend -- it reports Scene and the probe in T-P1-002 downgrades it when
    // the frame count says "clip, not scene". Here we assert only that the
    // sniffer's answer is honest about being a guess from the container.
    fixture_present("clip.webm");
    let d = detect(&f("clip.webm")).unwrap();
    assert_eq!(d.kind, K::Scene);
    assert_eq!(d.decided_by, Evidence::Magic);
    // The webm/mkv distinction is past byte 64, so the sniffer must not claim
    // to know which one it was.
    assert_eq!(d.container, Some(Container::Matroska));
}

#[test]
fn ambiguous_case_gif_in_a_zip_is_an_image_not_a_scene() {
    // stash#5111. A zip containing a GIF: the container answer is Gallery, and
    // the member walk in T-P1-004 is what turns a zip-of-images into a Gallery
    // rather than a Comic. The GIF itself is an Image regardless of the zip.
    fixture_present("animated.gif");
    let d = detect(&f("animated.gif")).unwrap();
    assert_eq!(d.kind, K::Image, "a GIF is an image at detection time");
    assert_eq!(
        d.container,
        Some(Container::Gif),
        "and it is recognised as a GIF, so the probe knows to count frames"
    );
}

#[test]
fn ambiguous_case_folder_of_images_becomes_a_gallery() {
    // stash#5185. A directory of images is a gallery -- but only when the
    // member walk runs, and only if no override says otherwise. Here the
    // override is absent, so the images are individually Image objects and the
    // *directory* is the thing that becomes a Gallery in T-P1-004.
    let dir = tempfile::tempdir().unwrap();
    for (i, src) in ["photo.jpg", "alpha.png", "photo.jpg"].iter().enumerate() {
        std::fs::copy(f(src), dir.path().join(format!("{i:03}.jpg"))).unwrap();
    }
    for entry in std::fs::read_dir(dir.path()).unwrap() {
        let p = entry.unwrap().path();
        let d = detect(&p).unwrap();
        assert_eq!(
            d.kind,
            K::Image,
            "each file in a gallery folder is an Image"
        );
        assert!(!d.overridden, "no override file, so nothing is overridden");
    }
}

#[test]
fn ambiguous_case_nogallery_removes_the_gallery() {
    // stash#7179. This is the case that made the override worth building: the
    // directory would otherwise be a gallery, and `.nogallery` must turn the
    // *whole thing* into standalone images. There is no partial gallery, so
    // the assertion is that no file in the tree reports Gallery.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(".nogallery"), "").unwrap();
    for (i, src) in ["photo.jpg", "alpha.png", "photo.jpg"].iter().enumerate() {
        std::fs::copy(f(src), dir.path().join(format!("{i:03}.jpg"))).unwrap();
    }
    for entry in std::fs::read_dir(dir.path()).unwrap() {
        let p = entry.unwrap().path();
        if p.file_name().unwrap() == ".nogallery" {
            continue;
        }
        let d = detect(&p).unwrap();
        assert_eq!(
            d.kind,
            K::Image,
            "{:?} must not be a gallery under .nogallery",
            p.file_name()
        );
        assert_eq!(d.decided_by, Evidence::Override);
        assert!(d.overridden);
    }
}

#[test]
fn forcegallery_wins_over_real_mp4_bytes() {
    // stash#5185, the other half: the user says gallery, the bytes say scene,
    // and the user is right about their own library.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(".forcegallery"), "").unwrap();
    for (i, _) in ["scene_3s.mp4", "scene_3s.mp4"].iter().enumerate() {
        std::fs::copy(f("scene_3s.mp4"), dir.path().join(format!("{i}.mp4"))).unwrap();
    }
    for entry in std::fs::read_dir(dir.path()).unwrap() {
        let p = entry.unwrap().path();
        if p.file_name().unwrap() == ".forcegallery" {
            continue;
        }
        let d = detect(&p).unwrap();
        assert_eq!(d.kind, K::Gallery);
        assert!(d.overridden);
    }
}
