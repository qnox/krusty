//! Minimal Kotlin `@Metadata` reader: decode the `d1` protobuf and report which functions are
//! `inline`, by their JVM `(name, descriptor)`. This is the complete inline-recognition the inliner
//! needs (the body `reifiedOperationMarker` scan only finds *reified* inline functions).
//!
//! Schema (kotlin `core/metadata/src/metadata.proto` + `metadata.jvm/.../jvm_metadata.proto`):
//!   Package.function = 3; Function.flags = 9 (IS_INLINE = bit 10); Function.name = 2;
//!   Function extension method_signature = 100 → JvmMethodSignature { name = 1, desc = 2 }.
//! String ids index the `d2` table.

use super::classreader::ClassInfo;
use std::collections::HashSet;

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

/// `Visibility` enum value from a Function/Class `flags` word: `hasAnnotations` is bit 0, then
/// `Visibility` occupies the next 3 bits (kotlin metadata `Flags.VISIBILITY`). Enum order:
/// INTERNAL=0, PRIVATE=1, PROTECTED=2, PUBLIC=3, PRIVATE_TO_THIS=4, LOCAL=5.
fn flags_visibility(flags: u64) -> u64 {
    (flags >> 1) & 0x7
}
const VIS_PUBLIC: u64 = 3;

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
    /// Each SOURCE value parameter's type `class_name` id (`None` for a type-parameter/builtin param).
    /// The COUNT is the source arity (excludes synthetic descriptor params); resolved to names downstream.
    value_param_classes: Vec<Option<u64>>,
    /// Each SOURCE value parameter's NAME (`ValueParameter.name = 2`, a string-table id) — parallel to
    /// `value_param_classes`. Drives NAMED-ARGUMENT resolution for a classpath function call (the call
    /// `foo(b = …, a = …)` maps each label to a position via these names). `0` when absent.
    value_param_names: Vec<u64>,
    /// Whether each SOURCE value parameter `DECLARES_DEFAULT_VALUE` (`ValueParameter.flags = 1`, bit 1) —
    /// parallel to `value_param_classes`. Lets a classpath CALL omit a defaulted argument (resolved via
    /// the count of NON-defaulted params; the omitted call lowers to the `<name>$default` synthetic).
    value_param_has_default: Vec<bool>,
    /// Per SOURCE value parameter: `(type_annotation_id, first_type_argument_class_id)` — for a RECEIVER
    /// function-type param (`Recv.() -> R`), the `@ExtensionFunctionType` annotation id and the receiver
    /// class id. Resolved downstream: when the annotation is `kotlin/ExtensionFunctionType`, the param is
    /// a receiver-lambda param whose `this` is the first type argument. Parallel to `value_param_classes`.
    value_param_recv_ids: Vec<(Option<u64>, Option<u64>)>,
}

/// Parse one `Function` message. The return type is `Function.return_type = 3` and the extension
/// receiver `Function.receiver_type = 5` (both inline `Type`s in package metadata).
fn parse_function(body: &[u8]) -> Option<ParsedFunction> {
    let mut pb = Pb { b: body, i: 0 };
    let mut flags = 0u64;
    let mut name_id = 0u64;
    let mut jvm_sig = None;
    let mut ret_class = None;
    let mut recv_class = None;
    let mut has_receiver = false;
    let mut ret_nullable = false;
    let mut value_param_classes: Vec<Option<u64>> = Vec::new();
    let mut value_param_names: Vec<u64> = Vec::new();
    let mut value_param_has_default: Vec<bool> = Vec::new();
    #[allow(clippy::type_complexity)]
    let mut value_param_recv_ids: Vec<(Option<u64>, Option<u64>)> = Vec::new();
    while !pb.at_end() {
        let tag = pb.varint()?;
        match (tag >> 3, tag & 7) {
            (9, 0) => flags = pb.varint()?,   // flags
            (2, 0) => name_id = pb.varint()?, // name (name id in table)
            (3, 2) => {
                // return_type (inline Type message)
                let n = pb.varint()? as usize;
                let tbody = pb.bytes(n)?;
                ret_class = parse_type_class_name(tbody);
                ret_nullable = parse_type_nullable(tbody);
            }
            (5, 2) => {
                // receiver_type (inline Type message) — PRESENCE marks an extension, even when the
                // receiver is a type parameter (`fun <T> T.takeIf`) whose `class_name` is absent.
                has_receiver = true;
                let n = pb.varint()? as usize;
                let tbody = pb.bytes(n)?;
                recv_class = parse_type_class_name(tbody);
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
                        }
                        (_, w) => vp.skip(w)?,
                    }
                }
                value_param_classes.push(tid);
                value_param_names.push(nid);
                value_param_recv_ids.push(recv_ids);
                // `DECLARES_DEFAULT_VALUE` is bit 1 of the ValueParameter flags (HAS_ANNOTATIONS is bit 0).
                value_param_has_default.push(vflags & DECLARES_DEFAULT_VALUE_BIT != 0);
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
        value_param_classes,
        value_param_names,
        value_param_has_default,
        value_param_recv_ids,
    })
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

