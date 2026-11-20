//! Fast hash collections for compiler-owned indexes.
//!
//! Most compiler keys ultimately contain source- or dependency-provided spellings. Those maps use
//! AHash's per-process random keys: lookup stays cheap without making chosen collision sets
//! precomputable. A few tightly scoped stores over interned or numeric identities use the exposed
//! Fx hasher directly when stable hashing is part of their implementation.

/// FxHash (rustc's non-cryptographic hasher), kept local to avoid another crate dependency.
#[derive(Default, Clone)]
pub struct FxHasher {
    hash: u64,
}

impl FxHasher {
    #[inline]
    fn add(&mut self, word: u64) {
        self.hash = (self.hash.rotate_left(5) ^ word).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }
}

impl std::hash::Hasher for FxHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let mut rest = bytes;
        while rest.len() >= 8 {
            self.add(u64::from_le_bytes(rest[..8].try_into().unwrap()));
            rest = &rest[8..];
        }
        if rest.len() >= 4 {
            self.add(u64::from(u32::from_le_bytes(rest[..4].try_into().unwrap())));
            rest = &rest[4..];
        }
        for &byte in rest {
            self.add(u64::from(byte));
        }
    }

    #[inline]
    fn write_u8(&mut self, value: u8) {
        self.add(u64::from(value));
    }

    #[inline]
    fn write_u32(&mut self, value: u32) {
        self.add(u64::from(value));
    }

    #[inline]
    fn write_u64(&mut self, value: u64) {
        self.add(value);
    }

    #[inline]
    fn write_usize(&mut self, value: usize) {
        self.add(value as u64);
    }

    #[inline]
    fn finish(&self) -> u64 {
        self.hash
    }
}

pub type FxBuildHasher = std::hash::BuildHasherDefault<FxHasher>;
pub type HashMap<K, V> = std::collections::HashMap<K, V, ahash::RandomState>;
pub type HashSet<T> = std::collections::HashSet<T, ahash::RandomState>;
