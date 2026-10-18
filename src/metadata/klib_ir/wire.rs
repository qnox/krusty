//! Protobuf wire reading and the per-file table layout of serialized KLIB IR.
//!
//! Every `default/ir/` table is a sequence of per-file blobs, and every blob is either an entry
//! table or the declaration index; entries are protobuf messages read field by field. This module
//! owns that container format so declaration and body decoding read only message semantics.

use crate::fx_hash::HashMap;

use crate::klib::KlibArchive;

use super::KlibIrDecodeError;

const IR_PREFIX: &str = "default/ir/";
const INLINABLE_IR_PREFIX: &str = "default/ir_inlinable_functions/";

/// A protobuf `int32` read through a varint: negative values arrive sign-extended to 64 bits.
pub(super) fn int32(value: u64) -> i32 {
    value as i64 as i32
}

/// `BinaryLattice`: two ints interleaved bit by bit, the first in the even bits.
pub(super) fn lattice(code: u64) -> (u64, u64) {
    fn deinterleave(mut x: u64) -> u64 {
        x &= 0x5555_5555_5555_5555;
        x = (x ^ (x >> 1)) & 0x3333_3333_3333_3333;
        x = (x ^ (x >> 2)) & 0x0f0f_0f0f_0f0f_0f0f;
        x = (x ^ (x >> 4)) & 0x00ff_00ff_00ff_00ff;
        x = (x ^ (x >> 8)) & 0x0000_ffff_0000_ffff;
        (x ^ (x >> 16)) & 0x0000_0000_ffff_ffff
    }
    (deinterleave(code), deinterleave(code >> 1))
}

/// The pre-2.4 `IrOperationPre_2_4_0` alternative's number in today's `IrExpression` oneof.
pub(super) fn current_operation(legacy: u64) -> Option<u64> {
    const CURRENT: [u64; 39] = [
        10, 22, 8, 19, 21, 5, 23, 26, 25, 27, 39, 18, 20, 15, 17, 6, 28, 40, 12, 16, 7, 29, 30, 31,
        14, 32, 13, 24, 33, 34, 35, 9, 36, 37, 38, 11, 43, 41, 42,
    ];
    let index = usize::try_from(legacy.checked_sub(1)?).ok()?;
    CURRENT.get(index).copied()
}

pub(super) struct DeclarationTreeTables {
    pub(super) files: Vec<Vec<u8>>,
    pub(super) strings: Vec<Vec<u8>>,
    pub(super) signatures: Vec<Vec<u8>>,
    pub(super) declarations: Vec<Vec<u8>>,
    pub(super) bodies: Vec<Vec<u8>>,
    pub(super) types: Vec<Vec<u8>>,
    pub(super) file_entries: Option<Vec<Vec<u8>>>,
}

/// Read and count-check every parallel table used by one declaration-tree component.
pub(super) fn read_declaration_tree_tables(
    archive: &KlibArchive,
    inlinable: bool,
) -> Result<DeclarationTreeTables, KlibIrDecodeError> {
    let table = |name| {
        if inlinable {
            read_inlinable_per_file_table(archive, name)
        } else {
            read_per_file_table(archive, name)
        }
    };
    let tables = DeclarationTreeTables {
        files: table("files.knf")?,
        strings: table("strings.knt")?,
        signatures: table("signatures.knt")?,
        declarations: table("irDeclarations.knd")?,
        bodies: table("bodies.knb")?,
        types: table("types.knt")?,
        file_entries: inlinable.then(|| table("fileEntries.knf")).transpose()?,
    };
    let expected = tables.files.len();
    let counts = [
        tables.strings.len(),
        tables.signatures.len(),
        tables.declarations.len(),
        tables.bodies.len(),
        tables.types.len(),
    ];
    let file_entries_disagree = tables
        .file_entries
        .as_ref()
        .is_some_and(|entries| entries.len() != expected);
    if counts.iter().any(|count| *count != expected) || file_entries_disagree {
        return Err(KlibIrDecodeError::malformed(
            if inlinable {
                "default/ir_inlinable_functions"
            } else {
                "default/ir"
            },
            0,
            format!(
                "per-file table counts disagree: files={expected}, tables={counts:?}, file entries={:?}",
                tables.file_entries.as_ref().map(Vec::len)
            ),
        ));
    }
    Ok(tables)
}

