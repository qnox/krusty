//! The Kotlin metadata protobuf READER: declarations, from whichever carrier holds them.
//!
//! Kotlin records a module's declarations as one protobuf schema (`core/metadata/src/metadata.proto`)
//! and ships it in three different carriers: a JVM classfile's `@Metadata` annotation, a
//! `.kotlin_builtins` resource in the stdlib jar, and a KLIB's `default/linkdata/package_*.knm`
//! fragments. The *schema* is the same in all three — only the carrier differs — so the decoder is
//! core, beside the [encoder](super) that writes that same schema back out.
//!
//! What lives here is the carrier-independent half: the wire-format cursor, the string and
//! qualified-name tables, and the `PackageFragment` → declarations decode that turns a `.knm` body
//! or a `.kotlin_builtins` body into a [`BuiltinPackage`]. What does not live here is any carrier or
//! target decision: unwrapping `@Metadata`'s `d1`/`d2` string arrays, computing JVM descriptors, and
//! mapping a `Class.flags` word onto a JVM access mask are the JVM backend's, and stay there.
//!
//! The declarations this reports are therefore Kotlin-level throughout. [`BuiltinClass`] carries the
//! raw `flags` word rather than a decoded JVM access mask, because what a flag word means for a
//! target's representation is that backend's answer to give.

pub mod library_type;
mod type_annotations;

use std::collections::HashMap;

use crate::libraries::TypeKind;
use crate::types::{Ty, Visibility};

/// The carrier-independent wire shape of Kotlin metadata's `Type` message. Both an annotation's
/// `@Metadata` payload and a `.kotlin_builtins` fragment use these same fields; only the way their
/// numeric class/string ids and type-table references are resolved differs. Keeping the protobuf walk
/// here prevents the two decoders from acquiring subtly different nullability, type-parameter,
/// annotation, or argument handling as either carrier evolves.
pub struct ParsedTypeNode<'a> {
    pub class_id: Option<u64>,
    pub type_parameter_id: Option<u64>,
    pub type_parameter_name_id: Option<u64>,
    pub type_alias_id: Option<u64>,
    pub nullable: bool,
    pub definitely_non_null: bool,
    pub flexible_upper_bound: Option<&'a [u8]>,
    pub flexible_upper_bound_id: Option<u64>,
    pub arguments: Vec<ParsedTypeArgument<'a>>,
    pub annotations: Vec<type_annotations::TypeAnnotation>,
}

#[derive(Clone, Copy)]
pub enum ParsedProjection {
    In,
    Out,
    Invariant,
}

pub fn project_ty(projection: ParsedProjection, ty: Ty) -> Ty {
    match projection {
        ParsedProjection::In => Ty::in_projection(ty),
        ParsedProjection::Out => Ty::out_projection(ty),
        ParsedProjection::Invariant => ty,
    }
}

/// A type argument is either an inline `Type`, an id into the carrier's `TypeTable`, or a star
/// projection with no type. Projection is part of this carrier-independent wire shape: every semantic
/// decoder must see the same `in`/`out`/invariant distinction.
pub enum ParsedTypeArgument<'a> {
    Inline(&'a [u8], ParsedProjection),
    Table(u64, ParsedProjection),
    Star,
}

pub fn parse_type_node(body: &[u8]) -> Option<ParsedTypeNode<'_>> {
    let mut pb = Pb { b: body, i: 0 };
    let mut node = ParsedTypeNode {
        class_id: None,
        type_parameter_id: None,
        type_parameter_name_id: None,
        type_alias_id: None,
        nullable: false,
        definitely_non_null: false,
        flexible_upper_bound: None,
        flexible_upper_bound_id: None,
        arguments: Vec::new(),
        annotations: Vec::new(),
    };
    while !pb.at_end() {
        let tag = pb.varint()?;
        match (tag >> 3, tag & 7) {
            (1, 0) => node.definitely_non_null = pb.varint()? & 0x2 != 0,
            (3, 0) => node.nullable = pb.varint()? != 0,
            (5, 2) => {
                let len = pb.varint()? as usize;
                node.flexible_upper_bound = Some(pb.bytes(len)?);
            }
            (6, 0) => node.class_id = Some(pb.varint()?),
            (7, 0) => node.type_parameter_id = Some(pb.varint()?),
            (8, 0) => node.flexible_upper_bound_id = Some(pb.varint()?),
            (9, 0) => node.type_parameter_name_id = Some(pb.varint()?),
            (12, 0) => node.type_alias_id = Some(pb.varint()?),
            (2, 2) => {
                let n = pb.varint()? as usize;
                let mut argument_pb = Pb {
                    b: pb.bytes(n)?,
                    i: 0,
                };
                let mut projection = ParsedProjection::Invariant;
                let mut star = false;
                let mut inline = None;
                let mut table = None;
                while !argument_pb.at_end() {
                    let tag = argument_pb.varint()?;
                    match (tag >> 3, tag & 7) {
                        (1, 0) => {
                            projection = match argument_pb.varint()? {
                                0 => ParsedProjection::In,
                                1 => ParsedProjection::Out,
                                2 => ParsedProjection::Invariant,
                                3 => {
                                    star = true;
                                    ParsedProjection::Invariant
                                }
                                _ => return None,
                            }
                        }
                        (2, 2) => {
                            let n = argument_pb.varint()? as usize;
                            inline = Some(argument_pb.bytes(n)?);
                        }
                        (3, 0) => table = Some(argument_pb.varint()?),
                        (_, wire) => argument_pb.skip(wire)?,
                    }
                }
                if star {
                    node.arguments.push(ParsedTypeArgument::Star);
                } else {
                    node.arguments.push(match (inline, table) {
                        (Some(body), _) => ParsedTypeArgument::Inline(body, projection),
                        (None, Some(id)) => ParsedTypeArgument::Table(id, projection),
                        (None, None) => return None,
                    });
                }
            }
            (100, 2) => {
                // `Type.annotation` is an extension carrying an `Annotation` message. Preserve its
                // semantic arguments as well as its class id: context-function arity is carried by
                // `ContextFunctionTypeParams.count` rather than by the function classifier.
                let n = pb.varint()? as usize;
                node.annotations
                    .push(type_annotations::parse(pb.bytes(n)?)?);
            }
            (_, wire) => pb.skip(wire)?,
        }
    }
    Some(node)
}

pub struct ParsedTypeParam {
    pub id: u64,
    pub name_id: u64,
    pub reified: bool,
    pub upper_bound_bodies: Vec<Vec<u8>>,
    /// `TypeParameter.upper_bound_id` (field 6) — the type-table form a `.kotlin_builtins` fragment
    /// uses instead of the inline `upper_bound`. Empty for the `@Metadata` carrier, which inlines.
    pub upper_bound_ids: Vec<u64>,
    pub variance: crate::types::TypeVariance,
    /// Raw core/builtins `Annotation` messages declared on this type parameter. The two metadata
    /// protocols use different field numbers but the annotation payload is identical.
    pub annotation_bodies: Vec<Vec<u8>>,
}

pub fn parse_type_param(body: &[u8]) -> Option<ParsedTypeParam> {
    let mut pb = Pb { b: body, i: 0 };
    let mut id = None;
    let mut name = None;
    let mut upper_bound_bodies = Vec::new();
    let mut upper_bound_ids = Vec::new();
    let mut reified = false;
    let mut variance = crate::types::TypeVariance::Invariant;
    let mut annotation_bodies = Vec::new();
    while !pb.at_end() {
        let tag = pb.varint()?;
        match (tag >> 3, tag & 7) {
            (1, 0) => id = Some(pb.varint()?),
            (2, 0) => name = Some(pb.varint()?),
            (3, 0) => reified = pb.varint()? != 0,
            (4, 0) => {
                variance = match pb.varint()? {
                    0 => crate::types::TypeVariance::In,
                    1 => crate::types::TypeVariance::Out,
                    _ => crate::types::TypeVariance::Invariant,
                }
            }
            (5, 2) => {
                let n = pb.varint()? as usize;
                upper_bound_bodies.push(pb.bytes(n)?.to_vec());
            }
            (6, 0) => upper_bound_ids.push(pb.varint()?),
            (6, 2) => {
                let n = pb.varint()? as usize;
                upper_bound_ids.extend(packed_varints(pb.bytes(n)?));
            }
            // `ProtoBuf.TypeParameter.annotation` and
            // `BuiltInsProtoBuf.typeParameterAnnotation`, respectively.
            (100, 2) | (150, 2) => {
                let n = pb.varint()? as usize;
                annotation_bodies.push(pb.bytes(n)?.to_vec());
            }
            (_, w) => pb.skip(w)?,
        }
    }
    Some(ParsedTypeParam {
        id: id?,
        name_id: name?,
        reified,
        upper_bound_bodies,
        upper_bound_ids,
        variance,
        annotation_bodies,
    })
}

