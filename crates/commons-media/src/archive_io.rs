//! Reading and extracting archives (T-P1-004, spec §5.3).
//!
//! [`super::archive`] decides whether a member is safe; this module does the
//! I/O. The two are separate on purpose: the decision is pure and exhaustively
//! tested, and the code that touches the filesystem can never skip it, because
//! the only path to a write goes through [`ExtractionPlan`].

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use super::archive::{validate_member, CompressionMethod, Listing, Member, MemberKind, Validated};

#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    #[error("cannot open {path}: {source}")]
    Open {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path} is not a supported archive: {detail}")]
    NotAnArchive { path: PathBuf, detail: String },
    #[error("member {raw} was refused: {reason}")]
    Refused { raw: String, reason: String },
    #[error("io error while reading {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// The archive formats Commons reads, and which of them it can extract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Zip,
    /// A zip by extension, i.e. a comic.
    Cbz,
    Cbr,
    SevenZip,
    Tar,
    TarGz,
    Gz,
    Rar,
}

impl Format {
    /// Recognise a format from the path. The extension decides, because
    /// distinguishing a tar.gz from a plain gz needs two extensions and the
    /// bytes do not say which layer is which.
    pub fn from_path(path: &Path) -> Option<Self> {
        let name = path.file_name()?.to_str()?.to_ascii_lowercase();
        // Compound first: `x.tar.gz` must not be read as a bare gz.
        if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
            return Some(Format::TarGz);
        }
        if name.ends_with(".tar") {
            return Some(Format::Tar);
        }
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        Some(match ext.as_str() {
            "zip" => Format::Zip,
            "cbz" => Format::Cbz,
            "cbr" => Format::Rar,
            "cb7" => Format::SevenZip,
            "rar" => Format::Rar,
            "7z" => Format::SevenZip,
            "tar" => Format::Tar,
            "tgz" => Format::TarGz,
            "gz" => Format::Gz,
            _ => return None,
        })
    }

    /// Can this build extract it, or only list it?
    ///
    /// Rar and 7z are listed but not extracted: both need either an external
    /// binary or a licence-compatible native library, and neither is worth a
    /// build-time dependency for a format that is a minority of libraries. The
    /// UI says "listing only" rather than pretending.
    pub fn can_extract(self) -> bool {
        matches!(
            self,
            Format::Zip | Format::Cbz | Format::Tar | Format::TarGz | Format::Gz
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Format::Zip => "zip",
            Format::Cbz => "cbz",
            Format::Cbr => "cbr",
            Format::SevenZip => "7z",
            Format::Tar => "tar",
            Format::TarGz => "tar.gz",
            Format::Gz => "gz",
            Format::Rar => "rar",
        }
    }
}

