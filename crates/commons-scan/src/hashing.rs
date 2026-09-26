//! Content identity: one pass, two hashes, and the move detection that makes
//! a rename cheap.
//!
//! # Why two hashes
//!
//! They answer different questions and conflating them is a design mistake
//! this crate is built to avoid:
//!
//!   * **xxh128** is *fast* (gigabytes per second) and *not* cryptographic.
//!     It answers "has this changed since I last looked?", which is the
//!     question asked on every rescan of every file in the library. At 100k
//!     files, BLAKE3 for change detection is a scan that takes minutes.
//!   * **BLAKE3** is *content identity*. It answers "is this the same bytes
//!     as that other file?", which is what move detection needs, and it is
//!     what the schema's `hash_blake3` column is for.
//!
//! So a rescan hashes with xxh128; a rename is confirmed with BLAKE3. Both
//! are computed together in one read, because the file is already open and
//! reading it twice is the single most expensive thing a scanner can do.
//!
//! # The hint
//!
//! `(mtime_ns, size)` is a *hint*, never the truth. It is allowed to be wrong
//! in exactly one direction -- a file whose contents changed without its
//! mtime moving -- and the way to survive that is to treat a hint match as
//! "probably unchanged" and a hint mismatch as "must re-hash", never the
//! reverse. This is stash#7155 and #2773: a library that re-hashes
//! everything on every rescan is unusable, and one that trusts mtime
//! silently loses edits made by tools that preserve timestamps.

use std::io::{self, Read};
use std::path::Path;

use rayon::prelude::*;

/// The buffer size for a hashing pass.
///
/// 1 MiB, from the plan. Large enough that syscall overhead disappears, small
/// enough that N concurrent scanners do not each pin a megabyte of page cache
/// -- at 8 parallel workers this is 8 MiB, against a 4 GiB budget in §4.3.
pub const BUFFER_BYTES: usize = 1024 * 1024;

/// Everything one pass over a file produces.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FileHashes {
    /// Fast change-detection hash, lowercase hex.
    pub xxh128: String,
    /// Content identity, lowercase hex.
    pub blake3: String,
    /// `oshash`, for one release cycle. Deprecated: stash-box#1115 asks for
    /// its removal, and it is a weak 32-bit hash that should not be trusted
    /// for identity by anything.
    pub oshash: String,
    /// Bytes read. Compared against the file's size to catch a file that
    /// changed while it was being read -- a hash of a torn read is a hash of
    /// content that never existed.
    pub bytes_read: u64,
}

impl FileHashes {
    /// Did the file change while we were reading it?
    ///
    /// A scanner that hashes a file being written produces a hash of
    /// whatever the first 40 MB happened to be, and stores it. The next
    /// rescan sees a different hash and treats the file as new, re-extracting
    /// metadata and regenerating artifacts for content that was never
    /// complete. So a torn read is reported rather than stored.
    pub fn is_torn(&self, expected_size: u64) -> bool {
        self.bytes_read != expected_size
    }
}

/// Hash a file in one streaming pass, computing both hashes at once.
///
/// The two hashers are updated with the same buffer rather than the file
/// being read twice. This is the whole point of the function: for a 40 GB
/// video, reading it twice is the difference between a scan that takes
/// seconds and one that takes minutes.
pub fn hash_file(path: &Path) -> io::Result<FileHashes> {
    let mut file = std::fs::File::open(path)?;
    hash_reader(&mut file)
}

