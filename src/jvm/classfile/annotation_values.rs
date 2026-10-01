//! The `element_value` encoding of annotations (JVMS §4.7.16.1), interning each constant as it is
//! written.

use super::{u2, ClassWriter};
use crate::kt_string::KtString;
use crate::types::TypeName;

/// The physical name an annotation type contributes to `InnerClasses`, and its descriptor.
/// Both strings are owned by the class writer, so user-defined annotation identities do not enter
/// a process-lifetime name or descriptor cache.
fn annotation_class_text(name: TypeName) -> (String, String) {
    let physical = crate::jvm::names::owned_classfile_internal_name(name);
    let mut descriptor = String::with_capacity(physical.len() + 2);
    descriptor.push('L');
    descriptor.push_str(&physical);
    descriptor.push(';');
    (physical, descriptor)
}

impl ClassWriter {
    fn record_annotation_class(&mut self, name: TypeName) -> u16 {
        let (internal, descriptor) = annotation_class_text(name);
        let utf8 = self.cp.utf8(&descriptor);
        self.annotation_class_refs.insert(internal);
        utf8
    }

    pub(super) fn ev_int(&mut self, out: &mut Vec<u8>, v: i32) {
        out.push(b'I');
        let idx = self.cp.integer(v);
        u2(out, idx);
    }
    pub(super) fn ev_str(&mut self, out: &mut Vec<u8>, s: &str) {
        out.push(b's');
        let idx = self.cp.utf8(s);
        u2(out, idx);
    }
    /// An annotation `element_value` holding a Kotlin string VALUE (see [`KtString`]).
    pub(super) fn ev_str_kt(&mut self, out: &mut Vec<u8>, s: &KtString) {
        out.push(b's');
        let idx = self.cp.utf8_kt(s);
        u2(out, idx);
    }
    pub(super) fn ev_int_array(&mut self, out: &mut Vec<u8>, vs: &[i32]) {
        out.push(b'[');
        u2(out, vs.len() as u16);
        for &v in vs {
            self.ev_int(out, v);
        }
    }
    pub(super) fn ev_str_array(&mut self, out: &mut Vec<u8>, ss: &[String]) {
        out.push(b'[');
        u2(out, ss.len() as u16);
        for s in ss {
            self.ev_str(out, s);
        }
    }

    /// Encode one `element_value` (JVMS §4.7.16.1) for a resolved annotation argument.
    pub(super) fn ev_value(&mut self, out: &mut Vec<u8>, v: &crate::ir::AnnoValue) {
        use crate::ir::{AnnoValue, IrConst};
        match v {
            AnnoValue::Const(c) => match c {
                IrConst::Boolean(b) => {
                    out.push(b'Z');
                    let i = self.cp.integer(*b as i32);
                    u2(out, i);
                }
                // An unsigned annotation argument is emitted under the signed primitive its
                // value class wraps, which is the element type the annotation's own descriptor
                // names.
                IrConst::UByte(x) => {
                    out.push(b'B');
                    let i = self.cp.integer(i32::from(*x as i8));
                    u2(out, i);
                }
                IrConst::UShort(x) => {
                    out.push(b'S');
                    let i = self.cp.integer(i32::from(*x as i16));
                    u2(out, i);
                }
                IrConst::UInt(x) => {
                    out.push(b'I');
                    let i = self.cp.integer(*x as i32);
                    u2(out, i);
                }
                IrConst::ULong(x) => {
                    out.push(b'J');
                    let i = self.cp.long(*x as i64);
                    u2(out, i);
                }
                IrConst::Byte(x) => {
                    out.push(b'B');
                    let i = self.cp.integer(*x as i32);
                    u2(out, i);
                }
                IrConst::Short(x) => {
                    out.push(b'S');
                    let i = self.cp.integer(*x as i32);
                    u2(out, i);
                }
                IrConst::Char(x) => {
                    out.push(b'C');
                    let i = self.cp.integer(*x as i32);
                    u2(out, i);
                }
                IrConst::Int(x) => {
                    out.push(b'I');
                    let i = self.cp.integer(*x);
                    u2(out, i);
                }
                IrConst::Long(x) => {
                    out.push(b'J');
                    let i = self.cp.long(*x);
                    u2(out, i);
                }
                IrConst::Float(x) => {
                    out.push(b'F');
                    let i = self.cp.float(*x);
                    u2(out, i);
                }
                IrConst::Double(x) => {
                    out.push(b'D');
                    let i = self.cp.double(*x);
                    u2(out, i);
                }
                IrConst::String(s) => self.ev_str_kt(out, s),
                IrConst::Null => self.ev_str(out, ""),
            },
            AnnoValue::Enum(ty, name) => {
                out.push(b'e');
                // An enum value's TYPE is a reference too: kotlinc records an `InnerClasses` entry
                // for a nested enum used purely as an annotation argument (verified on 2.4.10).
                let ti = self.record_annotation_class(*ty);
                u2(out, ti);
                let ni = self.cp.utf8(name);
                u2(out, ni);
            }
            AnnoValue::Class(internal) => {
                out.push(b'c');
                let mapped = crate::jvm::jvm_class_map::to_jvm_type_name(*internal);
                let ci = self.record_annotation_class(mapped);
                u2(out, ci);
            }
            AnnoValue::Annotation(a) => {
                out.push(b'@');
                self.ev_annotation(out, a);
            }
            AnnoValue::Array(items) => {
                out.push(b'[');
                u2(out, items.len() as u16);
                for it in items {
                    self.ev_value(out, it);
                }
            }
        }
    }

