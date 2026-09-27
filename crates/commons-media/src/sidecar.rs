//! Finding the sidecar files that belong to a video.
//!
//! T-P6-002, the piece §7 defers when it says "this ticket does sidecars by
//! *convention beside the file*". Reading one is [`crate::extract`]'s
//! `read_sidecar`; this is the part that decides *which* files to read, and it
//! is here rather than in `extract.rs` for the same reason `subtitles.rs` holds
//! no ffmpeg: **this module is pure path logic and must be testable without a
//! filesystem full of fixtures.** Every function takes a directory and a stem
//! and returns names; none of them open anything.
//!
//! # The two conventions, and why there are exactly two
//!
//! §8.1 names the failure this avoids: `<name>.en.srt` beside `<name>.mp4` and
//! `<name>.srt` beside `<name>.en.mp4` both "belong" to the object, so a
//! looser rule finds one sidecar twice and the user sees a doubled entry in the
//! track list. Two explicit shapes, and nothing else:
//!
//! ```text
//! <stem>.<ext>            the video's own name, a different extension
//! <stem>.<lang>.<ext>     the same, plus a language
//! ```
//!
//! The second is the one that carries a language; the first has none, and that
//! is a fact about the file rather than a missing value to guess at. Both are
//! *derived* names — no directory listing, no glob, no "what else is in here".
//! That is what makes the result the same on every machine and after every
//! re-scan, and it is why `foo.en.srt` beside `foo.mkv` is found while
//! `bar.srt` beside `foo.mkv` is not: `bar` is not this object's stem.
//!
//! # Why the language is not validated here
//!
//! `<stem>.<lang>.<ext>` puts a two-letter code where the extension goes, and
//! a five-character extension would collide with a two-letter "language". The
//! ambiguity is real and it is resolved by the extension check rather than by a
//! language grammar: if the last segment is a known subtitle extension, the one
//! before it is a language, whatever it looks like. A file called
//! `foo.forced.srt` therefore has the language "forced" — which is wrong as a
//! language and harmless as a label, and is better than refusing a file that is
//! sitting right there. Normalising a language is the store's job and happens
//! once, in one place, which §3 of the spec requires.

use crate::subtitles::Format;
use std::path::{Path, PathBuf};

/// One sidecar file, and what the filename says about it.
///
/// `language` is `None` for the bare `<stem>.<ext>` form. That is not "unknown
/// language" — it is a file that declares none, and the store distinguishes it
/// from an empty string for the same reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sidecar {
    pub path: PathBuf,
    /// From the filename, raw. `None` for the bare form.
    pub language: Option<String>,
    pub format: Format,
}

/// The extensions a sidecar may have, in the order they are tried.
///
/// Ordered rather than a set so a directory holding `foo.srt` and `foo.vtt`
/// resolves the same way twice. The order is the common formats first, because
/// `Format::from_extension` would otherwise be the only thing deciding and the
/// decision would be invisible.
pub const SIDECAR_EXTENSIONS: [&str; 4] = ["srt", "vtt", "ass", "ssa"];

/// The sidecars that belong to `<dir>/<stem>`, in a deterministic order.
///
/// Takes the *stem*, not the filename. That is deliberate: a caller holding
/// `foo.bar.mkv` must pass `foo.bar`, because a caller that passed the whole
/// name and this function also stripped an extension would disagree with a
/// caller that passed the stem — and two rules that disagree find two files or
/// none.
pub fn discover(dir: &Path, stem: &str) -> Vec<Sidecar> {
    let mut out = Vec::new();
    if stem.is_empty() {
        return out;
    }
    for ext in SIDECAR_EXTENSIONS {
        // The bare form first, so a file with and without a language both
        // appear and the bare one is not displaced by its own language sibling.
        let base = dir.join(format!("{stem}.{ext}"));
        if base.is_file() {
            out.push(Sidecar {
                path: base,
                language: None,
                format: Format::from_extension(ext).expect("listed in SIDECAR_EXTENSIONS"),
            });
        }
    }
    for ext in SIDECAR_EXTENSIONS {
        let Ok(entries) = std::fs::read_dir(dir) else {
            // An unreadable directory is not an error. A sidecar scan is a
            // best-effort enrichment of a file we already have, and failing it
            // would mean a permission problem on one directory takes out the
            // whole import.
            break;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            // `<stem>.<something>.<ext>` where the ext is one of ours.
            let Some(rest) = name.strip_prefix(stem) else {
                continue;
            };
            let Some(rest) = rest.strip_prefix('.') else {
                continue;
            };
            let Some((lang, found_ext)) = rest.rsplit_once('.') else {
                continue;
            };
            if found_ext != ext || lang.is_empty() {
                continue;
            }
            // `lang` must not itself be an extension -- that is the
            // `<stem>.<ext>` form already handled above, and accepting it here
            // would return the same file twice, which is the §8.1 bug.
            if SIDECAR_EXTENSIONS.contains(&lang) {
                continue;
            }
            out.push(Sidecar {
                path: entry.path(),
                language: Some(lang.to_string()),
                format: Format::from_extension(found_ext)
                    .expect("found_ext is one of SIDECAR_EXTENSIONS"),
            });
        }
    }
    out
}