/// kotlinc's `JvmNameResolverBase.PREDEFINED_STRINGS` — the fixed table a `StringTableTypes.Record`'s
/// `predefined_index` selects (common built-in class names that aren't stored in `d2`). Verbatim from
/// `core/metadata.jvm/.../JvmNameResolverBase.kt`, so `class_name` ids resolve identically to kotlinc.
pub const PREDEFINED_STRINGS: &[&str] = &[
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
pub struct Rec {
    pub predefined_index: Option<usize>,
    pub string: Option<String>,
    pub operation: u64, // 0 NONE, 1 INTERNAL_TO_CLASS_ID, 2 DESC_TO_CLASS_ID
    pub substring: Option<(usize, usize)>,
    pub replace: Option<(u32, u32)>,
}

/// Read a packed (length-delimited) repeated `int32` field into a Vec of varints.
pub fn packed_varints(body: &[u8]) -> Vec<u64> {
    let mut pb = Pb { b: body, i: 0 };
    let mut out = Vec::new();
    while !pb.at_end() {
        match pb.varint() {
            Some(v) => out.push(v),
            None => break,
        }
    }
    out
}

/// Parse one `StringTableTypes.Record` → `(range, Rec)`.
pub fn parse_record(body: &[u8]) -> Option<(u64, Rec)> {
    let mut pb = Pb { b: body, i: 0 };
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
                let v = packed_varints(pb.bytes(n)?);
                if v.len() >= 2 {
                    rec.substring = Some((v[0] as usize, v[1] as usize));
                }
            }
            (5, 2) => {
                let n = pb.varint()? as usize;
                let v = packed_varints(pb.bytes(n)?);
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
pub fn parse_string_table(body: &[u8]) -> Vec<Rec> {
    let mut pb = Pb { b: body, i: 0 };
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
pub fn resolve_string(records: &[Rec], d2: &[String], id: usize) -> Option<String> {
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

pub fn resolve_class_name(records: &[Rec], d2: &[String], id: usize) -> Option<String> {
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

/// A protobuf wire-format cursor over a message body.
pub struct Pb<'a> {
    pub b: &'a [u8],
    pub i: usize,
}

impl<'a> Pb<'a> {
    pub fn varint(&mut self) -> Option<u64> {
        let mut v = 0u64;
        let mut shift = 0;
        loop {
            let byte = *self.b.get(self.i)?;
            self.i += 1;
            v |= ((byte & 0x7f) as u64) << shift;
            if byte & 0x80 == 0 {
                return Some(v);
            }
            shift += 7;
            if shift >= 64 {
                return None;
            }
        }
    }
    pub fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.b.get(self.i..self.i.checked_add(n)?)?;
        self.i += n;
        Some(s)
    }
    pub fn at_end(&self) -> bool {
        self.i >= self.b.len()
    }
    /// Skip a field's value given its wire type; `false` on a malformed/unsupported wire type.
    pub fn skip(&mut self, wire: u64) -> Option<()> {
        match wire {
            0 => {
                self.varint()?;
            }
            1 => {
                self.bytes(8)?;
            }
            2 => {
                let n = self.varint()? as usize;
                self.bytes(n)?;
            }
            5 => {
                self.bytes(4)?;
            }
            _ => return None,
        }
        Some(())
    }
}

/// `IS_INLINE` is bit 10 of `Function.flags` (hasAnnotations·1 + Visibility·3 + Modality·2 +
/// MemberKind·2 + isOperator·1 + isInfix·1 → isInline).
pub const IS_INLINE_BIT: u64 = 1 << 10;

/// `IS_OPERATOR` immediately follows the 2-bit member-kind field in Kotlin metadata's function flags.
pub const IS_OPERATOR_BIT: u64 = 1 << 8;

/// `IS_INFIX` follows `IS_OPERATOR` in Kotlin metadata's function flags.
pub const IS_INFIX_BIT: u64 = 1 << 9;

/// A `JvmMethodSignature`. Both fields are independently optional in the protobuf: an omitted name
/// means the Kotlin declaration name, while an omitted descriptor is derived from the Kotlin types.
#[derive(Clone, Copy, Debug)]
pub struct ParsedJvmSignature {
    pub name_id: Option<u64>,
    pub desc_id: Option<u64>,
}

pub fn parse_jvm_signature(body: &[u8]) -> Option<ParsedJvmSignature> {
    let mut pb = Pb { b: body, i: 0 };
    let mut name = None;
    let mut desc = None;
    while !pb.at_end() {
        let tag = pb.varint()?;
        match (tag >> 3, tag & 7) {
            (1, 0) => name = Some(pb.varint()?),
            (2, 0) => desc = Some(pb.varint()?),
            (_, w) => pb.skip(w)?,
        }
    }
    Some(ParsedJvmSignature {
        name_id: name,
        desc_id: desc,
    })
}

/// The `class_name` (fq-name table id, `Type.class_name = 6`) of a `Type` message — the type's class
/// identity (`mutableListOf`'s return `Type` → the id whose `d2` string is `kotlin/collections/MutableList`).
/// `None` for a non-class type (a bare type parameter, etc.).
pub fn parse_type_class_name(body: &[u8]) -> Option<u64> {
    let mut pb = Pb { b: body, i: 0 };
    let mut class_name = None;
    while !pb.at_end() {
        let tag = pb.varint()?;
        match (tag >> 3, tag & 7) {
            (6, 0) => class_name = Some(pb.varint()?), // Type.class_name
            (_, w) => pb.skip(w)?,
        }
    }
    class_name
}

/// `Function.flags` bit for `suspend` (kotlin metadata `Flags.IS_SUSPEND`, function flag bit 13).
pub const IS_SUSPEND_BIT: u64 = 1 << 13;

/// `ValueParameter.flags` bit for `DECLARES_DEFAULT_VALUE` (bit 1; `HAS_ANNOTATIONS` is bit 0).
pub const DECLARES_DEFAULT_VALUE_BIT: u64 = 1 << 1;

/// `ValueParameter.flags` bits for `IS_CROSSINLINE` (bit 2) and `IS_NOINLINE` (bit 3) of an inline
/// function's functional parameter. Either one means the lambda argument is MATERIALIZED into a real
/// `FunctionN` object / nested class (not spliced into the caller frame), so a mutable local it captures
/// must be boxed in a `Ref` holder — the same as an ordinary closure.
pub const IS_CROSSINLINE_BIT: u64 = 1 << 2;

pub const IS_NOINLINE_BIT: u64 = 1 << 3;

/// `Visibility` enum value from a Function/Class `flags` word: `hasAnnotations` is bit 0, then
/// `Visibility` occupies the next 3 bits (kotlin metadata `Flags.VISIBILITY`). Enum order:
/// INTERNAL=0, PRIVATE=1, PROTECTED=2, PUBLIC=3, PRIVATE_TO_THIS=4, LOCAL=5.
pub fn flags_visibility(flags: u64) -> u64 {
    (flags >> 1) & 0x7
}

/// One source `ValueParameter` decoded from metadata. Keeping these facts together avoids the parser's
/// old parallel vectors drifting as more parameter-level facts are added.
pub struct ParsedValueParam {
    pub name_id: u64,
    pub has_default: bool,
    pub materialized: bool,
    /// The raw inline `ValueParameter.type` (field 3) message, when present. Keeping PRESENCE rather
    /// than an empty sentinel distinguishes an explicitly empty/default `Type` from a parameter that
    /// instead names the enclosing type table through [`Self::type_id`].
    pub type_body: Option<Vec<u8>>,
    /// `ValueParameter.type_id` (field 5), indexing the function/container `TypeTable` when the
    /// producer chose table-backed types.
    pub type_id: Option<u64>,
    /// The raw `ValueParameter.varargElementType` (field 4 as emitted by kotlin-stdlib 2.3.20) `Type`
    /// body when the parameter is a `vararg`.
    /// Present ⇒ the parameter is a vararg whose LOGICAL gsig is `Array<elem>`; kotlinc stores the element
    /// type here (the JVM descriptor's array-ness lives only in `type`/the descriptor).
    pub vararg_elem_body: Option<Vec<u8>>,
    /// `ValueParameter.vararg_element_type_id` (field 6), the table-backed form of
    /// [`Self::vararg_elem_body`].
    pub vararg_elem_id: Option<u64>,
    /// Compiler-known strict-equality refinement (`ValueParameter.equality_bound_type`, fields
    /// 9/10). This is declaration semantics, not an ordinary retained annotation.
    pub equality_bound_body: Option<Vec<u8>>,
    pub equality_bound_id: Option<u64>,
}

/// A decoded `Function` message: whether it's `inline`, whether it's `suspend`, its name string id, its
/// explicit JVM `(name id, desc id)` signature (if present), its operator flag, and its return type's
/// class_name id.
pub struct ParsedFunction {
    pub is_inline: bool,
    pub is_suspend: bool,
    /// Kotlin declaration modality from metadata. This is deliberately independent of the
    /// realization method's classfile access: under `-jvm-default=disable`, a concrete interface
    /// declaration is represented by an abstract interface method plus a static holder body.
    pub is_abstract: bool,
    pub is_final: bool,
    /// `Function.flags.MEMBER_KIND == SYNTHESIZED`. The enclosing declaration may attach implicit
    /// compiler semantics that kotlinc deliberately does not repeat on the value parameter.
    pub is_synthesized: bool,
    pub is_operator: bool,
    pub is_infix: bool,
    pub visibility: crate::types::Visibility,
    pub name_id: u64,
    pub jvm_sig: Option<ParsedJvmSignature>,
    /// Whether `receiver_type` (field 5) was present — TRUE for an extension on a type PARAMETER
    /// (`fun <T> T.takeIf`), where `recv_class` is None. Distinguishes an extension from a top-level fn.
    pub has_receiver: bool,
    /// Whether the Kotlin return type is nullable (`T?`) — `Type.nullable = 3`. The JVM
    /// descriptor/`Signature` erase this; only `@Metadata` carries it. Drives the elvis null-check for a
    /// nullable-returning scope fn (`takeIf`/`takeUnless` return `T?`).
    /// SOURCE value parameters in declaration order. The COUNT is the source arity (excludes synthetic
    /// descriptor params); fields are resolved to names downstream.
    pub value_params: Vec<ParsedValueParam>,
    /// The function's own `type_parameter` table (field 4): `(id, name string-id)` — for resolving a
    /// `Type.type_parameter` reference in a parameter/return type to its name.
    pub type_params: Vec<ParsedTypeParam>,
    /// Raw `Function.return_type` (field 3) `Type` body, for the metadata generic signature.
    pub return_body: Option<Vec<u8>>,
    /// `Function.return_type_id` (field 7), used when the return lives in the effective type table.
    pub return_type_id: Option<u64>,
    /// Raw `Function.receiver_type` (field 5) `Type` body (extensions only), for the metadata gsig.
    pub receiver_body: Option<Vec<u8>>,
    /// `Function.receiver_type_id` (field 8), whose presence also marks an extension.
    pub receiver_type_id: Option<u64>,
    /// Raw `Annotation` message bodies on the function (`Function.annotation`, field 12) — decoded to
    /// `(class name, arguments)` downstream where the string table is available. Kotlin stores an
    /// annotation here when it has `BINARY`/`RUNTIME` retention (`@JvmName`, `@OverloadResolutionBy…`).
    pub annotation_bodies: Vec<Vec<u8>>,
    /// Raw `Contract` message body (`Function.contract`, field 32) — the function's declared
    /// `contract { … }` effects, decoded downstream where the string table is available.
    pub contract_body: Option<Vec<u8>>,
    /// Raw `TypeTable` message body (`Function.type_table`, field 30) — contract expressions may
    /// reference their `is_instance_type` by id into this table instead of inlining the `Type`.
    pub type_table_body: Option<Vec<u8>>,
    /// Old unnamed context receivers (`context_receiver_type` fields 10/11).
    pub context_receiver_bodies: Vec<Vec<u8>>,
    pub context_receiver_type_ids: Vec<u64>,
    /// Named context parameters (`context_parameter`, field 13).
    pub context_params: Vec<ParsedValueParam>,
}

pub fn parse_value_parameter(body: &[u8]) -> Option<ParsedValueParam> {
    let mut pb = Pb { b: body, i: 0 };
    let mut name_id = None;
    let mut flags = 0u64;
    let mut type_body = None;
    let mut type_id = None;
    let mut vararg_elem_body = None;
    let mut vararg_elem_id = None;
    let mut equality_bound_body = None;
    let mut equality_bound_id = None;
    while !pb.at_end() {
        let tag = pb.varint()?;
        match (tag >> 3, tag & 7) {
            (1, 0) => flags = pb.varint()?,
            (2, 0) => name_id = pb.varint(),
            (3, 2) => {
                let len = pb.varint()? as usize;
                type_body = Some(pb.bytes(len)?.to_vec());
            }
            (4, 2) => {
                let len = pb.varint()? as usize;
                vararg_elem_body = Some(pb.bytes(len)?.to_vec());
            }
            (5, 0) => type_id = pb.varint(),
            (6, 0) => vararg_elem_id = pb.varint(),
            (9, 2) => {
                let len = pb.varint()? as usize;
                equality_bound_body = Some(pb.bytes(len)?.to_vec());
            }
            (10, 0) => equality_bound_id = pb.varint(),
            (_, wire) => pb.skip(wire)?,
        }
    }
    Some(ParsedValueParam {
        name_id: name_id?,
        has_default: flags & DECLARES_DEFAULT_VALUE_BIT != 0,
        materialized: flags & (IS_CROSSINLINE_BIT | IS_NOINLINE_BIT) != 0,
        type_body,
        type_id,
        vararg_elem_body,
        vararg_elem_id,
        equality_bound_body,
        equality_bound_id,
    })
}

/// Parse one `Function` message. The return type is `Function.return_type = 3` and the extension
/// receiver `Function.receiver_type = 5` (both inline `Type`s in package metadata).
pub fn parse_function(body: &[u8]) -> Option<ParsedFunction> {
    let mut pb = Pb { b: body, i: 0 };
    // Kotlin `metadata.proto` declares `Function.flags = 9 [default = 6]` — a PUBLIC FINAL declaration
    // (visibility bits 1-3 = 3, modality/memberKind = 0). protobuf OMITS a field equal to its default, so
    // the common public-final function serializes NO flags field; initializing to 0 would then decode it
    // as visibility INTERNAL (an interface's ABSTRACT method has non-default flags, so it was serialized
    // and decoded correctly — which hid the bug). Start from the proto default so an absent field is
    // read as public-final.
    let mut legacy_flags = None;
    let mut modern_flags = None;
    let mut name_id = 0u64;
    let mut jvm_sig = None;
    let mut has_receiver = false;
    let mut value_params: Vec<ParsedValueParam> = Vec::new();
    let mut type_params: Vec<ParsedTypeParam> = Vec::new();
    let mut return_body: Option<Vec<u8>> = None;
    let mut return_type_id = None;
    let mut receiver_body: Option<Vec<u8>> = None;
    let mut receiver_type_id = None;
    let mut annotation_bodies: Vec<Vec<u8>> = Vec::new();
    let mut contract_body: Option<Vec<u8>> = None;
    let mut type_table_body: Option<Vec<u8>> = None;
    let mut context_receiver_bodies = Vec::new();
    let mut context_receiver_type_ids = Vec::new();
    let mut context_params = Vec::new();
    let mut seen_fields = Vec::new();
    while !pb.at_end() {
        let tag = pb.varint()?;
        seen_fields.push((tag >> 3, tag & 7));
        match (tag >> 3, tag & 7) {
            (32, 2) => {
                // Function.contract (`Contract` message) — the declared contract's effects.
                let n = pb.varint()? as usize;
                contract_body = Some(pb.bytes(n)?.to_vec());
            }
            (30, 2) => {
                // Function.type_table (`TypeTable` message) — contract `is_instance_type_id`
                // references index into it.
                let n = pb.varint()? as usize;
                type_table_body = Some(pb.bytes(n)?.to_vec());
            }
            (13, 2) => {
                let n = pb.varint()? as usize;
                if let Some(parameter) = parse_value_parameter(pb.bytes(n)?) {
                    context_params.push(parameter);
                }
            }
            // Function.annotation (repeated `Annotation`) — decoded downstream (needs the string
            // table). Field 12 is the shape this reader's own carrier writes; field 170 is the
            // extension a KLIB uses for the same repeated field, and a declaration carries one or
            // the other, never both.
            (12, 2) | (KLIB_ANNOTATION_FIELD, 2) => {
                let n = pb.varint()? as usize;
                annotation_bodies.push(pb.bytes(n)?.to_vec());
            }
            (1, 0) => legacy_flags = pb.varint(),
            (9, 0) => modern_flags = pb.varint(),
            (2, 0) => name_id = pb.varint()?, // name (name id in table)
            (4, 2) => {
                // type_parameter (repeated `TypeParameter`) — the function's own generic parameters.
                let n = pb.varint()? as usize;
                let tpbody = pb.bytes(n)?;
                if let Some(tp) = parse_type_param(tpbody) {
                    type_params.push(tp);
                }
            }
            (3, 2) => {
                // return_type (inline Type message)
                let n = pb.varint()? as usize;
                let tbody = pb.bytes(n)?;
                return_body = Some(tbody.to_vec());
            }
            (5, 2) => {
                // receiver_type (inline Type message) — PRESENCE marks an extension, even when the
                // receiver is a type parameter (`fun <T> T.takeIf`) whose `class_name` is absent.
                has_receiver = true;
                let n = pb.varint()? as usize;
                let tbody = pb.bytes(n)?;
                receiver_body = Some(tbody.to_vec());
            }
            (7, 0) => return_type_id = pb.varint(),
            (8, 0) => {
                has_receiver = true;
                receiver_type_id = pb.varint();
            }
            (10, 2) => {
                let n = pb.varint()? as usize;
                context_receiver_bodies.push(pb.bytes(n)?.to_vec());
            }
            (11, 0) => context_receiver_type_ids.push(pb.varint()?),
            (11, 2) => {
                let n = pb.varint()? as usize;
                context_receiver_type_ids.extend(packed_varints(pb.bytes(n)?));
            }
            (6, 2) => {
                // value_parameter (repeated `ValueParameter`) — the SOURCE value parameters. Their count
                // and types are the Kotlin signature, WITHOUT the synthetic params a codegen pass appends
                // to the JVM descriptor (a `suspend`'s `Continuation`, a `@Composable`'s `Composer`/`int`).
                // `ValueParameter.type = 3` is an inline `Type`; recover its `class_name` id.
                let n = pb.varint()? as usize;
                if let Some(parameter) = parse_value_parameter(pb.bytes(n)?) {
                    value_params.push(parameter);
                }
            }
            (100, 2) => {
                // method_signature extension
                let n = pb.varint()? as usize;
                let ext = pb.bytes(n)?;
                jvm_sig = parse_jvm_signature(ext);
            }
            (_, w) => pb.skip(w)?,
        }
    }
    let flags = modern_flags.or(legacy_flags).unwrap_or(6);
    if contract_body.is_some() {
        crate::trace_compiler!(
            "metadata",
            "parsed contract function name_id={} fields={:?} values={} context_receivers={} context_params={}",
            name_id,
            seen_fields,
            value_params.len(),
            context_receiver_bodies.len() + context_receiver_type_ids.len(),
            context_params.len(),
        );
    }
    Some(ParsedFunction {
        is_inline: flags & IS_INLINE_BIT != 0,
        is_suspend: flags & IS_SUSPEND_BIT != 0,
        is_abstract: (flags >> 4) & 0x3 == 2,
        is_final: (flags >> 4) & 0x3 == 0,
        is_synthesized: (flags >> 6) & 0x3 == 3,
        is_operator: flags & IS_OPERATOR_BIT != 0,
        is_infix: flags & IS_INFIX_BIT != 0,
        visibility: crate::types::Visibility::from_metadata(flags_visibility(flags)),
        name_id,
        jvm_sig,
        has_receiver,
        value_params,
        type_params,
        return_body,
        return_type_id,
        receiver_body,
        receiver_type_id,
        annotation_bodies,
        contract_body,
        type_table_body,
        context_receiver_bodies,
        context_receiver_type_ids,
        context_params,
    })
}

/// The declaration facts carried directly by one Kotlin metadata `Type` message.
///
/// Nullability and suspend-function identity live in the same protobuf node. Decode them in one walk
/// so a value-parameter consumer cannot accidentally read one from an inline type and the other from
/// a type-table entry, or duplicate the wire parser as new type flags are added.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ParsedTypeFacts {
    pub nullable: bool,
    pub suspend_fun: bool,
}

pub fn parse_type_facts(body: &[u8]) -> ParsedTypeFacts {
    let mut pb = Pb { b: body, i: 0 };
    let mut facts = ParsedTypeFacts::default();
    while !pb.at_end() {
        let Some(tag) = pb.varint() else { break };
        match (tag >> 3, tag & 7) {
            // `Type.flags` bit zero is `SUSPEND_TYPE`. A suspend function type otherwise has the
            // same CPS-erased `FunctionN+1<..., Continuation<R>, Any?>` signature as an ordinary
            // continuation-taking function, so this bit is the semantic discriminator.
            (1, 0) => facts.suspend_fun = pb.varint().is_some_and(|v| v & 1 != 0),
            (3, 0) => facts.nullable = pb.varint().is_some_and(|v| v != 0),
            (_, w) => {
                if pb.skip(w).is_none() {
                    break;
                }
            }
        }
    }
    facts
}

/// Whether a `Type` message is nullable (`Type.nullable = 3`, a varint bool). Kept as the small
/// query used throughout the decoder; the wire interpretation itself remains centralized above.
pub fn parse_type_nullable(body: &[u8]) -> bool {
    parse_type_facts(body).nullable
}

pub fn metadata_class_kind(flags: u64) -> TypeKind {
    match (flags >> 6) & 0x7 {
        1 => TypeKind::Interface,
        2 => TypeKind::Enum,
        4 => TypeKind::Annotation,
        5 | 6 => TypeKind::Object,
        _ => TypeKind::Class,
    }
}

/// One `QualifiedNameTable.QualifiedName`: parent id (`-1` at the root), short-name id into the
/// `StringTable`, and kind (`0` CLASS, `1` PACKAGE, `2` LOCAL; default PACKAGE).
pub struct QName {
    pub parent: i64,
    pub short: usize,
    pub kind: u64,
}

pub fn parse_qname(body: &[u8]) -> QName {
    let mut pb = Pb { b: body, i: 0 };
    let mut q = QName {
        parent: -1,
        short: 0,
        kind: 1,
    };
    while !pb.at_end() {
        let Some(tag) = pb.varint() else { break };
        match (tag >> 3, tag & 7) {
            (1, 0) => q.parent = pb.varint().map(|v| v as i64).unwrap_or(-1),
            (2, 0) => q.short = pb.varint().unwrap_or(0) as usize,
            (3, 0) => q.kind = pb.varint().unwrap_or(1),
            (_, w) => {
                if pb.skip(w).is_none() {
                    break;
                }
            }
        }
    }
    q
}

/// Resolve a `QualifiedNameTable` id to its internal name, mirroring `NameResolverImpl.traverseIds`:
/// walk the parent chain, prepending each segment, joining PACKAGE segments with `/` and the relative
/// CLASS segments with `.`, then `package/Relative.Class` (`kotlin/collections/MutableList`).
pub fn resolve_qname(qnames: &[QName], strings: &[String], mut idx: i64) -> String {
    let mut pkg: Vec<&str> = Vec::new();
    let mut cls: Vec<&str> = Vec::new();
    while idx != -1 {
        let Some(q) = qnames.get(idx as usize) else {
            break;
        };
        let Some(name) = strings.get(q.short) else {
            break;
        };
        if q.kind == 1 {
            pkg.insert(0, name);
        } else {
            cls.insert(0, name);
        }
        idx = q.parent;
    }
    let c = cls.join(".");
    if pkg.is_empty() {
        c
    } else {
        format!("{}/{c}", pkg.join("/"))
    }
}

/// Drop the `BuiltInsBinaryVersion` header, returning the `PackageFragment` proto bytes.
pub fn strip_builtins_header(data: &[u8]) -> Option<&[u8]> {
    let count = u32::from_be_bytes(*data.get(0..4)?.first_chunk::<4>()?) as usize;
    data.get(4 + 4 * count..)
}

/// A type decoded from a `.kotlin_builtins` fragment. A bare internal name cannot express the two
/// facets the fragment actually records — a class's type ARGUMENTS (`Set<Map.Entry<K, V>>`) and a
/// reference to a declared type PARAMETER (`E` of `List<E>`) — so both are modelled here. Class names
/// are Kotlin internal names (`kotlin/Int`, `kotlin/collections/Map.Entry`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BuiltinTy {
    Class {
        internal: String,
        args: Vec<BuiltinTy>,
        nullable: bool,
    },
    Param {
        name: String,
        nullable: bool,
    },
    InProjection(Box<BuiltinTy>),
    OutProjection(Box<BuiltinTy>),
}

impl BuiltinTy {
    pub fn class(internal: impl Into<String>) -> BuiltinTy {
        BuiltinTy::Class {
            internal: internal.into(),
            args: Vec::new(),
            nullable: false,
        }
    }

    /// The declared internal name when this is a class type, `None` for a type parameter.
    pub fn internal(&self) -> Option<&str> {
        match self {
            BuiltinTy::Class { internal, .. } => Some(internal),
            BuiltinTy::Param { .. } | BuiltinTy::InProjection(_) | BuiltinTy::OutProjection(_) => {
                None
            }
        }
    }

    pub fn nullable(&self) -> bool {
        match self {
            BuiltinTy::Class { nullable, .. } | BuiltinTy::Param { nullable, .. } => *nullable,
            BuiltinTy::InProjection(_) | BuiltinTy::OutProjection(_) => false,
        }
    }

    /// A readable source-shaped rendering (`kotlin/collections/Set<kotlin/collections/Map.Entry<K,V>>`).
    pub fn render(&self) -> String {
        let (base, args, nullable) = match self {
            BuiltinTy::Class {
                internal,
                args,
                nullable,
            } => (internal.clone(), args.as_slice(), *nullable),
            BuiltinTy::Param { name, nullable } => (name.clone(), &[][..], *nullable),
            BuiltinTy::InProjection(inner) => return format!("in {}", inner.render()),
            BuiltinTy::OutProjection(inner) => return format!("out {}", inner.render()),
        };
        let mut out = base;
        if !args.is_empty() {
            let inner: Vec<String> = args.iter().map(BuiltinTy::render).collect();
            out.push('<');
            out.push_str(&inner.join(","));
            out.push('>');
        }
        if nullable {
            out.push('?');
        }
        out
    }
}

pub fn project_builtin_ty(projection: ParsedProjection, ty: BuiltinTy) -> BuiltinTy {
    match projection {
        ParsedProjection::In => BuiltinTy::InProjection(Box::new(ty)),
        ParsedProjection::Out => BuiltinTy::OutProjection(Box::new(ty)),
        ParsedProjection::Invariant => ty,
    }
}

/// One member of a builtins `Class`: its Kotlin name, value-parameter types, and return type, each
/// decoded through the fragment's type table.
pub struct BuiltinMember {
    pub name: String,
    pub params: Vec<BuiltinTy>,
    pub ret: BuiltinTy,
    pub is_property: bool,
    /// Kotlin's `operator` modifier from the function flags. Properties never set it.
    pub is_operator: bool,
    /// Kotlin's `infix` modifier from the function flags. Properties never set it.
    pub is_infix: bool,
    /// Semantic declaration modality from the builtins protobuf. A mapped JVM method with the same
    /// descriptor is only the physical realization and cannot replace this Kotlin fact.
    pub is_abstract: bool,
    /// The member's OWN type parameters (`<R>` of `fold`), with their declared upper bounds — kept
    /// apart from the class's so a consumer can build a generic signature whose formals shadow
    /// correctly.
    pub formals: Vec<BuiltinTypeParam>,
    /// Whether the declared return type is nullable (`V?`) — the JVM descriptor erases it, only the
    /// `.kotlin_builtins` `Type.nullable` flag carries it (`Map.get(K): V?`, `firstOrNull(): T?`).
    pub ret_nullable: bool,
    /// Source names of the value parameters, parallel to [`Self::params`]. Empty entries where the
    /// fragment records no name.
    ///
    /// A named argument needs these, and a descriptor cannot supply them.
    /// [`BuiltinConstructor`] has always carried them; a member did not, because the builtins
    /// consumer this decode was written for needed only types.
    pub param_names: Vec<String>,
    /// Which value parameters declare a default, parallel to [`Self::params`].
    pub param_defaults: Vec<bool>,
    /// Declared receiver of a MEMBER EXTENSION (`class Scope { fun String.f() }`, `val String.p`).
    /// `None` for an ordinary member. Nothing else records it: the declaring class is the dispatch
    /// receiver and the extension receiver is a separate declaration fact.
    pub receiver: Option<BuiltinTy>,
    /// A property declared `var`. Only the property flag word carries it, and without it a `var`
    /// reads as a `val` — every write against it would be rejected.
    pub is_var: bool,
    /// A property declared `const`. Functions never set it.
    pub is_const: bool,
    /// Source visibility, from the declaration's own flag word. The Kotlin/Native stdlib's linkdata
    /// carries 293 private, 67 protected and 84 internal member functions; reporting them all as
    /// public would let a source call declarations the library does not expose.
    pub visibility: Visibility,
    /// Annotation class identities declared on this member, by internal name. Identities only: that
    /// is what the member layer records, and it is what decides `@Deprecated`, `@PublishedApi` and
    /// the opt-in markers. The Native stdlib annotates 952 of its 2964 member functions, 90 of them
    /// `@Deprecated`.
    pub annotations: Vec<String>,
    /// A property's SETTER visibility, when the declaration states one of its own
    /// (`Property.setter_flags` = 8). `None` when the property declares no separate setter, in
    /// which case the setter is as visible as the property. `var guarded: Int = 1; private set`
    /// records 66 here — private in bits 1-3 plus the `isNotDefault` bit — while a plain `var`
    /// omits the field entirely.
    pub setter_visibility: Option<Visibility>,
}

/// `Class.flags` bit 9, `IS_INNER`: an `inner class`, which captures its outer instance.
pub const IS_INNER_CLASS_BIT: u64 = 1 << 9;
/// `Class.flags` bit 13, `IS_VALUE`: a `value class`.
pub const IS_VALUE_CLASS_BIT: u64 = 1 << 13;
/// `Class.flags` bit 14, `IS_FUN`: a `fun interface`, the declaration bit a Kotlin interface needs
/// before it may be SAM-converted. Read off a klib the reference compiler wrote for
/// `fun interface Handler`, whose flag word is 16486.
pub const IS_FUN_INTERFACE_BIT: u64 = 1 << 14;

/// One top-level function declared by a `.kotlin_builtins` package fragment. Unlike a class member,
/// it has no JVM facade method: it is a semantic compiler builtin whose physical realization is a
/// backend capability. Resolution still needs its complete source signature.
pub struct BuiltinFunction {
    pub name: String,
    pub receiver: Option<BuiltinTy>,
    pub params: Vec<BuiltinTy>,
    pub ret: BuiltinTy,
    pub formals: Vec<BuiltinTypeParam>,
    pub param_names: Vec<String>,
    pub param_defaults: Vec<bool>,
    pub vararg: Option<usize>,
    pub visibility: crate::types::Visibility,
    pub is_inline: bool,
    pub has_reified_type_params: bool,
    pub is_suspend: bool,
    pub is_operator: bool,
    pub is_infix: bool,
    /// Old unnamed context receivers followed by named context parameters. Both are leading
    /// implicit parameters in the semantic signature; only the latter have source names.
    pub context_count: usize,
    /// Annotation class identities declared on this function, by internal name.
    pub annotations: Vec<String>,
}

/// One top-level `typealias` (`Package.typeAlias` = 5).
///
/// The field numbers came off a klib the reference compiler wrote: `name` = 2, `typeParameter` = 3,
/// `underlyingTypeId` = 5, `expandedTypeId` = 7. `typealias Plain = PBox<Int, String>` records the
/// same id in 5 and 7; `typealias Chain = Boxed<Int>` records the spelled `Boxed<Int>` in 5 and the
/// expanded `PBox<Int, Int>` in 7, so the two are genuinely distinct facts.
pub struct BuiltinTypeAlias {
    pub name: String,
    /// The alias's own type parameters, in declaration order — the substitution domain.
    pub type_params: Vec<BuiltinTypeParam>,
    /// The right-hand side EXPANDED: the target applied to its arguments, with the alias's own
    /// parameters still symbolic.
    pub expanded: BuiltinTy,
}

#[derive(Default)]
pub struct BuiltinPackage {
    pub classes: std::collections::HashMap<String, BuiltinClass>,
    pub functions: Vec<BuiltinFunction>,
    /// Top-level properties (`Package.property` = 4), including extension properties, which carry
    /// their declared receiver. `kotlin.math.PI` is one of these and nothing else records it.
    pub properties: Vec<BuiltinMember>,
    /// Top-level type aliases (`Package.typeAlias` = 5). The Kotlin/Native stdlib ships 37 of them;
    /// each one is a name the library exports that resolves to nothing without this.
    pub type_aliases: Vec<BuiltinTypeAlias>,
}

/// One constructor declared by a builtin class. Unlike a function it has no return type or name;
/// its value-parameter types and source visibility are the complete semantic signature.
pub struct BuiltinConstructor {
    pub params: Vec<BuiltinTy>,
    pub param_names: Vec<String>,
    pub param_defaults: Vec<bool>,
    pub vararg: Option<usize>,
    pub visibility: crate::types::Visibility,
}

/// One declared type parameter of a builtin class or member: its source name and decoded upper bounds
/// (`E` unbounded, `T : Comparable<T>`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuiltinTypeParam {
    pub name: String,
    pub bounds: Vec<BuiltinTy>,
    pub variance: crate::types::TypeVariance,
    pub only_input: bool,
}

