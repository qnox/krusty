//! Content digests, and an injective encoding to feed them.
//!
//! Everything in this crate that protects against shipping a wrong binary — cache keys, file
//! identity, ABI fingerprints, artifact integrity — reduces to a digest. Two properties are
//! required of that reduction, and FNV-1a-over-a-formatted-string had neither.
//!
//! # 1. The digest must be collision-resistant
//!
//! A 64-bit non-cryptographic hash is trivially collided on purpose and meets birthday collisions
//! by accident at ~2^32 entries. For a value whose only job is to decide "are these two compilations
//! the same", that is the wrong tool: a collision is a stale cache hit, which is a wrong binary with
//! no diagnostic. SHA-256 is implemented here rather than taken as a dependency, in the same spirit
//! as the hand-written class-file writer and LRU — the algorithm is fully specified (FIPS 180-4),
//! it is ~100 lines, and it is checked against the standard vectors below.
//!
//! # 2. The encoding must be injective
//!
//! Hashing a rendered string is only sound if distinct inputs cannot render identically. The
//! previous `format!`-and-newline-join encoding was not injective: one compiler flag containing a
//! newline rendered exactly like two flags, and the same held for paths, environment values and
//! plugin options. Two genuinely different compilations could therefore share a key.
//!
//! [`Hasher::field`] fixes that by length-delimiting every value it absorbs: each field contributes
//! its tag length, tag bytes, value length and value bytes. No value's content can be mistaken for
//! a delimiter, because delimiters are lengths rather than characters.

use std::fmt;

/// A 256-bit content digest.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Digest([u8; 32]);

impl Digest {
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Parse the 64-character lowercase hex form produced by [`fmt::Display`].
    pub fn parse_hex(text: &str) -> Option<Self> {
        if text.len() != 64 {
            return None;
        }
        let mut bytes = [0u8; 32];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(text.get(index * 2..index * 2 + 2)?, 16).ok()?;
        }
        Some(Self(bytes))
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in &self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Digest({self})")
    }
}

/// Absorbs length-delimited fields and produces a [`Digest`].
#[derive(Clone, Default)]
pub struct Hasher {
    state: Sha256,
}

impl Hasher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Absorb one tagged field. The tag names what the value is, so two fields carrying the same
    /// bytes under different meanings cannot collide either.
    pub fn field(&mut self, tag: &str, value: &[u8]) -> &mut Self {
        self.state.update(&(tag.len() as u64).to_le_bytes());
        self.state.update(tag.as_bytes());
        self.state.update(&(value.len() as u64).to_le_bytes());
        self.state.update(value);
        self
    }

    pub fn text(&mut self, tag: &str, value: &str) -> &mut Self {
        self.field(tag, value.as_bytes())
    }

    /// Absorb a count, so a list's length is part of its identity rather than implied by its
    /// elements. Without this, one list of two items and two adjacent lists of one could agree.
    pub fn count(&mut self, tag: &str, value: usize) -> &mut Self {
        self.field(tag, &(value as u64).to_le_bytes())
    }

    /// Absorb another digest as a field — how a dependency's ABI enters a dependent's key.
    pub fn nested(&mut self, tag: &str, value: Digest) -> &mut Self {
        self.field(tag, value.as_bytes())
    }

    pub fn finish(&self) -> Digest {
        Digest(self.state.clone().finalize())
    }
}

/// Convenience: digest a byte slice directly.
pub fn digest_bytes(bytes: &[u8]) -> Digest {
    let mut state = Sha256::default();
    state.update(bytes);
    Digest(state.finalize())
}

// ---------------------------------------------------------------------------------------------
// SHA-256 (FIPS 180-4). Straightforward reference implementation; correctness is pinned by the
// standard test vectors in this module's tests.
// ---------------------------------------------------------------------------------------------

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

#[derive(Clone)]
struct Sha256 {
    state: [u32; 8],
    buffer: [u8; 64],
    buffered: usize,
    length: u64,
}

impl Default for Sha256 {
    fn default() -> Self {
        Self {
            state: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
                0x5be0cd19,
            ],
            buffer: [0; 64],
            buffered: 0,
            length: 0,
        }
    }
}

impl Sha256 {
    fn update(&mut self, mut data: &[u8]) {
        self.length = self.length.wrapping_add(data.len() as u64);
        if self.buffered > 0 {
            let take = (64 - self.buffered).min(data.len());
            self.buffer[self.buffered..self.buffered + take].copy_from_slice(&data[..take]);
            self.buffered += take;
            data = &data[take..];
            if self.buffered == 64 {
                let block = self.buffer;
                self.compress(&block);
                self.buffered = 0;
            }
        }
        while data.len() >= 64 {
            let (block, rest) = data.split_at(64);
            let mut fixed = [0u8; 64];
            fixed.copy_from_slice(block);
            self.compress(&fixed);
            data = rest;
        }
        if !data.is_empty() {
            self.buffer[..data.len()].copy_from_slice(data);
            self.buffered = data.len();
        }
    }

