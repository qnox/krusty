//! Raw Kotlin `@Metadata` payload inspection for byte-parity tests.

use std::collections::HashMap;

/// The raw `@kotlin.Metadata` payload of a class file: the `d1` bytes and `d2` strings exactly as
/// written. The normal class reader decodes metadata and does not retain this packed representation.
pub(crate) fn raw_kotlin_metadata(bytes: &[u8]) -> Option<(Vec<u8>, Vec<String>)> {
    let elements = kotlin_metadata_elements(bytes)?;
    let strings = |name: &str| {
        elements.iter().find_map(|(element, value)| match value {
            MetadataElement::Strings(strings) if element == name => Some(strings.clone()),
            _ => None,
        })
    };
    let payload = strings("d1")?.concat().chars().map(|c| c as u8).collect();
    Some((payload, strings("d2").unwrap_or_default()))
}

/// The integer elements of a class file's `@kotlin.Metadata` (`k`, `mv`, `xi`, ...) in the order
/// they are written.
pub(crate) fn kotlin_metadata_ints(bytes: &[u8]) -> Option<Vec<(String, Vec<i32>)>> {
    let elements = kotlin_metadata_elements(bytes)?;
    Some(
        elements
            .into_iter()
            .filter_map(|(element, value)| match value {
                MetadataElement::Ints(ints) => Some((element, ints)),
                MetadataElement::Strings(_) => None,
            })
            .collect(),
    )
}

enum MetadataElement {
    Strings(Vec<String>),
    Ints(Vec<i32>),
}

/// Every element of the class's `@kotlin.Metadata` annotation, in class-file order.
fn kotlin_metadata_elements(bytes: &[u8]) -> Option<Vec<(String, MetadataElement)>> {
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

    struct Pool {
        utf8: HashMap<usize, String>,
        ints: HashMap<usize, i32>,
    }

    /// Read one `element_value`: the strings of a string (array) value, the ints of an int (array)
    /// value, nothing for any other tag. Advances past the value either way.
    fn read_element(
        r: &mut Reader<'_>,
        pool: &Pool,
        strings: &mut Vec<String>,
        ints: &mut Vec<i32>,
    ) {
        let tag = r.u1();
        match tag {
            b'[' => {
                let n = r.u2();
                for _ in 0..n {
                    read_element(r, pool, strings, ints);
                }
            }
            b's' => {
                let index = r.u2();
                strings.push(pool.utf8.get(&index).cloned().unwrap_or_default());
            }
            b'I' => {
                let index = r.u2();
                ints.push(pool.ints[&index]);
            }
            b'e' => {
                r.skip(4); // type_name_index + const_name_index
            }
            b'c' | b'B' | b'C' | b'D' | b'F' | b'J' | b'S' | b'Z' => {
                r.skip(2);
            }
            b'@' => {
                r.skip(2);
                let pairs = r.u2();
                for _ in 0..pairs {
                    r.skip(2);
                    read_element(r, pool, &mut Vec::new(), &mut Vec::new());
                }
            }
            other => panic!("unknown annotation element tag {other}"),
        }
    }

    let mut r = Reader { bytes, at: 8 };
    let count = r.u2();
    let mut pool = Pool {
        utf8: HashMap::new(),
        ints: HashMap::new(),
    };
    let mut index = 1;
    while index < count {
        let tag = r.u1();
        match tag {
            1 => {
                let len = r.u2();
                let raw = &r.bytes[r.at..r.at + len];
                r.skip(len);
                pool.utf8.insert(index, mutf8_to_string(raw));
            }
            7 | 8 | 16 | 19 | 20 => r.skip(2),
            15 => r.skip(3),
            3 => {
                let value = r.u4() as u32 as i32;
                pool.ints.insert(index, value);
            }
            4 | 9 | 10 | 11 | 12 | 17 | 18 => r.skip(4),
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
        if pool.utf8.get(&name).map(String::as_str) != Some("RuntimeVisibleAnnotations") {
            r.at = end;
            continue;
        }
        let annotations = r.u2();
        for _ in 0..annotations {
            let ty = r.u2();
            let pairs = r.u2();
            let is_metadata = pool.utf8.get(&ty).map(String::as_str) == Some("Lkotlin/Metadata;");
            let mut elements = Vec::new();
            for _ in 0..pairs {
                let element = r.u2();
                let element = pool.utf8.get(&element).cloned().unwrap_or_default();
                let (mut strings, mut ints) = (Vec::new(), Vec::new());
                read_element(&mut r, &pool, &mut strings, &mut ints);
                match ints.is_empty() {
                    true => elements.push((element, MetadataElement::Strings(strings))),
                    false => elements.push((element, MetadataElement::Ints(ints))),
                }
            }
            if is_metadata {
                return Some(elements);
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
