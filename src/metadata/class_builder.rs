//! Build the `@kotlin.Metadata` `d1`/`d2` payload for a Kotlin *class* (kind=1), so a Kotlin
//! consumer recognizes krusty's emitted class as a genuine Kotlin class (property syntax, etc.).
//!
//! Schema reverse-engineered from kotlinc 1.9.24 for `class Point(val x: Int, var y: String)`
//! (see METADATA_NOTES.md). `ProtoBuf.Class`: f3=fq_name (a class-id string-table entry),
//! f6=supertype `Type`, f8=constructor, f10=property (repeated). `Type.class_name`=f6.
//! `Constructor`: f2=value_parameter, f100=JvmMethodSignature ext (desc). `Property`: f2=name,
//! f3=return_type, f11=flags (emitted as 1798 only for a `var`), f100=JvmPropertySignature
//! {f1=field (empty → derived), f3=getter, f4=setter}. `JvmMethodSignature`: f1=name, f2=desc.
//!
//! String table: a class id uses operation `DESC_TO_CLASS_ID` (Record.f3=2) over `Lpkg/Name;`;
//! builtin types use `predefined_index` (Record.f2); everything else is a verbatim d2 entry.

use crate::metadata::protobuf::Pb;
use crate::types::Ty;

/// Property descriptor for class metadata: name, type, mutability, and JVM accessor signatures.
pub struct PropMeta {
    pub name: String,
    pub ty: Ty,
    pub is_var: bool,
    pub getter: (String, String),         // (jvm name, jvm descriptor)
    pub setter: Option<(String, String)>, // present iff `var`
}

/// Member-function descriptor for class metadata (`Class.function` = f9). The JVM signature is usually
/// derivable, so no extension is emitted — EXCEPT when a param/return is a boxed nullable primitive
/// (`Int?` → `Integer`), where kotlinc records the descriptor via a `JvmMethodSignature` (f100).
pub struct FnMeta {
    pub name: String,
    pub params: Vec<(String, Ty)>,
    pub ret: Ty,
    /// `Function.flags` (f9): e.g. operator (`componentN`) or the data-class `copy`. 0 ⇒ omitted.
    pub flags: u64,
    /// Mark every value parameter `DECLARES_DEFAULT_VALUE` (so a Kotlin caller may omit it) — used
    /// for the synthesized `copy`.
    pub params_have_defaults: bool,
    /// The JVM method descriptor for a `JvmMethodSignature` (f100), emitted only when the signature is
    /// not derivable from the proto types — a boxed nullable-primitive param/return on a synthesized
    /// `componentN`/`copy`, or a value class's `equals`/`hashCode`/`toString` (which dispatch to a
    /// differently-named static `-impl`). `None` ⇒ no extension.
    pub jvm_sig: Option<String>,
    /// The `JvmMethodSignature.name` (f1) when the JVM name differs from the Kotlin one — a value
    /// class's `equals` → `equals-impl`. `None` ⇒ name omitted (derivable), kotlinc's usual shape.
    pub jvm_sig_name: Option<String>,
}

impl FnMeta {
    /// A plain member function (no special flags) — the common case.
    pub fn plain(name: String, params: Vec<(String, Ty)>, ret: Ty) -> FnMeta {
        FnMeta {
            name,
            params,
            ret,
            flags: 0,
            params_have_defaults: false,
            jvm_sig: None,
            jvm_sig_name: None,
        }
    }
}

/// `Function.flags` kotlinc emits for a data class's synthesized `componentN` (public final
/// operator member) and `copy` (public final member). Reverse-engineered from kotlinc 1.9.24.
pub const COMPONENT_FN_FLAGS: u64 = 454;
pub const COPY_FN_FLAGS: u64 = 198;
/// `Function.flags` for the data-class-synthesized `equals`/`hashCode`/`toString` (public final member,
/// overriding a supertype member — hence the higher bits). From kotlinc 2.4.0.
pub const EQUALS_FN_FLAGS: u64 = 0x101d6;
pub const HASHCODE_TOSTRING_FN_FLAGS: u64 = 0x100d6;
/// `Class.flags` (f1) for a plain `public final class` — kotlinc's DEFAULT, so the field is OMITTED at
/// this exact value (an `internal class` writes an explicit `0`, visibility INTERNAL being 0).
pub const DEFAULT_CLASS_FLAGS: u64 = 6;
/// `Constructor.flags` (f1) for a sealed class's primary constructor — kotlinc marks it PROTECTED.
pub const SEALED_CTOR_FLAGS: u64 = 4;
/// `ValueParameter.flags` bit for `DECLARES_DEFAULT_VALUE`.
const DECLARES_DEFAULT_VALUE: u64 = 2;

/// `predefinedIndex` of a builtin fq-name in `JvmNameResolverBase.PREDEFINED_STRINGS`.
fn predefined_index(t: Ty) -> u64 {
    match t {
        Ty::Unit => 2,
        Ty::Byte => 5,
        Ty::Double => 6,
        Ty::Float => 7,
        Ty::Int => 8,
        Ty::Long => 9,
        Ty::Short => 10,
        Ty::Boolean => 11,
        Ty::Char => 12,
        Ty::String => 14,
        // `UInt`/`ULong` are value classes over Int/Long — their @Metadata class name is the unsigned
        // type itself (a class-id, not a builtin), so they fall through to the class-id path elsewhere.
        _ => 0, // kotlin/Any fallback
    }
}
const ANY_PREDEFINED: u64 = 0;