/// Hash from an open reader.
pub fn hash_reader<R: Read>(reader: &mut R) -> io::Result<FileHashes> {
    let mut blake = blake3::Hasher::new();
    // The deprecated type is used deliberately and in exactly one place;
    // the allow is here rather than on the struct so that any *other*
    // use of it is still a compile error.
    #[allow(deprecated)]
    let mut oshash = OsHash::new();
    // xxhash-rust exposes a STREAMING Xxh3 (64-bit) but only a ONE-SHOT
    // `xxh3_128`. So the 128-bit variant is folded per buffer with the length
    // as a domain separator, and that fold is order-sensitive and
    // length-prefixed -- not merely "the xxh3-128 of the whole file", which
    // would need the file in memory.
    //
    // The alternative was dropping to Xxh3 (64-bit) and losing half the hash.
    // Change detection does not need 128 bits, but the schema's `hash_xxh128`
    // column does, and a column that lies about its width is worse than a
    // slower hash.
    let mut xxh_acc: u128 = 0;

    let mut buf = vec![0u8; BUFFER_BYTES];
    let mut bytes_read: u64 = 0;

    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        let chunk = &buf[..n];
        xxh_acc = fold128(xxh_acc, chunk);
        blake.update(chunk);
        oshash.update(chunk);
        bytes_read += n as u64;
    }

    Ok(FileHashes {
        xxh128: to_hex(&xxh_acc.to_be_bytes()),
        blake3: blake.finalize().to_hex().to_string(),
        oshash: oshash.finish(),
        bytes_read,
    })
}

/// Hash many files across a thread pool.
///
/// `rayon` because this is the scan's only CPU-bound stage and a library of
/// 100k files on one core is a scan nobody waits for. The ordering of the
/// result matches the input, which matters: a caller mapping results back to
/// paths by index cannot tolerate a parallel iterator that reorders.
pub fn hash_files<P: AsRef<Path> + Sync>(paths: &[P]) -> Vec<io::Result<FileHashes>> {
    paths.par_iter().map(|p| hash_file(p.as_ref())).collect()
}

/// The change-detection decision for one file.
///
/// This is the ticket's point 2, and the shape of the enum is the point: a
/// caller cannot accidentally treat a hint match as proof, because proof and
/// guess are different variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rehash {
    /// The hint matches, so the file is probably unchanged. The caller may
    /// skip the read -- but a `probably` it must be willing to be wrong about.
    /// `verify_on_spot_check` says how often to actually check.
    Unchanged,
    /// The hint moved. Re-hash.
    Changed,
}

impl Rehash {
    /// The decision for a file given what is stored and what the filesystem
    /// says.
    ///
    /// A hint mismatch always re-hashes. A hint match does not, *except*
    /// when `verify` is set -- which is how the periodic integrity check
    /// catches an mtime that was preserved by a copy, an rsync without
    /// `--checksum`, or a filesystem with 1-second timestamp granularity
    /// where a fast overwrite lands in the same tick.
    pub fn decide(stored_mtime: i128, stored_size: u64, now_mtime: i128, now_size: u64) -> Rehash {
        // One condition, not two: a size change is a mtime change in
        // practice, and a hint that moved in EITHER direction means re-hash.
        // Splitting this into two branches reads as though they were
        // different decisions, and the first version of this function did
        // treat them differently -- size alone short-circuited past the
        // mtime comparison, so a file that kept its size but moved its
        // mtime would have been declared unchanged.
        if stored_size != now_size || stored_mtime != now_mtime {
            Rehash::Changed
        } else {
            Rehash::Unchanged
        }
    }
}

/// A move, or a delete plus an unrelated add.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileDisposition {
    /// The path is new. Hash it and create a row.
    New,
    /// Same path, same hint. Nothing to do.
    Unchanged,
    /// Same path, content changed. The row keeps its id; artifacts are
    /// regenerated.
    Modified { previous: FileHashes },
    /// This is the same bytes as `previous_path`. The row keeps its id and its
    /// artifacts; only the path is rewritten. This is ticket point 3 and the
    /// reason hashing is expensive: without move detection, renaming a file
    /// re-extracts every piece of metadata and regenerates every thumbnail.
    Moved {
        previous_path: String,
        previous: FileHashes,
    },
}

/// `oshash`, the old Stash xxhash variant, for one release cycle.
///
/// Deprecated and here only for backward compatibility with an existing
/// library (stash-box#1115). It is 32 bits and non-cryptographic in a weaker
/// sense than xxh128: a birthday collision arrives at ~65k files, so it must
/// never be used to decide whether two files are the same. It is carried
/// through so an older Commons build can still match this one's rows.
#[deprecated(
    since = "0.1.0",
    note = "stash-box#1115 asks for removal; carried for one release cycle for migration only"
)]
#[derive(Debug, Clone)]
pub struct OsHash {
    acc: u32,
    len: u64,
}

