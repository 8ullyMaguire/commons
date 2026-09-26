//! The face sidecar: vectors and provenance beside the library, not in SQL.
//!
//! # Why a file and not a table
//!
//! Rule 2 of the spec: the index must work with no external service. A 512-dim
//! `f32` vector per face is 2 KiB, and a library with 200,000 faces is 400 MB
//! of vectors. In SQLite that is a table no query planner helps with, and in
//! Postgres it is a column that wants `pgvector` — which makes the index
//! depend on a Postgres extension, which is exactly what rule 2 forbids.
//!
//! So: a file. It is written once, read by [`Sidecar::search`], and its
//! contents are reproducible by re-running detection. The expensive thing —
//! the clusters derived from it — lives in SQL, and losing this file costs a
//! re-detect, not a re-index.
//!
//! # The format
//!
//! Deliberately simple, because a binary format nobody can read is a format
//! nobody can recover from:
//!
//! ```text
//!   magic   8 bytes  b"CMSIDEAR"
//!   version u32 le
//!   width   u32 le   embedding dimensionality
//!   count   u64 le
//!   ── per face, in insertion order ──
//!   id_len  u16 le, then UTF-8 bytes
//!   kind    u8        0 keyframe, 1 still, 2 headshot
//!   file_id_len u16 le, then UTF-8
//!   timestamp_ms u64 le   (keyframe only)
//!   page    u32 le        (still only)
//!   x,y,w,h f32 le × 4
//!   crop_w, crop_h u32 le × 2
//!   score   f32 le
//!   vector  width × f32 le
//!   ── trailer ──
//!   sha256  32 bytes  over everything above
//! ```
//!
//! The trailer is a digest of the payload, not a second copy of it. A crash
//! mid-flush leaves a short file, and the digest says so. Re-creating it as
//! empty would silently discard every face the user had, and the library
//! would re-detect on the next scan — losing the clusters, which are the
//! expensive part, and keeping nothing to show for it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::face::{BBox, CropSource, FaceCrop};

const MAGIC: &[u8; 8] = b"CMSIDEAR";
const VERSION: u32 = 1;

/// What went wrong with a sidecar.
#[derive(Debug, thiserror::Error)]
pub enum SidecarError {
    #[error("sidecar vector width mismatch: got {got}, expected {expected}")]
    WrongWidth { got: usize, expected: usize },

    #[error("sidecar at {path} is corrupt: {reason}")]
    Corrupt { path: PathBuf, reason: String },

