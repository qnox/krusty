//! The JVM string table a class's `@Metadata` names its strings through: the `StringTableTypes`
//! records that open `d1`, resolved against `d2` as kotlinc's `JvmNameResolverBase` resolves them.

use crate::metadata::decode::{packed_varints, Pb};

/// Decode the `@Metadata` `d1` string array to raw protobuf bytes. Modern metadata (since Kotlin 1.4)
/// stores each byte as one already-UTF8-decoded char.
pub(super) fn decode_d1(d1: &[String]) -> Vec<u8> {
    // `BitEncoding.decodeBytes`: a leading `UTF8_MODE_MARKER` ('0x00') as the first char of the first
    // string flags "UTF-8 mode" (each char IS one byte) and is DROPPED before decoding. Without dropping
    // it, the leading 0x00 shifts the `StringTableTypes`-delimited prefix by one and the split misreads.
    let mut out: Vec<u8> = Vec::new();
    for (i, s) in d1.iter().enumerate() {
        let mut chars = s.chars();
        if i == 0 && s.starts_with('\u{0}') {
            chars.next(); // drop the UTF8 mode marker
        }
        out.extend(chars.map(|c| c as u8));
    }
    out
}

/// kotlinc's `JvmNameResolverBase.PREDEFINED_STRINGS` — the fixed table a `StringTableTypes.Record`'s
/// `predefined_index` selects (common built-in class names that aren't stored in `d2`). Verbatim from
/// `core/metadata.jvm/.../JvmNameResolverBase.kt`, so `class_name` ids resolve identically to kotlinc.
pub(crate) const PREDEFINED_STRINGS: &[&str] = &[
    "kotlin/Any",
    "kotlin/Nothing",
    "kotlin/Unit",
    "kotlin/Throwable",
    "kotlin/Number",
    "kotlin/Byte",
    "kotlin/Double",
    "kotlin/Float",
    "kotlin/Int",
    "kotlin/Long",
    "kotlin/Short",
    "kotlin/Boolean",
    "kotlin/Char",
    "kotlin/CharSequence",
    "kotlin/String",
    "kotlin/Comparable",
    "kotlin/Enum",
    "kotlin/Array",
    "kotlin/ByteArray",
    "kotlin/DoubleArray",
    "kotlin/FloatArray",
    "kotlin/IntArray",
    "kotlin/LongArray",
    "kotlin/ShortArray",
    "kotlin/BooleanArray",
    "kotlin/CharArray",
    "kotlin/Cloneable",
    "kotlin/Annotation",
    "kotlin/collections/Iterable",
    "kotlin/collections/MutableIterable",
    "kotlin/collections/Collection",
    "kotlin/collections/MutableCollection",
    "kotlin/collections/List",
    "kotlin/collections/MutableList",
    "kotlin/collections/Set",
    "kotlin/collections/MutableSet",
    "kotlin/collections/Map",
    "kotlin/collections/MutableMap",
    "kotlin/collections/Map.Entry",
    "kotlin/collections/MutableMap.MutableEntry",
    "kotlin/collections/Iterator",
    "kotlin/collections/MutableIterator",
    "kotlin/collections/ListIterator",
    "kotlin/collections/MutableListIterator",
];

/// One expanded `StringTableTypes.Record` (the `range`-repeats are flattened so the index into the list
/// is the class-name id). Mirrors the fields kotlinc's `getString` consults.
#[derive(Clone, Default)]
pub(super) struct Rec {
    predefined_index: Option<usize>,
    string: Option<String>,
    operation: u64, // 0 NONE, 1 INTERNAL_TO_CLASS_ID, 2 DESC_TO_CLASS_ID
    substring: Option<(usize, usize)>,
    replace: Option<(u32, u32)>,
}

/// Parse one `StringTableTypes.Record` → `(range, Rec)`.
fn parse_record(body: &[u8]) -> Option<(u64, Rec)> {
    let mut pb = Pb::new(body);
    let mut range = 1u64;
    let mut rec = Rec::default();
    while !pb.at_end() {
        let tag = pb.varint()?;
        match (tag >> 3, tag & 7) {
            (1, 0) => range = pb.varint()?,
            (2, 0) => rec.predefined_index = Some(pb.varint()? as usize),
            (3, 0) => rec.operation = pb.varint()?,
            (4, 2) => {
                let n = pb.varint()? as usize;
                let v = packed_varints(pb.bytes(n)?)?;
                if v.len() >= 2 {
                    rec.substring = Some((v[0] as usize, v[1] as usize));
                }
            }
            (5, 2) => {
                let n = pb.varint()? as usize;
                let v = packed_varints(pb.bytes(n)?)?;
                if v.len() >= 2 {
                    rec.replace = Some((v[0] as u32, v[1] as u32));
                }
            }
            (6, 2) => {
                let n = pb.varint()? as usize;
                rec.string = Some(String::from_utf8_lossy(pb.bytes(n)?).into_owned());
            }
            (_, w) => pb.skip(w)?,
        }
    }
    Some((range, rec))
}

