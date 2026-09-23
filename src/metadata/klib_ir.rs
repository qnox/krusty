//! Target-neutral decoding of declaration bodies from serialized KLIB IR.
//!
//! KLIB `linkdata` states that a parameter has a default, but the expression that supplies that
//! default lives under `default/ir/`. This boundary decodes only literal defaults. Other expression
//! shapes remain unavailable, so a provider can decline a call instead of guessing how to realize
//! it. The same boundary recognizes the small inline-body shape that invokes one of the
//! declaration's own parameters. It preserves the invoked callable's exact public identity; a
//! provider must join that identity to metadata before publishing a normalized inline plan.
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

/// One declaration-owned inline body that consists solely of invoking one of the declaration's
/// own parameters, optionally followed by returning another parameter.
///
/// The decoder does not infer `FunctionN.invoke` from a name. It retains the call's exact public
/// identity so the provider can validate the selected callable through ordinary metadata. Parameter
/// ordinals follow call-site order: an extension receiver is first, followed by regular parameters.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KlibIrInlineBody {
    callee: KlibPublicIdSignature,
    lambda_parameter: usize,
    arguments: Box<[usize]>,
    result: Option<usize>,
}

impl KlibIrInlineBody {
    pub fn callee(&self) -> &KlibPublicIdSignature {
        &self.callee
    }

    pub fn lambda_parameter(&self) -> usize {
        self.lambda_parameter
    }

    pub fn arguments(&self) -> &[usize] {
        &self.arguments
    }

    pub fn result(&self) -> Option<usize> {
        self.result
    }
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

/// Supported inline bodies indexed by their declaration's exact serialized identity.
#[derive(Default)]
pub struct KlibIrInlineBodies {
    values: HashMap<KlibPublicIdSignature, KlibIrInlineBody>,
}

impl KlibIrInlineBodies {
    pub fn get(&self, signature: &KlibPublicIdSignature) -> Option<&KlibIrInlineBody> {
        self.values.get(signature)
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
        body: KlibIrInlineBody,
    ) -> Result<(), KlibIrDecodeError> {
        match self.values.get(&signature) {
            None => {
                self.values.insert(signature, body);
                Ok(())
            }
            Some(existing) if existing == &body => Ok(()),
            Some(_) => Err(KlibIrDecodeError::malformed(
                "irDeclarations.knd",
                0,
                "one public IdSignature carries conflicting inline bodies",
            )),
        }
    }
}

/// Declaration expressions decoded from one KLIB, before provider normalization.
#[derive(Default)]
pub struct KlibIrBodies {
    defaults: KlibIrDefaults,
    inline: KlibIrInlineBodies,
}

impl KlibIrBodies {
    pub fn defaults(&self) -> &KlibIrDefaults {
        &self.defaults
    }

    pub fn inline_bodies(&self) -> &KlibIrInlineBodies {
        &self.inline
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

/// Decode the supported declaration expressions published by a KLIB's serialized IR.
pub fn read_declaration_bodies(archive: &KlibArchive) -> Result<KlibIrBodies, KlibIrDecodeError> {
    read_bodies(archive, DecodeMode::AllBodies)
}

/// Decode every literal parameter default published by a KLIB's serialized IR.
///
/// This focused #1091 entry point deliberately does not inspect inline bodies. A malformed inline
/// expression therefore cannot hide an otherwise valid default from a defaults-only consumer.
pub fn read_constant_defaults(archive: &KlibArchive) -> Result<KlibIrDefaults, KlibIrDecodeError> {
    Ok(read_bodies(archive, DecodeMode::DefaultsOnly)?.defaults)
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum DecodeMode {
    DefaultsOnly,
    AllBodies,
}

impl DecodeMode {
    fn reads_inline(self) -> bool {
        self == Self::AllBodies
    }
}

fn read_bodies(archive: &KlibArchive, mode: DecodeMode) -> Result<KlibIrBodies, KlibIrDecodeError> {
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

    let mut decoded = KlibIrBodies::default();
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
            decode_declaration(&file, declaration, mode, &mut decoded)?;
        }
    }
    Ok(decoded)
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
        self.public_signature_for(symbol, "declaration")
    }

