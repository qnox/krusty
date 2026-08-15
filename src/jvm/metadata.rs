//! Minimal Kotlin `@Metadata` reader: decode the `d1` protobuf and report which functions are
//! `inline`, by their JVM `(name, descriptor)`. This is the complete inline-recognition the inliner
//! needs (the body `reifiedOperationMarker` scan only finds *reified* inline functions).
//!
//! Schema (kotlin `core/metadata/src/metadata.proto` + `metadata.jvm/.../jvm_metadata.proto`):
//!   Package.function = 3; Function.flags = 9 (IS_INLINE = bit 10); Function.name = 2;
//!   Function extension method_signature = 100 → JvmMethodSignature { name = 1, desc = 2 }.
//! String ids index the `d2` table.

use super::classfile::{
    ACC_ABSTRACT, ACC_ANNOTATION, ACC_ENUM, ACC_FINAL, ACC_INTERFACE, ACC_PRIVATE, ACC_PROTECTED,
    ACC_PUBLIC, ACC_STATIC,
};
use super::classreader::ClassInfo;
use super::names::method_descriptor;
use crate::libraries::{CallSig, GenericSig, ParamList, TypeKind};
use crate::types::{intern, type_name, Ty, TypeName, Visibility};
use std::collections::HashMap;

fn primary_erasure_bounds(formals: &[String], formal_bounds: &[Vec<Ty>]) -> HashMap<String, Ty> {
    fn resolve(
        name: &str,
        direct: &HashMap<String, Ty>,
        resolved: &mut HashMap<String, Ty>,
        visiting: &mut std::collections::HashSet<String>,
    ) -> Ty {
        if let Some(bound) = resolved.get(name) {
            return *bound;
        }
        if !visiting.insert(name.to_string()) {
            return Ty::obj("kotlin/Any");
        }
        let bound = match direct.get(name).copied() {
            Some(Ty::TyParam(other, _)) => resolve(other, direct, resolved, visiting),
            Some(bound) => bound,
            None => Ty::nullable(Ty::obj("kotlin/Any")),
        };
        visiting.remove(name);
        resolved.insert(name.to_string(), bound);
        bound
    }

    let direct = formals
        .iter()
        .zip(formal_bounds)
        .filter_map(|(name, bounds)| bounds.first().map(|bound| (name.clone(), *bound)))
        .collect::<HashMap<_, _>>();
    let mut resolved = HashMap::new();
    for name in formals {
        resolve(
            name,
            &direct,
            &mut resolved,
            &mut std::collections::HashSet::new(),
        );
    }
    resolved
}

/// The carrier-independent wire shape of Kotlin metadata's `Type` message. Both an annotation's
/// `@Metadata` payload and a `.kotlin_builtins` fragment use these same fields; only the way their
/// numeric class/string ids and type-table references are resolved differs. Keeping the protobuf walk
/// here prevents the two decoders from acquiring subtly different nullability, type-parameter,
/// annotation, or argument handling as either carrier evolves.
struct ParsedTypeNode<'a> {
    class_id: Option<u64>,
    type_parameter_id: Option<u64>,
    type_parameter_name_id: Option<u64>,
    type_alias_id: Option<u64>,
    nullable: bool,
    definitely_non_null: bool,
    flexible_upper_bound: Option<&'a [u8]>,
    flexible_upper_bound_id: Option<u64>,
    arguments: Vec<ParsedTypeArgument<'a>>,
    annotation_ids: Vec<u64>,
}

#[derive(Clone, Copy)]
enum ParsedProjection {
    In,
    Out,
    Invariant,
}

fn project_ty(projection: ParsedProjection, ty: Ty) -> Ty {
    match projection {
        ParsedProjection::In => Ty::in_projection(ty),
        ParsedProjection::Out => Ty::out_projection(ty),
        ParsedProjection::Invariant => ty,
    }
}

/// A type argument is either an inline `Type`, an id into the carrier's `TypeTable`, or a star
/// projection with no type. Projection is part of this carrier-independent wire shape: every semantic
/// decoder must see the same `in`/`out`/invariant distinction.
enum ParsedTypeArgument<'a> {
    Inline(&'a [u8], ParsedProjection),
    Table(u64, ParsedProjection),
    Star,
}

fn parse_type_node(body: &[u8]) -> Option<ParsedTypeNode<'_>> {
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
        annotation_ids: Vec::new(),
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
                // `Type.annotation` is an extension carrying an `Annotation` message whose field 1 is
                // the annotation class id. Preserve every occurrence; semantic interpretation (for
                // example `ExtensionFunctionType`) requires the caller's name resolver.
                let n = pb.varint()? as usize;
                let mut annotation_pb = Pb {
                    b: pb.bytes(n)?,
                    i: 0,
                };
                while !annotation_pb.at_end() {
                    let tag = annotation_pb.varint()?;
                    match (tag >> 3, tag & 7) {
                        (1, 0) => node.annotation_ids.push(annotation_pb.varint()?),
                        (_, wire) => annotation_pb.skip(wire)?,
                    }
                }
            }
            (_, wire) => pb.skip(wire)?,
        }
    }
    Some(node)
}

