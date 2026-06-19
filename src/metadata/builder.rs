//! Build the `@kotlin.Metadata` `d1` protobuf payload + `d2` string table for a file facade with
//! top-level functions. Schema/field numbers per `core/metadata/src/metadata.proto`; builtin type
//! names use `predefinedIndex` into `JvmNameResolverBase.PREDEFINED_STRINGS` (see METADATA_NOTES.md).

use std::collections::HashMap;

use crate::metadata::protobuf::Pb;
use crate::types::Ty;

/// One top-level function to describe in the package metadata.
pub struct FnMeta {
    pub name: String,
    pub params: Vec<(String, Ty)>,
    pub ret: Ty,
}

/// `predefinedIndex` of a builtin type's fq-name in `PREDEFINED_STRINGS`.
fn builtin_index(t: Ty) -> Option<u64> {
    Some(match t {
        Ty::Unit => 2,
        Ty::Double => 6,
        Ty::Int => 8,
        Ty::Long => 9,
        Ty::Boolean => 11,
        Ty::String => 14,
        _ => return None,
    })
}

/// Accumulates d2 strings + the parallel `StringTableTypes.Record` list, deduping builtin entries.
#[derive(Default)]
struct StringTable {
    strings: Vec<String>,
    records: Vec<Pb>, // one Record per string index
    builtin_dedup: HashMap<u64, u32>,
}

impl StringTable {
    /// Intern a local (source) string; returns its index. (No dedup — names are distinct in v0.)
    fn local(&mut self, s: &str) -> u32 {
        let i = self.strings.len() as u32;
        self.strings.push(s.to_string());
        self.records.push(Pb::new()); // empty Record => use d2 string verbatim
        i
    }

    /// Intern a builtin fq-name via predefinedIndex; deduped. The d2 slot is empty (`""`).
    fn builtin(&mut self, predefined: u64) -> u32 {
        if let Some(&i) = self.builtin_dedup.get(&predefined) {
            return i;
        }
        let i = self.strings.len() as u32;
        self.strings.push(String::new());
        let mut r = Pb::new();
        r.field_varint(2, predefined); // Record.predefined_index = 2
        self.records.push(r);
        self.builtin_dedup.insert(predefined, i);
        i
    }

    /// A class id from a type descriptor `Lpkg/Name;` via operation `DESC_TO_CLASS_ID` (Record.f3=2).
    fn class_id_from_desc(&mut self, descriptor: &str) -> u32 {
        let i = self.strings.len() as u32;
        self.strings.push(descriptor.to_string());
        let mut r = Pb::new();
        r.field_varint(3, 2); // operation = DESC_TO_CLASS_ID
        self.records.push(r);
        i
    }

    /// Serialize the `StringTableTypes` message (record = field 1, repeated).
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
        Ty::Obj(internal, _) => st.class_id_from_desc(&format!("L{internal};")),
        _ => st.builtin(builtin_index(t).unwrap_or(0)), // 0 = kotlin/Any fallback on erroring code
    };
    p.field_varint(6, class_name as u64); // Type.class_name = 6
    p
}

fn function_pb(st: &mut StringTable, f: &FnMeta) -> Pb {
    let mut p = Pb::new();
    p.field_varint(2, st.local(&f.name) as u64); // Function.name = 2
    let ret = type_pb(st, f.ret);
    p.field_message(3, &ret); // Function.return_type = 3
    for (pname, pty) in &f.params {
        let mut vp = Pb::new();
        vp.field_varint(2, st.local(pname) as u64); // ValueParameter.name = 2
        let ty = type_pb(st, *pty);
        vp.field_message(3, &ty); // ValueParameter.type = 3
        p.repeated_message(6, &vp); // Function.value_parameter = 6
    }
    p
}

/// A top-level property for the package metadata (`Package.property` = field 4).
pub struct PropMeta {
    pub name: String,
    pub ty: Ty,
    pub is_var: bool,
    pub getter: (String, String),
    pub setter: Option<(String, String)>,
}

/// `Package` property flags kotlinc emits for top-level `val`/`var` (public, with accessors).
const PKG_VAL_FLAGS: u64 = 8710;
const PKG_VAR_FLAGS: u64 = 1798;
const DECLARES_DEFAULT_VALUE: u64 = 0; // (unused here; kept for symmetry)