/// A builtin `Class` decoded from a `.kotlin_builtins` fragment: its direct supertypes and declared
/// members — the two facets the front end needs (the read-only/mutable hierarchy AND each type's API).
pub struct BuiltinClass {
    pub supertypes: Vec<String>,
    /// The supertypes WITH their type arguments (`MutableList<E> : List<E>`), which the name-only
    /// `supertypes` list cannot carry — the chain a receiver's type argument travels up.
    pub supertype_tys: Vec<BuiltinTy>,
    pub members: Vec<BuiltinMember>,
    pub constructors: Vec<BuiltinConstructor>,
    /// The declared companion object's simple source name (`Companion`, or a named companion), from
    /// `Class.companion_object_name` (field 4).
    pub companion_name: Option<String>,
    /// The class's own type parameters, in declaration order (`Map` → `[K, V]`).
    pub type_params: Vec<BuiltinTypeParam>,
    /// Whether the builtin is an interface (`List`, `CharSequence`, `Comparable`) vs a class (`Number`,
    /// `Enum`) — from the `@Metadata` `CLASS_KIND` flag. Needed when reporting a classless builtin whose
    /// JVM class is absent (a no-JDK compile), so member calls emit the right invoke opcode.
    pub kind: TypeKind,
    /// Source visibility from the metadata flag word. This is deliberately separate from `access`:
    /// Kotlin `internal` declarations are public in classfiles after name mangling.
    pub visibility: Visibility,
    /// Kotlin metadata's `IS_EXPECT_CLASS` declaration flag. KLIB consumers use this semantic bit
    /// to distinguish common declarations from platform-only classifiers in the same archive.
    pub is_expect: bool,
    pub is_nested: bool,
    /// The entry names an `enum class` declares, in declaration order (`Class.enumEntry` = 13, each
    /// an `EnumEntry` whose field 1 is a string-table index). Empty for a non-enum. Without them
    /// `Color.RED` resolves to nothing.
    pub enum_entries: Vec<String>,
    /// The direct subclasses a `sealed` declaration names (`Class.sealedSubclassFqName` = 16, a
    /// PACKED list of qualified-name ids). Empty for a non-sealed declaration. An exhaustive `when`
    /// over a dependency's sealed type is proven from these.
    pub sealed_subclasses: Vec<String>,
    /// Source name of a `value class`'s sole underlying property
    /// (`Class.inlineClassUnderlyingPropertyName` = 17). `None` for an ordinary class. The Native
    /// stdlib has 17 value classes and exactly 17 classes carrying this field; none carries
    /// `inlineClassUnderlyingTypeId` (18), so the underlying TYPE comes from the named property's
    /// own declaration rather than from a second record.
    pub value_underlying_property: Option<String>,
    /// A `fun interface` — the declaration bit that makes a Kotlin interface SAM-convertible.
    pub is_fun_interface: bool,
    /// An `inner class`, which captures an instance of its enclosing class. A plain nested class
    /// does not, and the two are otherwise recorded identically.
    pub is_inner: bool,
    /// The declaration's raw `Class.flags` word, as the fragment records it.
    ///
    /// The Kotlin facts this reader can name — [`Self::kind`], [`Self::visibility`],
    /// [`Self::is_expect`] — are decoded above. The word itself travels on because what it means for
    /// a TARGET is that backend's answer: the JVM reads it for the access mask an `InnerClasses`
    /// entry must carry when the mapped JVM owner has no class file to read it off, and that mapping
    /// is no more universal than any other representation decision.
    pub flags: u64,
    /// Nullable returns for declared function members keyed by `(name, value-arity)` (`Map.get(K): V?`,
    /// `firstOrNull(): T?`). A call may still resolve to the ERASED classpath method (`java/util/Map.get`
    /// returns `Object`), which carries no Kotlin nullability — this is then the only surviving record
    /// that the source return is `T?`. Consulted by the member walk to null-annotate that resolved return.
    pub nullable_member_returns: Vec<(String, usize)>,
}