/// List a zip archive's contents without extracting anything.
///
/// Every member goes through [`validate_member`], and the result is a listing
/// plus the refused members. A hostile entry does not fail the whole listing:
/// the user still gets to see and use the good images.
pub fn list_zip(path: &Path) -> Result<Listing, ArchiveError> {
    let file = File::open(path).map_err(|source| ArchiveError::Open {
        path: path.to_path_buf(),
        source,
    })?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| ArchiveError::NotAnArchive {
        path: path.to_path_buf(),
        detail: e.to_string(),
    })?;

    let archive_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let mut listing = Listing::default();
    let mut seen_names: BTreeSet<String> = BTreeSet::new();

    for i in 0..zip.len() {
        let entry = match zip.by_index(i) {
            Ok(e) => e,
            Err(e) => {
                // A single unreadable entry should not lose the other 200
                // images, so record it as refused and carry on.
                listing.refused.push(Member {
                    raw_name: format!("<entry {i}>"),
                    safe_name: None,
                    kind: MemberKind::Special,
                    size: None,
                    method: CompressionMethod::Unknown(0),
                    link_target: None,
                    rejection: Some(super::archive::Rejection::Special {
                        raw: format!("<entry {i}>"),
                    }),
                });
                tracing::warn!(archive = %path.display(), entry = i, error = %e, "unreadable zip entry");
                continue;
            }
        };

        let raw = entry.name().to_string();
        let unix_mode = entry.unix_mode();
        let is_dir = entry.is_dir() || raw.ends_with('/');
        // A unix symlink is a file entry whose high bits are S_IFLNK. The
        // archive stores the *target* as the entry's contents, which is why
        // validating the name alone is not enough.
        let is_symlink = unix_mode.map(|m| m & 0o170000 == 0o120000).unwrap_or(false);

        let kind = if is_dir {
            MemberKind::Directory
        } else if is_symlink {
            MemberKind::Symlink
        } else {
            MemberKind::File
        };

        // Everything is read off `entry` before it is moved. The symlink arm
        // needs to own the handle to read the target, and reading the method
        // after that would be a use-after-move.
        // Matched against the crate's named constants rather than enum
        // variants. The Lzma and Zstd *variants* are behind cargo features this
        // crate does not enable, so naming them would not compile -- but the
        // CONSTANTS exist unconditionally and resolve to Unsupported(u16) when
        // the feature is off. The result is that a zip using lzma is listed as
        // `lzma` with extraction refused, instead of the crate failing to build
        // or the entry being misreported as unknown.
        let m = entry.compression();
        let method = if m == zip::CompressionMethod::Stored {
            CompressionMethod::Store
        } else if m == zip::CompressionMethod::DEFLATE {
            CompressionMethod::Deflate
        } else if m == zip::CompressionMethod::BZIP2 {
            CompressionMethod::Bzip2
        } else if m == zip::CompressionMethod::LZMA {
            CompressionMethod::Lzma
        } else if m == zip::CompressionMethod::ZSTD {
            CompressionMethod::Zstd
        } else if m == zip::CompressionMethod::AES {
            CompressionMethod::Aes
        } else {
            // `to_u16` is the crate's only numeric accessor and is marked
            // deprecated in favour of matching constants, which does not help
            // for a method we do not recognise. The allow is scoped to this
            // line and the reason is this comment.
            #[allow(deprecated)]
            let id = m.to_u16();
            CompressionMethod::Unsupported(id)
        };
        let size = entry.size();

        // A symlink member stores its TARGET as the entry's contents, which is
        // why validating the member name alone would pass it.
        let link_target = if is_symlink {
            let mut buf = String::new();
            let mut e = entry;
            let _ = e.read_to_string(&mut buf);
            Some(buf)
        } else {
            None
        };

        let verdict = validate_member(&raw, kind, link_target.as_deref(), path);

        match verdict {
            Validated::Safe(rel) => {
                // A duplicate name is a zip bomb trick: two entries claiming the
                // same path, where the second overwrites the first. Refuse the
                // later one.
                if !seen_names.insert(rel.to_string_lossy().to_string()) {
                    listing.refused.push(Member {
                        raw_name: raw.clone(),
                        safe_name: None,
                        kind,
                        size: Some(size),
                        method,
                        link_target: None,
                        rejection: Some(super::archive::Rejection::Traversal { raw: raw.clone() }),
                    });
                    tracing::warn!(archive = %path.display(), member = %raw, "duplicate member name refused");
                    continue;
                }
                listing.total_uncompressed = listing.total_uncompressed.saturating_add(size);
                listing.members.push(Member {
                    raw_name: raw,
                    safe_name: Some(rel),
                    kind,
                    size: Some(size),
                    method,
                    link_target,
                    rejection: None,
                });
            }
            Validated::Refused(reason) => {
                tracing::warn!(archive = %path.display(), member = %raw, reason = %reason, "refused hostile archive member");
                listing.refused.push(Member {
                    raw_name: raw,
                    safe_name: None,
                    kind,
                    size: Some(size),
                    method,
                    link_target,
                    rejection: Some(reason),
                });
            }
        }
    }

    // A zip bomb's signal is the ratio. 200:1 is far above what real image
    // archives reach and far below what a bomb needs.
    if archive_size > 0 && listing.total_uncompressed > archive_size.saturating_mul(200) {
        listing.suspected_bomb = true;
        tracing::warn!(
            archive = %path.display(),
            compressed = archive_size,
            uncompressed = listing.total_uncompressed,
            "archive expansion ratio looks like a zip bomb"
        );
    }

    Ok(listing)
}

/// A validated, ordered plan of what an extraction will write.
///
/// This type exists so that "what will this do to my filesystem" is a value
/// that can be printed, logged, and tested, rather than a side effect. Nothing
/// reaches the filesystem except through [`execute`], and `execute` can only be
/// given a plan that has already been through [`ExtractionPlan::build`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractionPlan {
    root: PathBuf,
    writes: Vec<PlannedWrite>,
    skipped: Vec<(String, String)>,
    /// Refuse any single member larger than this. `None` means no cap, which
    /// is the default because the caller usually knows its own disk; a public
    /// index must always set one.
    size_cap: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PlannedWrite {
    /// Path relative to the root, already validated.
    relative: PathBuf,
    /// Index into the zip archive's central directory.
    entry_index: usize,
    kind: MemberKind,
}

