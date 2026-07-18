//! Minimal Kotlin `@Metadata` reader: decode the `d1` protobuf and report which functions are
//! `inline`, by their JVM `(name, descriptor)`. This is the complete inline-recognition the inliner
//! needs (the body `reifiedOperationMarker` scan only finds *reified* inline functions).
//!
//! Schema (kotlin `core/metadata/src/metadata.proto` + `metadata.jvm/.../jvm_metadata.proto`):
//!   Package.function = 3; Function.flags = 9 (IS_INLINE = bit 10); Function.name = 2;
//!   Function extension method_signature = 100 → JvmMethodSignature { name = 1, desc = 2 }.
//! String ids index the `d2` table.

use super::classreader::ClassInfo;
use crate::libraries::{CallSig, GenericSig, ParamList};
use crate::types::{intern, type_name, Ty, TypeName};
use std::collections::HashMap;

/// Decode a Kotlin `@Metadata` `Type` message into a signature [`Ty`] — the metadata-primary,
/// JVM-agnostic generic type. Kotlin generics come straight from `@Metadata` (the same source kotlinc
/// resolves against), NOT the JVM `Signature` attribute. `tparams` maps a `Type.type_parameter` id to its
/// name (built from the enclosing function's + class's `type_parameter` tables).
///
/// Proto (`ProtoBuf.Type`): `nullable`=3, `argument`=2 (repeated `Argument{projection=1, type=2}`),
/// `class_name`=6, `type_parameter`=8 (id), `type_parameter_name`=9 (string id). A `kotlin/FunctionN`
/// class becomes a [`Ty::Fun`] (its args are `[P1..Pn, R]`); a Kotlin primitive class collapses to its
/// dedicated [`Ty`] variant so it matches the rest of the pipeline. A type variable is a [`Ty::TyParam`]
/// (`kotlin/Any` bound). A `*`/unresolved argument erases to `Any`.
fn parse_type_gsig(
    body: &[u8],
    records: &[Rec],
    d2: &[String],
    tparams: &HashMap<u64, String>,
) -> Option<Ty> {
    let mut pb = Pb { b: body, i: 0 };
    let mut class_id = None;
    let mut tp_id = None;
    let mut tpn_id = None;
    let mut args: Vec<Ty> = Vec::new();
    while !pb.at_end() {
        let tag = pb.varint()?;
        match (tag >> 3, tag & 7) {
            (6, 0) => class_id = Some(pb.varint()?),
            (8, 0) => tp_id = Some(pb.varint()?),
            (9, 0) => tpn_id = Some(pb.varint()?),
            (2, 2) => {
                // Type.argument — `Argument.type` = field 2 (an inline `Type`); a `*` projection has none.
                let n = pb.varint()? as usize;
                let abody = pb.bytes(n)?;
                let mut ap = Pb { b: abody, i: 0 };
                let mut arg = None;
                while !ap.at_end() {
                    let at = ap.varint()?;
                    match (at >> 3, at & 7) {
                        (2, 2) => {
                            let tn = ap.varint()? as usize;
                            let tb = ap.bytes(tn)?;
                            arg = parse_type_gsig(tb, records, d2, tparams);
                        }
                        (_, w) => ap.skip(w)?,
                    }
                }
                args.push(arg.unwrap_or_else(|| Ty::obj("kotlin/Any")));
            }
            (_, w) => pb.skip(w)?,
        }
    }
    if let Some(id) = class_id {
        let internal = resolve_class_name(records, d2, id as usize)?;
        return Some(gsig_from_kotlin_class(&internal, args));
    }
    if let Some(id) = tp_id {
        return tparams
            .get(&id)
            .map(|n| Ty::ty_param(n, Ty::obj("kotlin/Any")));
    }
    if let Some(id) = tpn_id {
        return resolve_string(records, d2, id as usize)
            .map(|s| Ty::ty_param(&s, Ty::obj("kotlin/Any")));
    }
    None
}

