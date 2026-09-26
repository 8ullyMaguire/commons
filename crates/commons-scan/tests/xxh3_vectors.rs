//! The XXH3-128 test vectors from the reference implementation.
//!
//! Kept as its own file because it is data, not logic, and because it is the
//! one place in this crate that compares our output against something outside
//! it. Every other test in `hashing.rs` is a *self*-consistency check: it
//! would pass just as happily with a hash function that is merely
//! deterministic. Pinning to Cyan4973/xxHash's `tests/sanity_test_vectors.h`
//! (branch `dev`) is what makes `hash_xxh128` mean the same thing to a
//! Stash-compatible tool written in another language -- which is the only
//! reason to pick xxh3 at all.
//!
//! The reference builds its input with a byte-oriented LCG rather than
//! `0..n`, so the buffer is reproduced here exactly.

/// (len, expected hash) for seed 0, packed **low64 first**.
///
/// The upstream table writes `{ high64, low64 }` because that is the C struct's
/// field order. The Rust binding returns `low64 | (high64 << 64)`, so the
/// values here are the same bits in the opposite order. Writing them in the
/// upstream order is a mistake that reads as correct and fails on every
/// vector, which is why this note is here.
pub const XXH3_128_VECTORS: &[(usize, u128)] = &[
    (0, 0x99AA06D3014798D86001C324468D497Fu128),
    (1, 0xA6CD5E9392000F6AC44BDFF4074EECDBu128),
    (2, 0x76750C3C7BF956687A9978044CB8A8BBu128),
    (3, 0x20EFC49FF02422EA54247382A8D6B94Du128),
    (4, 0x970D585AC632BF8E2E7D8D6876A39FE9u128),
    (5, 0x62ED587687606B4E057C7ED2C01FA1D1u128),
    (6, 0x082AFE0B8162D12A3E7039BDDA43CFC6u128),
    (7, 0xDD9B6039F79EC416081C22DD284A2F0Au128),
    (8, 0x47A7F080D82BB45664C69CAB4BB21DC5u128),
    (15, 0xC402609E57EE5772958955DF1889E6BCu128),
    (16, 0xC68C368ECF8A9C05562980258A998629u128),
    (17, 0x955FA78643ED3669ABBC12D11973D7DBu128),
    (100, 0x9B50B05817AB158E5FCBC2E3295F2476u128),
    (101, 0xF96034BF004112585E9E9ED01FC1F1CFu128),
    (127, 0xDCFAE8002712DB1C802A565A8A79A999u128),
    (128, 0x39992220E045260AEBB15E34A7FB5AB1u128),
    (129, 0x03815FC91F1B30B686C9E3BC8F0A3B5Cu128),
    (239, 0xE59FC6554B5008BCF895E8B860B8A593u128),
    (240, 0xAA4202DAA2769DC85C9AAE94C8EBE5A0u128),
    (241, 0x99A80ECF0ECFC647C5A639ECD2030E5Eu128),
    (1000, 0x9B857ABF662E5A25ACA2DDE0F1951B9Au128),
    (4001, 0xEB813993C4D13A1940865A418C827014u128),
    (4160, 0x67140711C1E3E3354F323B15321E94E1u128),
];

/// The reference implementation's `fillTestBuffer`.
///
/// A byte stream from an LCG seeded with the 32-bit prime. Deliberately not
/// `0..n` and not a repeating pattern, so a hash implementation cannot pass
/// by special-casing a trivial input.
pub fn reference_buffer(n: usize) -> Vec<u8> {
    const PRIME32: u64 = 2_654_435_761;
    const PRIME64: u64 = 11_400_714_785_074_694_797;
    let mut out = Vec::with_capacity(n);
    let mut gen = PRIME32;
    for _ in 0..n {
        out.push((gen >> 56) as u8);
        gen = gen.wrapping_mul(PRIME64);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The binding's `xxh3_128` agrees with the reference on every vector.
    ///
    /// If this fails, the `hash_xxh128` column in a Commons database is not
    /// comparable with any other Stash-compatible build, and every
    /// already-indexed library needs rehashing. It is worth a test of its own
    /// for that reason.
    #[test]
    fn the_binding_matches_the_reference_xxh3_128() {
        for &(len, want) in XXH3_128_VECTORS {
            let buf = reference_buffer(len);
            let got = xxhash_rust::xxh3::xxh3_128(&buf);
            assert_eq!(got, want, "xxh3_128 of {} reference bytes", len);
        }
    }

    /// The vectors must actually be a good spread. A table of three near-
    /// identical lengths would let a broken implementation pass, and the
    /// lengths in XXH3 switch internal strategy at 16, 128, and 240 bytes --
    /// so a table that misses those boundaries tests almost nothing.
    #[test]
    fn the_vector_table_covers_the_algorithm_boundaries() {
        let lengths: Vec<usize> = XXH3_128_VECTORS.iter().map(|(n, _)| *n).collect();
        for boundary in [0, 16, 128, 240] {
            assert!(
                lengths.contains(&boundary),
                "no vector at the {boundary}-byte boundary, which is where XXH3 \
                 switches strategy"
            );
            assert!(
                lengths.contains(&(boundary + 1)),
                "no vector just past {boundary}, which is the other side of the switch"
            );
        }
        assert_eq!(lengths.len(), XXH3_128_VECTORS.len(), "lengths are unique");
    }
}