    fn public_signature_for(
        &self,
        symbol: u64,
        context: &str,
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
                format!("{context} symbol references absent signature {index}"),
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
    mode: DecodeMode,
    decoded: &mut KlibIrBodies,
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
                            mode,
                            decoded,
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
                decode_function(
                    file,
                    base,
                    mode.reads_inline() && field.number == 6,
                    decoded,
                )?;
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
                        decode_function(file, base, false, decoded)?;
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
    decode_inline: bool,
    decoded: &mut KlibIrBodies,
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
    let mut parameter_symbols = Vec::new();
    let extension_receiver = if decode_inline {
        unique_bytes(&base_fields, 5, "extension receiver", "irDeclarations.knd")?
    } else {
        None
    };
    if let Some(receiver) = extension_receiver {
        if let Some(parameter) = declaration_symbol(receiver.value)? {
            parameter_symbols.push(parameter);
        }
    }
    let mut has_unmodelled_receiver = false;
    if decode_inline {
        for receiver in base_fields
            .iter()
            .filter(|field| matches!(field.number, 4 | 9))
        {
            receiver.bytes("dispatch or context receiver", "irDeclarations.knd")?;
            has_unmodelled_receiver = true;
        }
    }
    let mut has_default = false;
    for parameter in base_fields.iter().filter(|field| field.number == 6) {
        let parameter = parameter.bytes("regular value parameter", "irDeclarations.knd")?;
        let parameter_fields = message(parameter, "irDeclarations.knd", 0)?;
        if decode_inline {
            if let Some(parameter) = declaration_symbol(parameter)? {
                parameter_symbols.push(parameter);
            }
        }
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
    if has_default {
        let Some(signature) = file.public_signature(symbol)? else {
            return Ok(());
        };
        decoded.defaults.record(signature, values)?;
    }

    if !decode_inline || has_unmodelled_receiver {
        return Ok(());
    }
    let expected_parameters = usize::from(extension_receiver.is_some())
        + base_fields.iter().filter(|field| field.number == 6).count();
    if parameter_symbols.len() != expected_parameters {
        return Ok(());
    }
    let mut unique_parameters = std::collections::HashSet::new();
    if parameter_symbols
        .iter()
        .any(|symbol| !unique_parameters.insert(*symbol))
    {
        return Err(KlibIrDecodeError::malformed(
            "irDeclarations.knd",
            0,
            "inline declaration reuses a parameter symbol",
        ));
    }
    let Some(body) = decode_inline_body(file, &base_fields, symbol, &parameter_symbols)? else {
        return Ok(());
    };
    let Some(signature) = file.public_signature(symbol)? else {
        return Ok(());
    };
    decoded.inline.record(signature, body)
}

/// The symbol stored in an `IrDeclarationBase`, shared by functions and value parameters.
fn declaration_symbol(declaration: &[u8]) -> Result<Option<u64>, KlibIrDecodeError> {
    let fields = message(declaration, "irDeclarations.knd", 0)?;
    let Some(base) = unique_bytes(&fields, 1, "declaration base", "irDeclarations.knd")? else {
        return Ok(None);
    };
    let fields = message(base.value, "irDeclarations.knd", base.value_offset)?;
    unique_varint(&fields, 1, "declaration symbol", "irDeclarations.knd")
}

fn decode_inline_body(
    file: &IrFile<'_>,
    base_fields: &[WireField<'_>],
    declaration: u64,
    parameters: &[u64],
) -> Result<Option<KlibIrInlineBody>, KlibIrDecodeError> {
    let Some(raw_body) = unique_varint(base_fields, 7, "function body", "irDeclarations.knd")?
    else {
        return Ok(None);
    };
    let body_index = usize::try_from(raw_body).map_err(|_| {
        KlibIrDecodeError::malformed(
            "bodies.knb",
            0,
            format!("function body index {raw_body} exceeds the host range"),
        )
    })?;
    let body = file.bodies.get(body_index).ok_or_else(|| {
        KlibIrDecodeError::malformed(
            "bodies.knb",
            0,
            format!("function references absent body {body_index}"),
        )
    })?;
    let body_fields = message(body, "bodies.knb", 0)?;
    let Some(block) = unique_bytes(&body_fields, 4, "block body", "bodies.knb")? else {
        return Ok(None);
    };
    let block_fields = message(block.value, "bodies.knb", block.value_offset)?;
    let mut statements = Vec::new();
    for statement in block_fields.iter().filter(|field| field.number == 1) {
        statements.push(statement.bytes("block statement", "bodies.knb")?);
    }
    match statements.as_slice() {
        [statement] => {
            let Some(value) = returned_value(statement, declaration)? else {
                return Ok(None);
            };
            decode_invocation(file, value, parameters, None)
        }
        [invoke, tail] => {
            let Some(expression) = statement_expression(invoke)? else {
                return Ok(None);
            };
            let Some(value) = returned_value(tail, declaration)? else {
                return Ok(None);
            };
            let Some(symbol) = get_value(value)? else {
                return Ok(None);
            };
            let Some(result) = parameter_ordinal(parameters, symbol) else {
                return Ok(None);
            };
            decode_invocation(file, expression, parameters, Some(result))
        }
        _ => Ok(None),
    }
}

fn statement_expression<'a>(statement: &'a [u8]) -> Result<Option<&'a [u8]>, KlibIrDecodeError> {
    let fields = message(statement, "bodies.knb", 0)?;
    Ok(unique_bytes(&fields, 3, "statement expression", "bodies.knb")?.map(|field| field.value))
}

fn expression_operation<'a>(
    expression: &'a [u8],
) -> Result<Option<(u64, BytesField<'a>)>, KlibIrDecodeError> {
    let fields = message(expression, "bodies.knb", 0)?;
    let mut operation = None;
    for field in fields
        .iter()
        .filter(|field| (5..=44).contains(&field.number))
    {
        let value = field.bytes("expression operation", "bodies.knb")?;
        if operation
            .replace((
                field.number,
                BytesField {
                    value,
                    value_offset: field.value_offset,
                },
            ))
            .is_some()
        {
            return Err(KlibIrDecodeError::malformed(
                "bodies.knb",
                field.offset,
                "expression contains more than one operation",
            ));
        }
    }
    Ok(operation)
}

fn returned_value<'a>(
    statement: &'a [u8],
    declaration: u64,
) -> Result<Option<&'a [u8]>, KlibIrDecodeError> {
    let Some(expression) = statement_expression(statement)? else {
        return Ok(None);
    };
    let Some((operation, value)) = expression_operation(expression)? else {
        return Ok(None);
    };
    if operation != 12 {
        return Ok(None);
    }
    let fields = message(value.value, "bodies.knb", value.value_offset)?;
    if unique_varint(&fields, 1, "return target", "bodies.knb")? != Some(declaration) {
        return Ok(None);
    }
    Ok(unique_bytes(&fields, 2, "returned value", "bodies.knb")?.map(|field| field.value))
}