/// The tables a `.kotlin_builtins` `Class` resolves its types against: the fragment's string and
/// qualified-name tables plus the class's own `type_table` (`Class.type_table` = field 30).
pub struct BuiltinTables<'a> {
    pub strings: &'a [String],
    pub qnames: &'a [QName],
    pub types: &'a [&'a [u8]],
}

/// A `TypeParameter.id` → source name map. A builtins `Type` names a type parameter by that id
/// (`Type.type_parameter` = field 7); without the map the type is undecodable and the whole member
/// used to be dropped.
pub type TypeParamNames = std::collections::HashMap<u64, String>;

/// `ValueParameter.flags` DECLARES_DEFAULT_VALUE, bit 1 — the `= "box"` of `describe(prefix: String
/// = "box")`. Only the declaration records it; a JVM descriptor cannot.
const DECLARES_DEFAULT_VALUE: u64 = 1 << 1;

/// One decoded `ValueParameter`: its type, and the two facts a CALL SITE needs that a descriptor
/// erases — the parameter's source name, for a named argument, and whether it declares a default.
pub struct BuiltinValueParameter {
    pub ty: BuiltinTy,
    /// The source name, or empty when the fragment records none.
    pub name: String,
    pub declares_default: bool,
}

/// Decode the type of one builtins `ValueParameter`. Functions and constructors use the same
/// message; keeping one reader prevents their accepted type-table layouts from drifting.
pub fn builtin_value_parameter_type(
    body: &[u8],
    tables: &BuiltinTables<'_>,
    tparams: &TypeParamNames,
) -> Option<BuiltinTy> {
    builtin_value_parameter(body, tables, tparams, &[]).map(|parameter| parameter.ty)
}

