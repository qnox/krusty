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

/// Member-function descriptor for class metadata (`Class.function` = f9). The JVM name/descriptor
/// are derivable, so no signature extension is emitted (matching kotlinc).
pub struct FnMeta {
    pub name: String,
    pub params: Vec<(String, Ty)>,
    pub ret: Ty,
    /// `Function.flags` (f9): e.g. operator (`componentN`) or the data-class `copy`. 0 ⇒ omitted.
    pub flags: u64,
    /// Mark every value parameter `DECLARES_DEFAULT_VALUE` (so a Kotlin caller may omit it) — used
    /// for the synthesized `copy`.
    pub params_have_defaults: bool,
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
/// `Class.flags` (f1) for a `public final data class` (IS_DATA + public + final). From kotlinc 2.4.0.
pub const DATA_CLASS_FLAGS: u64 = 0x406;
/// `ValueParameter.flags` bit for `DECLARES_DEFAULT_VALUE`.
const DECLARES_DEFAULT_VALUE: u64 = 2;

/// `predefinedIndex` of a builtin fq-name in `JvmNameResolverBase.PREDEFINED_STRINGS`.
fn predefined_index(t: Ty) -> u64 {
    match t {
        Ty::Unit => 2,
        Ty::Double => 6,
        Ty::Int => 8,
        Ty::Long => 9,
        Ty::Boolean => 11,
        Ty::String => 14,
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
        "kotlin/String" => Some(14),
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
    let class_name = match base {
        Ty::Obj(internal, _) => match builtin_name_index(internal) {
            Some(idx) => st.builtin(idx),
            None => st.class_id_from_desc(&format!("L{internal};")),
        },
        _ => st.builtin(predefined_index(base)),
    };
    if nullable {
        p.field_varint(3, 1); // Type.nullable = 3 (written before class_name, matching kotlinc)
    }
    p.field_varint(6, class_name as u64); // Type.class_name = 6
    p
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
#[derive(Default)]
pub struct ClassTail<'a> {
    pub flags: u64,
    pub companion: Option<&'a str>,
    pub nested: &'a [&'a str],
    /// The `-module-name` value → `Class.classModuleName` (f101, a JvmProtoBuf extension). kotlinc
    /// omits it for the default module `main`; infragnite always sets `-module-name`.
    pub module_name: Option<&'a str>,
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

    // f8 = primary constructor. kotlinc emits the ctor's JVM name `<init>` explicitly (not omitted).
    let mut ctor = Pb::new();
    for (pname, pty) in ctor_params {
        let mut vp = Pb::new();
        vp.field_varint(2, st.local(pname) as u64); // Constructor.ValueParameter.name = 2
        let ty = type_pb(&mut st, *pty);
        vp.field_message(3, &ty); // ValueParameter.type = 3
        ctor.repeated_message(2, &vp); // Constructor.value_parameter = 2
    }
    let ctor_sig = jvm_method_sig(&mut st, Some("<init>"), ctor_desc);
    ctor.field_message(100, &ctor_sig); // JvmProtoBuf.constructorSignature = 100

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
            jvm.field_message(1, &Pb::new()); // field (empty → derive backing field)
            let getter = jvm_method_sig(&mut st, Some(&p.getter.0), &p.getter.1);
            jvm.field_message(3, &getter); // JvmPropertySignature.getter = 3
            if let Some((sn, sd)) = &p.setter {
                let setter = jvm_method_sig(&mut st, Some(sn), sd);
                jvm.field_message(4, &setter); // JvmPropertySignature.setter = 4
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

    // Companion + nested class names intern LAST (kotlinc's d2 places them after all members).
    let companion_idx = companion_name.map(|c| st.local(c));
    let nested_idxs: Vec<u32> = nested_class_names.iter().map(|n| st.local(n)).collect();
    // The module name (f101) interns LAST — kotlinc places it at the end of d2.
    let module_idx = tail.module_name.map(|m| st.local(m));

    // Assemble the `Class` message in FIELD order: f1 flags, f3 fq_name, f4 companionObjectName,
    // f6 supertype, f7 nestedClassName (packed repeated int32), f8 ctors, f9 functions, f10 properties,
    // f13 enum entries.
    let mut class = Pb::new();
    if class_flags != 0 {
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
    class.repeated_message(8, &ctor);
    for func in &func_msgs {
        class.repeated_message(9, func); // Class.function = 9
    }
    for prop in &prop_msgs {
        class.repeated_message(10, prop); // Class.property = 10
    }
    for ee in &enum_msgs {
        class.repeated_message(13, ee); // Class.enum_entry = 13
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
            },
            FnMeta {
                name: "component2".into(),
                params: vec![],
                ret: Ty::String,
                flags: COMPONENT_FN_FLAGS,
                params_have_defaults: false,
            },
            FnMeta {
                name: "copy".into(),
                params: vec![("x".into(), Ty::Int), ("y".into(), Ty::String)],
                ret: Ty::obj("demo/Point"),
                flags: COPY_FN_FLAGS,
                params_have_defaults: true,
            },
            FnMeta {
                name: "equals".into(),
                params: vec![("other".into(), any_q)],
                ret: Ty::Boolean,
                flags: EQUALS_FN_FLAGS,
                params_have_defaults: false,
            },
            FnMeta {
                name: "hashCode".into(),
                params: vec![],
                ret: Ty::Int,
                flags: HASHCODE_TOSTRING_FN_FLAGS,
                params_have_defaults: false,
            },
            FnMeta {
                name: "toString".into(),
                params: vec![],
                ret: Ty::String,
                flags: HASHCODE_TOSTRING_FN_FLAGS,
                params_have_defaults: false,
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
                flags: DATA_CLASS_FLAGS,
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