fn jvm_method_sig(st: &mut StringTable, name: &str, desc: &str) -> Pb {
    let mut p = Pb::new();
    p.field_varint(1, st.local(name) as u64); // JvmMethodSignature.name = 1
    p.field_varint(2, st.local(desc) as u64); // JvmMethodSignature.desc = 2
    p
}

fn property_pb(st: &mut StringTable, m: &PropMeta) -> Pb {
    let _ = DECLARES_DEFAULT_VALUE;
    let mut p = Pb::new();
    p.field_varint(2, st.local(&m.name) as u64); // Property.name = 2
    let ret = type_pb(st, m.ty);
    p.field_message(3, &ret); // Property.return_type = 3
    p.field_varint(
        11,
        if m.is_var {
            PKG_VAR_FLAGS
        } else {
            PKG_VAL_FLAGS
        },
    ); // flags
    let mut jvm = Pb::new();
    jvm.field_message(1, &Pb::new()); // field (empty → derived)
    let getter = jvm_method_sig(st, &m.getter.0, &m.getter.1);
    jvm.field_message(3, &getter);
    if let Some((sn, sd)) = &m.setter {
        let setter = jvm_method_sig(st, sn, sd);
        jvm.field_message(4, &setter);
    }
    p.field_message(100, &jvm); // JvmProtoBuf.propertySignature = 100
    p
}

/// Build `(d1 bytes, d2 strings)` for a file facade. `d1 = delimited(StringTableTypes) + Package`.
pub fn build_package(funcs: &[FnMeta], props: &[PropMeta]) -> (Vec<u8>, Vec<String>) {
    let mut st = StringTable::default();
    let mut package = Pb::new();
    for f in funcs {
        let fp = function_pb(&mut st, f);
        package.repeated_message(3, &fp); // Package.function = 3
    }
    for m in props {
        let pp = property_pb(&mut st, m);
        package.repeated_message(4, &pp); // Package.property = 4
    }
    let stt = st.serialize_types();

    // Empirically required leading byte (kotlinc emits it and reads its own output): the metadata
    // payload begins with 0x00 before the delimited StringTableTypes. (Confirmed via the round-trip
    // test — without it kotlinc reports "unresolved reference" for the functions.)
    let mut bytes = vec![0x00u8];
    let mut d1 = Pb::new();
    d1.varint(stt.as_bytes().len() as u64); // writeDelimitedTo: length prefix
    bytes.extend_from_slice(&d1.into_bytes());
    bytes.extend_from_slice(stt.as_bytes());
    bytes.extend_from_slice(package.as_bytes());
    (bytes, st.strings)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reference bytes kotlinc 1.9.24 emits for `fun f(a: Int): Int = a` (METADATA_NOTES.md),
    /// minus the leading `0x00` (an artifact — kotlinc's own reader does parseDelimitedFrom first,
    /// so a real length-0 prefix would make it unreadable). We must match the rest exactly.
    /// Exact bytes kotlinc 1.9.24 emits for `fun f(a: Int): Int = a` (incl. the leading 0x00).
    const REF: &[u8] = &[
        0x00, 0x08, 0x0a, 0x00, 0x0a, 0x02, 0x10, 0x08, 0x0a, 0x00, 0x1a, 0x0e, 0x10, 0x00, 0x1a,
        0x02, 0x30, 0x01, 0x32, 0x06, 0x10, 0x02, 0x1a, 0x02, 0x30, 0x01,
    ];

    #[test]
    fn matches_kotlinc_reference_for_f_int_int() {
        let (d1, d2) = build_package(
            &[FnMeta {
                name: "f".into(),
                params: vec![("a".into(), Ty::Int)],
                ret: Ty::Int,
            }],
            &[],
        );
        assert_eq!(d2, vec!["f".to_string(), "".to_string(), "a".to_string()]);
        assert_eq!(d1, REF, "\n got: {:02x?}\n ref: {:02x?}", d1, REF);
    }

    #[test]
    fn dedups_builtin_types() {
        // return Int + param Int must share one string-table entry (index 1).
        let (_d1, d2) = build_package(
            &[FnMeta {
                name: "g".into(),
                params: vec![("x".into(), Ty::Int)],
                ret: Ty::Int,
            }],
            &[],
        );
        assert_eq!(d2.iter().filter(|s| s.is_empty()).count(), 1);
    }
}