    /// Encode an `annotation` structure: the type descriptor index + its `element_value_pairs`.
    pub(super) fn ev_annotation(&mut self, out: &mut Vec<u8>, a: &crate::ir::AppliedAnnotation) {
        let ti = self.record_annotation_class(a.internal);
        u2(out, ti);
        u2(out, a.values.len() as u16);
        for (name, v) in &a.values {
            let ni = self.cp.utf8(name);
            u2(out, ni);
            self.ev_value(out, v);
        }
    }
}

#[test]
fn repeated_annotation_reuses_one_descriptor_slot() {
    let mut writer = super::ClassWriter::new("Use", "java/lang/Object");
    let annotation = crate::ir::AppliedAnnotation {
        internal: crate::types::type_name("sample/anno6044/Marker"),
        values: Vec::new(),
    };
    let mut out = Vec::new();
    writer.ev_annotation(&mut out, &annotation);
    let entries = writer.cp.entries.len();
    let slot = writer.cp.lookup_utf8("Lsample/anno6044/Marker;");
    writer.ev_annotation(&mut out, &annotation);
    assert_eq!(writer.cp.entries.len(), entries);
    assert_eq!(writer.cp.lookup_utf8("Lsample/anno6044/Marker;"), slot);
    assert!(writer
        .annotation_class_refs
        .contains("sample/anno6044/Marker"));
}

#[test]
fn dotted_annotation_name_uses_its_physical_classfile_descriptor() {
    let dotted =
        crate::types::type_name_child(crate::types::type_name("sample/anno6044"), "Outer.Inner");
    let (internal, descriptor) = annotation_class_text(dotted);
    assert_eq!(internal, "sample/anno6044/Outer$Inner");
    assert_eq!(descriptor, "Lsample/anno6044/Outer$Inner;");
}

#[test]
fn annotation_values_record_physical_enum_class_and_nested_descriptors() {
    use crate::ir::AnnoValue;

    let mut writer = super::ClassWriter::new("Use", "java/lang/Object");
    let annotation = crate::ir::AppliedAnnotation {
        internal: crate::types::type_name("sample/anno6044/Marker"),
        values: vec![
            (
                "shade".to_string(),
                AnnoValue::Enum(
                    crate::types::type_name_child(
                        crate::types::type_name("sample/anno6044"),
                        "Color.Shade",
                    ),
                    "DARK".to_string(),
                ),
            ),
            (
                "token".to_string(),
                AnnoValue::Class(crate::types::type_name("sample/anno6044/Token")),
            ),
            (
                "nested".to_string(),
                AnnoValue::Annotation(crate::ir::AppliedAnnotation {
                    internal: crate::types::type_name_child(
                        crate::types::type_name("sample/anno6044"),
                        "Outer.Inner",
                    ),
                    values: Vec::new(),
                }),
            ),
        ],
    };
    let mut out = Vec::new();
    writer.ev_annotation(&mut out, &annotation);
    for descriptor in [
        "Lsample/anno6044/Marker;",
        "Lsample/anno6044/Color$Shade;",
        "Lsample/anno6044/Token;",
        "Lsample/anno6044/Outer$Inner;",
    ] {
        assert!(writer.cp.lookup_utf8(descriptor).is_some(), "{descriptor}");
    }
    for internal in [
        "sample/anno6044/Marker",
        "sample/anno6044/Color$Shade",
        "sample/anno6044/Token",
        "sample/anno6044/Outer$Inner",
    ] {
        assert!(
            writer.annotation_class_refs.contains(internal),
            "{internal}"
        );
    }
}

#[test]
fn distinct_annotation_writers_keep_only_writer_local_text() {
    for index in 0..24 {
        let left_internal = format!("sample/anno6044/Left{index}");
        let right_internal = format!("sample/anno6044/Right{index}");
        let mut left = super::ClassWriter::new("LeftUse", "java/lang/Object");
        let mut right = super::ClassWriter::new("RightUse", "java/lang/Object");
        let mut out = Vec::new();
        left.ev_annotation(
            &mut out,
            &crate::ir::AppliedAnnotation {
                internal: crate::types::type_name(&left_internal),
                values: Vec::new(),
            },
        );
        right.ev_annotation(
            &mut out,
            &crate::ir::AppliedAnnotation {
                internal: crate::types::type_name(&right_internal),
                values: Vec::new(),
            },
        );

        assert_eq!(left.cp.lookup_utf8(&format!("L{left_internal};")), Some(5));
        assert_eq!(left.annotation_class_refs.len(), 1);
        assert!(left.annotation_class_refs.contains(&left_internal));
        assert!(!left.annotation_class_refs.contains(&right_internal));
        assert_eq!(
            right.cp.lookup_utf8(&format!("L{right_internal};")),
            Some(5)
        );
        assert_eq!(right.annotation_class_refs.len(), 1);
        assert!(right.annotation_class_refs.contains(&right_internal));
        assert!(!right.annotation_class_refs.contains(&left_internal));

        if index % 2 == 0 {
            drop(left);
            drop(right);
        } else {
            drop(right);
            drop(left);
        }
    }
}
