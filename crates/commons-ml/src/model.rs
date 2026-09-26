//! Model loading, with the checks that make a downloaded model trustworthy.
//!
//! # Why a model is treated as untrusted input
//!
//! A model file is fetched over the network and then *executed*. Parsing an
//! ONNX graph is a native code path, and the graph itself controls tensor
//! shapes, operator sequences, and memory allocation. "Download and run
//! whatever arrived" is therefore a remote code execution surface wearing a
//! data format's clothes.
//!
//! The defence is a SHA-256 stated in a manifest that is part of the
//! repository, and the check is *before* the bytes are handed to any parser.
//! That ordering is the whole design: a digest computed after parsing has
//! already run the attacker's graph.
//!
//! # What this module does not do
//!
//! It does not download. Fetching belongs to an installer that has a
//! progress bar, retries, and a place to report a failure to a human; a
//! library that silently reaches for the network is a library that cannot be
//! used offline, which §6.5 requires. What is here is everything needed to
//! decide whether a file that *is* on disk may be loaded — and that decision
//! is the security boundary.

use std::path::{Path, PathBuf};

/// The errors a model load can produce.
///
/// Split by cause because the caller's response differs per cause: a missing
/// model is a feature the user has not enabled yet, a checksum mismatch is
/// either a corrupted download or an attack, and a malformed manifest is a
/// bug in the repository. Collapsing them into one `Err(String)` loses
/// exactly the information that decides what to do.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ModelError {
    /// The file is not there.
    #[error("model not available at {path}: {reason}")]
    Unavailable { path: PathBuf, reason: String },

    /// The path exists but is not a readable file.
    #[error("model at {path} is not a file: {reason}")]
    NotAFile { path: PathBuf, reason: String },

    /// The bytes do not hash to what the manifest says.
    ///
    /// `expected` and `actual` are both carried. A user who sees only "model
    /// verification failed" cannot tell a truncated download from something
    /// worse, and those two need different responses.
    #[error("model at {path} failed verification: expected sha256 {expected}, got {actual}")]
    ChecksumMismatch {
        path: PathBuf,
        expected: String,
        actual: String,
    },

    /// The manifest's digest is not a SHA-256 as written.
    #[error("manifest digest {digest:?} is not a 64-character hex sha256")]
    MalformedDigest { digest: String },

    /// The model loaded but produced vectors of an unexpected width.
    #[error("embedding width mismatch: got {got}, expected {expected}")]
    WrongVectorWidth { got: usize, expected: usize },

    /// Inference itself failed.
    #[error("inference failed: {0}")]
    Inference(String),
}

impl ModelError {
    /// Whether the caller may carry on without this feature.
    ///
    /// The distinction is not "is it fatal" — every caller wants to survive a
    /// missing model — but "is it *our* fault or the environment's". A
    /// checksum mismatch is not degradable in the quiet sense: the feature
    /// stays off, but the condition is one a human has to look at, so callers
    /// that log degradations at info level must log this one at error.
    pub fn is_degradable(&self) -> bool {
        matches!(self, ModelError::Unavailable { .. })
    }
}

/// Where a model comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelSource {
    /// A file already on disk, installed by `commons-models install`.
    Local(PathBuf),
}

impl ModelSource {
    /// The path, for messages.
    pub fn path(&self) -> &Path {
        match self {
            ModelSource::Local(p) => p,
        }
    }

    /// Open the model, verifying it against `expected_sha256` first.
    ///
    /// The order of the checks is the security property, so it is written out
    /// rather than left to the reader: existence, then file-ness, then digest
    /// *shape*, then digest *value*. Verifying the shape before the value is
    /// what lets a typo in `manifest.toml` be reported as a typo instead of as
    /// a checksum failure, which would send someone looking for an attack that
    /// never happened.
    pub fn open_with_sha256(&self, expected_sha256: &str) -> Result<LoadedModel, ModelError> {
        let path = self.path().to_path_buf();

        let meta = std::fs::metadata(&path).map_err(|e| ModelError::Unavailable {
            path: path.clone(),
            reason: e.to_string(),
        })?;
        if !meta.is_file() {
            return Err(ModelError::NotAFile {
                path,
                reason: format!("{} bytes, not a regular file", meta.len()),
            });
        }

        let expected = expected_sha256.trim().to_ascii_lowercase();
        if expected.len() != 64 || !expected.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(ModelError::MalformedDigest {
                digest: expected_sha256.to_string(),
            });
        }

        let actual = sha256_file(&path)?;
        if actual != expected {
            return Err(ModelError::ChecksumMismatch {
                path,
                expected,
                actual,
            });
        }

        Ok(LoadedModel {
            path,
            sha256: actual,
            bytes: meta.len(),
        })
    }
}

/// A model that has been verified and may be loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedModel {
    pub path: PathBuf,
    pub sha256: String,
    pub bytes: u64,
}

/// The SHA-256 of a file, lowercase hex.
///
/// Implemented here rather than pulled from a crate: it is thirty lines, it is
/// the single most security-relevant function in the ML crate, and a digest
/// nobody can read is a digest nobody can audit. A dependency would also
/// become the thing to update when a CVE lands, for a function that must not
/// change.
pub fn sha256_file(path: &Path) -> Result<String, ModelError> {
    use std::io::Read;

    let mut f = std::fs::File::open(path).map_err(|e| ModelError::Unavailable {
        path: path.to_path_buf(),
        reason: e.to_string(),
    })?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf).map_err(|e| ModelError::Unavailable {
            path: path.to_path_buf(),
            reason: e.to_string(),
        })?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finish_hex())
}