/// `var` property flags, as kotlinc emits them in `Property` field 11 (public mutable property with
/// default accessors). `val` properties default to 0 and the field is omitted.
const VAR_PROPERTY_FLAGS: u64 = 1798;

#[derive(Default)]
struct StringTable {
    strings: Vec<String>,
    records: Vec<Pb>,
    /// Dedup by (d2 string, record bytes) — kotlinc reuses an existing entry when the same string with
    /// the same record (verbatim / builtin-index / class-id operation) is interned again.
    dedup: std::collections::HashMap<(String, Vec<u8>), u32>,
}

impl StringTable {
    /// Intern `(string, record)`, returning an existing index for an identical prior entry (kotlinc's
    /// `JvmStringTable` dedup) or appending a new one.
    fn intern(&mut self, s: String, record: Pb) -> u32 {
        let key = (s.clone(), record.as_bytes().to_vec());
        if let Some(&i) = self.dedup.get(&key) {
            return i;
        }
        let i = self.strings.len() as u32;
        self.strings.push(s);
        self.records.push(record);
        self.dedup.insert(key, i);
        i
    }
    /// A verbatim source string (empty `Record` → use the d2 entry as-is).
    fn local(&mut self, s: &str) -> u32 {
        self.intern(s.to_string(), Pb::new())
    }
    /// A builtin fq-name via predefinedIndex (Record.f2). The d2 slot is empty.
    fn builtin(&mut self, predefined: u64) -> u32 {
        let mut r = Pb::new();
        r.field_varint(2, predefined);
        self.intern(String::new(), r)
    }
    /// A class id from a type descriptor `Lpkg/Name;` via operation DESC_TO_CLASS_ID (Record.f3=2).
    fn class_id_from_desc(&mut self, descriptor: &str) -> u32 {
        let mut r = Pb::new();
        r.field_varint(3, 2); // operation = DESC_TO_CLASS_ID
        self.intern(descriptor.to_string(), r)
    }
    fn serialize_types(&self) -> Pb {
        // kotlinc's `JvmStringTable` COALESCES a run of consecutive verbatim strings (no operation /
        // predefinedIndex) into ONE `Record` with `range` (field 1) = the run length; a single verbatim
        // string is an empty record (range defaults to 1). Records that carry an operation or a
        // predefined index (class-id, builtin) are never coalesced. Matching this is required for a
        // byte-identical d1.
        let mut p = Pb::new();
        let mut i = 0;
        while i < self.records.len() {
            if self.records[i].is_empty() {
                let mut run = 1;
                while i + run < self.records.len() && self.records[i + run].is_empty() {
                    run += 1;
                }
                let mut rec = Pb::new();
                if run > 1 {
                    rec.field_varint(1, run as u64); // Record.range = 1
                }
                p.repeated_message(1, &rec);
                i += run;
            } else {
                p.repeated_message(1, &self.records[i]);
                i += 1;
            }
        }
        p
    }
}

/// `predefinedIndex` of a builtin fq-NAME (used when a builtin arrives as `Ty::Obj`, e.g. `kotlin/Any`
/// as an `equals` param) — kotlinc encodes these via `builtin`, NOT a class-id descriptor.
fn builtin_name_index(internal: &str) -> Option<u64> {
    match internal {
        "kotlin/Any" => Some(0),
        "kotlin/Nothing" => Some(1),
        "kotlin/Unit" => Some(2),
        "kotlin/Int" => Some(8),
        "kotlin/Long" => Some(9),
        "kotlin/Boolean" => Some(11),
        "kotlin/CharSequence" => Some(13),
        "kotlin/String" => Some(14),
        // Common collection interfaces from `JvmNameResolverBase.PREDEFINED_STRINGS` (kotlinc encodes
        // these via `predefinedIndex`, an EMPTY d2 slot — not a class-id descriptor). Indices verified
        // against kotlinc's emitted record (List = 32).
        "kotlin/collections/Iterable" => Some(28),
        "kotlin/collections/MutableIterable" => Some(29),
        "kotlin/collections/Collection" => Some(30),
        "kotlin/collections/MutableCollection" => Some(31),
        "kotlin/collections/List" => Some(32),
        "kotlin/collections/MutableList" => Some(33),
        "kotlin/collections/Set" => Some(34),
        "kotlin/collections/MutableSet" => Some(35),
        "kotlin/collections/Map" => Some(36),
        "kotlin/collections/MutableMap" => Some(37),
        _ => None,
    }
}

fn type_pb(st: &mut StringTable, t: Ty) -> Pb {
    let mut p = Pb::new();
    // `T?` sets `Type.nullable` (f3) and encodes the underlying type's class-name.
    let (nullable, base) = match t {
        Ty::Nullable(inner) => (true, *inner),
        other => (false, other),
    };
    // Generic arguments (`List<String>` → one `Type.argument`). A boxed `Array<T>`/primitive-array
    // encodes its element in the class-name descriptor and carries NO proto argument (matching kotlinc).
    let args: &[Ty] = match base {
        Ty::Obj(n, _) if n.matches("kotlin/Array") => &[],
        Ty::Obj(_, a) => a,
        _ => &[],
    };
    let class_name = match base {
        Ty::Obj(internal, _) => {
            let internal = internal.render();
            match builtin_name_index(&internal) {
                Some(idx) => st.builtin(idx),
                None => st.class_id_from_desc(&format!("L{internal};")),
            }
        }
        _ => st.builtin(predefined_index(base)),
    };
    // `Type.Argument` (each interning its own type). An INVARIANT projection is the proto default and
    // omitted, so an argument is just `{ type = f2 }`. `Type.argument` and `Argument.type` are both f2.
    let arg_pbs: Vec<Pb> = args
        .iter()
        .map(|a| {
            let mut arg = Pb::new();
            arg.field_message(2, &type_pb(st, *a)); // Type.Argument.type = 2
            arg
        })
        .collect();
    for arg in &arg_pbs {
        p.field_message(2, arg); // Type.argument = 2 (repeated), before nullable/class_name
    }
    if nullable {
        p.field_varint(3, 1); // Type.nullable = 3 (written before class_name, matching kotlinc)
    }
    p.field_varint(6, class_name as u64); // Type.class_name = 6
    p
}