/// [`builtin_value_parameter_type`] with the name and default the same message carries.
///
/// `strings` resolves `name` (field 2), which is a string-table index. Pass an empty table to decode
/// the type alone — a consumer that only needs types should call
/// [`builtin_value_parameter_type`] instead.
pub fn builtin_value_parameter(
    body: &[u8],
    tables: &BuiltinTables<'_>,
    tparams: &TypeParamNames,
    strings: &[String],
) -> Option<BuiltinValueParameter> {
    let mut value = Pb { b: body, i: 0 };
    let mut ty = None;
    let mut name = String::new();
    let mut declares_default = false;
    while !value.at_end() {
        let tag = value.varint()?;
        match (tag >> 3, tag & 7) {
            (1, 0) => {
                declares_default = value.varint()? & DECLARES_DEFAULT_VALUE != 0;
            }
            (2, 0) => {
                if let Some(id) = value.varint() {
                    if let Some(found) = strings.get(id as usize) {
                        name = found.clone();
                    }
                }
            }
            // `type_id` is field 5 in the current builtins schema and field 4 in older fragments.
            (5, 0) | (4, 0) => {
                ty = value
                    .varint()
                    .and_then(|id| tables.ty_by_id(id as usize, tparams, 0));
            }
            // Inline `type`.
            (3, 2) => {
                let len = value.varint()? as usize;
                ty = value
                    .bytes(len)
                    .and_then(|type_body| tables.ty(type_body, tparams, 0));
            }
            (_, wire) => value.skip(wire)?,
        }
    }
    Some(BuiltinValueParameter {
        ty: ty?,
        name,
        declares_default,
    })
}

/// Decode either wire representation of a metadata type. Producers may inline the `Type` message or
/// reference the enclosing type table; consumers must treat those as the same declaration shape.
pub fn builtin_type_ref(
    inline: Option<&[u8]>,
    table_id: Option<u64>,
    tables: &BuiltinTables<'_>,
    tparams: &TypeParamNames,
) -> Option<BuiltinTy> {
    inline
        .and_then(|body| tables.ty(body, tparams, 0))
        .or_else(|| table_id.and_then(|id| tables.ty_by_id(id as usize, tparams, 0)))
}

/// How deep a `.kotlin_builtins` type may nest before the decode gives up — a type-table entry
/// references other entries by id, so a malformed (or cyclic) fragment must not recurse forever.
pub const BUILTIN_TYPE_DEPTH_LIMIT: u32 = 16;

impl BuiltinTables<'_> {
    /// Resolve the shared [`parse_type_node`] wire shape through a builtins fragment's tables. A type is
    /// `class_name` (field 6) with `argument`s, `type_parameter` (field 7, by id), or
    /// `type_parameter_name` (field 9, by string). An argument may carry its type inline or by table id;
    /// builtins commonly use the latter, so those edges consume the recursion budget as well.
    /// The annotation class identities in a repeated `Annotation` field.
    pub fn annotation_identities<'b>(
        &self,
        bodies: impl IntoIterator<Item = &'b [u8]>,
    ) -> Vec<String> {
        bodies
            .into_iter()
            .filter_map(annotation_class_id)
            .map(|id| resolve_qname(self.qnames, self.strings, id as i64))
            .collect()
    }

    pub fn ty(&self, body: &[u8], tparams: &TypeParamNames, depth: u32) -> Option<BuiltinTy> {
        if depth > BUILTIN_TYPE_DEPTH_LIMIT {
            return None;
        }
        let node = parse_type_node(body)?;
        let args = node
            .arguments
            .into_iter()
            .map(|argument| match argument {
                ParsedTypeArgument::Inline(body, projection) => self
                    .ty(body, tparams, depth + 1)
                    .map(|ty| project_builtin_ty(projection, ty)),
                ParsedTypeArgument::Table(id, projection) => usize::try_from(id)
                    .ok()
                    .and_then(|id| self.ty_by_id(id, tparams, depth + 1))
                    .map(|ty| project_builtin_ty(projection, ty)),
                ParsedTypeArgument::Star => {
                    Some(BuiltinTy::OutProjection(Box::new(BuiltinTy::Class {
                        internal: "kotlin/Any".to_string(),
                        args: Vec::new(),
                        nullable: true,
                    })))
                }
            })
            .collect::<Option<Vec<_>>>()?;
        if let Some(id) = node.class_id {
            return Some(BuiltinTy::Class {
                internal: resolve_qname(self.qnames, self.strings, id as i64),
                args,
                nullable: node.nullable,
            });
        }
        let name = match (node.type_parameter_id, node.type_parameter_name_id) {
            (Some(id), _) => tparams.get(&id).cloned()?,
            (None, Some(sid)) => self.strings.get(sid as usize).cloned()?,
            (None, None) => return None,
        };
        Some(BuiltinTy::Param {
            name,
            nullable: node.nullable,
        })
    }

    pub fn ty_by_id(&self, id: usize, tparams: &TypeParamNames, depth: u32) -> Option<BuiltinTy> {
        self.ty(self.types.get(id)?, tparams, depth)
    }

    /// Decode a run of `TypeParameter` messages: their names (added to `tparams` so a bound may refer
    /// to a sibling) and their upper bounds. Bounds are decoded against the names alone — a recursive
    /// bound (`T : Comparable<T>`) therefore terminates instead of chasing itself.
    pub fn type_params(
        &self,
        bodies: &[&[u8]],
        tparams: &mut TypeParamNames,
    ) -> Vec<BuiltinTypeParam> {
        let parsed: Vec<ParsedTypeParam> = bodies
            .iter()
            .filter_map(|b| parse_type_param(b))
            .filter(|tp| self.strings.get(tp.name_id as usize).is_some())
            .collect();
        for tp in &parsed {
            tparams.insert(tp.id, self.strings[tp.name_id as usize].clone());
        }
        parsed
            .iter()
            .map(|tp| BuiltinTypeParam {
                name: self.strings[tp.name_id as usize].clone(),
                variance: tp.variance,
                only_input: self
                    .annotation_identities(tp.annotation_bodies.iter().map(Vec::as_slice))
                    .iter()
                    .any(|identity| identity == "kotlin/internal/OnlyInputTypes"),
                bounds: tp
                    .upper_bound_ids
                    .iter()
                    .filter_map(|&id| self.ty_by_id(id as usize, tparams, 0))
                    .chain(
                        tp.upper_bound_bodies
                            .iter()
                            .filter_map(|body| self.ty(body, tparams, 0)),
                    )
                    .collect(),
            })
            .collect()
    }
}

/// `Class.type_parameter`. Field 5 on a `Class` — where a `Function`/`Property` instead carries its
/// `receiver_type`, hence the two distinct constants.
pub const CLASS_TYPE_PARAMETER_FIELD: u64 = 5;

/// `Function.type_parameter` / `Property.type_parameter`. Both are field 4 (matching the decoders in
/// [`class_functions`] and [`class_properties`]); field 5 on those messages is `receiver_type`.
pub const MEMBER_TYPE_PARAMETER_FIELD: u64 = 4;

/// `TypeAlias.typeParameter` = 3, its own carrier again. Read off a klib the reference compiler
/// wrote for `typealias Boxed<T> = PBox<T, T>`, whose alias records field 3 holding
/// `TypeParameter { id: 0, name: "T" }`.
pub const TYPE_ALIAS_TYPE_PARAMETER_FIELD: u64 = 3;

/// Collect a message's repeated `type_parameter` sub-message bodies. The field number differs by
/// carrier — see [`CLASS_TYPE_PARAMETER_FIELD`] / [`MEMBER_TYPE_PARAMETER_FIELD`].
pub fn type_param_bodies(body: &[u8], field: u64) -> Vec<&[u8]> {
    let mut pb = Pb { b: body, i: 0 };
    let mut out = Vec::new();
    while !pb.at_end() {
        let Some(tag) = pb.varint() else { break };
        match (tag >> 3, tag & 7) {
            (f, 2) if f == field => {
                let Some(n) = pb.varint() else { break };
                let Some(b) = pb.bytes(n as usize) else { break };
                out.push(b);
            }
            (_, w) => {
                if pb.skip(w).is_none() {
                    break;
                }
            }
        }
    }
    out
}

pub fn parse_builtin_package_functions(
    package: &[u8],
    strings: &[String],
    qnames: &[QName],
) -> Vec<BuiltinFunction> {
    parse_builtin_package(package, strings, qnames).functions
}

/// Everything a `Package` message declares at top level.
pub struct PackageDeclarations {
    pub functions: Vec<BuiltinFunction>,
    pub properties: Vec<BuiltinMember>,
    pub type_aliases: Vec<BuiltinTypeAlias>,
}

/// One `TypeAlias` declaration, read against its package's type table.
///
/// The alias's own type parameters are registered before the right-hand side is read, so
/// `typealias Boxed<T> = PBox<T, T>` resolves both `T`s to the alias's parameter rather than
/// leaving them unbound. The EXPANDED side (field 7) is what a use site substitutes into; the
/// spelled side (field 5) differs only for an alias whose right-hand side names another alias, and
/// that abbreviation lives in `Type.abbreviatedTypeId` (field 14), which is not read yet — an
/// expansion is therefore reported unabbreviated, which is a presentation loss and not a semantic
/// one.
fn builtin_type_alias(body: &[u8], tables: &BuiltinTables) -> Option<BuiltinTypeAlias> {
    let mut tparams = TypeParamNames::new();
    let type_params = tables.type_params(
        &type_param_bodies(body, TYPE_ALIAS_TYPE_PARAMETER_FIELD),
        &mut tparams,
    );
    let mut p = Pb { b: body, i: 0 };
    let mut name_id = None;
    let mut expanded_body = None;
    let mut expanded_id = None;
    while !p.at_end() {
        let tag = p.varint()?;
        match (tag >> 3, tag & 7) {
            (2, 0) => name_id = p.varint(),
            (6, 2) => {
                let len = p.varint()?;
                expanded_body = p.bytes(len as usize);
            }
            (7, 0) => expanded_id = p.varint(),
            (_, w) => {
                p.skip(w)?;
            }
        }
    }
    Some(BuiltinTypeAlias {
        name: tables.strings.get(name_id? as usize).cloned()?,
        type_params,
        expanded: builtin_type_ref(expanded_body, expanded_id, tables, &tparams)?,
    })
}

