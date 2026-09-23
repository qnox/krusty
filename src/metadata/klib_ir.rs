//! Target-neutral decoding of constant parameter defaults from serialized KLIB IR.
//!
//! KLIB `linkdata` states that a parameter has a default, but the expression that supplies that
//! default lives under `default/ir/`. This boundary decodes only literal defaults. Other expression
//! shapes remain unavailable, so a provider can decline a call instead of guessing how to realize
//! it.
//!
//! Declarations are joined to metadata exclusively through their public [`KlibPublicIdSignature`].
//! Source spellings, owner strings, and parameter names are never used as a substitute identity.

use std::collections::HashMap;

use crate::klib::{KlibArchive, KlibError};

use super::id_signature::{
    decode_public_id_signature, KlibIdSignatureDecodeError, KlibPublicIdSignature,
};

const IR_PREFIX: &str = "default/ir/";

/// One literal expression stored as a parameter default in serialized KLIB IR.
#[derive(Clone, Debug, PartialEq)]
pub enum KlibIrConstant {
    Null,
    Boolean(bool),
    Char(u16),
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    String(String),
}

/// Constant defaults indexed by the declaration identity serialized in the same IR file.
#[derive(Default)]
pub struct KlibIrDefaults {
    values: HashMap<KlibPublicIdSignature, Vec<Option<KlibIrConstant>>>,
}

