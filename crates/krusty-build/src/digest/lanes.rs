//! SHA-256 of several equal-length messages at once.
//!
//! The rounds of one message are a serial dependency chain, so a single stream leaves most of a
//! core idle; the same round over eight independent messages is eight-wide data parallelism. Where
//! the processor has AVX2 (detected at run time) each state word is one 256-bit vector holding all
//! eight lanes; elsewhere the same lane-major (`[word][lane]`) rounds run as plain loops. Each
//! lane computes exactly the FIPS 180-4 digest of its message; the scalar implementation's
//! standard vectors pin the arithmetic, and this module's tests pin agreement with it.

use super::{IV, K};

/// How many messages one pass digests.
pub(super) const LANES: usize = 8;

type Words = [u32; LANES];

/// The SHA-256 digest of each message. All messages have the same length, a whole number of
/// 64-byte blocks, and there are at most [`LANES`] of them.
pub(super) fn digest_equal(messages: &[&[u8]]) -> Vec<[u8; 32]> {
    assert!(!messages.is_empty() && messages.len() <= LANES);
    let length = messages[0].len();
    assert!(length.is_multiple_of(64) && messages.iter().all(|message| message.len() == length));
    let mut lanes = [messages[0]; LANES];
    lanes[..messages.len()].copy_from_slice(messages);

    #[cfg(target_arch = "x86_64")]
    let state = if std::arch::is_x86_feature_detected!("avx2") {
        // SAFETY: the processor was just found to support AVX2.
        unsafe { digest_avx2(&lanes, length) }
    } else {
        digest_lanes(&lanes, length)
    };
    #[cfg(not(target_arch = "x86_64"))]
    let state = digest_lanes(&lanes, length);

    (0..messages.len())
        .map(|lane| {
            let mut out = [0u8; 32];
            for (word, value) in state.iter().enumerate() {
                out[word * 4..word * 4 + 4].copy_from_slice(&value[lane].to_be_bytes());
            }
            out
        })
        .collect()
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn digest_avx2(lanes: &[&[u8]; LANES], length: usize) -> [Words; 8] {
    use std::arch::x86_64::*;

    #[inline(always)]
    unsafe fn rotr<const R: i32, const L: i32>(x: __m256i) -> __m256i {
        _mm256_or_si256(_mm256_srli_epi32::<R>(x), _mm256_slli_epi32::<L>(x))
    }
    #[inline(always)]
    unsafe fn add(x: __m256i, y: __m256i) -> __m256i {
        _mm256_add_epi32(x, y)
    }
    #[inline(always)]
    unsafe fn xor3(x: __m256i, y: __m256i, z: __m256i) -> __m256i {
        _mm256_xor_si256(_mm256_xor_si256(x, y), z)
    }
    #[inline(always)]
    unsafe fn compress(state: &mut [__m256i; 8], w: &mut [__m256i; 64]) {
        for index in 16..64 {
            let w15 = w[index - 15];
            let w2 = w[index - 2];
            let s0 = xor3(
                rotr::<7, 25>(w15),
                rotr::<18, 14>(w15),
                _mm256_srli_epi32::<3>(w15),
            );
            let s1 = xor3(
                rotr::<17, 15>(w2),
                rotr::<19, 13>(w2),
                _mm256_srli_epi32::<10>(w2),
            );
            w[index] = add(add(w[index - 16], s0), add(w[index - 7], s1));
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
        for (round, word) in w.iter().enumerate() {
            let s1 = xor3(rotr::<6, 26>(e), rotr::<11, 21>(e), rotr::<25, 7>(e));
            let ch = _mm256_xor_si256(_mm256_and_si256(e, f), _mm256_andnot_si256(e, g));
            let temp1 = add(
                add(h, s1),
                add(ch, add(_mm256_set1_epi32(K[round] as i32), *word)),
            );
            let s0 = xor3(rotr::<2, 30>(a), rotr::<13, 19>(a), rotr::<22, 10>(a));
            let maj = _mm256_or_si256(
                _mm256_and_si256(a, b),
                _mm256_and_si256(c, _mm256_or_si256(a, b)),
            );
            h = g;
            g = f;
            f = e;
            e = add(d, temp1);
            d = c;
            c = b;
            b = a;
            a = add(temp1, add(s0, maj));
        }
        for (slot, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *slot = add(*slot, value);
        }
    }
    #[inline(always)]
    unsafe fn word(lanes: &[&[u8]; LANES], start: usize) -> __m256i {
        let at = |lane: usize| {
            let bytes = &lanes[lane][start..start + 4];
            u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as i32
        };
        _mm256_setr_epi32(at(0), at(1), at(2), at(3), at(4), at(5), at(6), at(7))
    }

    let mut state = IV.map(|word| _mm256_set1_epi32(word as i32));
    let mut w = [_mm256_setzero_si256(); 64];
    for offset in (0..length).step_by(64) {
        for (index, slot) in w.iter_mut().take(16).enumerate() {
            *slot = word(lanes, offset + index * 4);
        }
        compress(&mut state, &mut w);
    }
    for (index, slot) in w.iter_mut().take(16).enumerate() {
        *slot = _mm256_set1_epi32(padding_word(length, index) as i32);
    }
    compress(&mut state, &mut w);
    state.map(|vector| std::mem::transmute::<__m256i, Words>(vector))
}

/// Word `index` of the padding block that ends every lane's message of `length` bytes.
fn padding_word(length: usize, index: usize) -> u32 {
    match index {
        0 => 0x8000_0000,
        14 => ((length as u64).wrapping_mul(8) >> 32) as u32,
        15 => (length as u64).wrapping_mul(8) as u32,
        _ => 0,
    }
}

fn digest_lanes(lanes: &[&[u8]; LANES], length: usize) -> [Words; 8] {
    let mut state = IV.map(|word| [word; LANES]);
    let mut schedule = [[0u32; LANES]; 16];
    for offset in (0..length).step_by(64) {
        for (index, words) in schedule.iter_mut().enumerate() {
            let start = offset + index * 4;
            for (word, message) in words.iter_mut().zip(lanes) {
                *word = u32::from_be_bytes([
                    message[start],
                    message[start + 1],
                    message[start + 2],
                    message[start + 3],
                ]);
            }
        }
        compress(&mut state, &schedule);
    }
    // Equal lengths give every lane the same final padding block.
    for (index, words) in schedule.iter_mut().enumerate() {
        *words = [padding_word(length, index); LANES];
    }
    compress(&mut state, &schedule);
    state
}

#[inline(always)]
fn map(f: impl Fn(usize) -> u32) -> Words {
    std::array::from_fn(f)
}

#[inline(always)]
fn compress(state: &mut [Words; 8], block: &[Words; 16]) {
    let mut w = [[0u32; LANES]; 64];
    w[..16].copy_from_slice(block);
    for index in 16..64 {
        let (w15, w2, w16, w7) = (w[index - 15], w[index - 2], w[index - 16], w[index - 7]);
        w[index] = map(|l| {
            let s0 = w15[l].rotate_right(7) ^ w15[l].rotate_right(18) ^ (w15[l] >> 3);
            let s1 = w2[l].rotate_right(17) ^ w2[l].rotate_right(19) ^ (w2[l] >> 10);
            w16[l].wrapping_add(s0).wrapping_add(w7[l]).wrapping_add(s1)
        });
    }

    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
    for (round, words) in w.iter().enumerate() {
        let temp1 = map(|l| {
            let s1 = e[l].rotate_right(6) ^ e[l].rotate_right(11) ^ e[l].rotate_right(25);
            let ch = (e[l] & f[l]) ^ (!e[l] & g[l]);
            h[l].wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[round])
                .wrapping_add(words[l])
        });
        let temp2 = map(|l| {
            let s0 = a[l].rotate_right(2) ^ a[l].rotate_right(13) ^ a[l].rotate_right(22);
            let maj = (a[l] & b[l]) ^ (a[l] & c[l]) ^ (b[l] & c[l]);
            s0.wrapping_add(maj)
        });
        h = g;
        g = f;
        f = e;
        e = map(|l| d[l].wrapping_add(temp1[l]));
        d = c;
        c = b;
        b = a;
        a = map(|l| temp1[l].wrapping_add(temp2[l]));
    }
    for (slot, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
        *slot = map(|l| slot[l].wrapping_add(value[l]));
    }
}

#[cfg(test)]
mod tests {
    use super::{digest_equal, digest_lanes, LANES};
    use crate::digest::digest_bytes;

    /// Every lane agrees with the scalar digest, for a full and a partial set of messages.
    #[test]
    fn each_lane_is_the_messages_sha256() {
        let messages: Vec<Vec<u8>> = (0..LANES)
            .map(|lane| {
                (0..64 * 37)
                    .map(|index| (index * 31 + lane * 7 + index / 64) as u8)
                    .collect()
            })
            .collect();
        for count in [1, 3, LANES] {
            let slices: Vec<&[u8]> = messages[..count].iter().map(Vec::as_slice).collect();
            let digests = digest_equal(&slices);
            for (message, digest) in slices.iter().zip(digests) {
                assert_eq!(&digest, digest_bytes(message).as_bytes());
            }
        }
        // The portable rounds, which a processor with AVX2 never reaches through `digest_equal`.
        let lanes: [&[u8]; LANES] = std::array::from_fn(|lane| messages[lane].as_slice());
        let state = digest_lanes(&lanes, lanes[0].len());
        for (lane, message) in lanes.iter().enumerate() {
            let expected = digest_bytes(message);
            for (word, value) in state.iter().enumerate() {
                assert_eq!(
                    value[lane].to_be_bytes(),
                    expected.as_bytes()[word * 4..word * 4 + 4],
                    "lane {lane}"
                );
            }
        }
        assert_eq!(
            &digest_equal(&[&[]])[0],
            digest_bytes(&[]).as_bytes(),
            "the empty message"
        );
    }
}
