//! CityHash64 (version 1.1), the hash Kotlin applies to a mangled declaration signature to form the
//! member id of a public `IdSignature`.

const K0: u64 = 0xc3a5_c85c_97cb_3127;
const K1: u64 = 0xb492_b66f_be98_f273;
const K2: u64 = 0x9ae1_6a3b_2f90_404f;
const K_MUL: u64 = 0x9ddf_ea08_eb38_2d69;

fn fetch64(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().expect("eight bytes"))
}

fn fetch32(bytes: &[u8], at: usize) -> u64 {
    u64::from(u32::from_le_bytes(
        bytes[at..at + 4].try_into().expect("four bytes"),
    ))
}

fn shift_mix(value: u64) -> u64 {
    value ^ (value >> 47)
}

fn hash_len16_mul(u: u64, v: u64, mul: u64) -> u64 {
    let a = shift_mix((u ^ v).wrapping_mul(mul));
    let b = shift_mix((v ^ a).wrapping_mul(mul));
    b.wrapping_mul(mul)
}

fn hash_len16(u: u64, v: u64) -> u64 {
    hash_len16_mul(u, v, K_MUL)
}

fn hash_len0_to16(bytes: &[u8]) -> u64 {
    let len = bytes.len();
    if len >= 8 {
        let mul = K2.wrapping_add(len as u64 * 2);
        let a = fetch64(bytes, 0).wrapping_add(K2);
        let b = fetch64(bytes, len - 8);
        let c = b.rotate_right(37).wrapping_mul(mul).wrapping_add(a);
        let d = a.rotate_right(25).wrapping_add(b).wrapping_mul(mul);
        return hash_len16_mul(c, d, mul);
    }
    if len >= 4 {
        let mul = K2.wrapping_add(len as u64 * 2);
        let a = fetch32(bytes, 0);
        return hash_len16_mul(len as u64 + (a << 3), fetch32(bytes, len - 4), mul);
    }
    if len > 0 {
        let a = u32::from(bytes[0]);
        let b = u32::from(bytes[len >> 1]);
        let c = u32::from(bytes[len - 1]);
        let y = a.wrapping_add(b << 8);
        let z = len as u32 + (c << 2);
        return shift_mix(u64::from(y).wrapping_mul(K2) ^ u64::from(z).wrapping_mul(K0))
            .wrapping_mul(K2);
    }
    K2
}

fn hash_len17_to32(bytes: &[u8]) -> u64 {
    let len = bytes.len();
    let mul = K2.wrapping_add(len as u64 * 2);
    let a = fetch64(bytes, 0).wrapping_mul(K1);
    let b = fetch64(bytes, 8);
    let c = fetch64(bytes, len - 8).wrapping_mul(mul);
    let d = fetch64(bytes, len - 16).wrapping_mul(K2);
    hash_len16_mul(
        a.wrapping_add(b)
            .rotate_right(43)
            .wrapping_add(c.rotate_right(30))
            .wrapping_add(d),
        a.wrapping_add(b.wrapping_add(K2).rotate_right(18))
            .wrapping_add(c),
        mul,
    )
}

fn hash_len33_to64(bytes: &[u8]) -> u64 {
    let len = bytes.len();
    let mul = K2.wrapping_add(len as u64 * 2);
    let a = fetch64(bytes, 0).wrapping_mul(K2);
    let b = fetch64(bytes, 8);
    let c = fetch64(bytes, len - 24);
    let d = fetch64(bytes, len - 32);
    let e = fetch64(bytes, 16).wrapping_mul(K2);
    let f = fetch64(bytes, 24).wrapping_mul(9);
    let g = fetch64(bytes, len - 8);
    let h = fetch64(bytes, len - 16).wrapping_mul(mul);
    let u = a
        .wrapping_add(g)
        .rotate_right(43)
        .wrapping_add(b.rotate_right(30).wrapping_add(c).wrapping_mul(9));
    let v = (a.wrapping_add(g) ^ d).wrapping_add(f).wrapping_add(1);
    let w = u
        .wrapping_add(v)
        .wrapping_mul(mul)
        .swap_bytes()
        .wrapping_add(h);
    let x = e.wrapping_add(f).rotate_right(42).wrapping_add(c);
    let y = v
        .wrapping_add(w)
        .wrapping_mul(mul)
        .swap_bytes()
        .wrapping_add(g)
        .wrapping_mul(mul);
    let z = e.wrapping_add(f).wrapping_add(c);
    let a = x
        .wrapping_add(z)
        .wrapping_mul(mul)
        .wrapping_add(y)
        .swap_bytes()
        .wrapping_add(b);
    let b = shift_mix(
        z.wrapping_add(a)
            .wrapping_mul(mul)
            .wrapping_add(d)
            .wrapping_add(h),
    )
    .wrapping_mul(mul);
    b.wrapping_add(x)
}

