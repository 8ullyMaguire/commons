//! Archive listing and safe extraction (T-P1-004, spec §5.3).
//!
//! # The hard rule
//!
//! Spec §5.3: *extract to a temp dir, validate every member path against the
//! destination root, never write outside it.* This is not defensive
//! programming — stash#7240 is a real Zip-Slip report, and a Zip-Slip in a
//! media library is trivially exploitable: the archive is downloaded content
//! whose filenames the user never sees, and the process doing the extracting
//! runs as the same user as the media library itself.
//!
//! So the validation is a separate, pure function ([`validate_member`]) that
//! takes a member path and a destination root and returns a decision. It does
//! no I/O, which is what makes it exhaustively testable — the alternative,
//! validating inline during extraction, means the interesting cases are only
//! reachable by actually running an attack.
//!
//! Three classes of hostile member are refused, and all three are in the
//! fixture corpus:
//!
//! | member | why it is refused |
//! |--------|-------------------|
//! | `../../etc/passwd` | relative traversal |
//! | `/etc/passwd` | absolute path |
//! | `link.jpg` → `/etc/passwd` | symlink whose target escapes |
//!
//! And one member that *looks* hostile but is not:
//!
//! | member | why it is accepted |
//! |--------|---------------------|
//! | `nested/../ok.txt` | traversal that lands back inside the root |
//!
//! That last case matters as much as the refusals. A validator that rejects
//! every member containing `..` is not a validator, it is a denial of service —
//! and archives with such names are common enough that refusing them all would
//! make Commons useless for real libraries.

use std::path::{Component, Path, PathBuf};

/// What kind of entry an archive member is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberKind {
    File,
    Directory,
    /// A symlink, whose payload is the link target.
    Symlink,
    /// Something else: a fifo, device, or socket. Refused on extraction.
    Special,
}

/// One entry in an archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    /// The path exactly as the archive stores it. Kept verbatim so the UI can
    /// show the user what is inside rather than a sanitised version of it.
    pub raw_name: String,
    /// The sanitised relative path, present only when the member validated.
    pub safe_name: Option<PathBuf>,
    pub kind: MemberKind,
    /// Uncompressed size in bytes. `None` when the archive does not say.
    pub size: Option<u64>,
    /// The compression method as a name, e.g. `deflate`, `store`.
    ///
    /// stash#7230 asks for this to be visible. It matters to a user because a
    /// "compressed" archive that is entirely `store` is not compressed, and a
    /// zip bomb looks like a highly-compressed zip.
    pub method: CompressionMethod,
    /// Set for a symlink: the link target as stored.
    pub link_target: Option<String>,
    /// The reason this member was refused, if it was.
    pub rejection: Option<Rejection>,
}

/// The compression method, named for display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressionMethod {
    Store,
    Deflate,
    Bzip2,
    Lzma,
    Zstd,
    /// AES-encrypted. The payload is ciphertext, so it is listed but never
    /// extracted without a password.
    Aes,
    Unknown(u16),
    /// A method this build cannot decompress. Listed, not extracted.
    Unsupported(u16),
}

impl CompressionMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            CompressionMethod::Store => "store",
            CompressionMethod::Deflate => "deflate",
            CompressionMethod::Bzip2 => "bzip2",
            CompressionMethod::Lzma => "lzma",
            CompressionMethod::Zstd => "zstd",
            CompressionMethod::Aes => "aes",
            CompressionMethod::Unknown(_) => "unknown",
            CompressionMethod::Unsupported(_) => "unsupported",
        }
    }
}

impl std::fmt::Display for CompressionMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompressionMethod::Unknown(m) | CompressionMethod::Unsupported(m) => {
                write!(f, "{}({m})", self.as_str())
            }
            other => f.write_str(other.as_str()),
        }
    }
}

