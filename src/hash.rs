//! Order-sensitive 64-bit structural hashing.
//!
//! Hand-rolled rather than `std::hash` because the hash is a value that is
//! combined bottom-up across the tree, and it must be stable across runs so
//! group fingerprints can be passed between CLI invocations.

const K: u64 = 0x9E37_79B9_7F4A_7C15;

/// splitmix64 finaliser: spreads entropy so sequential kind ids do not
/// produce correlated hashes.
fn mix(mut x: u64) -> u64 {
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

pub fn combine(h: u64, x: u64) -> u64 {
    mix(h.rotate_left(23) ^ x.wrapping_add(K))
}

pub fn seed(lang_seed: u64, kind: u64) -> u64 {
    combine(lang_seed, kind)
}

/// FNV-1a: tokens are short, so a simple byte loop beats anything fancier.
pub fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xCBF2_9CE4_8422_2325;
    for &b in bytes {
        h = (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01B3);
    }
    mix(h)
}