/// Parse a `StringTableTypes` message body → the flattened record list (each record repeated `range`
/// times, so the list index is the class-name id), matching kotlinc's `JvmNameResolverBase`.
pub(super) fn parse_string_table(body: &[u8]) -> Vec<Rec> {
    let mut pb = Pb::new(body);
    let mut records = Vec::new();
    while !pb.at_end() {
        let Some(tag) = pb.varint() else { break };
        match (tag >> 3, tag & 7) {
            (1, 2) => {
                let Some(n) = pb.varint() else { break };
                let Some(rbody) = pb.bytes(n as usize) else {
                    break;
                };
                if let Some((range, rec)) = parse_record(rbody) {
                    for _ in 0..range {
                        records.push(rec.clone());
                    }
                }
            }
            (_, w) => {
                if pb.skip(w).is_none() {
                    break;
                }
            }
        }
    }
    records
}

/// Resolve a class-name id to its qualified internal name, exactly as kotlinc's `JvmNameResolverBase.
/// getString`: pick the record's explicit string, else its predefined-table entry, else `d2[id]`; then
/// apply the substring/replace/operation transforms.
/// A PLAIN string-table entry (a method name or JVM descriptor from a `JvmMethodSignature`): the
/// `predefined`/`d2`/record string plus `substring`/`replace`, but NOT the `operation` (`$`→`.` /
/// strip-`L;`) class-name transform — that mangles a method name/descriptor (`sumOfInt` → `umOfIn`).
pub(super) fn resolve_string(records: &[Rec], d2: &[String], id: usize) -> Option<String> {
    let rec = records.get(id).cloned().unwrap_or_default();
    let mut s = if let Some(st) = rec.string {
        st
    } else if let Some(pi) = rec.predefined_index {
        PREDEFINED_STRINGS.get(pi)?.to_string()
    } else {
        d2.get(id)?.clone()
    };
    if let Some((begin, end)) = rec.substring {
        if begin <= end && end <= s.len() {
            s = s[begin..end].to_string();
        }
    }
    if let Some((from, to)) = rec.replace {
        if let (Some(f), Some(t)) = (char::from_u32(from), char::from_u32(to)) {
            s = s.replace(f, &t.to_string());
        }
    }
    Some(s)
}

pub(super) fn resolve_class_name(records: &[Rec], d2: &[String], id: usize) -> Option<String> {
    let rec = records.get(id).cloned().unwrap_or_default();
    let mut s = if let Some(st) = rec.string {
        st
    } else if let Some(pi) = rec.predefined_index {
        PREDEFINED_STRINGS.get(pi)?.to_string()
    } else {
        d2.get(id)?.clone()
    };
    if let Some((begin, end)) = rec.substring {
        if begin <= end && end <= s.len() {
            s = s[begin..end].to_string();
        }
    }
    if let Some((from, to)) = rec.replace {
        if let (Some(f), Some(t)) = (char::from_u32(from), char::from_u32(to)) {
            s = s.replace(f, &t.to_string());
        }
    }
    match rec.operation {
        1 => s = s.replace('$', "."),
        2 => {
            if s.len() >= 2 {
                s = s[1..s.len() - 1].to_string();
            }
            s = s.replace('$', ".");
        }
        _ => {}
    }
    Some(s)
}

/// Split decoded `d1` bytes into `(StringTableTypes body, Package body)`: JVM `@Metadata` prepends a
/// length-delimited `StringTableTypes` before the `Package` message.
pub(super) fn split_d1(bytes: &[u8]) -> (&[u8], &[u8]) {
    let mut pb = Pb::new(bytes);
    if let Some(len) = pb.varint() {
        let start = pb.position();
        if let Some(end) = start.checked_add(len as usize) {
            if end <= bytes.len() {
                return (&bytes[start..end], &bytes[end..]);
            }
        }
    }
    (&[], bytes)
}