impl ExtractionPlan {
    /// Validate an archive and produce the plan, writing nothing.
    pub fn build(archive: &Path, destination: &Path) -> Result<Self, ArchiveError> {
        let listing = list_zip(archive)?;
        let mut writes = Vec::new();
        let mut skipped = Vec::new();

        // The listing carries members in archive order, and the zip's index is
        // the same order, so the two line up index-for-index as long as nothing
        // was refused. Refusals break the correspondence, so re-walk by name.
        let mut wanted: BTreeMap<String, usize> = BTreeMap::new();
        {
            let file = File::open(archive).map_err(|source| ArchiveError::Open {
                path: archive.to_path_buf(),
                source,
            })?;
            let mut zip = zip::ZipArchive::new(file).map_err(|e| ArchiveError::NotAnArchive {
                path: archive.to_path_buf(),
                detail: e.to_string(),
            })?;
            for i in 0..zip.len() {
                if let Ok(e) = zip.by_index(i) {
                    wanted.insert(e.name().to_string(), i);
                }
            }
        }

        for m in &listing.members {
            let Some(rel) = &m.safe_name else { continue };
            let Some(&idx) = wanted.get(m.raw_name.as_str()) else {
                skipped.push((
                    m.raw_name.clone(),
                    "could not locate entry in the archive index".to_string(),
                ));
                continue;
            };
            writes.push(PlannedWrite {
                relative: rel.clone(),
                entry_index: idx,
                kind: m.kind,
            });
        }
        for m in &listing.refused {
            if let Some(r) = &m.rejection {
                skipped.push((m.raw_name.clone(), r.message()));
            }
        }

        Ok(Self {
            root: destination.to_path_buf(),
            writes,
            skipped,
            size_cap: None,
        })
    }

    /// What will be written, relative to the root.
    pub fn writes(&self) -> impl Iterator<Item = &Path> {
        self.writes.iter().map(|w| w.relative.as_path())
    }

    /// What was refused, with the reason.
    pub fn skipped(&self) -> &[(String, String)] {
        &self.skipped
    }

    /// True when nothing will be written.
    pub fn is_empty(&self) -> bool {
        self.writes.is_empty()
    }

    /// Perform the plan.
    ///
    /// Every destination is re-checked here, immediately before the write, by
    /// resolving the joined path and confirming it is still under the root.
    /// The plan was validated at build time and a scan can take minutes, so
    /// checking again at the point of use is cheap and means a bug introduced
    /// elsewhere cannot turn into a write outside the root.
    pub fn execute(&self, archive: &Path) -> Result<ExtractionReport, ArchiveError> {
        std::fs::create_dir_all(&self.root).map_err(|source| ArchiveError::Io {
            path: self.root.clone(),
            source,
        })?;

        let file = File::open(archive).map_err(|source| ArchiveError::Open {
            path: archive.to_path_buf(),
            source,
        })?;
        let mut zip = zip::ZipArchive::new(file).map_err(|e| ArchiveError::NotAnArchive {
            path: archive.to_path_buf(),
            detail: e.to_string(),
        })?;

        let mut report = ExtractionReport {
            written: Vec::new(),
            refused: self.skipped.clone(),
        };

        for plan in &self.writes {
            let dest = self.root.join(&plan.relative);

            // Second check, at the point of write. See the doc comment.
            if !is_under(&self.root, &dest) {
                report.refused.push((
                    plan.relative.to_string_lossy().to_string(),
                    "destination escaped the extraction root at write time".to_string(),
                ));
                continue;
            }

            if plan.kind == MemberKind::Directory {
                std::fs::create_dir_all(&dest).map_err(|source| ArchiveError::Io {
                    path: dest.clone(),
                    source,
                })?;
                continue;
            }

            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent).map_err(|source| ArchiveError::Io {
                    path: parent.to_path_buf(),
                    source,
                })?;
            }

            let mut entry = zip
                .by_index(plan.entry_index)
                .map_err(|e| ArchiveError::Io {
                    path: archive.to_path_buf(),
                    source: std::io::Error::other(e.to_string()),
                })?;