#[derive(Clone, Copy)]
pub(super) enum WireValue<'a> {
    Varint(u64),
    Fixed32(u32),
    Fixed64(u64),
    Bytes(&'a [u8]),
}

#[derive(Clone, Copy)]
pub(super) struct WireField<'a> {
    pub(super) number: u64,
    pub(super) offset: usize,
    pub(super) value_offset: usize,
    pub(super) value: WireValue<'a>,
}

impl<'a> WireField<'a> {
    pub(super) fn bytes(&self, context: &str, entry: &str) -> Result<&'a [u8], KlibIrDecodeError> {
        match self.value {
            WireValue::Bytes(value) => Ok(value),
            _ => Err(wire_error(entry, self.offset, context, 2)),
        }
    }

    pub(super) fn varint(&self, context: &str, entry: &str) -> Result<u64, KlibIrDecodeError> {
        match self.value {
            WireValue::Varint(value) => Ok(value),
            _ => Err(wire_error(entry, self.offset, context, 0)),
        }
    }

    pub(super) fn fixed32(&self, context: &str, entry: &str) -> Result<u32, KlibIrDecodeError> {
        match self.value {
            WireValue::Fixed32(value) => Ok(value),
            _ => Err(wire_error(entry, self.offset, context, 5)),
        }
    }

    pub(super) fn fixed64(&self, context: &str, entry: &str) -> Result<u64, KlibIrDecodeError> {
        match self.value {
            WireValue::Fixed64(value) => Ok(value),
            _ => Err(wire_error(entry, self.offset, context, 1)),
        }
    }
}

fn wire_error(entry: &str, offset: usize, context: &str, expected: u64) -> KlibIrDecodeError {
    KlibIrDecodeError::malformed(
        entry,
        offset,
        format!("{context} has the wrong wire type; expected {expected}"),
    )
}

pub(super) fn message<'a>(
    bytes: &'a [u8],
    entry: &str,
    base: usize,
) -> Result<Vec<WireField<'a>>, KlibIrDecodeError> {
    let mut cursor = 0usize;
    let mut fields = Vec::new();
    while cursor < bytes.len() {
        let offset = cursor;
        let tag = varint(bytes, &mut cursor, entry, base, "field tag")?;
        let number = tag >> 3;
        let wire = tag & 7;
        if number == 0 {
            return Err(KlibIrDecodeError::malformed(
                entry,
                base + offset,
                "protobuf field number is zero",
            ));
        }
        let value_offset = cursor;
        let value = match wire {
            0 => WireValue::Varint(varint(bytes, &mut cursor, entry, base, "varint field")?),
            1 => {
                let value = take(bytes, &mut cursor, 8, entry, base, "fixed64 field")?;
                WireValue::Fixed64(u64::from_le_bytes(
                    value.try_into().expect("eight bytes were read"),
                ))
            }
            2 => {
                let length = varint(bytes, &mut cursor, entry, base, "field length")?;
                let length = usize::try_from(length).map_err(|_| {
                    KlibIrDecodeError::malformed(
                        entry,
                        base + value_offset,
                        "length-delimited field exceeds the host range",
                    )
                })?;
                let payload_offset = cursor;
                let value = take(bytes, &mut cursor, length, entry, base, "field payload")?;
                fields.push(WireField {
                    number,
                    offset: base + offset,
                    value_offset: base + payload_offset,
                    value: WireValue::Bytes(value),
                });
                continue;
            }
            5 => {
                let value = take(bytes, &mut cursor, 4, entry, base, "fixed32 field")?;
                WireValue::Fixed32(u32::from_le_bytes(
                    value.try_into().expect("four bytes were read"),
                ))
            }
            _ => {
                return Err(KlibIrDecodeError::malformed(
                    entry,
                    base + offset,
                    format!("unsupported protobuf wire type {wire}"),
                ));
            }
        };
        fields.push(WireField {
            number,
            offset: base + offset,
            value_offset: base + value_offset,
            value,
        });
    }
    Ok(fields)
}