/// Per top-level function in a `Package`'s `@Metadata`: its Kotlin name → the *Kotlin* class name of its
/// return type (`mutableListOf` → `kotlin/collections/MutableList`), recovered from `@Metadata` (the JVM
/// descriptor/`Signature` erase this to `java/util/List`). Only entries whose return type is a class are
/// included. The `d2` string is the qualified name with `/` separators already.
pub fn return_types(fns: &[MetaFn]) -> std::collections::HashMap<String, String> {
    fns.iter()
        .filter_map(|f| Some((f.kotlin_name.clone(), f.ret_class.clone()?)))
        .collect()
}

/// Per top-level function name in a `Package`'s `@Metadata`: ALL the *Kotlin* class names of its
/// extension RECEIVERs (`plusAssign` → `[kotlin/collections/MutableCollection, …/MutableMap]`). A name
/// is overloaded across receivers (`plus` applies to `Collection`, `Map`, `Set`, `String`), so the value
/// is a list — a receiver applies if it is a subtype of ANY entry. The JVM descriptor erases the receiver
/// to its first parameter (`java/util/Collection`); only `@Metadata` keeps the read-only/mutable identity.
pub fn receivers(fns: &[MetaFn]) -> std::collections::HashMap<String, Vec<String>> {
    let mut out: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    for f in fns {
        if let Some(cn) = &f.receiver_class {
            let v = out.entry(f.kotlin_name.clone()).or_default();
            if !v.contains(cn) {
                v.push(cn.clone());
            }
        }
    }
    out
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
    pub jvm_desc: Option<String>,
    pub is_public: bool,
    pub is_inline: bool,
    pub is_suspend: bool,
    /// Extension-receiver Kotlin class name (`kotlin/Result` for `Result.getOrThrow`), if any. `None` for a
    /// top-level fn AND for an extension on a type PARAMETER — use [`MetaFn::is_extension`] to disambiguate.
    pub receiver_class: Option<String>,
    /// Whether this is an EXTENSION (has a receiver of any kind — class or type parameter) vs a true
    /// top-level function. Lets the classpath ext index avoid mis-indexing a top-level generic as an
    /// extension on its first parameter's type.
    pub is_extension: bool,
    /// The Kotlin return-type class name (`kotlin/UInt` for `UInt.coerceAtMost`), if it is a class type.
    pub ret_class: Option<String>,
    /// Whether the Kotlin return type is nullable (`T?`) — `Type.nullable`. The JVM descriptor/`Signature`
    /// erase this; only `@Metadata` carries it. Drives the elvis null-check for a nullable-returning scope
    /// fn (`takeIf`/`takeUnless` return `T?`).
    pub ret_nullable: bool,
    /// Each SOURCE value parameter's Kotlin type internal name (`kotlin/Function0` for `remember`'s
    /// `calculation`); `None` for a type-parameter/unresolved param. The LENGTH is the SOURCE arity — it
    /// EXCLUDES the synthetic params the JVM descriptor appends (`suspend` Continuation, `@Composable`
    /// Composer/int). The resolver matches a call against THIS signature; the descriptor drives emit.
    pub value_param_types: Vec<Option<String>>,
    /// Each SOURCE value parameter's NAME (parallel to `value_param_types`), for NAMED-ARGUMENT resolution
    /// of a classpath call. Empty string when metadata omits the name. LENGTH = source arity.
    pub value_param_names: Vec<String>,
    /// Whether each SOURCE value parameter declares a default value (parallel to `value_param_types`).
    /// A classpath call may omit a trailing run of these; the resolver counts the NON-defaulted params as
    /// the required arity, and the omitted call lowers to the `<name>$default` synthetic.
    pub value_param_has_default: Vec<bool>,
    /// Per SOURCE value parameter: `Some(receiver_internal)` when it is a RECEIVER function-type param
    /// (`Recv.() -> R`, carrying `@ExtensionFunctionType`). A lambda passed to it binds `this` to that
    /// receiver. Parallel to `value_param_types`; `None` for an ordinary parameter.
    pub value_param_recv_funs: Vec<Option<String>>,
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
                    let (jvm_name, mut jvm_desc) = match pf.jvm_sig {
                        Some((nid, did)) => (
                            resolve_string(&records, d2, nid as usize)
                                .unwrap_or_else(|| kotlin_name.clone()),
                            resolve_string(&records, d2, did as usize),
                        ),
                        None => (kotlin_name.clone(), None),
                    };
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
                    let value_param_types: Vec<Option<String>> = pf
                        .value_param_classes
                        .iter()
                        .map(|o| o.and_then(|id| resolve_class_name(&records, d2, id as usize)))
                        .collect();
                    // Param names are plain string-table entries (like the JVM name/desc), NOT class names.
                    let value_param_names: Vec<String> = pf
                        .value_param_names
                        .iter()
                        .map(|&id| resolve_string(&records, d2, id as usize).unwrap_or_default())
                        .collect();
                    // A RECEIVER function-type param: the type annotation must resolve to
                    // `kotlin/ExtensionFunctionType`; then the receiver is the first type argument's class.
                    let value_param_recv_funs: Vec<Option<String>> = pf
                        .value_param_recv_ids
                        .iter()
                        .map(|(anno_id, arg0)| {
                            let is_ext_fun = anno_id
                                .and_then(|id| resolve_class_name(&records, d2, id as usize))
                                .as_deref()
                                == Some("kotlin/ExtensionFunctionType");
                            if is_ext_fun {
                                arg0.and_then(|id| resolve_class_name(&records, d2, id as usize))
                            } else {
                                None
                            }
                        })
                        .collect();
                    out.push(MetaFn {
                        kotlin_name,
                        jvm_name,
                        jvm_desc,
                        is_public: pf.is_public,
                        is_inline: pf.is_inline,
                        is_suspend: pf.is_suspend,
                        receiver_class,
                        is_extension: pf.has_receiver,
                        ret_class,
                        ret_nullable: pf.ret_nullable,
                        value_param_types,
                        value_param_names,
                        value_param_has_default: pf.value_param_has_default,
                        value_param_recv_funs,
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

/// A classpath `@JvmInline value class` decoded from `@Metadata`: the single underlying property and its
/// Kotlin type. A value class erases to this underlying type on the old JVM (`UInt` → `kotlin/Int` → `int`;
/// `Result<T>` → a type parameter → `None`, erasing to `Object`).
#[derive(Clone, Debug)]
pub struct InlineClass {
    /// Kotlin class name of the underlying type (`kotlin/Int` for `UInt`); `None` when the underlying is a
    /// type parameter (`Result<T>`), which erases to `kotlin/Any`/`Object`.
    pub underlying_class: Option<String>,
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
    let mut property_name = None;
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
                underlying_class = parse_type_class_name(tbody)
                    .and_then(|id| resolve_class_name(&records, d2, id as usize));
            }
            (19, 0) => {
                // inline_class_underlying_type_id (type id in table) — marks a value class even when the
                // type is carried in the type table (not inlined); underlying stays unresolved here.
                pb.varint()?;
                is_value = true;
            }
            (_, w) => {
                if pb.skip(w).is_none() {
                    break;
                }
            }
        }
    }
    is_value.then_some(InlineClass {
        underlying_class,
        property_name,
    })
}