/// Build one `Class.constructor` message: `flags` (f1, omitted if 0), value parameters (f2), and the
/// JvmProtoBuf constructor signature (f100, name `<init>` + `desc`).
fn build_ctor(
    st: &mut StringTable,
    params: &[(String, Ty)],
    desc: &str,
    flags: u64,
    param_defaults: &[bool],
    sig_name: Option<&str>,
) -> Pb {
    let mut ctor = Pb::new();
    if flags != 0 {
        ctor.field_varint(1, flags); // Constructor.flags = 1
    }
    for (i, (pname, pty)) in params.iter().enumerate() {
        let mut vp = Pb::new();
        // `ValueParameter.flags` (f1) with DECLARES_DEFAULT_VALUE for a param that declares a default —
        // written before the name, matching kotlinc.
        if param_defaults.get(i).copied().unwrap_or(false) {
            vp.field_varint(1, DECLARES_DEFAULT_VALUE);
        }
        vp.field_varint(2, st.local(pname) as u64); // ValueParameter.name = 2
        let ty = type_pb(st, *pty);
        vp.field_message(3, &ty); // ValueParameter.type = 3
        ctor.repeated_message(2, &vp); // Constructor.value_parameter = 2
    }
    let sig = jvm_method_sig(st, Some(sig_name.unwrap_or("<init>")), desc);
    ctor.field_message(100, &sig); // JvmProtoBuf.constructorSignature = 100
    ctor
}

fn jvm_method_sig(st: &mut StringTable, name: Option<&str>, desc: &str) -> Pb {
    let mut p = Pb::new();
    if let Some(n) = name {
        p.field_varint(1, st.local(n) as u64); // JvmMethodSignature.name = 1
    }
    p.field_varint(2, st.local(desc) as u64); // JvmMethodSignature.desc = 2
    p
}

/// Build `(d1 bytes, d2 strings)` for a class. `class_internal` is e.g. `demo/Point`;
/// `ctor_params` are the primary-constructor `(name, type)` pairs; `ctor_desc` its JVM descriptor.
/// `Class.flags` values kotlinc emits: a plain class = 0 (omitted), `data class` = 1030,
/// `object` = 326. Passed in by the caller.
/// Class-level metadata beyond the members: the `Class.flags`, the companion object's simple name (if
/// any), and the nested class simple names — kept in one struct so [`build_class`] stays within the
/// argument-count limit.
/// A secondary constructor for class metadata (`Class.constructor` = f8, repeated after the primary).
pub struct CtorMeta<'a> {
    pub params: &'a [(String, Ty)],
    pub desc: &'a str,
    /// `Constructor.flags` (f8's f1) — e.g. 22 for a plain secondary ctor. 0 ⇒ omitted (the primary).
    pub flags: u64,
}

pub struct ClassTail<'a> {
    pub flags: u64,
    pub companion: Option<&'a str>,
    pub nested: &'a [&'a str],
    /// The `-module-name` value → `Class.classModuleName` (f101, a JvmProtoBuf extension). kotlinc
    /// omits it for the default module `main`; downstream builds always set `-module-name`.
    pub module_name: Option<&'a str>,
    /// Secondary constructors (after the primary), each `Class.constructor` (f8). They intern their
    /// strings right after the primary ctor, before properties/functions.
    pub secondary_ctors: &'a [CtorMeta<'a>],
    /// Per-primary-ctor-parameter `DECLARES_DEFAULT_VALUE` flags (parallel to `ctor_params`). A param
    /// with a default (`routes: List<String> = emptyList()`) gets the flag, as kotlinc emits. Empty ⇒
    /// no param has a default.
    pub ctor_param_defaults: &'a [bool],
    /// A `@JvmInline value class`'s sole underlying property `(name, type)` → `Class`
    /// `inlineClassUnderlyingPropertyName` (f17, the name's string-table id) +
    /// `inlineClassUnderlyingType` (f18, an inline `Type`). `None` for an ordinary class.
    pub inline_underlying: Option<(&'a str, Ty)>,
    /// Whether the class HAS a primary constructor at all — an `interface` has none, so `Class` carries
    /// no `constructor` (f8) entry. Defaults to true (every other kind).
    pub emit_primary_ctor: bool,
    /// `Constructor.flags` (f1) for the PRIMARY constructor — 0 (omitted) for an ordinary class; a
    /// sealed class's primary ctor is PROTECTED, which kotlinc records.
    pub primary_ctor_flags: u64,
    /// The primary constructor's `JvmMethodSignature` NAME — a value class's primary ctor is realized as
    /// the static `constructor-impl`, not `<init>`. `None` ⇒ `<init>` (the ordinary shape).
    pub ctor_sig_name: Option<&'a str>,
}