impl KlibIrDefaults {
    /// Defaults parallel to a declaration's regular value parameters.
    ///
    /// `None` means either that the parameter has no default or that its default is not a literal.
    /// An absent declaration means that no public, exact IR identity supplied any defaults.
    pub fn get(&self, signature: &KlibPublicIdSignature) -> Option<&[Option<KlibIrConstant>]> {
        self.values.get(signature).map(Vec::as_slice)
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    fn record(
        &mut self,
        signature: KlibPublicIdSignature,
        values: Vec<Option<KlibIrConstant>>,
    ) -> Result<(), KlibIrDecodeError> {
        match self.values.get(&signature) {
            None => {
                self.values.insert(signature, values);
                Ok(())
            }
            Some(existing) if existing == &values => Ok(()),
            Some(_) => Err(KlibIrDecodeError::malformed(
                "irDeclarations.knd",
                0,
                "one public IdSignature carries conflicting default expressions",
            )),
        }
    }
}

/// Failure to read the serialized-IR tables or decode one of their entries.
#[derive(Debug)]
pub enum KlibIrDecodeError {
    Missing {
        entry: String,
    },
    Container(KlibError),
    Malformed {
        entry: String,
        offset: usize,
        detail: String,
    },
}

impl KlibIrDecodeError {
    fn malformed(entry: impl Into<String>, offset: usize, detail: impl Into<String>) -> Self {
        Self::Malformed {
            entry: entry.into(),
            offset,
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for KlibIrDecodeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing { entry } => {
                write!(formatter, "the KLIB has no serialized IR at {entry}")
            }
            Self::Container(error) => write!(formatter, "invalid KLIB IR container: {error}"),
            Self::Malformed {
                entry,
                offset,
                detail,
            } => write!(
                formatter,
                "invalid KLIB IR {entry} at byte {offset}: {detail}"
            ),
        }
    }
}

impl std::error::Error for KlibIrDecodeError {}

impl From<KlibError> for KlibIrDecodeError {
    fn from(error: KlibError) -> Self {
        Self::Container(error)
    }
}

/// Decode every literal parameter default published by a KLIB's serialized IR.
pub fn read_constant_defaults(archive: &KlibArchive) -> Result<KlibIrDefaults, KlibIrDecodeError> {
    let files = read_per_file_table(archive, "files.knf")?;
    let strings = read_per_file_table(archive, "strings.knt")?;
    let signatures = read_per_file_table(archive, "signatures.knt")?;
    let declarations = read_per_file_table(archive, "irDeclarations.knd")?;
    let bodies = read_per_file_table(archive, "bodies.knb")?;
    let expected = files.len();
    if [
        strings.len(),
        signatures.len(),
        declarations.len(),
        bodies.len(),
    ]
    .into_iter()
    .any(|actual| actual != expected)
    {
        return Err(KlibIrDecodeError::malformed(
            "default/ir",
            0,
            format!(
                "per-file table counts disagree: files={expected}, strings={}, signatures={}, declarations={}, bodies={}",
                strings.len(),
                signatures.len(),
                declarations.len(),
                bodies.len()
            ),
        ));
    }

    let mut defaults = KlibIrDefaults::default();
    for index in 0..expected {
        let string_entries = entries(&strings[index], "strings.knt")?;
        let strings = string_entries
            .into_iter()
            .enumerate()
            .map(|(string, bytes)| {
                std::str::from_utf8(bytes)
                    .map(str::to_owned)
                    .map_err(|error| {
                        KlibIrDecodeError::malformed(
                            "strings.knt",
                            error.valid_up_to(),
                            format!("string {string} is not UTF-8"),
                        )
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let signatures = entries(&signatures[index], "signatures.knt")?;
        let bodies = entries(&bodies[index], "bodies.knb")?;
        let declarations = declaration_index(&declarations[index])?;
        let file = IrFile {
            strings: &strings,
            signatures: &signatures,
            bodies: &bodies,
        };
        let ids = packed_field(&files[index], "files.knf", 1, "declaration ids")?;
        let mut seen = std::collections::HashSet::new();
        for id in ids {
            if !seen.insert(id) {
                return Err(KlibIrDecodeError::malformed(
                    "files.knf",
                    0,
                    format!("duplicate top-level declaration id {id}"),
                ));
            }
            let declaration = declarations.get(&id).ok_or_else(|| {
                KlibIrDecodeError::malformed(
                    "files.knf",
                    0,
                    format!("references absent declaration id {id}"),
                )
            })?;
            decode_declaration(&file, declaration, &mut defaults)?;
        }
    }
    Ok(defaults)
}

struct IrFile<'a> {
    strings: &'a [String],
    signatures: &'a [&'a [u8]],
    bodies: &'a [&'a [u8]],
}

impl IrFile<'_> {
    fn public_signature(
        &self,
        symbol: u64,
    ) -> Result<Option<KlibPublicIdSignature>, KlibIrDecodeError> {
        let raw_index = symbol >> 8;
        let index = usize::try_from(raw_index).map_err(|_| {
            KlibIrDecodeError::malformed(
                "signatures.knt",
                0,
                format!("signature index {raw_index} exceeds the host range"),
            )
        })?;
        let bytes = self.signatures.get(index).ok_or_else(|| {
            KlibIrDecodeError::malformed(
                "signatures.knt",
                0,
                format!("declaration symbol references absent signature {index}"),
            )
        })?;
        decode_public_id_signature(bytes, self.strings)
            .map_err(|error| signature_error(index, error))
    }
}

fn signature_error(index: usize, error: KlibIdSignatureDecodeError) -> KlibIrDecodeError {
    KlibIrDecodeError::malformed(
        "signatures.knt",
        error.offset(),
        format!("signature {index}: {}", error.detail()),
    )
}

fn decode_declaration(
    file: &IrFile<'_>,
    declaration: &[u8],
    defaults: &mut KlibIrDefaults,
) -> Result<(), KlibIrDecodeError> {
    for field in message(declaration, "irDeclarations.knd", 0)? {
        match field.number {
            // IrDeclaration.ir_class
            2 => {
                let class = field.bytes("class declaration", "irDeclarations.knd")?;
                for nested in message(class, "irDeclarations.knd", field.value_offset)? {
                    if nested.number == 5 {
                        decode_declaration(
                            file,
                            nested.bytes("nested declaration", "irDeclarations.knd")?,
                            defaults,
                        )?;
                    }
                }
            }
            // IrDeclaration.ir_constructor / ir_function
            3 | 6 => {
                let function = field.bytes("function declaration", "irDeclarations.knd")?;
                let base = required_bytes_field(
                    function,
                    "irDeclarations.knd",
                    field.value_offset,
                    1,
                    "function base",
                )?;
                decode_function(file, base, defaults)?;
            }
            // IrDeclaration.ir_property: defaults, if any, belong to its accessors.
            7 => {
                let property = field.bytes("property declaration", "irDeclarations.knd")?;
                for accessor in message(property, "irDeclarations.knd", field.value_offset)? {
                    if matches!(accessor.number, 4 | 5) {
                        let function = accessor.bytes("property accessor", "irDeclarations.knd")?;
                        let base = required_bytes_field(
                            function,
                            "irDeclarations.knd",
                            accessor.value_offset,
                            1,
                            "accessor function base",
                        )?;
                        decode_function(file, base, defaults)?;
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn decode_function(
    file: &IrFile<'_>,
    base: &[u8],
    defaults: &mut KlibIrDefaults,
) -> Result<(), KlibIrDecodeError> {
    let base_fields = message(base, "irDeclarations.knd", 0)?;
    let declaration_base = unique_bytes(&base_fields, 1, "declaration base", "irDeclarations.knd")?
        .ok_or_else(|| {
            KlibIrDecodeError::malformed(
                "irDeclarations.knd",
                0,
                "function has no declaration base",
            )
        })?;
    let declaration_fields = message(
        declaration_base.value,
        "irDeclarations.knd",
        declaration_base.value_offset,
    )?;
    let symbol = unique_varint(
        &declaration_fields,
        1,
        "declaration symbol",
        "irDeclarations.knd",
    )?
    .ok_or_else(|| {
        KlibIrDecodeError::malformed(
            "irDeclarations.knd",
            declaration_base.value_offset,
            "function declaration base has no symbol",
        )
    })?;

    let mut values = Vec::new();
    let mut has_default = false;
    for parameter in base_fields.iter().filter(|field| field.number == 6) {
        let parameter = parameter.bytes("regular value parameter", "irDeclarations.knd")?;
        let parameter_fields = message(parameter, "irDeclarations.knd", 0)?;
        let body = unique_varint(
            &parameter_fields,
            4,
            "parameter default body",
            "irDeclarations.knd",
        )?;
        let value = if let Some(body) = body {
            has_default = true;
            let index = usize::try_from(body).map_err(|_| {
                KlibIrDecodeError::malformed(
                    "bodies.knb",
                    0,
                    format!("default body index {body} exceeds the host range"),
                )
            })?;
            let body = file.bodies.get(index).ok_or_else(|| {
                KlibIrDecodeError::malformed(
                    "bodies.knb",
                    0,
                    format!("parameter references absent default body {index}"),
                )
            })?;
            decode_constant(file, body)?
        } else {
            None
        };
        values.push(value);
    }
    if !has_default {
        return Ok(());
    }
    let Some(signature) = file.public_signature(symbol)? else {
        return Ok(());
    };
    defaults.record(signature, values)
}

fn decode_constant(
    file: &IrFile<'_>,
    body: &[u8],
) -> Result<Option<KlibIrConstant>, KlibIrDecodeError> {
    let body_fields = message(body, "bodies.knb", 0)?;
    let Some(literal) = unique_bytes(&body_fields, 5, "constant expression", "bodies.knb")? else {
        return Ok(None);
    };
    let fields = message(literal.value, "bodies.knb", literal.value_offset)?;
    let mut constant = None;
    for field in fields {
        let decoded = match field.number {
            1 => {
                field.varint("null constant", "bodies.knb")?;
                Some(KlibIrConstant::Null)
            }
            2 => {
                let value = field.varint("Boolean constant", "bodies.knb")?;
                if value > 1 {
                    return Err(KlibIrDecodeError::malformed(
                        "bodies.knb",
                        field.value_offset,
                        format!("Boolean constant has value {value}"),
                    ));
                }
                Some(KlibIrConstant::Boolean(value != 0))
            }
            3 => {
                let value = field.varint("Char constant", "bodies.knb")? as i64 as i32;
                let value = u16::try_from(value).map_err(|_| {
                    KlibIrDecodeError::malformed(
                        "bodies.knb",
                        field.value_offset,
                        format!("Char constant is outside the unsigned 16-bit range: {value}"),
                    )
                })?;
                Some(KlibIrConstant::Char(value))
            }
            4 => Some(KlibIrConstant::Byte(
                field.varint("Byte constant", "bodies.knb")? as i64 as i8,
            )),
            5 => Some(KlibIrConstant::Short(
                field.varint("Short constant", "bodies.knb")? as i64 as i16,
            )),
            6 => Some(KlibIrConstant::Int(
                field.varint("Int constant", "bodies.knb")? as i64 as i32,
            )),
            7 => Some(KlibIrConstant::Long(
                field.varint("Long constant", "bodies.knb")? as i64,
            )),
            8 => Some(KlibIrConstant::Float(f32::from_bits(
                field.fixed32("Float constant", "bodies.knb")?,
            ))),
            9 => Some(KlibIrConstant::Double(f64::from_bits(
                field.fixed64("Double constant", "bodies.knb")?,
            ))),
            10 => {
                let raw = field.varint("String constant", "bodies.knb")?;
                let index = usize::try_from(raw).map_err(|_| {
                    KlibIrDecodeError::malformed(
                        "strings.knt",
                        field.value_offset,
                        format!("constant string index {raw} exceeds the host range"),
                    )
                })?;
                let value = file.strings.get(index).cloned().ok_or_else(|| {
                    KlibIrDecodeError::malformed(
                        "strings.knt",
                        field.value_offset,
                        format!("constant references absent string {index}"),
                    )
                })?;
                Some(KlibIrConstant::String(value))
            }
            _ => None,
        };
        if let Some(decoded) = decoded {
            if constant.replace(decoded).is_some() {
                return Err(KlibIrDecodeError::malformed(
                    "bodies.knb",
                    field.offset,
                    "constant expression contains more than one literal kind",
                ));
            }
        }
    }
    Ok(constant)
}

#[derive(Clone, Copy)]
enum WireValue<'a> {
    Varint(u64),
    Fixed32(u32),
    Fixed64(u64),
    Bytes(&'a [u8]),
}

#[derive(Clone, Copy)]
struct WireField<'a> {
    number: u64,
    offset: usize,
    value_offset: usize,
    value: WireValue<'a>,
}

impl<'a> WireField<'a> {
    fn bytes(&self, context: &str, entry: &str) -> Result<&'a [u8], KlibIrDecodeError> {
        match self.value {
            WireValue::Bytes(value) => Ok(value),
            _ => Err(wire_error(entry, self.offset, context, 2)),
        }
    }

    fn varint(&self, context: &str, entry: &str) -> Result<u64, KlibIrDecodeError> {
        match self.value {
            WireValue::Varint(value) => Ok(value),
            _ => Err(wire_error(entry, self.offset, context, 0)),
        }
    }

    fn fixed32(&self, context: &str, entry: &str) -> Result<u32, KlibIrDecodeError> {
        match self.value {
            WireValue::Fixed32(value) => Ok(value),
            _ => Err(wire_error(entry, self.offset, context, 5)),
        }
    }

    fn fixed64(&self, context: &str, entry: &str) -> Result<u64, KlibIrDecodeError> {
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

fn message<'a>(
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

fn required_bytes_field<'a>(
    bytes: &'a [u8],
    entry: &str,
    base: usize,
    number: u64,
    context: &str,
) -> Result<&'a [u8], KlibIrDecodeError> {
    unique_bytes_field(bytes, entry, base, number, context)?
        .ok_or_else(|| KlibIrDecodeError::malformed(entry, base, format!("missing {context}")))
}

fn unique_bytes<'a>(
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

struct BytesField<'a> {
    value: &'a [u8],
    value_offset: usize,
}

fn unique_varint(
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

fn packed_field(
    bytes: &[u8],
    entry: &str,
    number: u64,
    context: &str,
) -> Result<Vec<u64>, KlibIrDecodeError> {
    let mut values = Vec::new();
    for field in message(bytes, entry, 0)?
        .into_iter()
        .filter(|field| field.number == number)
    {
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

fn read_per_file_table(
    archive: &KlibArchive,
    name: &str,
) -> Result<Vec<Vec<u8>>, KlibIrDecodeError> {
    let entry = format!("{IR_PREFIX}{name}");
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

fn entries<'a>(bytes: &'a [u8], entry: &str) -> Result<Vec<&'a [u8]>, KlibIrDecodeError> {
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

fn declaration_index(bytes: &[u8]) -> Result<HashMap<u64, &[u8]>, KlibIrDecodeError> {
    if bytes.is_empty() {
        return Ok(HashMap::new());
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
    let mut output = HashMap::with_capacity(count);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn push_varint(mut value: u64, into: &mut Vec<u8>) {
        loop {
            let byte = (value & 0x7f) as u8;
            value >>= 7;
            into.push(byte | if value == 0 { 0 } else { 0x80 });
            if value == 0 {
                return;
            }
        }
    }

    fn varint_field(number: u64, value: u64) -> Vec<u8> {
        let mut bytes = Vec::new();
        push_varint(number << 3, &mut bytes);
        push_varint(value, &mut bytes);
        bytes
    }

    fn bytes_field(number: u64, value: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        push_varint((number << 3) | 2, &mut bytes);
        push_varint(value.len() as u64, &mut bytes);
        bytes.extend_from_slice(value);
        bytes
    }

    fn fixed64_field(number: u64, value: u64) -> Vec<u8> {
        let mut bytes = Vec::new();
        push_varint((number << 3) | 1, &mut bytes);
        bytes.extend_from_slice(&value.to_le_bytes());
        bytes
    }

    fn public_signature(package: u64, declaration: &[u64], member: u64) -> Vec<u8> {
        let mut package_path = Vec::new();
        push_varint(package, &mut package_path);
        let mut declaration_path = Vec::new();
        for segment in declaration {
            push_varint(*segment, &mut declaration_path);
        }
        let mut common = bytes_field(1, &package_path);
        common.extend(bytes_field(2, &declaration_path));
        common.extend(fixed64_field(6, member));
        bytes_field(1, &common)
    }

    fn function_with_int_default(symbol: u64, body: u64) -> Vec<u8> {
        let declaration_base = bytes_field(1, &varint_field(1, symbol));
        let parameter = bytes_field(6, &varint_field(4, body));
        let mut base = declaration_base;
        base.extend(parameter);
        bytes_field(6, &bytes_field(1, &base))
    }

    #[test]
    fn a_default_is_published_by_its_exact_public_signature() {
        let strings = vec![
            "fixture".to_string(),
            "Owner".to_string(),
            "compute".to_string(),
        ];
        let signatures = vec![public_signature(0, &[1, 2], 41)];
        let signatures = signatures.iter().map(Vec::as_slice).collect::<Vec<_>>();
        let literal = varint_field(6, 7);
        let bodies = vec![bytes_field(5, &literal)];
        let bodies = bodies.iter().map(Vec::as_slice).collect::<Vec<_>>();
        let file = IrFile {
            strings: &strings,
            signatures: &signatures,
            bodies: &bodies,
        };
        let mut defaults = KlibIrDefaults::default();
        decode_declaration(&file, &function_with_int_default(0, 0), &mut defaults).unwrap();

        let signature = decode_public_id_signature(signatures[0], &strings)
            .unwrap()
            .unwrap();
        assert_eq!(
            defaults.get(&signature),
            Some([Some(KlibIrConstant::Int(7))].as_slice())
        );
    }

    #[test]
    fn equal_member_ids_do_not_alias_different_declaration_paths() {
        let strings = vec![
            "fixture".to_string(),
            "First".to_string(),
            "Second".to_string(),
        ];
        let first = public_signature(0, &[1], 9);
        let second = public_signature(0, &[2], 9);
        let first = decode_public_id_signature(&first, &strings)
            .unwrap()
            .unwrap();
        let second = decode_public_id_signature(&second, &strings)
            .unwrap()
            .unwrap();
        let mut defaults = KlibIrDefaults::default();
        defaults
            .record(first.clone(), vec![Some(KlibIrConstant::Int(1))])
            .unwrap();
        defaults
            .record(second.clone(), vec![Some(KlibIrConstant::Int(2))])
            .unwrap();
        assert_eq!(
            defaults.get(&first),
            Some([Some(KlibIrConstant::Int(1))].as_slice())
        );
        assert_eq!(
            defaults.get(&second),
            Some([Some(KlibIrConstant::Int(2))].as_slice())
        );
    }

    #[test]
    fn malformed_default_body_is_not_silently_absent() {
        let strings = vec!["fixture".to_string(), "compute".to_string()];
        let signature = public_signature(0, &[1], 3);
        let signatures = [signature.as_slice()];
        let truncated = [0x2a, 0x02, 0x30];
        let bodies = [truncated.as_slice()];
        let file = IrFile {
            strings: &strings,
            signatures: &signatures,
            bodies: &bodies,
        };
        let mut defaults = KlibIrDefaults::default();
        let error =
            decode_declaration(&file, &function_with_int_default(0, 0), &mut defaults).unwrap_err();
        assert_eq!(
            error.to_string(),
            "invalid KLIB IR bodies.knb at byte 2: truncated field payload"
        );
        assert!(defaults.is_empty());
    }

    #[test]
    fn absent_signature_index_is_an_error_not_a_spelling_fallback() {
        let strings = vec!["fixture".to_string()];
        let bodies = vec![bytes_field(5, &varint_field(6, 1))];
        let bodies = bodies.iter().map(Vec::as_slice).collect::<Vec<_>>();
        let file = IrFile {
            strings: &strings,
            signatures: &[],
            bodies: &bodies,
        };
        let mut defaults = KlibIrDefaults::default();
        let error = decode_declaration(&file, &function_with_int_default(7 << 8, 0), &mut defaults)
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "invalid KLIB IR signatures.knt at byte 0: declaration symbol references absent signature 7"
        );
        assert!(defaults.is_empty());
    }
}