/// A `Package` message's top-level declarations, read against its own type table.
pub fn parse_builtin_package(
    package: &[u8],
    strings: &[String],
    qnames: &[QName],
) -> PackageDeclarations {
    let mut functions = Vec::new();
    let mut property_bodies: Vec<&[u8]> = Vec::new();
    let mut alias_bodies: Vec<&[u8]> = Vec::new();
    let mut types = Vec::new();
    let mut package_message = Pb { b: package, i: 0 };
    while !package_message.at_end() {
        let Some(tag) = package_message.varint() else {
            break;
        };
        match (tag >> 3, tag & 7) {
            (3, 2) => {
                let Some(len) = package_message.varint() else {
                    break;
                };
                let Some(body) = package_message.bytes(len as usize) else {
                    break;
                };
                functions.push(body);
            }
            // `Package.property` = 4 — the same `Property` message a class member carries.
            (4, 2) => {
                let Some(len) = package_message.varint() else {
                    break;
                };
                let Some(body) = package_message.bytes(len as usize) else {
                    break;
                };
                property_bodies.push(body);
            }
            // `Package.typeAlias` = 5.
            (5, 2) => {
                let Some(len) = package_message.varint() else {
                    break;
                };
                let Some(body) = package_message.bytes(len as usize) else {
                    break;
                };
                alias_bodies.push(body);
            }
            (30, 2) => {
                let Some(len) = package_message.varint() else {
                    break;
                };
                let Some(table) = package_message.bytes(len as usize) else {
                    break;
                };
                let mut table = Pb { b: table, i: 0 };
                while !table.at_end() {
                    let Some(tag) = table.varint() else { break };
                    match (tag >> 3, tag & 7) {
                        (1, 2) => {
                            let Some(len) = table.varint() else { break };
                            let Some(ty) = table.bytes(len as usize) else {
                                break;
                            };
                            types.push(ty);
                        }
                        (_, wire) => {
                            if table.skip(wire).is_none() {
                                break;
                            }
                        }
                    }
                }
            }
            (_, wire) => {
                if package_message.skip(wire).is_none() {
                    break;
                }
            }
        }
    }

    let tables = BuiltinTables {
        strings,
        qnames,
        types: &types,
    };
    let properties = property_bodies
        .into_iter()
        .filter_map(|body| builtin_property(body, &tables, &TypeParamNames::new()))
        .collect();
    let type_aliases = alias_bodies
        .into_iter()
        .filter_map(|body| builtin_type_alias(body, &tables))
        .collect();
    let functions = functions
        .into_iter()
        .filter_map(|body| {
            let function = parse_function(body)?;
            let mut tparams = TypeParamNames::new();
            let formals = tables.type_params(
                &type_param_bodies(body, MEMBER_TYPE_PARAMETER_FIELD),
                &mut tparams,
            );
            let mut params = Vec::new();
            let mut param_names = Vec::new();
            let mut param_defaults = Vec::new();
            let mut vararg = None;
            let context_count = if function.context_params.is_empty() {
                function.context_receiver_bodies.len() + function.context_receiver_type_ids.len()
            } else {
                function.context_params.len()
            };
            if function.context_params.is_empty() {
                for ty in function
                    .context_receiver_bodies
                    .iter()
                    .map(|body| tables.ty(body, &tparams, 0))
                    .chain(
                        function
                            .context_receiver_type_ids
                            .iter()
                            .map(|&id| tables.ty_by_id(id as usize, &tparams, 0)),
                    )
                {
                    params.push(ty?);
                    param_names.push(String::new());
                    param_defaults.push(false);
                }
            }
            let values = function.context_params.iter().chain(&function.value_params);
            for value in values {
                let parameter_index = params.len();
                let vararg_element = value
                    .vararg_elem_body
                    .as_deref()
                    .and_then(|body| tables.ty(body, &tparams, 0))
                    .or_else(|| {
                        value
                            .vararg_elem_id
                            .and_then(|id| tables.ty_by_id(id as usize, &tparams, 0))
                    });
                let ty = if let Some(element) = vararg_element {
                    vararg = Some(parameter_index);
                    match &element {
                        BuiltinTy::Class {
                            internal,
                            args,
                            nullable: false,
                        } if args.is_empty()
                            && matches!(
                                internal.as_str(),
                                "kotlin/Boolean"
                                    | "kotlin/Byte"
                                    | "kotlin/Char"
                                    | "kotlin/Double"
                                    | "kotlin/Float"
                                    | "kotlin/Int"
                                    | "kotlin/Long"
                                    | "kotlin/Short"
                            ) =>
                        {
                            BuiltinTy::class(format!("{internal}Array"))
                        }
                        _ => BuiltinTy::Class {
                            internal: "kotlin/Array".to_string(),
                            args: vec![element],
                            nullable: false,
                        },
                    }
                } else {
                    value
                        .type_body
                        .as_deref()
                        .and_then(|body| tables.ty(body, &tparams, 0))
                        .or_else(|| {
                            value
                                .type_id
                                .and_then(|id| tables.ty_by_id(id as usize, &tparams, 0))
                        })?
                };
                params.push(ty);
                param_names.push(
                    strings
                        .get(value.name_id as usize)
                        .cloned()
                        .unwrap_or_else(|| format!("p{parameter_index}")),
                );
                param_defaults.push(value.has_default);
            }
            let ret = function
                .return_body
                .as_deref()
                .and_then(|body| tables.ty(body, &tparams, 0))
                .or_else(|| {
                    function
                        .return_type_id
                        .and_then(|id| tables.ty_by_id(id as usize, &tparams, 0))
                })?;
            let receiver = function
                .receiver_body
                .as_deref()
                .and_then(|body| tables.ty(body, &tparams, 0))
                .or_else(|| {
                    function
                        .receiver_type_id
                        .and_then(|id| tables.ty_by_id(id as usize, &tparams, 0))
                });
            Some(BuiltinFunction {
                name: strings.get(function.name_id as usize)?.clone(),
                receiver,
                params,
                ret,
                formals,
                param_names,
                param_defaults,
                vararg,
                visibility: function.visibility,
                is_inline: function.is_inline,
                has_reified_type_params: function
                    .type_params
                    .iter()
                    .any(|parameter| parameter.reified),
                is_suspend: function.is_suspend,
                is_operator: function.is_operator,
                is_infix: function.is_infix,
                context_count,
                annotations: tables
                    .annotation_identities(function.annotation_bodies.iter().map(Vec::as_slice)),
            })
        })
        .collect();
    PackageDeclarations {
        functions,
        properties,
        type_aliases,
    }
}

/// Parse a `.kotlin_builtins` resource → every declared `Class` (qualified name → its supertypes +
/// members). ONE walk over the fragment's `StringTable`/`QualifiedNameTable`/`Class` tables; each
/// class's supertypes and member types are resolved through its `type_table` (field 30 → `Type
/// .class_name` → `QualifiedNameTable`). The single source for both the collection hierarchy and a
/// builtin type's API — no curated/hardcoded tables.
pub fn parse_builtins(data: &[u8]) -> BuiltinPackage {
    let Some(pf) = strip_builtins_header(data) else {
        return BuiltinPackage::default();
    };
    parse_package_fragment(pf)
}

/// The annotation class's own id, from an `Annotation` message's field 1 (a qualified-name id).
///
/// `Annotation` is `{ id = 1, argument = 2 }`. The arguments are deliberately not read here: the
/// member and callable layers record annotation IDENTITIES only, so an identity is the whole
/// answer at that boundary. Measured on the Kotlin/Native stdlib: 1449 annotations on member
/// functions, 607 of which carry arguments.
fn annotation_class_id(body: &[u8]) -> Option<u64> {
    let mut p = Pb { b: body, i: 0 };
    while !p.at_end() {
        let tag = p.varint()?;
        match (tag >> 3, tag & 7) {
            (1, 0) => return p.varint(),
            (_, w) => {
                p.skip(w)?;
            }
        }
    }
    None
}

/// `KlibMetadataProtoBuf`'s annotation extension, field 170, which a KLIB uses on a `Class`, a
/// `Function` and a `Property` alike. Read off `klib/common/stdlib`, where exactly the 492 classes
/// with the `hasAnnotations` flag carry it.
pub const KLIB_ANNOTATION_FIELD: u64 = 170;

/// One `EnumEntry`'s declared name./// One `EnumEntry`'s declared name. The message carries the name in field 1 as a string-table
/// index; a malformed entry with no name contributes nothing rather than an empty entry name.
fn enum_entry_name(body: &[u8], strings: &[String]) -> Option<String> {
    let mut p = Pb { b: body, i: 0 };
    while !p.at_end() {
        let tag = p.varint()?;
        match (tag >> 3, tag & 7) {
            (1, 0) => return strings.get(p.varint()? as usize).cloned(),
            (_, w) => {
                p.skip(w)?;
            }
        }
    }
    None
}

/// One `Property` declaration, from a class's member list or a package's top-level list.
///
/// Both carry the same message, so both read it here. The field numbers were taken off a klib the
/// reference compiler wrote rather than from memory: `val String.memberExt` records the tags 2
/// (name), 7 (`getter_flags` — NOT a type id, which is the mistake this comment exists to prevent),
/// 9 (`return_type_id`), 10 (`receiver_type_id`) and 176 (file), and the flag word is field 11 with
/// protobuf default [`crate::metadata::property_flags::DEFAULT`] — a `var` writes 1798, a `const
/// val` 10758, and a `val` with a custom getter omits the field entirely.
fn builtin_property(
    body: &[u8],
    tables: &BuiltinTables,
    outer: &TypeParamNames,
) -> Option<BuiltinMember> {
    use crate::metadata::property_flags;
    let mut tparams = outer.clone();
    let formals = tables.type_params(
        &type_param_bodies(body, MEMBER_TYPE_PARAMETER_FIELD),
        &mut tparams,
    );
    let mut p = Pb { b: body, i: 0 };
    let mut name_id = None;
    let mut ret_body = None;
    let mut ret_id = None;
    let mut recv_body = None;
    let mut recv_id = None;
    let mut legacy_flags = None;
    let mut modern_flags = None;
    let mut setter_flags = None;
    let mut annotation_bodies: Vec<&[u8]> = Vec::new();
    while !p.at_end() {
        let Some(tag) = p.varint() else { break };
        match (tag >> 3, tag & 7) {
            (1, 0) => legacy_flags = p.varint(),
            (2, 0) => name_id = p.varint(),
            (KLIB_ANNOTATION_FIELD, 2) => {
                let Some(len) = p.varint() else { break };
                if let Some(body) = p.bytes(len as usize) {
                    annotation_bodies.push(body);
                }
            }
            // `Property.setter_flags` = 8, present only when the declaration states a setter of its
            // own. A plain `var` omits it and its setter is as visible as the property.
            (8, 0) => setter_flags = p.varint(),
            (3, 2) => {
                let len = p.varint()?;
                ret_body = p.bytes(len as usize);
            }
            (5, 2) => {
                let len = p.varint()?;
                recv_body = p.bytes(len as usize);
            }
            (9, 0) => ret_id = p.varint(),
            (10, 0) => recv_id = p.varint(),
            (11, 0) => modern_flags = p.varint(),
            (_, w) => {
                p.skip(w)?;
            }
        }
    }
    let name = tables.strings.get(name_id? as usize).cloned()?;
    let ret = builtin_type_ref(ret_body, ret_id, tables, &tparams)?;
    let flags = modern_flags
        .or(legacy_flags)
        .unwrap_or(property_flags::DEFAULT);
    Some(BuiltinMember {
        name,
        params: Vec::new(),
        ret_nullable: ret.nullable(),
        ret,
        is_property: true,
        is_operator: false,
        is_infix: false,
        is_abstract: flags & property_flags::MODALITY_MASK == property_flags::MODALITY_ABSTRACT,
        formals,
        // A property has no value parameters of its own; its accessors' are the backend's to shape.
        param_names: Vec::new(),
        param_defaults: Vec::new(),
        receiver: builtin_type_ref(recv_body, recv_id, tables, &tparams),
        is_var: flags & property_flags::IS_VAR != 0,
        is_const: flags & property_flags::IS_CONST != 0,
        visibility: builtin_declaration_visibility(flags),
        annotations: tables.annotation_identities(annotation_bodies.iter().copied()),
        setter_visibility: setter_flags.map(builtin_declaration_visibility),
    })
}

