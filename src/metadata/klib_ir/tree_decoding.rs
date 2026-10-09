//! Decoding whole declaration trees, bodies included, from a KLIB's serialized IR.
//!
//! [`read_declaration_trees`] reads every top-level declaration of every file and turns it into a
//! [`KlibIrDeclarationTree`]: the declaration, its members and every body below them, in one
//! arena. Decoding is total over the IR protobuf schema of Kotlin 2.4 and the pre-2.4 member-access
//! and operation layouts, so a library compiled by an older Kotlin decodes too. A field the schema
//! requires and the bytes lack, a reference to an absent table entry, or an unknown operation is
//! an error naming where it was found; nothing is skipped to make a body decode.
//!
//! Three kinds of serialized fact are deliberately not decoded yet, because nothing consumes them:
//! source coordinates, the annotations of declarations (a type's annotations are decoded) and the
//! file entries an inlined block records.

use std::collections::HashMap;
use std::ops::RangeInclusive;

use crate::klib::KlibArchive;

use super::symbols::{string, KlibIrSignature, KlibIrSymbol, SignatureTable};
use super::tree::{
    KlibIrArena, KlibIrArguments, KlibIrBody, KlibIrBranch, KlibIrCatch, KlibIrClass,
    KlibIrClassId, KlibIrEnumEntry, KlibIrExpr, KlibIrExprId, KlibIrExprKind, KlibIrField,
    KlibIrFunction, KlibIrFunctionId, KlibIrLocalDelegatedProperty, KlibIrLoop, KlibIrMember,
    KlibIrMemberAccess, KlibIrNullability, KlibIrParameter, KlibIrProperty, KlibIrStatement,
    KlibIrSyntheticBody, KlibIrType, KlibIrTypeArgument, KlibIrTypeId, KlibIrTypeOperator,
    KlibIrTypeParameter, KlibIrVarargElement, KlibIrVariable, KlibIrVariableId, KlibIrVariance,
};
use super::wire::{
    declaration_index, entries, message, packed_field, packed_values, read_per_file_table,
    unique_bytes, unique_varint, WireField,
};
use super::{decode_literal, file_strings, KlibIrDecodeError};

const BODIES: &str = "bodies.knb";
const DECLARATIONS: &str = "irDeclarations.knd";
const TYPES: &str = "types.knt";

/// Deepest expression or type nesting a body may have. Real bodies stay far below it; the bound
/// turns a corrupt, self-nesting message into an error instead of exhausting the stack.
const MAX_DEPTH: usize = 512;

/// One top-level declaration of a KLIB file and everything below it.
#[derive(Debug)]
pub struct KlibIrDeclarationTree {
    pub arena: KlibIrArena,
    pub declaration: KlibIrMember,
}

/// Every declaration tree of one KLIB, with its functions indexed by their linkable identity.
#[derive(Debug, Default)]
pub struct KlibIrModuleTrees {
    trees: Vec<KlibIrDeclarationTree>,
    functions: HashMap<KlibIrSignature, (usize, KlibIrFunctionId)>,
}

impl KlibIrModuleTrees {
    pub fn trees(&self) -> &[KlibIrDeclarationTree] {
        &self.trees
    }

    /// The function another module links to under `signature`, with the arena that owns it.
    /// Only public and public-accessor identities are indexed: a file-local identity has no
    /// meaning outside the tree that holds it.
    pub fn function(&self, signature: &KlibIrSignature) -> Option<(&KlibIrArena, &KlibIrFunction)> {
        let (tree, function) = *self.functions.get(signature)?;
        let arena = &self.trees[tree].arena;
        Some((arena, arena.function(function)))
    }

    pub fn function_count(&self) -> usize {
        self.functions.len()
    }

    fn index(&mut self) -> Result<(), KlibIrDecodeError> {
        for (tree_index, tree) in self.trees.iter().enumerate() {
            for (function_index, function) in tree.arena.functions.iter().enumerate() {
                if matches!(function.symbol.signature, KlibIrSignature::FileLocal { .. }) {
                    continue;
                }
                let id = KlibIrFunctionId(u32::try_from(function_index).expect("arena fits u32"));
                if let Some((previous, _)) = self
                    .functions
                    .insert(function.symbol.signature.clone(), (tree_index, id))
                {
                    return Err(KlibIrDecodeError::malformed(
                        DECLARATIONS,
                        0,
                        format!(
                            "declaration trees {previous} and {tree_index} both define {:?}",
                            function.symbol.signature
                        ),
                    ));
                }
            }
        }
        Ok(())
    }
}

/// Decode every top-level declaration tree a KLIB serializes.
pub fn read_declaration_trees(
    archive: &KlibArchive,
) -> Result<KlibIrModuleTrees, KlibIrDecodeError> {
    let files = read_per_file_table(archive, "files.knf")?;
    let strings = read_per_file_table(archive, "strings.knt")?;
    let signatures = read_per_file_table(archive, "signatures.knt")?;
    let declarations = read_per_file_table(archive, "irDeclarations.knd")?;
    let bodies = read_per_file_table(archive, "bodies.knb")?;
    let types = read_per_file_table(archive, "types.knt")?;
    let expected = files.len();
    let counts = [
        strings.len(),
        signatures.len(),
        declarations.len(),
        bodies.len(),
        types.len(),
    ];
    if counts.iter().any(|count| *count != expected) {
        return Err(KlibIrDecodeError::malformed(
            "default/ir",
            0,
            format!("per-file table counts disagree: files={expected}, others={counts:?}"),
        ));
    }

    let mut module = KlibIrModuleTrees::default();
    for index in 0..expected {
        let strings = file_strings(&strings[index])?;
        let signature_entries = entries(&signatures[index], "signatures.knt")?;
        let mut file = FileTables {
            strings: &strings,
            signatures: SignatureTable::new(
                u32::try_from(index).expect("file count fits u32"),
                &signature_entries,
                &strings,
            ),
            types: entries(&types[index], TYPES)?,
            bodies: entries(&bodies[index], BODIES)?,
        };
        let declarations = declaration_index(&declarations[index])?;
        for id in packed_field(&files[index], "files.knf", 1, "declaration ids")? {
            let bytes = declarations.get(&id).ok_or_else(|| {
                KlibIrDecodeError::malformed(
                    "files.knf",
                    0,
                    format!("references absent declaration id {id}"),
                )
            })?;
            let mut decoder = TreeDecoder::new(&mut file);
            let declaration = decoder.member(&Msg::parse(bytes, 0, DECLARATIONS)?)?;
            module.trees.push(KlibIrDeclarationTree {
                arena: decoder.arena,
                declaration,
            });
        }
    }
    module.index()?;
    Ok(module)
}

struct FileTables<'a> {
    strings: &'a [String],
    signatures: SignatureTable<'a>,
    types: Vec<&'a [u8]>,
    bodies: Vec<&'a [u8]>,
}

/// One decoded protobuf message and where it sits, for exact error offsets.
struct Msg<'a> {
    fields: Vec<WireField<'a>>,
    offset: usize,
    entry: &'static str,
}