/// A `@Metadata` class name + decoded type args → a signature [`Ty`]: a `kotlin/FunctionN` becomes a
/// [`Ty::Fun`] (args are `[P1..Pn, R]`), a Kotlin primitive collapses to its dedicated [`Ty`] variant (so
/// it matches a JVM-descriptor primitive downstream), everything else stays a [`Ty::Obj`].
///
/// `receiver_fun` is the type's `@kotlin.ExtensionFunctionType` mark: a receiver function type carries
/// its receiver as the FIRST type argument, which [`Ty::Fun`] models as the first parameter binding
/// `this` (`has_receiver`).
pub(super) fn gsig_from_kotlin_class(internal: &str, mut args: Vec<Ty>, receiver_fun: bool) -> Ty {
    let function_classifier = internal
        .strip_prefix("kotlin/Function")
        .is_some_and(|segment| {
            !segment.is_empty() && segment.bytes().all(|byte| byte.is_ascii_digit())
        });
    if function_classifier && !args.is_empty() {
        // The metadata arguments are the declaration shape: `[P1, …, R]`. The numeric classifier
        // suffix identifies the built-in family but is never parsed to recover or validate arity.
        let ret = args.pop().expect("checked non-empty function arguments");
        let has_receiver = receiver_fun && !args.is_empty();
        return Ty::fun_with_shape(args, ret, 0, has_receiver, false);
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

/// Convert the metadata/JVM carrier for a suspend function type to its Kotlin source shape.
/// Metadata represents `suspend R.(P) -> T` as a function whose final parameter is
/// `Continuation<T>` and whose physical return is `Any?`, plus the suspend-type flag. The
/// continuation is not a source parameter: its argument is the source return type.
fn source_suspend_function_type(ty: Ty) -> Ty {
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
fn kotlin_primitive(internal: &str) -> Option<crate::types::Ty> {
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
fn kotlin_canonical_ty(internal: &str) -> Option<crate::types::Ty> {
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

struct ParsedTypeParam {
    id: u64,
    name_id: u64,
    reified: bool,
    upper_bound_bodies: Vec<Vec<u8>>,
    /// `TypeParameter.upper_bound_id` (field 6) — the type-table form a `.kotlin_builtins` fragment
    /// uses instead of the inline `upper_bound`. Empty for the `@Metadata` carrier, which inlines.
    upper_bound_ids: Vec<u64>,
    variance: crate::types::TypeVariance,
}

fn parse_type_param(body: &[u8]) -> Option<ParsedTypeParam> {
    let mut pb = Pb { b: body, i: 0 };
    let mut id = None;
    let mut name = None;
    let mut upper_bound_bodies = Vec::new();
    let mut upper_bound_ids = Vec::new();
    let mut reified = false;
    let mut variance = crate::types::TypeVariance::Invariant;
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
    })
}

/// Decode the `@Metadata` `d1` string array to raw protobuf bytes. Modern metadata (since Kotlin 1.4)
/// stores each byte as one already-UTF8-decoded char.
fn decode_d1(d1: &[String]) -> Vec<u8> {
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
struct Rec {
    predefined_index: Option<usize>,
    string: Option<String>,
    operation: u64, // 0 NONE, 1 INTERNAL_TO_CLASS_ID, 2 DESC_TO_CLASS_ID
    substring: Option<(usize, usize)>,
    replace: Option<(u32, u32)>,
}

/// Read a packed (length-delimited) repeated `int32` field into a Vec of varints.
fn packed_varints(body: &[u8]) -> Vec<u64> {
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
fn parse_record(body: &[u8]) -> Option<(u64, Rec)> {
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
fn parse_string_table(body: &[u8]) -> Vec<Rec> {
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
fn resolve_string(records: &[Rec], d2: &[String], id: usize) -> Option<String> {
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

fn resolve_class_name(records: &[Rec], d2: &[String], id: usize) -> Option<String> {
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
fn split_d1(bytes: &[u8]) -> (&[u8], &[u8]) {
    let mut pb = Pb { b: bytes, i: 0 };
    if let Some(len) = pb.varint() {
        let start = pb.i;
        if let Some(end) = start.checked_add(len as usize) {
            if end <= bytes.len() {
                return (&bytes[start..end], &bytes[end..]);
            }
        }
    }
    (&[], bytes)
}

/// A protobuf wire-format cursor over a message body.
struct Pb<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Pb<'a> {
    fn varint(&mut self) -> Option<u64> {
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
    fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.b.get(self.i..self.i.checked_add(n)?)?;
        self.i += n;
        Some(s)
    }
    fn at_end(&self) -> bool {
        self.i >= self.b.len()
    }
    /// Skip a field's value given its wire type; `false` on a malformed/unsupported wire type.
    fn skip(&mut self, wire: u64) -> Option<()> {
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
const IS_INLINE_BIT: u64 = 1 << 10;
/// `IS_OPERATOR` immediately follows the 2-bit member-kind field in Kotlin metadata's function flags.
const IS_OPERATOR_BIT: u64 = 1 << 8;
/// `IS_INFIX` follows `IS_OPERATOR` in Kotlin metadata's function flags.
const IS_INFIX_BIT: u64 = 1 << 9;

/// A `JvmMethodSignature`. Both fields are independently optional in the protobuf: an omitted name
/// means the Kotlin declaration name, while an omitted descriptor is derived from the Kotlin types.
#[derive(Clone, Copy, Debug)]
struct ParsedJvmSignature {
    name_id: Option<u64>,
    desc_id: Option<u64>,
}

fn parse_jvm_signature(body: &[u8]) -> Option<ParsedJvmSignature> {
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
fn parse_type_class_name(body: &[u8]) -> Option<u64> {
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
const IS_SUSPEND_BIT: u64 = 1 << 13;

/// `ValueParameter.flags` bit for `DECLARES_DEFAULT_VALUE` (bit 1; `HAS_ANNOTATIONS` is bit 0).
const DECLARES_DEFAULT_VALUE_BIT: u64 = 1 << 1;
/// `ValueParameter.flags` bits for `IS_CROSSINLINE` (bit 2) and `IS_NOINLINE` (bit 3) of an inline
/// function's functional parameter. Either one means the lambda argument is MATERIALIZED into a real
/// `FunctionN` object / nested class (not spliced into the caller frame), so a mutable local it captures
/// must be boxed in a `Ref` holder — the same as an ordinary closure.
const IS_CROSSINLINE_BIT: u64 = 1 << 2;
const IS_NOINLINE_BIT: u64 = 1 << 3;

/// `Visibility` enum value from a Function/Class `flags` word: `hasAnnotations` is bit 0, then
/// `Visibility` occupies the next 3 bits (kotlin metadata `Flags.VISIBILITY`). Enum order:
/// INTERNAL=0, PRIVATE=1, PROTECTED=2, PUBLIC=3, PRIVATE_TO_THIS=4, LOCAL=5.
fn flags_visibility(flags: u64) -> u64 {
    (flags >> 1) & 0x7
}
const VIS_PUBLIC: u64 = 3;

/// One source `ValueParameter` decoded from metadata. Keeping these facts together avoids the parser's
/// old parallel vectors drifting as more parameter-level facts are added.
struct ParsedValueParam {
    name_id: u64,
    has_default: bool,
    materialized: bool,
    /// The raw inline `ValueParameter.type` (field 3) message, when present. Keeping PRESENCE rather
    /// than an empty sentinel distinguishes an explicitly empty/default `Type` from a parameter that
    /// instead names the enclosing type table through [`Self::type_id`].
    type_body: Option<Vec<u8>>,
    /// `ValueParameter.type_id` (field 5), indexing the function/container `TypeTable` when the
    /// producer chose table-backed types.
    type_id: Option<u64>,
    /// The raw `ValueParameter.varargElementType` (field 4 as emitted by kotlin-stdlib 2.3.20) `Type`
    /// body when the parameter is a `vararg`.
    /// Present ⇒ the parameter is a vararg whose LOGICAL gsig is `Array<elem>`; kotlinc stores the element
    /// type here (the JVM descriptor's array-ness lives only in `type`/the descriptor).
    vararg_elem_body: Option<Vec<u8>>,
    /// `ValueParameter.vararg_element_type_id` (field 6), the table-backed form of
    /// [`Self::vararg_elem_body`].
    vararg_elem_id: Option<u64>,
}

/// A decoded `Function` message: whether it's `inline`, whether it's `suspend`, its name string id, its
/// explicit JVM `(name id, desc id)` signature (if present), its operator flag, and its return type's
/// class_name id.
struct ParsedFunction {
    is_inline: bool,
    is_suspend: bool,
    is_operator: bool,
    is_infix: bool,
    visibility: crate::types::Visibility,
    name_id: u64,
    jvm_sig: Option<ParsedJvmSignature>,
    /// Whether `receiver_type` (field 5) was present — TRUE for an extension on a type PARAMETER
    /// (`fun <T> T.takeIf`), where `recv_class` is None. Distinguishes an extension from a top-level fn.
    has_receiver: bool,
    /// Whether the Kotlin return type is nullable (`T?`) — `Type.nullable = 3`. The JVM
    /// descriptor/`Signature` erase this; only `@Metadata` carries it. Drives the elvis null-check for a
    /// nullable-returning scope fn (`takeIf`/`takeUnless` return `T?`).
    /// SOURCE value parameters in declaration order. The COUNT is the source arity (excludes synthetic
    /// descriptor params); fields are resolved to names downstream.
    value_params: Vec<ParsedValueParam>,
    /// The function's own `type_parameter` table (field 4): `(id, name string-id)` — for resolving a
    /// `Type.type_parameter` reference in a parameter/return type to its name.
    type_params: Vec<ParsedTypeParam>,
    /// Raw `Function.return_type` (field 3) `Type` body, for the metadata generic signature.
    return_body: Option<Vec<u8>>,
    /// `Function.return_type_id` (field 7), used when the return lives in the effective type table.
    return_type_id: Option<u64>,
    /// Raw `Function.receiver_type` (field 5) `Type` body (extensions only), for the metadata gsig.
    receiver_body: Option<Vec<u8>>,
    /// `Function.receiver_type_id` (field 8), whose presence also marks an extension.
    receiver_type_id: Option<u64>,
    /// Raw `Annotation` message bodies on the function (`Function.annotation`, field 12) — decoded to
    /// `(class name, arguments)` downstream where the string table is available. Kotlin stores an
    /// annotation here when it has `BINARY`/`RUNTIME` retention (`@JvmName`, `@OverloadResolutionBy…`).
    annotation_bodies: Vec<Vec<u8>>,
    /// Raw `Contract` message body (`Function.contract`, field 32) — the function's declared
    /// `contract { … }` effects, decoded downstream where the string table is available.
    contract_body: Option<Vec<u8>>,
    /// Raw `TypeTable` message body (`Function.type_table`, field 30) — contract expressions may
    /// reference their `is_instance_type` by id into this table instead of inlining the `Type`.
    type_table_body: Option<Vec<u8>>,
    /// Old unnamed context receivers (`context_receiver_type` fields 10/11).
    context_receiver_bodies: Vec<Vec<u8>>,
    context_receiver_type_ids: Vec<u64>,
    /// Named context parameters (`context_parameter`, field 13).
    context_params: Vec<ParsedValueParam>,
}

fn parse_value_parameter(body: &[u8]) -> Option<ParsedValueParam> {
    let mut pb = Pb { b: body, i: 0 };
    let mut name_id = None;
    let mut flags = 0u64;
    let mut type_body = None;
    let mut type_id = None;
    let mut vararg_elem_body = None;
    let mut vararg_elem_id = None;
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
    })
}

/// Parse one `Function` message. The return type is `Function.return_type = 3` and the extension
/// receiver `Function.receiver_type = 5` (both inline `Type`s in package metadata).
fn parse_function(body: &[u8]) -> Option<ParsedFunction> {
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
            (12, 2) => {
                // Function.annotation (repeated `Annotation`) — decoded downstream (needs the string table).
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

// ---------------------------------------------------------------------------
// Contract decoding (`Function.contract`, field 32)
// ---------------------------------------------------------------------------
//
// Proto (`core/metadata/src/metadata.proto`): `Contract.effect` = 1 (repeated `Effect`).
// `Effect.effect_type` = 1: RETURNS_CONSTANT = 0 / CALLS = 1 / RETURNS_NOT_NULL = 2 /
// RETURNS_RESULT_OF = 3 — there is NO conditional type: `conclusion_of_conditional_effect` = 3
// being present (with `condition_kind` = 5 absent/CONCLUSION_CONDITION = 0) makes the whole
// message `<returns-effect> implies <conclusion>`. `Effect.kind` = 4: `InvocationKind`
// AT_MOST_ONCE = 0 / EXACTLY_ONCE = 1 / AT_LEAST_ONCE = 2 (the enum order is NOT the Kotlin
// declaration order — verified against kotlin-stdlib's `run`, which is EXACTLY_ONCE = 1).
// `Expression.flags` = 1 (bit 0 = negated, bit 1 = null-check predicate),
// `value_parameter_reference` = 2 (0 = extension receiver, else the 1-based value-parameter
// index), `constant_value` = 3 (TRUE = 0 / FALSE = 1 / NULL = 2), `is_instance_type` = 4
// (inline `Type`), `and_argument` = 6 / `or_argument` = 7 (repeated `Expression`; the FIRST
// operand of the formula is embedded inline in the parent when it is primitive).

/// Decode a `Contract` message body into the shared contract IR. `tparams` maps the function's
/// type-parameter ids to names (for an `is R` conclusion over a generic parameter).
fn decode_contract(
    body: &[u8],
    records: &[Rec],
    d2: &[String],
    tparams: &HashMap<u64, String>,
    type_table: Option<&[u8]>,
) -> Option<crate::contracts::Contract> {
    let mut pb = Pb { b: body, i: 0 };
    let mut effects = Vec::new();
    while !pb.at_end() {
        let tag = pb.varint()?;
        match (tag >> 3, tag & 7) {
            (1, 2) => {
                let n = pb.varint()? as usize;
                effects.push(decode_effect(
                    pb.bytes(n)?,
                    records,
                    d2,
                    tparams,
                    type_table,
                )?);
            }
            (_, w) => pb.skip(w)?,
        }
    }
    (!effects.is_empty()).then_some(crate::contracts::Contract { effects })
}

fn decode_effect(
    body: &[u8],
    records: &[Rec],
    d2: &[String],
    tparams: &HashMap<u64, String>,
    type_table: Option<&[u8]>,
) -> Option<crate::contracts::Effect> {
    use crate::contracts::{Effect, InvocationKind};
    let mut pb = Pb { b: body, i: 0 };
    let mut effect_type = 0u64;
    let mut args: Vec<Vec<u8>> = Vec::new();
    let mut conclusion: Option<Vec<u8>> = None;
    let mut kind = 0u64;
    let mut condition_kind = 0u64;
    while !pb.at_end() {
        let tag = pb.varint()?;
        match (tag >> 3, tag & 7) {
            (1, 0) => effect_type = pb.varint()?,
            (2, 2) => {
                let n = pb.varint()? as usize;
                args.push(pb.bytes(n)?.to_vec());
            }
            (3, 2) => {
                let n = pb.varint()? as usize;
                conclusion = Some(pb.bytes(n)?.to_vec());
            }
            (4, 0) => kind = pb.varint()?,
            (5, 0) => condition_kind = pb.varint()?,
            (_, w) => pb.skip(w)?,
        }
    }
    let base = match effect_type {
        // RETURNS_CONSTANT — returns()/returns(c) (the constant is the argument, if any).
        0 => Effect::Returns(returns_constant(args.first())),
        // CALLS — callsInPlace(param, kind).
        1 => Effect::CallsInPlace {
            param: crate::contracts::ParamRef::from_wire(expression_param_ref(args.first()?)?),
            kind: InvocationKind::from_wire(kind),
        },
        // RETURNS_NOT_NULL — returnsNotNull().
        2 => Effect::Returns(crate::contracts::ReturnsValue::NotNull),
        // RETURNS_RESULT_OF — not modeled.
        _ => return None,
    };
    // `conclusion_of_conditional_effect` with the default CONCLUSION_CONDITION kind turns the
    // returns-effect into `<returns> implies <conclusion>`. (RETURNS_CONDITION / HOLDSIN forms
    // are not modeled.)
    if condition_kind == 0 {
        if let Some(cb) = conclusion {
            if let Effect::Returns(returns) = base {
                return Some(Effect::ConditionalReturns {
                    returns,
                    conclusion: decode_expression(&cb, records, d2, tparams, type_table)?,
                });
            }
        }
    }
    Some(base)
}

/// The `returns(…)` constant of a RETURNS_CONSTANT effect: an `Expression` whose `constant_value`
/// is the returned literal; no argument at all spells `returns()`.
fn returns_constant(arg: Option<&Vec<u8>>) -> crate::contracts::ReturnsValue {
    use crate::contracts::ReturnsValue;
    let Some(body) = arg else {
        return ReturnsValue::Any;
    };
    let mut pb = Pb { b: body, i: 0 };
    let mut constant = None;
    while !pb.at_end() {
        let Some(tag) = pb.varint() else { break };
        match (tag >> 3, tag & 7) {
            (3, 0) => constant = pb.varint(),
            (_, w) => {
                if pb.skip(w).is_none() {
                    break;
                }
            }
        }
    }
    match constant {
        Some(0) => ReturnsValue::Bool(true),
        Some(1) => ReturnsValue::Bool(false),
        Some(2) => ReturnsValue::Null,
        _ => ReturnsValue::Any,
    }
}

/// The `value_parameter_reference` of an `Expression` body, when present.
fn expression_param_ref(body: &[u8]) -> Option<u64> {
    let mut pb = Pb { b: body, i: 0 };
    while !pb.at_end() {
        let tag = pb.varint()?;
        match (tag >> 3, tag & 7) {
            (2, 0) => return pb.varint(),
            (_, w) => pb.skip(w)?,
        }
    }
    None
}

/// Decode an `Expression` in conclusion position into a [`crate::contracts::Condition`]. A
/// boolean formula embeds its FIRST operand inline in this message when it is primitive, with
/// the rest in `and_argument` (field 6) / `or_argument` (field 7).
fn decode_expression(
    body: &[u8],
    records: &[Rec],
    d2: &[String],
    tparams: &HashMap<u64, String>,
    type_table: Option<&[u8]>,
) -> Option<crate::contracts::Condition> {
    use crate::contracts::{Condition, ConditionType};
    let mut pb = Pb { b: body, i: 0 };
    let mut flags = 0u64;
    let mut vpr = None;
    let mut constant = None;
    let mut instance_body = None;
    let mut instance_id = None;
    let mut ands: Vec<Condition> = Vec::new();
    let mut ors: Vec<Condition> = Vec::new();
    while !pb.at_end() {
        let tag = pb.varint()?;
        match (tag >> 3, tag & 7) {
            (1, 0) => flags = pb.varint()?,
            (2, 0) => vpr = Some(pb.varint()?),
            (3, 0) => constant = Some(pb.varint()?),
            (4, 2) => {
                let n = pb.varint()? as usize;
                instance_body = Some(pb.bytes(n)?.to_vec());
            }
            (5, 0) => instance_id = Some(pb.varint()?),
            (6, 2) => {
                let n = pb.varint()? as usize;
                ands.push(decode_expression(
                    pb.bytes(n)?,
                    records,
                    d2,
                    tparams,
                    type_table,
                )?);
            }
            (7, 2) => {
                let n = pb.varint()? as usize;
                ors.push(decode_expression(
                    pb.bytes(n)?,
                    records,
                    d2,
                    tparams,
                    type_table,
                )?);
            }
            (_, w) => pb.skip(w)?,
        }
    }
    // The primitive condition embedded inline in this message, if any.
    let negated = flags & 1 != 0;
    let null_check = flags & 2 != 0;
    let self_cond = if null_check {
        Some(Condition::IsNull {
            param: crate::contracts::ParamRef::from_wire(vpr?),
            negated,
        })
    } else if instance_body.is_some() || instance_id.is_some() {
        // `is_instance_type` may INLINE the `Type` (field 4) or reference the function's
        // `TypeTable` by id (field 5) — kotlinc writes the table form.
        let (tb, table_nullable) = match (instance_body, instance_id) {
            (Some(tb), _) => (tb, false),
            (None, Some(id)) => {
                let (tb, nullable) = type_table_entry(type_table?, id as usize)?;
                (tb.to_vec(), nullable)
            }
            _ => return None,
        };
        let ty = decode_metadata_type(
            &tb,
            type_table,
            records,
            d2,
            tparams,
            &HashMap::new(),
            table_nullable,
            0,
        )?;
        Some(Condition::IsType {
            param: crate::contracts::ParamRef::from_wire(vpr?),
            ty: ConditionType::Metadata(ty),
            negated,
        })
    } else {
        match constant {
            Some(0) => Some(Condition::Const(true)),
            Some(1) => Some(Condition::Const(false)),
            _ => vpr.map(|v| Condition::BoolParam(crate::contracts::ParamRef::from_wire(v))),
        }
    };
    fn fold(
        cs: Vec<Condition>,
        mk: fn(Box<Condition>, Box<Condition>) -> Condition,
    ) -> Option<Condition> {
        let mut it = cs.into_iter();
        let first = it.next()?;
        Some(it.fold(first, |acc, c| mk(Box::new(acc), Box::new(c))))
    }
    if !ands.is_empty() {
        return fold(self_cond.into_iter().chain(ands).collect(), Condition::And);
    }
    if !ors.is_empty() {
        return fold(self_cond.into_iter().chain(ors).collect(), Condition::Or);
    }
    self_cond
}

/// The `@kotlin.jvm.JvmName("...")` value from a function's decoded annotation bodies, if present. The
/// `@JvmName` annotation is `Annotation { id = <kotlin/jvm/JvmName class id>, argument = [{ value =
/// Value { stringValue = <string id> } }] }`. Returns the resolved string. Any other annotation (or a
/// field-12 body that isn't an `Annotation`) yields `None`, so the caller safely keeps the Kotlin name.
fn annotation_jvm_name(bodies: &[Vec<u8>], records: &[Rec], d2: &[String]) -> Option<String> {
    for body in bodies {
        let mut pb = Pb { b: body, i: 0 };
        let mut id: Option<u64> = None;
        let mut string_arg: Option<u64> = None;
        while !pb.at_end() {
            let tag = pb.varint()?;
            match (tag >> 3, tag & 7) {
                (1, 0) => id = pb.varint(), // Annotation.id (class id)
                (2, 2) => {
                    // Annotation.argument → Argument { value = 2: Value { stringValue = 5 } }.
                    let n = pb.varint()? as usize;
                    let arg = pb.bytes(n)?;
                    let mut ap = Pb { b: arg, i: 0 };
                    while !ap.at_end() {
                        let at = ap.varint()?;
                        match (at >> 3, at & 7) {
                            (2, 2) => {
                                let vn = ap.varint()? as usize;
                                let vb = ap.bytes(vn)?;
                                let mut vp = Pb { b: vb, i: 0 };
                                while !vp.at_end() {
                                    let vt = vp.varint()?;
                                    match (vt >> 3, vt & 7) {
                                        (5, 0) => string_arg = vp.varint(), // Value.stringValue
                                        (_, w) => vp.skip(w)?,
                                    }
                                }
                            }
                            (_, w) => ap.skip(w)?,
                        }
                    }
                }
                (_, w) => pb.skip(w)?,
            }
        }
        let is_jvm_name = id
            .and_then(|i| resolve_class_name(records, d2, i as usize))
            .as_deref()
            == Some("kotlin/jvm/JvmName");
        if is_jvm_name {
            if let Some(s) = string_arg.and_then(|s| resolve_string(records, d2, s as usize)) {
                return Some(s);
            }
        }
    }
    None
}

fn has_annotation(bodies: &[Vec<u8>], records: &[Rec], d2: &[String], expected: &str) -> bool {
    bodies.iter().any(|body| {
        let mut pb = Pb { b: body, i: 0 };
        let mut id = None;
        while !pb.at_end() {
            let Some(tag) = pb.varint() else {
                return false;
            };
            match (tag >> 3, tag & 7) {
                (1, 0) => id = pb.varint(),
                (_, wire) if pb.skip(wire).is_none() => return false,
                _ => {}
            }
        }
        id.and_then(|id| resolve_class_name(records, d2, id as usize))
            .map(|name| type_name(&name))
            .is_some_and(|name| name.matches(expected))
    })
}

/// The declaration facts carried directly by one Kotlin metadata `Type` message.
///
/// Nullability and suspend-function identity live in the same protobuf node. Decode them in one walk
/// so a value-parameter consumer cannot accidentally read one from an inline type and the other from
/// a type-table entry, or duplicate the wire parser as new type flags are added.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ParsedTypeFacts {
    nullable: bool,
    suspend_fun: bool,
}

fn parse_type_facts(body: &[u8]) -> ParsedTypeFacts {
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
fn parse_type_nullable(body: &[u8]) -> bool {
    parse_type_facts(body).nullable
}

/// Resolve a value parameter's declared `Type` through the one representation chosen by its
/// producer: an inline message or an index into the enclosing table. The boolean is the table's
/// `firstNullable` contribution, which is stored outside the `Type` entry itself.
///
/// All parameter consumers use this operation—generic-signature decoding, class/annotation recovery,
/// and per-type flags—so table-backed metadata cannot silently degrade only one of those views.
fn value_parameter_type<'a>(
    parameter: &'a ParsedValueParam,
    type_table: Option<&'a [u8]>,
) -> Option<(&'a [u8], bool)> {
    metadata_type_ref(
        parameter.type_body.as_deref(),
        parameter.type_id,
        type_table,
    )
}

fn vararg_element_type<'a>(
    parameter: &'a ParsedValueParam,
    type_table: Option<&'a [u8]>,
) -> Option<(&'a [u8], bool)> {
    metadata_type_ref(
        parameter.vararg_elem_body.as_deref(),
        parameter.vararg_elem_id,
        type_table,
    )
}

/// Project receiver-function facts from the fully decoded Kotlin type. This keeps annotation,
/// type-table, and nested generic decoding in [`decode_metadata_type`] instead of maintaining a
/// second protobuf walk that can lose a table-backed or parameterized receiver.
fn receiver_function_shape(ty: Option<Ty>) -> (bool, Option<TypeName>) {
    let Some(Ty::Fun(signature)) = ty.map(Ty::non_null) else {
        return (false, None);
    };
    if !signature.has_receiver {
        return (false, None);
    }
    let receiver =
        signature
            .params
            .get(signature.context_count)
            .and_then(|receiver| match receiver.non_null() {
                Ty::Obj(name, _) => Some(name),
                _ => None,
            });
    (true, receiver)
}

fn metadata_type_ref<'a>(
    body: Option<&'a [u8]>,
    id: Option<u64>,
    type_table: Option<&'a [u8]>,
) -> Option<(&'a [u8], bool)> {
    if let Some(body) = body {
        return Some((body, false));
    }
    let id = id?;
    type_table_entry(type_table?, id as usize)
}

struct TypeParameterContext {
    names: HashMap<u64, String>,
    formals: Vec<String>,
    formal_bounds: Vec<Vec<Ty>>,
    erasure_bounds: HashMap<String, Ty>,
}

fn decode_metadata_type_ref(
    resolved: Option<(&[u8], bool)>,
    type_table: Option<&[u8]>,
    records: &[Rec],
    d2: &[String],
    context: Option<&TypeParameterContext>,
) -> Option<Ty> {
    let (body, table_nullable) = resolved?;
    let context = context?;
    decode_metadata_type(
        body,
        type_table,
        records,
        d2,
        &context.names,
        &context.erasure_bounds,
        table_nullable,
        0,
    )
}

fn decode_value_parameter_types(
    parameter: &ParsedValueParam,
    type_table: Option<&[u8]>,
    records: &[Rec],
    d2: &[String],
    context: Option<&TypeParameterContext>,
) -> (Option<Ty>, Option<Ty>) {
    let declared = decode_metadata_type_ref(
        value_parameter_type(parameter, type_table),
        type_table,
        records,
        d2,
        context,
    );
    let receiver_shape =
        if parameter.vararg_elem_body.is_some() || parameter.vararg_elem_id.is_some() {
            decode_metadata_type_ref(
                vararg_element_type(parameter, type_table),
                type_table,
                records,
                d2,
                context,
            )
        } else {
            declared
        };
    (declared, receiver_shape)
}

fn type_parameter_context(
    inherited: &[(u64, String)],
    inherited_bounds: &[Vec<Ty>],
    declared: &[ParsedTypeParam],
    records: &[Rec],
    d2: &[String],
    type_table: Option<&[u8]>,
) -> Option<TypeParameterContext> {
    let mut names = inherited.iter().cloned().collect::<HashMap<_, _>>();
    let mut formals = inherited
        .iter()
        .map(|(_, name)| name.clone())
        .collect::<Vec<_>>();
    let mut resolved = Vec::new();
    for parameter in declared {
        let name = resolve_string(records, d2, parameter.name_id as usize)?;
        names.insert(parameter.id, name.clone());
        formals.push(name.clone());
        resolved.push(parameter);
    }
    let mut formal_bounds = inherited_bounds.to_vec();
    formal_bounds.resize(inherited.len(), Vec::new());
    formal_bounds.extend(resolved.into_iter().map(|parameter| {
        let inline = parameter.upper_bound_bodies.iter().filter_map(|body| {
            decode_metadata_type(
                body,
                type_table,
                records,
                d2,
                &names,
                &HashMap::new(),
                false,
                0,
            )
        });
        let indexed = parameter.upper_bound_ids.iter().filter_map(|id| {
            let (body, nullable) = type_table_entry(type_table?, *id as usize)?;
            decode_metadata_type(
                body,
                type_table,
                records,
                d2,
                &names,
                &HashMap::new(),
                nullable,
                0,
            )
        });
        inline.chain(indexed).collect()
    }));
    let erasure_bounds = primary_erasure_bounds(&formals, &formal_bounds);
    Some(TypeParameterContext {
        names,
        formals,
        formal_bounds,
        erasure_bounds,
    })
}

/// Build the metadata-primary [`GenericSig`] for a function: `formals` = the function's own declared
/// type-parameter names; `receiver` = the EXTENSION's `receiver_type`, or — for a member — the
/// declaring class parameterized by its own type parameters (`Box<T>`), or `None` for a top-level
/// function; `params` = the source parameters (leading context parameters, then ordinary value
/// parameters; no receiver and no synthetic `suspend` Continuation);
/// `ret` = the return type. Receiver is an ATTRIBUTE, uniform for member and extension: at the
/// checker/resolver level `class A { fun foo(): B }` and `A.foo(): B` are the same function on a receiver
/// `A`; that an extension emits the receiver as a leading JVM arg is only an emit detail. `None` only when
/// a receiver that WAS present fails to decode. `class_receiver` is `Some((declaring_class, class_tparams))`
/// for a member, `None` for an extension/top-level function.
fn build_generic_sig(
    pf: &ParsedFunction,
    records: &[Rec],
    d2: &[String],
    class_tparams: &[(u64, String)],
    class_tparam_bounds: &[Vec<Ty>],
    class_receiver: Option<(&str, &[(u64, String)])>,
    type_table: Option<&[u8]>,
) -> Option<GenericSig> {
    let context = type_parameter_context(
        class_tparams,
        class_tparam_bounds,
        &pf.type_params,
        records,
        d2,
        type_table,
    )?;
    let receiver_ref =
        metadata_type_ref(pf.receiver_body.as_deref(), pf.receiver_type_id, type_table);
    let receiver = if let Some((body, table_nullable)) = receiver_ref {
        // An EXTENSION: its `receiver_type` is the receiver gsig node (`T`, `Ch`, `List<T>`, …).
        Some(decode_metadata_type(
            body,
            type_table,
            records,
            d2,
            &context.names,
            &context.erasure_bounds,
            table_nullable,
            0,
        )?)
    } else {
        // A MEMBER: the declaring class parameterized by its own type parameters, so unifying it with the
        // actual receiver binds `T` exactly like an extension. `None` for a top-level function.
        class_receiver.map(|(internal, ctps)| {
            Ty::obj_args(
                internal,
                &ctps
                    .iter()
                    .map(|(_, n)| Ty::ty_param(n, Ty::nullable(Ty::obj("kotlin/Any"))))
                    .collect::<Vec<_>>(),
            )
        })
    };
    let decode_parameter = |vp: &ParsedValueParam| {
        if let Some((elem, table_nullable)) = vararg_element_type(vp, type_table) {
            decode_metadata_type(
                elem,
                type_table,
                records,
                d2,
                &context.names,
                &context.erasure_bounds,
                table_nullable,
                0,
            )
            .map(Ty::array)
        } else {
            value_parameter_type(vp, type_table).and_then(|(body, table_nullable)| {
                decode_metadata_type(
                    body,
                    type_table,
                    records,
                    d2,
                    &context.names,
                    &context.erasure_bounds,
                    table_nullable,
                    0,
                )
            })
        }
    };
    let context_params = if !pf.context_params.is_empty() {
        pf.context_params
            .iter()
            .map(decode_parameter)
            .collect::<Option<Vec<_>>>()?
    } else {
        pf.context_receiver_bodies
            .iter()
            .map(|body| (Some(body.as_slice()), None))
            .chain(
                pf.context_receiver_type_ids
                    .iter()
                    .map(|&id| (None, Some(id))),
            )
            .map(|(body, id)| {
                let (body, table_nullable) = metadata_type_ref(body, id, type_table)?;
                decode_metadata_type(
                    body,
                    type_table,
                    records,
                    d2,
                    &context.names,
                    &context.erasure_bounds,
                    table_nullable,
                    0,
                )
            })
            .collect::<Option<Vec<_>>>()?
    };
    let mut params = context_params;
    params.extend(
        pf.value_params
            .iter()
            .map(decode_parameter)
            .collect::<Option<Vec<_>>>()?,
    );
    let ret = metadata_type_ref(pf.return_body.as_deref(), pf.return_type_id, type_table)
        .and_then(|(body, table_nullable)| {
            decode_metadata_type(
                body,
                type_table,
                records,
                d2,
                &context.names,
                &context.erasure_bounds,
                table_nullable,
                0,
            )
        })?;
    // Enclosing class parameters belong in the DECODING context because a member extension may use
    // one in its receiver/value/return shape. They do not become function formals: consumers render
    // `formals` after `fun`, accept explicit method type arguments from it, and bind owner parameters
    // from the applied dispatch type. Keeping the inherited prefix here would turn every method of
    // `Class<E>` into a fictitious `fun <E>` and allow call inference to overwrite the owner's `E`.
    let inherited = class_tparams.len();
    Some(GenericSig {
        formals: context.formals[inherited..].to_vec(),
        formal_bounds: context.formal_bounds[inherited..].to_vec(),
        receiver,
        params,
        ret,
        return_policy: Default::default(),
    })
}

fn build_property_generic_sig(
    inherited: &[(u64, String)],
    inherited_bounds: &[Vec<Ty>],
    type_params: &[ParsedTypeParam],
    return_body: Option<&[u8]>,
    return_nullable: bool,
    receiver_body: Option<&[u8]>,
    receiver_nullable: bool,
    records: &[Rec],
    d2: &[String],
    type_table: Option<&[u8]>,
) -> Option<GenericSig> {
    let context = type_parameter_context(
        inherited,
        inherited_bounds,
        type_params,
        records,
        d2,
        type_table,
    )?;
    let receiver = match receiver_body {
        Some(body) => {
            let receiver = decode_metadata_type(
                body,
                type_table,
                records,
                d2,
                &context.names,
                &context.erasure_bounds,
                receiver_nullable,
                0,
            )?;
            Some(receiver)
        }
        None => None,
    };
    let ret = decode_metadata_type(
        return_body?,
        type_table,
        records,
        d2,
        &context.names,
        &context.erasure_bounds,
        return_nullable,
        0,
    )?;
    Some(GenericSig {
        formals: context.formals[inherited.len()..].to_vec(),
        formal_bounds: context.formal_bounds[inherited.len()..].to_vec(),
        receiver,
        params: Vec::new(),
        ret,
        return_policy: Default::default(),
    })
}

/// Bit-packed boolean flags for a [`MetaValueParam`], collapsing its `has_default`/`materialized`/
/// `vararg`/`recv_fun`/`nullable`/`suspend_fun`/`has_type_facts` bytes into one. Read through
/// the `MetaValueParam` accessors of the same names; built with the `with_*` chain. Headroom for
/// one more flag before the byte fills.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MvpFlags(u8);

impl MvpFlags {
    const HAS_DEFAULT: u8 = 1 << 0;
    const MATERIALIZED: u8 = 1 << 1;
    const VARARG: u8 = 1 << 2;
    const RECV_FUN: u8 = 1 << 3;
    const NULLABLE: u8 = 1 << 4;
    const SUSPEND_FUN: u8 = 1 << 5;
    const HAS_TYPE_FACTS: u8 = 1 << 6;

    #[inline]
    const fn with(mut self, mask: u8, on: bool) -> Self {
        if on {
            self.0 |= mask;
        } else {
            self.0 &= !mask;
        }
        self
    }
    #[inline]
    const fn has(self, mask: u8) -> bool {
        self.0 & mask != 0
    }

    #[inline]
    pub const fn with_has_default(self, on: bool) -> Self {
        self.with(Self::HAS_DEFAULT, on)
    }
    #[inline]
    pub const fn with_materialized(self, on: bool) -> Self {
        self.with(Self::MATERIALIZED, on)
    }
    #[inline]
    pub const fn with_vararg(self, on: bool) -> Self {
        self.with(Self::VARARG, on)
    }
    #[inline]
    pub const fn with_recv_fun(self, on: bool) -> Self {
        self.with(Self::RECV_FUN, on)
    }
    #[inline]
    pub const fn with_nullable(self, on: bool) -> Self {
        self.with(Self::NULLABLE, on)
    }
    #[inline]
    pub const fn with_suspend_fun(self, on: bool) -> Self {
        self.with(Self::SUSPEND_FUN, on)
    }
    #[inline]
    pub const fn with_has_type_facts(self, on: bool) -> Self {
        self.with(Self::HAS_TYPE_FACTS, on)
    }
}

#[derive(Clone, Debug)]
pub struct MetaValueParam {
    pub ty: Option<TypeName>,
    pub name: String,
    /// Bit-packed `has_default`/`materialized`/`vararg`/`recv_fun`/`nullable`/`suspend_fun` (read
    /// via the accessors below).
    /// `vararg` — `vararg elem: T`. Only `@Metadata` records this: the JVM descriptor shows just the
    /// packed array, so `f(vararg c: Char)` and `f(c: CharArray)` are indistinguishable without it,
    /// and overload resolution cannot know it may spread trailing arguments into the array.
    pub flags: MvpFlags,
    pub recv_fun_receiver: Option<TypeName>,
}

impl MetaValueParam {
    #[inline]
    pub fn has_default(&self) -> bool {
        self.flags.has(MvpFlags::HAS_DEFAULT)
    }
    #[inline]
    pub fn materialized(&self) -> bool {
        self.flags.has(MvpFlags::MATERIALIZED)
    }
    #[inline]
    pub fn vararg(&self) -> bool {
        self.flags.has(MvpFlags::VARARG)
    }
    #[inline]
    pub fn nullable(&self) -> bool {
        self.flags.has(MvpFlags::NULLABLE)
    }
    #[inline]
    pub fn recv_fun(&self) -> bool {
        self.flags.has(MvpFlags::RECV_FUN)
    }
    /// The parameter's declared type is a `suspend` FUNCTION TYPE (`suspend Scope.(Req) -> Resp`) —
    /// metadata's `Type.flags` SUSPEND_TYPE bit, the only witness that the CPS-erased
    /// `FunctionN+1<…, Continuation<T>, Any?>` shape is a suspend function type and not a
    /// source-level continuation-taking one.
    #[inline]
    pub fn suspend_fun(&self) -> bool {
        self.flags.has(MvpFlags::SUSPEND_FUN)
    }
    /// Whether the parameter's declared `Type` was resolved, either inline or through its enclosing
    /// type table. If neither representation can be read, type-level facts are absent rather than
    /// false, and a consumer must not treat them as disclaimers.
    #[inline]
    pub fn has_type_facts(&self) -> bool {
        self.flags.has(MvpFlags::HAS_TYPE_FACTS)
    }
}

/// Bit-packed boolean flags for a [`MetaFn`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MfnFlags(u16);

impl MfnFlags {
    const IS_INLINE: u16 = 1 << 0;
    const IS_SUSPEND: u16 = 1 << 1;
    const IS_EXTENSION: u16 = 1 << 2;
    const RET_NULLABLE: u16 = 1 << 3;
    const IS_OPERATOR: u16 = 1 << 4;
    const LOW_PRIORITY: u16 = 1 << 5;
    const IS_INFIX: u16 = 1 << 6;
    const HAS_REIFIED_TYPE_PARAMS: u16 = 1 << 7;
    const DEPRECATED_HIDDEN: u16 = 1 << 8;

    #[inline]
    const fn with(mut self, mask: u16, on: bool) -> Self {
        if on {
            self.0 |= mask;
        } else {
            self.0 &= !mask;
        }
        self
    }
    #[inline]
    const fn has(self, mask: u16) -> bool {
        self.0 & mask != 0
    }

    #[inline]
    pub const fn with_is_inline(self, on: bool) -> Self {
        self.with(Self::IS_INLINE, on)
    }
    #[inline]
    pub const fn with_is_suspend(self, on: bool) -> Self {
        self.with(Self::IS_SUSPEND, on)
    }
    #[inline]
    pub const fn with_is_extension(self, on: bool) -> Self {
        self.with(Self::IS_EXTENSION, on)
    }
    #[inline]
    pub const fn with_ret_nullable(self, on: bool) -> Self {
        self.with(Self::RET_NULLABLE, on)
    }
    #[inline]
    pub const fn with_is_operator(self, on: bool) -> Self {
        self.with(Self::IS_OPERATOR, on)
    }
    #[inline]
    pub const fn with_low_priority(self, on: bool) -> Self {
        self.with(Self::LOW_PRIORITY, on)
    }
    #[inline]
    pub const fn with_is_infix(self, on: bool) -> Self {
        self.with(Self::IS_INFIX, on)
    }
    #[inline]
    pub const fn with_has_reified_type_params(self, on: bool) -> Self {
        self.with(Self::HAS_REIFIED_TYPE_PARAMS, on)
    }
    #[inline]
    pub const fn with_deprecated_hidden(self, on: bool) -> Self {
        self.with(Self::DEPRECATED_HIDDEN, on)
    }
}

/// A function decoded from a `Class`/`Package` `@Metadata` message — the *metadata-truth* signature
/// kotlinc resolves against (`JvmProtoBufUtil.getJvmMethodSignature`): the Kotlin name, the JVM method
/// name + descriptor (from the `method_signature` extension when present), Kotlin visibility/`inline`/
/// `suspend`/`operator`, and the extension-receiver class. For an `inline` function the bytecode is
/// `private`/synthetic, so these flags differ from the access flags — metadata is primary, bytecode is
/// fallback.
#[derive(Clone, Debug)]
pub struct MetaFn {
    pub kotlin_name: String,
    pub jvm_name: String,
    /// The JVM descriptor from the `method_signature` extension; `None` when metadata omits it (the
    /// caller may then fall back to a bytecode method of the same name, or compute it from proto types).
    pub jvm_desc: Option<&'static str>,
    pub visibility: crate::types::Visibility,
    /// Bit-packed `is_inline`/`is_suspend`/`is_extension`/`is_operator`/`ret_nullable` (read via the
    /// accessors below).
    /// `is_extension` — whether this is an EXTENSION (a receiver of any kind, class or
    /// type parameter) vs a true top-level function; lets the classpath ext index avoid mis-indexing a
    /// top-level generic as an extension on its first parameter's type. `ret_nullable` — whether the
    /// Kotlin return type is nullable (`T?`, `Type.nullable`); the JVM descriptor/`Signature` erase this,
    /// only `@Metadata` carries it, and it drives the elvis null-check for a nullable-returning scope fn.
    pub flags: MfnFlags,
    /// Extension-receiver Kotlin class name (`kotlin/Result` for `Result.getOrThrow`), if any. `None` for a
    /// top-level fn AND for an extension on a type PARAMETER — use [`MetaFn::is_extension`] to disambiguate.
    pub receiver_class: Option<TypeName>,
    /// The Kotlin return-type class name (`kotlin/UInt` for `UInt.coerceAtMost`), if it is a class type.
    pub ret_class: Option<TypeName>,
    /// SOURCE value parameters in declaration order. The LENGTH is the source arity: it excludes
    /// synthetic JVM descriptor params such as suspend `Continuation` or Compose `Composer`/masks.
    pub value_params: Vec<MetaValueParam>,
    /// Leading context parameters. Named context parameters retain the same metadata shape as ordinary
    /// value parameters; legacy unnamed context receivers have an empty name.
    pub context_params: Vec<MetaValueParam>,
    /// The metadata-primary generic signature (type parameters + parameter/return gsig nodes), decoded
    /// straight from `@Metadata` rather than the JVM `Signature` attribute — a JVM-agnostic, Kotlin-faithful
    /// source (nullability, variance, Kotlin type identities). `None` when the return type won't decode.
    pub generic_sig: Option<GenericSig>,
    /// The function's declared contract (`Function.contract`, field 32), decoded into the shared
    /// contract IR — the effects the checker applies at call sites (`returns(…) implies …`,
    /// `callsInPlace`). `None` when the function declares no contract.
    pub contract: Option<std::sync::Arc<crate::contracts::Contract>>,
}

impl MetaFn {
    #[inline]
    pub fn is_public(&self) -> bool {
        self.visibility == crate::types::Visibility::Public
    }
    #[inline]
    pub fn is_inline(&self) -> bool {
        self.flags.has(MfnFlags::IS_INLINE)
    }
    #[inline]
    pub fn is_suspend(&self) -> bool {
        self.flags.has(MfnFlags::IS_SUSPEND)
    }
    #[inline]
    pub fn is_extension(&self) -> bool {
        self.flags.has(MfnFlags::IS_EXTENSION)
    }
    #[inline]
    pub fn is_operator(&self) -> bool {
        self.flags.has(MfnFlags::IS_OPERATOR)
    }
    #[inline]
    pub fn is_infix(&self) -> bool {
        self.flags.has(MfnFlags::IS_INFIX)
    }
    #[inline]
    pub fn has_reified_type_params(&self) -> bool {
        self.flags.has(MfnFlags::HAS_REIFIED_TYPE_PARAMS)
    }
    #[inline]
    pub fn ret_nullable(&self) -> bool {
        self.flags.has(MfnFlags::RET_NULLABLE)
    }
    #[inline]
    pub fn low_priority(&self) -> bool {
        self.flags.has(MfnFlags::LOW_PRIORITY)
    }
    /// `@Deprecated(level = HIDDEN)`: the declaration exists for binary compatibility only and
    /// kotlinc removes it from overload resolution entirely. Stamped from the realization
    /// method's `kotlin.Deprecated` annotation after metadata decode.
    #[inline]
    pub fn deprecated_hidden(&self) -> bool {
        self.flags.has(MfnFlags::DEPRECATED_HIDDEN)
    }

    pub fn context_count(&self) -> usize {
        self.context_params.len()
    }

    pub fn parameters(&self) -> impl Iterator<Item = &MetaValueParam> {
        self.context_params.iter().chain(&self.value_params)
    }

    pub fn member_call_sig(&self) -> CallSig {
        let parameters: Vec<_> = self.parameters().collect();
        let (lambda_receivers, lambda_receiver_params) = self.lambda_receiver_shape();
        let mut sig = CallSig::metadata_function(
            parameters.len(),
            parameters.iter().map(|p| p.name.clone()).collect(),
            parameters.iter().map(|p| p.has_default()).collect(),
            lambda_receivers,
            lambda_receiver_params,
            parameters.iter().map(|p| p.materialized()).collect(),
            self.vararg_index()
                .map(|index| index + self.context_count()),
        );
        sig.platform_nullable_params = parameters.iter().map(|p| p.nullable()).collect();
        sig
    }

    /// Decode the semantic receiver-function shape once for every metadata function consumer.
    /// A concrete `Recv.() -> R` carries both the receiver type and the mark; a generic
    /// `T.() -> R` carries only the mark and recovers `T` after call-site substitution.
    pub(super) fn lambda_receiver_shape(&self) -> (Vec<Option<Ty>>, Vec<bool>) {
        (
            self.parameters()
                .map(|p| p.recv_fun_receiver.map(crate::types::Ty::obj_name))
                .collect(),
            self.parameters().map(|p| p.recv_fun()).collect(),
        )
    }

    pub fn vararg_index(&self) -> Option<usize> {
        self.value_params
            .iter()
            .position(|parameter| parameter.vararg())
    }

    pub fn extension_call_sig(&self) -> CallSig {
        self.member_call_sig()
    }
}

/// A JVM method signature carried by Kotlin metadata: method name + descriptor as one fact.
#[derive(Clone, Debug)]
pub struct MetaJvmMethodSig {
    pub name: String,
    pub desc: String,
}

/// One constructor declaration from Kotlin class metadata. `params` is the complete source shape;
/// `jvm_desc` is only the exact key of its platform realization.
#[derive(Clone, Debug)]
pub struct MetaConstructor {
    pub params: ParamList,
    pub jvm_desc: Option<&'static str>,
    /// `@Deprecated(level = HIDDEN)`: binary-compatibility-only, never a resolution candidate.
    /// Stamped from the realization method's `kotlin.Deprecated` annotation after decode.
    pub deprecated_hidden: bool,
}

/// One `Property` decoded from a class's `@Metadata`: its source name, logical (Kotlin) return-type
/// class, the REAL getter/setter JVM method names + descriptors (from the `JvmPropertySignature`
/// extension — so a caller need not guess `getX`), and the source facts a resolver needs (visibility,
/// `const`). The property analogue of [`MetaFn`].
#[derive(Clone, Debug)]
pub struct MetaProp {
    pub name: String,
    /// The Kotlin return-type class name (`kotlin/String`), if it is a class type; `None` for a bare
    /// type parameter.
    pub ret_class: Option<TypeName>,
    /// Whether the Kotlin property return is nullable (`T?`). JVM getter descriptors/signatures erase
    /// this flag, so generic extension-property specialization must restore it from metadata.
    pub ret_nullable: bool,
    /// Metadata-primary generic relation between an extension receiver and its return.
    pub generic_sig: Option<GenericSig>,
    /// The JVM getter method name (`getLength`, or a `@JvmName`/value-class-mangled spelling) + its
    /// descriptor, from the `JvmPropertySignature`. `None` if the metadata omits an explicit getter.
    pub getter: Option<MetaJvmMethodSig>,
    /// The JVM setter (present iff the property is a `var` with an emitted setter).
    pub setter: Option<MetaJvmMethodSig>,
    pub visibility: crate::types::Visibility,
    pub is_const: bool,
    /// `var` (has a setter) vs `val`.
    pub is_var: bool,
    /// The EXTENSION receiver's class name (`val String.foo` → `kotlin/String`) — `None` for an
    /// ordinary member/top-level property.
    pub receiver_class: Option<TypeName>,
    /// Receiver presence, including a type-parameter receiver that has no class name.
    pub is_extension: bool,
}

/// The FULLY-decoded `@kotlin.Metadata` of one classfile — every projection the compiler consumes,
/// materialized in ONE decode at class-parse time. The packed `d1`/`d2` strings are dropped after this
/// decode: nothing downstream re-reads the protobuf, and the raw metadata never lives in a cache. A
/// plain-Java class (no `@Metadata`) is the `Default` (all projections empty) — consumers need not
/// distinguish the sources.
#[derive(Clone, Debug)]
pub struct KotlinMeta {
    /// `Class.flags` visibility for a Kotlin class (`None` for plain Java classes and package facades).
    /// JVM access flags cannot represent Kotlin `internal`, so classifier access must prefer this fact.
    pub class_visibility: Option<crate::types::Visibility>,
    /// `Class.flags.CLASS_KIND`. Present only for class metadata; package/file metadata has no
    /// classifier kind.
    pub class_kind: Option<TypeKind>,
    /// `Flags.IS_FUN_INTERFACE` from the Kotlin `Class.flags` word. Kotlin interfaces participate in
    /// SAM conversion only when this declaration flag is present; structural single-method detection
    /// remains valid for Java interfaces, which have no Kotlin metadata.
    pub is_fun_interface: bool,
    /// `Class.type_parameter` and `Class.supertype`/`supertype_id`, decoded in source terms. These
    /// define the semantic class graph for a Kotlin class; the classfile's superclass/interfaces are
    /// only its JVM realization and must never be unioned into this list.
    pub class_type_parameters: crate::types::TypeParameters<Vec<Vec<Ty>>>,
    pub class_supertypes: Vec<Ty>,
    /// `Class.function` (field 9) — member/extension functions of a class kind. `Arc` slices so a
    /// consumer cache shares the decode instead of copying it.
    pub class_functions: std::sync::Arc<[MetaFn]>,
    /// `Package.function` (field 3) — top-level/extension functions of a file-facade/part kind.
    pub package_functions: std::sync::Arc<[MetaFn]>,
    /// `Class.property` (field 10).
    pub class_properties: std::sync::Arc<[MetaProp]>,
    /// `Package.property` (field 4).
    pub package_properties: std::sync::Arc<[MetaProp]>,
    /// `Package.typeAlias` (field 5): `(full alias internal name, expanded class internal name)`.
    pub type_aliases: Vec<MetaTypeAlias>,
    /// `Class.constructor` (field 8): named-parameter lists in declaration order.
    pub constructors: std::sync::Arc<[MetaConstructor]>,
    /// `Class.companionObjectName` (field 4).
    pub companion_name: Option<String>,
    /// `Class.sealedSubclassFqName` (field 16), as JVM internal names.
    pub sealed_subclasses: Vec<String>,
    /// The `@JvmInline value class` shape (fields 17-19), if this is one.
    pub inline: Option<InlineClass>,
    /// For a MULTI-FILE FACADE (`@Metadata(k = 4)`): the part class internal names its `d1` lists —
    /// the facade has no declarations of its own. Empty for every other kind.
    pub multifile_parts: Vec<String>,
}

impl Default for KotlinMeta {
    fn default() -> Self {
        KotlinMeta {
            class_visibility: None,
            class_kind: None,
            is_fun_interface: false,
            class_type_parameters: crate::types::TypeParameters::default(),
            class_supertypes: Vec::new(),
            class_functions: std::sync::Arc::from([]),
            package_functions: std::sync::Arc::from([]),
            class_properties: std::sync::Arc::from([]),
            package_properties: std::sync::Arc::from([]),
            type_aliases: Vec::new(),
            constructors: std::sync::Arc::from([]),
            companion_name: None,
            sealed_subclasses: Vec::new(),
            inline: None,
            multifile_parts: Vec::new(),
        }
    }
}

impl KotlinMeta {
    /// Whether this classfile carried any Kotlin metadata at all.
    pub fn is_present(&self) -> bool {
        !(self.class_visibility.is_none()
            && self.class_kind.is_none()
            && !self.is_fun_interface
            && self.class_functions.is_empty()
            && self.package_functions.is_empty()
            && self.class_properties.is_empty()
            && self.package_properties.is_empty()
            && self.type_aliases.is_empty()
            && self.constructors.is_empty()
            && self.companion_name.is_none()
            && self.sealed_subclasses.is_empty()
            && self.inline.is_none()
            && self.multifile_parts.is_empty())
    }
}

/// Shared decode context: the protobuf message body plus the resolved string table — built once per
/// classfile, consumed by every projection.
struct MetaCtx<'a> {
    msg: &'a [u8],
    records: &'a [Rec],
    d2: &'a [String],
}

/// Decode a classfile's `@Metadata` into [`KotlinMeta`] — the ONE place the packed representation is
/// read. `k` is the header kind: a multi-file facade (`k = 4`) lists its part class names in `d1`
/// verbatim (no protobuf); every other kind carries the BitEncoded proto.
pub fn decode_metadata(
    d1: &[String],
    d2: &[String],
    k: Option<i32>,
    this_class: &str,
    methods: &[super::classreader::MethodSig],
) -> KotlinMeta {
    if k == Some(4) {
        return KotlinMeta {
            multifile_parts: d1.to_vec(),
            ..KotlinMeta::default()
        };
    }
    if d1.is_empty() {
        return KotlinMeta {
            class_visibility: (k == Some(1)).then_some(crate::types::Visibility::Public),
            ..KotlinMeta::default()
        };
    }
    let bytes = decode_d1(d1);
    let (st_body, msg) = split_d1(&bytes);
    let records = parse_string_table(st_body);
    let ctx = MetaCtx {
        msg,
        records: &records,
        d2,
    };
    // A class-body extension may use an ENCLOSING class parameter as its extension receiver
    // (`class Scope<T> { fun T.f() }`). Function metadata stores only the parameter id; decoding it
    // without the containing Class.type_parameter table silently widens the receiver to `Any` and
    // turns its physical leading parameter into an apparent value parameter. Recover the names once
    // at the metadata boundary and give every class function the complete semantic context.
    let class_tparams = if k == Some(1) {
        type_param_bodies(ctx.msg, CLASS_TYPE_PARAMETER_FIELD)
            .into_iter()
            .filter_map(parse_type_param)
            .filter_map(|parameter| {
                Some((
                    parameter.id,
                    resolve_string(ctx.records, ctx.d2, parameter.name_id as usize)?,
                ))
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let (class_type_params, class_type_param_bounds, class_supertypes) = if k == Some(1) {
        decode_class_signature(&ctx)
    } else {
        (Vec::new(), Vec::new(), Vec::new())
    };
    let class_type_param_variances = if k == Some(1) {
        type_param_bodies(ctx.msg, CLASS_TYPE_PARAMETER_FIELD)
            .into_iter()
            .filter_map(parse_type_param)
            .map(|parameter| parameter.variance)
            .collect()
    } else {
        Vec::new()
    };
    let class_flags = (k == Some(1)).then(|| class_flags(&ctx));
    // `@Deprecated(level = HIDDEN)` lives on the JVM realization (a `kotlin.Deprecated` runtime
    // annotation), not in the protobuf. A declaration whose realization method carries it exists
    // for binary compatibility only — kotlinc drops it from resolution — so stamp the fact here,
    // at the one point where declarations and classfile methods are both in hand. Matching is by
    // exact (name, descriptor); a declaration without a descriptor stays visible.
    let realization_hidden = |jvm_name: &str, jvm_desc: Option<&str>| -> bool {
        jvm_desc.is_some_and(|descriptor| {
            methods.iter().any(|method| {
                method.deprecated_hidden
                    && method.name == jvm_name
                    && method.descriptor == descriptor
            })
        })
    };
    let stamp_functions = |mut functions: Vec<MetaFn>| -> Vec<MetaFn> {
        for function in &mut functions {
            if realization_hidden(&function.jvm_name, function.jvm_desc) {
                function.flags = function.flags.with_deprecated_hidden(true);
            }
        }
        functions
    };
    let class_functions = stamp_functions(decode_functions(
        &ctx,
        9,
        &class_tparams,
        &class_type_param_bounds,
    ))
    .into();
    let mut constructors = ctor_params(&ctx);
    for constructor in &mut constructors {
        constructor.deprecated_hidden = realization_hidden("<init>", constructor.jvm_desc);
    }
    let class_properties =
        decode_properties(&ctx, 10, &class_tparams, &class_type_param_bounds).into();
    KotlinMeta {
        class_visibility: class_flags
            .map(flags_visibility)
            .map(crate::types::Visibility::from_metadata),
        class_kind: class_flags.map(metadata_class_kind),
        is_fun_interface: class_flags.is_some_and(|flags| flags & (1u64 << 14) != 0),
        class_type_parameters: crate::types::TypeParameters::new(
            class_type_params,
            class_type_param_bounds,
            class_type_param_variances,
        ),
        class_supertypes,
        class_functions,
        package_functions: stamp_functions(decode_functions(&ctx, 3, &[], &[])).into(),
        class_properties,
        package_properties: decode_properties(&ctx, 4, &[], &[]).into(),
        type_aliases: type_aliases(&ctx, this_class),
        constructors: constructors.into(),
        companion_name: companion_name(&ctx),
        sealed_subclasses: sealed_subclasses(&ctx),
        inline: inline_class(&ctx),
        multifile_parts: Vec::new(),
    }
}

fn metadata_class_kind(flags: u64) -> TypeKind {
    match (flags >> 6) & 0x7 {
        1 => TypeKind::Interface,
        2 => TypeKind::Enum,
        4 => TypeKind::Annotation,
        5 | 6 => TypeKind::Object,
        _ => TypeKind::Class,
    }
}

/// Decode a Kotlin class's own type parameters and direct applied supertypes from the Class proto.
/// Both inline `supertype` (field 6) and table-backed `supertype_id` (field 2) are valid encodings.
fn decode_class_signature(ctx: &MetaCtx<'_>) -> (Vec<String>, Vec<Vec<Ty>>, Vec<Ty>) {
    let parsed_params = type_param_bodies(ctx.msg, CLASS_TYPE_PARAMETER_FIELD)
        .into_iter()
        .filter_map(parse_type_param)
        .collect::<Vec<_>>();
    let mut table = None;
    let mut inline_supertypes = Vec::new();
    let mut supertype_ids = Vec::new();
    let mut pb = Pb { b: ctx.msg, i: 0 };
    while !pb.at_end() {
        let Some(tag) = pb.varint() else { break };
        match (tag >> 3, tag & 7) {
            (2, 0) => {
                if let Some(id) = pb.varint() {
                    supertype_ids.push(id);
                }
            }
            (2, 2) => {
                let Some(len) = pb.varint() else { break };
                let Some(body) = pb.bytes(len as usize) else {
                    break;
                };
                supertype_ids.extend(packed_varints(body));
            }
            (6, 2) => {
                let Some(len) = pb.varint() else { break };
                let Some(body) = pb.bytes(len as usize) else {
                    break;
                };
                inline_supertypes.push(body);
            }
            (30, 2) => {
                let Some(len) = pb.varint() else { break };
                table = pb.bytes(len as usize);
            }
            (_, wire) => {
                if pb.skip(wire).is_none() {
                    break;
                }
            }
        }
    }
    let Some(parameters) =
        type_parameter_context(&[], &[], &parsed_params, ctx.records, ctx.d2, table)
    else {
        return (Vec::new(), Vec::new(), Vec::new());
    };

    let decode = |body| {
        decode_metadata_type(
            body,
            table,
            ctx.records,
            ctx.d2,
            &parameters.names,
            &parameters.erasure_bounds,
            false,
            0,
        )
    };
    let supertypes = inline_supertypes
        .into_iter()
        .filter_map(decode)
        .chain(supertype_ids.into_iter().filter_map(|id| {
            let (body, nullable) = type_table_entry(table?, id as usize)?;
            decode_metadata_type(
                body,
                table,
                ctx.records,
                ctx.d2,
                &parameters.names,
                &parameters.erasure_bounds,
                nullable,
                0,
            )
        }))
        .collect();
    (parameters.formals, parameters.formal_bounds, supertypes)
}

fn decode_metadata_type(
    body: &[u8],
    type_table: Option<&[u8]>,
    records: &[Rec],
    d2: &[String],
    tparams: &HashMap<u64, String>,
    bounds: &HashMap<String, Ty>,
    table_nullable: bool,
    depth: u32,
) -> Option<Ty> {
    if depth > BUILTIN_TYPE_DEPTH_LIMIT {
        return None;
    }
    let node = parse_type_node(body)?;
    let args = node
        .arguments
        .into_iter()
        .map(|argument| match argument {
            ParsedTypeArgument::Inline(body, projection) => decode_metadata_type(
                body,
                type_table,
                records,
                d2,
                tparams,
                bounds,
                false,
                depth + 1,
            )
            .map(|ty| project_ty(projection, ty)),
            ParsedTypeArgument::Table(id, projection) => {
                let (body, nullable) = type_table_entry(type_table?, id as usize)?;
                decode_metadata_type(
                    body,
                    type_table,
                    records,
                    d2,
                    tparams,
                    bounds,
                    nullable,
                    depth + 1,
                )
                .map(|ty| project_ty(projection, ty))
            }
            ParsedTypeArgument::Star => {
                Some(Ty::out_projection(Ty::nullable(Ty::obj("kotlin/Any"))))
            }
        })
        .collect::<Option<Vec<_>>>()?;
    let flexible_upper_bound = node.flexible_upper_bound;
    let flexible_upper_bound_id = node.flexible_upper_bound_id;
    let definitely_non_null = node.definitely_non_null;
    let nullable = node.nullable || table_nullable;
    let receiver_fun = node.annotation_ids.iter().any(|&id| {
        resolve_class_name(records, d2, id as usize)
            .is_some_and(|name| name == "kotlin/ExtensionFunctionType")
    });
    let suspend_fun = parse_type_facts(body).suspend_fun;
    let ty = if let Some(id) = node.class_id.or(node.type_alias_id) {
        let internal = resolve_class_name(records, d2, id as usize)?;
        let decoded = gsig_from_kotlin_class(&internal, args, receiver_fun);
        if suspend_fun {
            source_suspend_function_type(decoded)
        } else {
            decoded
        }
    } else {
        let name = node
            .type_parameter_id
            .and_then(|id| tparams.get(&id).cloned())
            .or_else(|| {
                node.type_parameter_name_id
                    .and_then(|id| resolve_string(records, d2, id as usize))
            })?;
        Ty::ty_param(
            &name,
            bounds
                .get(&name)
                .copied()
                .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any"))),
        )
    };
    let ty = if definitely_non_null {
        ty.non_null()
    } else if nullable {
        Ty::nullable(ty)
    } else {
        ty
    };
    let flexible_upper = flexible_upper_bound
        .and_then(|body| {
            decode_metadata_type(
                body,
                type_table,
                records,
                d2,
                tparams,
                bounds,
                false,
                depth + 1,
            )
        })
        .or_else(|| {
            let (body, nullable) =
                type_table_entry(type_table?, flexible_upper_bound_id? as usize)?;
            decode_metadata_type(
                body,
                type_table,
                records,
                d2,
                tparams,
                bounds,
                nullable,
                depth + 1,
            )
        });
    Some(if flexible_upper.is_some_and(Ty::is_nullable) {
        Ty::platform_nullable(ty.non_null())
    } else {
        ty
    })
}

/// Kotlin `Class.flags`. The protobuf default is PUBLIC FINAL (`6`); decode the word once through
/// this boundary helper so every individual class flag uses identical wire-format defaulting.
fn class_flags(ctx: &MetaCtx<'_>) -> u64 {
    let mut flags = 6u64;
    let mut pb = Pb { b: ctx.msg, i: 0 };
    while !pb.at_end() {
        let Some(tag) = pb.varint() else { break };
        match (tag >> 3, tag & 7) {
            (1, 0) => flags = pb.varint().unwrap_or(6),
            (_, wire) => {
                if pb.skip(wire).is_none() {
                    break;
                }
            }
        }
    }
    flags
}

/// Decode every `Function` (proto field `fn_field`: 9 in a `Class`, 3 in a `Package`) of this class's
/// `@Metadata` message into [`MetaFn`]s. The single metadata-primary function reader.
fn decode_functions(
    ctx: &MetaCtx,
    fn_field: u64,
    class_tparams: &[(u64, String)],
    class_tparam_bounds: &[Vec<Ty>],
) -> Vec<MetaFn> {
    let declared_classifier = |ty: Ty| match ty.non_null() {
        Ty::Obj(internal, _) => Some(internal),
        _ => None,
    };
    let mut out = Vec::new();
    let records = ctx.records;
    let d2 = ctx.d2;
    // The containing message's `TypeTable` (Package.type_table / Class.type_table = 30): contract
    // `is_instance_type_id` references index into it. kotlinc appends the table AFTER the
    // functions, so pre-scan for it before the main decode loop (which decodes contracts inline).
    let mut type_table_body: Option<Vec<u8>> = None;
    {
        let mut scan = Pb { b: ctx.msg, i: 0 };
        while !scan.at_end() {
            let Some(tag) = scan.varint() else { break };
            match (tag >> 3, tag & 7) {
                (30, 2) => {
                    let Some(len) = scan.varint() else { break };
                    let Some(b) = scan.bytes(len as usize) else {
                        break;
                    };
                    type_table_body = Some(b.to_vec());
                }
                (_, w) => {
                    if scan.skip(w).is_none() {
                        break;
                    }
                }
            }
        }
    }
    let mut pb = Pb { b: ctx.msg, i: 0 };
    while !pb.at_end() {
        let Some(tag) = pb.varint() else { break };
        match (tag >> 3, tag & 7) {
            (f, 2) if f == fn_field => {
                let Some(len) = pb.varint() else { break };
                let Some(fbody) = pb.bytes(len as usize) else {
                    break;
                };
                if let Some(pf) = parse_function(fbody) {
                    let Some(kotlin_name) = d2.get(pf.name_id as usize).cloned() else {
                        continue;
                    };
                    // The `JvmMethodSignature` name/desc are plain string-table entries — resolve them as
                    // kotlinc's `getString` does (predefined/d2 + substring/replace), NOT as class names.
                    let (mut jvm_name, jvm_desc) = match pf.jvm_sig {
                        Some(signature) => (
                            signature
                                .name_id
                                .and_then(|id| resolve_string(records, d2, id as usize))
                                .unwrap_or_else(|| kotlin_name.clone()),
                            signature
                                .desc_id
                                .and_then(|id| resolve_string(records, d2, id as usize)),
                        ),
                        None => (kotlin_name.clone(), None),
                    };
                    // A `@kotlin.jvm.JvmName("...")` annotation is the AUTHORITATIVE bytecode name (kotlinc
                    // uses it for the emitted method) — e.g. each `@OverloadResolutionByLambdaReturnType`
                    // `sumOf` overload carries `@JvmName("sumOfInt")`/`@JvmName("sumOfLong")`. The
                    // `method_signature` extension may omit it, so read it from the annotation directly.
                    // Absent → the Kotlin name stands.
                    if let Some(n) = annotation_jvm_name(&pf.annotation_bodies, records, d2) {
                        jvm_name = n;
                    }
                    // Kotlin may place parameter types in the function-local table or in the
                    // containing package/class table. The nearer function table shadows the outer
                    // one, matching every other type-table lookup in this decoder.
                    let function_type_table =
                        pf.type_table_body.as_deref().or(type_table_body.as_deref());
                    let type_context = type_parameter_context(
                        class_tparams,
                        class_tparam_bounds,
                        &pf.type_params,
                        records,
                        d2,
                        function_type_table,
                    );
                    let decode_type = |body, id| {
                        decode_metadata_type_ref(
                            metadata_type_ref(body, id, function_type_table),
                            function_type_table,
                            records,
                            d2,
                            type_context.as_ref(),
                        )
                    };
                    let receiver_ty = decode_type(pf.receiver_body.as_deref(), pf.receiver_type_id);
                    let ret_ty = decode_type(pf.return_body.as_deref(), pf.return_type_id);
                    let receiver_class = receiver_ty.and_then(declared_classifier);
                    let ret_class = ret_ty.and_then(declared_classifier);
                    let decode_parameter = |p: &ParsedValueParam| {
                        let resolved_type = value_parameter_type(p, function_type_table);
                        let (decoded_type, receiver_type) = decode_value_parameter_types(
                            p,
                            function_type_table,
                            records,
                            d2,
                            type_context.as_ref(),
                        );
                        let (recv_fun, recv_fun_receiver) = receiver_function_shape(receiver_type);
                        let type_facts = resolved_type
                            .map(|(body, table_nullable)| {
                                let mut facts = parse_type_facts(body);
                                facts.nullable |= table_nullable;
                                facts
                            })
                            .unwrap_or_default();
                        MetaValueParam {
                            ty: decoded_type.and_then(declared_classifier),
                            // Param names are plain string-table entries (like the JVM name/desc), not class names.
                            name: resolve_string(records, d2, p.name_id as usize)
                                .unwrap_or_default(),
                            flags: MvpFlags::default()
                                .with_has_default(p.has_default)
                                .with_materialized(p.materialized)
                                .with_vararg(
                                    p.vararg_elem_body.is_some() || p.vararg_elem_id.is_some(),
                                )
                                .with_recv_fun(recv_fun)
                                .with_nullable(type_facts.nullable)
                                .with_suspend_fun(type_facts.suspend_fun)
                                .with_has_type_facts(resolved_type.is_some()),
                            recv_fun_receiver,
                        }
                    };
                    let value_params: Vec<MetaValueParam> =
                        pf.value_params.iter().map(decode_parameter).collect();
                    let context_params = if !pf.context_params.is_empty() {
                        Some(pf.context_params.iter().map(decode_parameter).collect())
                    } else {
                        let context_types = pf
                            .context_receiver_bodies
                            .iter()
                            .map(|body| (Some(body.as_slice()), None))
                            .chain(
                                pf.context_receiver_type_ids
                                    .iter()
                                    .map(|&id| (None, Some(id))),
                            )
                            .map(|(body, id)| decode_type(body, id));
                        context_types
                            .map(|ty| {
                                let ty = ty?;
                                Some(MetaValueParam {
                                    ty: declared_classifier(ty),
                                    name: String::new(),
                                    flags: MvpFlags::default()
                                        .with_nullable(ty.is_nullable())
                                        .with_has_type_facts(true),
                                    recv_fun_receiver: None,
                                })
                            })
                            .collect::<Option<Vec<_>>>()
                    };
                    let Some(context_params) = context_params else {
                        crate::trace_compiler!(
                            "metadata_missing_type",
                            "discard function {}: undecodable context receiver",
                            kotlin_name,
                        );
                        continue;
                    };
                    if value_params.iter().any(|parameter| {
                        parameter
                            .ty
                            .is_some_and(|ty| ty.render().contains("Function"))
                    }) {
                        crate::trace_compiler!(
                            "metadata_functions",
                            "function {} params={:?}",
                            kotlin_name,
                            value_params
                                .iter()
                                .map(|parameter| (
                                    parameter.name.as_str(),
                                    parameter.ty.map(TypeName::render),
                                    parameter.recv_fun(),
                                    parameter.recv_fun_receiver.map(TypeName::render),
                                    parameter.suspend_fun()
                                ))
                                .collect::<Vec<_>>()
                        );
                    }
                    if value_params.iter().any(|parameter| parameter.ty.is_none()) {
                        crate::trace_compiler!(
                            "metadata_missing_type",
                            "function {} receiver={:?} params={:?}",
                            kotlin_name,
                            receiver_class.map(TypeName::render),
                            value_params
                                .iter()
                                .map(|parameter| (
                                    parameter.name.as_str(),
                                    parameter.ty.map(TypeName::render),
                                    parameter.recv_fun(),
                                    parameter.recv_fun_receiver.map(TypeName::render),
                                    parameter.suspend_fun()
                                ))
                                .collect::<Vec<_>>()
                        );
                    }
                    // The metadata-primary generic signature. For now the structure MATCHES the JVM
                    // `Signature`-derived gsig (extension: receiver at `params[0]`; member/top-level: value
                    // params only) so it is a drop-in replacement; the uniform member-receiver synthesis is
                    // a later step (`class_receiver = None` here keeps a member's params value-only).
                    let generic_sig = build_generic_sig(
                        &pf,
                        records,
                        d2,
                        class_tparams,
                        class_tparam_bounds,
                        None,
                        function_type_table,
                    );
                    let contract = pf.contract_body.as_deref().and_then(|body| {
                        let tparams = type_parameter_context(
                            &[],
                            &[],
                            &pf.type_params,
                            records,
                            d2,
                            function_type_table,
                        )
                        .map(|c| c.names)
                        .unwrap_or_default();
                        // Function-level table wins if present; the container's otherwise.
                        decode_contract(body, records, d2, &tparams, function_type_table)
                            .map(std::sync::Arc::new)
                    });
                    if pf.contract_body.is_some() {
                        crate::trace_compiler!(
                            "metadata_contracts",
                            "contract function={} value_params={} context_params={} context_receivers={}",
                            kotlin_name,
                            pf.value_params.len(),
                            pf.context_params.len(),
                            pf.context_receiver_bodies.len()
                                + pf.context_receiver_type_ids.len(),
                        );
                    }
                    // `JvmMethodSignature.desc` is optional when the physical descriptor is the
                    // default derived from the protobuf types. Omission therefore does not mean
                    // that the callable lacks a JVM realization. Materialize the default here,
                    // while the complete metadata declaration is still available, so no consumer
                    // has to search bytecode methods or guess from a source name.
                    let jvm_desc = jvm_desc.or_else(|| {
                        let signature = generic_sig.as_ref()?;
                        let mut physical_params = Vec::new();
                        if pf.has_receiver {
                            physical_params.push(signature.receiver?);
                        }
                        physical_params.extend(signature.params.iter().copied());
                        let physical_ret = if pf.is_suspend {
                            physical_params.push(Ty::obj("kotlin/coroutines/Continuation"));
                            Ty::obj("kotlin/Any")
                        } else {
                            signature.ret
                        };
                        Some(method_descriptor(&physical_params, physical_ret))
                    });
                    out.push(MetaFn {
                        kotlin_name,
                        jvm_name,
                        jvm_desc: jvm_desc.map(|s| intern(&s)),
                        visibility: pf.visibility,
                        flags: MfnFlags::default()
                            .with_is_inline(pf.is_inline)
                            .with_is_suspend(pf.is_suspend)
                            .with_is_extension(pf.has_receiver)
                            .with_ret_nullable(ret_ty.is_some_and(Ty::is_nullable))
                            .with_is_operator(pf.is_operator)
                            .with_is_infix(pf.is_infix)
                            .with_has_reified_type_params(
                                pf.type_params.iter().any(|parameter| parameter.reified),
                            )
                            .with_low_priority(has_annotation(
                                &pf.annotation_bodies,
                                records,
                                d2,
                                "kotlin/internal/LowPriorityInOverloadResolution",
                            )),
                        receiver_class,
                        ret_class,
                        value_params,
                        generic_sig,
                        contract,
                        context_params,
                    });
                }
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

/// Functions declared in a `Class`'s `@Metadata` (member + companion functions live in their own class).
pub fn class_functions(ci: &ClassInfo) -> &[MetaFn] {
    &ci.meta.class_functions
}

/// Top-level / extension functions declared in a file facade's `Package` `@Metadata`.
pub fn package_functions(ci: &ClassInfo) -> &[MetaFn] {
    &ci.meta.package_functions
}

/// Public type aliases declared in a file facade's `Package` `@Metadata`.
pub fn package_type_aliases(ci: &ClassInfo) -> &[MetaTypeAlias] {
    &ci.meta.type_aliases
}

/// Named-parameter lists of the class's constructors, from its `@Metadata`.
pub fn class_constructor_params(ci: &ClassInfo) -> Vec<ParamList> {
    // A HIDDEN-deprecated constructor is not a resolution candidate in ANY channel; leaving its
    // param list here would let the named/default slot channel map a call against a declaration
    // no selection can ever produce (kotlinpoet's hidden 3-name `ClassName` ctor swallowing the
    // 3-arg call that belongs to the visible vararg secondary).
    ci.meta
        .constructors
        .iter()
        .filter(|constructor| !constructor.deprecated_hidden)
        .map(|constructor| constructor.params.clone())
        .collect()
}

pub fn class_constructors(ci: &ClassInfo) -> &[MetaConstructor] {
    &ci.meta.constructors
}

/// The class's companion object name (`Companion`, or a custom one), from its `@Metadata`.
pub fn class_companion_name(ci: &ClassInfo) -> Option<String> {
    ci.meta.companion_name.clone()
}

/// The direct subclasses of a `sealed` class, from its `@Metadata`, as JVM internal names.
pub fn class_sealed_subclasses(ci: &ClassInfo) -> Vec<String> {
    ci.meta.sealed_subclasses.clone()
}

pub fn class_properties(ci: &ClassInfo) -> &[MetaProp] {
    &ci.meta.class_properties
}

/// Top-level / extension properties declared in a file facade's `Package` `@Metadata`.
pub fn package_properties(ci: &ClassInfo) -> &[MetaProp] {
    &ci.meta.package_properties
}

pub fn class_inline(ci: &ClassInfo) -> Option<&InlineClass> {
    ci.meta.inline.as_ref()
}

/// Type aliases declared in a file facade's `Package` `@Metadata` (`typealias Alias = Real` →
/// `("Alias", "pkg/Real")`). Reads the `Package.typeAlias` entries (field 5) from the proto directly:
/// each alias's name (field 2, a string-table id) and its EXPANDED type (field 6, fully resolved to the
/// concrete class, so an alias chain collapses to the final class; falls back to the immediate
/// underlying type, field 4). This is robust where the older `d2` `$annotations` heuristic was not — a
/// file facade also carries annotated top-level properties whose `$annotations` markers that heuristic
/// would misread as aliases.
fn type_aliases(ctx: &MetaCtx, this_class: &str) -> Vec<MetaTypeAlias> {
    let mut out = Vec::new();
    let records = ctx.records;
    let d2 = ctx.d2;
    let mut pb = Pb { b: ctx.msg, i: 0 };
    while !pb.at_end() {
        let Some(tag) = pb.varint() else { break };
        match (tag >> 3, tag & 7) {
            // Package.typeAlias = 5 (length-delimited message).
            (5, 2) => {
                let Some(len) = pb.varint() else { break };
                let Some(body) = pb.bytes(len as usize) else {
                    break;
                };
                if let Some(alias) = parse_type_alias(body, records, d2) {
                    // Key the alias by its FULL internal name — its declaring package (the facade's) plus
                    // the alias's simple name — so `kotlin/collections/ArrayList` is distinct from any other
                    // package's `ArrayList`. `resolve_type` looks it up by that full name.
                    let pkg = this_class.rsplit_once('/').map_or("", |(p, _)| p);
                    let full = if pkg.is_empty() {
                        alias.name.clone()
                    } else {
                        format!("{pkg}/{}", alias.name)
                    };
                    out.push(MetaTypeAlias {
                        name: full,
                        ..alias
                    });
                }
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

/// One public `typealias` from a file facade's `Package` metadata.
#[derive(Clone, Debug)]
pub struct MetaTypeAlias {
    /// The alias's simple name; the classpath index keys it by declaring package.
    pub name: String,
    /// The expanded target's class internal name.
    pub target: String,
    /// The alias's own type-parameter names, in declaration order — the substitution domain.
    pub formals: Vec<String>,
    /// The target applied to its own arguments, with the alias's parameters as `Ty::TyParam`.
    /// Metadata decoding is authoritative: an alias is not published when this type cannot decode.
    pub expansion: Ty,
}

/// Decode a public `TypeAlias` message → its name, the expanded/underlying class internal name, and
/// the EXPANSION TEMPLATE: the target applied to its own arguments, with the alias's parameters left
/// as `Ty::TyParam`. `typealias Lens<S, A> = PLens<S, S, A, A>` declares two parameters for a
/// four-parameter target, so a use site's arguments must be substituted into the template rather
/// than pasted onto the target — the template is the only place that mapping exists.
fn parse_type_alias(body: &[u8], records: &[Rec], d2: &[String]) -> Option<MetaTypeAlias> {
    let mut pb = Pb { b: body, i: 0 };
    let mut flags = 6u64;
    let mut name_id: Option<u64> = None;
    let mut expanded_class: Option<u64> = None;
    let mut underlying_class: Option<u64> = None;
    let mut expanded_body: Option<&[u8]> = None;
    let mut underlying_body: Option<&[u8]> = None;
    while !pb.at_end() {
        let tag = pb.varint()?;
        match (tag >> 3, tag & 7) {
            (1, 0) => flags = pb.varint()?,
            (2, 0) => name_id = pb.varint(),
            (4, 2) => {
                let len = pb.varint()? as usize;
                let tb = pb.bytes(len)?;
                underlying_class = parse_type_class_name(tb);
                underlying_body = Some(tb);
            }
            (6, 2) => {
                let len = pb.varint()? as usize;
                let tb = pb.bytes(len)?;
                expanded_class = parse_type_class_name(tb);
                expanded_body = Some(tb);
            }
            (_, w) => pb.skip(w)?,
        }
    }
    if flags_visibility(flags) != VIS_PUBLIC {
        return None;
    }
    let name = d2.get(name_id? as usize).cloned()?;
    let class_id = expanded_class.or(underlying_class)?;
    let internal = resolve_class_name(records, d2, class_id as usize)?;
    // The alias's OWN type parameters, by metadata id, so the expansion decodes their uses as
    // `TyParam` rather than as unknown classifiers.
    let mut parameters: HashMap<u64, String> = HashMap::new();
    let mut formals = Vec::new();
    for parameter in type_param_bodies(body, TYPE_ALIAS_TYPE_PARAMETER_FIELD)
        .into_iter()
        .filter_map(parse_type_param)
    {
        if let Some(parameter_name) = resolve_string(records, d2, parameter.name_id as usize) {
            parameters.insert(parameter.id, parameter_name.clone());
            formals.push(parameter_name);
        }
    }
    let expansion = expanded_body.or(underlying_body).and_then(|type_body| {
        decode_metadata_type(
            type_body,
            None,
            records,
            d2,
            &parameters,
            &HashMap::new(),
            false,
            0,
        )
    })?;
    Some(MetaTypeAlias {
        name,
        target: internal,
        formals,
        expansion,
    })
}

/// Constructor source parameter names/default flags from `Class` `@Metadata`, in declaration order.
fn ctor_params(ctx: &MetaCtx) -> Vec<MetaConstructor> {
    let mut out = Vec::new();
    let records = ctx.records;
    let d2 = ctx.d2;
    let mut type_table = None;
    let mut table_scan = Pb { b: ctx.msg, i: 0 };
    while !table_scan.at_end() {
        let Some(tag) = table_scan.varint() else {
            break;
        };
        match (tag >> 3, tag & 7) {
            (30, 2) => {
                let Some(len) = table_scan.varint() else {
                    break;
                };
                type_table = table_scan.bytes(len as usize).map(Vec::from);
            }
            (_, wire) => {
                if table_scan.skip(wire).is_none() {
                    break;
                }
            }
        }
    }
    let parsed_type_params = type_param_bodies(ctx.msg, CLASS_TYPE_PARAMETER_FIELD)
        .into_iter()
        .filter_map(parse_type_param)
        .collect::<Vec<_>>();
    let type_context = type_parameter_context(
        &[],
        &[],
        &parsed_type_params,
        records,
        d2,
        type_table.as_deref(),
    );
    let mut pb = Pb { b: ctx.msg, i: 0 };
    while !pb.at_end() {
        let Some(tag) = pb.varint() else { break };
        match (tag >> 3, tag & 7) {
            (8, 2) => {
                // Class.constructor (repeated Constructor)
                let Some(len) = pb.varint() else { break };
                let Some(cbody) = pb.bytes(len as usize) else {
                    break;
                };
                let mut cp = Pb { b: cbody, i: 0 };
                // Constructor.flags has protobuf default `6` (PUBLIC, no annotations), just like
                // Function.flags. Kotlin omits that field for an ordinary public primary constructor.
                let mut flags = 6u64;
                let mut names = Vec::new();
                let mut defaults = Vec::new();
                let mut types = Vec::new();
                let mut recv_fun = Vec::new();
                let mut vararg = None;
                let mut jvm_desc = None;
                while !cp.at_end() {
                    let Some(ct) = cp.varint() else { break };
                    match (ct >> 3, ct & 7) {
                        (1, 0) => flags = cp.varint().unwrap_or(0),
                        (2, 2) => {
                            // Constructor.value_parameter (repeated ValueParameter)
                            let Some(vlen) = cp.varint() else { break };
                            let Some(vbody) = cp.bytes(vlen as usize) else {
                                break;
                            };
                            let Some(parameter) = parse_value_parameter(vbody) else {
                                continue;
                            };
                            names.push(
                                resolve_string(records, d2, parameter.name_id as usize)
                                    .unwrap_or_default(),
                            );
                            defaults.push(parameter.has_default);
                            let (decoded, receiver_type) = decode_value_parameter_types(
                                &parameter,
                                type_table.as_deref(),
                                records,
                                d2,
                                type_context.as_ref(),
                            );
                            types.push(decoded.unwrap_or(Ty::Error));
                            recv_fun.push(receiver_function_shape(receiver_type).0);
                            if parameter.vararg_elem_body.is_some()
                                || parameter.vararg_elem_id.is_some()
                            {
                                vararg = Some(names.len() - 1);
                            }
                        }
                        (100, 2) => {
                            let Some(len) = cp.varint() else { break };
                            let Some(body) = cp.bytes(len as usize) else {
                                break;
                            };
                            jvm_desc = parse_jvm_signature(body)
                                .and_then(|signature| signature.desc_id)
                                .and_then(|id| resolve_string(records, d2, id as usize));
                        }
                        (_, w) => {
                            if cp.skip(w).is_none() {
                                break;
                            }
                        }
                    }
                }
                let params = ParamList {
                    visibility: crate::types::Visibility::from_metadata(flags_visibility(flags)),
                    names,
                    defaults,
                    types,
                    recv_fun,
                    vararg,
                };
                let jvm_desc = jvm_desc.or_else(|| {
                    params
                        .types
                        .iter()
                        .all(|ty| *ty != Ty::Error)
                        .then(|| method_descriptor(&params.types, Ty::Unit))
                });
                out.push(MetaConstructor {
                    params,
                    jvm_desc: jvm_desc.map(|descriptor| intern(&descriptor)),
                    deprecated_hidden: false,
                });
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

/// The simple name of a class's companion object (`Class.companion_object_name = 4`), e.g. `Companion`.
/// `None` if the class has no companion.
fn companion_name(ctx: &MetaCtx) -> Option<String> {
    let records = ctx.records;
    let d2 = ctx.d2;
    let mut pb = Pb { b: ctx.msg, i: 0 };
    while !pb.at_end() {
        let Some(tag) = pb.varint() else { break };
        match (tag >> 3, tag & 7) {
            (4, 0) => {
                let id = pb.varint()?;
                return resolve_class_name(records, d2, id as usize);
            }
            (_, w) => {
                if pb.skip(w).is_none() {
                    break;
                }
            }
        }
    }
    None
}

/// The direct subclasses of a `sealed` class, from its `@Metadata` — `Class.sealedSubclassFqName` (field
/// 16, a repeated `QualifiedName` index). Returned as JVM internal names (`lib/D$A`), so an exhaustive
/// `when` over a CLASSPATH sealed subject can be proven exhaustive the same way a same-module one is. Only
/// a sealed class records these, so a non-empty result also implies `is_sealed`.
fn sealed_subclasses(ctx: &MetaCtx) -> Vec<String> {
    let records = ctx.records;
    let d2 = ctx.d2;
    let mut out = Vec::new();
    let push_id = |id: usize, out: &mut Vec<String>| {
        if let Some(name) = resolve_class_name(records, d2, id) {
            // A metadata class name spells a nested type with `.` (`lib/D.A`) and the package with `/`;
            // the JVM internal name uses `$` for nesting (`lib/D$A`). Convert only after the last `/`.
            let internal = match name.rfind('/') {
                Some(slash) => {
                    format!("{}{}", &name[..=slash], name[slash + 1..].replace('.', "$"))
                }
                None => name.replace('.', "$"),
            };
            if !out.contains(&internal) {
                out.push(internal);
            }
        }
    };
    let mut pb = Pb { b: ctx.msg, i: 0 };
    while !pb.at_end() {
        let Some(tag) = pb.varint() else { break };
        match (tag >> 3, tag & 7) {
            // sealedSubclassFqName = 16, unpacked repeated varint.
            (16, 0) => {
                if let Some(id) = pb.varint() {
                    push_id(id as usize, &mut out);
                }
            }
            // packed repeated form.
            (16, 2) => {
                if let Some(n) = pb.varint() {
                    if let Some(bytes) = pb.bytes(n as usize) {
                        let mut ip = Pb { b: bytes, i: 0 };
                        while let Some(id) = ip.varint() {
                            push_id(id as usize, &mut out);
                        }
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
    out
}

/// A `JvmMethodSignature` reference decoded from metadata: `(name string id, descriptor string id)`.
type JvmSig = Option<ParsedJvmSignature>;

/// Parse a `JvmPropertySignature` extension body → the getter (field 3) and setter (field 4)
/// `JvmMethodSignature`s. Either is `None` when absent.
fn parse_jvm_property_signature(body: &[u8]) -> (JvmSig, JvmSig) {
    let mut pb = Pb { b: body, i: 0 };
    let mut getter = None;
    let mut setter = None;
    while !pb.at_end() {
        let Some(tag) = pb.varint() else { break };
        match (tag >> 3, tag & 7) {
            (3, 2) => {
                if let Some(n) = pb.varint() {
                    if let Some(b) = pb.bytes(n as usize) {
                        getter = parse_jvm_signature(b);
                    }
                }
            }
            (4, 2) => {
                if let Some(n) = pb.varint() {
                    if let Some(b) = pb.bytes(n as usize) {
                        setter = parse_jvm_signature(b);
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
    (getter, setter)
}

/// Decode every `Property` (`prop_field`: 10 in a `Class`, 4 in a `Package`) of this metadata message
/// into [`MetaProp`]s — the property analogue of [`decode_functions`]. Carries the REAL getter/setter
/// JVM names from the `JvmPropertySignature`, so a resolver reads the accessor instead of guessing `getX`.
fn decode_properties(
    ctx: &MetaCtx,
    prop_field: u64,
    class_tparams: &[(u64, String)],
    class_tparam_bounds: &[Vec<Ty>],
) -> Vec<MetaProp> {
    let mut out = Vec::new();
    let records = ctx.records;
    let d2 = ctx.d2;
    let mut type_table = None;
    let mut props: Vec<&[u8]> = Vec::new();
    let mut pb = Pb { b: ctx.msg, i: 0 };
    while !pb.at_end() {
        let Some(tag) = pb.varint() else { break };
        match (tag >> 3, tag & 7) {
            (f, 2) if f == prop_field => {
                let Some(n) = pb.varint() else { break };
                let Some(b) = pb.bytes(n as usize) else { break };
                props.push(b);
            }
            (30, 2) => {
                let Some(n) = pb.varint() else { break };
                let Some(b) = pb.bytes(n as usize) else { break };
                type_table = Some(b);
            }
            (_, w) => {
                if pb.skip(w).is_none() {
                    break;
                }
            }
        }
    }
    let type_body_of_id = |tid: u64| type_table_entry(type_table?, tid as usize);
    let type_of_id = |tid: u64| -> Option<TypeName> {
        let (tb, _) = type_body_of_id(tid)?;
        let cn = parse_type_class_name(tb)?;
        resolve_class_name(records, d2, cn as usize).map(|name| type_name(&name))
    };
    let type_id_nullable = |tid: u64| -> bool {
        type_table
            .and_then(|table| type_table_entry(table, tid as usize))
            .is_some_and(|(body, table_nullable)| table_nullable || parse_type_nullable(body))
    };
    // Current `Property.flags` is field 11. Its shared declaration prefix is HAS_ANNOTATIONS(0) ·
    // VISIBILITY(1..3) · MODALITY(4..5) · MEMBER_KIND(6..7), so property-specific IS_VAR and IS_CONST
    // live at bits 8 and 11. Older metadata may instead carry `old_flags` in field 1, whose shorter
    // layout puts those facts at bits 6 and 9. Decode the two words independently and prefer field 11
    // regardless of wire order; collapsing them into one mutable word would let a reordered legacy
    // field override the authoritative modern value. The shared modern constants also drive both writers.
    const LEGACY_IS_VAR: u64 = 1 << 6;
    const LEGACY_IS_CONST: u64 = 1 << 9;
    for prop in props {
        let mut p = Pb { b: prop, i: 0 };
        let mut name_id = None;
        let mut ret = None;
        let mut ret_nullable = false;
        let mut ret_body = None;
        let mut legacy_flags = None;
        let mut modern_flags = None;
        let mut sig = (None, None);
        let mut receiver_class = None;
        let mut receiver_body = None;
        let mut receiver_nullable = false;
        let mut type_params = Vec::new();
        while !p.at_end() {
            let Some(tag) = p.varint() else { break };
            match (tag >> 3, tag & 7) {
                (1, 0) => legacy_flags = p.varint(),
                (11, 0) => modern_flags = p.varint(),
                (2, 0) => name_id = p.varint(),
                (3, 2) => {
                    let Some(n) = p.varint() else { break };
                    let Some(tb) = p.bytes(n as usize) else { break };
                    ret_nullable = parse_type_nullable(tb);
                    ret_body = Some(tb);
                    ret = parse_type_class_name(tb)
                        .and_then(|cn| resolve_class_name(records, d2, cn as usize))
                        .map(|name| type_name(&name));
                }
                (4, 2) => {
                    let Some(n) = p.varint() else { break };
                    let Some(body) = p.bytes(n as usize) else {
                        break;
                    };
                    if let Some(parameter) = parse_type_param(body) {
                        type_params.push(parameter);
                    }
                }
                (9, 0) => {
                    if let Some(tid) = p.varint() {
                        ret = type_of_id(tid);
                        ret_nullable = type_id_nullable(tid);
                        ret_body = type_body_of_id(tid).map(|(body, _)| body);
                    }
                }
                // `Property.receiver_type` (field 5, inline `Type`) / `receiver_type_id` (field 10) —
                // PRESENCE marks an EXTENSION property; recover the receiver's class name.
                (5, 2) => {
                    let Some(n) = p.varint() else { break };
                    let Some(tb) = p.bytes(n as usize) else { break };
                    receiver_body = Some(tb);
                    receiver_nullable = parse_type_nullable(tb);
                    receiver_class = parse_type_class_name(tb)
                        .and_then(|cn| resolve_class_name(records, d2, cn as usize))
                        .map(|name| type_name(&name));
                }
                (10, 0) => {
                    if let Some(tid) = p.varint() {
                        receiver_class = type_of_id(tid);
                        if let Some((body, table_nullable)) = type_body_of_id(tid) {
                            receiver_body = Some(body);
                            receiver_nullable = table_nullable || parse_type_nullable(body);
                        }
                    }
                }
                (100, 2) => {
                    let Some(n) = p.varint() else { break };
                    let Some(ext) = p.bytes(n as usize) else {
                        break;
                    };
                    sig = parse_jvm_property_signature(ext);
                }
                (_, w) => {
                    if p.skip(w).is_none() {
                        break;
                    }
                }
            }
        }
        let Some(name_id) = name_id else { continue };
        let Some(name) = resolve_string(records, d2, name_id as usize) else {
            continue;
        };
        let (getter_signature, setter_signature) = sig;
        let (flags, is_var_bit, is_const_bit) = modern_flags.map_or_else(
            || {
                legacy_flags.map_or(
                    (
                        crate::metadata::property_flags::DEFAULT,
                        crate::metadata::property_flags::IS_VAR,
                        crate::metadata::property_flags::IS_CONST,
                    ),
                    |flags| (flags, LEGACY_IS_VAR, LEGACY_IS_CONST),
                )
            },
            |flags| {
                (
                    flags,
                    crate::metadata::property_flags::IS_VAR,
                    crate::metadata::property_flags::IS_CONST,
                )
            },
        );
        let is_var = setter_signature.is_some() || flags & is_var_bit != 0;
        let generic_sig = build_property_generic_sig(
            class_tparams,
            class_tparam_bounds,
            &type_params,
            ret_body,
            ret_nullable,
            receiver_body,
            receiver_nullable,
            records,
            d2,
            type_table,
        );
        if let Some(signature) = &generic_sig {
            ret = signature.ret.non_null().obj_internal();
            ret_nullable = signature.ret.is_nullable();
            if receiver_body.is_some() {
                receiver_class = signature
                    .receiver
                    .and_then(|ty| ty.non_null().obj_internal());
            }
        }
        // `JvmPropertySignature` and each nested `JvmMethodSignature` field are optional when the
        // physical accessor follows Kotlin's default mapping. Complete that metadata declaration
        // here, while its receiver/return types and flags are still together. Downstream symbol
        // sources may verify the resulting handle against bytecode, but must not guess it by name.
        let accessor_types = generic_sig.as_ref().map(|signature| {
            let mut getter_params = Vec::new();
            if let Some(receiver) = signature.receiver {
                getter_params.push(receiver);
            }
            (getter_params, signature.ret)
        });
        let default_getter_desc = accessor_types
            .as_ref()
            .map(|(params, ty)| method_descriptor(params, *ty));
        let default_setter_desc = accessor_types.as_ref().map(|(params, ty)| {
            let mut params = params.clone();
            params.push(*ty);
            method_descriptor(&params, Ty::Unit)
        });
        let materialize_accessor =
            |signature: Option<ParsedJvmSignature>,
             default_name: String,
             default_desc: Option<String>| {
                let name = signature
                    .and_then(|signature| signature.name_id)
                    .and_then(|id| resolve_string(records, d2, id as usize))
                    .unwrap_or(default_name);
                let desc = signature
                    .and_then(|signature| signature.desc_id)
                    .and_then(|id| resolve_string(records, d2, id as usize))
                    .or(default_desc)?;
                Some(MetaJvmMethodSig { name, desc })
            };
        let getter = materialize_accessor(
            getter_signature,
            crate::names::property_getter_name(&name),
            default_getter_desc,
        );
        let setter = is_var
            .then(|| {
                materialize_accessor(
                    setter_signature,
                    crate::names::property_setter_name(&name),
                    default_setter_desc,
                )
            })
            .flatten();
        out.push(MetaProp {
            name,
            ret_class: ret,
            ret_nullable,
            generic_sig,
            getter,
            setter,
            visibility: crate::types::Visibility::from_metadata(flags_visibility(flags)),
            is_const: flags & is_const_bit != 0,
            is_var,
            receiver_class,
            is_extension: receiver_body.is_some(),
        });
    }
    out
}

/// A classpath `@JvmInline value class` decoded from `@Metadata`: the single underlying property and its
/// Kotlin type. A value class erases to this underlying type on the old JVM (`UInt` → `kotlin/Int` → `int`;
/// `Result<T>` → a type parameter → `None`, erasing to `Object`).
#[derive(Clone, Debug)]
pub struct InlineClass {
    /// Kotlin class name of the underlying type (`kotlin/Int` for `UInt`); `None` when the underlying is a
    /// type parameter (`Result<T>`), which erases to `kotlin/Any`/`Object`.
    pub underlying_class: Option<String>,
    /// Whether the underlying type is declared NULLABLE (`value class X(val v: String?)`). Decides the
    /// null-representation: a nullable use `X?` stays UNBOXED (null carried by the underlying reference)
    /// only when the underlying is non-null; over a nullable underlying `X?` must box. `None` when the
    /// metadata didn't carry the type inline or in the type table (unknown — treat as nullable,
    /// conservative).
    pub underlying_nullable: Option<bool>,
    /// The sole property's name (`data` for `UInt`/`Result`).
    pub property_name: Option<String>,
}

/// If `ci` is a Kotlin `@JvmInline value class`, its decoded [`InlineClass`] (presence of the
/// `inline_class_underlying_type` proto field is the marker); `None` for an ordinary class.
fn inline_class(ctx: &MetaCtx) -> Option<InlineClass> {
    let records = ctx.records;
    let d2 = ctx.d2;
    let mut pb = Pb { b: ctx.msg, i: 0 };
    let mut is_value = false;
    let mut underlying_class = None;
    let mut underlying_nullable = None;
    let mut property_name = None;
    let mut underlying_type_id: Option<u64> = None;
    let mut type_table: Option<&[u8]> = None;
    while !pb.at_end() {
        let Some(tag) = pb.varint() else { break };
        match (tag >> 3, tag & 7) {
            (17, 0) => {
                // inline_class_underlying_property_name (name id in table)
                let id = pb.varint()?;
                is_value = true;
                property_name = resolve_string(records, d2, id as usize);
            }
            (18, 2) => {
                // inline_class_underlying_type (inline Type message)
                let n = pb.varint()? as usize;
                let tbody = pb.bytes(n)?;
                is_value = true;
                let (cls, nullable) = parse_type_class_and_nullable(tbody);
                underlying_class = cls.and_then(|id| resolve_class_name(records, d2, id as usize));
                underlying_nullable = Some(nullable);
            }
            (19, 0) => {
                // inline_class_underlying_type_id (type id in the class's TypeTable) — marks a value
                // class even when the type isn't inlined; resolved from the table after the loop.
                underlying_type_id = pb.varint();
                is_value = true;
            }
            (30, 2) => {
                // Class.typeTable — holds the referenced `Type`s when the compiler shares them by id.
                let n = pb.varint()? as usize;
                type_table = pb.bytes(n);
            }
            (_, w) => {
                if pb.skip(w).is_none() {
                    break;
                }
            }
        }
    }
    // Resolve a table-carried underlying type (field 19): index the TypeTable; a type at
    // `index >= firstNullable` is nullable even without its own `nullable` flag (the table's
    // nullability-sharing optimization).
    if underlying_class.is_none() {
        if let (Some(id), Some(tt)) = (underlying_type_id, type_table) {
            if let Some((tbody, table_nullable)) = type_table_entry(tt, id as usize) {
                let (cls, own_nullable) = parse_type_class_and_nullable(tbody);
                underlying_class =
                    cls.and_then(|cid| resolve_class_name(records, d2, cid as usize));
                underlying_nullable = Some(own_nullable || table_nullable);
            }
        }
    }
    // When BOTH the inline type (18) and the table id (19) are absent, the underlying type is the
    // declared type of the underlying PROPERTY (field 17 names it; `Class.property` = field 10
    // carries it) — kotlinc omits the class-level copy as derivable. `Property.returnType` = 3
    // (inline `Type`) or `returnTypeId` = 9 (a TypeTable id; 7 is the RECEIVER type id).
    if is_value && underlying_class.is_none() {
        if let Some(pname) = &property_name {
            let mut pb = Pb { b: ctx.msg, i: 0 };
            while !pb.at_end() {
                let Some(tag) = pb.varint() else { break };
                match (tag >> 3, tag & 7) {
                    (10, 2) => {
                        let Some(n) = pb.varint() else { break };
                        let Some(prop) = pb.bytes(n as usize) else {
                            break;
                        };
                        let Some((nid, rt, rtid)) = parse_property_name_and_return(prop) else {
                            continue;
                        };
                        if resolve_string(records, d2, nid as usize).as_deref() != Some(pname) {
                            continue;
                        }
                        if let Some(tbody) = rt {
                            let (cls, nullable) = parse_type_class_and_nullable(tbody);
                            underlying_class =
                                cls.and_then(|cid| resolve_class_name(records, d2, cid as usize));
                            underlying_nullable = Some(nullable);
                        } else if let (Some(id), Some(tt)) = (rtid, type_table) {
                            if let Some((tbody, table_nullable)) = type_table_entry(tt, id as usize)
                            {
                                let (cls, own_nullable) = parse_type_class_and_nullable(tbody);
                                underlying_class = cls
                                    .and_then(|cid| resolve_class_name(records, d2, cid as usize));
                                underlying_nullable = Some(own_nullable || table_nullable);
                            }
                        }
                        break;
                    }
                    (_, w) => {
                        if pb.skip(w).is_none() {
                            break;
                        }
                    }
                }
            }
        }
    }
    is_value.then_some(InlineClass {
        underlying_class,
        underlying_nullable,
        property_name,
    })
}

/// A property's decoded `(name id, inline returnType body, returnTypeId)`.
type PropNameAndReturn<'a> = (u64, Option<&'a [u8]>, Option<u64>);

/// A `Property` message's `name` (field 2, string id), inline `returnType` (field 3), and
/// `returnTypeId` (field 9, a TypeTable index — field 7 is the RECEIVER type id, unlike `Function`).
fn parse_property_name_and_return(body: &[u8]) -> Option<PropNameAndReturn<'_>> {
    let mut pb = Pb { b: body, i: 0 };
    let mut name = None;
    let mut rt: Option<&[u8]> = None;
    let mut rtid = None;
    while !pb.at_end() {
        let tag = pb.varint()?;
        match (tag >> 3, tag & 7) {
            (2, 0) => name = pb.varint(),
            (3, 2) => {
                let n = pb.varint()? as usize;
                rt = pb.bytes(n);
            }
            (9, 0) => rtid = pb.varint(),
            (_, w) => pb.skip(w)?,
        }
    }
    Some((name?, rt, rtid))
}

/// The `index`-th `Type` in a `TypeTable` message (field 1, repeated), plus whether the table's
/// `firstNullable` (field 2) marks it nullable: kotlinc stores a nullable variant of type N at
/// `firstNullable + k` positions, flagging every entry at `index >= firstNullable` nullable.
fn type_table_entry(body: &[u8], index: usize) -> Option<(&[u8], bool)> {
    let mut pb = Pb { b: body, i: 0 };
    let mut types: Vec<&[u8]> = Vec::new();
    let mut first_nullable: Option<u64> = None;
    while !pb.at_end() {
        let tag = pb.varint()?;
        match (tag >> 3, tag & 7) {
            (1, 2) => {
                let n = pb.varint()? as usize;
                types.push(pb.bytes(n)?);
            }
            (2, 0) => first_nullable = pb.varint(),
            (_, w) => pb.skip(w)?,
        }
    }
    let t = types.get(index)?;
    let nullable = first_nullable.is_some_and(|fnl| index as u64 >= fnl);
    Some((t, nullable))
}

/// A `Type` message's `class_name` (field 6) and `nullable` flag (field 3).
fn parse_type_class_and_nullable(body: &[u8]) -> (Option<u64>, bool) {
    let mut pb = Pb { b: body, i: 0 };
    let mut class_name = None;
    let mut nullable = false;
    while !pb.at_end() {
        let Some(tag) = pb.varint() else { break };
        match (tag >> 3, tag & 7) {
            (3, 0) => nullable = pb.varint().is_some_and(|v| v != 0),
            (6, 0) => {
                class_name = pb.varint();
                if class_name.is_none() {
                    break;
                }
            }
            (_, w) => {
                if pb.skip(w).is_none() {
                    break;
                }
            }
        }
    }
    (class_name, nullable)
}

// === `.kotlin_builtins` supertype reader ==========================================================
// A `.kotlin_builtins` resource (e.g. `kotlin/collections/collections.kotlin_builtins`) stores a
// `BuiltInsProtoBuf.PackageFragment` preceded by a `BuiltInsBinaryVersion` header — a big-endian int
// count followed by that many big-endian ints (`BuiltInsBinaryVersion.readFrom`). The Kotlin collection
// read-only/mutable hierarchy (`MutableList : List, MutableCollection`) lives in the fragment's `Class`
// messages and exists nowhere else (the JVM descriptor erases both `List` and `MutableList` to
// `java/util/List`). Each `Class.supertype_id` (packed, field 2) indexes the class's `type_table`
// (field 30 → `TypeTable.type` field 1), whose `Type.class_name` (field 6) is a `QualifiedNameTable`
// id, resolved against the fragment's `StringTable` exactly as kotlinc's `NameResolverImpl`.

/// One `QualifiedNameTable.QualifiedName`: parent id (`-1` at the root), short-name id into the
/// `StringTable`, and kind (`0` CLASS, `1` PACKAGE, `2` LOCAL; default PACKAGE).
struct QName {
    parent: i64,
    short: usize,
    kind: u64,
}

fn parse_qname(body: &[u8]) -> QName {
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
fn resolve_qname(qnames: &[QName], strings: &[String], mut idx: i64) -> String {
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
fn strip_builtins_header(data: &[u8]) -> Option<&[u8]> {
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

fn project_builtin_ty(projection: ParsedProjection, ty: BuiltinTy) -> BuiltinTy {
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
    /// The member's OWN type parameters (`<R>` of `fold`), with their declared upper bounds — kept
    /// apart from the class's so a consumer can build a generic signature whose formals shadow
    /// correctly.
    pub formals: Vec<BuiltinTypeParam>,
    /// Whether the declared return type is nullable (`V?`) — the JVM descriptor erases it, only the
    /// `.kotlin_builtins` `Type.nullable` flag carries it (`Map.get(K): V?`, `firstOrNull(): T?`).
    pub ret_nullable: bool,
}

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
}

#[derive(Default)]
pub struct BuiltinPackage {
    pub classes: std::collections::HashMap<String, BuiltinClass>,
    pub functions: Vec<BuiltinFunction>,
}

/// One constructor declared by a builtin class. Unlike a function it has no return type or name;
/// its value-parameter types and source visibility are the complete semantic signature.
pub struct BuiltinConstructor {
    pub params: Vec<BuiltinTy>,
    pub visibility: crate::types::Visibility,
}

/// One declared type parameter of a builtin class or member: its source name and decoded upper bounds
/// (`E` unbounded, `T : Comparable<T>`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuiltinTypeParam {
    pub name: String,
    pub bounds: Vec<BuiltinTy>,
    pub variance: crate::types::TypeVariance,
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
    pub is_nested: bool,
    /// The JVM class access flags the same `Class.flags` word describes (`public static interface
    /// abstract` for `kotlin/collections/Map.Entry`) — what an `InnerClasses` entry naming this builtin
    /// has to carry when the mapped JVM owner has no class file to read it off.
    pub access: u16,
    /// Nullable returns for declared function members keyed by `(name, value-arity)` (`Map.get(K): V?`,
    /// `firstOrNull(): T?`). A call may still resolve to the ERASED classpath method (`java/util/Map.get`
    /// returns `Object`), which carries no Kotlin nullability — this is then the only surviving record
    /// that the source return is `T?`. Consulted by the member walk to null-annotate that resolved return.
    pub nullable_member_returns: Vec<(String, usize)>,
}

/// The tables a `.kotlin_builtins` `Class` resolves its types against: the fragment's string and
/// qualified-name tables plus the class's own `type_table` (`Class.type_table` = field 30).
struct BuiltinTables<'a> {
    strings: &'a [String],
    qnames: &'a [QName],
    types: &'a [&'a [u8]],
}

/// A `TypeParameter.id` → source name map. A builtins `Type` names a type parameter by that id
/// (`Type.type_parameter` = field 7); without the map the type is undecodable and the whole member
/// used to be dropped.
type TypeParamNames = std::collections::HashMap<u64, String>;

/// Decode the type of one builtins `ValueParameter`. Functions and constructors use the same
/// message; keeping one reader prevents their accepted type-table layouts from drifting.
fn builtin_value_parameter_type(
    body: &[u8],
    tables: &BuiltinTables<'_>,
    tparams: &TypeParamNames,
) -> Option<BuiltinTy> {
    let mut value = Pb { b: body, i: 0 };
    let mut ty = None;
    while !value.at_end() {
        let tag = value.varint()?;
        match (tag >> 3, tag & 7) {
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
    ty
}

/// Decode either wire representation of a metadata type. Producers may inline the `Type` message or
/// reference the enclosing type table; consumers must treat those as the same declaration shape.
fn builtin_type_ref(
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
const BUILTIN_TYPE_DEPTH_LIMIT: u32 = 16;

impl BuiltinTables<'_> {
    /// Resolve the shared [`parse_type_node`] wire shape through a builtins fragment's tables. A type is
    /// `class_name` (field 6) with `argument`s, `type_parameter` (field 7, by id), or
    /// `type_parameter_name` (field 9, by string). An argument may carry its type inline or by table id;
    /// builtins commonly use the latter, so those edges consume the recursion budget as well.
    fn ty(&self, body: &[u8], tparams: &TypeParamNames, depth: u32) -> Option<BuiltinTy> {
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

    fn ty_by_id(&self, id: usize, tparams: &TypeParamNames, depth: u32) -> Option<BuiltinTy> {
        self.ty(self.types.get(id)?, tparams, depth)
    }

    /// Decode a run of `TypeParameter` messages: their names (added to `tparams` so a bound may refer
    /// to a sibling) and their upper bounds. Bounds are decoded against the names alone — a recursive
    /// bound (`T : Comparable<T>`) therefore terminates instead of chasing itself.
    fn type_params(&self, bodies: &[&[u8]], tparams: &mut TypeParamNames) -> Vec<BuiltinTypeParam> {
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
                bounds: tp
                    .upper_bound_ids
                    .iter()
                    .filter_map(|&id| self.ty_by_id(id as usize, tparams, 0))
                    .chain(
                        tp.upper_bound_bodies
                            .iter()
                            .filter_map(|b| self.ty(b, tparams, 0)),
                    )
                    .collect(),
            })
            .collect()
    }
}

/// `Class.type_parameter`. Field 5 on a `Class` — where a `Function`/`Property` instead carries its
/// `receiver_type`, hence the two distinct constants.
const CLASS_TYPE_PARAMETER_FIELD: u64 = 5;
/// `TypeAlias.typeParameter` — the alias's OWN parameters, which its expansion refers to.
const TYPE_ALIAS_TYPE_PARAMETER_FIELD: u64 = 3;
/// `Function.type_parameter` / `Property.type_parameter`. Both are field 4 (matching the decoders in
/// [`class_functions`] and [`class_properties`]); field 5 on those messages is `receiver_type`.
const MEMBER_TYPE_PARAMETER_FIELD: u64 = 4;

/// Collect a message's repeated `type_parameter` sub-message bodies. The field number differs by
/// carrier — see [`CLASS_TYPE_PARAMETER_FIELD`] / [`MEMBER_TYPE_PARAMETER_FIELD`].
fn type_param_bodies(body: &[u8], field: u64) -> Vec<&[u8]> {
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

fn parse_builtin_package_functions(
    package: &[u8],
    strings: &[String],
    qnames: &[QName],
) -> Vec<BuiltinFunction> {
    let mut functions = Vec::new();
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
    functions
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
            })
        })
        .collect()
}

/// Parse a `.kotlin_builtins` resource → every declared `Class` (qualified name → its supertypes +
/// members). ONE walk over the fragment's `StringTable`/`QualifiedNameTable`/`Class` tables; each
/// class's supertypes and member types are resolved through its `type_table` (field 30 → `Type
/// .class_name` → `QualifiedNameTable`). The single source for both the collection hierarchy and a
/// builtin type's API — no curated/hardcoded tables.
pub fn parse_builtins(data: &[u8]) -> BuiltinPackage {
    let mut out = BuiltinPackage::default();
    let Some(pf) = strip_builtins_header(data) else {
        return out;
    };
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
        // `Class.flags` has the protobuf default PUBLIC FINAL (`6`). Keep wire-format defaulting at
        // the decode boundary, as the ordinary `@Metadata` class reader does, so every consumer sees
        // the semantic flag word. Treating omission as zero conflates it with an explicitly INTERNAL
        // declaration and forces downstream JVM-specific code to guess which input it received.
        let mut flags = 6u64;
        let mut supids: Vec<u64> = Vec::new();
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
                (8, 2) => {
                    // Class.constructor.
                    if let Some(n) = cp.varint() {
                        if let Some(body) = cp.bytes(n as usize) {
                            ctors.push(body);
                        }
                    }
                }
                (10, 2) => {
                    // Class.property = 10 (each: name=2, return_type_id=7 — same shape as a function).
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
            // `Function.flags` has protobuf default PUBLIC FINAL (`6`), matching `parse_function`.
            let mut flags = 6u64;
            let mut params = Vec::new();
            let mut complete = true;
            while !p.at_end() {
                let Some(tag) = p.varint() else { break };
                match (tag >> 3, tag & 7) {
                    (9, 0) => flags = p.varint().unwrap_or(6),
                    (2, 0) => name_id = p.varint(), // name
                    (7, 0) => ret_id = p.varint(),  // return_type_id (type-table ref)
                    (6, 2) => {
                        // value_parameter: ValueParameter.type_id = 4 (type-table ref)
                        if let Some(n) = p.varint() {
                            if let Some(vb) = p.bytes(n as usize) {
                                match builtin_value_parameter_type(vb, &tables, &fn_tparams) {
                                    Some(parameter) => params.push(parameter),
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
                            formals,
                            ret_nullable,
                        });
                    }
                }
            }
        }
        for pb_ in &props {
            let mut prop_tparams = class_tparams.clone();
            let formals = tables.type_params(
                &type_param_bodies(pb_, MEMBER_TYPE_PARAMETER_FIELD),
                &mut prop_tparams,
            );
            let mut p = Pb { b: pb_, i: 0 };
            let mut name_id = None;
            let mut ret_body = None;
            let mut ret_id = None;
            while !p.at_end() {
                let Some(tag) = p.varint() else { break };
                match (tag >> 3, tag & 7) {
                    (2, 0) => name_id = p.varint(),
                    (3, 2) => {
                        let Some(len) = p.varint() else { break };
                        ret_body = p.bytes(len as usize);
                    }
                    // `Property.return_type_id` is field 9 (field 7 is the receiver_type_id — distinct
                    // from `Function`, whose return_type_id is field 7). `val length: Int` → field 9 → Int.
                    (9, 0) => ret_id = p.varint(),
                    (_, w) => {
                        if p.skip(w).is_none() {
                            break;
                        }
                    }
                }
            }
            if let Some(ni) = name_id {
                if let (Some(name), Some(ret)) = (
                    strings.get(ni as usize).cloned(),
                    builtin_type_ref(ret_body, ret_id, &tables, &prop_tparams),
                ) {
                    let ret_nullable = ret.nullable();
                    members.push(BuiltinMember {
                        name,
                        params: vec![],
                        ret,
                        is_property: true,
                        is_operator: false,
                        is_infix: false,
                        formals,
                        ret_nullable,
                    });
                }
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
                members,
                constructors,
                companion_name,
                type_params,
                nullable_member_returns,
                kind: builtin_class_kind(flags),
                visibility: builtin_class_visibility(flags),
                is_nested,
                access: builtin_class_access(flags),
            },
        );
    }
    if let Some(package) = package {
        out.functions = parse_builtin_package_functions(package, &strings, &qnames);
    }
    out
}

fn builtin_class_kind(flags: u64) -> TypeKind {
    metadata_class_kind(flags)
}

fn builtin_class_visibility(flags: u64) -> Visibility {
    match (flags >> 1) & 0x7 {
        1 | 4 => Visibility::Private,
        2 => Visibility::Protected,
        3 => Visibility::Public,
        _ => Visibility::Internal,
    }
}

/// The JVM class access flags a `.kotlin_builtins` `Class.flags` word describes.
///
/// The word packs `HAS_ANNOTATIONS` (bit 0), `VISIBILITY` (bits 1..4), `MODALITY` (bits 4..6),
/// `CLASS_KIND` (bits 6..9 — the field [`BuiltinClass::is_interface`] already reads) and `IS_INNER`
/// (bit 9). Each maps onto the JVM flag the compiler that produced the mapped `java.*` class file
/// emitted, so a nesting fact recovered from the builtin agrees byte-for-byte with the one read off
/// that class file: `kotlin/collections/Map.Entry` is a public, non-inner (hence `ACC_STATIC`) nested
/// interface, exactly the `0x0609` `java/util/Map$Entry` carries.
fn builtin_class_access(flags: u64) -> u16 {
    // VISIBILITY: 0 INTERNAL, 1 PRIVATE, 2 PROTECTED, 3 PUBLIC, 4 PRIVATE_TO_THIS, 5 LOCAL. `internal`
    // is PUBLIC on the JVM — kotlinc mangles the NAME rather than narrowing the flag. An omitted field
    // has already become the protobuf default `6` at the parse boundary; zero here therefore means an
    // explicit INTERNAL declaration, not a second encoding of omission. `local` has no enclosing-
    // declaration visibility to record, so it stays package-private.
    let visibility = match (flags >> 1) & 0x7 {
        0 | 3 => ACC_PUBLIC,
        2 => ACC_PROTECTED,
        1 | 4 => ACC_PRIVATE,
        _ => 0,
    };
    // MODALITY: 0 FINAL, 1 OPEN, 2 ABSTRACT, 3 SEALED. `sealed` is abstract on the JVM.
    let modality = match (flags >> 4) & 0x3 {
        0 => ACC_FINAL,
        2 | 3 => ACC_ABSTRACT,
        _ => 0,
    };
    // CLASS_KIND: 0 CLASS, 1 INTERFACE, 2 ENUM_CLASS, 3 ENUM_ENTRY, 4 ANNOTATION_CLASS, 5 OBJECT,
    // 6 COMPANION_OBJECT. An interface is always `ACC_ABSTRACT` regardless of its declared modality.
    let kind = match (flags >> 6) & 0x7 {
        1 => ACC_INTERFACE | ACC_ABSTRACT,
        2 => ACC_ENUM,
        4 => ACC_ANNOTATION | ACC_INTERFACE | ACC_ABSTRACT,
        _ => 0,
    };
    // A nested class that is not `inner` is `static`; a top-level class is never nested, so the flag is
    // simply ignored by a caller that is not building an `InnerClasses` entry.
    let inner = if (flags >> 9) & 1 == 1 { 0 } else { ACC_STATIC };
    // An interface has no meaningful modality bit of its own (`ACC_FINAL` on an interface is illegal).
    let modality = if kind & ACC_INTERFACE != 0 {
        0
    } else {
        modality
    };
    visibility | modality | kind | inner
}

/// The Kotlin names of every `suspend` function in a class's `@Metadata` (from the `IS_SUSPEND` flag
/// bit). A call to a method of one of these names (in this class) is a suspension point. Both function
/// carriers are read: a file facade's `Package.function` (field 3, top-level `suspend fun`s) AND a
/// `Class.function` (field 9, `suspend` members of a class/interface).
/// Parse a `META-INF/*.kotlin_module` file → `(package fq-name /slashed/, [facade internal names])`.
/// The counterpart of [`crate::metadata::module::build_kotlin_module`]: a 20-byte header (five
/// big-endian `i32` words `[len, maj, min, patch, flags]`) then a `Module` protobuf whose field 1 is
/// repeated `PackageParts { package_fq_name = 1 (dotted), short_class_name = 2 (repeated) }`. The
/// package name is returned slashed (`kotlin/collections`) and each facade as a full internal name
/// (`kotlin/collections/CollectionsKt`) so the caller can resolve it directly.
pub fn read_kotlin_module(bytes: &[u8]) -> Vec<(String, Vec<String>)> {
    if bytes.len() < 20 {
        return Vec::new();
    }
    let mut pb = Pb {
        b: &bytes[20..],
        i: 0,
    };
    // Two carriers matter: the `PackageParts` messages (field 1) and the module-level `jvm_package_name`
    // table (field 3) — the `@JvmPackageName` relocation targets that a `PackageParts` references by
    // index. Collect both, then parse each `PackageParts` against the table.
    let mut parts: Vec<&[u8]> = Vec::new();
    let mut jvm_pkgs: Vec<String> = Vec::new();
    while !pb.at_end() {
        let Some(tag) = pb.varint() else { break };
        match (tag >> 3, tag & 7) {
            (1, 2) => {
                let Some(n) = pb.varint() else { break };
                let Some(msg) = pb.bytes(n as usize) else {
                    break;
                };
                parts.push(msg);
            }
            (3, 2) => {
                let Some(n) = pb.varint() else { break };
                let Some(msg) = pb.bytes(n as usize) else {
                    break;
                };
                jvm_pkgs.push(String::from_utf8_lossy(msg).replace('.', "/"));
            }
            (_, w) => {
                if pb.skip(w).is_none() {
                    break;
                }
            }
        }
    }
    parts
        .into_iter()
        .filter_map(|msg| parse_package_parts(msg, &jvm_pkgs))
        .collect()
}

/// Decode one `PackageParts` message → slashed DECLARED package + full facade internal names. A facade
/// whose class was relocated by `@JvmPackageName` (`kotlin.collections`'s `UArraysKt` emitted into
/// `kotlin/collections/unsigned/`) is still cataloged under its DECLARED package (`@JvmPackageName` is an
/// emit directive, invisible to name resolution) but its internal name uses the JVM location so its
/// `@Metadata` reads from the right class. Fields: `package_fq_name = 1`, `short_class_name = 2`,
/// `class_with_jvm_package_name_short_name = 5`, `class_with_jvm_package_name_package_id = 6` (packed
/// indices into the module `jvm_package_name` table; a list shorter than field 5 repeats its last entry).
fn parse_package_parts(body: &[u8], jvm_pkgs: &[String]) -> Option<(String, Vec<String>)> {
    let mut pb = Pb { b: body, i: 0 };
    let mut pkg: Option<String> = None;
    let mut parts: Vec<String> = Vec::new();
    let mut jvm_shorts: Vec<String> = Vec::new();
    let mut jvm_ids: Vec<usize> = Vec::new();
    while !pb.at_end() {
        let tag = pb.varint()?;
        match (tag >> 3, tag & 7) {
            (1, 2) => {
                let n = pb.varint()? as usize;
                pkg = Some(std::str::from_utf8(pb.bytes(n)?).ok()?.replace('.', "/"));
            }
            (2, 2) => {
                let n = pb.varint()? as usize;
                parts.push(std::str::from_utf8(pb.bytes(n)?).ok()?.to_string());
            }
            (5, 2) => {
                let n = pb.varint()? as usize;
                jvm_shorts.push(std::str::from_utf8(pb.bytes(n)?).ok()?.to_string());
            }
            (6, 2) => {
                let n = pb.varint()? as usize;
                let mut packed = Pb {
                    b: pb.bytes(n)?,
                    i: 0,
                };
                while !packed.at_end() {
                    jvm_ids.push(packed.varint()? as usize);
                }
            }
            (_, w) => pb.skip(w)?,
        }
    }
    let pkg = pkg?;
    let join = |p: &str, f: &str| {
        if p.is_empty() {
            f.to_string()
        } else {
            format!("{p}/{f}")
        }
    };
    let mut facades: Vec<String> = parts.iter().map(|f| join(&pkg, f)).collect();
    for (k, short) in jvm_shorts.iter().enumerate() {
        // The class is relocated to `jvm_pkgs[id]`; an id list shorter than the shorts repeats its last
        // entry (kotlinc's encoding). No id at all → treat as the declared package.
        let idx = jvm_ids.get(k).or_else(|| jvm_ids.last()).copied();
        let loc = idx
            .and_then(|i| jvm_pkgs.get(i))
            .map_or(pkg.as_str(), |s| s.as_str());
        facades.push(join(loc, short));
    }
    Some((pkg, facades))
}

#[cfg(test)]
mod builtin_class_access_tests {
    use super::builtin_class_access;

    /// A `.kotlin_builtins` `Class.flags` word, assembled from the field positions the decoder reads.
    fn flags(visibility: u64, modality: u64, kind: u64, is_inner: bool) -> u64 {
        visibility << 1 | modality << 4 | kind << 6 | u64::from(is_inner) << 9
    }

    /// Every arm has to land on the flags the compiler that produced the mapped `java.*` class file
    /// emitted, since the recovered `InnerClasses` entry is compared byte-for-byte against that one.
    /// The nested INTERFACE case is the one a JDK-less compile actually hits today
    /// (`kotlin/collections/Map.Entry` ⇒ the `0x0609` `java/util/Map$Entry` carries); the rest pin the
    /// mapping so a later builtin of another shape cannot silently pick up wrong flags.
    #[test]
    fn builtin_class_flags_map_to_jvm_access() {
        // public abstract interface, nested (not `inner`) — public static interface abstract.
        assert_eq!(builtin_class_access(flags(3, 2, 1, false)), 0x0609);
        // public final class, nested — public static final (what `MethodHandles$Lookup` carries).
        assert_eq!(builtin_class_access(flags(3, 0, 0, false)), 0x0019);
        // The same class declared `inner`: an inner class is not static.
        assert_eq!(builtin_class_access(flags(3, 0, 0, true)), 0x0011);
        // public abstract class — abstract survives, final does not appear.
        assert_eq!(builtin_class_access(flags(3, 2, 0, false)), 0x0409);
        // `sealed` is abstract on the JVM.
        assert_eq!(builtin_class_access(flags(3, 3, 0, false)), 0x0409);
        // public enum class — public static final enum.
        assert_eq!(builtin_class_access(flags(3, 0, 2, false)), 0x4019);
        // public annotation class — an annotation interface.
        assert_eq!(builtin_class_access(flags(3, 2, 4, false)), 0x2609);
        // private and protected keep their own bit; `internal` is PUBLIC on the JVM (kotlinc mangles
        // the NAME instead of narrowing the flag), so it must not come out package-private.
        assert_eq!(builtin_class_access(flags(1, 0, 0, false)), 0x001a);
        assert_eq!(builtin_class_access(flags(2, 0, 0, false)), 0x001c);
        assert_eq!(builtin_class_access(flags(0, 0, 0, false)), 0x0019);
        // `open` is neither final nor abstract.
        assert_eq!(builtin_class_access(flags(3, 1, 0, false)), 0x0009);
    }

    /// The arm assertions above assemble the flag word with the very shifts the decoder reads back, so
    /// they pin the MAPPING but say nothing about the OFFSETS — moving `IS_INNER` a bit over would keep
    /// them all green. These are `Class.flags` words as they actually appear in the stdlib's shipped
    /// `.kotlin_builtins` fragments, so they pin the layout itself.
    #[test]
    fn real_builtins_flag_words_decode_to_jvm_access() {
        // `kotlin/collections/Map.Entry` — public abstract interface, not `inner`. Anchors VISIBILITY
        // and CLASS_KIND, and is the one word a JDK-less compile actually decodes today; the entry it
        // produces is compared byte-for-byte against `java/util/Map$Entry`'s in
        // `tests/no_jdk_builtin_emit_e2e.rs`.
        assert_eq!(builtin_class_access(0x0066), 0x0609);
        // `kotlin/Any` — public OPEN class. Its modality is the one that pins MODALITY at bits 4..6:
        // read one bit over and it would decode as ABSTRACT or FINAL.
        assert_eq!(builtin_class_access(0x0016), 0x0009);
        // The protobuf default for an omitted `Class.flags` field is PUBLIC FINAL (`6`). The parser
        // supplies that word before this decoder runs; it must land on public static final.
        assert_eq!(builtin_class_access(6), 0x0019);
        // An EXPLICIT zero is INTERNAL FINAL. Internal classes are public in JVM bytecode because
        // Kotlin enforces the visibility through metadata/name mangling rather than the ACC bits.
        assert_eq!(builtin_class_access(0), 0x0019);
    }
}

#[cfg(test)]
mod module_reader_tests {
    use super::{
        decode_metadata_type, decode_properties, parse_function, parse_type_alias,
        parse_type_facts, primary_erasure_bounds, read_kotlin_module, value_parameter_type,
        MetaCtx, ParsedValueParam,
    };
    use crate::metadata::module::build_kotlin_module;
    use crate::types::Ty;
    use std::collections::HashMap;

    /// The decoded contract of a stdlib function, or `None` when the stdlib jar is not
    /// provisioned in this environment (the test then vacuously passes).
    fn stdlib_contract(facade: &str, name: &str) -> Option<crate::contracts::Contract> {
        let cp = crate::toolchain::stdlib_classpath();
        cp.meta_functions_name(crate::types::type_name(facade))
            .iter()
            .find(|f| f.kotlin_name == name)
            .and_then(|f| f.contract.as_deref().cloned())
    }

    #[test]
    fn decodes_stdlib_is_null_or_blank_contract() {
        use crate::contracts::{Condition, Effect, ParamRef, ReturnsValue};
        let Some(c) = stdlib_contract("kotlin/text/StringsKt", "isNullOrBlank") else {
            return;
        };
        assert_eq!(
            c.effects,
            vec![Effect::ConditionalReturns {
                returns: ReturnsValue::Bool(false),
                conclusion: Condition::IsNull {
                    param: ParamRef::Receiver,
                    negated: true,
                },
            }]
        );
    }

    #[test]
    fn decodes_stdlib_require_and_require_not_null_contracts() {
        use crate::contracts::{Condition, Effect, ParamRef, ReturnsValue};
        let Some(req) = stdlib_contract("kotlin/PreconditionsKt", "require") else {
            return;
        };
        assert!(
            req.effects.contains(&Effect::ConditionalReturns {
                returns: ReturnsValue::Any,
                conclusion: Condition::BoolParam(ParamRef::Param(0)),
            }),
            "require contract effects: {:?}",
            req.effects
        );
        let Some(rnn) = stdlib_contract("kotlin/PreconditionsKt", "requireNotNull") else {
            return;
        };
        assert!(
            rnn.effects.contains(&Effect::ConditionalReturns {
                returns: ReturnsValue::Any,
                conclusion: Condition::IsNull {
                    param: ParamRef::Param(0),
                    negated: true,
                },
            }),
            "requireNotNull contract effects: {:?}",
            rnn.effects
        );
    }

    #[test]
    fn decodes_stdlib_run_calls_in_place_contract() {
        use crate::contracts::{Effect, InvocationKind, ParamRef};
        let Some(c) = stdlib_contract("kotlin/StandardKt", "run") else {
            return;
        };
        assert!(
            c.effects.contains(&Effect::CallsInPlace {
                param: ParamRef::Param(0),
                kind: InvocationKind::ExactlyOnce,
            }),
            "run contract effects: {:?}",
            c.effects
        );
    }

    #[test]
    fn function_operator_flag_uses_kotlin_metadata_bit_eight() {
        // Function.flags is field 9 (tag 0x48). The protobuf default public/final flags are 6;
        // adding IS_OPERATOR (1 << 8) yields 262, encoded as the varint 0x86 0x02.
        let operator = parse_function(&[0x48, 0x86, 0x02]).expect("function message");
        let ordinary = parse_function(&[]).expect("default function message");
        assert!(operator.is_operator);
        assert!(!ordinary.is_operator);
    }

    #[test]
    fn value_parameter_type_facts_share_inline_and_table_backed_decoding() {
        // Type.flags = SUSPEND_TYPE followed by Type.nullable = true. Keeping both fields in one
        // miniature message pins the shared wire walk rather than testing two helpers that could
        // drift independently.
        let declared_type = vec![0x08, 0x01, 0x18, 0x01];
        assert_eq!(
            parse_type_facts(&declared_type),
            super::ParsedTypeFacts {
                nullable: true,
                suspend_fun: true,
            }
        );

        let parameter = |type_body, type_id| ParsedValueParam {
            name_id: 0,
            has_default: false,
            materialized: false,
            type_body,
            type_id,
            vararg_elem_body: None,
            vararg_elem_id: None,
        };
        let inline = parameter(Some(declared_type.clone()), None);
        assert_eq!(
            value_parameter_type(&inline, None),
            Some((declared_type.as_slice(), false))
        );

        // TypeTable.type[0] = declared_type and firstNullable = 0. A table-backed parameter must
        // recover the identical Type body plus the table-level nullability fact; this is the form
        // produced when metadata enables type-table compaction.
        let mut table = vec![0x0a, declared_type.len() as u8];
        table.extend_from_slice(&declared_type);
        table.extend_from_slice(&[0x10, 0x00]);
        let indexed = parameter(None, Some(0));
        assert_eq!(
            value_parameter_type(&indexed, Some(&table)),
            Some((declared_type.as_slice(), true))
        );

        // Function.valueParameter { name = 0, typeId = 0 }. `type_id` is field 5; field 4 is
        // exclusively the inline vararg-element type.
        let function = parse_function(&[0x32, 0x04, 0x10, 0x00, 0x28, 0x00])
            .expect("function carrying one table-backed value parameter");
        assert_eq!(function.value_params.len(), 1);
        assert_eq!(function.value_params[0].type_id, Some(0));
        assert!(function.value_params[0].type_body.is_none());
    }

    #[test]
    fn function_signature_decodes_every_table_backed_type_reference() {
        // fun String.f(x: Int): Int, with receiver/parameter/return all represented by TypeTable ids.
        let function = [
            0x10, 0x00, // name = d2[0]
            0x38, 0x01, // return_type_id = 1
            0x40, 0x00, // receiver_type_id = 0
            0x32, 0x04, 0x10, 0x03, 0x28, 0x01, // value_parameter(name=3,type_id=1)
        ];
        let message = [
            0x1a, 0x0c, // Package.function
            0x10, 0x00, 0x38, 0x01, 0x40, 0x00, 0x32, 0x04, 0x10, 0x03, 0x28, 0x01, 0xf2, 0x01,
            0x08, // Package.type_table
            0x0a, 0x02, 0x30, 0x01, // type[0] = String
            0x0a, 0x02, 0x30, 0x02, // type[1] = Int
        ];
        assert_eq!(function.len(), 12);
        let d2 = vec![
            "f".to_string(),
            "kotlin/String".to_string(),
            "kotlin/Int".to_string(),
            "x".to_string(),
        ];
        let ctx = MetaCtx {
            msg: &message,
            records: &[],
            d2: &d2,
        };
        let decoded = super::decode_functions(&ctx, 3, &[], &[]);
        assert_eq!(decoded.len(), 1);
        let function = &decoded[0];
        assert!(function.is_extension());
        assert_eq!(function.receiver_class, Ty::String.obj_internal());
        assert_eq!(function.ret_class, Ty::Int.obj_internal());
        assert_eq!(function.value_params[0].ty, Ty::Int.obj_internal());
        let signature = function.generic_sig.as_ref().expect("semantic signature");
        assert_eq!(signature.receiver, Some(Ty::String));
        assert_eq!(signature.params, vec![Ty::Int]);
        assert_eq!(signature.ret, Ty::Int);
    }

    #[test]
    fn value_parameter_vararg_element_id_is_field_six() {
        let function = parse_function(&[
            0x32, 0x06, // value_parameter
            0x10, 0x00, // name
            0x28, 0x00, // type_id
            0x30, 0x01, // vararg_element_type_id
        ])
        .expect("function carrying a table-backed vararg");
        let parameter = &function.value_params[0];
        assert_eq!(parameter.type_id, Some(0));
        assert_eq!(parameter.vararg_elem_id, Some(1));
    }

    #[test]
    fn flexible_upper_bound_id_decodes_platform_nullability_not_a_type_parameter() {
        // Lower bound String plus flexible_upper_bound_id=0. The table entry is nullable String.
        let lower = [0x30, 0x00, 0x40, 0x00];
        let table = [
            0x0a, 0x04, 0x30, 0x00, 0x18, 0x01, // type[0] = String?
        ];
        assert_eq!(
            super::decode_metadata_type(
                &lower,
                Some(&table),
                &[],
                &["kotlin/String".to_string()],
                &HashMap::new(),
                &HashMap::new(),
                false,
                0,
            ),
            Some(Ty::platform_nullable(Ty::String))
        );
    }

    #[test]
    fn metadata_type_preserves_nullable_type_parameters() {
        let type_parameter = [0x38, 0x00, 0x18, 0x01];
        let parameters = HashMap::from([(0, "T".to_string())]);

        assert_eq!(
            decode_metadata_type(
                &type_parameter,
                None,
                &[],
                &[],
                &parameters,
                &HashMap::new(),
                false,
                0,
            ),
            Some(Ty::nullable(Ty::ty_param(
                "T",
                Ty::nullable(Ty::obj("kotlin/Any"))
            )))
        );
    }

    #[test]
    fn receiver_function_mark_is_independent_of_type_annotation_order() {
        // Type.class_name = d2[0] (`Function1`), followed by its receiver and return type arguments.
        // Type.annotation is extension field 100 (tag varint `a2 06`); each nested Annotation stores its
        // class-name id in field 1. Two copies exercise the repeated-field contract in both orders: an
        // unrelated annotation after `ExtensionFunctionType` must not overwrite the receiver marker.
        let prefix = [
            0x30, 0x00, // Function1
            0x12, 0x04, 0x12, 0x02, 0x30, 0x01, // argument[0] = String receiver
            0x12, 0x04, 0x12, 0x02, 0x30, 0x02, // argument[1] = Unit return
        ];
        let extension_annotation = [0xa2, 0x06, 0x02, 0x08, 0x03];
        let unrelated_annotation = [0xa2, 0x06, 0x02, 0x08, 0x04];
        let d2 = [
            "kotlin/Function1".to_string(),
            "kotlin/String".to_string(),
            "kotlin/Unit".to_string(),
            "kotlin/ExtensionFunctionType".to_string(),
            "sample/TypeUseMarker".to_string(),
        ];
        let expected = Ty::fun_with_shape(vec![Ty::String], Ty::Unit, 0, true, false);
        for annotations in [
            [extension_annotation, unrelated_annotation],
            [unrelated_annotation, extension_annotation],
        ] {
            let body = prefix
                .into_iter()
                .chain(annotations.into_iter().flatten())
                .collect::<Vec<_>>();
            assert_eq!(
                decode_metadata_type(
                    &body,
                    None,
                    &[],
                    &d2,
                    &HashMap::new(),
                    &HashMap::new(),
                    false,
                    0,
                ),
                Some(expected)
            );
        }
    }

    #[test]
    fn extension_receiver_signature_preserves_top_level_nullability() {
        let nullable_string = [0x30, 0x00, 0x18, 0x01];
        assert_eq!(
            decode_metadata_type(
                &nullable_string,
                None,
                &[],
                &["kotlin/String".to_string()],
                &HashMap::new(),
                &HashMap::new(),
                false,
                0,
            ),
            Some(Ty::nullable(Ty::String))
        );
    }

    #[test]
    fn metadata_type_arguments_preserve_every_projection() {
        let names = ["sample/Box".to_string(), "kotlin/String".to_string()];
        let decode = |projection| {
            let argument = if projection == 3 {
                vec![0x08, projection]
            } else {
                vec![0x08, projection, 0x12, 0x02, 0x30, 0x01]
            };
            let body = [0x30, 0x00, 0x12, argument.len() as u8]
                .into_iter()
                .chain(argument)
                .collect::<Vec<_>>();
            decode_metadata_type(
                &body,
                None,
                &[],
                &names,
                &HashMap::new(),
                &HashMap::new(),
                false,
                0,
            )
        };
        assert_eq!(
            decode(0),
            Some(Ty::obj_args("sample/Box", &[Ty::in_projection(Ty::String)]))
        );
        assert_eq!(
            decode(1),
            Some(Ty::obj_args(
                "sample/Box",
                &[Ty::out_projection(Ty::String)]
            ))
        );
        assert_eq!(decode(2), Some(Ty::obj_args("sample/Box", &[Ty::String])));
        assert_eq!(
            decode(3),
            Some(Ty::obj_args(
                "sample/Box",
                &[Ty::out_projection(Ty::nullable(Ty::obj("kotlin/Any")))]
            ))
        );
    }

    #[test]
    fn type_alias_visibility_uses_the_public_default() {
        let d2 = vec!["Alias".to_string(), "sample/Real".to_string()];
        let omitted_flags = [0x10, 0x00, 0x32, 0x02, 0x30, 0x01];
        let internal_flags = [0x08, 0x00, 0x10, 0x00, 0x32, 0x02, 0x30, 0x01];

        let public = parse_type_alias(&omitted_flags, &[], &d2).expect("public alias decodes");
        assert_eq!(public.name, "Alias");
        assert_eq!(public.target, "sample/Real");
        // A bare target remains an explicit expansion rather than becoming a consumer fallback.
        assert!(public.formals.is_empty());
        assert_eq!(public.expansion, Ty::obj("sample/Real"));
        assert!(parse_type_alias(&internal_flags, &[], &d2).is_none());
    }

    #[test]
    fn primary_erasure_follows_dependent_bounds_and_stops_cycles() {
        let any = Ty::obj("kotlin/Any");
        let char_sequence = Ty::obj("kotlin/CharSequence");
        let formals = vec!["T".to_string(), "U".to_string()];
        let bounds = vec![
            vec![Ty::ty_param("U", any)],
            vec![char_sequence, Ty::obj("java/io/Serializable")],
        ];
        let resolved = primary_erasure_bounds(&formals, &bounds);
        assert_eq!(resolved["T"], char_sequence);
        assert_eq!(resolved["U"], char_sequence);

        let cyclic = vec![vec![Ty::ty_param("U", any)], vec![Ty::ty_param("T", any)]];
        let resolved = primary_erasure_bounds(&formals, &cyclic);
        assert_eq!(resolved["T"], any);
        assert_eq!(resolved["U"], any);
    }

    #[test]
    fn round_trips_package_facades() {
        let bytes = build_kotlin_module(&[
            ("kotlin.collections".into(), vec!["CollectionsKt".into()]),
            ("demo".into(), vec!["Lib1Kt".into(), "Lib2Kt".into()]),
        ]);
        let got = read_kotlin_module(&bytes);
        assert!(got.contains(&(
            "kotlin/collections".to_string(),
            vec!["kotlin/collections/CollectionsKt".to_string()]
        )));
        assert!(got.contains(&(
            "demo".to_string(),
            vec!["demo/Lib1Kt".to_string(), "demo/Lib2Kt".to_string()]
        )));
    }

    #[test]
    fn empty_or_short_input_is_empty() {
        assert!(read_kotlin_module(&[]).is_empty());
        assert!(read_kotlin_module(&[0u8; 8]).is_empty());
    }

    #[test]
    fn property_return_type_id_honors_first_nullable() {
        // Package.property (field 4): name=d2[0], returnTypeId=0.
        // Package.typeTable (field 30): Type(className=d2[1]), firstNullable=0.
        let msg = [
            0x22, 0x04, 0x10, 0x00, 0x48, 0x00, 0xf2, 0x01, 0x06, 0x0a, 0x02, 0x30, 0x01, 0x10,
            0x00,
        ];
        let d2 = vec!["maybe".to_string(), "kotlin/String".to_string()];
        let ctx = MetaCtx {
            msg: &msg,
            records: &[],
            d2: &d2,
        };

        let properties = decode_properties(&ctx, 4, &[], &[]);

        assert_eq!(properties.len(), 1);
        assert_eq!(properties[0].name, "maybe");
        assert_eq!(
            properties[0].ret_class.map(|name| name.render()),
            Some("kotlin/String".to_string())
        );
        assert!(properties[0].ret_nullable);
        assert_eq!(properties[0].visibility, crate::types::Visibility::Public);
    }

    #[test]
    fn property_omitted_jvm_signatures_materialize_default_accessors() {
        // Kotlin omits JvmPropertySignature when both accessor names and descriptors are the
        // metadata-derived defaults. A missing extension message therefore means "default JVM
        // realization", not "no accessor". Keep this at the protobuf boundary: the symbol source
        // must receive a complete declaration and must not reconstruct it later from a source name.
        let msg = [
            0x22, 0x0a, // Package.property, ten-byte read-only extension property
            0x10, 0x00, // Property.name = d2[0] (length)
            0x1a, 0x02, 0x30, 0x02, // returnType.className = d2[2] (Int)
            0x2a, 0x02, 0x30, 0x03, // receiverType.className = d2[3] (String)
            0x22, 0x0d, // Package.property, thirteen-byte mutable extension property
            0x10, 0x01, // Property.name = d2[1] (label)
            0x1a, 0x02, 0x30, 0x03, // returnType.className = d2[3] (String)
            0x2a, 0x02, 0x30, 0x03, // receiverType.className = d2[3] (String)
            0x58, 0x86, 0x0e, // Property.flags = public + isVar + hasSetter
        ];
        let d2 = vec![
            "length".to_string(),
            "label".to_string(),
            "kotlin/Int".to_string(),
            "kotlin/String".to_string(),
        ];
        let ctx = MetaCtx {
            msg: &msg,
            records: &[],
            d2: &d2,
        };

        let properties = decode_properties(&ctx, 4, &[], &[]);

        assert_eq!(properties.len(), 2);
        let length = &properties[0];
        assert_eq!(
            length.getter.as_ref().map(|it| it.name.as_str()),
            Some("getLength")
        );
        assert_eq!(
            length.getter.as_ref().map(|it| it.desc.as_str()),
            Some("(Ljava/lang/String;)I")
        );
        assert!(length.setter.is_none());

        let label = &properties[1];
        assert_eq!(
            label.getter.as_ref().map(|it| it.name.as_str()),
            Some("getLabel")
        );
        assert_eq!(
            label.getter.as_ref().map(|it| it.desc.as_str()),
            Some("(Ljava/lang/String;)Ljava/lang/String;")
        );
        assert_eq!(
            label.setter.as_ref().map(|it| it.name.as_str()),
            Some("setLabel")
        );
        assert_eq!(
            label.setter.as_ref().map(|it| it.desc.as_str()),
            Some("(Ljava/lang/String;Ljava/lang/String;)V")
        );
    }

    #[test]
    fn property_flags_honor_modern_and_legacy_schema_layouts() {
        // A Package containing five minimal Property messages. `ordinary` omits flags, which Kotlin's
        // schema defines as 518 (public/final/default getter). `moduleOnly` writes field 11 as 512:
        // the same default word with VISIBILITY bits 1..3 cleared to INTERNAL. The last two declarations
        // isolate property-specific IS_VAR (bit 8) and IS_CONST (bit 11); both sit past MEMBER_KIND and
        // therefore catch the former function-style offsets too. Keeping these assertions at the protobuf
        // boundary prevents a JVM accessor—or a later resolver policy—from masking a layout regression.
        // `legacy` exercises old_flags field 1 with that layout's earlier IS_VAR/IS_CONST positions.
        let msg = [
            0x22, 0x06, // Package.property, six-byte public property body
            0x10, 0x00, // Property.name = d2[0]
            0x1a, 0x02, 0x30, 0x05, // returnType.className = d2[5]
            0x22, 0x09, // Package.property, nine-byte internal property body
            0x10, 0x01, // Property.name = d2[1]
            0x1a, 0x02, 0x30, 0x05, // returnType.className = d2[5]
            0x58, 0x80, 0x04, // Property.flags (field 11) = 512
            0x22, 0x09, // Package.property, nine-byte mutable property body
            0x10, 0x02, // Property.name = d2[2]
            0x1a, 0x02, 0x30, 0x05, // returnType.className = d2[5]
            0x58, 0x86, 0x0e, // Property.flags = 1798 (default + isVar + hasSetter)
            0x22, 0x09, // Package.property, nine-byte const property body
            0x10, 0x03, // Property.name = d2[3]
            0x1a, 0x02, 0x30, 0x05, // returnType.className = d2[5]
            0x58, 0x86, 0x14, // Property.flags = 2566 (default + isConst)
            0x22, 0x09, // Package.property, nine-byte legacy property body
            0x08, 0xc0,
            0x04, // Property.old_flags (field 1) = 576 (internal + isVar + isConst)
            0x10, 0x04, // Property.name = d2[4]
            0x1a, 0x02, 0x30, 0x05, // returnType.className = d2[5]
        ];
        let d2 = vec![
            "ordinary".to_string(),
            "moduleOnly".to_string(),
            "mutable".to_string(),
            "constant".to_string(),
            "legacy".to_string(),
            "kotlin/Int".to_string(),
        ];
        let ctx = MetaCtx {
            msg: &msg,
            records: &[],
            d2: &d2,
        };

        let properties = decode_properties(&ctx, 4, &[], &[]);

        assert_eq!(properties.len(), 5);
        assert_eq!(properties[0].visibility, crate::types::Visibility::Public);
        assert_eq!(properties[1].visibility, crate::types::Visibility::Internal);
        assert!(properties[2].is_var);
        assert!(!properties[2].is_const);
        assert!(!properties[3].is_var);
        assert!(properties[3].is_const);
        assert_eq!(properties[4].visibility, crate::types::Visibility::Internal);
        assert!(properties[4].is_var);
        assert!(properties[4].is_const);
    }
}