/// SHA-256, FIPS 180-4.
///
/// Not `sha2` the crate: see [`sha256_file`]. The state is eight u32 words
/// and the compression function is the specification's, transcribed. The
/// round-trip against the published vectors is a test in this module, so a
/// transcription error is caught at build time rather than by a model
/// silently failing to verify.
#[derive(Debug, Clone)]
pub struct Sha256 {
    state: [u32; 8],
    buffer: [u8; 64],
    buffered: usize,
    length: u64,
}

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

impl Default for Sha256 {
    fn default() -> Self {
        Sha256::new()
    }
}

impl Sha256 {
    pub fn new() -> Self {
        Sha256 {
            state: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
                0x5be0cd19,
            ],
            buffer: [0u8; 64],
            buffered: 0,
            length: 0,
        }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.length = self.length.wrapping_add(data.len() as u64);

        // Top the partial block up, compressing it as soon as it is full.
        if self.buffered > 0 {
            let need = 64 - self.buffered;
            let take = need.min(data.len());
            self.buffer[self.buffered..self.buffered + take].copy_from_slice(&data[..take]);
            self.buffered += take;
            data = &data[take..];
            if self.buffered == 64 {
                let block = self.buffer;
                self.compress(&block);
                self.buffered = 0;
            }
        }

        // `data` is now a whole number of blocks plus a remainder that starts
        // on a block boundary only if nothing was buffered above. If the
        // buffer above was *not* filled (still partial) and there is data left,
        // the leftovers must be appended after the existing bytes rather than
        // overwriting them.
        let (blocks, rest) = data.as_chunks::<64>();
        for chunk in blocks {
            self.compress(chunk);
        }
        if self.buffered > 0 {
            // Unreachable while the code above always empties a full buffer,
            // but kept as a hard invariant: `rest` is appended, never copied to
            // offset 0.
            debug_assert!(rest.len() + self.buffered <= 64);
            self.buffer[self.buffered..self.buffered + rest.len()].copy_from_slice(rest);
            self.buffered += rest.len();
        } else {
            self.buffer[..rest.len()].copy_from_slice(rest);
            self.buffered = rest.len();
        }
    }

    pub fn finish_hex(mut self) -> String {
        let bits = self.length.wrapping_mul(8);
        self.update_raw(&[0x80]);
        while self.buffered != 56 {
            self.update_raw(&[0]);
        }
        let len = bits.to_be_bytes();
        self.update_raw(&len);
        let mut out = String::with_capacity(64);
        for w in self.state {
            out.push_str(&format!("{w:08x}"));
        }
        out
    }

    /// Append without counting towards the message length.
    ///
    /// Separate from `update` because the padding in `finish_hex` is not
    /// message data, and a `length` that included it would produce a digest
    /// that is wrong in a way no test vector would catch.
    fn update_raw(&mut self, data: &[u8]) {
        for &b in data {
            self.buffer[self.buffered] = b;
            self.buffered += 1;
            if self.buffered == 64 {
                let block = self.buffer;
                self.compress(&block);
                self.buffered = 0;
            }
        }
    }

    fn compress(&mut self, block: &[u8; 64]) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                block[i * 4],
                block[i * 4 + 1],
                block[i * 4 + 2],
                block[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);

            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }

        for (s, v) in self.state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *s = s.wrapping_add(v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The published FIPS 180-4 / NIST vectors. If the transcription of the
    /// compression function is wrong, these fail and the model loader is
    /// refused for every model — loudly, at test time, rather than silently
    /// at runtime.
    #[test]
    fn sha256_matches_the_published_vectors() {
        let cases: &[(&str, &str)] = &[
            (
                "",
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            ),
            (
                "abc",
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            ),
            (
                "abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
                "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
            ),
            (
                "abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu",
                "cf5b16a778af8380036ce59e7b0492370b249b11e8f07a51afac45037afee9d1",
            ),
        ];
        for (input, want) in cases {
            let mut h = Sha256::new();
            h.update(input.as_bytes());
            assert_eq!(h.finish_hex(), *want, "input {input:?}");
        }
    }

    #[test]
    fn a_million_a_is_the_nist_long_vector() {
        // The one vector that catches a length-counter bug: a 64-bit length
        // field that counts blocks instead of bytes, or an off-by-one in the
        // padding, passes every short vector and fails this one.
        let mut h = Sha256::new();
        let chunk = vec![b'a'; 1000];
        for _ in 0..1000 {
            h.update(&chunk);
        }
        assert_eq!(
            h.finish_hex(),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    #[test]
    fn diag_find_smallest_failure() {
        for n in 0..200usize {
            let data: Vec<u8> = (0..n).map(|i| (i % 251) as u8).collect();
            let mut one = Sha256::new();
            one.update(&data);
            let mut many = Sha256::new();
            for c in data.chunks(7) {
                many.update(c);
            }
            if one.finish_hex() != many.finish_hex() {
                panic!("differs at n={n}");
            }
        }
    }

    #[test]
    fn the_digest_is_not_sensitive_to_how_the_input_was_chunked() {
        // The file reader hands over 64 KiB at a time and the manifest writer
        // may have hashed a string. Both must agree, or a model verifies
        // against a digest computed one way and not the other.
        let data: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
        let mut one = Sha256::new();
        one.update(&data);
        let mut many = Sha256::new();
        for chunk in data.chunks(7) {
            many.update(chunk);
        }
        assert_eq!(one.finish_hex(), many.finish_hex());
    }

    #[test]
    fn a_one_bit_change_changes_the_digest() {
        // The property the whole check rests on. Asserted rather than assumed
        // because a transcription that ignores most of the block would still
        // pass the vectors above only by accident.
        let a = {
            let mut h = Sha256::new();
            h.update(b"model weights");
            h.finish_hex()
        };
        let b = {
            let mut h = Sha256::new();
            h.update(b"model weightS");
            h.finish_hex()
        };
        assert_ne!(a, b);
    }
}