/// Why a member was refused. Each variant is a distinct attack or a distinct
/// legitimate case we cannot handle, and keeping them separate means a user can
/// be told which happened instead of "invalid archive".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejection {
    /// `..` escaped the destination root.
    Traversal { raw: String },
    /// An absolute path, or a Windows drive/UNC prefix.
    Absolute { raw: String },
    /// A symlink whose target points outside the root, or is absolute.
    SymlinkEscape { raw: String, target: String },
    /// Not a regular file or directory.
    Special { raw: String },
    /// The name is empty, or reduces to nothing (`./`, `.`).
    Empty,
    /// A component that cannot be represented, or a NUL byte.
    Unrepresentable { raw: String },
}

impl Rejection {
    /// A message safe to show a user. Names the file and the reason, and never
    /// includes a resolved filesystem path (which would leak the extraction
    /// root's layout for no benefit).
    pub fn message(&self) -> String {
        match self {
            Rejection::Traversal { raw } => {
                format!("{raw}: path escapes the extraction directory")
            }
            Rejection::Absolute { raw } => {
                format!("{raw}: absolute paths are not extracted")
            }
            Rejection::SymlinkEscape { raw, target } => {
                format!("{raw}: link points outside the extraction directory ({target})")
            }
            Rejection::Special { raw } => {
                format!("{raw}: not a regular file or directory")
            }
            Rejection::Empty => "entry has no name".to_string(),
            Rejection::Unrepresentable { raw } => {
                format!("{raw}: name cannot be represented on this filesystem")
            }
        }
    }
}

impl std::fmt::Display for Rejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for Rejection {}

/// The result of validating one member path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Validated {
    /// Safe to extract, as this relative path.
    Safe(PathBuf),
    /// Refused, for the given reason.
    Refused(Rejection),
}

/// Validate one archive member path against an extraction root.
///
/// **Pure**: performs no filesystem access and does not consult `root` beyond
/// knowing the extraction is confined to a single directory. That is the point —
/// the decision depends only on the member's own name and kind, so every
/// hostile case is testable without extracting anything.
///
/// `root` is accepted so the signature stays honest about what a caller is
/// doing (it is extracting *somewhere*), and so a future hardening pass can
/// resolve against a real root without changing every call site.
pub fn validate_member(
    raw_name: &str,
    kind: MemberKind,
    link_target: Option<&str>,
    root: &Path,
) -> Validated {
    let _ = root;

    // A NUL in a path is not representable on any filesystem this runs on, and
    // passing one to an open() is how truncation bugs start.
    if raw_name.contains('\0') {
        return Validated::Refused(Rejection::Unrepresentable {
            raw: raw_name.to_string(),
        });
    }

    let path = Path::new(raw_name);

    if path.as_os_str().is_empty() {
        return Validated::Refused(Rejection::Empty);
    }

    // Reject on the *lexical* form, before normalisation. A path that needs
    // `..` to become safe is a path we do not touch: normalising first and then
    // checking would mean trusting the very traversal we are looking for, and
    // would also let `a/../b` through when `b` was meant to be refused.
    if is_absolute_or_unc(path) {
        return Validated::Refused(Rejection::Absolute {
            raw: raw_name.to_string(),
        });
    }

    // Now normalise, and confirm the result stayed inside. This is what lets
    // `nested/../ok.txt` through: it normalises to `ok.txt`, which is inside.
    // The check is on the *normalised* depth, so it catches anything that
    // climbs out even via a route the first pass missed.
    //
    // `..\..\evil` is split on backslashes first, because on this platform it
    // is a single filename component and the walk would not see the climb.
    let normalised = if raw_name.contains('\\') {
        normalise_lexically(&PathBuf::from(raw_name.replace('\\', "/")))
    } else {
        normalise_lexically(path)
    };
    if normalised.is_none() {
        return Validated::Refused(Rejection::Traversal {
            raw: raw_name.to_string(),
        });
    }
    let relative = normalised.expect("checked above");

    if relative.as_os_str().is_empty() {
        return Validated::Refused(Rejection::Empty);
    }

    // A symlink is only safe if its target is itself safe. This is the case a
    // naive "check the member name" validator misses entirely: the member is
    // named `link.jpg`, which is innocuous, and the damage happens when
    // something later follows the link.
    //
    // The target resolves relative to the SYMLINK'S OWN DIRECTORY, as POSIX
    // says -- not relative to the extraction root. So `pages/logo.png` pointing
    // at `../shared/logo.png` lands on `shared/logo.png`, inside the root, and
    // is safe. Resolving the target as if it were root-relative (as an earlier
    // version did) refused every legitimate relative symlink, which is a false
    // positive on precisely the case that should work.
    if kind == MemberKind::Symlink {
        let Some(target) = link_target else {
            return Validated::Refused(Rejection::SymlinkEscape {
                raw: raw_name.to_string(),
                target: String::new(),
            });
        };
        if target.contains('\0') || target.is_empty() {
            return Validated::Refused(Rejection::SymlinkEscape {
                raw: raw_name.to_string(),
                target: target.to_string(),
            });
        }
        let t = Path::new(target);
        // An absolute target is unsafe whatever the link's own location.
        if is_absolute_or_unc(t) {
            return Validated::Refused(Rejection::SymlinkEscape {
                raw: raw_name.to_string(),
                target: target.to_string(),
            });
        }
        // Resolve the target against the link's own directory and require the
        // result to stay at or below the root. The test is whether depth goes
        // negative at ANY point along the way, not whether it ends negative:
        // `../../etc/passwd` ends at depth 0 (two down, two back up) yet escapes
        // completely, and checking only the final value accepted it.
        let mut depth: i32 = path
            .parent()
            .map(|p| {
                p.components()
                    .filter(|c| matches!(c, Component::Normal(_)))
                    .count()
            })
            .unwrap_or(0) as i32;

        for c in t.components() {
            match c {
                Component::CurDir => {}
                Component::ParentDir => depth -= 1,
                Component::Normal(_) => depth += 1,
                Component::RootDir | Component::Prefix(_) => depth = -1,
            }
            if depth < 0 {
                return Validated::Refused(Rejection::SymlinkEscape {
                    raw: raw_name.to_string(),
                    target: target.to_string(),
                });
            }
        }
    }

    Validated::Safe(relative)
}