    fn finalize(mut self) -> [u8; 32] {
        let bit_length = self.length.wrapping_mul(8);
        self.update(&[0x80]);
        // `update` advanced `length`; padding length is derived from the buffered count instead.
        while self.buffered != 56 {
            self.update(&[0x00]);
        }
        let mut block = self.buffer;
        block[56..].copy_from_slice(&bit_length.to_be_bytes());
        self.compress(&block);

        let mut out = [0u8; 32];
        for (index, word) in self.state.iter().enumerate() {
            out[index * 4..index * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        out
    }

    fn compress(&mut self, block: &[u8; 64]) {
        let mut w = [0u32; 64];
        for (index, word) in w.iter_mut().enumerate().take(16) {
            let start = index * 4;
            *word = u32::from_be_bytes([
                block[start],
                block[start + 1],
                block[start + 2],
                block[start + 3],
            ]);
        }
        for index in 16..64 {
            let s0 = w[index - 15].rotate_right(7)
                ^ w[index - 15].rotate_right(18)
                ^ (w[index - 15] >> 3);
            let s1 = w[index - 2].rotate_right(17)
                ^ w[index - 2].rotate_right(19)
                ^ (w[index - 2] >> 10);
            w[index] = w[index - 16]
                .wrapping_add(s0)
                .wrapping_add(w[index - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;
        for index in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[index])
                .wrapping_add(w[index]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        for (slot, value) in self.state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *slot = slot.wrapping_add(value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FIPS 180-4 / NIST standard vectors. These pin the implementation; if it is ever replaced by
    /// a dependency, these keep the replacement honest.
    #[test]
    fn matches_the_standard_sha256_vectors() {
        for (input, expected) in [
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
        ] {
            assert_eq!(
                digest_bytes(input.as_bytes()).to_string(),
                expected,
                "SHA-256 of {input:?}"
            );
        }
    }

    /// A multi-block message exercises the buffering path across `update` boundaries.
    #[test]
    fn a_long_message_hashes_across_block_boundaries() {
        let million_a = vec![b'a'; 1_000_000];
        assert_eq!(
            digest_bytes(&million_a).to_string(),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    #[test]
    fn chunked_updates_agree_with_one_shot() {
        let data: Vec<u8> = (0..500u32).map(|n| (n % 251) as u8).collect();
        let one_shot = digest_bytes(&data);
        let mut chunked = Sha256::default();
        for chunk in data.chunks(7) {
            chunked.update(chunk);
        }
        assert_eq!(Digest(chunked.finalize()), one_shot);
    }

    #[test]
    fn hex_round_trips() {
        let digest = digest_bytes(b"round trip");
        assert_eq!(Digest::parse_hex(&digest.to_string()), Some(digest));
        assert_eq!(Digest::parse_hex("short"), None);
        assert_eq!(Digest::parse_hex(&"z".repeat(64)), None);
    }

    /// The property the previous encoding lacked: a value containing the old delimiter cannot
    /// impersonate two values.
    #[test]
    fn field_encoding_is_injective_across_delimiters() {
        let mut one_flag = Hasher::new();
        one_flag.count("flags", 1).text("flag", "-a\n-b");

        let mut two_flags = Hasher::new();
        two_flags
            .count("flags", 2)
            .text("flag", "-a")
            .text("flag", "-b");

        assert_ne!(
            one_flag.finish(),
            two_flags.finish(),
            "one flag containing a newline must not hash like two flags"
        );
    }

    #[test]
    fn field_tags_separate_identical_values() {
        let mut as_source = Hasher::new();
        as_source.text("source", "same-bytes");
        let mut as_classpath = Hasher::new();
        as_classpath.text("classpath", "same-bytes");
        assert_ne!(
            as_source.finish(),
            as_classpath.finish(),
            "the same bytes under different meanings must not collide"
        );
    }

    #[test]
    fn list_length_participates() {
        let mut empty = Hasher::new();
        empty.count("items", 0);
        let mut one_empty_item = Hasher::new();
        one_empty_item.count("items", 1).text("item", "");
        assert_ne!(empty.finish(), one_empty_item.finish());
    }

    #[test]
    fn identical_field_sequences_agree() {
        let build = || {
            let mut h = Hasher::new();
            h.text("compiler", "krusty-1").count("sources", 2);
            h.text("source", "a.kt").text("source", "b.kt");
            h.finish()
        };
        assert_eq!(build(), build());
    }
}
