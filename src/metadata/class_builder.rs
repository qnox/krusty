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
        FnMeta { name, params, ret, flags: 0, params_have_defaults: false }
    }
}

/// `Function.flags` kotlinc emits for a data class's synthesized `componentN` (public final
/// operator member) and `copy` (public final member). Reverse-engineered from kotlinc 1.9.24.
pub const COMPONENT_FN_FLAGS: u64 = 454;
pub const COPY_FN_FLAGS: u64 = 198;
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
}

impl StringTable {
    /// A verbatim source string (empty `Record` → use the d2 entry as-is).
    fn local(&mut self, s: &str) -> u32 {
        let i = self.strings.len() as u32;
        self.strings.push(s.to_string());
        self.records.push(Pb::new());
        i
    }
    /// A builtin fq-name via predefinedIndex (Record.f2). The d2 slot is empty.
    fn builtin(&mut self, predefined: u64) -> u32 {
        let i = self.strings.len() as u32;
        self.strings.push(String::new());
        let mut r = Pb::new();
        r.field_varint(2, predefined);
        self.records.push(r);
        i
    }
    /// A class id from a type descriptor `Lpkg/Name;` via operation DESC_TO_CLASS_ID (Record.f3=2).
    fn class_id_from_desc(&mut self, descriptor: &str) -> u32 {
        let i = self.strings.len() as u32;
        self.strings.push(descriptor.to_string());
        let mut r = Pb::new();
        r.field_varint(3, 2); // operation = DESC_TO_CLASS_ID
        self.records.push(r);
        i
    }
    fn serialize_types(&self) -> Pb {
        let mut p = Pb::new();
        for r in &self.records {
            p.repeated_message(1, r);
        }
        p
    }
}

fn type_pb(st: &mut StringTable, t: Ty) -> Pb {
    let mut p = Pb::new();
    let class_name = match t {
        Ty::Obj(internal) => st.class_id_from_desc(&format!("L{internal};")),
        _ => st.builtin(predefined_index(t)),
    };
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
pub fn build_class(
    class_internal: &str,
    ctor_params: &[(String, Ty)],
    ctor_desc: &str,
    props: &[PropMeta],
    methods: &[FnMeta],
    enum_entries: &[String],
    class_flags: u64,
) -> (Vec<u8>, Vec<String>) {
    let mut st = StringTable::default();
    let mut class = Pb::new();

    // f1 = flags (omitted ⇒ 0 for a plain class; IS_DATA / object bits otherwise).
    if class_flags != 0 {
        class.field_varint(1, class_flags);
    }

    // f3 = fq_name: a class-id derived from the `L...;` descriptor.
    let fq = st.class_id_from_desc(&format!("L{class_internal};"));
    class.field_varint(3, fq as u64);

    // f6 = supertype kotlin/Any.
    let mut any = Pb::new();
    any.field_varint(6, st.builtin(ANY_PREDEFINED) as u64);
    class.field_message(6, &any);

    // f8 = primary constructor.
    let mut ctor = Pb::new();
    for (pname, pty) in ctor_params {
        let mut vp = Pb::new();
        vp.field_varint(2, st.local(pname) as u64); // Constructor.ValueParameter.name = 2
        let ty = type_pb(&mut st, *pty);
        vp.field_message(3, &ty); // ValueParameter.type = 3
        ctor.repeated_message(2, &vp); // Constructor.value_parameter = 2
    }
    let ctor_sig = jvm_method_sig(&mut st, None, ctor_desc); // name omitted → <init>
    ctor.field_message(100, &ctor_sig); // JvmProtoBuf.constructorSignature = 100
    class.repeated_message(8, &ctor);

    // f9 = member functions (name f2, return_type f3, value_parameter f6, flags f9; JVM sig derivable).
    for m in methods {
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
        class.repeated_message(9, &func); // Class.function = 9
    }

    // f10 = properties.
    for p in props {
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
        class.repeated_message(10, &prop);
    }

    // f13 = enum entries (`EnumEntry { name = f1 }`).
    for entry in enum_entries {
        let mut ee = Pb::new();
        ee.field_varint(1, st.local(entry) as u64);
        class.repeated_message(13, &ee);
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

    #[test]
    fn class_metadata_has_expected_strings() {
        let (_d1, d2) = build_class(
            "demo/Point",
            &[("x".into(), Ty::Int), ("y".into(), Ty::String)],
            "(ILjava/lang/String;)V",
            &[
                PropMeta { name: "x".into(), ty: Ty::Int, is_var: false, getter: ("getX".into(), "()I".into()), setter: None },
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
            0,
        );
        // The class id descriptor and the JVM signatures must all appear verbatim in d2.
        assert!(d2.contains(&"Ldemo/Point;".to_string()));
        assert!(d2.contains(&"getX".to_string()));
        assert!(d2.contains(&"setY".to_string()));
        assert!(d2.contains(&"(ILjava/lang/String;)V".to_string()));
    }
}