/// Parse one raw Kotlin metadata `PackageFragment`, as stored in a KLIB `.knm` entry. Builtins use
/// the same protobuf after a small version header; keeping the carrier split at this boundary lets
/// both providers share the declaration decoder without fabricating a builtins header.
pub fn parse_package_fragment(pf: &[u8]) -> BuiltinPackage {
    let mut out = BuiltinPackage::default();
    let mut strings: Vec<String> = Vec::new();
    let mut qnames: Vec<QName> = Vec::new();
    let mut package = None;
    let mut classes: Vec<&[u8]> = Vec::new();
    let mut pb = Pb { b: pf, i: 0 };
    while !pb.at_end() {
        let Some(tag) = pb.varint() else { break };
        match (tag >> 3, tag & 7) {
            (1, 2) => {
                let Some(n) = pb.varint() else { break };
                let Some(b) = pb.bytes(n as usize) else { break };
                let mut sp = Pb { b, i: 0 };
                while !sp.at_end() {
                    let Some(t) = sp.varint() else { break };
                    match (t >> 3, t & 7) {
                        (1, 2) => {
                            let Some(m) = sp.varint() else { break };
                            let Some(s) = sp.bytes(m as usize) else { break };
                            strings.push(String::from_utf8_lossy(s).into_owned());
                        }
                        (_, w) => {
                            if sp.skip(w).is_none() {
                                break;
                            }
                        }
                    }
                }
            }
            (2, 2) => {
                let Some(n) = pb.varint() else { break };
                let Some(b) = pb.bytes(n as usize) else { break };
                let mut qp = Pb { b, i: 0 };
                while !qp.at_end() {
                    let Some(t) = qp.varint() else { break };
                    match (t >> 3, t & 7) {
                        (1, 2) => {
                            let Some(m) = qp.varint() else { break };
                            let Some(qb) = qp.bytes(m as usize) else {
                                break;
                            };
                            qnames.push(parse_qname(qb));
                        }
                        (_, w) => {
                            if qp.skip(w).is_none() {
                                break;
                            }
                        }
                    }
                }
            }
            (3, 2) => {
                let Some(n) = pb.varint() else { break };
                package = pb.bytes(n as usize);
            }
            (4, 2) => {
                let Some(n) = pb.varint() else { break };
                let Some(b) = pb.bytes(n as usize) else { break };
                classes.push(b);
            }
            (_, w) => {
                if pb.skip(w).is_none() {
                    break;
                }
            }
        }
    }
    for cb in &classes {
        let mut cp = Pb { b: cb, i: 0 };
        let mut fq = None;
        let mut companion_name_id = None;
        let mut value_property_id = None;
        // `Class.flags` has the protobuf default PUBLIC FINAL (`6`). Keep wire-format defaulting at
        // the decode boundary, as the ordinary `@Metadata` class reader does, so every consumer sees
        // the semantic flag word. Treating omission as zero conflates it with an explicitly INTERNAL
        // declaration and forces downstream JVM-specific code to guess which input it received.
        let mut flags = 6u64;
        let mut supids: Vec<u64> = Vec::new();
        let mut sealed_ids: Vec<u64> = Vec::new();
        let mut enum_entry_bodies: Vec<&[u8]> = Vec::new();
        let mut types: Vec<&[u8]> = Vec::new();
        let mut ctors: Vec<&[u8]> = Vec::new();
        let mut funcs: Vec<&[u8]> = Vec::new();
        let mut props: Vec<&[u8]> = Vec::new();
        let mut class_tparam_bodies: Vec<&[u8]> = Vec::new();
        while !cp.at_end() {
            let Some(tag) = cp.varint() else { break };
            match (tag >> 3, tag & 7) {
                // Class.flags = 1 (varint). `CLASS_KIND` occupies bits 6..8 (after HAS_ANNOTATIONS,
                // VISIBILITY[3], MODALITY[2]); 1 = INTERFACE.
                (1, 0) => flags = cp.varint().unwrap_or(6),
                (3, 0) => fq = cp.varint(),
                (4, 0) => companion_name_id = cp.varint(),
                // `Class.inlineClassUnderlyingPropertyName` = 17, a string-table index.
                (17, 0) => value_property_id = cp.varint(),
                (2, 2) => {
                    // supertype_id (packed) — indexes the class's type_table.
                    if let Some(n) = cp.varint() {
                        if let Some(b) = cp.bytes(n as usize) {
                            supids.extend(packed_varints(b));
                        }
                    }
                }
                (f, 2) if f == CLASS_TYPE_PARAMETER_FIELD => {
                    // The names behind every `Type.type_parameter` id a member of this class references.
                    if let Some(n) = cp.varint() {
                        if let Some(b) = cp.bytes(n as usize) {
                            class_tparam_bodies.push(b);
                        }
                    }
                }
                // `Class.enumEntry` = 13 — one message per entry, its name in field 1. Read off a
                // klib the reference compiler wrote for `enum class Color { RED, GREEN }`.
                (13, 2) => {
                    if let Some(n) = cp.varint() {
                        if let Some(body) = cp.bytes(n as usize) {
                            enum_entry_bodies.push(body);
                        }
                    }
                }
                // `Class.sealedSubclassFqName` = 16 — PACKED qualified-name ids, the same encoding
                // `supertype_id` uses. `sealed class Shape` with two subclasses writes two bytes.
                (16, 2) => {
                    if let Some(n) = cp.varint() {
                        if let Some(b) = cp.bytes(n as usize) {
                            sealed_ids.extend(packed_varints(b));
                        }
                    }
                }
                (16, 0) => {
                    if let Some(id) = cp.varint() {
                        sealed_ids.push(id);
                    }
                }
                (8, 2) => {
                    // Class.constructor.
                    if let Some(n) = cp.varint() {
                        if let Some(body) = cp.bytes(n as usize) {
                            ctors.push(body);
                        }
                    }
                }
                (10, 2) => {
                    // Class.property = 10. Its shape is NOT a function's: see `builtin_property`.
                    if let Some(n) = cp.varint() {
                        if let Some(b) = cp.bytes(n as usize) {
                            props.push(b);
                        }
                    }
                }
                (9, 2) => {
                    if let Some(n) = cp.varint() {
                        if let Some(b) = cp.bytes(n as usize) {
                            funcs.push(b);
                        }
                    }
                }
                (30, 2) => {
                    let Some(n) = cp.varint() else { break };
                    let Some(b) = cp.bytes(n as usize) else { break };
                    let mut tp = Pb { b, i: 0 };
                    while !tp.at_end() {
                        let Some(t) = tp.varint() else { break };
                        match (t >> 3, t & 7) {
                            (1, 2) => {
                                let Some(m) = tp.varint() else { break };
                                let Some(ty) = tp.bytes(m as usize) else {
                                    break;
                                };
                                types.push(ty);
                            }
                            (_, w) => {
                                if tp.skip(w).is_none() {
                                    break;
                                }
                            }
                        }
                    }
                }
                (_, w) => {
                    if cp.skip(w).is_none() {
                        break;
                    }
                }
            }
        }
        let Some(fq) = fq else { continue };
        let fqname = resolve_qname(&qnames, &strings, fq as i64);
        let companion_name = companion_name_id
            .and_then(|id| strings.get(id as usize))
            .cloned();
        if companion_name.is_some() {
            crate::trace_compiler!(
                "metadata_companions",
                "builtin classifier {fqname} companion={companion_name:?}"
            );
        }
        if ((flags >> 6) & 0x7) == 6 {
            crate::trace_compiler!(
                "metadata_companions",
                "builtin companion classifier {fqname} flags={flags:#x}"
            );
        }
        let tables = BuiltinTables {
            strings: &strings,
            qnames: &qnames,
            types: &types,
        };
        // The class's own type parameters name every `Type.type_parameter` id its members reference.
        let mut class_tparams = TypeParamNames::new();
        let type_params = tables.type_params(&class_tparam_bodies, &mut class_tparams);
        // A `*_type_id` indexes the class `type_table`; decode the entry in full (class + arguments,
        // or a type-parameter reference) — a bare class name cannot express either.
        let type_of_id = |tid: u64, tps: &TypeParamNames| -> Option<BuiltinTy> {
            tables.ty_by_id(tid as usize, tps, 0)
        };
        let supertype_tys: Vec<BuiltinTy> = supids
            .iter()
            .filter_map(|&sid| type_of_id(sid, &class_tparams))
            .collect();
        let supertypes: Vec<String> = supertype_tys
            .iter()
            .filter_map(|t| t.internal().map(str::to_string))
            .collect();
        let sealed_subclasses: Vec<String> = sealed_ids
            .iter()
            .map(|&id| resolve_qname(&qnames, &strings, id as i64))
            .collect();
        let enum_entries: Vec<String> = enum_entry_bodies
            .iter()
            .filter_map(|body| enum_entry_name(body, &strings))
            .collect();
        let mut members = Vec::new();
        let constructors = ctors
            .iter()
            .filter_map(|constructor| {
                let mut message = Pb {
                    b: constructor,
                    i: 0,
                };
                // Constructor.flags protobuf default is PUBLIC (`6`).
                let mut flags = 6u64;
                let mut params = Vec::new();
                let mut param_names = Vec::new();
                let mut param_defaults = Vec::new();
                let mut vararg = None;
                while !message.at_end() {
                    let Some(tag) = message.varint() else {
                        break;
                    };
                    match (tag >> 3, tag & 7) {
                        (1, 0) => flags = message.varint().unwrap_or(6),
                        (2, 2) => {
                            let Some(len) = message.varint() else {
                                break;
                            };
                            let Some(value) = message.bytes(len as usize) else {
                                break;
                            };
                            let parsed = parse_value_parameter(value)?;
                            if parsed.vararg_elem_body.is_some() || parsed.vararg_elem_id.is_some()
                            {
                                vararg = Some(params.len());
                            }
                            param_names.push(
                                strings
                                    .get(parsed.name_id as usize)
                                    .cloned()
                                    .unwrap_or_else(|| format!("p{}", params.len())),
                            );
                            param_defaults.push(parsed.has_default);
                            params.push(builtin_value_parameter_type(
                                value,
                                &tables,
                                &class_tparams,
                            )?);
                        }
                        (_, wire) => {
                            if message.skip(wire).is_none() {
                                break;
                            }
                        }
                    }
                }
                Some(BuiltinConstructor {
                    params,
                    param_names,
                    param_defaults,
                    vararg,
                    visibility: crate::types::Visibility::from_metadata(flags_visibility(flags)),
                })
            })
            .collect::<Vec<_>>();
        let mut nullable_member_returns = Vec::new();
        for fb in &funcs {
            // A function may declare its OWN type parameters (`<R>` of `fold`); they shadow/extend the
            // class's, so decode this function's types against the union.
            let mut fn_tparams = class_tparams.clone();
            let formals = tables.type_params(
                &type_param_bodies(fb, MEMBER_TYPE_PARAMETER_FIELD),
                &mut fn_tparams,
            );
            let type_of_id = |tid: u64| type_of_id(tid, &fn_tparams);
            let mut p = Pb { b: fb, i: 0 };
            let mut name_id = None;
            let mut ret_id = None;
            let mut recv_body = None;
            let mut recv_id = None;
            let mut annotation_bodies: Vec<&[u8]> = Vec::new();
            // `Function.flags` has protobuf default PUBLIC FINAL (`6`), matching `parse_function`.
            let mut flags = 6u64;
            let mut params = Vec::new();
            let mut param_names = Vec::new();
            let mut param_defaults = Vec::new();
            let mut complete = true;
            while !p.at_end() {
                let Some(tag) = p.varint() else { break };
                match (tag >> 3, tag & 7) {
                    (9, 0) => flags = p.varint().unwrap_or(6),
                    (2, 0) => name_id = p.varint(), // name
                    (7, 0) => ret_id = p.varint(),  // return_type_id (type-table ref)
                    // `Function.receiver_type` = 5, `receiver_type_id` = 8 — read off a klib the
                    // reference compiler wrote for `class Holder { fun Int.memberExtFun() }`, whose
                    // member carries exactly the tags 2, 7, 8, 172.
                    (5, 2) => {
                        let Some(len) = p.varint() else { break };
                        recv_body = p.bytes(len as usize);
                    }
                    (8, 0) => recv_id = p.varint(),
                    (12, 2) | (KLIB_ANNOTATION_FIELD, 2) => {
                        if let Some(n) = p.varint() {
                            if let Some(body) = p.bytes(n as usize) {
                                annotation_bodies.push(body);
                            }
                        }
                    }
                    (6, 2) => {
                        // value_parameter: ValueParameter.type_id = 4 (type-table ref)
                        if let Some(n) = p.varint() {
                            if let Some(vb) = p.bytes(n as usize) {
                                match builtin_value_parameter(vb, &tables, &fn_tparams, &strings) {
                                    Some(parameter) => {
                                        params.push(parameter.ty);
                                        param_names.push(parameter.name);
                                        param_defaults.push(parameter.declares_default);
                                    }
                                    None => complete = false,
                                }
                            }
                        }
                    }
                    (_, w) => {
                        if p.skip(w).is_none() {
                            break;
                        }
                    }
                }
            }
            if complete {
                if let (Some(ni), Some(ri)) = (name_id, ret_id) {
                    // The return type's nullability (`Map.get(K): V?`) lives on the type-table entry's
                    // `Type.nullable` flag — the JVM descriptor erases it.
                    let ret_nullable = types
                        .get(ri as usize)
                        .is_some_and(|tb| parse_type_nullable(tb));
                    // Record nullable returns separately too: a call may still resolve to the ERASED
                    // classpath method (`java/util/Map.get` → `Object`), which carries no Kotlin
                    // nullability, and this is then the only surviving record that the source return is `T?`.
                    if let Some(name) = strings.get(ni as usize).filter(|_| ret_nullable) {
                        nullable_member_returns.push((name.clone(), params.len()));
                    }
                    if let Some((name, ret)) = strings.get(ni as usize).cloned().zip(type_of_id(ri))
                    {
                        members.push(BuiltinMember {
                            name,
                            params,
                            ret,
                            is_property: false,
                            is_operator: flags & IS_OPERATOR_BIT != 0,
                            is_infix: flags & IS_INFIX_BIT != 0,
                            is_abstract: (flags >> 4) & 0x3 == 2,
                            formals,
                            ret_nullable,
                            param_names,
                            param_defaults,
                            receiver: builtin_type_ref(recv_body, recv_id, &tables, &fn_tparams),
                            is_var: false,
                            is_const: false,
                            visibility: builtin_declaration_visibility(flags),
                            annotations: tables
                                .annotation_identities(annotation_bodies.iter().copied()),
                            setter_visibility: None,
                        });
                    }
                }
            }
        }
        for pb_ in &props {
            if let Some(property) = builtin_property(pb_, &tables, &class_tparams) {
                members.push(property);
            }
        }
        let is_nested = fqname
            .rsplit('/')
            .next()
            .is_some_and(|tail| tail.contains('.'));
        out.classes.insert(
            fqname,
            BuiltinClass {
                supertypes,
                supertype_tys,
                sealed_subclasses,
                enum_entries,
                value_underlying_property: (flags & IS_VALUE_CLASS_BIT != 0)
                    .then(|| value_property_id.and_then(|id| strings.get(id as usize).cloned()))
                    .flatten(),
                is_fun_interface: flags & IS_FUN_INTERFACE_BIT != 0,
                is_inner: flags & IS_INNER_CLASS_BIT != 0,
                members,
                constructors,
                companion_name,
                type_params,
                nullable_member_returns,
                kind: builtin_class_kind(flags),
                visibility: builtin_class_visibility(flags),
                is_expect: flags & (1 << 12) != 0,
                is_nested,
                flags,
            },
        );
    }
    if let Some(package) = package {
        let declarations = parse_builtin_package(package, &strings, &qnames);
        out.functions = declarations.functions;
        out.properties = declarations.properties;
        out.type_aliases = declarations.type_aliases;
    }
    out
}