fn unique_bytes_field<'a>(
    bytes: &'a [u8],
    entry: &str,
    base: usize,
    number: u64,
    context: &str,
) -> Result<Option<&'a [u8]>, KlibIrDecodeError> {
    let fields = message(bytes, entry, base)?;
    Ok(unique_bytes(&fields, number, context, entry)?.map(|field| field.value))
}

pub(super) fn required_bytes_field<'a>(
    bytes: &'a [u8],
    entry: &str,
    base: usize,
    number: u64,
    context: &str,
) -> Result<&'a [u8], KlibIrDecodeError> {
    unique_bytes_field(bytes, entry, base, number, context)?
        .ok_or_else(|| KlibIrDecodeError::malformed(entry, base, format!("missing {context}")))
}

pub(super) fn unique_bytes<'a>(
    fields: &[WireField<'a>],
    number: u64,
    context: &str,
    entry: &str,
) -> Result<Option<BytesField<'a>>, KlibIrDecodeError> {
    let mut found = None;
    for field in fields.iter().filter(|field| field.number == number) {
        let value = field.bytes(context, entry)?;
        if found
            .replace(BytesField {
                value,
                value_offset: field.value_offset,
            })
            .is_some()
        {
            return Err(KlibIrDecodeError::malformed(
                entry,
                field.offset,
                format!("duplicate {context}"),
            ));
        }
    }
    Ok(found)
}

#[derive(Clone, Copy)]
pub(super) struct BytesField<'a> {
    pub(super) value: &'a [u8],
    pub(super) value_offset: usize,
}