fn weak_hash_len32_with_seeds(bytes: &[u8], at: usize, a: u64, b: u64) -> (u64, u64) {
    let w = fetch64(bytes, at);
    let x = fetch64(bytes, at + 8);
    let y = fetch64(bytes, at + 16);
    let z = fetch64(bytes, at + 24);
    let a = a.wrapping_add(w);
    let b = b.wrapping_add(a).wrapping_add(z).rotate_right(21);
    let c = a;
    let a = a.wrapping_add(x).wrapping_add(y);
    let b = b.wrapping_add(a.rotate_right(44));
    (a.wrapping_add(z), b.wrapping_add(c))
}

pub(super) fn city_hash64(bytes: &[u8]) -> u64 {
    let len = bytes.len();
    if len <= 16 {
        return hash_len0_to16(bytes);
    }
    if len <= 32 {
        return hash_len17_to32(bytes);
    }
    if len <= 64 {
        return hash_len33_to64(bytes);
    }
    let mut x = fetch64(bytes, len - 40);
    let mut y = fetch64(bytes, len - 16).wrapping_add(fetch64(bytes, len - 56));
    let mut z = hash_len16(
        fetch64(bytes, len - 48).wrapping_add(len as u64),
        fetch64(bytes, len - 24),
    );
    let mut v = weak_hash_len32_with_seeds(bytes, len - 64, len as u64, z);
    let mut w = weak_hash_len32_with_seeds(bytes, len - 32, y.wrapping_add(K1), x);
    x = x.wrapping_mul(K1).wrapping_add(fetch64(bytes, 0));
    let mut at = 0;
    let mut remaining = (len - 1) & !63;
    loop {
        x = x
            .wrapping_add(y)
            .wrapping_add(v.0)
            .wrapping_add(fetch64(bytes, at + 8))
            .rotate_right(37)
            .wrapping_mul(K1);
        y = y
            .wrapping_add(v.1)
            .wrapping_add(fetch64(bytes, at + 48))
            .rotate_right(42)
            .wrapping_mul(K1);
        x ^= w.1;
        y = y.wrapping_add(v.0).wrapping_add(fetch64(bytes, at + 40));
        z = z.wrapping_add(w.0).rotate_right(33).wrapping_mul(K1);
        v = weak_hash_len32_with_seeds(bytes, at, v.1.wrapping_mul(K1), x.wrapping_add(w.0));
        w = weak_hash_len32_with_seeds(
            bytes,
            at + 32,
            z.wrapping_add(w.1),
            y.wrapping_add(fetch64(bytes, at + 16)),
        );
        std::mem::swap(&mut z, &mut x);
        at += 64;
        remaining -= 64;
        if remaining == 0 {
            break;
        }
    }
    hash_len16(
        hash_len16(v.0, w.0)
            .wrapping_add(shift_mix(y).wrapping_mul(K1))
            .wrapping_add(z),
        hash_len16(v.1, w.1).wrapping_add(x),
    )
}

#[cfg(test)]
mod tests {
    use super::city_hash64;

    fn alphabet(len: usize) -> String {
        (0..len)
            .map(|i| char::from(b'a' + (i % 26) as u8))
            .collect()
    }

    /// Reference values from Google's CityHash 1.1 `CityHash64`, one per length class.
    #[test]
    fn every_length_class_matches_the_reference() {
        let cases: [(String, u64); 14] = [
            (String::new(), 0x9ae1_6a3b_2f90_404f),
            ("a".into(), 0xb345_4265_b6df_75e3),
            ("ab".into(), 0xaa8d_6e52_42ad_a51e),
            ("abc".into(), 0x24a5_b3a0_74e7_f369),
            ("abcd".into(), 0x1a55_02de_4a1f_8101),
            ("kotlin.Int".into(), 0x4c13_3f3d_8ca0_97f0),
            ("println(){}".into(), 0x0cbd_247a_db87_3be1),
            ("0123456789abcdef".into(), 0x54b9_61e5_dc83_4067),
            ("0123456789abcdefg".into(), 0xa6dd_ff87_a449_d24a),
            (
                "maxOf(0:0;0:0){0§<kotlin.Comparable<0:0>>}".into(),
                0xba31_5fe0_bd1b_9796,
            ),
            ("x".repeat(33), 0x3efc_e6b6_a0cf_e809),
            ("y".repeat(64), 0x955f_8ade_2aa9_ae6b),
            ("z".repeat(65), 0xfdf0_5150_ba8c_92ab),
            (alphabet(200), 0x0145_b90b_9ab0_6ce7),
        ];
        for (input, expected) in cases {
            assert_eq!(
                city_hash64(input.as_bytes()),
                expected,
                "{} bytes",
                input.len()
            );
        }
    }
}
