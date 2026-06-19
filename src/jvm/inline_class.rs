//! Inline (`value`) class **name mangling** — kotlinc's scheme for functions whose signature mentions
//! an inline class.
//!
//! A function that takes an inline-class parameter (or, for certain members, returns one) gets a name
//! suffix `-<hash>`, where `<hash>` is the URL-safe-base64 (no padding) of the first 5 bytes of the MD5
//! of a *signature string*. This keeps overloads distinct after inline-class parameters are erased to
//! their underlying representation. The algorithm mirrors
//! `compiler/backend/.../inlineClassManglingUtils.kt` (new mangling rules, K2 / language 2.x):
//!
//!   * each value parameter contributes `L<fqName>[?];` when its type is an inline class, or the
//!     placeholder `_` otherwise;
//!   * a return type that must be mangled contributes `:` + that same element;
//!   * the elements are concatenated (no separator), MD5'd, and the first 5 bytes base64url-encoded.
//!
//! Verified against kotlinc 2.4.0: a getter returning `value class S` → signature `:LS;` → `-C-fiWsc`;
//! `fun useS(s: S)` → signature `LS;` → `-gSa4wCw`.

/// What a type contributes to a mangling signature: its FqName (slash-or-dot form as kotlinc prints
/// it, e.g. `S` or `foo.Bar`), whether it is an inline/`value` class, and whether it is nullable.
pub struct InfoForMangling {
    pub fq_name: String,
    pub is_value: bool,
    pub is_nullable: bool,
}

/// One signature element (new mangling rules): an inline class is spelled `L<fqName>[?];`, any other
/// type is the placeholder `_`.
fn signature_element(info: &InfoForMangling) -> String {
    if info.is_value {
        let mut s = String::with_capacity(info.fq_name.len() + 3);
        s.push('L');
        s.push_str(&info.fq_name);
        if info.is_nullable {
            s.push('?');
        }
        s.push(';');
        s
    } else {
        "_".to_string()
    }
}

/// The mangling **signature string** for a function with the given value parameters and (optional,
/// caller-decided) mangled return type, or `None` when no mangling applies (no inline-class parameter
/// and no return to mangle). `params` is every value parameter (an extension receiver, if any, comes
/// first); `ret` is `Some` only when the caller has determined the return type must be mangled.
pub fn mangling_signature(
    params: &[InfoForMangling],
    ret: Option<&InfoForMangling>,
) -> Option<String> {
    let requires_param_mangling = params.iter().any(|p| p.is_value);
    if !requires_param_mangling && ret.is_none() {
        return None;
    }
    let mut sig: String = params.iter().map(signature_element).collect();
    if let Some(r) = ret {
        sig.push(':');
        sig.push_str(&signature_element(r));
    }
    Some(sig)
}

/// The name suffix (`-<hash>`) for a mangling signature string: `"-"` + base64url(MD5(sig)[0..5]).
pub fn mangle_suffix(signature: &str) -> String {
    let digest = md5(signature.as_bytes());
    let mut out = String::with_capacity(8);
    out.push('-');
    out.push_str(&base64_url_nopad(&digest[0..5]));
    out
}

/// `base` mangled for the given signature, or `base` unchanged when no mangling applies.
pub fn mangled_name(
    base: &str,
    params: &[InfoForMangling],
    ret: Option<&InfoForMangling>,
) -> String {
    match mangling_signature(params, ret) {
        Some(sig) => format!("{base}{}", mangle_suffix(&sig)),
        None => base.to_string(),
    }
}

/// URL-safe base64 (alphabet `A–Z a–z 0–9 - _`) without padding — kotlinc uses
/// `Base64.getUrlEncoder().withoutPadding()`, whose characters are legal in JVM/Dalvik member names.
fn base64_url_nopad(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(n >> 18 & 0x3f) as usize] as char);
        out.push(ALPHABET[(n >> 12 & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[(n >> 6 & 0x3f) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(n & 0x3f) as usize] as char);
        }
    }
    out
}