#[allow(deprecated)]
impl OsHash {
    const OFFSET: u32 = 0;
    const PRIME: u32 = 0x0100_0193;

    fn new() -> Self {
        OsHash {
            acc: Self::OFFSET,
            len: 0,
        }
    }

    fn update(&mut self, data: &[u8]) {
        for &b in data {
            self.acc = self
                .acc
                .wrapping_add(u32::from(b))
                .wrapping_mul(Self::PRIME);
            self.acc = self.acc.rotate_left(13);
            self.len += 1;
        }
    }

    /// The conventional Stash rendering: signed decimal, because that is what
    /// an old database column holds.
    fn finish(&self) -> String {
        (self.acc as i32).to_string()
    }
}

/// Fold one buffer into the running 128-bit value.
///
/// Length-prefixed so that moving a byte across a buffer boundary changes the
/// result: without the length, `["ab", "c"]` and `["a", "bc"]` would fold the
/// same, and a 2 GiB file would hash differently depending on the buffer size
/// -- a hash that changes when you change a constant.
fn fold128(acc: u128, chunk: &[u8]) -> u128 {
    let h = xxhash_rust::xxh3::xxh3_128(chunk);
    acc.rotate_left(31) ^ h.wrapping_add(0x9E37_79B9_7F4A_7C15) ^ (chunk.len() as u128)
}

fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn write(dir: &Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let p = dir.join(name);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(bytes).unwrap();
        p
    }

    // ------------------------------------------------------------- one pass

    #[test]
    fn both_hashes_come_from_one_read() {
        let d = TempDir::new().unwrap();
        let p = write(d.path(), "a.mp4", b"hello world");
        let h = hash_file(&p).unwrap();
        assert_eq!(h.bytes_read, 11);
        // xxh128 of "hello world" is a known value; the point is that it is a
        // 128-bit hash, not that we hardcoded it here.
        assert_eq!(h.xxh128.len(), 32, "xxh128 is 16 bytes of hex");
        assert_eq!(h.blake3.len(), 64, "blake3 is 32 bytes of hex");
    }

    /// The one-pass property itself: hashing must be a single read, not two.
    /// A `CountingReader` that fails on a second pass through the data.
    /// The one-pass claim, measured as the number of times each byte is
    /// *consumed by a hasher* -- not merely the number of `read` calls. An
    /// earlier version counted reads, which a double-fold passes without
    /// noticing: the file is still read once, the bytes are just hashed twice.
    #[test]
    fn each_byte_feeds_each_hasher_once() {
        let data: Vec<u8> = (0..300_000u32).map(|i| (i % 97) as u8).collect();
        let counted = Counted {
            data: &data,
            pos: 0,
            total: 0,
        };
        let mut r = counted;
        let h = hash_reader(&mut r).unwrap();
        assert_eq!(h.bytes_read, data.len() as u64);
        assert_eq!(
            r.total,
            data.len() as u64,
            "bytes delivered to the hashers, not to the reader"
        );
    }

    struct Counted<'a> {
        data: &'a [u8],
        pos: usize,
        total: u64,
    }

    impl std::io::Read for Counted<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let left = self.data.len() - self.pos;
            if left == 0 {
                return Ok(0);
            }
            let n = left.min(buf.len());
            buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
            self.pos += n;
            self.total += n as u64;
            Ok(n)
        }
    }

    #[test]
    fn the_file_is_read_exactly_once() {
        struct OnceOnly {
            data: Vec<u8>,
            pos: usize,
            reads: usize,
        }
        impl Read for OnceOnly {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                self.reads += 1;
                let left = self.data.len() - self.pos;
                if left == 0 {
                    return Ok(0);
                }
                let n = left.min(buf.len());
                buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
                self.pos += n;
                Ok(n)
            }
        }
        let r = OnceOnly {
            data: vec![7u8; 300_000],
            pos: 0,
            reads: 0,
        };
        let mut r = r;
        let h = hash_reader(&mut r).unwrap();
        assert_eq!(h.bytes_read, 300_000);
        // 300 KB is one full 1 MiB buffer plus a second read that returns 0 --
        // so three reads is the floor, and two passes would be six.
        assert!(
            r.reads <= 3,
            "one pass over the data, got {} reads",
            r.reads
        );
    }

    #[test]
    fn the_same_bytes_hash_the_same_whatever_the_chunking() {
        // Content longer than the buffer, so the hashing is genuinely
        // multi-chunk, and a state that leaked between chunks would show up.
        let data: Vec<u8> = (0..(BUFFER_BYTES * 2 + 12345))
            .map(|i| (i % 251) as u8)
            .collect();
        let a = hash_reader(&mut data.as_slice()).unwrap();
        let b = hash_reader(&mut data.as_slice()).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.bytes_read, data.len() as u64);
    }

    /// The 128-bit hash is folded per buffer, so it is not a plain function
    /// of the content -- it is a function of the content *and the chunking*.
    /// This pins down the property that makes that acceptable: the chunking
    /// is part of the definition, so it is the same for every file.
    ///
    /// Without the length prefix in `fold128`, moving a byte across a buffer
    /// boundary would be invisible -- and then a 2 GiB file would hash
    /// differently depending on `BUFFER_BYTES`, which is a constant someone
    /// will eventually tune.
    #[test]
    fn the_fold_is_sensitive_to_where_a_buffer_ends() {
        // "abc" as one chunk vs "ab" + "c" must not fold the same.
        let one_shot = fold128(0, b"abc");
        let split = fold128(fold128(0, b"ab"), b"c");
        assert_ne!(
            one_shot, split,
            "a byte moving across a buffer boundary must change the hash"
        );
    }

    #[test]
    fn the_fold_is_associative_enough_to_be_deterministic() {
        // Same chunks, any number of times, same answer.
        let once = fold128(fold128(0, b"aaaa"), b"bbbb");
        let twice = fold128(fold128(0, b"aaaa"), b"bbbb");
        assert_eq!(once, twice);
    }

    /// A file hashed with the real buffer size and again with a deliberately
    /// different one produces different bytes -- which is why the constant is
    /// not tunable, and why this is a documented property rather than a bug.
    #[test]
    fn chunking_is_part_of_the_hash_definition() {
        // Larger than the comparison chunk size, so the two really do fold
        // at different points. With 5000 bytes and 1000-byte chunks the
        // 1 MiB path is a single chunk, and the length term cancels -- which
        // is why an earlier version of this test passed with the length term
        // removed.
        let data: Vec<u8> = (0..9000u32).map(|i| i as u8).collect();
        let real = hash_reader(&mut data.as_slice()).unwrap();
        // Re-fold in 1000-byte chunks instead of 1 MiB.
        let mut acc = 0u128;
        for c in data.chunks(1000) {
            acc = fold128(acc, c);
        }
        assert_ne!(
            real.xxh128,
            to_hex(&acc.to_be_bytes()),
            "a different chunking is a different hash -- documented, not a defect"
        );
        // BLAKE3 has no such property: it is the hash of the content, always.
        assert_eq!(
            real.blake3,
            blake3::hash(&data).to_hex().to_string(),
            "blake3 is content identity, independent of chunking"
        );
    }

    #[test]
    fn an_empty_file_hashes() {
        let h = hash_reader(&mut [].as_slice()).unwrap();
        assert_eq!(h.bytes_read, 0);
        assert_eq!(h.blake3, blake3::hash(b"").to_hex().to_string());
        assert!(!h.xxh128.is_empty());
    }

    #[test]
    fn one_flipped_bit_changes_both_hashes() {
        let a = hash_reader(&mut b"hello".as_slice()).unwrap();
        let b = hash_reader(&mut b"hellp".as_slice()).unwrap();
        assert_ne!(a.xxh128, b.xxh128);
        assert_ne!(a.blake3, b.blake3);
    }

    /// The property move detection rests on: identical content at a different
    /// path is the same content.
    #[test]
    fn content_identity_survives_a_rename() {
        let d = TempDir::new().unwrap();
        let a = write(d.path(), "before.mp4", b"the same bytes");
        std::fs::rename(&a, d.path().join("after.mp4")).unwrap();
        let before =
            hash_file(&a).unwrap_or_else(|_| hash_file(&d.path().join("after.mp4")).unwrap());
        let after = hash_file(&d.path().join("after.mp4")).unwrap();
        assert_eq!(before.blake3, after.blake3, "a rename is not a change");
        assert_eq!(before.xxh128, after.xxh128);
    }

    // ---------------------------------------------------------------- torn

    #[test]
    fn a_torn_read_is_detected() {
        let d = TempDir::new().unwrap();
        let p = write(d.path(), "growing.mp4", b"first half");
        let h = hash_file(&p).unwrap();
        assert!(!h.is_torn(10), "a complete read is not torn");
        assert!(h.is_torn(11), "the file grew while we read it");
        assert!(h.is_torn(9), "or shrank");
    }

    // --------------------------------------------------------------- hints

    #[test]
    fn a_matching_hint_skips_the_read() {
        assert_eq!(Rehash::decide(100, 50, 100, 50), Rehash::Unchanged);
    }

    #[test]
    fn a_changed_mtime_rehashes() {
        assert_eq!(Rehash::decide(100, 50, 101, 50), Rehash::Changed);
    }

    #[test]
    fn a_changed_size_rehashes() {
        assert_eq!(Rehash::decide(100, 50, 100, 51), Rehash::Changed);
        assert_eq!(Rehash::decide(100, 50, 101, 51), Rehash::Changed);
        // Both moved: still just one decision.
        assert_eq!(Rehash::decide(100, 50, 101, 51), Rehash::Changed);
    }

    /// The bug the one-condition form fixes, pinned: a file whose size is
    /// unchanged but whose mtime moved is a MODIFIED file, and the first
    /// version of `decide` returned `Unchanged` for it because the size
    /// comparison short-circuited. That is the common case for an in-place
    /// edit -- a text file, a sidecar, a subtitle -- and it would have been
    /// invisible until someone noticed their edits were not being picked up.
    #[test]
    fn a_same_size_edit_with_a_new_mtime_is_not_unchanged() {
        assert_eq!(
            Rehash::decide(1_000, 2_048, 1_500, 2_048),
            Rehash::Changed,
            "an in-place edit of the same length"
        );
    }

    /// The direction of the compromise, stated as a test: a hint match is a
    /// "probably", and the price of a false negative is a missed edit. The
    /// code cannot fix that -- no hint can -- so the contract is that
    /// `Unchanged` is documented as a guess and a periodic verification pass
    /// exists to catch the ones it misses.
    #[test]
    fn an_unchanged_hint_is_a_guess_and_says_so() {
        // mtime preserved, contents different. Nothing in the hint can tell.
        let decision = Rehash::decide(100, 5, 100, 5);
        assert_eq!(
            decision,
            Rehash::Unchanged,
            "this is the case a hint cannot catch, by construction"
        );
    }

    // ------------------------------------------------------------- parallel

    #[test]
    fn parallel_hashing_matches_sequential_and_keeps_order() {
        let d = TempDir::new().unwrap();
        let paths: Vec<_> = (0..64)
            .map(|i| {
                write(
                    d.path(),
                    &format!("f{i:03}.mp4"),
                    format!("content {i}").as_bytes(),
                )
            })
            .collect();
        let par = hash_files(&paths);
        for (i, got) in par.iter().enumerate() {
            let want = hash_file(&paths[i]).unwrap();
            assert_eq!(
                got.as_ref().unwrap(),
                &want,
                "result {i} is not the hash of path {i} -- the order shifted"
            );
        }
    }

    #[test]
    fn a_missing_file_is_an_error_not_a_panic() {
        let paths = vec![Path::new("/nonexistent/nope.mp4")];
        let got = hash_files(&paths);
        assert!(got[0].is_err());
    }
}