/// The stem of a media filename: everything before the LAST dot.
///
/// Last dot, not first, and the difference is a real filename. `My.Movie.2024.mkv`
/// has the stem `My.Movie.2024`; a first-dot rule gives `My`, which matches
/// `My.srt` and not `My.Movie.2024.srt` — so a dotted title finds no sidecar and
/// a file named `My.srt` next to it is claimed by something else. Release
/// titles are dotted constantly.
///
/// A name with no dot is its own stem. A leading dot (`.hidden`) is a stem of
/// the empty string, which `discover` refuses — a dotfile is not a video.
pub fn stem_of(file_name: &str) -> &str {
    match file_name.rfind('.') {
        // `rfind` on a name like `.mkv` returns 0, and taking the tail gives
        // "" — correct, and `discover` turns that into no sidecars rather than
        // a bare-directory match.
        Some(i) => &file_name[..i],
        None => file_name,
    }
}

/// The sidecars for a media file, given its path.
///
/// The convenience over [`discover`], and the one a caller wants: a path in, the
/// sidecars out. A file with no extension still gets its stem, so `README`
/// finds `README.srt` — which is right, because the convention is about the
/// name and not about the media being playable.
pub fn discover_for(media: &Path) -> Vec<Sidecar> {
    let Some(name) = media.file_name().and_then(|n| n.to_str()) else {
        // A non-UTF-8 filename cannot be matched against a UTF-8 stem, and
        // guessing an encoding to do it would be the mojibake the extractor's
        // decoder exists to avoid.
        return Vec::new();
    };
    let Some(dir) = media.parent() else {
        return Vec::new();
    };
    discover(dir, stem_of(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory unique per test *and* per process, because cargo
    /// runs the tests in this binary concurrently and two of them writing the
    /// same path race into each other's fixtures.
    fn tempdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("commons-sidecar-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("scratch dir");
        d
    }

    fn touch(d: &Path, name: &str) -> PathBuf {
        let p = d.join(name);
        std::fs::write(&p, b"WEBVTT\n\n").expect("write fixture");
        p
    }

    fn names(found: &[Sidecar]) -> Vec<String> {
        found
            .iter()
            .map(|s| s.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn a_dotless_name_is_its_own_stem() {
        assert_eq!(stem_of("README"), "README");
    }

    #[test]
    fn the_stem_splits_on_the_last_dot() {
        assert_eq!(stem_of("movie.mkv"), "movie");
        assert_eq!(stem_of("My.Movie.2024.mkv"), "My.Movie.2024");
        assert_eq!(stem_of("a.b.c.d.mp4"), "a.b.c.d");
    }

    #[test]
    fn a_leading_dot_is_not_a_stem() {
        // `.hidden` has no name before the dot, and a bare-directory match on
        // "" would claim every `.srt` in the directory.
        assert_eq!(stem_of(".hidden"), "");
        assert!(discover(Path::new("/tmp"), "").is_empty());
    }

    #[test]
    fn a_dotted_stem_finds_its_own_sidecar_and_not_a_prefix_ones() {
        // The case first-dot breaks. `My.Movie.2024.srt` must be found for
        // `My.Movie.2024.mkv`, and `My.srt` must not be claimed by it.
        let d = tempdir("stem");
        touch(&d, "My.Movie.2024.srt");
        touch(&d, "My.srt");
        let found = discover(&d, "My.Movie.2024");
        assert_eq!(vec!["My.Movie.2024.srt"], names(&found));
        assert!(found[0].language.is_none());
    }

    #[test]
    fn a_bare_sidecar_has_no_language() {
        let d = tempdir("bare");
        touch(&d, "film.srt");
        let found = discover(&d, "film");
        assert_eq!(1, found.len());
        assert_eq!(
            None, found[0].language,
            "the bare form declares no language"
        );
        assert_eq!(Format::SubRip, found[0].format);
    }

    #[test]
    fn a_language_suffix_is_read_off_the_filename() {
        let d = tempdir("lang");
        touch(&d, "film.en.srt");
        let found = discover(&d, "film");
        assert_eq!(1, found.len());
        assert_eq!(Some("en".to_string()), found[0].language);
    }

    #[test]
    fn a_sidecar_is_never_found_twice() {
        // The §8.1 bug, stated as a test. `film.srt` matches the bare form AND
        // looks like `film.<lang>.srt` with lang="srt" -- so a rule that does
        // not exclude an extension-shaped language returns it twice, and the
        // user sees a doubled entry in the track list.
        let d = tempdir("dupe");
        touch(&d, "film.srt");
        let found = discover(&d, "film");
        assert_eq!(1, found.len(), "found twice: {:?}", names(&found));
    }

    #[test]
    fn a_prefix_of_the_stem_is_not_a_sidecar() {
        // `film.srt` does not belong to `film2.mkv`, and `extra.en.srt` does
        // not belong to `ex.mkv`. Matching has to be on the full segment.
        let d = tempdir("prefix");
        touch(&d, "film2.srt");
        touch(&d, "extra.en.srt");
        assert!(discover(&d, "film").is_empty());
        assert!(discover(&d, "ex").is_empty());
    }

    #[test]
    fn every_supported_extension_is_found_and_ordered() {
        let d = tempdir("exts");
        for ext in SIDECAR_EXTENSIONS {
            touch(&d, &format!("film.{ext}"));
        }
        let found = discover(&d, "film");
        assert_eq!(
            vec!["film.srt", "film.vtt", "film.ass", "film.ssa"],
            names(&found),
            "the order is the constant's, so two scans agree"
        );
    }

    #[test]
    fn an_extension_outside_the_list_is_not_a_sidecar() {
        // `.txt` beside a video is a transcript or a note, not a subtitle track,
        // and offering it as one is a track that renders as a wall of prose.
        let d = tempdir("txt");
        touch(&d, "film.txt");
        assert!(discover(&d, "film").is_empty());
    }

    #[test]
    fn a_media_file_with_no_extension_still_finds_its_sidecar() {
        // The convention is about the name, not about the media being playable.
        let d = tempdir("noext");
        touch(&d, "clip.srt");
        let found = discover_for(&d.join("clip"));
        assert_eq!(vec!["clip.srt"], names(&found));
    }

    #[test]
    fn discover_for_uses_the_file_name_not_the_whole_path() {
        let d = tempdir("path");
        touch(&d, "movie.srt");
        // A sidecar in a DIFFERENT directory with the right name is not this
        // file's sidecar, and looking upward to find one would claim it.
        let other = tempdir("path-other");
        touch(&other, "elsewhere.srt");
        let found = discover_for(&other.join("elsewhere.mkv"));
        assert_eq!(vec!["elsewhere.srt"], names(&found));
        assert_eq!(1, found.len());
    }

    #[test]
    fn a_bare_filename_with_no_directory_finds_nothing_rather_than_panicking() {
        // `Path::parent()` on a bare name is `Some("")`, and joining onto that
        // is the current directory -- so this must return empty rather than
        // silently scanning wherever the process happens to be running.
        assert!(discover_for(Path::new("film.mkv")).is_empty());
    }

    #[test]
    fn a_missing_directory_is_empty_and_not_an_error() {
        // A sidecar scan enriches a file we already have. A permission problem
        // in one directory must not take out the whole import.
        let found = discover(Path::new("/nonexistent-dir-for-sidecar-test"), "film");
        assert!(found.is_empty());
    }

    #[test]
    fn discovery_is_stable_across_repeated_scans() {
        // Idempotency is what makes a re-scan safe: the same directory scanned
        // twice must give the same answer in the same order, or every import
        // reshuffles the user's track list.
        let d = tempdir("stable");
        touch(&d, "film.srt");
        touch(&d, "film.en.srt");
        touch(&d, "film.ja.vtt");
        assert_eq!(names(&discover(&d, "film")), names(&discover(&d, "film")));
    }
}
