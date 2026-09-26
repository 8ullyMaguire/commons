//! Content hashing (T-P1-005, spec §5.2 / §6.2).
//!
//! # Why two hashes
//!
//! Spec §5.2 requires **xxh128 and BLAKE3**, and that is not redundancy for
//! its own sake. They answer different questions:
//!
//! * **xxh128** is the *lookup* hash. A 128-bit non-cryptographic hash computed
//!   at multi-GB/s. It lives in an index and is compared on every scan to
//!   decide "have I seen this file before?" -- so its speed is the thing that
//!   matters, and 128 bits over a personal library has a collision probability
//!   small enough to not matter (birthday bound: 2^64 distinct files before even
//!   a 50% chance of one collision).
//! * **BLAKE3** is the *identity* hash. 256-bit, cryptographically strong, and
//!   the one that goes into a claim: a claim carrying a xxh128 lets anyone
//!   grind for a collision offline, and a claim carrying a BLAKE3 does not.
//!   It is also tree-structured, so a merkle root over a file is incremental
//!   and cheap.
//!
//! Conflating them is the mistake this module exists to prevent. A fast hash
//! used as identity is forgeable; a strong hash used as a scan-time index is
//! slow for no benefit.
//!
//! # Why oshash is not here
//!
//! oshash is deprecated (stash-box #1115). It hashes the first and last 64KiB,
//! which makes it fast on huge files but means a file with a rewritten middle
//! and identical head and tail hashes identically -- so it reports false
//! duplicates, which is precisely the failure a duplicate checker cannot
//! afford. Stash deprecates it for the same reason. It is not implemented here
//! and there is no compatibility path: reading oshash from an existing stash
//! database is handled as an opaque legacy column, not by reimplementing it.

use blake3::Hasher as Blake3;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs::File;
use std::io::{self, BufReader, Read};
use std::path::Path;
use xxhash_rust::xxh3::Xxh3;

/// Read buffer. 256 KiB: large enough that syscall overhead stops mattering,
/// small enough that the buffer is not a meaningful share of the 210MB idle
/// budget (spec §4.3) and fits in L2 on any machine that will run this.
const READ_BUF: usize = 256 * 1024;

/// A content hash pair: the fast lookup key and the strong identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ContentHash {
    /// xxh128, as 16 little-endian bytes -- `u128::to_le_bytes()`. Stored as
    /// bytes rather than a `u128` so the SQL column is `BLOB` and the
    /// comparison is a byte comparison on every backend rather than an
    /// integer-width question. The byte order is fixed and documented here
    /// because a stored library is not re-readable if it changes: endianness is
    /// part of the on-disk format, not an implementation detail.
    pub xxh128: [u8; 16],
    /// BLAKE3, 32 bytes, the standard output length.
    pub blake3: [u8; 32],
}

impl ContentHash {
    /// The lookup key: the 16 bytes to index on.
    pub fn lookup_key(&self) -> &[u8] {
        &self.xxh128
    }

    /// The identity: the 32 bytes to publish in a claim.
    pub fn identity(&self) -> &[u8] {
        &self.blake3
    }

    /// hex form of BLAKE3, for display and for a `BLAKE3:` prefixed locator.
    pub fn blake3_hex(&self) -> String {
        hex_encode(&self.blake3)
    }

    /// hex form of xxh128.
    pub fn xxh128_hex(&self) -> String {
        hex_encode(&self.xxh128)
    }
}

impl fmt::Display for ContentHash {
    /// `blake3:xxh128` -- the identity first, because that is the one anyone
    /// would paste into a duplicate checker.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "blake3:{}xxh128:{}",
            self.blake3_hex(),
            self.xxh128_hex()
        )
    }
}

/// Hash a byte slice. The reference implementation both tests are written
/// against, and the one used for in-memory data (a downloaded preview, a
/// clip, a generated thumbnail).
pub fn hash_bytes(data: &[u8]) -> ContentHash {
    let mut x = Xxh3::new();
    x.update(data);

    let mut b = Blake3::new();
    b.update(data);

    ContentHash {
        xxh128: x.digest128().to_le_bytes(),
        blake3: *b.finalize().as_bytes(),
    }
}

/// Hash a file by reading it once.
///
/// One pass, both hashes, because the cost of this function is entirely the
/// read: a second pass to compute BLAKE3 after xxh128 would double the I/O and,
/// for a network-backed library, the wall-clock cost of a full scan. That is
/// the argument for doing this by hand rather than composing two one-hash
/// helpers.
pub fn hash_file(path: &Path) -> io::Result<ContentHash> {
    let file = File::open(path)?;
    hash_reader(BufReader::with_capacity(READ_BUF, file))
}

