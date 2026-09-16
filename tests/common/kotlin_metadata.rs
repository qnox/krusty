//! Raw Kotlin `@Metadata` payload inspection for byte-parity tests.

use std::collections::HashMap;

/// The raw `@kotlin.Metadata` payload of a class file: the `d1` bytes and `d2` strings exactly as
/// written. The normal class reader decodes metadata and does not retain this packed representation.
pub(super) fn raw_kotlin_metadata(bytes: &[u8]) -> Option<(Vec<u8>, Vec<String>)> {
    struct Reader<'a> {
        bytes: &'a [u8],
        at: usize,
    }

    impl Reader<'_> {
        fn u1(&mut self) -> u8 {
            let value = self.bytes[self.at];
            self.at += 1;
            value
        }

        fn u2(&mut self) -> usize {
            let value = u16::from_be_bytes([self.bytes[self.at], self.bytes[self.at + 1]]);
            self.at += 2;
            value as usize
        }

        fn u4(&mut self) -> usize {
            let value = u32::from_be_bytes(self.bytes[self.at..self.at + 4].try_into().unwrap());
            self.at += 4;
            value as usize
        }

        fn skip(&mut self, n: usize) {
            self.at += n;
        }
    }

    /// Read one `element_value`: the strings of a string or array-of-string value, empty for any
    /// other tag. Advances past the value either way, so the caller stays in sync.
    fn read_element(r: &mut Reader<'_>, utf8: &HashMap<usize, String>) -> Vec<String> {
        let tag = r.u1();
        match tag {
            b'[' => {
                let n = r.u2();
                let mut all = Vec::new();
                for _ in 0..n {
                    all.extend(read_element(r, utf8));
                }
                all
            }
            b's' => {
                let index = r.u2();
                vec![utf8.get(&index).cloned().unwrap_or_default()]
            }
            b'e' => {
                r.skip(4); // type_name_index + const_name_index
                Vec::new()
            }
            b'c' | b'B' | b'C' | b'D' | b'F' | b'I' | b'J' | b'S' | b'Z' => {
                r.skip(2);
                Vec::new()
            }
            b'@' => {
                r.skip(2);
                let pairs = r.u2();
                for _ in 0..pairs {
                    r.skip(2);
                    read_element(r, utf8);
                }
                Vec::new()
            }
            other => panic!("unknown annotation element tag {other}"),
        }
    }

    let mut r = Reader { bytes, at: 8 };
    let count = r.u2();
    let mut utf8: HashMap<usize, String> = HashMap::new();
    let mut index = 1;
    while index < count {
        let tag = r.u1();
        match tag {
            1 => {
                let len = r.u2();
                let raw = &r.bytes[r.at..r.at + len];
                r.skip(len);
                utf8.insert(index, mutf8_to_string(raw));
            }
            7 | 8 | 16 | 19 | 20 => r.skip(2),
            15 => r.skip(3),
            3 | 4 | 9 | 10 | 11 | 12 | 17 | 18 => r.skip(4),
            5 | 6 => {
                r.skip(8);
                index += 1; // long/double occupy two entries
            }
            other => panic!("unknown constant-pool tag {other}"),
        }
        index += 1;
    }
    r.skip(6); // access, this_class, super_class
    let interfaces = r.u2();
    r.skip(interfaces * 2);
    for _ in 0..2 {
        let members = r.u2();
        for _ in 0..members {
            r.skip(6);
            let attributes = r.u2();
            for _ in 0..attributes {
                r.skip(2);
                let len = r.u4();
                r.skip(len);
            }
        }
    }
    let attributes = r.u2();
    for _ in 0..attributes {
        let name = r.u2();
        let len = r.u4();
        let end = r.at + len;
        if utf8.get(&name).map(String::as_str) != Some("RuntimeVisibleAnnotations") {
            r.at = end;
            continue;
        }
        let annotations = r.u2();
        for _ in 0..annotations {
            let ty = r.u2();
            let pairs = r.u2();
            let is_metadata = utf8.get(&ty).map(String::as_str) == Some("Lkotlin/Metadata;");
            let mut d1: Option<Vec<String>> = None;
            let mut d2: Option<Vec<String>> = None;
            for _ in 0..pairs {
                let element = r.u2();
                let element = utf8.get(&element).cloned().unwrap_or_default();
                let value = read_element(&mut r, &utf8);
                match element.as_str() {
                    "d1" if is_metadata => d1 = Some(value),
                    "d2" if is_metadata => d2 = Some(value),
                    _ => {}
                }
            }
            if let Some(d1) = d1 {
                let payload = d1.concat().chars().map(|c| c as u8).collect();
                return Some((payload, d2.unwrap_or_default()));
            }
        }
        r.at = end;
    }
    None
}

/// Decode modified UTF-8 into a `String`.
fn mutf8_to_string(raw: &[u8]) -> String {
    let mut out = String::new();
    let mut at = 0;
    while at < raw.len() {
        let byte = raw[at];
        let code = if byte < 0x80 {
            at += 1;
            byte as u32
        } else if byte & 0xE0 == 0xC0 {
            let code = ((byte as u32 & 0x1F) << 6) | (raw[at + 1] as u32 & 0x3F);
            at += 2;
            code
        } else {
            let code = ((byte as u32 & 0x0F) << 12)
                | ((raw[at + 1] as u32 & 0x3F) << 6)
                | (raw[at + 2] as u32 & 0x3F);
            at += 3;
            code
        };
        out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
    }
    out
}