/// Collapse `.` and `..` lexically, returning `None` if the result escapes.
///
/// `None` means "climbed above the root". Note this is deliberately *not*
/// `canonicalize`: that requires the path to exist and would resolve symlinks on
/// disk, which is both a syscall per member and the wrong question at scan time.
fn normalise_lexically(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                // Popping past the root is the escape we are looking for.
                if !out.pop() {
                    return None;
                }
            }
            Component::Normal(part) => out.push(part),
            // RootDir and Prefix were already rejected by is_absolute_or_unc.
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(out)
}

/// Does a backslash-separated path climb out of the root?
///
/// True when the path has more `..` components than real directory components
/// before it. The answer is deliberately conservative: a path that balances
/// (`a/../b`) is safe, and a path that does not is not.
fn backslash_traverses(s: &str) -> bool {
    let mut depth: i32 = 0;
    for part in s.split(['\\', '/']) {
        match part {
            "" | "." => {}
            ".." => {
                depth -= 1;
                if depth < 0 {
                    return true;
                }
            }
            _ => depth += 1,
        }
    }
    false
}

/// Absolute in POSIX terms, or a Windows drive letter / UNC prefix.
///
/// Checked on the raw string as well as the parsed path, because on Linux
/// `C:\Windows\System32\evil.dll` parses as a single relative component and
/// would sail past a `path.is_absolute()` test. An archive made on Windows is a
/// normal thing to find in a library.
fn is_absolute_or_unc(path: &Path) -> bool {
    if path.is_absolute() || path.has_root() {
        return true;
    }
    let s = path.to_string_lossy();
    let b = s.as_bytes();
    // A Windows-style traversal written with backslashes. On Linux this is one
    // odd filename and harmless, but the same archive extracted on Windows
    // escapes -- and a Commons library is a thing people sync between machines.
    // Refusing it here is the portable answer.
    // A Windows-style UNC prefix, which is absolute. Distinct from the
    // backslash-traversal case below, which is caught as a parent component.
    if s.starts_with("\\\\") {
        return true;
    }
    // A Windows-style traversal written with backslashes. On Linux this is one
    // odd filename and harmless, but the same archive extracted on Windows
    // escapes -- and a Commons library is a thing people sync between machines.
    // Refusing it here is the portable answer.
    if backslash_traverses(&s) {
        return false;
    }
    // `C:\...` or `C:/...`
    if b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/')
    {
        return true;
    }
    // `\\server\share` — a UNC path.
    if b.len() >= 2 && b[0] == b'\\' && b[1] == b'\\' {
        return true;
    }
    false
}