/// MD5 digest (RFC 1321). Kotlinc mangling needs MD5 specifically; krusty carries this small pure
/// implementation rather than a crypto dependency.
fn md5(input: &[u8]) -> [u8; 16] {
    // Per-round left-rotate amounts.
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    // Binary integer parts of sines of integers (radians) — the K table.
    const K: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613,
        0xfd469501, 0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193,
        0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d,
        0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
        0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122,
        0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa,
        0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244,
        0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb,
        0xeb86d391,
    ];

    let (mut a0, mut b0, mut c0, mut d0) =
        (0x67452301u32, 0xefcdab89u32, 0x98badcfeu32, 0x10325476u32);

    // Pad: append 0x80, then zeros to length ≡ 56 (mod 64), then the 64-bit little-endian bit length.
    let mut msg = input.to_vec();
    let bit_len = (input.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_le_bytes());

    for block in msg.chunks_exact(64) {
        let mut m = [0u32; 16];
        for (i, w) in m.iter_mut().enumerate() {
            *w = u32::from_le_bytes([
                block[i * 4],
                block[i * 4 + 1],
                block[i * 4 + 2],
                block[i * 4 + 3],
            ]);
        }
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i {
                0..=15 => ((b & c) | (!b & d), i),
                16..=31 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                32..=47 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let tmp = d;
            d = c;
            c = b;
            let sum = a.wrapping_add(f).wrapping_add(K[i]).wrapping_add(m[g]);
            b = b.wrapping_add(sum.rotate_left(S[i]));
            a = tmp;
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }

    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&a0.to_le_bytes());
    out[4..8].copy_from_slice(&b0.to_le_bytes());
    out[8..12].copy_from_slice(&c0.to_le_bytes());
    out[12..16].copy_from_slice(&d0.to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    #[test]
    fn md5_known_vectors() {
        assert_eq!(hex(&md5(b"")), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(hex(&md5(b"abc")), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            hex(&md5(b"The quick brown fox jumps over the lazy dog")),
            "9e107d9d372bb6826bd81d3542a419d6"
        );
    }

    #[test]
    fn base64_url_matches_jdk() {
        // 5-byte inputs (what mangling uses) → 7 chars, URL-safe, no padding.
        assert_eq!(base64_url_nopad(&[0, 0, 0, 0, 0]), "AAAAAAA");
        assert_eq!(base64_url_nopad(&[0xff, 0xff, 0xff, 0xff, 0xff]), "______8");
    }

    #[test]
    fn mangle_suffix_matches_kotlinc() {
        // Verified against kotlinc 2.4.0 output for `value class S(val string: String)`:
        //   getter returning S → return-mangled, signature ":LS;"  → getS-C-fiWsc
        assert_eq!(mangle_suffix(":LS;"), "-C-fiWsc");
        //   `fun useS(s: S)`   → param-mangled,  signature "LS;"   → useS-gSa4wCw
        assert_eq!(mangle_suffix("LS;"), "-gSa4wCw");
    }

    #[test]
    fn signature_assembly() {
        let s = InfoForMangling {
            fq_name: "S".to_string(),
            is_value: true,
            is_nullable: false,
        };
        // A value parameter → "LS;".
        assert_eq!(
            mangling_signature(std::slice::from_ref(&s), None).as_deref(),
            Some("LS;")
        );
        // A return-only mangle → ":LS;".
        assert_eq!(mangling_signature(&[], Some(&s)).as_deref(), Some(":LS;"));
        // No inline class anywhere → no mangling.
        let i = InfoForMangling {
            fq_name: "kotlin/Int".to_string(),
            is_value: false,
            is_nullable: false,
        };
        assert_eq!(mangling_signature(std::slice::from_ref(&i), None), None);
        // End-to-end name.
        assert_eq!(
            mangled_name("useS", std::slice::from_ref(&s), None),
            "useS-gSa4wCw"
        );
        assert_eq!(mangled_name("getS", &[], Some(&s)), "getS-C-fiWsc");
    }
}