pub(super) fn unique_varint(
    fields: &[WireField<'_>],
    number: u64,
    context: &str,
    entry: &str,
) -> Result<Option<u64>, KlibIrDecodeError> {
    let mut found = None;
    for field in fields.iter().filter(|field| field.number == number) {
        let value = field.varint(context, entry)?;
        if found.replace(value).is_some() {
            return Err(KlibIrDecodeError::malformed(
                entry,
                field.offset,
                format!("duplicate {context}"),
            ));
        }
    }
    Ok(found)
}

pub(super) fn packed_field(
    bytes: &[u8],
    entry: &str,
    number: u64,
    context: &str,
) -> Result<Vec<u64>, KlibIrDecodeError> {
    packed_values(&message(bytes, entry, 0)?, number, entry, context)
}

/// Every value of a repeated scalar field, whether the serializer packed it or not.
pub(super) fn packed_values(
    fields: &[WireField<'_>],
    number: u64,
    entry: &str,
    context: &str,
) -> Result<Vec<u64>, KlibIrDecodeError> {
    let mut values = Vec::new();
    for field in fields.iter().filter(|field| field.number == number) {
        match field.value {
            WireValue::Varint(value) => values.push(value),
            WireValue::Bytes(packed) => {
                let mut cursor = 0;
                while cursor < packed.len() {
                    values.push(varint(
                        packed,
                        &mut cursor,
                        entry,
                        field.value_offset,
                        context,
                    )?);
                }
            }
            _ => return Err(wire_error(entry, field.offset, context, 0)),
        }
    }
    Ok(values)
}

fn varint(
    bytes: &[u8],
    cursor: &mut usize,
    entry: &str,
    base: usize,
    context: &str,
) -> Result<u64, KlibIrDecodeError> {
    let start = *cursor;
    let mut value = 0u64;
    for shift in (0..=63).step_by(7) {
        let byte = *bytes.get(*cursor).ok_or_else(|| {
            KlibIrDecodeError::malformed(entry, base + start, format!("truncated {context}"))
        })?;
        *cursor += 1;
        if shift == 63 && byte > 1 {
            return Err(KlibIrDecodeError::malformed(
                entry,
                base + start,
                format!("overflowing {context}"),
            ));
        }
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err(KlibIrDecodeError::malformed(
        entry,
        base + start,
        format!("overflowing {context}"),
    ))
}

fn take<'a>(
    bytes: &'a [u8],
    cursor: &mut usize,
    length: usize,
    entry: &str,
    base: usize,
    context: &str,
) -> Result<&'a [u8], KlibIrDecodeError> {
    let start = *cursor;
    let end = start.checked_add(length).ok_or_else(|| {
        KlibIrDecodeError::malformed(entry, base + start, format!("oversized {context}"))
    })?;
    let value = bytes.get(start..end).ok_or_else(|| {
        KlibIrDecodeError::malformed(entry, base + start, format!("truncated {context}"))
    })?;
    *cursor = end;
    Ok(value)
}

pub(super) fn read_per_file_table(
    archive: &KlibArchive,
    name: &str,
) -> Result<Vec<Vec<u8>>, KlibIrDecodeError> {
    read_per_file_table_at(archive, IR_PREFIX, name)
}

pub(super) fn read_inlinable_per_file_table(
    archive: &KlibArchive,
    name: &str,
) -> Result<Vec<Vec<u8>>, KlibIrDecodeError> {
    read_per_file_table_at(archive, INLINABLE_IR_PREFIX, name)
}

fn read_per_file_table_at(
    archive: &KlibArchive,
    prefix: &str,
    name: &str,
) -> Result<Vec<Vec<u8>>, KlibIrDecodeError> {
    let entry = format!("{prefix}{name}");
    if !archive
        .entries()
        .iter()
        .any(|candidate| candidate == &entry)
    {
        return Err(KlibIrDecodeError::Missing { entry });
    }
    let bytes = archive.read(&entry)?;
    per_file(&bytes, name)
}

fn per_file(bytes: &[u8], entry: &str) -> Result<Vec<Vec<u8>>, KlibIrDecodeError> {
    let count = usize::try_from(u32_at(bytes, 0, entry, "per-file count")?).expect("u32 fits");
    let header = 4usize
        .checked_add(count.checked_mul(4).ok_or_else(|| {
            KlibIrDecodeError::malformed(entry, 0, "per-file size table overflows")
        })?)
        .ok_or_else(|| KlibIrDecodeError::malformed(entry, 0, "per-file header overflows"))?;
    if header > bytes.len() {
        return Err(KlibIrDecodeError::malformed(
            entry,
            bytes.len(),
            "truncated per-file size table",
        ));
    }
    let mut offset = header;
    let mut output = Vec::with_capacity(count);
    for index in 0..count {
        let size =
            usize::try_from(u32_at(bytes, 4 + index * 4, entry, "file size")?).expect("u32 fits");
        let end = offset.checked_add(size).ok_or_else(|| {
            KlibIrDecodeError::malformed(entry, offset, format!("file {index} size overflows"))
        })?;
        output.push(
            bytes
                .get(offset..end)
                .ok_or_else(|| {
                    KlibIrDecodeError::malformed(entry, offset, format!("truncated file {index}"))
                })?
                .to_vec(),
        );
        offset = end;
    }
    require_end(bytes, offset, entry)?;
    Ok(output)
}

pub(super) fn entries<'a>(
    bytes: &'a [u8],
    entry: &str,
) -> Result<Vec<&'a [u8]>, KlibIrDecodeError> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    let raw = u32_at(bytes, 0, entry, "entry count")? as i32;
    let count = usize::try_from(raw.unsigned_abs()).expect("u32 fits");
    let mut cursor = 4usize;
    let mut sizes = Vec::with_capacity(count.min(bytes.len()));
    if raw < 0 {
        if count > bytes.len().saturating_sub(cursor) {
            return Err(KlibIrDecodeError::malformed(
                entry,
                cursor,
                "entry count exceeds the possible varint size table",
            ));
        }
        for index in 0..count {
            let size = varint(bytes, &mut cursor, entry, 0, "entry size")?;
            sizes.push(usize::try_from(size).map_err(|_| {
                KlibIrDecodeError::malformed(
                    entry,
                    cursor,
                    format!("entry {index} size exceeds the host range"),
                )
            })?);
        }
    } else {
        let table_size = count.checked_mul(4).ok_or_else(|| {
            KlibIrDecodeError::malformed(entry, cursor, "entry size table overflows")
        })?;
        if cursor.saturating_add(table_size) > bytes.len() {
            return Err(KlibIrDecodeError::malformed(
                entry,
                cursor,
                "truncated entry size table",
            ));
        }
        for index in 0..count {
            sizes.push(
                usize::try_from(u32_at(bytes, cursor + index * 4, entry, "entry size")?)
                    .expect("u32 fits"),
            );
        }
        cursor += table_size;
    }
    let mut output = Vec::with_capacity(count);
    for (index, size) in sizes.into_iter().enumerate() {
        let end = cursor.checked_add(size).ok_or_else(|| {
            KlibIrDecodeError::malformed(entry, cursor, format!("entry {index} size overflows"))
        })?;
        output.push(bytes.get(cursor..end).ok_or_else(|| {
            KlibIrDecodeError::malformed(entry, cursor, format!("truncated entry {index}"))
        })?);
        cursor = end;
    }
    require_end(bytes, cursor, entry)?;
    Ok(output)
}