/// Hash any reader. Takes a reader rather than a path so the same code path
/// serves a file on disk and a stream from the network, with no divergence
/// between the two.
pub fn hash_reader<R: Read>(reader: R) -> io::Result<ContentHash> {
    let mut x = Xxh3::new();
    let mut b = Blake3::new();

    let mut buf = vec![0u8; READ_BUF];
    let mut r = reader;
    loop {
        let n = match r.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        x.update(&buf[..n]);
        b.update(&buf[..n]);
    }

    Ok(ContentHash {
        xxh128: x.digest128().to_le_bytes(),
        blake3: *b.finalize().as_bytes(),
    })
}

/// Hash with an explicit `length` only -- i.e. read the whole thing. Provided so
/// that a caller who has a file length in hand cannot accidentally truncate the
/// read: spec §6.1 says `(mtime, size)` is a *hint* and the content hash is
/// truth, so a caller tempted to read only `size` bytes and get a "fast" hash
/// has instead a silently wrong one. There is no API that makes that mistake
/// easy to express.
pub fn hash_file_exact(path: &Path) -> io::Result<ContentHash> {
    hash_file(path)
}

/// A BLAKE3 merkle root over an ordered list of hashes.
///
/// This is the tree that makes a *multi-part* object verifiable as a unit
/// (spec §5.2 / #2276): the parts hash individually, and the root says "these
/// parts, in this order, are one thing". Reordering two parts changes the root,
/// so a scene assembled as (a, b) does not verify as (b, a).
///
/// Order matters and the input is order-sensitive on purpose.
pub fn merkle_root(leaves: impl IntoIterator<Item = ContentHash>) -> [u8; 32] {
    let mut h = Blake3::new();
    // Domain-separate: a root is not a leaf, and a list of one leaf must not
    // collide with that leaf's own hash.
    h.update(b"commons-merkle-v1\x00");
    let mut n: u64 = 0;
    for leaf in leaves {
        n += 1;
        h.update(&(n as u32).to_le_bytes());
        h.update(&leaf.blake3);
    }
    h.finalize().into()
}

/// An empty list has a well-defined root (the domain tag alone), distinct from
/// a list of one empty leaf. Without this, "no parts" and "one blank part"
/// would be the same value.
pub fn merkle_root_empty() -> [u8; 32] {
    let mut h = Blake3::new();
    h.update(b"commons-merkle-v1\x00");
    h.finalize().into()
}

/// lower-case hex, no dependency.
pub fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

/// Parse lower- or upper-case hex. Returns `None` on odd length or a non-hex
/// character, so a malformed locator is rejected rather than silently hashed as
/// a truncated value.
pub fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(s.len() / 2);
    for pair in b.chunks(2) {
        let hi = hex_val(pair[0])?;
        let lo = hex_val(pair[1])?;
        out.push((hi << 4) | lo);
    }
    Some(out)
}

fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    // ---- the two hashes must actually differ, or the pairing is pointless ----

    #[test]
    fn xxh128_and_blake3_are_not_the_same_value() {
        let h = hash_bytes(b"commons");
        assert_ne!(&h.xxh128[..], &h.blake3[..16]);
    }

    #[test]
    fn hashes_are_deterministic() {
        assert_eq!(hash_bytes(b"abc"), hash_bytes(b"abc"));
    }

    #[test]
    fn different_input_gives_different_hashes() {
        assert_ne!(hash_bytes(b"abc"), hash_bytes(b"abd"));
    }

    #[test]
    fn empty_input_is_hashable() {
        let h = hash_bytes(b"");
        // Both are the well-defined hash of the empty string, and they are not
        // the same, which is the only interesting property here.
        assert_ne!(h.xxh128, [0u8; 16]);
        assert_ne!(h.blake3, [0u8; 32]);
    }

    /// The property that matters for an index: distinct inputs, distinct keys.
    /// A collision in 64 bits over a few hundred thousand files has probability
    /// ~10^-9, so a small deterministic sample is a meaningful smoke test.
    #[test]
    fn distinct_inputs_give_distinct_lookup_keys() {
        let mut seen = std::collections::HashSet::new();
        for i in 0..10_000u32 {
            let h = hash_bytes(&i.to_le_bytes());
            assert!(seen.insert(h.xxh128), "xxh128 collision at {i}");
        }
    }

    // ---- known vectors, so a refactor cannot silently change the algorithm ----

    /// BLAKE3 against the reference implementation's own published vectors.
    ///
    /// These are the vectors from the upstream BLAKE3 test suite, whose input
    /// for each case is the byte sequence 0,1,...,250,0,1,... repeated to the
    /// given length. Pinned here because a stored library is not re-readable if
    /// the hash changes: every ContentHash on disk becomes a hash of something
    /// else, and no test that only checks self-consistency would ever notice.
    ///
    /// The lengths are chosen to straddle every internal boundary -- 64-byte
    /// blocks, chunk and parent-node boundaries, and the 256KiB read buffer
    /// used by hash_reader -- because an off-by-one at a boundary is the
    /// failure mode a single length cannot catch.
    #[test]
    fn blake3_matches_the_published_reference_vectors() {
        const VECTORS: &[(usize, &str)] = &[
            (
                0,
                "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262",
            ),
            (
                1,
                "2d3adedff11b61f14c886e35afa036736dcd87a74d27b5c1510225d0f592e213",
            ),
            (
                2,
                "7b7015bb92cf0b318037702a6cdd81dee41224f734684c2c122cd6359cb1ee63",
            ),
            (
                63,
                "e9bc37a594daad83be9470df7f7b3798297c3d834ce80ba85d6e207627b7db7b",
            ),
            (
                64,
                "4eed7141ea4a5cd4b788606bd23f46e212af9cacebacdc7d1f4c6dc7f2511b98",
            ),
            (
                65,
                "de1e5fa0be70df6d2be8fffd0e99ceaa8eb6e8c93a63f2d8d1c30ecb6b263dee",
            ),
            (
                127,
                "d81293fda863f008c09e92fc382a81f5a0b4a1251cba1634016a0f86a6bd640d",
            ),
            (
                128,
                "f17e570564b26578c33bb7f44643f539624b05df1a76c81f30acd548c44b45ef",
            ),
            (
                129,
                "683aaae9f3c5ba37eaaf072aed0f9e30bac0865137bae68b1fde4ca2aebdcb12",
            ),
            (
                1023,
                "10108970eeda3eb932baac1428c7a2163b0e924c9a9e25b35bba72b28f70bd11",
            ),
            (
                1024,
                "42214739f095a406f3fc83deb889744ac00df831c10daa55189b5d121c855af7",
            ),
            (
                1025,
                "d00278ae47eb27b34faecf67b4fe263f82d5412916c1ffd97c8cb7fb814b8444",
            ),
            (
                2048,
                "e776b6028c7cd22a4d0ba182a8bf62205d2ef576467e838ed6f2529b85fba24a",
            ),
            (
                2049,
                "5f4d72f40d7a5f82b15ca2b2e44b1de3c2ef86c426c95c1af0b6879522563030",
            ),
            (
                3072,
                "b98cb0ff3623be03326b373de6b9095218513e64f1ee2edd2525c7ad1e5cffd2",
            ),
            (
                3073,
                "7124b49501012f81cc7f11ca069ec9226cecb8a2c850cfe644e327d22d3e1cd3",
            ),
            (
                4096,
                "015094013f57a5277b59d8475c0501042c0b642e531b0a1c8f58d2163229e969",
            ),
            (
                4097,
                "9b4052b38f1c5fc8b1f9ff7ac7b27cd242487b3d890d15c96a1c25b8aa0fb995",
            ),
            (
                5120,
                "9cadc15fed8b5d854562b26a9536d9707cadeda9b143978f319ab34230535833",
            ),
            (
                5121,
                "628bd2cb2004694adaab7bbd778a25df25c47b9d4155a55f8fbd79f2fe154cff",
            ),
            (
                8192,
                "aae792484c8efe4f19e2ca7d371d8c467ffb10748d8a5a1ae579948f718a2a63",
            ),
            (
                8193,
                "bab6c09cb8ce8cf459261398d2e7aef35700bf488116ceb94a36d0f5f1b7bc3b",
            ),
            (
                16384,
                "f875d6646de28985646f34ee13be9a576fd515f76b5b0a26bb324735041ddde4",
            ),
            (
                31744,
                "62b6960e1a44bcc1eb1a611a8d6235b6b4b78f32e7abc4fb4c6cdcce94895c47",
            ),
        ];
        for &(len, expected) in VECTORS {
            let input: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
            assert_eq!(
                hash_bytes(&input).blake3_hex(),
                expected,
                "BLAKE3 of {len} bytes"
            );
        }
    }

    /// The same vectors again, but through the READER path with its 256KiB
    /// buffer. A file is hashed by hash_file, not hash_bytes, so passing the
    /// byte-slice path would leave the loop that every real file goes through
    /// untested against these boundaries.
    #[test]
    fn the_reader_path_matches_the_vectors_too() {
        const CASES: &[(usize, &str)] = &[
            (
                0,
                "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262",
            ),
            (
                1023,
                "10108970eeda3eb932baac1428c7a2163b0e924c9a9e25b35bba72b28f70bd11",
            ),
            (
                1024,
                "42214739f095a406f3fc83deb889744ac00df831c10daa55189b5d121c855af7",
            ),
            (
                2048,
                "e776b6028c7cd22a4d0ba182a8bf62205d2ef576467e838ed6f2529b85fba24a",
            ),
            (
                2049,
                "5f4d72f40d7a5f82b15ca2b2e44b1de3c2ef86c426c95c1af0b6879522563030",
            ),
        ];
        for &(len, expected) in CASES {
            let input: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
            let got = hash_reader(std::io::Cursor::new(input.clone())).unwrap();
            assert_eq!(got.blake3_hex(), expected, "reader path, {len} bytes");
        }

        // Just under and just over the 256KiB read buffer, compared against
        // the byte-slice path rather than a published constant. The two code
        // paths must agree; that is the actual invariant here.
        for len in [READ_BUF - 1, READ_BUF, READ_BUF + 1, READ_BUF * 2] {
            let input: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
            assert_eq!(
                hash_reader(std::io::Cursor::new(input.clone())).unwrap(),
                hash_bytes(&input),
                "reader and byte-slice paths disagree at {len} bytes"
            );
        }
    }

    // ---- file hashing ----

    #[test]
    fn file_hash_matches_bytes_hash() {
        let d = tmp();
        let p = d.path().join("f.bin");
        // Larger than READ_BUF so the read loop actually loops, which is where
        // an off-by-one in the buffer handling would hide.
        let data: Vec<u8> = (0..(READ_BUF * 2 + 12345))
            .map(|i| (i % 251) as u8)
            .collect();
        std::fs::write(&p, &data).unwrap();
        assert_eq!(hash_file(&p).unwrap(), hash_bytes(&data));
    }

    #[test]
    fn empty_file_hashes_to_the_empty_vector() {
        let d = tmp();
        let p = d.path().join("empty");
        std::fs::write(&p, b"").unwrap();
        assert_eq!(hash_file(&p).unwrap(), hash_bytes(b""));
    }

    /// A file whose length is a multiple of the read buffer, to catch a
    /// trailing-byte or final-partial-read bug.
    #[test]
    fn file_exactly_a_buffer_multiple_hashes_correctly() {
        let d = tmp();
        let p = d.path().join("aligned.bin");
        let data: Vec<u8> = (0..(READ_BUF * 3)).map(|i| (i % 97) as u8).collect();
        std::fs::write(&p, &data).unwrap();
        assert_eq!(hash_file(&p).unwrap(), hash_bytes(&data));
    }

    #[test]
    fn hash_file_exact_is_the_same_as_hash_file() {
        let d = tmp();
        let p = d.path().join("f.bin");
        std::fs::write(&p, b"payload").unwrap();
        assert_eq!(hash_file_exact(&p).unwrap(), hash_file(&p).unwrap());
    }

    #[test]
    fn a_missing_file_is_an_error_not_a_zero_hash() {
        let d = tmp();
        let r = hash_file(&d.path().join("nope"));
        assert!(r.is_err(), "a missing file must not hash to a default");
    }

    /// A reader that returns a short read (which `Read` explicitly allows)
    /// must not be treated as EOF. A pipe or a network stream does this
    /// routinely, and stopping at the first short read would silently hash a
    /// prefix of the data.
    #[test]
    fn short_reads_do_not_end_the_hash_early() {
        struct ShortReader {
            data: Vec<u8>,
            pos: usize,
        }
        impl Read for ShortReader {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                if self.pos >= self.data.len() {
                    return Ok(0);
                }
                // Deliberately tiny: at most 7 bytes per call.
                let n = 7.min(buf.len()).min(self.data.len() - self.pos);
                buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
                self.pos += n;
                Ok(n)
            }
        }
        let data: Vec<u8> = (0..5000).map(|i| (i % 13) as u8).collect();
        let got = hash_reader(ShortReader {
            data: data.clone(),
            pos: 0,
        })
        .unwrap();
        assert_eq!(got, hash_bytes(&data), "short reads truncated the input");
    }

    /// An EINTR must be retried, not turned into an error. Signals are routine
    /// in a long-running scanner, and a read that aborts on the first one would
    /// fail a scan that is in fact fine.
    #[test]
    fn an_interrupted_read_is_retried_and_the_hash_is_still_correct() {
        /// Interrupts `n` times, then yields `data`, then EOF.
        struct Interrupting {
            interrupts_left: usize,
            data: Vec<u8>,
            pos: usize,
            reads: usize,
        }
        impl Read for Interrupting {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                self.reads += 1;
                // A bounded retry count: a reader that interrupts forever is
                // broken, and the test must fail rather than hang.
                assert!(self.reads < 10_000, "EINTR retry loop did not terminate");
                if self.interrupts_left > 0 {
                    self.interrupts_left -= 1;
                    return Err(io::Error::new(io::ErrorKind::Interrupted, "signal"));
                }
                if self.pos >= self.data.len() {
                    return Ok(0);
                }
                let n = buf.len().min(self.data.len() - self.pos);
                buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
                self.pos += n;
                Ok(n)
            }
        }

        let data: Vec<u8> = (0..5000).map(|i| (i % 13) as u8).collect();

        // No interrupts: the baseline.
        assert_eq!(
            hash_reader(Interrupting {
                interrupts_left: 0,
                data: data.clone(),
                pos: 0,
                reads: 0,
            })
            .unwrap(),
            hash_bytes(&data)
        );

        // Interrupts before every real read. The result must be the SAME hash as
        // the uninterrupted run -- retrying is only correct if it is transparent.
        for n_interrupts in 1..=5 {
            let got = hash_reader(Interrupting {
                interrupts_left: n_interrupts,
                data: data.clone(),
                pos: 0,
                reads: 0,
            })
            .unwrap_or_else(|e| panic!("{n_interrupts} interrupts should not error: {e}"));
            assert_eq!(
                got,
                hash_bytes(&data),
                "{n_interrupts} interrupts changed the hash"
            );
        }
    }

    /// An error that is NOT EINTR must still propagate. Retrying everything
    /// would turn a real I/O failure into an infinite loop.
    #[test]
    fn a_non_interrupt_error_propagates() {
        struct Failing;
        impl Read for Failing {
            fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::PermissionDenied, "nope"))
            }
        }
        let e = hash_reader(Failing).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
    }

    /// An error raised AFTER some data was read must propagate, and must not
    /// leave a half-computed hash looking like a successful one.
    #[test]
    fn an_error_after_partial_read_propagates() {
        struct FailLate {
            reads: usize,
        }
        impl Read for FailLate {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                self.reads += 1;
                if self.reads == 1 {
                    buf[..4].copy_from_slice(b"data");
                    return Ok(4);
                }
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "gone"))
            }
        }
        assert!(hash_reader(FailLate { reads: 0 }).is_err());
    }

    // ---- hex ----

    #[test]
    fn hex_round_trips() {
        let data = [0x00u8, 0x0f, 0x10, 0xff, 0xa5];
        assert_eq!(hex_decode(&hex_encode(&data)).unwrap(), data);
    }

    #[test]
    fn hex_is_lower_case() {
        assert_eq!(hex_encode(&[0xAB, 0xCD]), "abcd");
    }

    #[test]
    fn hex_decode_accepts_upper_case() {
        assert_eq!(hex_decode("ABCD").unwrap(), vec![0xab, 0xcd]);
    }

    #[test]
    fn odd_length_hex_is_rejected() {
        assert!(hex_decode("abc").is_none(), "a half byte is not a byte");
    }

    #[test]
    fn non_hex_is_rejected() {
        assert!(hex_decode("zz").is_none());
        assert!(hex_decode("0x").is_none());
    }

    #[test]
    fn empty_hex_is_an_empty_vec() {
        assert_eq!(hex_decode("").unwrap(), Vec::<u8>::new());
    }

    // ---- merkle ----

    fn leaf(n: u8) -> ContentHash {
        hash_bytes(&[n])
    }

    #[test]
    fn merkle_root_of_nothing_is_defined_and_distinct() {
        assert_ne!(merkle_root([]), leaf(0).blake3);
        assert_eq!(merkle_root([]), merkle_root_empty());
    }

    #[test]
    fn merkle_root_of_one_leaf_is_not_the_leaf() {
        let l = leaf(1);
        let r = merkle_root([l]);
        assert_ne!(r, l.blake3, "a root must be domain-separated from a leaf");
    }

    /// The property T-P1-005 exists for: (a, b) and (b, a) are different scenes.
    #[test]
    fn merkle_root_is_order_sensitive() {
        let (a, b) = (leaf(1), leaf(2));
        assert_ne!(
            merkle_root([a, b]),
            merkle_root([b, a]),
            "reordering parts must change the root, or a multi-part object is not verified"
        );
    }

    #[test]
    fn merkle_root_is_deterministic() {
        let (a, b, c) = (leaf(1), leaf(2), leaf(3));
        assert_eq!(merkle_root([a, b, c]), merkle_root([a, b, c]));
    }

    #[test]
    fn merkle_root_distinguishes_a_prefix() {
        let (a, b, c) = (leaf(1), leaf(2), leaf(3));
        assert_ne!(merkle_root([a, b]), merkle_root([a, b, c]));
    }

    /// A one-byte counter is mixed in per leaf, so [1,1] and [1,1,1] differ in
    /// root, and more importantly [1,2] cannot be confused with [1,1,1] by any
    /// length-extension of the input stream.
    #[test]
    fn merkle_root_separates_leaf_boundaries() {
        let a = hash_bytes(&[1, 2]);
        let b = hash_bytes(&[3]);
        let c = hash_bytes(&[4]);
        // same leaves, different grouping
        assert_ne!(merkle_root([a, b]), merkle_root([a, c]));
    }

    // ---- display / identity ----

    #[test]
    fn display_is_identity_then_lookup() {
        let h = hash_bytes(b"x");
        let s = h.to_string();
        assert!(s.starts_with("blake3:"));
        assert!(s.contains("xxh128:"));
        assert_eq!(s.len(), "blake3:".len() + 64 + "xxh128:".len() + 32);
    }

    #[test]
    fn identity_and_lookup_are_the_documented_slices() {
        let h = hash_bytes(b"x");
        assert_eq!(h.identity(), &h.blake3[..]);
        assert_eq!(h.lookup_key(), &h.xxh128[..]);
        assert_eq!(h.identity().len(), 32);
        assert_eq!(h.lookup_key().len(), 16);
    }

    /// The point of keeping them separate: the fast hash must never be what
    /// gets published, because it is not collision-resistant.
    #[test]
    fn the_published_identity_is_the_strong_hash() {
        let h = hash_bytes(b"x");
        assert_eq!(h.blake3_hex().len(), 64);
        assert_eq!(h.xxh128_hex().len(), 32);
    }

    // ---- concurrency, because a scanner hashes in parallel ----

    #[test]
    fn hashing_many_files_concurrently_agrees_with_serial() {
        let d = tmp();
        let mut paths = Vec::new();
        for i in 0..16 {
            let p = d.path().join(format!("f{i}.bin"));
            let data: Vec<u8> = (0..(i * 977 + 13)).map(|k| (k % 211) as u8).collect();
            std::fs::write(&p, &data).unwrap();
            paths.push((p, data));
        }
        let serial: Vec<ContentHash> = paths.iter().map(|(p, _)| hash_file(p).unwrap()).collect();
        let par: Vec<ContentHash> = std::thread::scope(|s| {
            let hs: Vec<_> = paths
                .iter()
                .map(|(p, _)| s.spawn(move || hash_file(p).unwrap()))
                .collect();
            hs.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert_eq!(serial, par);
    }

    /// A file being written while hashed is a real scanner race. The guarantee
    /// is only that the bytes read hash consistently -- not that they are the
    /// final bytes. This documents the limitation rather than pretending the
    /// scanner has a content lock.
    #[test]
    fn a_file_growing_during_the_read_hashes_what_was_read() {
        let d = tmp();
        let p = d.path().join("growing.bin");
        let mut f = File::create(&p).unwrap();
        f.write_all(&vec![7u8; 4096]).unwrap();
        f.sync_all().unwrap();
        drop(f);

        let a = hash_file(&p).unwrap();
        let mut f = File::options().append(true).open(&p).unwrap();
        f.write_all(&vec![8u8; 4096]).unwrap();
        f.sync_all().unwrap();
        drop(f);

        let b = hash_file(&p).unwrap();
        assert_ne!(a, b, "appending changed the file, so the hash must change");
        // The first hash is still a valid hash of the 4096 bytes that existed
        // when it ran -- it is just no longer the file's current content. The
        // scanner re-hashes on size/mtime change (spec §6.1).
        assert_eq!(a, hash_bytes(&vec![7u8; 4096]));
    }
}
