//! Inline-function expansion (JVM): splice a classpath `inline` function's compiled body into a call
//! site. The body's bytecode references *its own* class's constant pool, so every pool index in it
//! must be **relocated** into the target class's pool — that relocation is the core primitive here.
//! Built on the lazily-read [`MethodCode`](super::classreader::MethodCode); the instruction walk,
//! local remapping, and `reifiedOperationMarker` handling layer on top in later phases.

use super::classfile::ClassWriter;
use super::classreader::C;

fn utf8(cp: &[C], i: u16) -> Option<&str> {
    match cp.get(i as usize)? {
        C::Utf8(s) => Some(s),
        _ => None,
    }
}

fn class_name(cp: &[C], i: u16) -> Option<&str> {
    match cp.get(i as usize)? {
        C::Class(n) => utf8(cp, *n),
        _ => None,
    }
}

fn name_and_type(cp: &[C], i: u16) -> Option<(&str, &str)> {
    match cp.get(i as usize)? {
        C::NameAndType(n, d) => Some((utf8(cp, *n)?, utf8(cp, *d)?)),
        _ => None,
    }
}

/// Re-intern the source constant-pool entry at `idx` (from the inline body's defining class, `src_cp`)
/// into the target class's pool (`cw`), returning the new pool index. Resolving each entry to its
/// semantic form (class/method/field names, descriptors, constant values) and re-interning is what
/// lets a body compiled against one class run inside another. `None` for an entry kind not yet
/// relocatable (`invokedynamic`/method handles — those need bootstrap-method relocation too).
pub fn relocate_const(src_cp: &[C], idx: u16, cw: &mut ClassWriter) -> Option<u16> {
    match src_cp.get(idx as usize)? {
        C::Class(n) => Some(cw.class_ref(utf8(src_cp, *n)?)),
        C::String(u) => Some(cw.const_string(utf8(src_cp, *u)?)),
        C::Integer(v) => Some(cw.const_int(*v)),
        C::Float(b) => Some(cw.const_float(f32::from_bits(*b))),
        C::Long(v) => Some(cw.const_long(*v)),
        C::Double(b) => Some(cw.const_double(f64::from_bits(*b))),
        C::Methodref(c, nt) => {
            let cn = class_name(src_cp, *c)?.to_string();
            let (n, d) = name_and_type(src_cp, *nt)?;
            Some(cw.methodref(&cn, &n.to_string(), &d.to_string()))
        }
        C::Fieldref(c, nt) => {
            let cn = class_name(src_cp, *c)?.to_string();
            let (n, d) = name_and_type(src_cp, *nt)?;
            Some(cw.fieldref(&cn, &n.to_string(), &d.to_string()))
        }
        C::InterfaceMethodref(c, nt) => {
            let cn = class_name(src_cp, *c)?.to_string();
            let (n, d) = name_and_type(src_cp, *nt)?;
            Some(cw.interface_methodref(&cn, &n.to_string(), &d.to_string()))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relocates_pool_entries() {
        // A miniature source pool: Object.hashCode()I as a Methodref, plus a String and an Integer.
        let src_cp = vec![
            C::Other,
            C::Utf8("java/lang/Object".into()), // 1
            C::Class(1),                         // 2
            C::Utf8("hashCode".into()),          // 3
            C::Utf8("()I".into()),               // 4
            C::NameAndType(3, 4),                // 5
            C::Methodref(2, 5),                  // 6
            C::Utf8("hi".into()),                // 7
            C::String(7),                        // 8
            C::Integer(42),                      // 9
        ];
        let mut cw = ClassWriter::new("Target", "java/lang/Object");
        let m = relocate_const(&src_cp, 6, &mut cw).expect("methodref");
        let s = relocate_const(&src_cp, 8, &mut cw).expect("string");
        let n = relocate_const(&src_cp, 9, &mut cw).expect("integer");
        assert!(m > 0 && s > 0 && n > 0);
        // Interning is idempotent: relocating the same entry again yields the same index.
        assert_eq!(m, relocate_const(&src_cp, 6, &mut cw).unwrap());
        // An unrelocatable kind (a bare NameAndType / Utf8) returns None.
        assert!(relocate_const(&src_cp, 5, &mut cw).is_none());
        assert!(relocate_const(&src_cp, 1, &mut cw).is_none());
    }
}