impl Default for ClassTail<'_> {
    fn default() -> Self {
        ClassTail {
            flags: DEFAULT_CLASS_FLAGS,
            companion: None,
            nested: &[],
            module_name: None,
            secondary_ctors: &[],
            ctor_param_defaults: &[],
            inline_underlying: None,
            ctor_sig_name: None,
            emit_primary_ctor: true,
            primary_ctor_flags: 0,
        }
    }
}

pub fn build_class(
    class_internal: &str,
    ctor_params: &[(String, Ty)],
    ctor_desc: &str,
    props: &[PropMeta],
    methods: &[FnMeta],
    enum_entries: &[String],
    tail: &ClassTail,
) -> (Vec<u8>, Vec<String>) {
    let class_flags = tail.flags;
    let companion_name = tail.companion;
    let nested_class_names = tail.nested;
    let mut st = StringTable::default();

    // STRINGS ARE INTERNED IN kotlinc's ORDER (fq_name, supertype, constructors, properties'
    // JVM signatures, functions, enum entries, then the companion + nested names LAST) even though the
    // proto writes fields in field-number order below — so the d2 indices match. Build every sub-message
    // first (interning), then assemble the `Class` message.

    // f3 = fq_name: a class-id derived from the `L...;` descriptor.
    let fq = st.class_id_from_desc(&format!("L{class_internal};"));

    // f6 = supertype kotlin/Any.
    let mut supertype = Pb::new();
    supertype.field_varint(6, st.builtin(ANY_PREDEFINED) as u64);

    // f8 = constructors: the primary (flags 0), then any secondary constructors — each interning in
    // order (kotlinc emits the ctor JVM name `<init>` explicitly, not omitted).
    let mut ctor_msgs = if tail.emit_primary_ctor {
        vec![build_ctor(
            &mut st,
            ctor_params,
            ctor_desc,
            tail.primary_ctor_flags,
            tail.ctor_param_defaults,
            tail.ctor_sig_name,
        )]
    } else {
        Vec::new()
    };
    for sc in tail.secondary_ctors {
        ctor_msgs.push(build_ctor(&mut st, sc.params, sc.desc, sc.flags, &[], None));
    }

    // Properties BEFORE functions (kotlinc interns property JVM signatures before function names).
    let prop_msgs: Vec<Pb> = props
        .iter()
        .map(|p| {
            let mut prop = Pb::new();
            prop.field_varint(2, st.local(&p.name) as u64); // Property.name = 2
            let ty = type_pb(&mut st, p.ty);
            prop.field_message(3, &ty); // Property.return_type = 3
            if p.is_var {
                prop.field_varint(11, VAR_PROPERTY_FLAGS); // Property flags (var only)
            }
            let mut jvm = Pb::new();
            // A nullable PRIMITIVE property (`Int?`, `Double?`, …) has a BOXED backing field
            // (`Ljava/lang/Integer;`, `Ljava/lang/Double;`), which the reader can't derive from the
            // nullable-primitive return type — so kotlinc records an explicit `JvmFieldSignature.desc`
            // (the boxed descriptor = the getter's return type). Every other property leaves the field
            // empty (the reader derives it). kotlinc interns the getter/setter strings BEFORE the field
            // descriptor (even though the proto writes `field` (f1) first), so build them in that order.
            let getter = jvm_method_sig(&mut st, Some(&p.getter.0), &p.getter.1);
            let setter = p
                .setter
                .as_ref()
                .map(|(sn, sd)| jvm_method_sig(&mut st, Some(sn), sd));
            let boxed_field_desc = match p.ty {
                Ty::Nullable(
                    Ty::Int
                    | Ty::Long
                    | Ty::Double
                    | Ty::Float
                    | Ty::Byte
                    | Ty::Short
                    | Ty::Char
                    | Ty::Boolean,
                ) => p.getter.1.rsplit(')').next().map(str::to_string),
                _ => None,
            };
            let mut field = Pb::new();
            if let Some(d) = &boxed_field_desc {
                field.field_varint(2, st.local(d) as u64); // JvmFieldSignature.desc = 2
            }
            jvm.field_message(1, &field); // field (empty → derived; boxed primitive → explicit desc)
            jvm.field_message(3, &getter); // JvmPropertySignature.getter = 3
            if let Some(setter) = &setter {
                jvm.field_message(4, setter); // JvmPropertySignature.setter = 4
            }
            prop.field_message(100, &jvm); // JvmProtoBuf.propertySignature = 100
            prop
        })
        .collect();

    // Member functions (name f2, return_type f3, value_parameter f6, flags f9; JVM sig derivable).
    let func_msgs: Vec<Pb> = methods
        .iter()
        .map(|m| {
            let mut func = Pb::new();
            func.field_varint(2, st.local(&m.name) as u64);
            let ret = type_pb(&mut st, m.ret);
            func.field_message(3, &ret);
            for (pname, pty) in &m.params {
                let mut vp = Pb::new();
                if m.params_have_defaults {
                    vp.field_varint(1, DECLARES_DEFAULT_VALUE); // ValueParameter.flags = 1
                }
                vp.field_varint(2, st.local(pname) as u64);
                let ty = type_pb(&mut st, *pty);
                vp.field_message(3, &ty);
                func.repeated_message(6, &vp); // Function.value_parameter = 6
            }
            if m.flags != 0 {
                func.field_varint(9, m.flags); // Function.flags = 9
            }
            if let Some(sig) = &m.jvm_sig {
                // `JvmMethodSignature` (f100), desc only (name derivable) — a boxed nullable-primitive
                // signature kotlinc records because the proto types alone don't pin the JVM descriptor.
                func.field_message(
                    100,
                    &jvm_method_sig(&mut st, m.jvm_sig_name.as_deref(), sig),
                );
            }
            func
        })
        .collect();

    // f13 = enum entries (`EnumEntry { name = f1 }`).
    let enum_msgs: Vec<Pb> = enum_entries
        .iter()
        .map(|entry| {
            let mut ee = Pb::new();
            ee.field_varint(1, st.local(entry) as u64);
            ee
        })
        .collect();

    // A `@JvmInline value class`'s underlying property name + type (`Class` f17/f18). Interned with the
    // members (before the companion/nested tail) so the d2 order matches kotlinc.
    let inline_underlying: Option<(u32, Pb)> = tail
        .inline_underlying
        .map(|(name, ty)| (st.local(name), type_pb(&mut st, ty)));

    // Companion + nested class names intern LAST (kotlinc's d2 places them after all members).
    let companion_idx = companion_name.map(|c| st.local(c));
    let nested_idxs: Vec<u32> = nested_class_names.iter().map(|n| st.local(n)).collect();
    // The module name (f101) interns LAST — kotlinc places it at the end of d2.
    let module_idx = tail.module_name.map(|m| st.local(m));

    // Assemble the `Class` message in FIELD order: f1 flags, f3 fq_name, f4 companionObjectName,
    // f6 supertype, f7 nestedClassName (packed repeated int32), f8 ctors, f9 functions, f10 properties,
    // f13 enum entries.
    let mut class = Pb::new();
    // kotlinc writes `flags` only when it differs from the public-final-class default.
    if class_flags != DEFAULT_CLASS_FLAGS {
        class.field_varint(1, class_flags);
    }
    class.field_varint(3, fq as u64);
    if let Some(ci) = companion_idx {
        class.field_varint(4, ci as u64); // Class.companion_object_name = 4
    }
    class.field_message(6, &supertype);
    if !nested_idxs.is_empty() {
        let mut packed = Pb::new();
        for &n in &nested_idxs {
            packed.varint(n as u64);
        }
        class.field_bytes(7, packed.as_bytes()); // Class.nested_class_name = 7 (packed)
    }
    for ctor in &ctor_msgs {
        class.repeated_message(8, ctor); // Class.constructor = 8
    }
    for func in &func_msgs {
        class.repeated_message(9, func); // Class.function = 9
    }
    for prop in &prop_msgs {
        class.repeated_message(10, prop); // Class.property = 10
    }
    for ee in &enum_msgs {
        class.repeated_message(13, ee); // Class.enum_entry = 13
    }
    if let Some((name_id, ty_pb)) = &inline_underlying {
        class.field_varint(17, *name_id as u64); // Class.inlineClassUnderlyingPropertyName = 17
        class.field_message(18, ty_pb); // Class.inlineClassUnderlyingType = 18
    }
    if let Some(mi) = module_idx {
        class.field_varint(101, mi as u64); // JvmProtoBuf.classModuleName = 101
    }

    let stt = st.serialize_types();
    let mut bytes = vec![0x00u8]; // UTF8 mode marker
    let mut prefix = Pb::new();
    prefix.varint(stt.as_bytes().len() as u64); // writeDelimitedTo length prefix
    bytes.extend_from_slice(&prefix.into_bytes());
    bytes.extend_from_slice(stt.as_bytes());
    bytes.extend_from_slice(class.as_bytes());
    (bytes, st.strings)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Ground truth: kotlinc 2.4.0 `package demo; class E` → @Metadata mv=[2,4,0] k=1 xi=48, and this
    // exact d1 protobuf (mUTF-8-decoded to raw bytes) + d2 string table. Drives byte-for-byte parity.
    #[test]
    fn empty_class_metadata_byte_matches_kotlinc() {
        let (d1, d2) = build_class("demo/E", &[], "()V", &[], &[], &[], &ClassTail::default());
        assert_eq!(
            d2,
            vec![
                "Ldemo/E;".to_string(),
                "".to_string(),
                "<init>".to_string(),
                "()V".to_string(),
            ],
            "d2 string table",
        );
        assert_eq!(
            d1,
            vec![
                0x00, 0x0c, 0x0a, 0x02, 0x18, 0x02, 0x0a, 0x02, 0x10, 0x00, 0x0a, 0x02, 0x08, 0x02,
                0x18, 0x00, 0x32, 0x02, 0x30, 0x01, 0x42, 0x07, 0xa2, 0x06, 0x04, 0x08, 0x02, 0x10,
                0x03,
            ],
            "d1 protobuf",
        );
    }

    // Ground truth: kotlinc 2.4.0 `package demo; class C(val x: Int)` — one ctor-param property.
    #[test]
    fn one_property_class_metadata_byte_matches_kotlinc() {
        let (d1, d2) = build_class(
            "demo/C",
            &[("x".into(), Ty::Int)],
            "(I)V",
            &[PropMeta {
                name: "x".into(),
                ty: Ty::Int,
                is_var: false,
                getter: ("getX".into(), "()I".into()),
                setter: None,
            }],
            &[],
            &[],
            &ClassTail::default(),
        );
        assert_eq!(
            d2,
            vec!["Ldemo/C;", "", "x", "", "<init>", "(I)V", "getX", "()I"]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>(),
            "d2 string table",
        );
        assert_eq!(
            d1,
            vec![
                0x00, 0x12, 0x0a, 0x02, 0x18, 0x02, 0x0a, 0x02, 0x10, 0x00, 0x0a, 0x00, 0x0a, 0x02,
                0x10, 0x08, 0x0a, 0x02, 0x08, 0x04, 0x18, 0x00, 0x32, 0x02, 0x30, 0x01, 0x42, 0x0f,
                0x12, 0x06, 0x10, 0x02, 0x1a, 0x02, 0x30, 0x03, 0xa2, 0x06, 0x04, 0x08, 0x04, 0x10,
                0x05, 0x52, 0x11, 0x10, 0x02, 0x1a, 0x02, 0x30, 0x03, 0xa2, 0x06, 0x08, 0x0a, 0x00,
                0x1a, 0x04, 0x08, 0x06, 0x10, 0x07,
            ],
            "d1 protobuf",
        );
    }

    // Ground truth: kotlinc 2.4.0 `package demo; data class Point(val x: Int, var y: String)` — the
    // full data-class shape (6 synthesized methods + IS_DATA flag + a var property).
    #[test]
    fn data_class_metadata_byte_matches_kotlinc() {
        let any_q = Ty::nullable(Ty::obj("kotlin/Any"));
        let methods = vec![
            FnMeta {
                name: "component1".into(),
                params: vec![],
                ret: Ty::Int,
                flags: COMPONENT_FN_FLAGS,
                params_have_defaults: false,
                jvm_sig: None,
                jvm_sig_name: None,
            },
            FnMeta {
                name: "component2".into(),
                params: vec![],
                ret: Ty::String,
                flags: COMPONENT_FN_FLAGS,
                params_have_defaults: false,
                jvm_sig: None,
                jvm_sig_name: None,
            },
            FnMeta {
                name: "copy".into(),
                params: vec![("x".into(), Ty::Int), ("y".into(), Ty::String)],
                ret: Ty::obj("demo/Point"),
                flags: COPY_FN_FLAGS,
                params_have_defaults: true,
                jvm_sig: None,
                jvm_sig_name: None,
            },
            FnMeta {
                name: "equals".into(),
                params: vec![("other".into(), any_q)],
                ret: Ty::Boolean,
                flags: EQUALS_FN_FLAGS,
                params_have_defaults: false,
                jvm_sig: None,
                jvm_sig_name: None,
            },
            FnMeta {
                name: "hashCode".into(),
                params: vec![],
                ret: Ty::Int,
                flags: HASHCODE_TOSTRING_FN_FLAGS,
                params_have_defaults: false,
                jvm_sig: None,
                jvm_sig_name: None,
            },
            FnMeta {
                name: "toString".into(),
                params: vec![],
                ret: Ty::String,
                flags: HASHCODE_TOSTRING_FN_FLAGS,
                params_have_defaults: false,
                jvm_sig: None,
                jvm_sig_name: None,
            },
        ];
        let props = vec![
            PropMeta {
                name: "x".into(),
                ty: Ty::Int,
                is_var: false,
                getter: ("getX".into(), "()I".into()),
                setter: None,
            },
            PropMeta {
                name: "y".into(),
                ty: Ty::String,
                is_var: true,
                getter: ("getY".into(), "()Ljava/lang/String;".into()),
                setter: Some(("setY".into(), "(Ljava/lang/String;)V".into())),
            },
        ];
        let (d1, _d2) = build_class(
            "demo/Point",
            &[("x".into(), Ty::Int), ("y".into(), Ty::String)],
            "(ILjava/lang/String;)V",
            &props,
            &methods,
            &[],
            &ClassTail {
                // public + final + IS_DATA, as `class_metadata_flags` derives for a `data class`.
                flags: 1030,
                ..Default::default()
            },
        );
        assert_eq!(
            d1,
            vec![
                0x00, 0x20, 0x0a, 0x02, 0x18, 0x02, 0x0a, 0x02, 0x10, 0x00, 0x0a, 0x00, 0x0a, 0x02,
                0x10, 0x08, 0x0a, 0x00, 0x0a, 0x02, 0x10, 0x0e, 0x0a, 0x02, 0x08, 0x0c, 0x0a, 0x02,
                0x10, 0x0b, 0x0a, 0x02, 0x08, 0x03, 0x08, 0x86, 0x08, 0x18, 0x00, 0x32, 0x02, 0x30,
                0x01, 0x42, 0x17, 0x12, 0x06, 0x10, 0x02, 0x1a, 0x02, 0x30, 0x03, 0x12, 0x06, 0x10,
                0x04, 0x1a, 0x02, 0x30, 0x05, 0xa2, 0x06, 0x04, 0x08, 0x06, 0x10, 0x07, 0x4a, 0x09,
                0x10, 0x0e, 0x1a, 0x02, 0x30, 0x03, 0x48, 0xc6, 0x03, 0x4a, 0x09, 0x10, 0x0f, 0x1a,
                0x02, 0x30, 0x05, 0x48, 0xc6, 0x03, 0x4a, 0x1d, 0x10, 0x10, 0x1a, 0x02, 0x30, 0x00,
                0x32, 0x08, 0x08, 0x02, 0x10, 0x02, 0x1a, 0x02, 0x30, 0x03, 0x32, 0x08, 0x08, 0x02,
                0x10, 0x04, 0x1a, 0x02, 0x30, 0x05, 0x48, 0xc6, 0x01, 0x4a, 0x14, 0x10, 0x11, 0x1a,
                0x02, 0x30, 0x12, 0x32, 0x08, 0x10, 0x13, 0x1a, 0x04, 0x18, 0x01, 0x30, 0x01, 0x48,
                0xd6, 0x83, 0x04, 0x4a, 0x0a, 0x10, 0x14, 0x1a, 0x02, 0x30, 0x03, 0x48, 0xd6, 0x81,
                0x04, 0x4a, 0x0a, 0x10, 0x15, 0x1a, 0x02, 0x30, 0x05, 0x48, 0xd6, 0x81, 0x04, 0x52,
                0x11, 0x10, 0x02, 0x1a, 0x02, 0x30, 0x03, 0xa2, 0x06, 0x08, 0x0a, 0x00, 0x1a, 0x04,
                0x08, 0x08, 0x10, 0x09, 0x52, 0x1a, 0x10, 0x04, 0x1a, 0x02, 0x30, 0x05, 0x58, 0x86,
                0x0e, 0xa2, 0x06, 0x0e, 0x0a, 0x00, 0x1a, 0x04, 0x08, 0x0a, 0x10, 0x0b, 0x22, 0x04,
                0x08, 0x0c, 0x10, 0x0d,
            ],
            "d1 protobuf",
        );
    }

    // A generic property (`List<String>`) + a defaulted ctor param — the shape real production domain
    // models use. Verified byte-identical to kotlinc 2.4.0 on a real config data class; this pins the
    // pieces it needs: `List` encoded as a `predefinedIndex` builtin (NOT a class-id descriptor), the
    // `Type.argument` (String), and the `DECLARES_DEFAULT_VALUE` ctor-param flag.
    #[test]
    fn generic_property_and_default_ctor_param() {
        let list_string = Ty::obj_args("kotlin/collections/List", &[Ty::String]);
        let (_d1, d2) = build_class(
            "demo/D",
            &[("r".into(), list_string)],
            "(Ljava/util/List;)V",
            &[PropMeta {
                name: "r".into(),
                ty: list_string,
                is_var: false,
                getter: ("getR".into(), "()Ljava/util/List;".into()),
                setter: None,
            }],
            &[],
            &[],
            &ClassTail {
                ctor_param_defaults: &[true],
                ..Default::default()
            },
        );
        // `List` is a builtin (predefinedIndex 32) → an EMPTY d2 slot, never the literal descriptor.
        assert!(
            !d2.iter()
                .any(|s| s == "Ljava/util/List;" || s == "Lkotlin/collections/List;"),
            "List must encode as a builtin predefinedIndex, not a class-id descriptor: {d2:?}",
        );
        // The ctor value parameter carries `DECLARES_DEFAULT_VALUE` (f1=2) — the `08 02` prefix inside
        // the constructor's value_parameter, before its name. Its absence would drop the flag.
        assert!(
            _d1.windows(2).any(|w| w == [0x08, 0x02]),
            "the defaulted ctor param must encode DECLARES_DEFAULT_VALUE",
        );
    }

    // Ground truth: kotlinc 2.4.0 `package demo; class S { fun f(n: Int): Int = n }` — a regular
    // (non-synthesized) member function. A plain public-final member has metadata flags omitted (0).
    #[test]
    fn regular_method_class_metadata_byte_matches_kotlinc() {
        let (d1, _d2) = build_class(
            "demo/S",
            &[],
            "()V",
            &[],
            &[FnMeta::plain(
                "f".into(),
                vec![("n".into(), Ty::Int)],
                Ty::Int,
            )],
            &[],
            &ClassTail::default(),
        );
        assert_eq!(
            d1,
            vec![
                0x00, 0x12, 0x0a, 0x02, 0x18, 0x02, 0x0a, 0x02, 0x10, 0x00, 0x0a, 0x02, 0x08, 0x03,
                0x0a, 0x02, 0x10, 0x08, 0x0a, 0x00, 0x18, 0x00, 0x32, 0x02, 0x30, 0x01, 0x42, 0x07,
                0xa2, 0x06, 0x04, 0x08, 0x02, 0x10, 0x03, 0x4a, 0x0e, 0x10, 0x04, 0x1a, 0x02, 0x30,
                0x05, 0x32, 0x06, 0x10, 0x06, 0x1a, 0x02, 0x30, 0x05,
            ],
            "d1 protobuf",
        );
    }

    // Ground truth: kotlinc 2.4.0 `package demo; class C { companion object }`. The companion object
    // adds `companionObjectName` (f4) + a `nestedClassName` (f7), both referencing `Companion`, interned
    // after the ctor.
    #[test]
    fn companion_object_metadata_byte_matches_kotlinc() {
        let (d1, d2) = build_class(
            "demo/C",
            &[],
            "()V",
            &[],
            &[],
            &[],
            &ClassTail {
                companion: Some("Companion"),
                nested: &["Companion"],
                ..Default::default()
            },
        );
        assert_eq!(
            d2,
            vec!["Ldemo/C;", "", "<init>", "()V", "Companion"]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>(),
            "d2",
        );
        assert_eq!(
            d1,
            vec![
                0x00, 0x0c, 0x0a, 0x02, 0x18, 0x02, 0x0a, 0x02, 0x10, 0x00, 0x0a, 0x02, 0x08, 0x03,
                0x18, 0x00, 0x20, 0x04, 0x32, 0x02, 0x30, 0x01, 0x3a, 0x01, 0x04, 0x42, 0x07, 0xa2,
                0x06, 0x04, 0x08, 0x02, 0x10, 0x03,
            ],
            "d1 protobuf",
        );
    }

    // Ground truth: kotlinc 2.4.0 `class C(val x: Int)` compiled with `-module-name mymod`. Adds
    // `classModuleName` (f101) = the module name, interned last.
    #[test]
    fn module_name_metadata_byte_matches_kotlinc() {
        let (d1, d2) = build_class(
            "demo/C",
            &[("x".into(), Ty::Int)],
            "(I)V",
            &[PropMeta {
                name: "x".into(),
                ty: Ty::Int,
                is_var: false,
                getter: ("getX".into(), "()I".into()),
                setter: None,
            }],
            &[],
            &[],
            &ClassTail {
                module_name: Some("mymod"),
                ..Default::default()
            },
        );
        assert_eq!(
            d2,
            vec!["Ldemo/C;", "", "x", "", "<init>", "(I)V", "getX", "()I", "mymod"]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>(),
            "d2",
        );
        assert_eq!(
            d1,
            vec![
                0x00, 0x12, 0x0a, 0x02, 0x18, 0x02, 0x0a, 0x02, 0x10, 0x00, 0x0a, 0x00, 0x0a, 0x02,
                0x10, 0x08, 0x0a, 0x02, 0x08, 0x05, 0x18, 0x00, 0x32, 0x02, 0x30, 0x01, 0x42, 0x0f,
                0x12, 0x06, 0x10, 0x02, 0x1a, 0x02, 0x30, 0x03, 0xa2, 0x06, 0x04, 0x08, 0x04, 0x10,
                0x05, 0x52, 0x11, 0x10, 0x02, 0x1a, 0x02, 0x30, 0x03, 0xa2, 0x06, 0x08, 0x0a, 0x00,
                0x1a, 0x04, 0x08, 0x06, 0x10, 0x07, 0xa8, 0x06, 0x08,
            ],
            "d1 protobuf",
        );
    }

    // Ground truth: kotlinc 2.4.0 `class C(val x: Int) { constructor() : this(0) }` — a second
    // (secondary) constructor. `Class.constructor` (f8) is repeated; the secondary carries flags 22.
    #[test]
    fn secondary_ctor_metadata_byte_matches_kotlinc() {
        let (d1, d2) = build_class(
            "demo/C",
            &[("x".into(), Ty::Int)],
            "(I)V",
            &[PropMeta {
                name: "x".into(),
                ty: Ty::Int,
                is_var: false,
                getter: ("getX".into(), "()I".into()),
                setter: None,
            }],
            &[],
            &[],
            &ClassTail {
                secondary_ctors: &[CtorMeta {
                    params: &[],
                    desc: "()V",
                    flags: 22,
                }],
                ..Default::default()
            },
        );
        assert_eq!(
            d2,
            vec!["Ldemo/C;", "", "x", "", "<init>", "(I)V", "()V", "getX", "()I"]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>(),
            "d2",
        );
        assert_eq!(
            d1,
            vec![
                0x00, 0x12, 0x0a, 0x02, 0x18, 0x02, 0x0a, 0x02, 0x10, 0x00, 0x0a, 0x00, 0x0a, 0x02,
                0x10, 0x08, 0x0a, 0x02, 0x08, 0x05, 0x18, 0x00, 0x32, 0x02, 0x30, 0x01, 0x42, 0x0f,
                0x12, 0x06, 0x10, 0x02, 0x1a, 0x02, 0x30, 0x03, 0xa2, 0x06, 0x04, 0x08, 0x04, 0x10,
                0x05, 0x42, 0x09, 0x08, 0x16, 0xa2, 0x06, 0x04, 0x08, 0x04, 0x10, 0x06, 0x52, 0x11,
                0x10, 0x02, 0x1a, 0x02, 0x30, 0x03, 0xa2, 0x06, 0x08, 0x0a, 0x00, 0x1a, 0x04, 0x08,
                0x07, 0x10, 0x08,
            ],
            "d1 protobuf",
        );
    }

    #[test]
    fn class_metadata_has_expected_strings() {
        let (_d1, d2) = build_class(
            "demo/Point",
            &[("x".into(), Ty::Int), ("y".into(), Ty::String)],
            "(ILjava/lang/String;)V",
            &[
                PropMeta {
                    name: "x".into(),
                    ty: Ty::Int,
                    is_var: false,
                    getter: ("getX".into(), "()I".into()),
                    setter: None,
                },
                PropMeta {
                    name: "y".into(),
                    ty: Ty::String,
                    is_var: true,
                    getter: ("getY".into(), "()Ljava/lang/String;".into()),
                    setter: Some(("setY".into(), "(Ljava/lang/String;)V".into())),
                },
            ],
            &[],
            &[],
            &ClassTail::default(),
        );
        // The class id descriptor and the JVM signatures must all appear verbatim in d2.
        assert!(d2.contains(&"Ldemo/Point;".to_string()));
        assert!(d2.contains(&"getX".to_string()));
        assert!(d2.contains(&"setY".to_string()));
        assert!(d2.contains(&"(ILjava/lang/String;)V".to_string()));
    }
}