    #[error("sidecar io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl SidecarError {
    fn io(path: &Path, source: std::io::Error) -> Self {
        SidecarError::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}

/// A face index on disk.
#[derive(Debug)]
pub struct Sidecar {
    path: PathBuf,
    width: usize,
    /// Insertion order, because the index is append-only and a face's
    /// position in the file is its id.
    entries: Vec<Entry>,
    dirty: bool,
}

#[derive(Debug, Clone, PartialEq)]
struct Entry {
    crop: FaceCrop,
    vector: Vec<f32>,
}

impl Sidecar {
    /// Create a new, empty sidecar at `path`.
    pub fn create(path: &Path, width: usize) -> Result<Self, SidecarError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| SidecarError::io(path, e))?;
            }
        }
        Ok(Sidecar {
            path: path.to_path_buf(),
            width: width.max(1),
            entries: Vec::new(),
            dirty: true,
        })
    }

    /// Open an existing sidecar, or an empty one if the file is not there.
    ///
    /// A missing file is the normal first-run state and must not be an error.
    /// A *corrupt* file is an error, and the difference matters: the first is
    /// a library with no faces yet, the second is a library whose faces exist
    /// and cannot be read.
    pub fn open(path: &Path) -> Result<Self, SidecarError> {
        Sidecar::open_with_width(path, 0)
    }

    /// Open, requiring a specific embedding width.
    ///
    /// A file written by a different embedding model is refused rather than
    /// reinterpreted: the vectors are opaque, so reading them at the wrong
    /// width produces distances that are all nonsense and none of them
    /// obviously so.
    pub fn open_with_width(path: &Path, expect_width: usize) -> Result<Self, SidecarError> {
        let raw = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let w = expect_width.max(1);
                return Ok(Sidecar {
                    path: path.to_path_buf(),
                    width: w,
                    entries: Vec::new(),
                    dirty: false,
                });
            }
            Err(e) => return Err(SidecarError::io(path, e)),
        };

        let corrupt = |reason: &str| SidecarError::Corrupt {
            path: path.to_path_buf(),
            reason: reason.to_string(),
        };

        if raw.len() < MAGIC.len() + 4 + 4 + 8 + 32 {
            return Err(corrupt("file is shorter than its own header"));
        }
        if &raw[..MAGIC.len()] != MAGIC {
            return Err(corrupt("not a commons face sidecar"));
        }
        let mut o = MAGIC.len();
        let version = u32::from_le_bytes(raw[o..o + 4].try_into().expect("sliced")).to_owned();
        if version != VERSION {
            return Err(corrupt(&format!("unsupported version {version}")));
        }
        o += 4;
        let width = u32::from_le_bytes(raw[o..o + 4].try_into().expect("sliced")) as usize;
        o += 4;
        if expect_width > 0 && width != expect_width {
            return Err(SidecarError::WrongWidth {
                got: width,
                expected: expect_width,
            });
        }
        if width == 0 {
            return Err(corrupt("header declares a zero-width embedding"));
        }
        let count = u64::from_le_bytes(raw[o..o + 8].try_into().expect("sliced"));
        o += 8;

        let body = &raw[o..raw.len() - 32];
        let mut want_digest = [0u8; 32];
        want_digest.copy_from_slice(&raw[raw.len() - 32..]);
        let got = sha256_bytes(body);
        if got != want_digest {
            return Err(corrupt(
                "payload digest does not match: truncated or modified",
            ));
        }

        let mut p = 0usize;
        let mut entries = Vec::with_capacity(count.min(1 << 20) as usize);
        // A free function, not a closure: a closure cannot return a borrow
        // tied to one of its own arguments, and this has to return a slice of
        // the payload.
        fn take<'a>(
            b: &'a [u8],
            p: &mut usize,
            n: usize,
            path: &Path,
        ) -> Result<&'a [u8], SidecarError> {
            if *p + n > b.len() {
                return Err(SidecarError::Corrupt {
                    path: path.to_path_buf(),
                    reason: "payload ended mid-entry".to_string(),
                });
            }
            let s = &b[*p..*p + n];
            *p += n;
            Ok(s)
        }
        let take = |p: &mut usize, n: usize| take(body, p, n, path);
        for _ in 0..count {
            let id_len = u16::from_le_bytes(take(&mut p, 2)?.try_into().expect("sliced")) as usize;
            let id = std::str::from_utf8(take(&mut p, id_len)?)
                .map_err(|_| corrupt("face id is not UTF-8"))?
                .to_string();
            let kind = take(&mut p, 1)?[0];
            let fid_len = u16::from_le_bytes(take(&mut p, 2)?.try_into().expect("sliced")) as usize;
            let fid = std::str::from_utf8(take(&mut p, fid_len)?)
                .map_err(|_| corrupt("file id is not UTF-8"))?
                .to_string();
            let ts = u64::from_le_bytes(take(&mut p, 8)?.try_into().expect("sliced"));
            let page = u32::from_le_bytes(take(&mut p, 4)?.try_into().expect("sliced"));
            let mut coords = [0f32; 4];
            for c in coords.iter_mut() {
                *c = f32::from_le_bytes(take(&mut p, 4)?.try_into().expect("sliced"));
            }
            let cw = u32::from_le_bytes(take(&mut p, 4)?.try_into().expect("sliced"));
            let ch = u32::from_le_bytes(take(&mut p, 4)?.try_into().expect("sliced"));
            let score = f32::from_le_bytes(take(&mut p, 4)?.try_into().expect("sliced"));
            let mut vector = Vec::with_capacity(width);
            for _ in 0..width {
                vector.push(f32::from_le_bytes(
                    take(&mut p, 4)?.try_into().expect("sliced"),
                ));
            }
            let source = match kind {
                0 => CropSource::Keyframe {
                    file_id: fid,
                    timestamp_ms: ts,
                },
                1 => CropSource::Still { file_id: fid, page },
                2 => CropSource::Headshot { file_id: fid },
                other => return Err(corrupt(&format!("unknown crop kind {other}"))),
            };
            // A box read from disk bypasses `BBox::new`, so it is re-checked:
            // a corrupt file is exactly where an inverted box would come from.
            let bbox = BBox::new(coords[0], coords[1], coords[2], coords[3])
                .map_err(|e| corrupt(&format!("stored bounding box is invalid: {e}")))?;
            entries.push(Entry {
                crop: FaceCrop {
                    id,
                    source,
                    bbox,
                    crop_width: cw,
                    crop_height: ch,
                    detector_score: score,
                },
                vector,
            });
        }
        if p != body.len() {
            return Err(corrupt("trailing bytes after the last face"));
        }

        Ok(Sidecar {
            path: path.to_path_buf(),
            width,
            entries,
            dirty: false,
        })
    }

    /// Add a face and its embedding.
    pub fn add(&mut self, crop: &FaceCrop, vector: &[f32]) -> Result<usize, SidecarError> {
        if vector.len() != self.width {
            return Err(SidecarError::WrongWidth {
                got: vector.len(),
                expected: self.width,
            });
        }
        let idx = self.entries.len();
        self.entries.push(Entry {
            crop: crop.clone(),
            vector: vector.to_vec(),
        });
        self.dirty = true;
        Ok(idx)
    }

    /// Write to disk.
    pub fn flush(&mut self) -> Result<(), SidecarError> {
        let mut body = Vec::new();
        for e in &self.entries {
            let id = e.crop.id.as_bytes();
            body.extend_from_slice(&(id.len() as u16).to_le_bytes());
            body.extend_from_slice(id);
            let (kind, ts, page) = match &e.crop.source {
                CropSource::Keyframe { timestamp_ms, .. } => (0u8, *timestamp_ms, 0u32),
                CropSource::Still { page, .. } => (1u8, 0, *page),
                CropSource::Headshot { .. } => (2u8, 0, 0u32),
            };
            body.push(kind);
            let fid = e.crop.source.file_id().as_bytes();
            body.extend_from_slice(&(fid.len() as u16).to_le_bytes());
            body.extend_from_slice(fid);
            body.extend_from_slice(&ts.to_le_bytes());
            body.extend_from_slice(&page.to_le_bytes());
            for v in [e.crop.bbox.x, e.crop.bbox.y, e.crop.bbox.w, e.crop.bbox.h] {
                body.extend_from_slice(&v.to_le_bytes());
            }
            body.extend_from_slice(&e.crop.crop_width.to_le_bytes());
            body.extend_from_slice(&e.crop.crop_height.to_le_bytes());
            body.extend_from_slice(&e.crop.detector_score.to_le_bytes());
            for v in &e.vector {
                body.extend_from_slice(&v.to_le_bytes());
            }
        }

        let mut out = Vec::with_capacity(body.len() + 64);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&(self.width as u32).to_le_bytes());
        out.extend_from_slice(&(self.entries.len() as u64).to_le_bytes());
        out.extend_from_slice(&body);
        out.extend_from_slice(&sha256_bytes(&body));

        // Write to a sibling and rename, so a crash mid-write leaves the old
        // file intact rather than a half-written one that reads as corrupt.
        let tmp = self.path.with_extension("usearch.tmp");
        std::fs::write(&tmp, &out).map_err(|e| SidecarError::io(&tmp, e))?;
        std::fs::rename(&tmp, &self.path).map_err(|e| SidecarError::io(&self.path, e))?;
        self.dirty = false;
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn exists_on_disk(&self) -> bool {
        self.path.exists()
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// The crop at `index`, or `None` if out of range.
    pub fn crop(&self, index: usize) -> Option<&FaceCrop> {
        self.entries.get(index).map(|e| &e.crop)
    }

    /// The embedding at `index`, or `None` if out of range.
    pub fn vector(&self, index: usize) -> Option<&[f32]> {
        self.entries.get(index).map(|e| e.vector.as_slice())
    }

    /// Every crop, in insertion order.
    pub fn crops(&self) -> Vec<&FaceCrop> {
        self.entries.iter().map(|e| &e.crop).collect()
    }

    /// Faces grouped by the file they came from, in file-id order.
    pub fn by_file(&self) -> BTreeMap<&str, Vec<&FaceCrop>> {
        let mut out: BTreeMap<&str, Vec<&FaceCrop>> = BTreeMap::new();
        for e in &self.entries {
            out.entry(e.crop.source.file_id())
                .or_default()
                .push(&e.crop);
        }
        out
    }

    /// The `k` nearest faces to `query`, most similar first.
    ///
    /// Cosine similarity, computed as a dot product because the vectors are
    /// unit length by construction. `1.0 - dot` is the distance; the
    /// subtraction is done here rather than left to a caller so that the
    /// threshold in T-P3-002 is compared against a number with one meaning.
    ///
    /// An exact scan, not an approximate index. At 200k faces this is a few
    /// milliseconds of SIMD, and the ANN index is where T-P3-003 earns its
    /// keep — introduced when a measurement says the exact scan is too slow,
    /// not before, because an approximate index that silently drops a true
    /// match is harder to debug than a slow one.
    pub fn search(&self, query: &[f32], k: usize) -> Result<Vec<SearchHit<'_>>, SidecarError> {
        if query.len() != self.width {
            return Err(SidecarError::WrongWidth {
                got: query.len(),
                expected: self.width,
            });
        }
        let qn: f32 = query.iter().map(|x| x * x).sum::<f32>().sqrt();
        if qn <= f32::EPSILON {
            return Ok(Vec::new());
        }
        let mut hits: Vec<SearchHit<'_>> = self
            .entries
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let dot: f32 = e.vector.iter().zip(query).map(|(a, b)| a * b).sum::<f32>();
                let vn: f32 = e.vector.iter().map(|x| x * x).sum::<f32>().sqrt();
                let sim = if vn > f32::EPSILON {
                    dot / (vn * qn)
                } else {
                    0.0
                };
                SearchHit {
                    index: i,
                    crop: &e.crop,
                    similarity: sim,
                }
            })
            .collect();
        // Ties broken by index so a result set is deterministic: an unstable
        // order makes a clustering bug look like it comes and goes.
        hits.sort_by(|a, b| {
            b.similarity
                .total_cmp(&a.similarity)
                .then(a.index.cmp(&b.index))
        });
        hits.truncate(k);
        Ok(hits)
    }
}

/// One search result.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit<'a> {
    pub index: usize,
    pub crop: &'a FaceCrop,
    pub similarity: f32,
}

impl SearchHit<'_> {
    /// `1 - similarity`: 0 identical, 2 opposite.
    pub fn distance(&self) -> f32 {
        1.0 - self.similarity
    }
}

/// SHA-256 of a byte slice, matching [`crate::model`]'s implementation.
fn sha256_bytes(data: &[u8]) -> [u8; 32] {
    use crate::model::Sha256;
    let mut h = Sha256::new();
    h.update(data);
    let hex = h.finish_hex();
    let mut out = [0u8; 32];
    for (i, chunk) in hex.as_bytes().chunks(2).enumerate() {
        let s = std::str::from_utf8(chunk).expect("hex is ASCII");
        out[i] = u8::from_str_radix(s, 16).expect("hex digits");
    }
    out
}