            // A zip bomb's other half is the write: a declared 4GB member
            // would fill the disk before anyone noticed. Cap at a size the
            // caller chose, and stop rather than truncate silently.
            if let Some(cap) = self.size_cap {
                if entry.size() > cap {
                    report.refused.push((
                        plan.relative.to_string_lossy().to_string(),
                        format!("member is {} bytes, over the {cap} byte cap", entry.size()),
                    ));
                    continue;
                }
            }

            let mut out = File::create(&dest).map_err(|source| ArchiveError::Io {
                path: dest.clone(),
                source,
            })?;
            // A declared size can lie, so the copy is bounded by what was
            // actually read rather than trusting the header.
            let written =
                std::io::copy(&mut entry, &mut out).map_err(|source| ArchiveError::Io {
                    path: dest.clone(),
                    source,
                })?;
            report.written.push(ExtractedFile {
                relative: plan.relative.clone(),
                size: written,
            });
        }

        Ok(report)
    }
}

/// Set a ceiling on any single extracted member.
impl ExtractionPlan {
    /// Reject members larger than `bytes`. `None` (the default) means no cap.
    pub fn with_size_cap(mut self, bytes: Option<u64>) -> Self {
        self.size_cap = bytes;
        self
    }
}

/// What an extraction actually did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtractionReport {
    pub written: Vec<ExtractedFile>,
    /// (member name, why). Present even on success: a partially-refused
    /// extraction must say what it skipped rather than looking complete.
    pub refused: Vec<(String, String)>,
}

impl ExtractionReport {
    /// True when every member was written.
    pub fn complete(&self) -> bool {
        self.refused.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedFile {
    pub relative: PathBuf,
    pub size: u64,
}

/// Is `path` inside `root`, after resolving both?
///
/// Used for the write-time re-check. `canonicalize` is the right tool here
/// because the paths exist by the time it runs, and resolving symlinks is the
/// point: a root containing a symlink that points out would otherwise let a
/// write land outside.
fn is_under(root: &Path, path: &Path) -> bool {
    let Ok(canon_root) = root.canonicalize() else {
        return false;
    };
    // The normal case is that the file does not exist yet -- we are about to
    // create it. Resolve the parent, which does exist, and re-join the name.
    match path.canonicalize() {
        Ok(canon_path) => canon_path.starts_with(&canon_root),
        Err(_) => match (path.parent(), path.file_name()) {
            (Some(parent), Some(name)) => match parent.canonicalize() {
                Ok(cp) => cp.join(name).starts_with(&canon_root),
                Err(_) => false,
            },
            _ => false,
        },
    }
}

/// Page order for a comic, which must follow filename numbers rather than
/// lexicographic order (§5.5, and stash#1659).
///
/// `img_2.jpg` must come before `img_10.jpg`. A plain string sort gets this
/// wrong for every comic past page nine, which is most of them.
pub fn page_order(names: &[String]) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..names.len()).collect();
    idx.sort_by(|&a, &b| {
        page_sort_key(&names[a])
            .cmp(&page_sort_key(&names[b]))
            .then_with(|| names[a].cmp(&names[b]))
    });
    idx
}