pub fn builtin_class_kind(flags: u64) -> TypeKind {
    metadata_class_kind(flags)
}

pub fn builtin_class_visibility(flags: u64) -> Visibility {
    builtin_declaration_visibility(flags)
}

/// Source visibility from a metadata flag word, bits 1-3.
///
/// One decoder for every declaration kind: a `Class`, a `Function` and a `Property` all carry
/// visibility in the same three bits, which is why the property flag constants can name a shared
/// [`crate::metadata::property_flags::VISIBILITY_MASK`]. `PRIVATE_TO_THIS` reads as `Private` —
/// it is a stricter private, and nothing outside the declaration may reach either.
pub fn builtin_declaration_visibility(flags: u64) -> Visibility {
    match (flags >> 1) & 0x7 {
        1 | 4 => Visibility::Private,
        2 => Visibility::Protected,
        3 => Visibility::Public,
        _ => Visibility::Internal,
    }
}

// ---------------------------------------------------------------------------------------------
// From a decoded metadata NAME to a semantic `Ty`.
//
// The decode above answers what a library declares; this answers what those names MEAN as types.
// Both are carrier-independent: the classifier spellings are Kotlin's (`kotlin/Function1`,
// `kotlin/IntArray`, `kotlin/Unit`), and collapsing one to its dedicated `Ty` variant is a
// statement about Kotlin rather than about a target. What a backend then does with a `Ty` — erase
// it, box it, choose a carrier width — stays with that backend, which is why the JVM's
// `builtin_erased` is not here.
// ---------------------------------------------------------------------------------------------
/// A `@Metadata` class name + decoded type args → a signature [`Ty`]: a `kotlin/FunctionN` becomes a
/// [`Ty::Fun`] (args are `[P1..Pn, R]`), a Kotlin primitive collapses to its dedicated [`Ty`] variant (so
/// it matches a JVM-descriptor primitive downstream), everything else stays a [`Ty::Obj`].
///
/// `receiver_fun` and `context_count` come from the type's resolved Kotlin type annotations. A
/// receiver function type carries its receiver after its context parameters; [`Ty::Fun`] keeps all
/// implicit parameters at the front and records their semantic shape explicitly.
pub fn gsig_from_kotlin_class(
    internal: &str,
    mut args: Vec<Ty>,
    receiver_fun: bool,
    context_count: usize,
) -> Ty {
    let function_classifier = is_kotlin_function_classifier(internal);
    if function_classifier && !args.is_empty() {
        // The metadata arguments are the declaration shape: `[P1, …, R]`. The numeric classifier
        // suffix identifies the built-in family but is never parsed to recover or validate arity.
        let ret = args.pop().expect("checked non-empty function arguments");
        let has_receiver = receiver_fun && !args.is_empty();
        return Ty::fun_with_shape(args, ret, context_count, has_receiver, false);
    }
    // Arrays are `Obj` types. A boxed `Array<T>` carries its element as a type argument — built directly
    // so a primitive element stays the LOGICAL `Array<Int>` (`Obj("kotlin/Array", [Int])`), NOT the
    // primitive `IntArray` that `Ty::array(Int)` would mint. A primitive-array class (`IntArray`) carries
    // the (unboxed) element implicitly (its name minus `Array`) and IS `Ty::array`'s primitive form.
    if internal == "kotlin/Array" {
        return Ty::obj_args(
            "kotlin/Array",
            &[args.pop().unwrap_or_else(|| Ty::obj("kotlin/Any"))],
        );
    }
    if let Some(elem) = internal.strip_suffix("Array").and_then(kotlin_primitive) {
        return Ty::array(elem);
    }
    // A canonical scalar/reference type (`Int`, `String`, `Unit`, `Nothing`) has ONE dedicated `Ty`
    // variant; decode it to that here so a gsig-derived return is identical to the one a source annotation
    // produces — `Obj("kotlin/Unit")` would not drive the expression-body `areturn`'s `Unit.INSTANCE`
    // materialization the way `Ty::Unit` does.
    match kotlin_canonical_ty(internal) {
        Some(t) => t,
        None => Ty::obj_args(internal, &args),
    }
}

pub fn is_kotlin_function_classifier(internal: &str) -> bool {
    internal
        .strip_prefix("kotlin/Function")
        .is_some_and(|segment| {
            !segment.is_empty() && segment.bytes().all(|byte| byte.is_ascii_digit())
        })
}

/// Convert the metadata/JVM carrier for a suspend function type to its Kotlin source shape.
/// Metadata represents `suspend R.(P) -> T` as a function whose final parameter is
/// `Continuation<T>` and whose physical return is `Any?`, plus the suspend-type flag. The
/// continuation is not a source parameter: its argument is the source return type.
pub fn source_suspend_function_type(ty: Ty) -> Ty {
    let Ty::Fun(signature) = ty else {
        return ty;
    };
    let mut params = signature.params.clone();
    let ret = match params.last().copied().map(Ty::non_null) {
        Some(Ty::Obj(continuation, args))
            if continuation.matches("kotlin/coroutines/Continuation") =>
        {
            let ret = args
                .first()
                .copied()
                .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")));
            params.pop();
            ret
        }
        _ => signature.ret,
    };
    Ty::fun_with_shape(
        params,
        ret,
        signature.context_count,
        signature.has_receiver,
        true,
    )
}

/// The JVM primitive a Kotlin primitive class name denotes (`kotlin/Int` → `Int`), or `None`. Only the
/// eight primitives — used to recover a primitive-array's (unboxed) element type.
pub fn kotlin_primitive(internal: &str) -> Option<crate::types::Ty> {
    use crate::types::Ty;
    Some(match internal {
        "kotlin/Int" => Ty::Int,
        "kotlin/Long" => Ty::Long,
        "kotlin/Short" => Ty::Short,
        "kotlin/Byte" => Ty::Byte,
        "kotlin/Double" => Ty::Double,
        "kotlin/Float" => Ty::Float,
        "kotlin/Boolean" => Ty::Boolean,
        "kotlin/Char" => Ty::Char,
        _ => return None,
    })
}

/// The canonical `Ty` a Kotlin built-in class name denotes — the primitives PLUS the reference types that
/// carry a dedicated variant (`String`/`Unit`/`Nothing`). `None` for a class with no canonical variant.
pub fn kotlin_canonical_ty(internal: &str) -> Option<crate::types::Ty> {
    use crate::types::Ty;
    kotlin_primitive(internal).or_else(|| {
        Some(match internal {
            "kotlin/String" => Ty::String,
            "kotlin/Unit" => Ty::Unit,
            "kotlin/Nothing" => Ty::Nothing,
            _ => return None,
        })
    })
}

/// A decoded `.kotlin_builtins` type as a semantic [`Ty`]. `bounds` supplies each in-scope type
/// parameter's declared upper bound; an unlisted one is `Any?`, matching the `@Metadata`
/// generic-signature decoder. JVM erasure is derived separately by [`builtin_erased`].
pub fn builtin_ty(t: &BuiltinTy, bounds: &HashMap<String, Ty>) -> Ty {
    let ty = match t {
        BuiltinTy::Class { internal, args, .. } => {
            let args = args
                .iter()
                .map(|argument| builtin_ty(argument, bounds))
                .collect();
            gsig_from_kotlin_class(internal, args, false, 0)
        }
        BuiltinTy::Param { name, .. } => {
            let bound = bounds
                .get(name)
                .copied()
                .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")));
            Ty::ty_param(name, bound)
        }
        BuiltinTy::InProjection(inner) => Ty::in_projection(builtin_ty(inner, bounds)),
        BuiltinTy::OutProjection(inner) => Ty::out_projection(builtin_ty(inner, bounds)),
    };
    if t.nullable() {
        Ty::nullable(ty)
    } else {
        ty
    }
}

/// The declared upper bound of each type parameter, keyed by name. Bounds are decoded with an EMPTY
/// bound map so a recursive bound (`E : Comparable<E>`) terminates.
pub fn builtin_bounds(
    params: &[BuiltinTypeParam],
    inherited: &HashMap<String, Ty>,
) -> HashMap<String, Ty> {
    let mut out = inherited.clone();
    for p in params {
        let bound = p
            .bounds
            .first()
            .map(|b| builtin_ty(b, &HashMap::new()))
            .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")));
        out.insert(p.name.clone(), bound);
    }
    out
}