/// An archive's contents, already validated.
#[derive(Debug, Clone, Default)]
pub struct Listing {
    pub members: Vec<Member>,
    /// Members refused, by reason. A listing is still returned when this is
    /// non-empty: a user wants to see the ten good images in an archive that
    /// also holds one hostile entry, not an error page.
    pub refused: Vec<Member>,
    pub total_uncompressed: u64,
    /// True when the total is suspiciously large relative to the archive's own
    /// size. A zip bomb: the ratio is the signal, and surfacing it is better
    /// than discovering it through a full disk.
    pub suspected_bomb: bool,
}

impl Listing {
    /// The members that are regular files, in archive order.
    pub fn files(&self) -> impl Iterator<Item = &Member> {
        self.members.iter().filter(|m| m.kind == MemberKind::File)
    }

    /// True when every file member looks like an image, which is what makes a
    /// zip a gallery rather than an arbitrary archive (§5.3, and the
    /// `kind_for_container` decision in T-P1-001).
    pub fn all_files_are_images(&self) -> bool {
        let mut any = false;
        for m in self.files() {
            any = true;
            if !is_image_name(&m.raw_name) {
                return false;
            }
        }
        any
    }
}

/// Does this filename look like a raster or vector image?
///
/// An extension check, and it is honest about being one: a `.jpg` containing
/// arbitrary bytes is still treated as an image here. Deciding by content is
/// T-P1-001's job at scan time; this is the cheap question "should the gallery
/// UI try to render this", and getting it wrong costs a broken thumbnail
/// rather than a security problem.
pub fn is_image_name(name: &str) -> bool {
    let ext = Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    matches!(
        ext.as_str(),
        "jpg"
            | "jpeg"
            | "png"
            | "gif"
            | "webp"
            | "avif"
            | "heic"
            | "heif"
            | "bmp"
            | "tif"
            | "tiff"
            | "jxl"
            | "jpe"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        PathBuf::from("/tmp/extract")
    }

    fn v(name: &str) -> Validated {
        validate_member(name, MemberKind::File, None, &root())
    }

    /// stash#7240. The headline case, named on its own because it is the one
    /// the CVE class report is about.
    #[test]
    fn relative_traversal_is_refused() {
        for evil in [
            "../../etc/passwd",
            "..",
            "a/../../etc/passwd",
            "a/b/../../../etc/shadow",
            "..\\..\\windows\\system32\\evil.dll",
        ] {
            assert_eq!(
                v(evil),
                Validated::Refused(Rejection::Traversal {
                    raw: evil.to_string()
                }),
                "{evil} must be refused"
            );
        }
    }

    /// The other half: an absolute path is a perfectly ordinary archive entry
    /// and it is the easiest thing in the world to write.
    #[test]
    fn absolute_paths_are_refused() {
        for evil in [
            "/etc/passwd",
            "/",
            "/tmp/x",
            "C:\\Windows\\evil.dll",
            "C:/evil.dll",
        ] {
            assert!(
                matches!(v(evil), Validated::Refused(Rejection::Absolute { .. })),
                "{evil} must be refused as absolute"
            );
        }
    }

    /// UNC, which is a Windows-only form and would parse as a relative path on
    /// Linux. An archive made on Windows is a normal thing to find.
    #[test]
    fn unc_paths_are_refused() {
        for evil in [r"\\server\share\evil.dll", r"\\?\C:\evil.dll"] {
            assert!(
                matches!(v(evil), Validated::Refused(Rejection::Absolute { .. })),
                "{evil} must be refused"
            );
        }
    }

    /// The case a naive validator gets wrong. `nested/../ok.txt` *contains*
    /// `..` and lands back inside the root, so it is safe. Refusing every member
    /// with a `..` in it would break archives that are entirely legitimate, and
    /// a validator that refuses everything is a denial of service rather than a
    /// defence.
    #[test]
    fn traversal_that_lands_back_inside_is_accepted() {
        assert_eq!(
            v("nested/../ok.txt"),
            Validated::Safe(PathBuf::from("ok.txt")),
            "normalises to ok.txt, which is inside the root"
        );
        assert_eq!(
            v("./a/./b/c.jpg"),
            Validated::Safe(PathBuf::from("a/b/c.jpg"))
        );
    }

    /// stash#7240's second payload: a member with an innocent name whose *target*
    /// escapes. Validating only the member name would pass this.
    #[test]
    fn a_symlink_whose_target_escapes_is_refused() {
        let got = validate_member(
            "link.jpg",
            MemberKind::Symlink,
            Some("/etc/passwd"),
            &root(),
        );
        assert!(
            matches!(got, Validated::Refused(Rejection::SymlinkEscape { .. })),
            "an innocent-looking name with an escaping target must be refused"
        );
    }

    #[test]
    fn a_symlink_whose_target_also_traverses_is_refused() {
        let got = validate_member(
            "link.jpg",
            MemberKind::Symlink,
            Some("../../etc/passwd"),
            &root(),
        );
        assert!(matches!(
            got,
            Validated::Refused(Rejection::SymlinkEscape { .. })
        ));
    }

    #[test]
    fn a_sympoint_inside_the_root_is_accepted() {
        // Not a hostile case. A comic archive that symlinks a shared logo to
        // two pages is legitimate, and refusing it would be a bug.
        let got = validate_member(
            "pages/logo.png",
            MemberKind::Symlink,
            Some("../shared/logo.png"),
            &root(),
        );
        assert_eq!(got, Validated::Safe(PathBuf::from("pages/logo.png")));
    }

    #[test]
    fn a_symlink_with_no_target_is_refused_rather_than_assumed_harmless() {
        // A symlink member with empty content could be a broken link or an
        // attempt to smuggle a target past a string check. Refuse and say so.
        let got = validate_member("link.jpg", MemberKind::Symlink, None, &root());
        assert!(matches!(
            got,
            Validated::Refused(Rejection::SymlinkEscape { .. })
        ));
    }

    #[test]
    fn a_nul_byte_in_a_name_is_refused() {
        // Truncation at the syscall boundary is how a validated name becomes a
        // different name on disk.
        let got = v("ok.jpg\0../../etc/passwd");
        assert!(matches!(
            got,
            Validated::Refused(Rejection::Unrepresentable { .. })
        ));
    }

    #[test]
    fn an_empty_or_dot_only_name_is_refused() {
        for name in ["", ".", "./", "../"] {
            assert!(
                matches!(v(name), Validated::Refused(_)),
                "{name:?} must be refused"
            );
        }
    }

    #[test]
    fn ordinary_names_pass_through_unchanged() {
        for good in [
            "001.jpg",
            "sub/dir/002.png",
            "a b c.jpg",
            "naïve.jpg",
            "deep/a/b/c/d/e/f.png",
        ] {
            assert_eq!(
                v(good),
                Validated::Safe(PathBuf::from(good)),
                "{good} should be accepted verbatim"
            );
        }
    }

    /// The property that matters, stated as a property: whatever the input, the
    /// accepted path never contains a `..` component and is never absolute.
    /// Fuzz-ish over a grid of shapes rather than a hand-picked list.
    #[test]
    fn no_accepted_path_ever_contains_a_parent_component_or_is_absolute() {
        let pieces = ["a", "..", ".", "b", "", "sub"];
        let mut checked = 0;
        for a in pieces {
            for b in pieces {
                for c in pieces {
                    let name = format!("{a}/{b}/{c}");
                    if let Validated::Safe(rel) = v(&name) {
                        assert!(
                            !rel.is_absolute(),
                            "{name} was accepted as an absolute path"
                        );
                        assert!(
                            !rel.components().any(|c| matches!(c, Component::ParentDir)),
                            "{name} was accepted with a .. component"
                        );
                        assert!(
                            !rel.as_os_str().is_empty(),
                            "{name} was accepted as an empty path"
                        );
                        checked += 1;
                    }
                }
            }
        }
        assert!(
            checked > 10,
            "the grid should accept a good many cases, got {checked}"
        );
    }

    #[test]
    fn a_directory_entry_is_validated_the_same_way_a_file_is() {
        let got = validate_member("../escape/", MemberKind::Directory, None, &root());
        assert!(matches!(
            got,
            Validated::Refused(Rejection::Traversal { .. })
        ));
        let ok = validate_member("sub/", MemberKind::Directory, None, &root());
        assert_eq!(ok, Validated::Safe(PathBuf::from("sub")));
    }

    #[test]
    fn a_rejection_message_names_the_file_and_the_reason_without_leaking_the_root() {
        let m = Rejection::Traversal {
            raw: "../../etc/passwd".into(),
        }
        .message();
        assert!(m.contains("../../etc/passwd"), "{m}");
        assert!(m.contains("escapes"), "{m}");
        assert!(!m.contains("/tmp/extract"), "must not leak the root: {m}");
    }

    #[test]
    fn image_extensions_are_recognised_for_the_gallery_decision() {
        for img in [
            "a.jpg", "a.JPEG", "a.png", "a.webp", "a.avif", "a.heic", "a.tif",
        ] {
            assert!(is_image_name(img), "{img}");
        }
        for not in ["a.mp4", "a.txt", "a.nfo", "a", "a.jpg.exe", "readme.md"] {
            assert!(!is_image_name(not), "{not} must not count as an image");
        }
    }

    #[test]
    fn a_listing_of_only_images_is_a_gallery() {
        let l = Listing {
            members: vec![
                Member {
                    raw_name: "001.jpg".into(),
                    safe_name: Some("001.jpg".into()),
                    kind: MemberKind::File,
                    size: Some(10),
                    method: CompressionMethod::Deflate,
                    link_target: None,
                    rejection: None,
                },
                Member {
                    raw_name: "002.png".into(),
                    safe_name: Some("002.png".into()),
                    kind: MemberKind::File,
                    size: Some(20),
                    method: CompressionMethod::Store,
                    link_target: None,
                    rejection: None,
                },
            ],
            ..Listing::default()
        };
        assert!(l.all_files_are_images());
    }

    #[test]
    fn a_listing_with_a_readme_is_not_a_gallery() {
        let l = Listing {
            members: vec![Member {
                raw_name: "readme.txt".into(),
                safe_name: Some("readme.txt".into()),
                kind: MemberKind::File,
                size: Some(1),
                method: CompressionMethod::Store,
                link_target: None,
                rejection: None,
            }],
            ..Listing::default()
        };
        assert!(
            !l.all_files_are_images(),
            "a readme means not a pure image set"
        );
    }

    #[test]
    fn an_empty_archive_is_not_a_gallery() {
        // `any` must be true, or an empty zip reports itself as a gallery of
        // zero images and shows an empty grid.
        assert!(!Listing::default().all_files_are_images());
    }

    #[test]
    fn a_compression_method_is_named_for_display() {
        assert_eq!(CompressionMethod::Store.to_string(), "store");
        assert_eq!(CompressionMethod::Deflate.to_string(), "deflate");
        // stash#7230: the method must be visible, including when we cannot
        // decompress it, so "why did this fail" is answerable.
        assert_eq!(
            CompressionMethod::Unsupported(99).to_string(),
            "unsupported(99)"
        );
    }
}
