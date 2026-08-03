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
/// dedicated [`Ty`] variant so it matches the rest of the pipeline. An unbounded type variable is a
/// [`Ty::TyParam`] with Kotlin's implicit nullable `kotlin/Any?` upper bound. A `*`/unresolved argument
/// erases to `Any`.
fn parse_type_gsig(
    body: &[u8],
    records: &[Rec],
    d2: &[String],
    tparams: &HashMap<u64, String>,
) -> Option<Ty> {
    parse_type_gsig_bounded(body, records, d2, tparams, &HashMap::new())
}

fn parse_type_gsig_bounded(
    body: &[u8],
    records: &[Rec],
    d2: &[String],
    tparams: &HashMap<u64, String>,
    bounds: &HashMap<String, Ty>,
) -> Option<Ty> {
    parse_type_gsig_node(body, records, d2, tparams, bounds, false)
}

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

/// Decode an extension receiver with its top-level nullability.
#[cfg(test)]
fn parse_receiver_type_gsig(
    body: &[u8],
    records: &[Rec],
    d2: &[String],
    tparams: &HashMap<u64, String>,
) -> Option<Ty> {
    let ty = parse_type_gsig(body, records, d2, tparams)?;
    Some(if parse_type_nullable(body) {
        Ty::nullable(ty)
    } else {
        ty
    })
}

fn parse_receiver_type_gsig_bounded(
    body: &[u8],
    records: &[Rec],
    d2: &[String],
    tparams: &HashMap<u64, String>,
    bounds: &HashMap<String, Ty>,
) -> Option<Ty> {
    let ty = parse_type_gsig_bounded(body, records, d2, tparams, bounds)?;
    Some(if parse_type_nullable(body) {
        Ty::nullable(ty)
    } else {
        ty
    })
}

fn parse_type_gsig_node(
    body: &[u8],
    records: &[Rec],
    d2: &[String],
    tparams: &HashMap<u64, String>,
    bounds: &HashMap<String, Ty>,
    nested: bool,
) -> Option<Ty> {
    let mut pb = Pb { b: body, i: 0 };
    let mut class_id = None;
    let mut tp_id = None;
    let mut tpn_id = None;
    let mut nullable = false;
    let mut receiver_fun = false;
    let mut args: Vec<Ty> = Vec::new();
    while !pb.at_end() {
        let tag = pb.varint()?;
        match (tag >> 3, tag & 7) {
            (3, 0) => nullable = pb.varint()? != 0,
            (6, 0) => class_id = Some(pb.varint()?),
            (8, 0) => tp_id = Some(pb.varint()?),
            // `Type.type_parameter` = 7 per `metadata.proto` (the id of the type parameter);
            // field 8 above is `flexible_upper_bound_id`, historically also treated as one.
            (7, 0) => tp_id = Some(pb.varint()?),
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
                            arg = parse_type_gsig_node(tb, records, d2, tparams, bounds, true);
                        }
                        (_, w) => ap.skip(w)?,
                    }
                }
                args.push(arg.unwrap_or_else(|| Ty::obj("kotlin/Any")));
            }
            (100, 2) => {
                // Type.annotation (extension field 100) — `Annotation.id` = 1. A RECEIVER function type
                // (`Cfg.() -> Unit`) is a plain `kotlin/FunctionN` classifier carrying the
                // `@kotlin.ExtensionFunctionType` annotation; without it the decoded `Ty::Fun` would read
                // as an ordinary `(Cfg) -> Unit` and never match a receiver-lambda argument.
                let n = pb.varint()? as usize;
                let abody = pb.bytes(n)?;
                let mut ap = Pb { b: abody, i: 0 };
                let mut annotation_id = None;
                while !ap.at_end() {
                    let at = ap.varint()?;
                    match (at >> 3, at & 7) {
                        (1, 0) => annotation_id = ap.varint(),
                        (_, w) => ap.skip(w)?,
                    }
                }
                // `Type.annotation` is REPEATED. Receiver-ness is the presence of ONE semantic marker,
                // not a property of whichever annotation happened to be serialized last. Accumulate the
                // predicate while walking the field so adding an unrelated type-use annotation cannot
                // erase an earlier `@ExtensionFunctionType` mark (protobuf preserves no useful ordering
                // contract between independent annotations).
                receiver_fun |= annotation_id
                    .and_then(|id| resolve_class_name(records, d2, id as usize))
                    .is_some_and(|name| name == "kotlin/ExtensionFunctionType");
            }
            (_, w) => pb.skip(w)?,
        }
    }
    let ty = if let Some(id) = class_id {
        let internal = resolve_class_name(records, d2, id as usize)?;
        gsig_from_kotlin_class(&internal, args, receiver_fun)
    } else if let Some(id) = tp_id {
        tparams.get(&id).map(|n| {
            let bound = bounds
                .get(n)
                .copied()
                .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")));
            Ty::ty_param(n, bound)
        })?
    } else {
        let id = tpn_id?;
        resolve_string(records, d2, id as usize).map(|s| {
            let bound = bounds
                .get(&s)
                .copied()
                .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")));
            Ty::ty_param(&s, bound)
        })?
    };
    Some(if nested && nullable && matches!(ty, Ty::TyParam(..)) {
        Ty::nullable(ty)
    } else {
        ty
    })
}