/// One `@JvmName`-mangled overload of a Kotlin function name: the JVM method name, JVM descriptor, and the
/// Kotlin return-type class. The data `@OverloadResolutionByLambdaReturnType` needs to pick `sumOf` →
/// `sumOfInt`/`sumOfLong`/… by the lambda's return type.
#[derive(Clone, Debug)]
pub struct JvmOverload {
    pub jvm_name: String,
    pub jvm_desc: String,
    pub ret_class: String,
}

/// Per `Package` function: its Kotlin name → its `@JvmName`-mangled overloads. Bridges the Kotlin name to
/// the JVM method that `@OverloadResolutionByLambdaReturnType` selects by return type (the source name
/// `sumOf` has no JVM method of its own — only `sumOfInt`/`sumOfLong`/…).
pub fn package_lambda_return_overloads(
    ci: &ClassInfo,
) -> std::collections::HashMap<String, Vec<JvmOverload>> {
    let mut out: std::collections::HashMap<String, Vec<JvmOverload>> =
        std::collections::HashMap::new();
    if ci.kotlin_d1.is_empty() {
        return out;
    }
    let bytes = decode_d1(&ci.kotlin_d1);
    let (st_body, pkg_body) = split_d1(&bytes);
    let records = parse_string_table(st_body);
    let d2 = &ci.kotlin_d2;
    let mut pb = Pb { b: pkg_body, i: 0 };
    while !pb.at_end() {
        let Some(tag) = pb.varint() else { break };
        match (tag >> 3, tag & 7) {
            (3, 2) => {
                let Some(len) = pb.varint() else { break };
                let Some(fbody) = pb.bytes(len as usize) else {
                    break;
                };
                if let Some(f) = parse_function(fbody) {
                    if let (Some(kname), Some((ni, di)), Some(rc)) =
                        (d2.get(f.name_id as usize), f.jvm_sig, f.ret_class)
                    {
                        // The JVM `@JvmName` method name/descriptor index the d1 STRING TABLE — resolve as
                        // PLAIN strings (no class-name `operation` transform). The return type is a class id
                        // (`resolve_class_name`, which applies the transform).
                        if let (Some(jvm_name), Some(jvm_desc), Some(ret_class)) = (
                            resolve_string(&records, d2, ni as usize),
                            resolve_string(&records, d2, di as usize),
                            resolve_class_name(&records, d2, rc as usize),
                        ) {
                            out.entry(kname.clone()).or_default().push(JvmOverload {
                                jvm_name,
                                jvm_desc,
                                ret_class,
                            });
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

/// Per top-level function name in a `Package`'s `@Metadata`: whether its Kotlin return type is nullable
/// (`takeIf`/`takeUnless` → `T?` → `true`). The JVM descriptor/`Signature` erase nullability; only
/// `@Metadata` keeps it. A name is `true` if ANY overload's return is nullable (the scope fns are not
/// overloaded, so this is exact for them).
pub fn return_nullable(fns: &[MetaFn]) -> std::collections::HashMap<String, bool> {
    let mut out = std::collections::HashMap::new();
    for f in fns {
        let e = out.entry(f.kotlin_name.clone()).or_insert(false);
        *e = *e || f.ret_nullable;
    }
    out
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

/// Read a `.kotlin_builtins` resource into the direct-supertype map of every `Class` it declares
/// (`kotlin/collections/MutableList` → `[…/List, …/MutableCollection]`) — the Kotlin collection
/// hierarchy as kotlinc stores it. A thin view over [`parse_builtins`] (which also yields members).
pub fn builtins_supertypes(data: &[u8]) -> std::collections::HashMap<String, Vec<String>> {
    parse_builtins(data)
        .into_iter()
        .map(|(k, c)| (k, c.supertypes))
        .collect()
}

/// One member of a builtins `Class`: its Kotlin name, value-parameter type names, and return type name
/// — all Kotlin internal names (`kotlin/Int`, `kotlin/String`, …) resolved from the fragment's tables.
pub struct BuiltinMember {
    pub name: String,
    pub params: Vec<String>,
    pub ret: String,
}

/// A builtin `Class` decoded from a `.kotlin_builtins` fragment: its direct supertypes and declared
/// members — the two facets the front end needs (the read-only/mutable hierarchy AND each type's API).
#[derive(Default)]
pub struct BuiltinClass {
    pub supertypes: Vec<String>,
    pub members: Vec<BuiltinMember>,
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
        let mut supids: Vec<u64> = Vec::new();
        let mut types: Vec<&[u8]> = Vec::new();
        let mut funcs: Vec<&[u8]> = Vec::new();
        let mut props: Vec<&[u8]> = Vec::new();
        while !cp.at_end() {
            let Some(tag) = cp.varint() else { break };
            match (tag >> 3, tag & 7) {
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
                if let (Some(name), Some(ret)) = (strings.get(ni as usize).cloned(), type_of_id(ri))
                {
                    members.push(BuiltinMember { name, params, ret });
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
                    members.push(BuiltinMember {
                        name,
                        params: vec![],
                        ret,
                    });
                }
            }
        }
        out.insert(
            fqname,
            BuiltinClass {
                supertypes,
                members,
            },
        );
    }
    out
}

/// All `inline` functions declared in a `Package` body: explicit JVM `(name, descriptor)` pairs (when
/// a `method_signature` extension is present) and the set of inline function *names* (always, from
/// `Function.name`) — the latter catches the common inline functions (`map`, `let`, …) whose JVM
/// signature equals the computed default, so they omit the extension.
fn package_inline(body: &[u8], d2: &[String]) -> (HashSet<(String, String)>, HashSet<String>) {
    package_flagged(body, d2, |f| f.is_inline)
}

/// All functions in a `Package` body whose decoded `Function` satisfies `pred` (e.g. `is_inline` /
/// `is_suspend`): explicit JVM `(name, descriptor)` pairs (when a `method_signature` extension is
/// present) and the set of function *names* (always, from `Function.name`).
fn package_flagged(
    body: &[u8],
    d2: &[String],
    pred: impl Fn(&ParsedFunction) -> bool,
) -> (HashSet<(String, String)>, HashSet<String>) {
    let mut methods = HashSet::new();
    let mut names = HashSet::new();
    // Skip the leading delimited `StringTableTypes`; the `Package` (with the functions) follows it.
    let (_, pkg) = split_d1(body);
    let mut pb = Pb { b: pkg, i: 0 };
    while !pb.at_end() {
        let Some(tag) = pb.varint() else { break };
        match (tag >> 3, tag & 7) {
            (3, 2) => {
                // repeated Function function = 3
                let Some(len) = pb.varint() else { break };
                let Some(fbody) = pb.bytes(len as usize) else {
                    break;
                };
                if let Some(f) = parse_function(fbody).filter(&pred) {
                    if let Some(n) = d2.get(f.name_id as usize) {
                        names.insert(n.clone());
                    }
                    if let Some((ni, di)) = f.jvm_sig {
                        if let (Some(n), Some(d)) = (d2.get(ni as usize), d2.get(di as usize)) {
                            methods.insert((n.clone(), d.clone()));
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
    (methods, names)
}

/// The Kotlin names of every `suspend` function in a class's `@Metadata` (from the `IS_SUSPEND` flag
/// bit). A call to a method of one of these names (in this class) is a suspension point.
pub fn suspend_method_names(ci: &ClassInfo) -> HashSet<String> {
    if ci.kotlin_d1.is_empty() {
        return HashSet::new();
    }
    package_flagged(&decode_d1(&ci.kotlin_d1), &ci.kotlin_d2, |f| f.is_suspend).1
}

/// The JVM `(name, descriptor)` of every `inline` function in a class with an explicit method
/// signature in its `@Metadata`. (Common inline functions omit it — see [`inline_method_names`].)
pub fn inline_methods(ci: &ClassInfo) -> HashSet<(String, String)> {
    if ci.kotlin_d1.is_empty() {
        return HashSet::new();
    }
    package_inline(&decode_d1(&ci.kotlin_d1), &ci.kotlin_d2).0
}

/// The Kotlin names of every `inline` function in a class's `@Metadata`. A call to a method of one of
/// these names (in this class) is inline — descriptor-agnostic, so it catches the functions whose
/// signature equals the default and thus carry no explicit `method_signature`.
pub fn inline_method_names(ci: &ClassInfo) -> HashSet<String> {
    if ci.kotlin_d1.is_empty() {
        return HashSet::new();
    }
    package_inline(&decode_d1(&ci.kotlin_d1), &ci.kotlin_d2).1
}
