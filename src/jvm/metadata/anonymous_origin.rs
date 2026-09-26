//! The `@Metadata` of an anonymous object or lambda class regenerated for an inline call site:
//! kotlinc's `AnonymousObjectTransformer.transformMetadata`, which records the class the copy was
//! made from (`anonymous_object_origin_name` on a class, `lambda_class_origin_name` on a synthetic
//! class) and otherwise leaves the metadata as the original class wrote it.
//!
//! kotlinc reads the proto, sets the extension and writes the proto and its string table back. The
//! string table (`JvmStringTable(nameResolver)`) keeps every record it read and names the origin by
//! the last string that resolves to it, appending a string only when none does. A proto written by
//! kotlinc serializes its fields in number order and its extensions after them, so the rewrite here
//! keeps every field's bytes and places the extension among the extensions by its number.

use super::string_table::{decode_d1, parse_string_table, resolve_class_name, split_d1};
use super::MetadataDecodeError;
use crate::metadata::decode::Pb;
use crate::metadata::encoding::bytes_to_strings;

/// `KotlinClassHeader.Kind.CLASS`.
const CLASS_KIND: i32 = 1;
/// `KotlinClassHeader.Kind.SYNTHETIC_CLASS`.
const SYNTHETIC_CLASS_KIND: i32 = 3;
/// `JvmProtoBuf.anonymousObjectOriginName`, an extension of `Class`.
const ANONYMOUS_OBJECT_ORIGIN_NAME: u64 = 103;
/// `JvmProtoBuf.lambdaClassOriginName`, an extension of `Function`.
const LAMBDA_CLASS_ORIGIN_NAME: u64 = 101;
/// `StringTableTypes.record`.
const RECORD: u64 = 1;
/// `Record.range`.
const RECORD_RANGE: u64 = 1;

const VARINT: u64 = 0;
const LENGTH_DELIMITED: u64 = 2;

/// A class's `@Metadata` `d1` (the encoded message) and `d2` (its strings).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct MetadataStrings {
    pub d1: Vec<String>,
    pub d2: Vec<String>,
}

/// The `d1` and `d2` of the regenerated copy of a class whose metadata is `kind` over `strings`
/// and whose internal name is `origin`. `None` for a kind kotlinc leaves as it is.
pub(crate) fn record_origin_name(
    kind: i32,
    strings: &MetadataStrings,
    origin: &str,
) -> Result<Option<MetadataStrings>, MetadataDecodeError> {
    let extension = match kind {
        CLASS_KIND => ANONYMOUS_OBJECT_ORIGIN_NAME,
        SYNTHETIC_CLASS_KIND => LAMBDA_CLASS_ORIGIN_NAME,
        _ => return Ok(None),
    };
    let bytes = decode_d1(&strings.d1);
    let (table, message) = split_d1(&bytes);
    let mut strings = strings.d2.clone();
    let mut table = RawStringTable::read(table)?;
    let index = table.string_index(&mut strings, origin);
    let message = with_varint_extension(message, extension, index as u64)?;

    let table = table.write();
    let mut encoded = vec![0x00]; // BitEncoding's UTF-8 mode marker
    varint(&mut encoded, table.len() as u64);
    encoded.extend_from_slice(&table);
    encoded.extend_from_slice(&message);
    Ok(Some(MetadataStrings {
        d1: bytes_to_strings(&encoded),
        d2: strings,
    }))
}

/// A `StringTableTypes` message as kotlinc's `JvmStringTable` holds it: the records unexpanded, in
/// order, and the fields after them (`local_name`) as they were written.
struct RawStringTable {
    /// Each record's message bytes.
    records: Vec<Vec<u8>>,
    /// Every field that is not a record, encoded, in its order.
    rest: Vec<u8>,
    /// The table expanded, as `JvmNameResolverBase` indexes it.
    resolved: Vec<super::string_table::Rec>,
}