/// A `@Metadata` class name + decoded type args → a signature [`Ty`]: a `kotlin/FunctionN` becomes a
/// [`Ty::Fun`] (args are `[P1..Pn, R]`), a Kotlin primitive collapses to its dedicated [`Ty`] variant (so
/// it matches a JVM-descriptor primitive downstream), everything else stays a [`Ty::Obj`].
fn gsig_from_kotlin_class(internal: &str, mut args: Vec<Ty>) -> Ty {
    if let Some(arity) = internal.strip_prefix("kotlin/Function") {
        if arity.parse::<u8>().is_ok() {
            let ret = args.pop().unwrap_or_else(|| Ty::obj("kotlin/Any"));
            return Ty::fun(args, ret);
        }
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

/// Parse a `TypeParameter` message → `(id, name string-id)`. Proto: `id`=1, `name`=2 (string-table id).
fn parse_type_param(body: &[u8]) -> Option<(u64, u64)> {
    let mut pb = Pb { b: body, i: 0 };
    let mut id = None;
    let mut name = None;
    while !pb.at_end() {
        let tag = pb.varint()?;
        match (tag >> 3, tag & 7) {
            (1, 0) => id = Some(pb.varint()?),
            (2, 0) => name = Some(pb.varint()?),
            (_, w) => pb.skip(w)?,
        }
    }
    Some((id?, name?))
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
const PREDEFINED_STRINGS: &[&str] = &[
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

/// Parse a `JvmMethodSignature` (extension body) → `(name string id, desc string id)`.
fn parse_jvm_signature(body: &[u8]) -> Option<(u64, u64)> {
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
    Some((name?, desc?))
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

/// For a function-type `Type` (`kotlin/FunctionN`), recover whether it is a RECEIVER function type
/// (`Recv.(…) -> R`) and the receiver's class id: returns `(annotation_id, first_argument_class_id)`,
/// where `annotation_id` is the `Type.annotation` (field 100) `Annotation.id` (which a caller checks
/// resolves to `kotlin/ExtensionFunctionType`) and the first `Type.argument` (field 1) carries the
/// receiver type. Either is `None` when absent.
fn parse_type_recv_fun(body: &[u8]) -> (Option<u64>, Option<u64>) {
    let mut pb = Pb { b: body, i: 0 };
    let mut anno_id = None;
    let mut arg0_class = None;
    let mut seen_arg = false;
    while !pb.at_end() {
        let Some(tag) = pb.varint() else { break };
        match (tag >> 3, tag & 7) {
            (2, 2) => {
                // Type.argument (repeated, field 2) — the FIRST argument is the receiver. `Argument.type` = 2.
                let Some(n) = pb.varint() else { break };
                let Some(abody) = pb.bytes(n as usize) else {
                    break;
                };
                if !seen_arg {
                    seen_arg = true;
                    let mut ap = Pb { b: abody, i: 0 };
                    while !ap.at_end() {
                        let Some(at) = ap.varint() else { break };
                        match (at >> 3, at & 7) {
                            (2, 2) => {
                                if let Some(tn) = ap.varint() {
                                    if let Some(tb) = ap.bytes(tn as usize) {
                                        arg0_class = parse_type_class_name(tb);
                                    }
                                }
                            }
                            (_, w) => {
                                if ap.skip(w).is_none() {
                                    break;
                                }
                            }
                        }
                    }
                }
            }
            (100, 2) => {
                // Type.annotation (extension) — `Annotation.id` = 1 (the annotation class id).
                let Some(n) = pb.varint() else { break };
                let Some(abody) = pb.bytes(n as usize) else {
                    break;
                };
                let mut ap = Pb { b: abody, i: 0 };
                while !ap.at_end() {
                    let Some(at) = ap.varint() else { break };
                    match (at >> 3, at & 7) {
                        (1, 0) => anno_id = ap.varint(),
                        (_, w) => {
                            if ap.skip(w).is_none() {
                                break;
                            }
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
    (anno_id, arg0_class)
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
    class_id: Option<u64>,
    name_id: u64,
    has_default: bool,
    materialized: bool,
    recv_fun: (Option<u64>, Option<u64>),
    /// The raw `ValueParameter.type` (field 3) `Type` message body — decoded to a signature [`Ty`] with the
    /// enclosing type-parameter table (needs `records`/`d2`, so it happens in `decode_functions`).
    type_body: Vec<u8>,
    /// The raw `ValueParameter.varargElementType` (field 5) `Type` body when the parameter is a `vararg`.
    /// Present ⇒ the parameter is a vararg whose LOGICAL gsig is `Array<elem>`; kotlinc stores the element
    /// type here (the JVM descriptor's array-ness lives only in `type`/the descriptor).
    vararg_elem_body: Option<Vec<u8>>,
}

/// A decoded `Function` message: whether it's `inline`, whether it's `suspend`, its name string id, its
/// explicit JVM `(name id, desc id)` signature (if present), and its return type's class_name id.
struct ParsedFunction {
    is_inline: bool,
    is_suspend: bool,
    /// `true` when the Kotlin `Visibility` in `flags` is `PUBLIC` — the metadata-truth visibility, which
    /// differs from the bytecode access flags for an `inline` function (private/synthetic in bytecode).
    is_public: bool,
    name_id: u64,
    jvm_sig: Option<(u64, u64)>,
    ret_class: Option<u64>,
    recv_class: Option<u64>,
    /// Whether `receiver_type` (field 5) was present — TRUE for an extension on a type PARAMETER
    /// (`fun <T> T.takeIf`), where `recv_class` is None. Distinguishes an extension from a top-level fn.
    has_receiver: bool,
    /// Whether the Kotlin return type is nullable (`T?`) — `Type.nullable = 3`. The JVM
    /// descriptor/`Signature` erase this; only `@Metadata` carries it. Drives the elvis null-check for a
    /// nullable-returning scope fn (`takeIf`/`takeUnless` return `T?`).
    ret_nullable: bool,
    /// SOURCE value parameters in declaration order. The COUNT is the source arity (excludes synthetic
    /// descriptor params); fields are resolved to names downstream.
    value_params: Vec<ParsedValueParam>,
    /// The function's own `type_parameter` table (field 4): `(id, name string-id)` — for resolving a
    /// `Type.type_parameter` reference in a parameter/return type to its name.
    type_params: Vec<(u64, u64)>,
    /// Raw `Function.return_type` (field 3) `Type` body, for the metadata generic signature.
    return_body: Option<Vec<u8>>,
    /// Raw `Function.receiver_type` (field 5) `Type` body (extensions only), for the metadata gsig.
    receiver_body: Option<Vec<u8>>,
    /// Raw `Annotation` message bodies on the function (`Function.annotation`, field 12) — decoded to
    /// `(class name, arguments)` downstream where the string table is available. Kotlin stores an
    /// annotation here when it has `BINARY`/`RUNTIME` retention (`@JvmName`, `@OverloadResolutionBy…`).
    annotation_bodies: Vec<Vec<u8>>,
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
    let mut flags = 6u64;
    let mut name_id = 0u64;
    let mut jvm_sig = None;
    let mut ret_class = None;
    let mut recv_class = None;
    let mut has_receiver = false;
    let mut ret_nullable = false;
    let mut value_params: Vec<ParsedValueParam> = Vec::new();
    let mut type_params: Vec<(u64, u64)> = Vec::new();
    let mut return_body: Option<Vec<u8>> = None;
    let mut receiver_body: Option<Vec<u8>> = None;
    let mut annotation_bodies: Vec<Vec<u8>> = Vec::new();
    while !pb.at_end() {
        let tag = pb.varint()?;
        match (tag >> 3, tag & 7) {
            (12, 2) => {
                // Function.annotation (repeated `Annotation`) — decoded downstream (needs the string table).
                let n = pb.varint()? as usize;
                annotation_bodies.push(pb.bytes(n)?.to_vec());
            }
            (9, 0) => flags = pb.varint()?,   // flags
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
                ret_class = parse_type_class_name(tbody);
                ret_nullable = parse_type_nullable(tbody);
                return_body = Some(tbody.to_vec());
            }
            (5, 2) => {
                // receiver_type (inline Type message) — PRESENCE marks an extension, even when the
                // receiver is a type parameter (`fun <T> T.takeIf`) whose `class_name` is absent.
                has_receiver = true;
                let n = pb.varint()? as usize;
                let tbody = pb.bytes(n)?;
                recv_class = parse_type_class_name(tbody);
                receiver_body = Some(tbody.to_vec());
            }
            (6, 2) => {
                // value_parameter (repeated `ValueParameter`) — the SOURCE value parameters. Their count
                // and types are the Kotlin signature, WITHOUT the synthetic params a codegen pass appends
                // to the JVM descriptor (a `suspend`'s `Continuation`, a `@Composable`'s `Composer`/`int`).
                // `ValueParameter.type = 3` is an inline `Type`; recover its `class_name` id.
                let n = pb.varint()? as usize;
                let vbody = pb.bytes(n)?;
                let mut vp = Pb { b: vbody, i: 0 };
                let mut tid = None;
                let mut nid = 0u64;
                let mut vflags = 0u64;
                let mut recv_ids = (None, None);
                let mut type_body = Vec::new();
                let mut vararg_elem_body = None;
                while !vp.at_end() {
                    let vt = vp.varint()?;
                    match (vt >> 3, vt & 7) {
                        (1, 0) => vflags = vp.varint()?, // ValueParameter.flags
                        (2, 0) => nid = vp.varint()?,    // ValueParameter.name (string-table id)
                        (3, 2) => {
                            let tn = vp.varint()? as usize;
                            let tb = vp.bytes(tn)?;
                            tid = parse_type_class_name(tb);
                            // A RECEIVER function-type param (`Recv.() -> R`) carries the
                            // `@ExtensionFunctionType` type annotation + the receiver as its first arg.
                            recv_ids = parse_type_recv_fun(tb);
                            type_body = tb.to_vec();
                        }
                        (5, 2) => {
                            // varargElementType — PRESENCE marks a `vararg`; body is the element `Type`.
                            let tn = vp.varint()? as usize;
                            vararg_elem_body = Some(vp.bytes(tn)?.to_vec());
                        }
                        (_, w) => vp.skip(w)?,
                    }
                }
                // `DECLARES_DEFAULT_VALUE` is bit 1 of the ValueParameter flags (HAS_ANNOTATIONS is bit 0).
                value_params.push(ParsedValueParam {
                    class_id: tid,
                    name_id: nid,
                    has_default: vflags & DECLARES_DEFAULT_VALUE_BIT != 0,
                    materialized: vflags & (IS_CROSSINLINE_BIT | IS_NOINLINE_BIT) != 0,
                    recv_fun: recv_ids,
                    type_body,
                    vararg_elem_body,
                });
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
    Some(ParsedFunction {
        is_inline: flags & IS_INLINE_BIT != 0,
        is_suspend: flags & IS_SUSPEND_BIT != 0,
        is_public: flags_visibility(flags) == VIS_PUBLIC,
        name_id,
        jvm_sig,
        ret_class,
        recv_class,
        has_receiver,
        ret_nullable,
        value_params,
        type_params,
        return_body,
        receiver_body,
        annotation_bodies,
    })
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

/// Whether a `Type` message is nullable (`Type.nullable = 3`, a varint bool). The JVM signature erases
/// Kotlin nullability; only `@Metadata` carries it.
fn parse_type_nullable(body: &[u8]) -> bool {
    let mut pb = Pb { b: body, i: 0 };
    while !pb.at_end() {
        let Some(tag) = pb.varint() else { break };
        match (tag >> 3, tag & 7) {
            (3, 0) => return pb.varint().is_some_and(|v| v != 0), // Type.nullable
            (_, w) => {
                if pb.skip(w).is_none() {
                    break;
                }
            }
        }
    }
    false
}

/// Build the metadata-primary [`GenericSig`] for a function: `formals` = the function's + enclosing
/// class's type-parameter names; `receiver` = the EXTENSION's `receiver_type`, or — for a member — the
/// declaring class parameterized by its own type parameters (`Box<T>`), or `None` for a top-level
/// function; `params` = the source VALUE parameters (no receiver, no synthetic `suspend` Continuation);
/// `ret` = the return type. Receiver is an ATTRIBUTE, uniform for member and extension: at the
/// checker/resolver level `class A { fun foo(): B }` and `A.foo(): B` are the same function on a receiver
/// `A`; that an extension emits the receiver as a leading JVM arg is only an emit detail. `None` only when
/// a receiver that WAS present fails to decode. `class_receiver` is `Some((declaring_class, class_tparams))`
/// for a member, `None` for an extension/top-level function.
fn build_generic_sig(
    pf: &ParsedFunction,
    records: &[Rec],
    d2: &[String],
    class_receiver: Option<(&str, &[(u64, String)])>,
) -> Option<GenericSig> {
    // id → name for every type parameter in scope (the enclosing class's, then the function's).
    let class_tparams = class_receiver.map(|(_, tps)| tps).unwrap_or(&[]);
    let mut tparams: HashMap<u64, String> = class_tparams.iter().cloned().collect();
    let mut formals: Vec<String> = class_tparams.iter().map(|(_, n)| n.clone()).collect();
    for (id, name_id) in &pf.type_params {
        if let Some(name) = resolve_string(records, d2, *name_id as usize) {
            tparams.insert(*id, name.clone());
            formals.push(name);
        }
    }
    let receiver = if let Some(rb) = &pf.receiver_body {
        // An EXTENSION: its `receiver_type` is the receiver gsig node (`T`, `Ch`, `List<T>`, …).
        Some(parse_type_gsig(rb, records, d2, &tparams)?)
    } else {
        // A MEMBER: the declaring class parameterized by its own type parameters, so unifying it with the
        // actual receiver binds `T` exactly like an extension. `None` for a top-level function.
        class_receiver.map(|(internal, ctps)| {
            Ty::obj_args(
                internal,
                &ctps
                    .iter()
                    .map(|(_, n)| Ty::ty_param(n, Ty::obj("kotlin/Any")))
                    .collect::<Vec<_>>(),
            )
        })
    };
    let params: Vec<Ty> = pf
        .value_params
        .iter()
        .map(|vp| {
            // A `vararg elem: T` param's LOGICAL type is `Array<T>` (the JVM descriptor's array-ness); its
            // element type is `varargElementType`, so wrap it in `Array` to match the JVM `Signature` shape.
            let decoded = if let Some(elem) = &vp.vararg_elem_body {
                parse_type_gsig(elem, records, d2, &tparams).map(Ty::array)
            } else {
                parse_type_gsig(&vp.type_body, records, d2, &tparams)
            };
            // An unresolvable param erases to a fresh unbound var (→ `Any` downstream).
            decoded.unwrap_or_else(|| Ty::ty_param("\u{0}", Ty::obj("kotlin/Any")))
        })
        .collect();
    let ret = pf
        .return_body
        .as_ref()
        .and_then(|rb| parse_type_gsig(rb, records, d2, &tparams))
        .unwrap_or_else(|| Ty::obj("kotlin/Any"));
    Some(GenericSig {
        formals,
        receiver,
        params,
        ret,
    })
}

#[derive(Clone, Debug)]
pub struct MetaValueParam {
    pub ty: Option<TypeName>,
    pub name: String,
    pub has_default: bool,
    pub materialized: bool,
    pub recv_fun: bool,
    pub recv_fun_receiver: Option<TypeName>,
}

/// A function decoded from a `Class`/`Package` `@Metadata` message — the *metadata-truth* signature
/// kotlinc resolves against (`JvmProtoBufUtil.getJvmMethodSignature`): the Kotlin name, the JVM method
/// name + descriptor (from the `method_signature` extension when present), Kotlin visibility/`inline`/
/// `suspend`, and the extension-receiver class. For an `inline` function the bytecode is `private`/
/// synthetic, so these flags differ from the access flags — metadata is primary, bytecode is fallback.
#[derive(Clone, Debug)]
pub struct MetaFn {
    pub kotlin_name: String,
    pub jvm_name: String,
    /// The JVM descriptor from the `method_signature` extension; `None` when metadata omits it (the
    /// caller may then fall back to a bytecode method of the same name, or compute it from proto types).
    pub jvm_desc: Option<&'static str>,
    pub is_public: bool,
    pub is_inline: bool,
    pub is_suspend: bool,
    /// Extension-receiver Kotlin class name (`kotlin/Result` for `Result.getOrThrow`), if any. `None` for a
    /// top-level fn AND for an extension on a type PARAMETER — use [`MetaFn::is_extension`] to disambiguate.
    pub receiver_class: Option<TypeName>,
    /// Whether this is an EXTENSION (has a receiver of any kind — class or type parameter) vs a true
    /// top-level function. Lets the classpath ext index avoid mis-indexing a top-level generic as an
    /// extension on its first parameter's type.
    pub is_extension: bool,
    /// The Kotlin return-type class name (`kotlin/UInt` for `UInt.coerceAtMost`), if it is a class type.
    pub ret_class: Option<TypeName>,
    /// Whether the Kotlin return type is nullable (`T?`) — `Type.nullable`. The JVM descriptor/`Signature`
    /// erase this; only `@Metadata` carries it. Drives the elvis null-check for a nullable-returning scope
    /// fn (`takeIf`/`takeUnless` return `T?`).
    pub ret_nullable: bool,
    /// SOURCE value parameters in declaration order. The LENGTH is the source arity: it excludes
    /// synthetic JVM descriptor params such as suspend `Continuation` or Compose `Composer`/masks.
    pub value_params: Vec<MetaValueParam>,
    /// The metadata-primary generic signature (type parameters + parameter/return gsig nodes), decoded
    /// straight from `@Metadata` rather than the JVM `Signature` attribute — a JVM-agnostic, Kotlin-faithful
    /// source (nullability, variance, Kotlin type identities). `None` when the return type won't decode.
    pub generic_sig: Option<GenericSig>,
}

impl MetaFn {
    pub fn member_call_sig(&self) -> CallSig {
        CallSig::metadata_member(
            self.value_params.len(),
            self.value_params.iter().map(|p| p.name.clone()).collect(),
            self.value_params.iter().map(|p| p.has_default).collect(),
        )
    }

    pub fn extension_call_sig(&self) -> CallSig {
        CallSig::metadata_extension(
            self.value_params.len() + 1,
            self.value_params.iter().map(|p| p.name.clone()).collect(),
            self.value_params.iter().map(|p| p.has_default).collect(),
        )
    }
}

/// A JVM method signature carried by Kotlin metadata: method name + descriptor as one fact.
#[derive(Clone, Debug)]
pub struct MetaJvmMethodSig {
    pub name: String,
    pub desc: String,
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
}

/// Decode every `Function` (proto field `fn_field`: 9 in a `Class`, 3 in a `Package`) of this class's
/// `@Metadata` message into [`MetaFn`]s. The single metadata-primary function reader.
fn decode_functions(ci: &ClassInfo, fn_field: u64) -> Vec<MetaFn> {
    let mut out = Vec::new();
    if ci.kotlin_d1.is_empty() {
        return out;
    }
    let bytes = decode_d1(&ci.kotlin_d1);
    let (st_body, msg_body) = split_d1(&bytes);
    let records = parse_string_table(st_body);
    let d2 = &ci.kotlin_d2;
    let mut pb = Pb { b: msg_body, i: 0 };
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
                    let (mut jvm_name, mut jvm_desc) = match pf.jvm_sig {
                        Some((nid, did)) => (
                            resolve_string(&records, d2, nid as usize)
                                .unwrap_or_else(|| kotlin_name.clone()),
                            resolve_string(&records, d2, did as usize),
                        ),
                        None => (kotlin_name.clone(), None),
                    };
                    // A `@kotlin.jvm.JvmName("...")` annotation is the AUTHORITATIVE bytecode name (kotlinc
                    // uses it for the emitted method) — e.g. each `@OverloadResolutionByLambdaReturnType`
                    // `sumOf` overload carries `@JvmName("sumOfInt")`/`@JvmName("sumOfLong")`. The
                    // `method_signature` extension may omit it, so read it from the annotation directly.
                    // Absent → the Kotlin name stands.
                    if let Some(n) = annotation_jvm_name(&pf.annotation_bodies, &records, d2) {
                        jvm_name = n;
                    }
                    // Metadata omits the JVM descriptor for a function whose signature isn't `@JvmName`-
                    // mangled (it would be computed from proto types). The bytecode is the fallback: if
                    // exactly one method of this JVM name exists, take its descriptor — covers `inline`
                    // value-class members (`Result.Companion.success`) erased to `(Object)Object`.
                    if jvm_desc.is_none() {
                        let mut same: Vec<&str> = ci
                            .methods
                            .iter()
                            .filter(|m| m.name == jvm_name)
                            .map(|m| m.descriptor.as_str())
                            .collect();
                        same.dedup();
                        if same.len() == 1 {
                            jvm_desc = Some(same[0].to_string());
                        }
                    }
                    let receiver_class = pf
                        .recv_class
                        .and_then(|id| resolve_class_name(&records, d2, id as usize));
                    let ret_class = pf
                        .ret_class
                        .and_then(|id| resolve_class_name(&records, d2, id as usize));
                    let value_params: Vec<MetaValueParam> = pf
                        .value_params
                        .iter()
                        .map(|p| {
                            let recv_fun_ty = p
                                .recv_fun
                                .0
                                .and_then(|id| resolve_class_name(&records, d2, id as usize))
                                .map(|name| type_name(&name));
                            let recv_fun = recv_fun_ty
                                .is_some_and(|name| name.matches("kotlin/ExtensionFunctionType"));
                            MetaValueParam {
                                ty: p
                                    .class_id
                                    .and_then(|id| resolve_class_name(&records, d2, id as usize))
                                    .map(|name| type_name(&name)),
                                // Param names are plain string-table entries (like the JVM name/desc), not class names.
                                name: resolve_string(&records, d2, p.name_id as usize)
                                    .unwrap_or_default(),
                                has_default: p.has_default,
                                materialized: p.materialized,
                                recv_fun,
                                recv_fun_receiver: if recv_fun {
                                    p.recv_fun
                                        .1
                                        .and_then(|id| {
                                            resolve_class_name(&records, d2, id as usize)
                                        })
                                        .map(|name| type_name(&name))
                                } else {
                                    None
                                },
                            }
                        })
                        .collect();
                    // The metadata-primary generic signature. For now the structure MATCHES the JVM
                    // `Signature`-derived gsig (extension: receiver at `params[0]`; member/top-level: value
                    // params only) so it is a drop-in replacement; the uniform member-receiver synthesis is
                    // a later step (`class_receiver = None` here keeps a member's params value-only).
                    let generic_sig = build_generic_sig(&pf, &records, d2, None);
                    out.push(MetaFn {
                        kotlin_name,
                        jvm_name,
                        jvm_desc: jvm_desc.map(|s| intern(&s)),
                        is_public: pf.is_public,
                        is_inline: pf.is_inline,
                        is_suspend: pf.is_suspend,
                        receiver_class: receiver_class.map(|s| type_name(&s)),
                        is_extension: pf.has_receiver,
                        ret_class: ret_class.map(|s| type_name(&s)),
                        ret_nullable: pf.ret_nullable,
                        value_params,
                        generic_sig,
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
pub fn class_functions(ci: &ClassInfo) -> Vec<MetaFn> {
    decode_functions(ci, 9)
}

/// Top-level / extension functions declared in a file facade's `Package` `@Metadata`.
pub fn package_functions(ci: &ClassInfo) -> Vec<MetaFn> {
    decode_functions(ci, 3)
}

/// Type aliases declared in a file facade's `Package` `@Metadata` (`typealias Alias = Real` →
/// `("Alias", "pkg/Real")`). Reads the `Package.typeAlias` entries (field 5) from the proto directly:
/// each alias's name (field 2, a string-table id) and its EXPANDED type (field 6, fully resolved to the
/// concrete class, so an alias chain collapses to the final class; falls back to the immediate
/// underlying type, field 4). This is robust where the older `d2` `$annotations` heuristic was not — a
/// file facade also carries annotated top-level properties whose `$annotations` markers that heuristic
/// would misread as aliases.
pub fn package_type_aliases(ci: &ClassInfo) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if ci.kotlin_d1.is_empty() {
        return out;
    }
    let bytes = decode_d1(&ci.kotlin_d1);
    let (st_body, msg_body) = split_d1(&bytes);
    let records = parse_string_table(st_body);
    let d2 = &ci.kotlin_d2;
    let mut pb = Pb { b: msg_body, i: 0 };
    while !pb.at_end() {
        let Some(tag) = pb.varint() else { break };
        match (tag >> 3, tag & 7) {
            // Package.typeAlias = 5 (length-delimited message).
            (5, 2) => {
                let Some(len) = pb.varint() else { break };
                let Some(body) = pb.bytes(len as usize) else {
                    break;
                };
                if let Some((name, internal)) = parse_type_alias(body, &records, d2) {
                    // Key the alias by its FULL internal name — its declaring package (the facade's) plus
                    // the alias's simple name — so `kotlin/collections/ArrayList` is distinct from any other
                    // package's `ArrayList`. `resolve_type` looks it up by that full name.
                    let this_class = ci.this_class();
                    let pkg = this_class.rsplit_once('/').map_or("", |(p, _)| p);
                    let full = if pkg.is_empty() {
                        name
                    } else {
                        format!("{pkg}/{name}")
                    };
                    out.push((full, internal));
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

/// Decode a `TypeAlias` message → `(alias name, expanded/underlying class internal name)`.
/// `TypeAlias.name` = 2 (string-table id), `underlyingType` = 4, `expandedType` = 6 (both `Type`).
fn parse_type_alias(body: &[u8], records: &[Rec], d2: &[String]) -> Option<(String, String)> {
    let mut pb = Pb { b: body, i: 0 };
    let mut name_id: Option<u64> = None;
    let mut expanded_class: Option<u64> = None;
    let mut underlying_class: Option<u64> = None;
    while !pb.at_end() {
        let tag = pb.varint()?;
        match (tag >> 3, tag & 7) {
            (2, 0) => name_id = pb.varint(),
            (4, 2) => {
                let len = pb.varint()? as usize;
                let tb = pb.bytes(len)?;
                underlying_class = parse_type_class_name(tb);
            }
            (6, 2) => {
                let len = pb.varint()? as usize;
                let tb = pb.bytes(len)?;
                expanded_class = parse_type_class_name(tb);
            }
            (_, w) => pb.skip(w)?,
        }
    }
    let name = d2.get(name_id? as usize).cloned()?;
    let class_id = expanded_class.or(underlying_class)?;
    let internal = resolve_class_name(records, d2, class_id as usize)?;
    Some((name, internal))
}

/// Constructor source parameter names/default flags from `Class` `@Metadata`, in declaration order.
pub fn class_constructor_params(ci: &ClassInfo) -> Vec<ParamList> {
    let mut out = Vec::new();
    if ci.kotlin_d1.is_empty() {
        return out;
    }
    let bytes = decode_d1(&ci.kotlin_d1);
    let (st_body, msg_body) = split_d1(&bytes);
    let records = parse_string_table(st_body);
    let d2 = &ci.kotlin_d2;
    let mut pb = Pb { b: msg_body, i: 0 };
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
                let mut names = Vec::new();
                let mut defaults = Vec::new();
                while !cp.at_end() {
                    let Some(ct) = cp.varint() else { break };
                    match (ct >> 3, ct & 7) {
                        (2, 2) => {
                            // Constructor.value_parameter (repeated ValueParameter)
                            let Some(vlen) = cp.varint() else { break };
                            let Some(vbody) = cp.bytes(vlen as usize) else {
                                break;
                            };
                            let mut vp = Pb { b: vbody, i: 0 };
                            let mut nid = 0u64;
                            let mut vflags = 0u64;
                            while !vp.at_end() {
                                let Some(vt) = vp.varint() else { break };
                                match (vt >> 3, vt & 7) {
                                    (1, 0) => vflags = vp.varint().unwrap_or(0), // ValueParameter.flags
                                    (2, 0) => nid = vp.varint().unwrap_or(0), // ValueParameter.name
                                    (_, w) => {
                                        if vp.skip(w).is_none() {
                                            break;
                                        }
                                    }
                                }
                            }
                            names.push(
                                resolve_string(&records, d2, nid as usize).unwrap_or_default(),
                            );
                            defaults.push(vflags & DECLARES_DEFAULT_VALUE_BIT != 0);
                        }
                        (_, w) => {
                            if cp.skip(w).is_none() {
                                break;
                            }
                        }
                    }
                }
                out.push(ParamList { names, defaults });
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
pub fn class_companion_name(ci: &ClassInfo) -> Option<String> {
    if ci.kotlin_d1.is_empty() {
        return None;
    }
    let bytes = decode_d1(&ci.kotlin_d1);
    let (st_body, msg_body) = split_d1(&bytes);
    let records = parse_string_table(st_body);
    let d2 = &ci.kotlin_d2;
    let mut pb = Pb { b: msg_body, i: 0 };
    while !pb.at_end() {
        let Some(tag) = pb.varint() else { break };
        match (tag >> 3, tag & 7) {
            (4, 0) => {
                let id = pb.varint()?;
                return resolve_class_name(&records, d2, id as usize);
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
pub fn class_sealed_subclasses(ci: &ClassInfo) -> Vec<String> {
    if ci.kotlin_d1.is_empty() {
        return Vec::new();
    }
    let bytes = decode_d1(&ci.kotlin_d1);
    let (st_body, msg_body) = split_d1(&bytes);
    let records = parse_string_table(st_body);
    let d2 = &ci.kotlin_d2;
    let mut out = Vec::new();
    let push_id = |id: usize, out: &mut Vec<String>| {
        if let Some(name) = resolve_class_name(&records, d2, id) {
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
    let mut pb = Pb { b: msg_body, i: 0 };
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
type JvmSig = Option<(u64, u64)>;

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

/// Member properties declared in a class's `@Metadata` (`Class.property` = field 10).
pub fn class_properties(ci: &ClassInfo) -> Vec<MetaProp> {
    decode_properties(ci, 10)
}

/// Top-level / extension properties declared in a file facade's `Package` `@Metadata`
/// (`Package.property` = field 4). An extension property carries a non-`None` `receiver_class`.
pub fn package_properties(ci: &ClassInfo) -> Vec<MetaProp> {
    decode_properties(ci, 4)
}

/// Decode every `Property` (`prop_field`: 10 in a `Class`, 4 in a `Package`) of this metadata message
/// into [`MetaProp`]s — the property analogue of [`decode_functions`]. Carries the REAL getter/setter
/// JVM names from the `JvmPropertySignature`, so a resolver reads the accessor instead of guessing `getX`.
fn decode_properties(ci: &ClassInfo, prop_field: u64) -> Vec<MetaProp> {
    let mut out = Vec::new();
    if ci.kotlin_d1.is_empty() {
        return out;
    }
    let bytes = decode_d1(&ci.kotlin_d1);
    let (st_body, msg_body) = split_d1(&bytes);
    let records = parse_string_table(st_body);
    let d2 = &ci.kotlin_d2;
    let mut types: Vec<&[u8]> = Vec::new();
    let mut props: Vec<&[u8]> = Vec::new();
    let mut pb = Pb { b: msg_body, i: 0 };
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
                if pb.skip(w).is_none() {
                    break;
                }
            }
        }
    }
    let type_of_id = |tid: u64| -> Option<TypeName> {
        let tb = types.get(tid as usize)?;
        let cn = parse_type_class_name(tb)?;
        resolve_class_name(&records, d2, cn as usize).map(|name| type_name(&name))
    };
    // `Property.flags`: HAS_ANNOTATIONS(0) · VISIBILITY(1..3) · MODALITY(4..5) · IS_VAR(6) ·
    // HAS_GETTER(7) · HAS_SETTER(8) · IS_CONST(9) · …
    const IS_VAR_BIT: u64 = 1 << 6;
    const IS_CONST_BIT: u64 = 1 << 9;
    for prop in props {
        let mut p = Pb { b: prop, i: 0 };
        let mut name_id = None;
        let mut ret = None;
        let mut flags = 0u64;
        let mut sig = (None, None);
        let mut receiver_class = None;
        while !p.at_end() {
            let Some(tag) = p.varint() else { break };
            match (tag >> 3, tag & 7) {
                (1, 0) => flags = p.varint().unwrap_or(0),
                (2, 0) => name_id = p.varint(),
                (3, 2) => {
                    let Some(n) = p.varint() else { break };
                    let Some(tb) = p.bytes(n as usize) else { break };
                    ret = parse_type_class_name(tb)
                        .and_then(|cn| resolve_class_name(&records, d2, cn as usize))
                        .map(|name| type_name(&name));
                }
                (9, 0) => ret = p.varint().and_then(type_of_id),
                // `Property.receiver_type` (field 5, inline `Type`) / `receiver_type_id` (field 10) —
                // PRESENCE marks an EXTENSION property; recover the receiver's class name.
                (5, 2) => {
                    let Some(n) = p.varint() else { break };
                    let Some(tb) = p.bytes(n as usize) else { break };
                    receiver_class = parse_type_class_name(tb)
                        .and_then(|cn| resolve_class_name(&records, d2, cn as usize))
                        .map(|name| type_name(&name));
                }
                (10, 0) => receiver_class = p.varint().and_then(type_of_id),
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
        let Some(name) = resolve_string(&records, d2, name_id as usize) else {
            continue;
        };
        let (getter, setter) = sig;
        let resolve_sig = |(nid, did): (u64, u64)| {
            Some(MetaJvmMethodSig {
                name: resolve_string(&records, d2, nid as usize)?,
                desc: resolve_string(&records, d2, did as usize)?,
            })
        };
        let is_var = setter.is_some() || flags & IS_VAR_BIT != 0;
        out.push(MetaProp {
            name,
            ret_class: ret,
            getter: getter.and_then(resolve_sig),
            setter: setter.and_then(resolve_sig),
            visibility: crate::types::Visibility::from_metadata(flags_visibility(flags)),
            is_const: flags & IS_CONST_BIT != 0,
            is_var,
            receiver_class,
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
pub fn class_inline(ci: &ClassInfo) -> Option<InlineClass> {
    if ci.kotlin_d1.is_empty() {
        return None;
    }
    let bytes = decode_d1(&ci.kotlin_d1);
    let (st_body, msg_body) = split_d1(&bytes);
    let records = parse_string_table(st_body);
    let d2 = &ci.kotlin_d2;
    let mut pb = Pb { b: msg_body, i: 0 };
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
                property_name = resolve_string(&records, d2, id as usize);
            }
            (18, 2) => {
                // inline_class_underlying_type (inline Type message)
                let n = pb.varint()? as usize;
                let tbody = pb.bytes(n)?;
                is_value = true;
                let (cls, nullable) = parse_type_class_and_nullable(tbody);
                underlying_class = cls.and_then(|id| resolve_class_name(&records, d2, id as usize));
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
                    cls.and_then(|cid| resolve_class_name(&records, d2, cid as usize));
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
            let mut pb = Pb { b: msg_body, i: 0 };
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
                        if resolve_string(&records, d2, nid as usize).as_deref() != Some(pname) {
                            continue;
                        }
                        if let Some(tbody) = rt {
                            let (cls, nullable) = parse_type_class_and_nullable(tbody);
                            underlying_class =
                                cls.and_then(|cid| resolve_class_name(&records, d2, cid as usize));
                            underlying_nullable = Some(nullable);
                        } else if let (Some(id), Some(tt)) = (rtid, type_table) {
                            if let Some((tbody, table_nullable)) = type_table_entry(tt, id as usize)
                            {
                                let (cls, own_nullable) = parse_type_class_and_nullable(tbody);
                                underlying_class = cls
                                    .and_then(|cid| resolve_class_name(&records, d2, cid as usize));
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

/// One member of a builtins `Class`: its Kotlin name, value-parameter type names, and return type name
/// — all Kotlin internal names (`kotlin/Int`, `kotlin/String`, …) resolved from the fragment's tables.
pub struct BuiltinMember {
    pub name: String,
    pub params: Vec<String>,
    pub ret: String,
    pub is_property: bool,
    /// Whether the declared return type is nullable (`V?`) — the JVM descriptor erases it, only the
    /// `.kotlin_builtins` `Type.nullable` flag carries it (`Map.get(K): V?`, `firstOrNull(): T?`).
    pub ret_nullable: bool,
}

/// A builtin `Class` decoded from a `.kotlin_builtins` fragment: its direct supertypes and declared
/// members — the two facets the front end needs (the read-only/mutable hierarchy AND each type's API).
#[derive(Default)]
pub struct BuiltinClass {
    pub supertypes: Vec<String>,
    pub members: Vec<BuiltinMember>,
    /// Whether the builtin is an interface (`List`, `CharSequence`, `Comparable`) vs a class (`Number`,
    /// `Enum`) — from the `@Metadata` `CLASS_KIND` flag. Needed when reporting a classless builtin whose
    /// JVM class is absent (a no-JDK compile), so member calls emit the right invoke opcode.
    pub is_interface: bool,
    /// Nullable returns for declared function members keyed by `(name, value-arity)`, INCLUDING
    /// members `members` drops because their return is a bare type parameter (`Map.get(K): V?`,
    /// `firstOrNull(): T?`). The resolved member for such a call is the erased classpath method (`java/util
    /// /Map.get` returns `Object`) which carries no Kotlin nullability — this is the only surviving record
    /// that the source return is `T?`. Consulted by the member walk to null-annotate that resolved return.
    pub nullable_member_returns: Vec<(String, usize)>,
}

/// Parse a `.kotlin_builtins` resource → every declared `Class` (qualified name → its supertypes +
/// members). ONE walk over the fragment's `StringTable`/`QualifiedNameTable`/`Class` tables; each
/// class's supertypes and member types are resolved through its `type_table` (field 30 → `Type
/// .class_name` → `QualifiedNameTable`). The single source for both the collection hierarchy and a
/// builtin type's API — no curated/hardcoded tables.
pub fn parse_builtins(data: &[u8]) -> std::collections::HashMap<String, BuiltinClass> {
    let mut out = std::collections::HashMap::new();
    let Some(pf) = strip_builtins_header(data) else {
        return out;
    };
    let mut strings: Vec<String> = Vec::new();
    let mut qnames: Vec<QName> = Vec::new();
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
        let mut flags = 0u64;
        let mut supids: Vec<u64> = Vec::new();
        let mut types: Vec<&[u8]> = Vec::new();
        let mut funcs: Vec<&[u8]> = Vec::new();
        let mut props: Vec<&[u8]> = Vec::new();
        while !cp.at_end() {
            let Some(tag) = cp.varint() else { break };
            match (tag >> 3, tag & 7) {
                // Class.flags = 1 (varint). `CLASS_KIND` occupies bits 6..8 (after HAS_ANNOTATIONS,
                // VISIBILITY[3], MODALITY[2]); 1 = INTERFACE.
                (1, 0) => flags = cp.varint().unwrap_or(0),
                (3, 0) => fq = cp.varint(),
                (2, 2) => {
                    // supertype_id (packed) — indexes the class's type_table.
                    if let Some(n) = cp.varint() {
                        if let Some(b) = cp.bytes(n as usize) {
                            supids.extend(packed_varints(b));
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
        // A `*_type_id` indexes the class `type_table`; resolve to the type's class_name → internal name.
        let type_of_id = |tid: u64| -> Option<String> {
            let tb = types.get(tid as usize)?;
            let cn = parse_type_class_name(tb)?;
            Some(resolve_qname(&qnames, &strings, cn as i64))
        };
        let supertypes: Vec<String> = supids.iter().filter_map(|&sid| type_of_id(sid)).collect();
        let mut members = Vec::new();
        let mut nullable_member_returns = Vec::new();
        for fb in &funcs {
            let mut p = Pb { b: fb, i: 0 };
            let mut name_id = None;
            let mut ret_id = None;
            let mut params = Vec::new();
            while !p.at_end() {
                let Some(tag) = p.varint() else { break };
                match (tag >> 3, tag & 7) {
                    (2, 0) => name_id = p.varint(), // name
                    (7, 0) => ret_id = p.varint(),  // return_type_id (type-table ref)
                    (6, 2) => {
                        // value_parameter: ValueParameter.type_id = 4 (type-table ref)
                        if let Some(n) = p.varint() {
                            if let Some(vb) = p.bytes(n as usize) {
                                let mut vp = Pb { b: vb, i: 0 };
                                let mut pty = None;
                                while !vp.at_end() {
                                    let Some(vt) = vp.varint() else { break };
                                    match (vt >> 3, vt & 7) {
                                        // ValueParameter.type_id (a type-table ref; field 5 in the
                                        // builtins schema, 4 in some) → the parameter's type.
                                        (5, 0) | (4, 0) => pty = vp.varint().and_then(type_of_id),
                                        (3, 2) => {
                                            // inline `type` Type → its class_name
                                            if let Some(n) = vp.varint() {
                                                if let Some(tb) = vp.bytes(n as usize) {
                                                    pty = parse_type_class_name(tb).map(|cn| {
                                                        resolve_qname(&qnames, &strings, cn as i64)
                                                    });
                                                }
                                            }
                                        }
                                        (_, w) => {
                                            if vp.skip(w).is_none() {
                                                break;
                                            }
                                        }
                                    }
                                }
                                params.push(pty.unwrap_or_default());
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
            if let (Some(ni), Some(ri)) = (name_id, ret_id) {
                // The return type's nullability (`Map.get(K): V?`) lives on the type-table entry's
                // `Type.nullable` flag — the JVM descriptor erases it.
                let ret_nullable = types
                    .get(ri as usize)
                    .is_some_and(|tb| parse_type_nullable(tb));
                // Record nullable returns even for type-parameter-return functions the member list drops
                // just below, so the erased classpath member can be null-annotated later.
                if let Some(name) = strings.get(ni as usize).filter(|_| ret_nullable) {
                    nullable_member_returns.push((name.clone(), params.len()));
                }
                if let Some((name, ret)) = strings.get(ni as usize).cloned().zip(type_of_id(ri)) {
                    members.push(BuiltinMember {
                        name,
                        params,
                        ret,
                        is_property: false,
                        ret_nullable,
                    });
                }
            }
        }
        for pb_ in &props {
            let mut p = Pb { b: pb_, i: 0 };
            let mut name_id = None;
            let mut ret_id = None;
            while !p.at_end() {
                let Some(tag) = p.varint() else { break };
                match (tag >> 3, tag & 7) {
                    (2, 0) => name_id = p.varint(),
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
            if let (Some(ni), Some(ri)) = (name_id, ret_id) {
                if let (Some(name), Some(ret)) = (strings.get(ni as usize).cloned(), type_of_id(ri))
                {
                    let ret_nullable = types
                        .get(ri as usize)
                        .is_some_and(|tb| parse_type_nullable(tb));
                    members.push(BuiltinMember {
                        name,
                        params: vec![],
                        ret,
                        is_property: true,
                        ret_nullable,
                    });
                }
            }
        }
        out.insert(
            fqname,
            BuiltinClass {
                supertypes,
                members,
                nullable_member_returns,
                is_interface: (flags >> 6) & 0x7 == 1,
            },
        );
    }
    out
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
mod module_reader_tests {
    use super::read_kotlin_module;
    use crate::metadata::module::build_kotlin_module;

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
}