/// A `@Metadata` class name + decoded type args → a signature [`Ty`]: a `kotlin/FunctionN` becomes a
/// [`Ty::Fun`] (args are `[P1..Pn, R]`), a Kotlin primitive collapses to its dedicated [`Ty`] variant (so
/// it matches a JVM-descriptor primitive downstream), everything else stays a [`Ty::Obj`].
///
/// `receiver_fun` is the type's `@kotlin.ExtensionFunctionType` mark: a receiver function type carries
/// its receiver as the FIRST type argument, which [`Ty::Fun`] models as the first parameter binding
/// `this` (`has_receiver`).
fn gsig_from_kotlin_class(internal: &str, mut args: Vec<Ty>, receiver_fun: bool) -> Ty {
    if let Some(arity) = internal.strip_prefix("kotlin/Function") {
        if arity.parse::<u8>().is_ok() {
            let ret = args.pop().unwrap_or_else(|| Ty::obj("kotlin/Any"));
            let has_receiver = receiver_fun && !args.is_empty();
            return Ty::fun_with_shape(args, ret, 0, has_receiver, false);
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

struct ParsedTypeParam {
    id: u64,
    name_id: u64,
    upper_bound_bodies: Vec<Vec<u8>>,
}

fn parse_type_param(body: &[u8]) -> Option<ParsedTypeParam> {
    let mut pb = Pb { b: body, i: 0 };
    let mut id = None;
    let mut name = None;
    let mut upper_bound_bodies = Vec::new();
    while !pb.at_end() {
        let tag = pb.varint()?;
        match (tag >> 3, tag & 7) {
            (1, 0) => id = Some(pb.varint()?),
            (2, 0) => name = Some(pb.varint()?),
            (5, 2) => {
                let n = pb.varint()? as usize;
                upper_bound_bodies.push(pb.bytes(n)?.to_vec());
            }
            (_, w) => pb.skip(w)?,
        }
    }
    Some(ParsedTypeParam {
        id: id?,
        name_id: name?,
        upper_bound_bodies,
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
/// `IS_OPERATOR` immediately follows the 2-bit member-kind field in Kotlin metadata's function flags.
const IS_OPERATOR_BIT: u64 = 1 << 8;

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
/// (`Recv.(…) -> R`) and the receiver's class id: returns `(annotation_ids, first_argument_class_id)`,
/// where `annotation_ids` contains EVERY repeated `Type.annotation` (field 100) `Annotation.id` (a caller
/// checks whether any resolves to `kotlin/ExtensionFunctionType`) and the first `Type.argument` (field 1)
/// carries the receiver type. The receiver id is `None` when absent.
fn parse_type_recv_fun(body: &[u8]) -> (Vec<u64>, Option<u64>) {
    let mut pb = Pb { b: body, i: 0 };
    let mut annotation_ids = Vec::new();
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
                        (1, 0) => {
                            if let Some(id) = ap.varint() {
                                // `Type.annotation` is repeated. Preserve the whole semantic set so a
                                // later, unrelated type-use annotation cannot overwrite an earlier
                                // receiver-function marker in this lightweight parameter decoder.
                                annotation_ids.push(id);
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
            (_, w) => {
                if pb.skip(w).is_none() {
                    break;
                }
            }
        }
    }
    (annotation_ids, arg0_class)
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
    recv_fun: (Vec<u64>, Option<u64>),
    /// The raw `ValueParameter.type` (field 3) `Type` message body — decoded to a signature [`Ty`] with the
    /// enclosing type-parameter table (needs `records`/`d2`, so it happens in `decode_functions`).
    type_body: Vec<u8>,
    /// The raw `ValueParameter.varargElementType` (field 4 as emitted by kotlin-stdlib 2.3.20) `Type`
    /// body when the parameter is a `vararg`.
    /// Present ⇒ the parameter is a vararg whose LOGICAL gsig is `Array<elem>`; kotlinc stores the element
    /// type here (the JVM descriptor's array-ness lives only in `type`/the descriptor).
    vararg_elem_body: Option<Vec<u8>>,
}

/// A decoded `Function` message: whether it's `inline`, whether it's `suspend`, its name string id, its
/// explicit JVM `(name id, desc id)` signature (if present), its operator flag, and its return type's
/// class_name id.
struct ParsedFunction {
    is_inline: bool,
    is_suspend: bool,
    is_operator: bool,
    visibility: crate::types::Visibility,
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
    type_params: Vec<ParsedTypeParam>,
    /// Raw `Function.return_type` (field 3) `Type` body, for the metadata generic signature.
    return_body: Option<Vec<u8>>,
    /// Raw `Function.receiver_type` (field 5) `Type` body (extensions only), for the metadata gsig.
    receiver_body: Option<Vec<u8>>,
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
    /// `Function.context_parameter` (field 13) entries — the LEADING context parameters
    /// (`context(a: A, b: B) fun f(…)`), excluded from the source value-parameter arity.
    /// Per entry: whether its type is nullable (drives context-source matching at call sites).
    context_params_nullable: Vec<bool>,
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
    let mut type_params: Vec<ParsedTypeParam> = Vec::new();
    let mut return_body: Option<Vec<u8>> = None;
    let mut receiver_body: Option<Vec<u8>> = None;
    let mut annotation_bodies: Vec<Vec<u8>> = Vec::new();
    let mut contract_body: Option<Vec<u8>> = None;
    let mut type_table_body: Option<Vec<u8>> = None;
    let mut context_params_nullable: Vec<bool> = Vec::new();
    while !pb.at_end() {
        let tag = pb.varint()?;
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
                // Function.context_parameter (repeated `ValueParameter`) — leading context
                // parameters, NOT part of the source arity. `ValueParameter.type` = 3 (inline
                // `Type`) carries the nullability.
                let n = pb.varint()? as usize;
                let vb = pb.bytes(n)?;
                let mut vp = Pb { b: vb, i: 0 };
                let mut nullable = false;
                while !vp.at_end() {
                    let vt = vp.varint()?;
                    match (vt >> 3, vt & 7) {
                        (3, 2) => {
                            let tn = vp.varint()? as usize;
                            nullable = parse_type_nullable(vp.bytes(tn)?);
                        }
                        (_, w) => vp.skip(w)?,
                    }
                }
                context_params_nullable.push(nullable);
            }
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
                let mut recv_ids = (Vec::new(), None);
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
                        (4, 2) => {
                            // varargElementType — PRESENCE marks a `vararg`; body is the element `Type`.
                            // Field FOUR, verified against kotlin-stdlib 2.3.20 by dumping every
                            // (field, wiretype) pair the value-param decoder sees: field 5 never occurs,
                            // and field 4 wire 2 occurs on exactly the vararg params (167 of ~10525).
                            // `type_id` also nominally lives at 4 but is a varint (wire 0), so a
                            // length-delimited 4 is unambiguous.
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
        is_operator: flags & IS_OPERATOR_BIT != 0,
        visibility: crate::types::Visibility::from_metadata(flags_visibility(flags)),
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
        contract_body,
        type_table_body,
        context_params_nullable,
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
        let mut ty = parse_type_gsig(&tb, records, d2, tparams)?;
        if table_nullable {
            ty = Ty::nullable(ty);
        }
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

struct TypeParameterContext {
    names: HashMap<u64, String>,
    formals: Vec<String>,
    formal_bounds: Vec<Vec<Ty>>,
    erasure_bounds: HashMap<String, Ty>,
}

fn type_parameter_context(
    inherited: &[(u64, String)],
    declared: &[ParsedTypeParam],
    records: &[Rec],
    d2: &[String],
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
    let mut formal_bounds = vec![Vec::new(); inherited.len()];
    formal_bounds.extend(resolved.into_iter().map(|parameter| {
        parameter
            .upper_bound_bodies
            .iter()
            .filter_map(|body| parse_type_gsig(body, records, d2, &names))
            .collect()
    }));
    let erasure_bounds = primary_erasure_bounds(&formals, &formal_bounds);
    Some(TypeParameterContext {
        names,
        formals,
        formal_bounds,
        erasure_bounds,
    })
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
    let class_tparams = class_receiver.map(|(_, tps)| tps).unwrap_or(&[]);
    let context = type_parameter_context(class_tparams, &pf.type_params, records, d2)?;
    let receiver = if let Some(rb) = &pf.receiver_body {
        // An EXTENSION: its `receiver_type` is the receiver gsig node (`T`, `Ch`, `List<T>`, …).
        Some(parse_receiver_type_gsig_bounded(
            rb,
            records,
            d2,
            &context.names,
            &context.erasure_bounds,
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
    let params: Vec<Ty> = pf
        .value_params
        .iter()
        .map(|vp| {
            // A `vararg elem: T` param's LOGICAL type is `Array<T>` (the JVM descriptor's array-ness); its
            // element type is `varargElementType`, so wrap it in `Array` to match the JVM `Signature` shape.
            let decoded = if let Some(elem) = &vp.vararg_elem_body {
                parse_type_gsig_bounded(elem, records, d2, &context.names, &context.erasure_bounds)
                    .map(Ty::array)
            } else {
                parse_type_gsig_bounded(
                    &vp.type_body,
                    records,
                    d2,
                    &context.names,
                    &context.erasure_bounds,
                )
            };
            // An unresolvable param erases to a fresh unbound var (→ `Any` downstream).
            decoded.unwrap_or_else(|| Ty::ty_param("\u{0}", Ty::nullable(Ty::obj("kotlin/Any"))))
        })
        .collect();
    let ret = pf
        .return_body
        .as_ref()
        .and_then(|rb| {
            parse_type_gsig_bounded(rb, records, d2, &context.names, &context.erasure_bounds)
        })
        .unwrap_or_else(|| Ty::obj("kotlin/Any"));
    Some(GenericSig {
        formals: context.formals,
        formal_bounds: context.formal_bounds,
        receiver,
        params,
        ret,
    })
}

fn build_property_generic_sig(
    type_params: &[ParsedTypeParam],
    return_body: Option<&[u8]>,
    return_nullable: bool,
    receiver_body: Option<&[u8]>,
    receiver_nullable: bool,
    records: &[Rec],
    d2: &[String],
) -> Option<GenericSig> {
    let context = type_parameter_context(&[], type_params, records, d2)?;
    let receiver = match receiver_body {
        Some(body) => {
            let receiver = parse_type_gsig_bounded(
                body,
                records,
                d2,
                &context.names,
                &context.erasure_bounds,
            )?;
            Some(if receiver_nullable {
                Ty::nullable(receiver)
            } else {
                receiver
            })
        }
        None => None,
    };
    let ret = parse_type_gsig_bounded(
        return_body?,
        records,
        d2,
        &context.names,
        &context.erasure_bounds,
    )?;
    Some(GenericSig {
        formals: context.formals,
        formal_bounds: context.formal_bounds,
        receiver,
        params: Vec::new(),
        ret: if return_nullable {
            Ty::nullable(ret)
        } else {
            ret
        },
    })
}

/// Bit-packed boolean flags for a [`MetaValueParam`], collapsing its `has_default`/`materialized`/
/// `vararg`/`recv_fun` bytes into one. Read through the `MetaValueParam` accessors of the same names;
/// built with the `with_*` chain. Headroom for four more flags before the byte fills.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MvpFlags(u8);

impl MvpFlags {
    const HAS_DEFAULT: u8 = 1 << 0;
    const MATERIALIZED: u8 = 1 << 1;
    const VARARG: u8 = 1 << 2;
    const RECV_FUN: u8 = 1 << 3;
    const NULLABLE: u8 = 1 << 4;

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
}

#[derive(Clone, Debug)]
pub struct MetaValueParam {
    pub ty: Option<TypeName>,
    pub name: String,
    /// Bit-packed `has_default`/`materialized`/`vararg`/`recv_fun` (read via the accessors below).
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
}

/// Bit-packed boolean flags for a [`MetaFn`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MfnFlags(u8);

impl MfnFlags {
    const IS_INLINE: u8 = 1 << 0;
    const IS_SUSPEND: u8 = 1 << 1;
    const IS_EXTENSION: u8 = 1 << 2;
    const RET_NULLABLE: u8 = 1 << 3;
    const IS_OPERATOR: u8 = 1 << 4;

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
    /// The metadata-primary generic signature (type parameters + parameter/return gsig nodes), decoded
    /// straight from `@Metadata` rather than the JVM `Signature` attribute — a JVM-agnostic, Kotlin-faithful
    /// source (nullability, variance, Kotlin type identities). `None` when the return type won't decode.
    pub generic_sig: Option<GenericSig>,
    /// The function's declared contract (`Function.contract`, field 32), decoded into the shared
    /// contract IR — the effects the checker applies at call sites (`returns(…) implies …`,
    /// `callsInPlace`). `None` when the function declares no contract.
    pub contract: Option<std::sync::Arc<crate::contracts::Contract>>,
    /// Number of LEADING context parameters (`context(a: A) fun f()`), excluded from the source
    /// arity — a caller supplies them implicitly from the enclosing context, not positionally.
    pub context_count: usize,
    /// Per context parameter: whether its declared type is nullable.
    pub context_params_nullable: Vec<bool>,
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
    pub fn ret_nullable(&self) -> bool {
        self.flags.has(MfnFlags::RET_NULLABLE)
    }

    pub fn member_call_sig(&self) -> CallSig {
        let (lambda_receivers, lambda_receiver_params) = self.lambda_receiver_shape();
        let mut sig = CallSig::metadata_function(
            self.value_params.len(),
            self.value_params.iter().map(|p| p.name.clone()).collect(),
            self.value_params.iter().map(|p| p.has_default()).collect(),
            lambda_receivers,
            lambda_receiver_params,
            self.value_params.iter().map(|p| p.materialized()).collect(),
            self.vararg_index(),
        );
        sig.platform_nullable_params = self.value_params.iter().map(|p| p.nullable()).collect();
        sig
    }

    /// Decode the semantic receiver-function shape once for every metadata function consumer.
    /// A concrete `Recv.() -> R` carries both the receiver type and the mark; a generic
    /// `T.() -> R` carries only the mark and recovers `T` after call-site substitution.
    pub(super) fn lambda_receiver_shape(&self) -> (Vec<Option<Ty>>, Vec<bool>) {
        (
            self.value_params
                .iter()
                .map(|p| p.recv_fun_receiver.map(crate::types::Ty::obj_name))
                .collect(),
            self.value_params.iter().map(|p| p.recv_fun()).collect(),
        )
    }

    pub fn vararg_index(&self) -> Option<usize> {
        self.value_params
            .iter()
            .position(|parameter| parameter.vararg())
    }

    pub fn extension_call_sig(&self) -> CallSig {
        let mut sig = CallSig::metadata_extension(
            self.value_params.len() + 1,
            self.value_params.iter().map(|p| p.name.clone()).collect(),
            self.value_params.iter().map(|p| p.has_default()).collect(),
            self.vararg_index(),
        );
        sig.platform_nullable_params = self.value_params.iter().map(|p| p.nullable()).collect();
        sig
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
    pub type_aliases: Vec<(String, String)>,
    /// `Class.constructor` (field 8): named-parameter lists in declaration order.
    pub constructor_params: Vec<ParamList>,
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
            class_functions: std::sync::Arc::from([]),
            package_functions: std::sync::Arc::from([]),
            class_properties: std::sync::Arc::from([]),
            package_properties: std::sync::Arc::from([]),
            type_aliases: Vec::new(),
            constructor_params: Vec::new(),
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
            && self.class_functions.is_empty()
            && self.package_functions.is_empty()
            && self.class_properties.is_empty()
            && self.package_properties.is_empty()
            && self.type_aliases.is_empty()
            && self.constructor_params.is_empty()
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
    /// The classfile's bytecode methods — the descriptor fallback when metadata omits a
    /// `method_signature` extension.
    methods: &'a [super::classreader::MethodSig],
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
        methods,
    };
    KotlinMeta {
        class_visibility: (k == Some(1)).then(|| class_visibility(&ctx)),
        class_functions: decode_functions(&ctx, 9).into(),
        package_functions: decode_functions(&ctx, 3).into(),
        class_properties: decode_properties(&ctx, 10).into(),
        package_properties: decode_properties(&ctx, 4).into(),
        type_aliases: type_aliases(&ctx, this_class),
        constructor_params: ctor_params(&ctx),
        companion_name: companion_name(&ctx),
        sealed_subclasses: sealed_subclasses(&ctx),
        inline: inline_class(&ctx),
        multifile_parts: Vec::new(),
    }
}

/// Kotlin `Class.flags` visibility. The protobuf default is PUBLIC FINAL (`6`), so an omitted flags
/// field must decode as public rather than internal.
fn class_visibility(ctx: &MetaCtx<'_>) -> crate::types::Visibility {
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
    crate::types::Visibility::from_metadata(flags_visibility(flags))
}

/// Decode every `Function` (proto field `fn_field`: 9 in a `Class`, 3 in a `Package`) of this class's
/// `@Metadata` message into [`MetaFn`]s. The single metadata-primary function reader.
fn decode_functions(ctx: &MetaCtx, fn_field: u64) -> Vec<MetaFn> {
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
                    let (mut jvm_name, mut jvm_desc) = match pf.jvm_sig {
                        Some((nid, did)) => (
                            resolve_string(records, d2, nid as usize)
                                .unwrap_or_else(|| kotlin_name.clone()),
                            resolve_string(records, d2, did as usize),
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
                    // Metadata omits the JVM descriptor for a function whose signature isn't `@JvmName`-
                    // mangled (it would be computed from proto types). The bytecode is the fallback: if
                    // exactly one method of this JVM name exists, take its descriptor — covers `inline`
                    // value-class members (`Result.Companion.success`) erased to `(Object)Object`.
                    if jvm_desc.is_none() {
                        let mut same: Vec<&str> = ctx
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
                        .and_then(|id| resolve_class_name(records, d2, id as usize));
                    let ret_class = pf
                        .ret_class
                        .and_then(|id| resolve_class_name(records, d2, id as usize));
                    let value_params: Vec<MetaValueParam> = pf
                        .value_params
                        .iter()
                        .map(|p| {
                            let recv_fun = p.recv_fun.0.iter().copied().any(|id| {
                                resolve_class_name(records, d2, id as usize)
                                    .is_some_and(|name| name == "kotlin/ExtensionFunctionType")
                            });
                            MetaValueParam {
                                ty: p
                                    .class_id
                                    .and_then(|id| resolve_class_name(records, d2, id as usize))
                                    .map(|name| type_name(&name)),
                                // Param names are plain string-table entries (like the JVM name/desc), not class names.
                                name: resolve_string(records, d2, p.name_id as usize)
                                    .unwrap_or_default(),
                                flags: MvpFlags::default()
                                    .with_has_default(p.has_default)
                                    .with_materialized(p.materialized)
                                    .with_vararg(p.vararg_elem_body.is_some())
                                    .with_recv_fun(recv_fun)
                                    .with_nullable(parse_type_nullable(&p.type_body)),
                                recv_fun_receiver: if recv_fun {
                                    p.recv_fun
                                        .1
                                        .and_then(|id| resolve_class_name(records, d2, id as usize))
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
                    let generic_sig = build_generic_sig(&pf, records, d2, None);
                    let contract = pf.contract_body.as_deref().and_then(|body| {
                        let tparams = type_parameter_context(&[], &pf.type_params, records, d2)
                            .map(|c| c.names)
                            .unwrap_or_default();
                        // Function-level table wins if present; the container's otherwise.
                        let table = pf.type_table_body.as_deref().or(type_table_body.as_deref());
                        decode_contract(body, records, d2, &tparams, table).map(std::sync::Arc::new)
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
                            .with_ret_nullable(pf.ret_nullable)
                            .with_is_operator(pf.is_operator),
                        receiver_class: receiver_class.map(|s| type_name(&s)),
                        ret_class: ret_class.map(|s| type_name(&s)),
                        value_params,
                        generic_sig,
                        contract,
                        context_count: pf.context_params_nullable.len(),
                        context_params_nullable: pf.context_params_nullable.clone(),
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
pub fn package_type_aliases(ci: &ClassInfo) -> &[(String, String)] {
    &ci.meta.type_aliases
}

/// Named-parameter lists of the class's constructors, from its `@Metadata`.
pub fn class_constructor_params(ci: &ClassInfo) -> Vec<ParamList> {
    ci.meta.constructor_params.clone()
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
fn type_aliases(ctx: &MetaCtx, this_class: &str) -> Vec<(String, String)> {
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
                if let Some((name, internal)) = parse_type_alias(body, records, d2) {
                    // Key the alias by its FULL internal name — its declaring package (the facade's) plus
                    // the alias's simple name — so `kotlin/collections/ArrayList` is distinct from any other
                    // package's `ArrayList`. `resolve_type` looks it up by that full name.
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

/// Decode a public `TypeAlias` message → `(alias name, expanded/underlying class internal name)`.
fn parse_type_alias(body: &[u8], records: &[Rec], d2: &[String]) -> Option<(String, String)> {
    let mut pb = Pb { b: body, i: 0 };
    let mut flags = 6u64;
    let mut name_id: Option<u64> = None;
    let mut expanded_class: Option<u64> = None;
    let mut underlying_class: Option<u64> = None;
    while !pb.at_end() {
        let tag = pb.varint()?;
        match (tag >> 3, tag & 7) {
            (1, 0) => flags = pb.varint()?,
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
    if flags_visibility(flags) != VIS_PUBLIC {
        return None;
    }
    let name = d2.get(name_id? as usize).cloned()?;
    let class_id = expanded_class.or(underlying_class)?;
    let internal = resolve_class_name(records, d2, class_id as usize)?;
    Some((name, internal))
}

/// Constructor source parameter names/default flags from `Class` `@Metadata`, in declaration order.
fn ctor_params(ctx: &MetaCtx) -> Vec<ParamList> {
    let mut out = Vec::new();
    let records = ctx.records;
    let d2 = ctx.d2;
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
                let mut names = Vec::new();
                let mut defaults = Vec::new();
                let mut vararg = None;
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
                            let mut is_vararg = false;
                            while !vp.at_end() {
                                let Some(vt) = vp.varint() else { break };
                                match (vt >> 3, vt & 7) {
                                    (1, 0) => vflags = vp.varint().unwrap_or(0), // ValueParameter.flags
                                    (2, 0) => nid = vp.varint().unwrap_or(0), // ValueParameter.name
                                    (4, 2) => {
                                        let Some(len) = vp.varint() else { break };
                                        is_vararg = true;
                                        if vp.bytes(len as usize).is_none() {
                                            break;
                                        }
                                    }
                                    (_, w) => {
                                        if vp.skip(w).is_none() {
                                            break;
                                        }
                                    }
                                }
                            }
                            names.push(
                                resolve_string(records, d2, nid as usize).unwrap_or_default(),
                            );
                            defaults.push(vflags & DECLARES_DEFAULT_VALUE_BIT != 0);
                            if is_vararg {
                                vararg = Some(names.len() - 1);
                            }
                        }
                        (_, w) => {
                            if cp.skip(w).is_none() {
                                break;
                            }
                        }
                    }
                }
                out.push(ParamList {
                    names,
                    defaults,
                    vararg,
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

/// Decode every `Property` (`prop_field`: 10 in a `Class`, 4 in a `Package`) of this metadata message
/// into [`MetaProp`]s — the property analogue of [`decode_functions`]. Carries the REAL getter/setter
/// JVM names from the `JvmPropertySignature`, so a resolver reads the accessor instead of guessing `getX`.
fn decode_properties(ctx: &MetaCtx, prop_field: u64) -> Vec<MetaProp> {
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
    // `Property.flags`: HAS_ANNOTATIONS(0) · VISIBILITY(1..3) · MODALITY(4..5) · IS_VAR(6) ·
    // HAS_GETTER(7) · HAS_SETTER(8) · IS_CONST(9) · …
    const IS_VAR_BIT: u64 = 1 << 6;
    const IS_CONST_BIT: u64 = 1 << 9;
    for prop in props {
        let mut p = Pb { b: prop, i: 0 };
        let mut name_id = None;
        let mut ret = None;
        let mut ret_nullable = false;
        let mut ret_body = None;
        let mut flags = 6u64;
        let mut sig = (None, None);
        let mut receiver_class = None;
        let mut receiver_body = None;
        let mut receiver_nullable = false;
        let mut type_params = Vec::new();
        while !p.at_end() {
            let Some(tag) = p.varint() else { break };
            match (tag >> 3, tag & 7) {
                (1, 0) => flags = p.varint().unwrap_or(6),
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
        let (getter, setter) = sig;
        let resolve_sig = |(nid, did): (u64, u64)| {
            Some(MetaJvmMethodSig {
                name: resolve_string(records, d2, nid as usize)?,
                desc: resolve_string(records, d2, did as usize)?,
            })
        };
        let is_var = setter.is_some() || flags & IS_VAR_BIT != 0;
        let generic_sig = build_property_generic_sig(
            &type_params,
            ret_body,
            ret_nullable,
            receiver_body,
            receiver_nullable,
            records,
            d2,
        );
        out.push(MetaProp {
            name,
            ret_class: ret,
            ret_nullable,
            generic_sig,
            getter: getter.and_then(resolve_sig),
            setter: setter.and_then(resolve_sig),
            visibility: crate::types::Visibility::from_metadata(flags_visibility(flags)),
            is_const: flags & IS_CONST_BIT != 0,
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
    use super::{
        decode_properties, parse_function, parse_receiver_type_gsig, parse_type_alias,
        parse_type_gsig, parse_type_gsig_node, parse_type_recv_fun, primary_erasure_bounds,
        read_kotlin_module, MetaCtx,
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
    fn nested_generic_signature_preserves_nullable_type_parameters() {
        let type_parameter = [0x40, 0x00, 0x18, 0x01];
        let parameters = HashMap::from([(0, "T".to_string())]);

        assert_eq!(
            parse_type_gsig_node(
                &type_parameter,
                &[],
                &[],
                &parameters,
                &HashMap::new(),
                true
            ),
            Some(Ty::nullable(Ty::ty_param(
                "T",
                Ty::nullable(Ty::obj("kotlin/Any"))
            )))
        );
        assert_eq!(
            parse_type_gsig(&type_parameter, &[], &[], &parameters),
            Some(Ty::ty_param("T", Ty::nullable(Ty::obj("kotlin/Any"))))
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
        for (annotations, expected_ids) in [
            ([extension_annotation, unrelated_annotation], vec![3, 4]),
            ([unrelated_annotation, extension_annotation], vec![4, 3]),
        ] {
            let body = prefix
                .into_iter()
                .chain(annotations.into_iter().flatten())
                .collect::<Vec<_>>();
            assert_eq!(
                parse_type_recv_fun(&body),
                (expected_ids, Some(1)),
                "the lightweight value-parameter decoder must preserve every annotation too"
            );
            assert_eq!(
                parse_type_gsig(&body, &[], &d2, &HashMap::new()),
                Some(expected)
            );
        }
    }

    #[test]
    fn extension_receiver_signature_preserves_top_level_nullability() {
        let nullable_string = [0x30, 0x00, 0x18, 0x01];
        assert_eq!(
            parse_receiver_type_gsig(
                &nullable_string,
                &[],
                &["kotlin/String".to_string()],
                &HashMap::new(),
            ),
            Some(Ty::nullable(Ty::String))
        );
        assert_eq!(
            parse_type_gsig(
                &nullable_string,
                &[],
                &["kotlin/String".to_string()],
                &HashMap::new(),
            ),
            Some(Ty::String),
            "ordinary generic-signature decoding keeps its existing top-level policy"
        );
    }

    #[test]
    fn type_alias_visibility_uses_the_public_default() {
        let d2 = vec!["Alias".to_string(), "sample/Real".to_string()];
        let omitted_flags = [0x10, 0x00, 0x32, 0x02, 0x30, 0x01];
        let internal_flags = [0x08, 0x00, 0x10, 0x00, 0x32, 0x02, 0x30, 0x01];

        assert_eq!(
            parse_type_alias(&omitted_flags, &[], &d2),
            Some(("Alias".to_string(), "sample/Real".to_string()))
        );
        assert_eq!(parse_type_alias(&internal_flags, &[], &d2), None);
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
            methods: &[],
        };

        let properties = decode_properties(&ctx, 4);

        assert_eq!(properties.len(), 1);
        assert_eq!(properties[0].name, "maybe");
        assert_eq!(
            properties[0].ret_class.map(|name| name.render()),
            Some("kotlin/String".to_string())
        );
        assert!(properties[0].ret_nullable);
        assert_eq!(properties[0].visibility, crate::types::Visibility::Public);
    }
}