impl<'a> Msg<'a> {
    fn parse(
        bytes: &'a [u8],
        offset: usize,
        entry: &'static str,
    ) -> Result<Self, KlibIrDecodeError> {
        Ok(Self {
            fields: message(bytes, entry, offset)?,
            offset,
            entry,
        })
    }

    fn error(&self, detail: impl Into<String>) -> KlibIrDecodeError {
        KlibIrDecodeError::malformed(self.entry, self.offset, detail)
    }

    fn message(&self, number: u64, context: &str) -> Result<Option<Msg<'a>>, KlibIrDecodeError> {
        unique_bytes(&self.fields, number, context, self.entry)?
            .map(|field| Msg::parse(field.value, field.value_offset, self.entry))
            .transpose()
    }

    fn required(&self, number: u64, context: &str) -> Result<Msg<'a>, KlibIrDecodeError> {
        self.message(number, context)?
            .ok_or_else(|| self.error(format!("missing {context}")))
    }

    fn messages(&self, number: u64, context: &str) -> Result<Vec<Msg<'a>>, KlibIrDecodeError> {
        self.fields
            .iter()
            .filter(|field| field.number == number)
            .map(|field| {
                Msg::parse(
                    field.bytes(context, self.entry)?,
                    field.value_offset,
                    self.entry,
                )
            })
            .collect()
    }

    fn varint(&self, number: u64, context: &str) -> Result<Option<u64>, KlibIrDecodeError> {
        unique_varint(&self.fields, number, context, self.entry)
    }

    fn required_varint(&self, number: u64, context: &str) -> Result<u64, KlibIrDecodeError> {
        self.varint(number, context)?
            .ok_or_else(|| self.error(format!("missing {context}")))
    }

    fn packed(&self, number: u64, context: &str) -> Result<Vec<u64>, KlibIrDecodeError> {
        packed_values(&self.fields, number, self.entry, context)
    }

    /// The one field of a `oneof` whose numbers are `numbers`, as a message.
    fn oneof(
        &self,
        numbers: RangeInclusive<u64>,
        context: &str,
    ) -> Result<Option<(u64, Msg<'a>)>, KlibIrDecodeError> {
        let mut found = None;
        for field in self
            .fields
            .iter()
            .filter(|field| numbers.contains(&field.number))
        {
            let value = Msg::parse(
                field.bytes(context, self.entry)?,
                field.value_offset,
                self.entry,
            )?;
            if found.replace((field.number, value)).is_some() {
                return Err(KlibIrDecodeError::malformed(
                    self.entry,
                    field.offset,
                    format!("{context} has more than one alternative"),
                ));
            }
        }
        Ok(found)
    }
}

/// A protobuf `int32` read through a varint: negative values arrive sign-extended to 64 bits.
fn int32(value: u64) -> i32 {
    value as i64 as i32
}

