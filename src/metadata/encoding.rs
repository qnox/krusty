//! `@Metadata.d1` encoding and JVM modified-UTF-8.
//!
//! Kotlin's default (`BitEncoding`, `FORCE_8TO7_ENCODING=false`) packs the protobuf bytes into
//! `String[]` by mapping each byte to the char with the same value (byte `0x30` → `'0'`, confirmed
//! against a real kotlinc-emitted class). Strings are chunked so each fits the class-file Utf8
//! limit (65535 *modified-UTF-8* bytes). The constant pool must then write those chars in
//! **modified UTF-8** (U+0000 → C0 80; no plain NUL), which is what `modified_utf8` does.

const MAX_UTF8_INFO_LENGTH: usize = 65535;

/// Encode protobuf bytes into the `d1` `String[]` (byte→char identity, chunked).
pub fn bytes_to_strings(bytes: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut len = 0usize;
    for &b in bytes {
        let w = utf8_width(b as u32);
        if len + w > MAX_UTF8_INFO_LENGTH {
            out.push(std::mem::take(&mut cur));
            len = 0;
        }
        cur.push(b as char); // byte 0..=255 -> U+0000..=U+00FF
        len += w;
    }
    out.push(cur);
    out
}

/// Inverse of [bytes_to_strings] (each char's low byte). Used for round-trip tests.
pub fn strings_to_bytes(strings: &[String]) -> Vec<u8> {
    strings.iter().flat_map(|s| s.chars().map(|c| c as u8)).collect()
}

fn utf8_width(c: u32) -> usize {
    match c {
        0x0001..=0x007f => 1,
        0 | 0x0080..=0x07ff => 2, // U+0000 is 2 bytes in modified UTF-8
        _ => 3,
    }
}

/// JVM "modified UTF-8" (JVMS 4.4.7): like UTF-8 except U+0000 is `C0 80` and supplementary chars
/// use surrogate pairs (not needed for our `d1`, whose chars are U+0000..U+00FF).
pub fn modified_utf8(s: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for c in s.chars() {
        let u = c as u32;
        match u {
            0x0001..=0x007f => out.push(u as u8),
            0 | 0x0080..=0x07ff => {
                out.push(0xc0 | (u >> 6) as u8);
                out.push(0x80 | (u & 0x3f) as u8);
            }
            0x0800..=0xffff => {
                out.push(0xe0 | (u >> 12) as u8);
                out.push(0x80 | ((u >> 6) & 0x3f) as u8);
                out.push(0x80 | (u & 0x3f) as u8);
            }
            _ => {
                // supplementary: encode as surrogate pair, each 3 bytes
                let v = u - 0x10000;
                let hi = 0xd800 + (v >> 10);
                let lo = 0xdc00 + (v & 0x3ff);
                for s in [hi, lo] {
                    out.push(0xe0 | (s >> 12) as u8);
                    out.push(0x80 | ((s >> 6) & 0x3f) as u8);
                    out.push(0x80 | (s & 0x3f) as u8);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn d1_byte_to_char_identity_matches_reference() {
        // The exact d1 payload kotlinc 1.9.24 emits for `fun f(a: Int): Int = a`.
        let bytes: &[u8] = &[
            0x00, 0x08, 0x0a, 0x00, 0x0a, 0x02, 0x10, 0x08, 0x0a, 0x00, 0x1a, 0x0e, 0x10, 0x00,
            0x1a, 0x02, 0x30, 0x01, 0x32, 0x06, 0x10, 0x02, 0x1a, 0x02, 0x30, 0x01,
        ];
        let strings = bytes_to_strings(bytes);
        assert_eq!(strings.len(), 1);
        // chars equal the byte values (e.g. 0x30 -> '0', 0x31 -> '1')
        let chars: Vec<u32> = strings[0].chars().map(|c| c as u32).collect();
        let expect: Vec<u32> = bytes.iter().map(|&b| b as u32).collect();
        assert_eq!(chars, expect);
        assert_eq!(strings_to_bytes(&strings), bytes); // round-trip
    }

    #[test]
    fn modified_utf8_nul_is_c0_80() {
        assert_eq!(modified_utf8("\u{0}"), vec![0xc0, 0x80]);
    }

    #[test]
    fn modified_utf8_ascii_unchanged() {
        assert_eq!(modified_utf8("Code"), b"Code".to_vec());
        assert_eq!(modified_utf8("(II)I"), b"(II)I".to_vec());
    }

    #[test]
    fn modified_utf8_latin1_two_bytes() {
        // U+00FF -> 0xC3 0xBF (same as standard UTF-8 for this range)
        assert_eq!(modified_utf8("\u{ff}"), vec![0xc3, 0xbf]);
    }

    #[test]
    fn d1_string_encodes_without_plain_nul() {
        let bytes: &[u8] = &[0x00, 0x30, 0x01];
        let s = &bytes_to_strings(bytes)[0];
        let enc = modified_utf8(s);
        assert!(!enc.contains(&0x00), "modified UTF-8 must not contain a plain NUL: {enc:?}");
        assert_eq!(enc, vec![0xc0, 0x80, 0x30, 0x01]);
    }
}