impl RawStringTable {
    fn read(body: &[u8]) -> Result<RawStringTable, MetadataDecodeError> {
        let mut pb = Pb::new(body);
        let mut records = Vec::new();
        let mut rest = Vec::new();
        while !pb.at_end() {
            let start = pb.position();
            let tag = pb.varint().ok_or(MetadataDecodeError::MalformedWire)?;
            if tag == (RECORD << 3 | LENGTH_DELIMITED) {
                let length = pb.varint().ok_or(MetadataDecodeError::MalformedWire)?;
                let record = pb
                    .bytes(length as usize)
                    .ok_or(MetadataDecodeError::MalformedWire)?;
                records.push(record.to_vec());
            } else {
                pb.skip(tag & 7).ok_or(MetadataDecodeError::MalformedWire)?;
                rest.extend_from_slice(&body[start..pb.position()]);
            }
        }
        Ok(RawStringTable {
            records,
            rest,
            resolved: parse_string_table(body),
        })
    }

    /// `JvmStringTable.getStringIndex`: the last index whose string resolves to `string`, or a new
    /// string appended to `strings`, covered by the last record when that record is trivial and by
    /// a new record otherwise.
    fn string_index(&mut self, strings: &mut Vec<String>, string: &str) -> usize {
        let existing = (0..strings.len()).rev().find(|&index| {
            resolve_class_name(&self.resolved, strings, index).as_deref() == Some(string)
        });
        if let Some(index) = existing {
            return index;
        }
        let index = strings.len();
        strings.push(string.to_string());
        match self.records.last_mut() {
            Some(last) if is_trivial(last) => *last = with_range(last, range(last) + 1),
            _ => self.records.push(Vec::new()),
        }
        index
    }

    fn write(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for record in &self.records {
            varint(&mut out, RECORD << 3 | LENGTH_DELIMITED);
            varint(&mut out, record.len() as u64);
            out.extend_from_slice(record);
        }
        out.extend_from_slice(&self.rest);
        out
    }
}

/// `Record.isTrivial` in `JvmStringTable`: no predefined index, operation, substring or replaced
/// character. (A literal `string` does not count.)
fn is_trivial(record: &[u8]) -> bool {
    fields(record).is_some_and(|fields| {
        fields
            .iter()
            .all(|field| field.number == RECORD_RANGE || field.number == 6)
    })
}

fn range(record: &[u8]) -> u64 {
    fields(record)
        .and_then(|fields| {
            fields
                .iter()
                .find(|field| field.number == RECORD_RANGE)
                .and_then(|field| Pb::new(&record[field.value.clone()]).varint())
        })
        .unwrap_or(1)
}

/// `record` with its `range` set to `range`, its fields in number order as the builder writes them.
fn with_range(record: &[u8], range: u64) -> Vec<u8> {
    let mut field = Vec::new();
    varint(&mut field, RECORD_RANGE << 3 | VARINT);
    varint(&mut field, range);
    replace_field(record, RECORD_RANGE, field)
}

/// `message` with the varint extension `number` set to `value`.
fn with_varint_extension(
    message: &[u8],
    number: u64,
    value: u64,
) -> Result<Vec<u8>, MetadataDecodeError> {
    fields(message).ok_or(MetadataDecodeError::MalformedWire)?;
    let mut field = Vec::new();
    varint(&mut field, number << 3 | VARINT);
    varint(&mut field, value);
    Ok(replace_field(message, number, field))
}

/// `message` with every field `number` removed and `field` placed before the first field numbered
/// above it.
fn replace_field(message: &[u8], number: u64, field: Vec<u8>) -> Vec<u8> {
    let existing = fields(message).unwrap_or_default();
    let mut out = Vec::with_capacity(message.len() + field.len());
    let mut placed = false;
    for existing in existing {
        if existing.number == number {
            continue;
        }
        if !placed && existing.number > number {
            out.extend_from_slice(&field);
            placed = true;
        }
        out.extend_from_slice(&message[existing.whole.clone()]);
    }
    if !placed {
        out.extend_from_slice(&field);
    }
    out
}

