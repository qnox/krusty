//! Minimal JVM `.class` reader: parses the constant pool, this/super class, fields and methods to
//! recover **public signatures**. This is how krusty resolves Java/JDK dependencies — read the
//! callee's `.class`, learn its method descriptors — instead of hardcoding intrinsics (Phase 6,
//! "java supported"). It reads enough to drive interop, not the full attribute set.
//!
//! Also reads the `@kotlin.Metadata` annotation (RuntimeVisibleAnnotations) to extract the `d2`
//! string table, which contains type-alias targets used by `classpath.rs` for type resolution.

pub const ACC_PUBLIC: u16 = 0x0001;
pub const ACC_STATIC: u16 = 0x0008;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MethodSig {
    pub access: u16,
    pub name: String,
    pub descriptor: String,
}

impl MethodSig {
    pub fn is_public(&self) -> bool {
        self.access & ACC_PUBLIC != 0
    }
    pub fn is_static(&self) -> bool {
        self.access & ACC_STATIC != 0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldSig {
    pub access: u16,
    pub name: String,
    pub descriptor: String,
}

#[derive(Clone, Debug)]
pub struct ClassInfo {
    pub major: u16,
    /// class access flags (`ACC_PUBLIC`, …)
    pub access: u16,
    /// internal name, e.g. `java/lang/String`
    pub this_class: String,
    pub super_class: Option<String>,
    pub fields: Vec<FieldSig>,
    pub methods: Vec<MethodSig>,
    /// Strings from the `@kotlin.Metadata` `d2` annotation element, if present.
    pub kotlin_d2: Vec<String>,
}

impl ClassInfo {
    pub fn is_public(&self) -> bool {
        self.access & ACC_PUBLIC != 0
    }

    pub fn method(&self, name: &str, descriptor: &str) -> Option<&MethodSig> {
        self.methods.iter().find(|m| m.name == name && m.descriptor == descriptor)
    }
    /// All overloads of a method name (to resolve a call when only arg types are known).
    pub fn methods_named(&self, name: &str) -> Vec<&MethodSig> {
        self.methods.iter().filter(|m| m.name == name).collect()
    }
}

#[derive(Debug)]
pub enum ReadError {
    NotAClass,
    Truncated,
    BadConstant(u8),
}

/// Constant-pool entry (only the variants we need to resolve names/descriptors).
#[allow(dead_code)] // NameAndType payload retained for completeness / future Methodref resolution
enum C {
    Utf8(String),
    Class(u16),        // name_index
    NameAndType(u16, u16),
    Other,
}

pub fn parse_class(bytes: &[u8]) -> Result<ClassInfo, ReadError> {
    let mut r = Reader { b: bytes, i: 0 };
    if r.u4()? != 0xCAFEBABE {
        return Err(ReadError::NotAClass);
    }
    let _minor = r.u2()?;
    let major = r.u2()?;
    let cp_count = r.u2()? as usize;
    let mut cp: Vec<C> = Vec::with_capacity(cp_count);
    cp.push(C::Other); // index 0 unused
    let mut idx = 1;
    while idx < cp_count {
        let tag = r.u1()?;
        let entry = match tag {
            1 => {
                let len = r.u2()? as usize;
                let s = decode_modified_utf8(r.take(len)?);
                C::Utf8(s)
            }
            7 => C::Class(r.u2()?),
            12 => C::NameAndType(r.u2()?, r.u2()?),
            9 | 10 | 11 | 17 | 18 => { r.u2()?; r.u2()?; C::Other }
            8 | 16 | 19 | 20 => { r.u2()?; C::Other }
            3 | 4 => { r.u4()?; C::Other }
            5 | 6 => { r.u4()?; r.u4()?; C::Other } // long/double: 2 slots
            15 => { r.u1()?; r.u2()?; C::Other }
            _ => return Err(ReadError::BadConstant(tag)),
        };
        let two_slots = matches!(tag, 5 | 6);
        cp.push(entry);
        idx += 1;
        if two_slots {
            cp.push(C::Other);
            idx += 1;
        }
    }

    let utf8 = |i: u16| -> String {
        match cp.get(i as usize) {
            Some(C::Utf8(s)) => s.clone(),
            _ => String::new(),
        }
    };
    let class_name = |i: u16| -> String {
        match cp.get(i as usize) {
            Some(C::Class(n)) => utf8(*n),
            _ => String::new(),
        }
    };

    let access = r.u2()?;
    let this_class = class_name(r.u2()?);
    let super_idx = r.u2()?;
    let super_class = if super_idx == 0 { None } else { Some(class_name(super_idx)) };

    let ifaces = r.u2()?;
    for _ in 0..ifaces {
        r.u2()?;
    }

    let read_members = |r: &mut Reader| -> Result<Vec<(u16, String, String)>, ReadError> {
        let n = r.u2()?;
        let mut v = Vec::new();
        for _ in 0..n {
            let access = r.u2()?;
            let name = utf8(r.u2()?);
            let desc = utf8(r.u2()?);
            skip_attributes(r)?;
            v.push((access, name, desc));
        }
        Ok(v)
    };

    let fields = read_members(&mut r)?
        .into_iter()
        .map(|(access, name, descriptor)| FieldSig { access, name, descriptor })
        .collect();
    let methods = read_members(&mut r)?
        .into_iter()
        .map(|(access, name, descriptor)| MethodSig { access, name, descriptor })
        .collect();

    // Read class-level attributes to find @kotlin.Metadata → d2 array.
    let kotlin_d2 = read_kotlin_d2(&mut r, &cp).unwrap_or_default();

    Ok(ClassInfo { major, access, this_class, super_class, fields, methods, kotlin_d2 })
}

/// Parse class-level attributes looking for RuntimeVisibleAnnotations → @kotlin/Metadata → d2.
fn read_kotlin_d2(r: &mut Reader, cp: &[C]) -> Option<Vec<String>> {
    let utf8 = |i: u16| -> &str {
        match cp.get(i as usize) {
            Some(C::Utf8(s)) => s.as_str(),
            _ => "",
        }
    };

    let n_attrs = r.u2().ok()?;
    for _ in 0..n_attrs {
        let name = utf8(r.u2().ok()?);
        let len = r.u4().ok()? as usize;
        if name != "RuntimeVisibleAnnotations" {
            r.take(len).ok()?;
            continue;
        }
        // Parse annotations: find the one with type == "Lkotlin/Metadata;"
        let n_ann = r.u2().ok()?;
        for _ in 0..n_ann {
            let ann_type = utf8(r.u2().ok()?);
            let is_kotlin_meta = ann_type == "Lkotlin/Metadata;";
            let n_pairs = r.u2().ok()?;
            for _ in 0..n_pairs {
                let elem_name = utf8(r.u2().ok()?);
                let want_d2 = is_kotlin_meta && elem_name == "d2";
                match skip_element_value_extract_string_array(r, cp, want_d2) {
                    Ok(Some(strings)) => return Some(strings),
                    Ok(None) => {}
                    Err(_) => return None,
                }
            }
        }
    }
    None
}

/// Skip or extract an element_value. If `extract` is true and the value is a string array,
/// return the strings; otherwise return None.
fn skip_element_value_extract_string_array(
    r: &mut Reader,
    cp: &[C],
    extract: bool,
) -> Result<Option<Vec<String>>, ReadError> {
    let utf8 = |i: u16| -> String {
        match cp.get(i as usize) {
            Some(C::Utf8(s)) => s.clone(),
            _ => String::new(),
        }
    };

    let tag = r.u1()? as char;
    match tag {
        'B' | 'C' | 'D' | 'F' | 'I' | 'J' | 'S' | 'Z' | 's' | 'c' => { r.u2()?; }
        'e' => { r.u2()?; r.u2()?; }
        '@' => {
            r.u2()?; // annotation type
            let n = r.u2()?;
            for _ in 0..n {
                r.u2()?; // element name
                skip_element_value_extract_string_array(r, cp, false)?;
            }
        }
        '[' => {
            let n = r.u2()? as usize;
            if extract {
                let mut result = Vec::with_capacity(n);
                for _ in 0..n {
                    let t = r.u1()? as char;
                    let s = r.u2()?;
                    if t == 's' {
                        result.push(utf8(s));
                    }
                }
                return Ok(Some(result));
            } else {
                for _ in 0..n {
                    skip_element_value_extract_string_array(r, cp, false)?;
                }
            }
        }
        _ => {} // unknown tag — best effort, may corrupt position but we handle errors
    }
    Ok(None)
}

fn skip_attributes(r: &mut Reader) -> Result<(), ReadError> {
    let n = r.u2()?;
    for _ in 0..n {
        r.u2()?; // name index
        let len = r.u4()? as usize;
        r.take(len)?;
    }
    Ok(())
}

struct Reader<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Reader<'a> {
    fn u1(&mut self) -> Result<u8, ReadError> {
        let v = *self.b.get(self.i).ok_or(ReadError::Truncated)?;
        self.i += 1;
        Ok(v)
    }
    fn u2(&mut self) -> Result<u16, ReadError> {
        Ok(((self.u1()? as u16) << 8) | self.u1()? as u16)
    }
    fn u4(&mut self) -> Result<u32, ReadError> {
        Ok(((self.u2()? as u32) << 16) | self.u2()? as u32)
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], ReadError> {
        let end = self.i.checked_add(n).ok_or(ReadError::Truncated)?;
        let s = self.b.get(self.i..end).ok_or(ReadError::Truncated)?;
        self.i = end;
        Ok(s)
    }
}

/// Decode JVM modified UTF-8 (handles `C0 80` → U+0000 and 2/3-byte sequences).
fn decode_modified_utf8(bytes: &[u8]) -> String {
    let mut s = String::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b & 0x80 == 0 {
            s.push(b as char);
            i += 1;
        } else if b & 0xe0 == 0xc0 && i + 1 < bytes.len() {
            let c = (((b & 0x1f) as u32) << 6) | (bytes[i + 1] & 0x3f) as u32;
            s.push(char::from_u32(c).unwrap_or('\u{fffd}'));
            i += 2;
        } else if b & 0xf0 == 0xe0 && i + 2 < bytes.len() {
            let c = (((b & 0x0f) as u32) << 12) | (((bytes[i + 1] & 0x3f) as u32) << 6) | (bytes[i + 2] & 0x3f) as u32;
            s.push(char::from_u32(c).unwrap_or('\u{fffd}'));
            i += 3;
        } else {
            s.push('\u{fffd}');
            i += 1;
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jvm::classfile::*;

    #[test]
    fn reads_krusty_emitted_class_roundtrip() {
        // Emit a class with the writer, then read it back and check the signature survives.
        let mut cw = ClassWriter::new("demo/RKt", "java/lang/Object");
        let mut code = CodeBuilder::new(2);
        code.iload(0);
        code.iload(1);
        code.iadd();
        code.ireturn();
        cw.add_method(0x0001 | 0x0008 | 0x0010, "add", "(II)I", &code);
        let bytes = cw.finish();
        let ci = parse_class(&bytes).unwrap();
        assert_eq!(ci.this_class, "demo/RKt");
        assert_eq!(ci.methods.len(), 1);
        assert_eq!(ci.methods[0].name, "add");
        assert_eq!(ci.methods[0].descriptor, "(II)I");
    }
}