fn get_value(expression: &[u8]) -> Result<Option<u64>, KlibIrDecodeError> {
    let Some((operation, value)) = expression_operation(expression)? else {
        return Ok(None);
    };
    if operation != 6 {
        return Ok(None);
    }
    let fields = message(value.value, "bodies.knb", value.value_offset)?;
    unique_varint(&fields, 1, "read value", "bodies.knb")
}

fn parameter_ordinal(parameters: &[u64], symbol: u64) -> Option<usize> {
    parameters.iter().position(|parameter| *parameter == symbol)
}

fn decode_invocation(
    file: &IrFile<'_>,
    expression: &[u8],
    parameters: &[u64],
    result: Option<usize>,
) -> Result<Option<KlibIrInlineBody>, KlibIrDecodeError> {
    let Some((operation, call)) = expression_operation(expression)? else {
        return Ok(None);
    };
    if operation != 8 {
        return Ok(None);
    }
    let fields = message(call.value, "bodies.knb", call.value_offset)?;
    if fields.iter().any(|field| field.number == 3) {
        return Ok(None);
    }
    let Some(symbol) = unique_varint(&fields, 1, "called declaration", "bodies.knb")? else {
        return Ok(None);
    };
    let Some(callee) = file.public_signature_for(symbol, "inline call")? else {
        return Ok(None);
    };
    let mut arguments = Vec::new();
    for argument in fields.iter().filter(|field| field.number == 5) {
        let expression = argument.bytes("call argument", "bodies.knb")?;
        let Some(symbol) = get_value(expression)? else {
            return Ok(None);
        };
        let Some(parameter) = parameter_ordinal(parameters, symbol) else {
            return Ok(None);
        };
        arguments.push(parameter);
    }
    let Some((&lambda_parameter, arguments)) = arguments.split_first() else {
        return Ok(None);
    };
    Ok(Some(KlibIrInlineBody {
        callee,
        lambda_parameter,
        arguments: arguments.into(),
        result,
    }))
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

#[derive(Clone, Copy)]
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

    fn value_parameter(symbol: u64) -> Vec<u8> {
        bytes_field(1, &varint_field(1, symbol))
    }

    fn get_value_expression(symbol: u64) -> Vec<u8> {
        bytes_field(6, &varint_field(1, symbol))
    }

    fn call_expression(callee: u64, arguments: &[u64]) -> Vec<u8> {
        let mut call = varint_field(1, callee);
        for argument in arguments {
            call.extend(bytes_field(5, &get_value_expression(*argument)));
        }
        bytes_field(8, &call)
    }

    fn returned_statement(declaration: u64, value: &[u8]) -> Vec<u8> {
        let mut returned = varint_field(1, declaration);
        returned.extend(bytes_field(2, value));
        bytes_field(3, &bytes_field(12, &returned))
    }

    fn expression_statement(expression: &[u8]) -> Vec<u8> {
        bytes_field(3, expression)
    }

    fn block_body(statements: &[Vec<u8>]) -> Vec<u8> {
        let mut block = Vec::new();
        for statement in statements {
            block.extend(bytes_field(1, statement));
        }
        bytes_field(4, &block)
    }

    fn function_with_inline_body(
        symbol: u64,
        extension_receiver: Option<u64>,
        parameters: &[u64],
        body: u64,
    ) -> Vec<u8> {
        let mut base = bytes_field(1, &varint_field(1, symbol));
        if let Some(receiver) = extension_receiver {
            base.extend(bytes_field(5, &value_parameter(receiver)));
        }
        for parameter in parameters {
            base.extend(bytes_field(6, &value_parameter(*parameter)));
        }
        base.extend(varint_field(7, body));
        bytes_field(6, &bytes_field(1, &base))
    }

    fn function_with_default_and_inline_body(
        symbol: u64,
        parameter_symbol: u64,
        default_body: u64,
        inline_body: u64,
    ) -> Vec<u8> {
        let mut parameter = value_parameter(parameter_symbol);
        parameter.extend(varint_field(4, default_body));
        let mut base = bytes_field(1, &varint_field(1, symbol));
        base.extend(bytes_field(6, &parameter));
        base.extend(varint_field(7, inline_body));
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
        let mut decoded = KlibIrBodies::default();
        decode_declaration(
            &file,
            &function_with_int_default(0, 0),
            DecodeMode::DefaultsOnly,
            &mut decoded,
        )
        .unwrap();

        let signature = decode_public_id_signature(signatures[0], &strings)
            .unwrap()
            .unwrap();
        assert_eq!(
            decoded.defaults().get(&signature),
            Some([Some(KlibIrConstant::Int(7))].as_slice())
        );
    }

    #[test]
    fn defaults_only_does_not_inspect_a_malformed_inline_body() {
        let strings = vec![
            "fixture".to_string(),
            "Scope".to_string(),
            "around".to_string(),
        ];
        let signature = public_signature(0, &[1, 2], 41);
        let signatures = [signature.as_slice()];
        let bodies = vec![bytes_field(5, &varint_field(6, 7))];
        let bodies = bodies.iter().map(Vec::as_slice).collect::<Vec<_>>();
        let file = IrFile {
            strings: &strings,
            signatures: &signatures,
            bodies: &bodies,
        };
        let mut decoded = KlibIrBodies::default();
        decode_declaration(
            &file,
            &function_with_default_and_inline_body(0, 31, 0, 99),
            DecodeMode::DefaultsOnly,
            &mut decoded,
        )
        .unwrap();

        let signature = decode_public_id_signature(signatures[0], &strings)
            .unwrap()
            .unwrap();
        assert_eq!(
            decoded.defaults().get(&signature),
            Some([Some(KlibIrConstant::Int(7))].as_slice())
        );
        assert!(decoded.inline_bodies().is_empty());

        let mut all_bodies = KlibIrBodies::default();
        let error = decode_declaration(
            &file,
            &function_with_default_and_inline_body(0, 31, 0, 99),
            DecodeMode::AllBodies,
            &mut all_bodies,
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "invalid KLIB IR bodies.knb at byte 0: function references absent body 99"
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
    fn an_inline_body_retains_both_exact_public_signatures() {
        let strings = vec![
            "fixture".to_string(),
            "Scope".to_string(),
            "around".to_string(),
            "Callback".to_string(),
            "execute".to_string(),
        ];
        let signatures = vec![
            public_signature(0, &[1, 2], 41),
            public_signature(0, &[3, 4], 17),
        ];
        let signatures = signatures.iter().map(Vec::as_slice).collect::<Vec<_>>();
        let declaration = 0;
        let callee = 1 << 8;
        let receiver = 31;
        let lambda = 47;
        let bodies = vec![block_body(&[returned_statement(
            declaration,
            &call_expression(callee, &[lambda, receiver]),
        )])];
        let bodies = bodies.iter().map(Vec::as_slice).collect::<Vec<_>>();
        let file = IrFile {
            strings: &strings,
            signatures: &signatures,
            bodies: &bodies,
        };
        let mut decoded = KlibIrBodies::default();
        decode_declaration(
            &file,
            &function_with_inline_body(declaration, Some(receiver), &[lambda], 0),
            DecodeMode::AllBodies,
            &mut decoded,
        )
        .unwrap();

        let declaration = decode_public_id_signature(signatures[0], &strings)
            .unwrap()
            .unwrap();
        let callee = decode_public_id_signature(signatures[1], &strings)
            .unwrap()
            .unwrap();
        assert_eq!(
            decoded.inline_bodies().get(&declaration),
            Some(&KlibIrInlineBody {
                callee,
                lambda_parameter: 1,
                arguments: Box::new([0]),
                result: None,
            })
        );
    }

    #[test]
    fn same_spelling_signatures_do_not_alias_inline_bodies() {
        let strings = vec![
            "fixture".to_string(),
            "Scope".to_string(),
            "around".to_string(),
            "Callback".to_string(),
            "execute".to_string(),
        ];
        let signature_bytes = vec![
            public_signature(0, &[1, 2], 41),
            public_signature(0, &[3, 4], 17),
            public_signature(0, &[1, 2], 42),
            public_signature(0, &[3, 4], 18),
        ];
        let signatures = signature_bytes
            .iter()
            .map(Vec::as_slice)
            .collect::<Vec<_>>();
        let first_parameter = 31;
        let second_parameter = 47;
        let bodies = vec![
            block_body(&[returned_statement(
                0,
                &call_expression(1 << 8, &[first_parameter]),
            )]),
            block_body(&[
                expression_statement(&call_expression(3 << 8, &[second_parameter])),
                returned_statement(2 << 8, &get_value_expression(second_parameter)),
            ]),
        ];
        let bodies = bodies.iter().map(Vec::as_slice).collect::<Vec<_>>();
        let file = IrFile {
            strings: &strings,
            signatures: &signatures,
            bodies: &bodies,
        };
        let mut decoded = KlibIrBodies::default();
        decode_declaration(
            &file,
            &function_with_inline_body(0, None, &[first_parameter], 0),
            DecodeMode::AllBodies,
            &mut decoded,
        )
        .unwrap();
        decode_declaration(
            &file,
            &function_with_inline_body(2 << 8, None, &[second_parameter], 1),
            DecodeMode::AllBodies,
            &mut decoded,
        )
        .unwrap();

        let first = decode_public_id_signature(signatures[0], &strings)
            .unwrap()
            .unwrap();
        let first_callee = decode_public_id_signature(signatures[1], &strings)
            .unwrap()
            .unwrap();
        let second = decode_public_id_signature(signatures[2], &strings)
            .unwrap()
            .unwrap();
        let second_callee = decode_public_id_signature(signatures[3], &strings)
            .unwrap()
            .unwrap();
        assert_eq!(
            decoded.inline_bodies().get(&first),
            Some(&KlibIrInlineBody {
                callee: first_callee,
                lambda_parameter: 0,
                arguments: Box::new([]),
                result: None,
            })
        );
        assert_eq!(
            decoded.inline_bodies().get(&second),
            Some(&KlibIrInlineBody {
                callee: second_callee,
                lambda_parameter: 0,
                arguments: Box::new([]),
                result: Some(0),
            })
        );
        assert_eq!(decoded.inline_bodies().len(), 2);
    }

    #[test]
    fn an_inline_call_without_a_public_identity_is_not_guessed_from_its_shape() {
        let strings = vec![
            "fixture".to_string(),
            "Scope".to_string(),
            "around".to_string(),
        ];
        let signature_bytes = [public_signature(0, &[1, 2], 41), bytes_field(2, &[])];
        let signatures = signature_bytes
            .iter()
            .map(Vec::as_slice)
            .collect::<Vec<_>>();
        let parameter = 31;
        let bodies = vec![block_body(&[returned_statement(
            0,
            &call_expression(1 << 8, &[parameter]),
        )])];
        let bodies = bodies.iter().map(Vec::as_slice).collect::<Vec<_>>();
        let file = IrFile {
            strings: &strings,
            signatures: &signatures,
            bodies: &bodies,
        };
        let mut decoded = KlibIrBodies::default();
        decode_declaration(
            &file,
            &function_with_inline_body(0, None, &[parameter], 0),
            DecodeMode::AllBodies,
            &mut decoded,
        )
        .unwrap();
        assert!(decoded.inline_bodies().is_empty());
    }

    #[test]
    fn an_absent_inline_callee_signature_is_an_exact_error() {
        let strings = vec![
            "fixture".to_string(),
            "Scope".to_string(),
            "around".to_string(),
        ];
        let signature = public_signature(0, &[1, 2], 41);
        let signatures = [signature.as_slice()];
        let parameter = 31;
        let bodies = vec![block_body(&[returned_statement(
            0,
            &call_expression(7 << 8, &[parameter]),
        )])];
        let bodies = bodies.iter().map(Vec::as_slice).collect::<Vec<_>>();
        let file = IrFile {
            strings: &strings,
            signatures: &signatures,
            bodies: &bodies,
        };
        let mut decoded = KlibIrBodies::default();
        let error = decode_declaration(
            &file,
            &function_with_inline_body(0, None, &[parameter], 0),
            DecodeMode::AllBodies,
            &mut decoded,
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "invalid KLIB IR signatures.knt at byte 0: inline call symbol references absent signature 7"
        );
        assert!(decoded.inline_bodies().is_empty());
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
        let mut decoded = KlibIrBodies::default();
        let error = decode_declaration(
            &file,
            &function_with_int_default(0, 0),
            DecodeMode::DefaultsOnly,
            &mut decoded,
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "invalid KLIB IR bodies.knb at byte 2: truncated field payload"
        );
        assert!(decoded.defaults().is_empty());
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
        let mut decoded = KlibIrBodies::default();
        let error = decode_declaration(
            &file,
            &function_with_int_default(7 << 8, 0),
            DecodeMode::DefaultsOnly,
            &mut decoded,
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "invalid KLIB IR signatures.knt at byte 0: declaration symbol references absent signature 7"
        );
        assert!(decoded.defaults().is_empty());
    }
}