struct Field {
    number: u64,
    /// The whole field: its tag and its value.
    whole: std::ops::Range<usize>,
    /// Its value without the tag.
    value: std::ops::Range<usize>,
}

fn fields(message: &[u8]) -> Option<Vec<Field>> {
    let mut pb = Pb::new(message);
    let mut out = Vec::new();
    while !pb.at_end() {
        let start = pb.position();
        let tag = pb.varint()?;
        let value = pb.position();
        pb.skip(tag & 7)?;
        out.push(Field {
            number: tag >> 3,
            whole: start..pb.position(),
            value: value..pb.position(),
        });
    }
    Some(out)
}

fn varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value as u8 & 0x7f) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

#[cfg(test)]
mod tests {
    use super::{record_origin_name, MetadataStrings};
    use crate::metadata::encoding::strings_to_bytes;

    /// `object : Greeter { override fun greet() = … }` in `lib.greeter`, as kotlinc writes it.
    fn greeter() -> MetadataStrings {
        let d1 = "\u{0}\u{f}\n\u{0}\n\u{2}\u{18}\u{2}\n\u{0}\n\u{2}\u{10}\u{e}*\u{1}\u{0}\u{8}\n\u{18}\u{0}2\u{2}0\u{1}J\u{8}\u{10}\u{2}\u{1a}\u{2}0\u{3}H\u{16}";
        let d2 = ["lib/LibKt$greeter$1", "Llib/Greeter;", "greet", ""];
        MetadataStrings {
            d1: vec![d1.to_string()],
            d2: d2.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn a_class_names_its_origin_by_the_string_that_already_spells_it() {
        let original = greeter();
        let copy = record_origin_name(1, &original, "lib/LibKt$greeter$1")
            .expect("readable metadata")
            .expect("a class records its origin");
        assert_eq!(copy.d2, original.d2);
        let mut expected = strings_to_bytes(&original.d1);
        expected.extend_from_slice(&[0xb8, 0x06, 0x00]);
        assert_eq!(strings_to_bytes(&copy.d1), expected);
    }

    #[test]
    fn an_origin_no_string_spells_is_appended_with_a_record_of_its_own() {
        let original = greeter();
        let copy = record_origin_name(1, &original, "lib/Elsewhere$1")
            .expect("readable metadata")
            .expect("a class records its origin");
        assert_eq!(copy.d2.last().map(String::as_str), Some("lib/Elsewhere$1"));
        assert_eq!(copy.d2.len(), original.d2.len() + 1);
        let bytes = strings_to_bytes(&copy.d1);
        // The table's last record names a predefined string, so it is not trivial: the new string
        // gets an empty record of its own, and the origin is string 4.
        assert!(bytes.ends_with(&[0xb8, 0x06, 0x04]));
    }

    #[test]
    fn a_trivial_last_record_covers_the_appended_origin() {
        // One empty record over `d2 = ["x"]`, then an empty class.
        let original = MetadataStrings {
            d1: vec!["\u{0}\u{2}\n\u{0}".to_string()],
            d2: vec!["x".to_string()],
        };
        let copy = record_origin_name(1, &original, "lib/Origin$1")
            .expect("readable metadata")
            .expect("a class records its origin");
        assert_eq!(copy.d2, ["x", "lib/Origin$1"]);
        assert_eq!(
            strings_to_bytes(&copy.d1),
            [0x00, 0x04, 0x0a, 0x02, 0x08, 0x02, 0xb8, 0x06, 0x01]
        );
    }

    #[test]
    fn a_kind_without_an_origin_is_left_alone() {
        assert_eq!(
            record_origin_name(2, &greeter(), "lib/LibKt").expect("readable"),
            None
        );
    }
}