/// `BinaryLattice`: two ints interleaved bit by bit, the first in the even bits.
fn lattice(code: u64) -> (u64, u64) {
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
fn current_operation(legacy: u64) -> Option<u64> {
    const CURRENT: [u64; 39] = [
        10, 22, 8, 19, 21, 5, 23, 26, 25, 27, 39, 18, 20, 15, 17, 6, 28, 40, 12, 16, 7, 29, 30, 31,
        14, 32, 13, 24, 33, 34, 35, 9, 36, 37, 38, 11, 43, 41, 42,
    ];
    let index = usize::try_from(legacy.checked_sub(1)?).ok()?;
    CURRENT.get(index).copied()
}

/// Decodes one top-level declaration into a fresh arena.
struct TreeDecoder<'t, 'a> {
    file: &'t mut FileTables<'a>,
    arena: KlibIrArena,
    types: HashMap<usize, Option<KlibIrTypeId>>,
    depth: usize,
}

impl<'t, 'a> TreeDecoder<'t, 'a> {
    fn new(file: &'t mut FileTables<'a>) -> Self {
        Self {
            file,
            arena: KlibIrArena::default(),
            types: HashMap::new(),
            depth: 0,
        }
    }

    /// Run `decode` one nesting level deeper. The level is released on every exit, error or not.
    fn nested<T>(
        &mut self,
        at: &Msg<'_>,
        decode: impl FnOnce(&mut Self) -> Result<T, KlibIrDecodeError>,
    ) -> Result<T, KlibIrDecodeError> {
        if self.depth >= MAX_DEPTH {
            return Err(at.error(format!("nesting exceeds {MAX_DEPTH} levels")));
        }
        self.depth += 1;
        let result = decode(self);
        self.depth -= 1;
        result
    }

    fn string(&self, index: u64, context: &str) -> Result<String, KlibIrDecodeError> {
        string(self.file.strings, index, context)
    }

    fn optional_string(
        &self,
        msg: &Msg<'_>,
        number: u64,
        context: &str,
    ) -> Result<Option<String>, KlibIrDecodeError> {
        msg.varint(number, context)?
            .map(|index| self.string(index, context))
            .transpose()
    }

    fn symbol(&mut self, code: u64) -> Result<KlibIrSymbol, KlibIrDecodeError> {
        self.file.signatures.symbol(code)
    }

    fn required_symbol(
        &mut self,
        msg: &Msg<'_>,
        number: u64,
        context: &str,
    ) -> Result<KlibIrSymbol, KlibIrDecodeError> {
        let code = msg.required_varint(number, context)?;
        self.symbol(code)
    }

    fn optional_symbol(
        &mut self,
        msg: &Msg<'_>,
        number: u64,
        context: &str,
    ) -> Result<Option<KlibIrSymbol>, KlibIrDecodeError> {
        msg.varint(number, context)?
            .map(|code| self.symbol(code))
            .transpose()
    }

    // ---- types ------------------------------------------------------------------------------

    fn ty(&mut self, raw: u64) -> Result<KlibIrTypeId, KlibIrDecodeError> {
        let index = usize::try_from(raw)
            .ok()
            .filter(|index| *index < self.file.types.len())
            .ok_or_else(|| {
                KlibIrDecodeError::malformed(TYPES, 0, format!("references absent type {raw}"))
            })?;
        match self.types.get(&index) {
            Some(Some(id)) => return Ok(*id),
            Some(None) => {
                return Err(KlibIrDecodeError::malformed(
                    TYPES,
                    0,
                    format!("type {index} contains itself"),
                ))
            }
            None => {}
        }
        let msg = Msg::parse(self.file.types[index], 0, TYPES)?;
        self.types.insert(index, None);
        let ty = self.nested(&msg, |decoder| decoder.decode_type(&msg));
        let ty = match ty {
            Ok(ty) => ty,
            Err(error) => {
                self.types.remove(&index);
                return Err(error);
            }
        };
        let id = KlibIrTypeId(u32::try_from(self.arena.types.len()).expect("arena fits u32"));
        self.arena.types.push(ty);
        self.types.insert(index, Some(id));
        Ok(id)
    }

    fn decode_type(&mut self, msg: &Msg<'_>) -> Result<KlibIrType, KlibIrDecodeError> {
        let (kind, ty) = msg
            .oneof(1..=5, "IrType kind")?
            .ok_or_else(|| msg.error("IrType has no kind"))?;
        Ok(match kind {
            1 | 5 => {
                let classifier = self.required_symbol(&ty, 2, "type classifier")?;
                let nullability = if kind == 1 {
                    match ty.required_varint(3, "has_question_mark")? {
                        0 => KlibIrNullability::NotSpecified,
                        _ => KlibIrNullability::MarkedNullable,
                    }
                } else {
                    match ty.varint(3, "type nullability")?.unwrap_or(1) {
                        0 => KlibIrNullability::MarkedNullable,
                        1 => KlibIrNullability::NotSpecified,
                        2 => KlibIrNullability::DefinitelyNotNull,
                        other => return Err(ty.error(format!("unknown nullability {other}"))),
                    }
                };
                let mut arguments = Vec::new();
                for code in ty.packed(4, "type argument")? {
                    arguments.push(if code == 0 {
                        KlibIrTypeArgument::Star
                    } else {
                        let variance = match code & 3 {
                            1 => KlibIrVariance::Invariant,
                            2 => KlibIrVariance::In,
                            3 => KlibIrVariance::Out,
                            _ => return Err(ty.error("type argument has no variance")),
                        };
                        KlibIrTypeArgument::Type {
                            variance,
                            ty: self.ty(code >> 2)?,
                        }
                    });
                }
                let mut annotations = Vec::new();
                for annotation in ty.messages(1, "type annotation")? {
                    annotations.push(self.required_symbol(
                        &annotation,
                        1,
                        "annotation constructor",
                    )?);
                }
                KlibIrType::Simple {
                    classifier,
                    nullability,
                    arguments,
                    annotations,
                }
            }
            2 => KlibIrType::Dynamic,
            3 => KlibIrType::Error,
            4 => match ty.packed(1, "definitely-not-null operand")?.as_slice() {
                [operand] => KlibIrType::DefinitelyNotNull(self.ty(*operand)?),
                operands => {
                    return Err(ty.error(format!(
                        "definitely-not-null type has {} operands",
                        operands.len()
                    )))
                }
            },
            _ => unreachable!("the oneof range is 1..=5"),
        })
    }

    fn optional_type(
        &mut self,
        raw: Option<u64>,
    ) -> Result<Option<KlibIrTypeId>, KlibIrDecodeError> {
        match raw.map(int32) {
            None => Ok(None),
            Some(index) if index < 0 => Ok(None),
            Some(index) => self.ty(index as u64).map(Some),
        }
    }

    // ---- table entries ----------------------------------------------------------------------

    fn body_entry(&self, raw: u64, context: &str) -> Result<Msg<'a>, KlibIrDecodeError> {
        let bytes = usize::try_from(raw)
            .ok()
            .and_then(|index| self.file.bodies.get(index))
            .ok_or_else(|| {
                KlibIrDecodeError::malformed(
                    BODIES,
                    0,
                    format!("{context} references absent body {raw}"),
                )
            })?;
        Msg::parse(bytes, 0, BODIES)
    }

    fn expression_entry(
        &mut self,
        raw: u64,
        context: &str,
    ) -> Result<KlibIrExprId, KlibIrDecodeError> {
        let msg = self.body_entry(raw, context)?;
        self.expression(&msg)
    }

    fn statement_body_entry(
        &mut self,
        raw: u64,
        context: &str,
    ) -> Result<KlibIrBody, KlibIrDecodeError> {
        let msg = self.body_entry(raw, context)?;
        let (kind, body) = msg
            .oneof(2..=7, "body statement")?
            .ok_or_else(|| msg.error(format!("{context} is not a statement")))?;
        match kind {
            4 => Ok(KlibIrBody::Block(self.statements(&body, 1)?)),
            7 => Ok(KlibIrBody::Synthetic(
                match body.required_varint(1, "synthetic body kind")? {
                    1 => KlibIrSyntheticBody::EnumValues,
                    2 => KlibIrSyntheticBody::EnumValueOf,
                    3 => KlibIrSyntheticBody::EnumEntries,
                    other => return Err(body.error(format!("unknown synthetic body {other}"))),
                },
            )),
            _ => Err(body.error(format!("{context} is statement kind {kind}, not a body"))),
        }
    }

    // ---- expressions ------------------------------------------------------------------------

    fn push(&mut self, ty: Option<KlibIrTypeId>, kind: KlibIrExprKind) -> KlibIrExprId {
        let id = KlibIrExprId(u32::try_from(self.arena.exprs.len()).expect("arena fits u32"));
        self.arena.exprs.push(KlibIrExpr { ty, kind });
        id
    }

    fn expression(&mut self, msg: &Msg<'_>) -> Result<KlibIrExprId, KlibIrDecodeError> {
        self.nested(msg, |decoder| decoder.nested_expression(msg))
    }

    fn nested_expression(&mut self, msg: &Msg<'_>) -> Result<KlibIrExprId, KlibIrDecodeError> {
        let ty = self.optional_type(msg.varint(2, "expression type")?)?;
        let current = msg.oneof(5..=44, "IrExpression operation")?;
        let legacy = msg.message(1, "pre-2.4 operation")?;
        let (number, operation) = match (current, legacy) {
            (Some(current), None) => current,
            (None, Some(legacy)) => {
                let (number, operation) = legacy
                    .oneof(1..=39, "pre-2.4 operation")?
                    .ok_or_else(|| legacy.error("pre-2.4 operation has no alternative"))?;
                let number = current_operation(number)
                    .ok_or_else(|| legacy.error(format!("unknown operation {number}")))?;
                (number, operation)
            }
            (Some(_), Some(_)) => {
                return Err(msg.error("expression carries both operation layouts"));
            }
            (None, None) => return Err(msg.error("expression has no operation")),
        };
        let kind = self.operation(number, &operation)?;
        Ok(self.push(ty, kind))
    }

    fn optional_expression(
        &mut self,
        msg: &Msg<'_>,
        number: u64,
        context: &str,
    ) -> Result<Option<KlibIrExprId>, KlibIrDecodeError> {
        msg.message(number, context)?
            .map(|expression| self.expression(&expression))
            .transpose()
    }

    fn required_expression(
        &mut self,
        msg: &Msg<'_>,
        number: u64,
        context: &str,
    ) -> Result<KlibIrExprId, KlibIrDecodeError> {
        let expression = msg.required(number, context)?;
        self.expression(&expression)
    }

    fn expressions(
        &mut self,
        msg: &Msg<'_>,
        number: u64,
        context: &str,
    ) -> Result<Vec<KlibIrExprId>, KlibIrDecodeError> {
        msg.messages(number, context)?
            .iter()
            .map(|expression| self.expression(expression))
            .collect()
    }

    /// An argument slot: `IrMissingExpression` is an argument the call does not supply.
    fn argument(&mut self, msg: &Msg<'_>) -> Result<Option<KlibIrExprId>, KlibIrDecodeError> {
        let id = self.expression(msg)?;
        Ok(match self.arena.expr(id).kind {
            KlibIrExprKind::Missing => None,
            _ => Some(id),
        })
    }

    /// `NullableIrExpression`.
    fn nullable_argument(
        &mut self,
        msg: &Msg<'_>,
    ) -> Result<Option<KlibIrExprId>, KlibIrDecodeError> {
        msg.message(1, "nullable argument")?
            .map(|expression| self.argument(&expression))
            .transpose()
            .map(Option::flatten)
    }

    /// A member access in either layout. `legacy`, `arguments` and `type_arguments` are the field
    /// numbers this message kind gives its pre-2.4 member access, arguments and type arguments.
    fn access(
        &mut self,
        msg: &Msg<'_>,
        symbol: KlibIrSymbol,
        legacy: u64,
        arguments: u64,
        type_arguments: u64,
        origin: Option<u64>,
    ) -> Result<KlibIrMemberAccess, KlibIrDecodeError> {
        let current = msg.messages(arguments, "argument")?;
        let current_types = msg.packed(type_arguments, "type argument")?;
        let (arguments, types) = match msg.message(legacy, "pre-2.4 member access")? {
            None => {
                let mut values = Vec::with_capacity(current.len());
                for argument in &current {
                    values.push(self.argument(argument)?);
                }
                (KlibIrArguments::Flat(values), current_types)
            }
            Some(access) => {
                if !current.is_empty() || !current_types.is_empty() {
                    return Err(msg.error("member access carries both argument layouts"));
                }
                let flat = access.messages(6, "pre-2.4 argument")?;
                let regular = access.messages(3, "regular argument")?;
                let dispatch = access.message(1, "dispatch receiver")?;
                let extension = access.message(2, "extension receiver")?;
                let arguments = if flat.is_empty() {
                    let dispatch_receiver = dispatch.map(|d| self.expression(&d)).transpose()?;
                    let extension_receiver = extension.map(|e| self.expression(&e)).transpose()?;
                    let mut values = Vec::with_capacity(regular.len());
                    for argument in &regular {
                        values.push(self.nullable_argument(argument)?);
                    }
                    KlibIrArguments::Split {
                        dispatch_receiver,
                        extension_receiver,
                        values,
                    }
                } else {
                    if dispatch.is_some() || extension.is_some() || !regular.is_empty() {
                        return Err(access.error("pre-2.4 member access mixes argument layouts"));
                    }
                    let mut values = Vec::with_capacity(flat.len());
                    for argument in &flat {
                        values.push(self.nullable_argument(argument)?);
                    }
                    KlibIrArguments::Flat(values)
                };
                (arguments, access.packed(4, "type argument")?)
            }
        };
        let type_arguments = types
            .into_iter()
            .map(|raw| self.optional_type(Some(raw)))
            .collect::<Result<_, _>>()?;
        let origin = match origin {
            Some(number) => self.optional_string(msg, number, "origin")?,
            None => None,
        };
        Ok(KlibIrMemberAccess {
            symbol,
            arguments,
            type_arguments,
            origin,
        })
    }

    fn loop_(&mut self, msg: &Msg<'_>) -> Result<KlibIrLoop, KlibIrDecodeError> {
        let found = msg.required(1, "loop")?;
        let id = int32(found.required_varint(1, "loop id")?);
        Ok(KlibIrLoop {
            id: u32::try_from(id).map_err(|_| found.error(format!("negative loop id {id}")))?,
            condition: self.required_expression(&found, 2, "loop condition")?,
            label: self.optional_string(&found, 3, "loop label")?,
            body: self.optional_expression(&found, 4, "loop body")?,
            origin: self.optional_string(&found, 5, "loop origin")?,
        })
    }

    fn jump(&self, msg: &Msg<'_>) -> Result<(u32, Option<String>), KlibIrDecodeError> {
        let id = int32(msg.required_varint(1, "loop id")?);
        let id = u32::try_from(id).map_err(|_| msg.error(format!("negative loop id {id}")))?;
        Ok((id, self.optional_string(msg, 2, "loop label")?))
    }

    fn block(
        &mut self,
        msg: &Msg<'_>,
    ) -> Result<(Vec<KlibIrStatement>, Option<String>), KlibIrDecodeError> {
        Ok((
            self.statements(msg, 1)?,
            self.optional_string(msg, 2, "block origin")?,
        ))
    }

    fn operation(
        &mut self,
        number: u64,
        op: &Msg<'_>,
    ) -> Result<KlibIrExprKind, KlibIrDecodeError> {
        use KlibIrExprKind as K;
        Ok(match number {
            5 => K::Const(
                decode_literal(self.file.strings, op.fields.clone())?
                    .ok_or_else(|| op.error("constant has no literal"))?,
            ),
            6 => K::GetValue {
                symbol: self.required_symbol(op, 1, "read value")?,
                origin: self.optional_string(op, 2, "origin")?,
            },
            7 => K::SetValue {
                symbol: self.required_symbol(op, 1, "written value")?,
                value: self.required_expression(op, 2, "written value")?,
                origin: self.optional_string(op, 3, "origin")?,
            },
            8 => {
                let symbol = self.required_symbol(op, 1, "callee")?;
                K::Call {
                    access: self.access(op, symbol, 2, 5, 6, Some(4))?,
                    super_qualifier: self.optional_symbol(op, 3, "super qualifier")?,
                }
            }
            9 => {
                let symbol = self.required_symbol(op, 1, "constructor")?;
                let count = op.required_varint(2, "constructor type argument count")?;
                K::ConstructorCall {
                    access: self.access(op, symbol, 3, 5, 6, Some(4))?,
                    constructor_type_arguments: u32::try_from(int32(count))
                        .map_err(|_| op.error("negative constructor type argument count"))?,
                }
            }
            10 => {
                let (statements, origin) = self.block(op)?;
                K::Block { statements, origin }
            }
            11 => {
                let symbol = self.required_symbol(op, 1, "returnable block")?;
                let (statements, origin) = self.block(&op.required(2, "returnable block base")?)?;
                K::ReturnableBlock {
                    symbol,
                    statements,
                    origin,
                }
            }
            12 => K::Return {
                target: self.required_symbol(op, 1, "return target")?,
                value: self.required_expression(op, 2, "returned value")?,
            },
            13 => {
                let mut branches = Vec::new();
                for statement in op.messages(1, "when branch")? {
                    let branch = statement.required(5, "when branch")?;
                    branches.push(KlibIrBranch {
                        condition: self.required_expression(&branch, 1, "branch condition")?,
                        result: self.required_expression(&branch, 2, "branch result")?,
                    });
                }
                K::When {
                    branches,
                    origin: self.optional_string(op, 2, "origin")?,
                }
            }
            14 => K::TypeOperator {
                operator: match op.required_varint(1, "type operator")? {
                    1 => KlibIrTypeOperator::Cast,
                    2 => KlibIrTypeOperator::ImplicitCast,
                    3 => KlibIrTypeOperator::ImplicitNotNull,
                    4 => KlibIrTypeOperator::ImplicitCoercionToUnit,
                    5 => KlibIrTypeOperator::ImplicitIntegerCoercion,
                    6 => KlibIrTypeOperator::SafeCast,
                    7 => KlibIrTypeOperator::InstanceOf,
                    8 => KlibIrTypeOperator::NotInstanceOf,
                    9 => KlibIrTypeOperator::SamConversion,
                    10 => KlibIrTypeOperator::ImplicitDynamicCast,
                    11 => KlibIrTypeOperator::ReinterpretCast,
                    other => return Err(op.error(format!("unknown type operator {other}"))),
                },
                operand: self.ty(op.required_varint(2, "type operand")?)?,
                argument: self.required_expression(op, 3, "type operator argument")?,
            },
            15 => {
                let access = op.required(1, "field access")?;
                K::GetField {
                    symbol: self.required_symbol(&access, 1, "field")?,
                    super_qualifier: self.optional_symbol(&access, 2, "super qualifier")?,
                    receiver: self.optional_expression(&access, 3, "field receiver")?,
                    origin: self.optional_string(op, 2, "origin")?,
                }
            }
            16 => {
                let access = op.required(1, "field access")?;
                K::SetField {
                    symbol: self.required_symbol(&access, 1, "field")?,
                    super_qualifier: self.optional_symbol(&access, 2, "super qualifier")?,
                    receiver: self.optional_expression(&access, 3, "field receiver")?,
                    value: self.required_expression(op, 2, "field value")?,
                    origin: self.optional_string(op, 3, "origin")?,
                }
            }
            17 => K::GetObject(self.required_symbol(op, 1, "object")?),
            18 => K::GetClass(self.required_expression(op, 1, "class of")?),
            19 => K::ClassReference {
                class: self.required_symbol(op, 1, "referenced class")?,
                class_type: self.ty(op.required_varint(2, "class reference type")?)?,
            },
            20 => K::GetEnumValue(self.required_symbol(op, 1, "enum entry")?),
            21 => {
                let (statements, origin) = self.block(op)?;
                K::Composite { statements, origin }
            }
            22 => {
                let (loop_id, label) = self.jump(op)?;
                K::Break { loop_id, label }
            }
            23 => {
                let (loop_id, label) = self.jump(op)?;
                K::Continue { loop_id, label }
            }
            24 => K::While(self.loop_(op)?),
            25 => K::DoWhile(self.loop_(op)?),
            26 | 27 => {
                let symbol = self.required_symbol(op, 1, "constructor")?;
                let access = self.access(op, symbol, 2, 3, 4, None)?;
                if number == 26 {
                    K::DelegatingConstructorCall(access)
                } else {
                    K::EnumConstructorCall(access)
                }
            }
            28 => K::InstanceInitializerCall(self.required_symbol(op, 1, "initialized class")?),
            29 => K::StringConcat(self.expressions(op, 1, "concatenated value")?),
            30 => K::Throw(self.required_expression(op, 1, "thrown value")?),
            31 => {
                let result = self.required_expression(op, 1, "try result")?;
                let mut catches = Vec::new();
                for statement in op.messages(2, "catch")? {
                    let catch = statement.required(6, "catch")?;
                    let parameter = self.variable(&catch.required(1, "catch parameter")?)?;
                    catches.push(KlibIrCatch {
                        parameter,
                        result: self.required_expression(&catch, 2, "catch result")?,
                    });
                }
                K::Try {
                    result,
                    catches,
                    finally: self.optional_expression(op, 3, "finally")?,
                }
            }
            32 => {
                let element_type = self.ty(op.required_varint(1, "vararg element type")?)?;
                let mut elements = Vec::new();
                for element in op.messages(2, "vararg element")? {
                    let (kind, value) = element
                        .oneof(1..=2, "vararg element")?
                        .ok_or_else(|| element.error("vararg element is empty"))?;
                    elements.push(if kind == 1 {
                        KlibIrVarargElement::Expression(self.expression(&value)?)
                    } else {
                        KlibIrVarargElement::Spread(self.required_expression(&value, 1, "spread")?)
                    });
                }
                K::Vararg {
                    element_type,
                    elements,
                }
            }
            33 => K::DynamicMember {
                member: self.string(op.required_varint(1, "dynamic member")?, "dynamic member")?,
                receiver: self.required_expression(op, 2, "dynamic receiver")?,
            },
            34 => K::DynamicOperator {
                operator: u32::try_from(op.required_varint(1, "dynamic operator")?)
                    .map_err(|_| op.error("dynamic operator exceeds u32"))?,
                receiver: self.required_expression(op, 2, "dynamic receiver")?,
                arguments: self.expressions(op, 3, "dynamic argument")?,
            },
            35 => K::LocalDelegatedPropertyReference {
                delegate: self.optional_symbol(op, 1, "delegate")?,
                getter: self.optional_symbol(op, 2, "getter")?,
                setter: self.optional_symbol(op, 3, "setter")?,
                symbol: self.required_symbol(op, 4, "local delegated property")?,
                origin: self.optional_string(op, 5, "origin")?,
            },
            36 => K::FunctionExpression {
                function: self.function(&op.required(1, "function expression")?, false)?,
                origin: self.optional_string(op, 2, "origin")?,
            },
            37 => K::Error {
                description: self.string(op.required_varint(1, "error")?, "error description")?,
            },
            38 => K::ErrorCall {
                description: self.string(op.required_varint(1, "error")?, "error description")?,
                receiver: self.optional_expression(op, 2, "error receiver")?,
                arguments: self.expressions(op, 3, "error argument")?,
            },
            39 => {
                let symbol = self.required_symbol(op, 1, "referenced function")?;
                K::FunctionReference {
                    access: self.access(op, symbol, 3, 5, 6, Some(2))?,
                    reflection_target: self.optional_symbol(op, 4, "reflection target")?,
                }
            }
            40 => {
                let symbol = self.required_symbol(op, 6, "referenced property")?;
                K::PropertyReference {
                    access: self.access(op, symbol, 5, 7, 8, Some(4))?,
                    field: self.optional_symbol(op, 1, "field")?,
                    getter: self.optional_symbol(op, 2, "getter")?,
                    setter: self.optional_symbol(op, 3, "setter")?,
                }
            }
            41 => K::RichFunctionReference {
                bound_values: self.expressions(op, 1, "bound value")?,
                reflection_target: self.optional_symbol(op, 2, "reflection target")?,
                overridden: self.required_symbol(op, 3, "overridden function")?,
                invoke: self.function(&op.required(4, "invoke function")?, false)?,
                flags: op.varint(5, "flags")?.unwrap_or(0),
                origin: self.optional_string(op, 6, "origin")?,
            },
            42 => K::RichPropertyReference {
                bound_values: self.expressions(op, 1, "bound value")?,
                reflection_target: self.optional_symbol(op, 2, "reflection target")?,
                getter: self.function(&op.required(3, "getter function")?, false)?,
                setter: op
                    .message(4, "setter function")?
                    .map(|setter| self.function(&setter, false))
                    .transpose()?,
                origin: self.optional_string(op, 5, "origin")?,
            },
            43 => {
                let inlined_function = self.optional_symbol(op, 1, "inlined function")?;
                let (statements, origin) = self.block(&op.required(3, "inlined block base")?)?;
                K::InlinedFunctionBlock {
                    inlined_function,
                    statements,
                    origin,
                }
            }
            44 => K::Missing,
            other => return Err(op.error(format!("unknown operation {other}"))),
        })
    }

    // ---- statements -------------------------------------------------------------------------

    fn statements(
        &mut self,
        msg: &Msg<'_>,
        number: u64,
    ) -> Result<Vec<KlibIrStatement>, KlibIrDecodeError> {
        msg.messages(number, "statement")?
            .iter()
            .map(|statement| self.statement(statement))
            .collect()
    }

    fn statement(&mut self, msg: &Msg<'_>) -> Result<KlibIrStatement, KlibIrDecodeError> {
        let (kind, value) = msg
            .oneof(2..=7, "IrStatement")?
            .ok_or_else(|| msg.error("statement is empty"))?;
        match kind {
            2 => {
                let (kind, declaration) = value
                    .oneof(1..=12, "IrDeclaration")?
                    .ok_or_else(|| value.error("declaration is empty"))?;
                Ok(match kind {
                    2 => KlibIrStatement::Class(self.class(&declaration)?),
                    6 => KlibIrStatement::Function(self.function(&declaration, false)?),
                    9 => KlibIrStatement::Variable(self.variable(&declaration)?),
                    11 => KlibIrStatement::LocalDelegatedProperty(
                        self.local_delegated_property(&declaration)?,
                    ),
                    12 => KlibIrStatement::TypeAlias(self.declaration_symbol(&declaration)?),
                    other => {
                        return Err(declaration
                            .error(format!("declaration kind {other} cannot be a statement")))
                    }
                })
            }
            3 => Ok(KlibIrStatement::Expression(self.expression(&value)?)),
            other => Err(value.error(format!("statement kind {other} outside its owner"))),
        }
    }

    // ---- declarations -----------------------------------------------------------------------

    /// `IrDeclarationBase` of a declaration message: symbol, origin and flags.
    fn base(
        &mut self,
        declaration: &Msg<'_>,
    ) -> Result<(KlibIrSymbol, String, u64), KlibIrDecodeError> {
        let base = declaration.required(1, "declaration base")?;
        let symbol = self.required_symbol(&base, 1, "declaration symbol")?;
        let origin = self.string(base.required_varint(2, "declaration origin")?, "origin")?;
        let flags = base.varint(4, "declaration flags")?.unwrap_or(0);
        Ok((symbol, origin, flags))
    }

    fn declaration_symbol(
        &mut self,
        declaration: &Msg<'_>,
    ) -> Result<KlibIrSymbol, KlibIrDecodeError> {
        Ok(self.base(declaration)?.0)
    }

    fn name_and_type(
        &mut self,
        msg: &Msg<'_>,
        number: u64,
    ) -> Result<(String, KlibIrTypeId), KlibIrDecodeError> {
        let (name, ty) = lattice(msg.required_varint(number, "name and type")?);
        Ok((self.string(name, "name")?, self.ty(ty)?))
    }

    fn parameter(&mut self, msg: &Msg<'_>) -> Result<KlibIrParameter, KlibIrDecodeError> {
        let (symbol, origin, flags) = self.base(msg)?;
        let (name, ty) = self.name_and_type(msg, 2)?;
        let vararg_element_type = msg
            .varint(3, "vararg element type")?
            .map(|raw| self.ty(raw))
            .transpose()?;
        let default_value = msg
            .varint(4, "default value")?
            .map(|raw| self.expression_entry(raw, "default value"))
            .transpose()?;
        Ok(KlibIrParameter {
            symbol,
            name,
            ty,
            vararg_element_type,
            default_value,
            origin,
            flags,
        })
    }

    fn type_parameters(
        &mut self,
        msg: &Msg<'_>,
        number: u64,
    ) -> Result<Vec<KlibIrTypeParameter>, KlibIrDecodeError> {
        let mut parameters = Vec::new();
        for parameter in msg.messages(number, "type parameter")? {
            let (symbol, _, flags) = self.base(&parameter)?;
            let name = self.string(parameter.required_varint(2, "type parameter name")?, "name")?;
            let supertypes = parameter
                .packed(3, "type parameter bound")?
                .into_iter()
                .map(|raw| self.ty(raw))
                .collect::<Result<_, _>>()?;
            parameters.push(KlibIrTypeParameter {
                symbol,
                name,
                supertypes,
                flags,
            });
        }
        Ok(parameters)
    }

    /// `IrFunction` (or `IrConstructor` when `constructor`): both wrap an `IrFunctionBase`.
    fn function(
        &mut self,
        msg: &Msg<'_>,
        constructor: bool,
    ) -> Result<KlibIrFunctionId, KlibIrDecodeError> {
        self.nested(msg, |decoder| decoder.nested_function(msg, constructor))
    }

    fn nested_function(
        &mut self,
        msg: &Msg<'_>,
        constructor: bool,
    ) -> Result<KlibIrFunctionId, KlibIrDecodeError> {
        let base = msg.required(1, "function base")?;
        let (symbol, origin, flags) = self.base(&base)?;
        let (name, return_type) = self.name_and_type(&base, 2)?;
        let type_parameters = self.type_parameters(&base, 3)?;
        let dispatch_receiver = base
            .message(4, "dispatch receiver")?
            .map(|p| self.parameter(&p))
            .transpose()?;
        let context_parameters = base
            .messages(9, "context parameter")?
            .iter()
            .map(|p| self.parameter(p))
            .collect::<Result<_, _>>()?;
        let extension_receiver = base
            .message(5, "extension receiver")?
            .map(|p| self.parameter(&p))
            .transpose()?;
        let regular_parameters = base
            .messages(6, "regular parameter")?
            .iter()
            .map(|p| self.parameter(p))
            .collect::<Result<_, _>>()?;
        let body = base
            .varint(7, "function body")?
            .map(|raw| self.statement_body_entry(raw, "function body"))
            .transpose()?;
        let overridden = if constructor {
            Vec::new()
        } else {
            msg.packed(2, "overridden function")?
                .into_iter()
                .map(|code| self.symbol(code))
                .collect::<Result<_, _>>()?
        };
        let id =
            KlibIrFunctionId(u32::try_from(self.arena.functions.len()).expect("arena fits u32"));
        self.arena.functions.push(KlibIrFunction {
            symbol,
            name,
            constructor,
            origin,
            flags,
            type_parameters,
            dispatch_receiver,
            context_parameters,
            extension_receiver,
            regular_parameters,
            return_type,
            overridden,
            body,
        });
        Ok(id)
    }

    fn variable(&mut self, msg: &Msg<'_>) -> Result<KlibIrVariableId, KlibIrDecodeError> {
        let (symbol, origin, flags) = self.base(msg)?;
        let (name, ty) = self.name_and_type(msg, 2)?;
        let initializer = self.optional_expression(msg, 3, "variable initializer")?;
        let id =
            KlibIrVariableId(u32::try_from(self.arena.variables.len()).expect("arena fits u32"));
        self.arena.variables.push(KlibIrVariable {
            symbol,
            name,
            ty,
            initializer,
            origin,
            flags,
        });
        Ok(id)
    }

    fn field(&mut self, msg: &Msg<'_>) -> Result<KlibIrField, KlibIrDecodeError> {
        let (symbol, _, flags) = self.base(msg)?;
        let (name, ty) = self.name_and_type(msg, 2)?;
        let initializer = msg
            .varint(3, "field initializer")?
            .map(|raw| self.expression_entry(raw, "field initializer"))
            .transpose()?;
        Ok(KlibIrField {
            symbol,
            name,
            ty,
            initializer,
            flags,
        })
    }

    fn property(&mut self, msg: &Msg<'_>) -> Result<KlibIrProperty, KlibIrDecodeError> {
        let (symbol, _, flags) = self.base(msg)?;
        let name = self.string(msg.required_varint(2, "property name")?, "name")?;
        let backing_field = msg
            .message(3, "backing field")?
            .map(|field| self.field(&field))
            .transpose()?;
        let getter = msg
            .message(4, "getter")?
            .map(|getter| self.function(&getter, false))
            .transpose()?;
        let setter = msg
            .message(5, "setter")?
            .map(|setter| self.function(&setter, false))
            .transpose()?;
        Ok(KlibIrProperty {
            symbol,
            name,
            flags,
            backing_field,
            getter,
            setter,
        })
    }

    fn local_delegated_property(
        &mut self,
        msg: &Msg<'_>,
    ) -> Result<KlibIrLocalDelegatedProperty, KlibIrDecodeError> {
        let (symbol, _, flags) = self.base(msg)?;
        let (name, ty) = self.name_and_type(msg, 2)?;
        Ok(KlibIrLocalDelegatedProperty {
            symbol,
            name,
            ty,
            delegate: msg
                .message(3, "delegate")?
                .map(|delegate| self.variable(&delegate))
                .transpose()?,
            getter: msg
                .message(4, "getter")?
                .map(|getter| self.function(&getter, false))
                .transpose()?,
            setter: msg
                .message(5, "setter")?
                .map(|setter| self.function(&setter, false))
                .transpose()?,
            flags,
        })
    }

    fn class(&mut self, msg: &Msg<'_>) -> Result<KlibIrClassId, KlibIrDecodeError> {
        self.nested(msg, |decoder| decoder.nested_class(msg))
    }

    fn nested_class(&mut self, msg: &Msg<'_>) -> Result<KlibIrClassId, KlibIrDecodeError> {
        let (symbol, origin, flags) = self.base(msg)?;
        let name = self.string(msg.required_varint(2, "class name")?, "name")?;
        let this_receiver = msg
            .message(3, "this receiver")?
            .map(|p| self.parameter(&p))
            .transpose()?;
        let type_parameters = self.type_parameters(msg, 4)?;
        let mut members = Vec::new();
        for member in msg.messages(5, "class member")? {
            members.push(self.member(&member)?);
        }
        let supertypes = msg
            .packed(6, "supertype")?
            .into_iter()
            .map(|raw| self.ty(raw))
            .collect::<Result<_, _>>()?;
        if let Some(representation) = msg.message(7, "inline class representation")? {
            self.ty(representation.required_varint(2, "underlying type")?)?;
        }
        let sealed_subclasses = msg
            .packed(8, "sealed subclass")?
            .into_iter()
            .map(|code| self.symbol(code))
            .collect::<Result<_, _>>()?;
        let id = KlibIrClassId(u32::try_from(self.arena.classes.len()).expect("arena fits u32"));
        self.arena.classes.push(KlibIrClass {
            symbol,
            name,
            origin,
            flags,
            this_receiver,
            type_parameters,
            supertypes,
            members,
            sealed_subclasses,
        });
        Ok(id)
    }

    /// One `IrDeclaration` that may stand at file or class level.
    fn member(&mut self, msg: &Msg<'_>) -> Result<KlibIrMember, KlibIrDecodeError> {
        let (kind, declaration) = msg
            .oneof(1..=12, "IrDeclaration")?
            .ok_or_else(|| msg.error("declaration is empty"))?;
        Ok(match kind {
            1 => {
                let (symbol, _, _) = self.base(&declaration)?;
                let body = self.statement_body_entry(
                    declaration.required_varint(2, "anonymous initializer body")?,
                    "anonymous initializer body",
                )?;
                let KlibIrBody::Block(statements) = body else {
                    return Err(declaration.error("anonymous initializer has a synthetic body"));
                };
                KlibIrMember::AnonymousInitializer { symbol, statements }
            }
            2 => KlibIrMember::Class(self.class(&declaration)?),
            3 => KlibIrMember::Function(self.function(&declaration, true)?),
            4 => {
                let (symbol, _, _) = self.base(&declaration)?;
                let name = self.string(declaration.required_varint(2, "entry name")?, "name")?;
                let initializer = declaration
                    .varint(3, "enum entry initializer")?
                    .map(|raw| self.expression_entry(raw, "enum entry initializer"))
                    .transpose()?;
                let class = declaration
                    .message(4, "enum entry class")?
                    .map(|class| self.class(&class))
                    .transpose()?;
                KlibIrMember::EnumEntry(KlibIrEnumEntry {
                    symbol,
                    name,
                    initializer,
                    class,
                })
            }
            5 => KlibIrMember::Field(self.field(&declaration)?),
            6 => KlibIrMember::Function(self.function(&declaration, false)?),
            7 => KlibIrMember::Property(self.property(&declaration)?),
            12 => KlibIrMember::TypeAlias(self.declaration_symbol(&declaration)?),
            other => {
                return Err(declaration.error(format!(
                    "declaration kind {other} cannot be a file or class member"
                )))
            }
        })
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

    /// `IdSignature.public_sig` for `package.declaration`, by string-table index.
    fn public_signature(package: u64, declaration: u64) -> Vec<u8> {
        let mut common = bytes_field(1, &{
            let mut path = Vec::new();
            push_varint(package, &mut path);
            path
        });
        common.extend(bytes_field(2, &{
            let mut path = Vec::new();
            push_varint(declaration, &mut path);
            path
        }));
        bytes_field(1, &common)
    }

    /// `IdSignature.scoped_local_sig`: identity private to the file.
    fn local_signature(slot: u64) -> Vec<u8> {
        varint_field(4, slot)
    }

    const FUNCTION: u64 = 0;
    const VALUE_PARAMETER: u64 = 4;

    fn symbol(slot: u64, kind: u64) -> u64 {
        (slot << 8) | kind
    }

    fn get_value(slot: u64) -> Vec<u8> {
        bytes_field(6, &varint_field(1, symbol(slot, VALUE_PARAMETER)))
    }

    fn missing() -> Vec<u8> {
        bytes_field(44, &[])
    }

    fn strings() -> Vec<String> {
        ["kotlin", "plus", "x"].map(str::to_string).to_vec()
    }

    fn decode(expression: &[u8]) -> (KlibIrArena, KlibIrExprId) {
        let strings = strings();
        let signatures = [public_signature(0, 1), local_signature(7)];
        let signatures = signatures.iter().map(Vec::as_slice).collect::<Vec<_>>();
        let mut file = FileTables {
            strings: &strings,
            signatures: SignatureTable::new(3, &signatures, &strings),
            types: Vec::new(),
            bodies: Vec::new(),
        };
        let mut decoder = TreeDecoder::new(&mut file);
        let id = decoder
            .expression(&Msg::parse(expression, 0, BODIES).unwrap())
            .unwrap();
        (decoder.arena, id)
    }

    fn plus() -> KlibIrSymbol {
        let strings = strings();
        let signature = public_signature(0, 1);
        let signatures = [signature.as_slice()];
        SignatureTable::new(0, &signatures, &strings)
            .symbol(symbol(0, FUNCTION))
            .unwrap()
    }

    #[test]
    fn a_missing_argument_is_an_unsupplied_slot() {
        let mut call = varint_field(1, symbol(0, FUNCTION));
        call.extend(bytes_field(5, &get_value(1)));
        call.extend(bytes_field(5, &missing()));
        let (arena, id) = decode(&bytes_field(8, &call));
        let KlibIrExprKind::Call {
            access,
            super_qualifier,
        } = &arena.expr(id).kind
        else {
            panic!("{:?}", arena.expr(id));
        };
        assert_eq!(super_qualifier, &None);
        assert_eq!(access.symbol, plus());
        let KlibIrArguments::Flat(arguments) = &access.arguments else {
            panic!("{:?}", access.arguments);
        };
        let [Some(read), None] = arguments.as_slice() else {
            panic!("{arguments:?}");
        };
        assert_eq!(
            arena.expr(*read).kind,
            KlibIrExprKind::GetValue {
                symbol: KlibIrSymbol {
                    kind: super::super::KlibIrSymbolKind::ValueParameter,
                    signature: KlibIrSignature::FileLocal { file: 3, slot: 1 },
                },
                origin: None,
            }
        );
    }

    #[test]
    fn a_pre_2_4_call_keeps_its_receivers_apart() {
        let mut access = bytes_field(1, &get_value(1));
        access.extend(bytes_field(3, &bytes_field(1, &get_value(1))));
        access.extend(bytes_field(3, &[]));
        let mut call = varint_field(1, symbol(0, FUNCTION));
        call.extend(bytes_field(2, &access));
        // IrOperationPre_2_4_0.call is alternative 3.
        let expression = bytes_field(1, &bytes_field(3, &call));
        let (arena, id) = decode(&expression);
        let KlibIrExprKind::Call { access, .. } = &arena.expr(id).kind else {
            panic!("{:?}", arena.expr(id));
        };
        let KlibIrArguments::Split {
            dispatch_receiver: Some(_),
            extension_receiver: None,
            values,
        } = &access.arguments
        else {
            panic!("{:?}", access.arguments);
        };
        assert!(matches!(values.as_slice(), [Some(_), None]), "{values:?}");
    }

    #[test]
    fn both_operation_layouts_in_one_expression_are_rejected() {
        let mut expression = bytes_field(1, &bytes_field(16, &varint_field(1, symbol(1, 4))));
        expression.extend(get_value(1));
        let strings = strings();
        let signatures = [public_signature(0, 1), local_signature(7)];
        let signatures = signatures.iter().map(Vec::as_slice).collect::<Vec<_>>();
        let mut file = FileTables {
            strings: &strings,
            signatures: SignatureTable::new(0, &signatures, &strings),
            types: Vec::new(),
            bodies: Vec::new(),
        };
        let error = TreeDecoder::new(&mut file)
            .expression(&Msg::parse(&expression, 0, BODIES).unwrap())
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "invalid KLIB IR bodies.knb at byte 0: expression carries both operation layouts"
        );
    }

    #[test]
    fn every_pre_2_4_operation_maps_to_a_distinct_current_one() {
        let mapped = (1..=39)
            .map(|legacy| current_operation(legacy).unwrap())
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(mapped.len(), 39);
        assert!(mapped.iter().all(|number| (5..=43).contains(number)));
        assert_eq!(current_operation(0), None);
        assert_eq!(current_operation(40), None);
    }

    /// A type table where type `i` is `type(i + 1) & Any` and the last type is a plain class.
    fn definitely_not_null_chain(length: usize) -> Vec<Vec<u8>> {
        let mut types = (1..length)
            .map(|next| {
                let mut packed = Vec::new();
                push_varint(next as u64, &mut packed);
                bytes_field(4, &bytes_field(1, &packed))
            })
            .collect::<Vec<_>>();
        types.push(bytes_field(5, &varint_field(2, symbol(0, 6))));
        types
    }

    #[test]
    fn an_over_deep_acyclic_type_chain_is_an_error_and_releases_its_depth() {
        let strings = strings();
        let signatures = [public_signature(0, 1)];
        let signatures = signatures.iter().map(Vec::as_slice).collect::<Vec<_>>();
        let deep = definitely_not_null_chain(MAX_DEPTH + 1);
        let mut file = FileTables {
            strings: &strings,
            signatures: SignatureTable::new(0, &signatures, &strings),
            types: deep.iter().map(Vec::as_slice).collect(),
            bodies: Vec::new(),
        };
        let mut decoder = TreeDecoder::new(&mut file);
        let error = decoder.ty(0).unwrap_err();
        assert_eq!(
            error.to_string(),
            format!("invalid KLIB IR types.knt at byte 0: nesting exceeds {MAX_DEPTH} levels")
        );
        assert_eq!(decoder.depth, 0);
        assert!(decoder.types.is_empty(), "{:?}", decoder.types);
        // The deepest type that fits is still decoded after the failure, from the same decoder.
        let fitting = decoder.ty(1).unwrap();
        assert!(matches!(
            decoder.arena.ty(fitting),
            KlibIrType::DefinitelyNotNull(_)
        ));
        assert_eq!(decoder.depth, 0);
        assert_eq!(decoder.arena.types.len(), MAX_DEPTH);
    }

    #[test]
    fn lattice_interleaves_two_ints() {
        // BinaryLattice.encode(5, 3): 5 in the even bits, 3 in the odd bits.
        assert_eq!(lattice(0b01_10_11), (5, 3));
        assert_eq!(lattice(0), (0, 0));
    }
}