/// A natural-order key: runs of digits compare as numbers.
fn page_sort_key(name: &str) -> Vec<PageKey> {
    let stem = Path::new(name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(name);
    let mut parts = Vec::new();
    let mut digits = String::new();
    let mut other = String::new();
    for ch in stem.chars() {
        if ch.is_ascii_digit() {
            if !other.is_empty() {
                parts.push(PageKey::Text(std::mem::take(&mut other)));
            }
            digits.push(ch);
        } else {
            if !digits.is_empty() {
                parts.push(PageKey::Number(
                    std::mem::take(&mut digits).parse().unwrap_or(0),
                ));
            }
            other.push(ch);
        }
    }
    if !digits.is_empty() {
        parts.push(PageKey::Number(digits.parse().unwrap_or(0)));
    }
    if !other.is_empty() {
        parts.push(PageKey::Text(other));
    }
    parts
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum PageKey {
    Text(String),
    Number(u128),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../commons-scan/tests/fixtures")
            .join(name)
    }

    fn fixture_present(name: &str) {
        let p = fixture(name);
        assert!(
            p.exists(),
            "fixture {name} missing; run scripts/make-fixtures.sh"
        );
    }

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn formats_are_recognised_including_compound_extensions() {
        assert_eq!(Format::from_path(Path::new("a.zip")), Some(Format::Zip));
        assert_eq!(Format::from_path(Path::new("a.CBZ")), Some(Format::Cbz));
        // A cbr IS a rar. The enum keeps both names because the user thinks in
        // "comic" and "rar" and the UI labels them differently, but they are the
        // same container and must not diverge in capability.
        assert_eq!(Format::from_path(Path::new("a.cbr")), Some(Format::Rar));
        assert_eq!(Format::from_path(Path::new("a.7z")), Some(Format::SevenZip));
        assert_eq!(
            Format::from_path(Path::new("a.tar.gz")),
            Some(Format::TarGz),
            "x.tar.gz must not be read as a bare gz"
        );
        assert_eq!(Format::from_path(Path::new("a.tgz")), Some(Format::TarGz));
        assert_eq!(Format::from_path(Path::new("a.mp4")), None);
    }

    #[test]
    fn listing_only_formats_say_so_rather_than_pretending() {
        assert!(Format::Zip.can_extract());
        assert!(
            !Format::Rar.can_extract(),
            "rar needs a native library or an external binary; say so"
        );
        assert!(!Format::SevenZip.can_extract());
    }

    // --- the Zip-Slip corpus -------------------------------------------------

    /// stash#7240, the headline acceptance criterion. Three hostile members and
    /// one that only looks hostile.
    #[test]
    fn the_malicious_fixture_lists_its_good_members_and_refuses_the_rest() {
        fixture_present("evil.zip");
        let l = list_zip(&fixture("evil.zip")).expect("list");

        let good: Vec<&str> = l.files().map(|m| m.raw_name.as_str()).collect();
        assert_eq!(
            good,
            vec!["nested/../ok.txt"],
            "only the member that normalises back inside the root is listed"
        );

        let refused: Vec<&str> = l.refused.iter().map(|m| m.raw_name.as_str()).collect();
        assert!(refused.contains(&"../../etc/passwd"), "{refused:?}");
        assert!(refused.contains(&"/absolute/escape.txt"), "{refused:?}");
        assert!(refused.contains(&"link.jpg"), "{refused:?}");
        assert_eq!(
            refused.len(),
            3,
            "exactly three hostile members: {refused:?}"
        );

        // Every refusal carries a reason, so the UI can explain rather than
        // showing a silently missing file.
        for m in &l.refused {
            assert!(m.rejection.is_some(), "{} has no reason", m.raw_name);
        }
    }

    /// The acceptance criterion stated as a filesystem assertion: extract the
    /// malicious archive and prove the sentinel was not written outside.
    #[test]
    fn extracting_the_malicious_archive_writes_nothing_outside_the_root() {
        fixture_present("evil.zip");
        let outer = tempfile::tempdir().unwrap();
        let dest = outer.path().join("out");

        // A sentinel the attack would overwrite if it succeeded.
        let sentinel = outer.path().join("etc").join("passwd");
        std::fs::create_dir_all(sentinel.parent().unwrap()).unwrap();
        std::fs::write(&sentinel, b"ORIGINAL\n").unwrap();
        let absolute_target = outer.path().join("absolute").join("escape.txt");

        let plan = ExtractionPlan::build(&fixture("evil.zip"), &dest)
            .unwrap()
            .with_size_cap(Some(64 * 1024 * 1024));
        assert_eq!(plan.writes().count(), 1, "only ok.txt is planned");
        assert_eq!(plan.skipped().len(), 3, "three refused, with reasons");

        let report = plan.execute(&fixture("evil.zip")).unwrap();
        assert_eq!(report.written.len(), 1);
        assert!(
            !report.complete(),
            "a partial extraction must not report itself complete"
        );

        // The sentinel is untouched.
        assert_eq!(
            std::fs::read(&sentinel).unwrap(),
            b"ORIGINAL\n",
            "Zip-Slip: the traversal member wrote to {}",
            sentinel.display()
        );
        // The absolute member went nowhere.
        assert!(
            !absolute_target.exists(),
            "the absolute member was written to {}",
            absolute_target.display()
        );
        // And nothing escaped the destination.
        assert_eq!(
            std::fs::read(dest.join("ok.txt")).unwrap(),
            b"payload\n",
            "the one legitimate member was extracted"
        );
        // Prove it by listing what is actually under the parent.
        let stray: Vec<String> = std::fs::read_dir(outer.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(
            stray,
            vec!["etc".to_string(), "out".to_string()],
            "nothing else appeared in the parent directory: {stray:?}"
        );
    }

    #[test]
    fn a_gallery_lists_as_images_and_a_comic_lists_as_pages() {
        fixture_present("gallery.zip");
        fixture_present("book.cbz");

        let g = list_zip(&fixture("gallery.zip")).unwrap();
        assert!(
            g.all_files_are_images(),
            "gallery.zip: {:?}",
            g.files().map(|m| &m.raw_name).collect::<Vec<_>>()
        );
        assert!(g.refused.is_empty());
        assert!(g.total_uncompressed > 0);

        let c = list_zip(&fixture("book.cbz")).unwrap();
        assert!(c.all_files_are_images(), "a cbz is pages, which are images");
        assert_eq!(c.files().count(), 4);
    }

    #[test]
    fn compression_methods_are_reported_for_display() {
        // stash#7230: the method must be visible.
        fixture_present("gallery.zip"); // deflate
        fixture_present("book.cbz"); // store
        let g = list_zip(&fixture("gallery.zip")).unwrap();
        assert!(g.files().all(|m| m.method == CompressionMethod::Deflate));
        let c = list_zip(&fixture("book.cbz")).unwrap();
        assert!(c.files().all(|m| m.method == CompressionMethod::Store));
    }

    #[test]
    fn extracting_a_gallery_produces_every_image() {
        fixture_present("gallery.zip");
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("g");
        let plan = ExtractionPlan::build(&fixture("gallery.zip"), &dest).unwrap();
        let report = plan.execute(&fixture("gallery.zip")).unwrap();
        assert!(
            report.complete(),
            "nothing should be refused: {:?}",
            report.refused
        );
        assert_eq!(report.written.len(), 3);
        for f in &report.written {
            let p = dest.join(&f.relative);
            assert!(p.exists(), "{} missing", p.display());
            assert!(f.size > 0, "{} is empty", f.relative.display());
        }
    }

    /// A zip bomb's write half: a declared 4GB member fills the disk before
    /// anyone notices. The cap is checked against the archive's own declared
    /// size, and a member over it is refused rather than truncated.
    #[test]
    fn a_size_cap_refuses_an_oversized_member_rather_than_filling_the_disk() {
        fixture_present("gallery.zip");
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("g");

        // 1KB sits between the smallest and largest members of this fixture
        // (002.png is 228 bytes, the jpgs are ~10KB), so the assertion is
        // partial: the png is written, the jpgs are refused. That is the
        // interesting case -- a cap that refused everything would be a
        // different test, and a cap that refused nothing would be a no-op.
        let plan = ExtractionPlan::build(&fixture("gallery.zip"), &dest)
            .unwrap()
            .with_size_cap(Some(1024));
        let report = plan.execute(&fixture("gallery.zip")).unwrap();

        assert!(!report.complete(), "the jpgs should have been refused");
        assert_eq!(
            report.written.len(),
            1,
            "only the 228-byte png is under the cap: {:?}",
            report.written
        );
        assert_eq!(report.written[0].relative, PathBuf::from("002.png"));
        assert_eq!(report.refused.len(), 2);
        assert!(
            report.refused.iter().all(|(_, why)| why.contains("cap")),
            "each refusal must name the cap: {:?}",
            report.refused
        );
        // And the refused members were NOT written, in whole or in part.
        assert!(!dest.join("001.jpg").exists());
        assert!(!dest.join("003.jpg").exists());

        // A cap above every member: nothing refused. Both directions, or the
        // cap could be a no-op that always passes.
        let dir2 = tempfile::tempdir().unwrap();
        let dest2 = dir2.path().join("g");
        let plan2 = ExtractionPlan::build(&fixture("gallery.zip"), &dest2)
            .unwrap()
            .with_size_cap(Some(1024 * 1024));
        let report2 = plan2.execute(&fixture("gallery.zip")).unwrap();
        assert!(report2.complete(), "{:?}", report2.refused);
        assert_eq!(report2.written.len(), 3);
    }

    /// No cap at all is the default, and a default that silently refused
    /// everything would be a very confusing failure mode.
    #[test]
    fn no_cap_extracts_everything() {
        fixture_present("gallery.zip");
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("g");
        let plan = ExtractionPlan::build(&fixture("gallery.zip"), &dest).unwrap();
        let report = plan.execute(&fixture("gallery.zip")).unwrap();
        assert!(report.complete());
        assert_eq!(report.written.len(), 3);
    }

    #[test]
    fn a_non_archive_is_reported_as_such_rather_than_as_an_empty_listing() {
        fixture_present("photo.jpg");
        let err = list_zip(&fixture("photo.jpg")).unwrap_err();
        assert!(
            matches!(err, ArchiveError::NotAnArchive { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn a_missing_archive_reports_the_path() {
        let err = list_zip(Path::new("/nonexistent/a.zip")).unwrap_err();
        assert!(matches!(err, ArchiveError::Open { .. }));
        assert!(err.to_string().contains("a.zip"), "{err}");
    }

    // --- page ordering, §5.5 and stash#1659 -------------------------------

    /// The ticket's named assertion for comics: `img_2` sorts before `img_10`.
    #[test]
    fn pages_sort_by_number_not_lexicographically() {
        let order = page_order(&names(&["img_10.jpg", "img_2.jpg", "img_1.jpg"]));
        let sorted: Vec<&str> = order
            .iter()
            .map(|&i| ["img_10.jpg", "img_2.jpg", "img_1.jpg"][i])
            .collect();
        assert_eq!(sorted, vec!["img_1.jpg", "img_2.jpg", "img_10.jpg"]);
    }

    #[test]
    fn zero_padded_and_unpadded_pages_sort_together_naturally() {
        let order = page_order(&names(&[
            "page-3.jpg",
            "page-03.jpg",
            "page-21.jpg",
            "page-100.jpg",
        ]));
        let src = ["page-3.jpg", "page-03.jpg", "page-21.jpg", "page-100.jpg"];
        let sorted: Vec<&str> = order.iter().map(|&i| src[i]).collect();
        // page-03 and page-3 have the same numeric value, so the name tie-break
        // decides: "page-03.jpg" sorts first. The point of the test is that both
        // come before page-21 and page-100, which is where a lexicographic sort
        // would put page-100 second.
        assert_eq!(
            sorted,
            vec!["page-03.jpg", "page-3.jpg", "page-21.jpg", "page-100.jpg"],
            "3 before 21 before 100, with padding not changing the value"
        );
        assert!(
            sorted.iter().position(|s| s.starts_with("page-100"))
                > sorted.iter().position(|s| s.starts_with("page-21")),
            "a lexicographic sort would put page-100 before page-21"
        );
    }

    #[test]
    fn a_comic_fixtures_pages_come_out_in_reading_order() {
        fixture_present("book.cbz");
        let l = list_zip(&fixture("book.cbz")).unwrap();
        let listed: Vec<String> = l.files().map(|m| m.raw_name.clone()).collect();
        let order = page_order(&listed);
        let sorted: Vec<&str> = order.iter().map(|&i| listed[i].as_str()).collect();
        assert_eq!(
            sorted,
            vec!["cover.png", "img_1.jpg", "img_2.jpg", "img_10.jpg"],
            "the fixture stores them out of order on purpose"
        );
    }

    #[test]
    fn names_without_numbers_keep_a_stable_alphabetical_order() {
        let order = page_order(&names(&["cover.png", "back.png", "intro.png"]));
        let src = ["cover.png", "back.png", "intro.png"];
        let sorted: Vec<&str> = order.iter().map(|&i| src[i]).collect();
        assert_eq!(sorted, vec!["back.png", "cover.png", "intro.png"]);
    }

    #[test]
    fn ties_on_the_numeric_key_fall_back_to_the_name() {
        // page1 and page01 have the same value; the tie-break makes the order
        // total rather than dependent on the input order.
        let src_ab = ["page01.jpg", "page1.jpg"];
        let src_ba = ["page1.jpg", "page01.jpg"];
        let from_ab: Vec<&str> = page_order(&names(&src_ab))
            .iter()
            .map(|&i| src_ab[i])
            .collect();
        let from_ba: Vec<&str> = page_order(&names(&src_ba))
            .iter()
            .map(|&i| src_ba[i])
            .collect();
        assert_eq!(
            from_ab, from_ba,
            "the resulting page order must not depend on archive order"
        );
        assert_eq!(from_ab, vec!["page01.jpg", "page1.jpg"]);
    }
}
