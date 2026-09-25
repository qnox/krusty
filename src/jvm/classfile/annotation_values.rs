//! The `element_value` encoding of annotations (JVMS §4.7.16.1), interning each constant as it is
//! written.

use super::{u2, ClassWriter};
use crate::kt_string::KtString;

impl ClassWriter {
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
                let ty = ty.render();
                // An enum value's TYPE is a reference too: kotlinc records an `InnerClasses` entry
                // for a nested enum used purely as an annotation argument (verified on 2.4.10).
                self.annotation_class_refs.insert(ty.clone());
                let ti = self.cp.utf8(&format!("L{ty};"));
                u2(out, ti);
                let ni = self.cp.utf8(name);
                u2(out, ni);
            }
            AnnoValue::Class(internal) => {
                out.push(b'c');
                let internal = crate::jvm::jvm_class_map::to_jvm_type_name(*internal).render();
                self.annotation_class_refs.insert(internal.clone());
                let ci = self.cp.utf8(&format!("L{internal};"));
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
        let internal = a.internal.render();
        self.annotation_class_refs.insert(internal.clone());
        let ti = self.cp.utf8(&format!("L{internal};"));
        u2(out, ti);
        u2(out, a.values.len() as u16);
        for (name, v) in &a.values {
            let ni = self.cp.utf8(name);
            u2(out, ni);
            self.ev_value(out, v);
        }
    }
}