pub(super) fn declaration_index(bytes: &[u8]) -> Result<HashMap<u64, &[u8]>, KlibIrDecodeError> {
    if bytes.is_empty() {
        return Ok(HashMap::default());
    }
    let count = usize::try_from(u32_at(bytes, 0, "irDeclarations.knd", "declaration count")?)
        .expect("u32 fits");
    let header = 4usize
        .checked_add(count.checked_mul(12).ok_or_else(|| {
            KlibIrDecodeError::malformed("irDeclarations.knd", 0, "declaration index overflows")
        })?)
        .ok_or_else(|| {
            KlibIrDecodeError::malformed("irDeclarations.knd", 0, "declaration header overflows")
        })?;
    if header > bytes.len() {
        return Err(KlibIrDecodeError::malformed(
            "irDeclarations.knd",
            bytes.len(),
            "truncated declaration index",
        ));
    }
    let mut output = HashMap::with_capacity_and_hasher(count, Default::default());
    for index in 0..count {
        let at = 4 + index * 12;
        let id = u64::from(u32_at(bytes, at, "irDeclarations.knd", "declaration id")?);
        let offset = usize::try_from(u32_at(
            bytes,
            at + 4,
            "irDeclarations.knd",
            "declaration offset",
        )?)
        .expect("u32 fits");
        let size = usize::try_from(u32_at(
            bytes,
            at + 8,
            "irDeclarations.knd",
            "declaration size",
        )?)
        .expect("u32 fits");
        let end = offset.checked_add(size).ok_or_else(|| {
            KlibIrDecodeError::malformed(
                "irDeclarations.knd",
                at + 4,
                format!("declaration {id} range overflows"),
            )
        })?;
        let declaration = bytes.get(offset..end).ok_or_else(|| {
            KlibIrDecodeError::malformed(
                "irDeclarations.knd",
                offset,
                format!("truncated declaration {id}"),
            )
        })?;
        if output.insert(id, declaration).is_some() {
            return Err(KlibIrDecodeError::malformed(
                "irDeclarations.knd",
                at,
                format!("duplicate declaration id {id}"),
            ));
        }
    }
    Ok(output)
}

fn u32_at(
    bytes: &[u8],
    offset: usize,
    entry: &str,
    context: &str,
) -> Result<u32, KlibIrDecodeError> {
    let end = offset.checked_add(4).ok_or_else(|| {
        KlibIrDecodeError::malformed(entry, offset, format!("{context} offset overflows"))
    })?;
    let value = bytes.get(offset..end).ok_or_else(|| {
        KlibIrDecodeError::malformed(entry, offset, format!("truncated {context}"))
    })?;
    Ok(u32::from_be_bytes(
        value.try_into().expect("four bytes were read"),
    ))
}

fn require_end(bytes: &[u8], offset: usize, entry: &str) -> Result<(), KlibIrDecodeError> {
    if offset == bytes.len() {
        Ok(())
    } else {
        Err(KlibIrDecodeError::malformed(
            entry,
            offset,
            format!("{} trailing bytes", bytes.len() - offset),
        ))
    }
}
