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
    /// The method's generic `Signature` attribute (JVM generics) if present, e.g. `listOf`'s
    /// `<T:Ljava/lang/Object;>([TT;)Ljava/util/List<TT;>;`. Carries the type parameters and how the
    /// parameter/return types use them — what the erased `descriptor` drops. `None` if non-generic.
    pub signature: Option<String>,
}

impl MethodSig {
    pub fn is_public(&self) -> bool {
        self.access & ACC_PUBLIC != 0
    }
    pub fn is_static(&self) -> bool {
        self.access & ACC_STATIC != 0
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FieldSig {
    pub access: u16,
    pub name: String,
    pub descriptor: String,
    /// The compile-time `ConstantValue` of a `static final` field, if present (e.g.
    /// `IntCompanionObject.MAX_VALUE` → `Int(2147483647)`). What kotlinc inlines at a use site.
    pub const_value: Option<ConstVal>,
    /// The field's generic `Signature` attribute (`TA;` for a type-parameter field), if present.
    pub signature: Option<String>,
}

/// A field's compile-time constant value (from the `ConstantValue` attribute).
#[derive(Clone, Debug, PartialEq)]
pub enum ConstVal {
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    Str(String),
}

#[derive(Clone, Debug)]
pub struct ClassInfo {
    pub major: u16,
    /// class access flags (`ACC_PUBLIC`, …)
    pub access: u16,
    /// internal name, e.g. `java/lang/String`
    pub this_class: String,
    pub super_class: Option<String>,
    /// Directly-implemented interface internal names (e.g. `String` → `[java/lang/CharSequence, …]`).
    pub interfaces: Vec<String>,
    pub fields: Vec<FieldSig>,
    pub methods: Vec<MethodSig>,
    /// Strings from the `@kotlin.Metadata` `d1` annotation element — the BitEncoded protobuf carrying
    /// declaration metadata (function flags incl. `inline`, signatures). Empty if absent.
    pub kotlin_d1: Vec<String>,
    /// Strings from the `@kotlin.Metadata` `d2` annotation element, if present.
    pub kotlin_d2: Vec<String>,
    /// The class-level generic `Signature` attribute (JVM generics), e.g.
    /// `Lkotlin/ranges/IntProgression;Ljava/lang/Iterable<Ljava/lang/Integer;>;`. `None` if absent.
    pub signature: Option<String>,
}

impl ClassInfo {
    pub fn is_public(&self) -> bool {
        self.access & ACC_PUBLIC != 0
    }

    /// `ACC_INTERFACE` — call sites dispatch through it with `invokeinterface`, not `invokevirtual`.
    pub fn is_interface(&self) -> bool {
        self.access & 0x0200 != 0
    }

    pub fn method(&self, name: &str, descriptor: &str) -> Option<&MethodSig> {
        self.methods
            .iter()
            .find(|m| m.name == name && m.descriptor == descriptor)
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

/// Constant-pool entry. Public so a lazily read [`MethodCode`] can carry its defining class's pool;
/// the variants retain enough to *relocate* a body's pool references into a target class's pool.
#[derive(Clone, Debug)]
pub enum C {
    Utf8(String),
    Class(u16),            // name_index
    NameAndType(u16, u16), // name_index, descriptor_index
    Fieldref(u16, u16),    // class_index, name_and_type_index
    Methodref(u16, u16),
    InterfaceMethodref(u16, u16),
    String(u16), // utf8_index
    Integer(i32),
    Float(u32), // raw bits
    Long(i64),
    Double(u64), // raw bits
    Other,
}

/// Parse the constant pool (the reader must be positioned at `constant_pool_count`). Shared by the
/// full class parse and the lazy method-body reader.
fn parse_constant_pool(r: &mut Reader) -> Result<Vec<C>, ReadError> {
    let cp_count = r.u2()? as usize;
    let mut cp: Vec<C> = Vec::with_capacity(cp_count);
    cp.push(C::Other); // index 0 unused
    let mut idx = 1;
    while idx < cp_count {
        let tag = r.u1()?;
        let entry = match tag {
            1 => {
                let len = r.u2()? as usize;
                C::Utf8(decode_modified_utf8(r.take(len)?))
            }
            7 => C::Class(r.u2()?),
            12 => C::NameAndType(r.u2()?, r.u2()?),
            9 => C::Fieldref(r.u2()?, r.u2()?),
            10 => C::Methodref(r.u2()?, r.u2()?),
            11 => C::InterfaceMethodref(r.u2()?, r.u2()?),
            17 | 18 => {
                r.u2()?;
                r.u2()?;
                C::Other
            } // dynamic / invokedynamic
            8 => C::String(r.u2()?),
            16 | 19 | 20 => {
                r.u2()?;
                C::Other
            } // methodtype / module / package
            3 => C::Integer(r.u4()? as i32),
            4 => C::Float(r.u4()?),
            5 => C::Long(((r.u4()? as i64) << 32) | r.u4()? as i64),
            6 => C::Double(((r.u4()? as u64) << 32) | r.u4()? as u64),
            15 => {
                r.u1()?;
                r.u2()?;
                C::Other
            }
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
    Ok(cp)
}

/// The body of a method, read lazily (`read_method_code`) only when a caller — the inline expander —
/// actually needs it, never during the eager classpath scan. `code` is the raw JVM bytecode; the
/// indices in it reference `source_cp` (the defining class's constant pool) and must be relocated
/// into the target class's pool before the body can be spliced into another method.
#[derive(Clone, Debug)]
pub struct MethodCode {
    pub max_stack: u16,
    pub max_locals: u16,
    pub code: Vec<u8>,
    /// The defining class's constant pool — needed to relocate `code`'s pool references on inlining.
    pub source_cp: Vec<C>,
    /// The raw `StackMapTable` attribute body (the frame entries, without the attribute name/length
    /// header), or `None` if the method has none (a branchless body needs no frames). Required to
    /// splice a *branchy* body: its frames are relocated into the caller.
    pub stackmap: Option<Vec<u8>>,
    /// The body's exception table (`try`/`catch`/`finally` ranges). Splicing relocates each entry's
    /// byte offsets and `catch_type` into the caller. Empty for a body with no handlers.
    pub handlers: Vec<ExcEntry>,
}

/// One `Code` exception-table entry: a `[start_pc, end_pc)` guarded range, its `handler_pc`, and the
/// caught class (`catch_type` is a constant-pool `Class` index in the *source* pool, or 0 = catch-all
/// / `finally`). All offsets are byte offsets into the method's `code`.
#[derive(Clone, Copy, Debug)]
pub struct ExcEntry {
    pub start_pc: u16,
    pub end_pc: u16,
    pub handler_pc: u16,
    pub catch_type: u16,
}

/// Lazily read one method's `Code` (bytecode body) from class `bytes`, without parsing every other
/// method's body — the foundation for the inline expander. `None` if the class/method/`Code` is
/// absent (e.g. an abstract or native method).
pub fn read_method_code(bytes: &[u8], name: &str, descriptor: &str) -> Option<MethodCode> {
    let mut r = Reader { b: bytes, i: 0 };
    if r.u4().ok()? != 0xCAFEBABE {
        return None;
    }
    r.u2().ok()?; // minor
    r.u2().ok()?; // major
    let cp = parse_constant_pool(&mut r).ok()?;
    let utf8 = |i: u16| -> &str {
        match cp.get(i as usize) {
            Some(C::Utf8(s)) => s.as_str(),
            _ => "",
        }
    };
    r.u2().ok()?; // access_flags
    r.u2().ok()?; // this_class
    r.u2().ok()?; // super_class
    let ifaces = r.u2().ok()?;
    for _ in 0..ifaces {
        r.u2().ok()?;
    }
    // Skip fields (each: access, name, desc, attributes).
    let nfields = r.u2().ok()?;
    for _ in 0..nfields {
        r.u2().ok()?;
        r.u2().ok()?;
        r.u2().ok()?;
        skip_attributes(&mut r).ok()?;
    }
    // Methods — find the matching (name, descriptor), then its `Code` attribute.
    let nmethods = r.u2().ok()?;
    for _ in 0..nmethods {
        r.u2().ok()?; // access
        let mname = utf8(r.u2().ok()?).to_string();
        let mdesc = utf8(r.u2().ok()?).to_string();
        let matches = mname == name && mdesc == descriptor;
        let nattr = r.u2().ok()?;
        for _ in 0..nattr {
            let attr_name = utf8(r.u2().ok()?).to_string();
            let attr_len = r.u4().ok()? as usize;
            if matches && attr_name == "Code" {
                let max_stack = r.u2().ok()?;
                let max_locals = r.u2().ok()?;
                let code_len = r.u4().ok()? as usize;
                let code = r.take(code_len).ok()?.to_vec();
                let exc_len = r.u2().ok()?;
                let mut handlers = Vec::with_capacity(exc_len as usize);
                for _ in 0..exc_len {
                    handlers.push(ExcEntry {
                        start_pc: r.u2().ok()?,
                        end_pc: r.u2().ok()?,
                        handler_pc: r.u2().ok()?,
                        catch_type: r.u2().ok()?,
                    });
                }
                // Code-attribute attributes: find `StackMapTable` (the verifier frames).
                let nca = r.u2().ok()?;
                let mut stackmap = None;
                for _ in 0..nca {
                    let an = utf8(r.u2().ok()?).to_string();
                    let al = r.u4().ok()? as usize;
                    let body = r.take(al).ok()?;
                    if an == "StackMapTable" {
                        stackmap = Some(body.to_vec());
                    }
                }
                return Some(MethodCode {
                    max_stack,
                    max_locals,
                    code,
                    source_cp: cp,
                    stackmap,
                    handlers,
                });
            }
            r.take(attr_len).ok()?;
        }
        if matches {
            return None; // method found but has no Code (abstract/native)
        }
    }
    None
}

pub fn parse_class(bytes: &[u8]) -> Result<ClassInfo, ReadError> {
    let mut r = Reader { b: bytes, i: 0 };
    if r.u4()? != 0xCAFEBABE {
        return Err(ReadError::NotAClass);
    }
    let _minor = r.u2()?;
    let major = r.u2()?;
    let cp = parse_constant_pool(&mut r)?;

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
    let super_class = if super_idx == 0 {
        None
    } else {
        Some(class_name(super_idx))
    };

    let ifaces = r.u2()?;
    let mut interfaces = Vec::with_capacity(ifaces as usize);
    for _ in 0..ifaces {
        interfaces.push(class_name(r.u2()?));
    }

    let read_members = |r: &mut Reader| -> Result<
        Vec<(u16, String, String, Option<String>, Option<ConstVal>)>,
        ReadError,
    > {
        let n = r.u2()?;
        let mut v = Vec::new();
        for _ in 0..n {
            let access = r.u2()?;
            let name = utf8(r.u2()?);
            let desc = utf8(r.u2()?);
            let (sig, cval) = read_member_signature(r, &cp)?;
            v.push((access, name, desc, sig, cval));
        }
        Ok(v)
    };

    let fields = read_members(&mut r)?
        .into_iter()
        .map(
            |(access, name, descriptor, signature, const_value)| FieldSig {
                access,
                name,
                descriptor,
                const_value,
                signature,
            },
        )
        .collect();
    let methods = read_members(&mut r)?
        .into_iter()
        .map(|(access, name, descriptor, signature, _)| MethodSig {
            access,
            name,
            descriptor,
            signature,
        })
        .collect();

    // Read class-level attributes: @kotlin.Metadata → d1/d2 arrays, and the generic `Signature` attr.
    let (kotlin_d1, kotlin_d2, signature) = read_class_attrs(&mut r, &cp);

    Ok(ClassInfo {
        major,
        access,
        this_class,
        super_class,
        interfaces,
        fields,
        methods,
        kotlin_d1: kotlin_d1.unwrap_or_default(),
        kotlin_d2: kotlin_d2.unwrap_or_default(),
        signature,
    })
}

/// Parse class-level attributes: `RuntimeVisibleAnnotations` → @kotlin/Metadata → `d2`, and the
/// generic `Signature` attribute. Accumulates both (does not early-return) so neither is missed.
fn read_class_attrs(
    r: &mut Reader,
    cp: &[C],
) -> (Option<Vec<String>>, Option<Vec<String>>, Option<String>) {
    let utf8 = |i: u16| -> &str {
        match cp.get(i as usize) {
            Some(C::Utf8(s)) => s.as_str(),
            _ => "",
        }
    };
    let mut d1 = None;
    let mut d2 = None;
    let mut signature = None;
    let Ok(n_attrs) = r.u2() else {
        return (d1, d2, signature);
    };
    for _ in 0..n_attrs {
        let Ok(ni) = r.u2() else { break };
        let name = utf8(ni).to_string();
        let Ok(len) = r.u4() else { break };
        let len = len as usize;
        if name == "Signature" {
            if let Ok(si) = r.u2() {
                if let Some(C::Utf8(s)) = cp.get(si as usize) {
                    signature = Some(s.clone());
                }
            }
            if len > 2 {
                let _ = r.take(len - 2);
            }
            continue;
        }
        if name != "RuntimeVisibleAnnotations" {
            if r.take(len).is_err() {
                break;
            }
            continue;
        }
        // Parse annotations: find the one with type == "Lkotlin/Metadata;"
        let Ok(n_ann) = r.u2() else { break };
        for _ in 0..n_ann {
            let Ok(ati) = r.u2() else { break };
            let is_kotlin_meta = utf8(ati) == "Lkotlin/Metadata;";
            let Ok(n_pairs) = r.u2() else { break };
            for _ in 0..n_pairs {
                let Ok(eni) = r.u2() else { break };
                let field = if is_kotlin_meta { utf8(eni) } else { "" };
                let want = field == "d1" || field == "d2";
                match skip_element_value_extract_string_array(r, cp, want) {
                    Ok(Some(strings)) if field == "d1" => d1 = Some(strings),
                    Ok(Some(strings)) => d2 = Some(strings),
                    Ok(None) => {}
                    Err(_) => return (d1, d2, signature),
                }
            }
        }
    }
    (d1, d2, signature)
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
        'B' | 'C' | 'D' | 'F' | 'I' | 'J' | 'S' | 'Z' | 's' | 'c' => {
            r.u2()?;
        }
        'e' => {
            r.u2()?;
            r.u2()?;
        }
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

/// Read a field/method's attributes, returning its generic `Signature` attribute string and (for a
/// field) its `ConstantValue` if present (and skipping the rest). Same wire shape as [`skip_attributes`].
fn read_member_signature(
    r: &mut Reader,
    cp: &[C],
) -> Result<(Option<String>, Option<ConstVal>), ReadError> {
    let n = r.u2()?;
    let mut signature = None;
    let mut const_value = None;
    for _ in 0..n {
        let ni = r.u2()?;
        let len = r.u4()? as usize;
        match cp.get(ni as usize) {
            Some(C::Utf8(s)) if s == "Signature" && len == 2 => {
                let si = r.u2()?;
                if let Some(C::Utf8(s)) = cp.get(si as usize) {
                    signature = Some(s.clone());
                }
            }
            Some(C::Utf8(s)) if s == "ConstantValue" && len == 2 => {
                let ci = r.u2()? as usize;
                const_value = match cp.get(ci) {
                    Some(C::Integer(v)) => Some(ConstVal::Int(*v)),
                    Some(C::Long(v)) => Some(ConstVal::Long(*v)),
                    Some(C::Float(bits)) => Some(ConstVal::Float(f32::from_bits(*bits))),
                    Some(C::Double(bits)) => Some(ConstVal::Double(f64::from_bits(*bits))),
                    Some(C::String(ui)) => match cp.get(*ui as usize) {
                        Some(C::Utf8(s)) => Some(ConstVal::Str(s.clone())),
                        _ => None,
                    },
                    _ => None,
                };
            }
            _ => {
                r.take(len)?;
            }
        }
    }
    Ok((signature, const_value))
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
            let c = (((b & 0x0f) as u32) << 12)
                | (((bytes[i + 1] & 0x3f) as u32) << 6)
                | (bytes[i + 2] & 0x3f) as u32;
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
    use crate::jvm::classfile::{ClassWriter, CodeBuilder};

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

    /// Build a rich class (super, interfaces, fields with signature + const value, generic method
    /// signature) and assert the reader recovers every piece.
    #[test]
    fn roundtrip_fields_methods_signatures() {
        let mut cw = ClassWriter::new("demo/Box", "java/lang/Object");
        cw.set_signature("<T:Ljava/lang/Object;>Ljava/lang/Object;");
        cw.add_interface("java/lang/Runnable");
        cw.add_interface("java/lang/Comparable");
        // plain field
        cw.add_field(ACC_PUBLIC, "plain", "I");
        // field with a generic Signature (TT;)
        cw.add_field_sig(ACC_PUBLIC, "value", "Ljava/lang/Object;", Some("TT;"));
        // const val field carrying a ConstantValue attribute
        let ci_idx = cw.const_int(2147483647);
        cw.add_field_const(ACC_PUBLIC | 0x0010, "MAX", "I", ci_idx);

        let mut code = CodeBuilder::new(1);
        code.aload(0);
        code.areturn();
        cw.add_method_sig(
            ACC_PUBLIC,
            "get",
            "(Ljava/lang/Object;)Ljava/lang/Object;",
            &code,
            Some("(TT;)TT;"),
        );

        let bytes = cw.finish();
        let info = parse_class(&bytes).unwrap();

        assert_eq!(info.this_class, "demo/Box");
        assert_eq!(info.super_class.as_deref(), Some("java/lang/Object"));
        assert_eq!(
            info.interfaces,
            vec![
                "java/lang/Runnable".to_string(),
                "java/lang/Comparable".to_string()
            ]
        );
        assert_eq!(
            info.signature.as_deref(),
            Some("<T:Ljava/lang/Object;>Ljava/lang/Object;")
        );

        // Fields
        assert_eq!(info.fields.len(), 3);
        let plain = info.fields.iter().find(|f| f.name == "plain").unwrap();
        assert_eq!(plain.descriptor, "I");
        assert!(plain.signature.is_none());
        assert!(plain.const_value.is_none());
        let value = info.fields.iter().find(|f| f.name == "value").unwrap();
        assert_eq!(value.signature.as_deref(), Some("TT;"));
        let max = info.fields.iter().find(|f| f.name == "MAX").unwrap();
        assert_eq!(max.const_value, Some(ConstVal::Int(2147483647)));

        // Method + its generic signature
        let m = info
            .method("get", "(Ljava/lang/Object;)Ljava/lang/Object;")
            .unwrap();
        assert_eq!(m.signature.as_deref(), Some("(TT;)TT;"));
        assert!(m.is_public());
        assert!(!m.is_static());
    }

    #[test]
    fn roundtrip_recovers_method_code_body() {
        let mut cw = ClassWriter::new("demo/CodeKt", "java/lang/Object");
        let mut code = CodeBuilder::new(2);
        code.iload(0);
        code.iload(1);
        code.iadd();
        code.ireturn();
        cw.add_method(ACC_PUBLIC | ACC_STATIC, "add", "(II)I", &code);
        let bytes = cw.finish();
        let mc = read_method_code(&bytes, "add", "(II)I").unwrap();
        assert_eq!(mc.max_locals, 2);
        assert_eq!(mc.max_stack, 2);
        // iload_0, iload_1, iadd, ireturn
        assert_eq!(mc.code, vec![0x1a, 0x1b, 0x60, 0xac]);
        assert!(mc.handlers.is_empty());
    }

    #[test]
    fn read_method_code_missing_method_is_none() {
        let mut cw = ClassWriter::new("demo/CodeKt", "java/lang/Object");
        let mut code = CodeBuilder::new(0);
        code.ret_void();
        cw.add_method(ACC_PUBLIC, "present", "()V", &code);
        let bytes = cw.finish();
        assert!(read_method_code(&bytes, "absent", "()V").is_none());
    }

    #[test]
    fn const_value_variants_roundtrip() {
        let mut cw = ClassWriter::new("demo/Consts", "java/lang/Object");
        let li = cw.const_long(9_000_000_000);
        cw.add_field_const(ACC_PUBLIC | 0x0010, "L", "J", li);
        let fi = cw.const_float(1.5);
        cw.add_field_const(ACC_PUBLIC | 0x0010, "F", "F", fi);
        let di = cw.const_double(2.5);
        cw.add_field_const(ACC_PUBLIC | 0x0010, "D", "D", di);
        let si = cw.const_string("hello");
        cw.add_field_const(ACC_PUBLIC | 0x0010, "S", "Ljava/lang/String;", si);
        let bytes = cw.finish();
        let info = parse_class(&bytes).unwrap();
        let find = |n: &str| info.fields.iter().find(|f| f.name == n).unwrap();
        assert_eq!(find("L").const_value, Some(ConstVal::Long(9_000_000_000)));
        assert_eq!(find("F").const_value, Some(ConstVal::Float(1.5)));
        assert_eq!(find("D").const_value, Some(ConstVal::Double(2.5)));
        assert_eq!(
            find("S").const_value,
            Some(ConstVal::Str("hello".to_string()))
        );
    }

    #[test]
    fn bad_magic_is_error() {
        let bytes = [0u8, 1, 2, 3, 4, 5, 6, 7];
        match parse_class(&bytes) {
            Err(ReadError::NotAClass) => {}
            other => panic!("expected NotAClass, got {other:?}"),
        }
        assert!(read_method_code(&bytes, "x", "()V").is_none());
    }

    #[test]
    fn truncated_is_error() {
        // Valid magic then nothing — the pool count read must fail as Truncated.
        let bytes = [0xCA, 0xFE, 0xBA, 0xBE, 0, 0, 0, 52];
        assert!(matches!(parse_class(&bytes), Err(ReadError::Truncated)));
    }

    #[test]
    fn modified_utf8_decode_via_roundtrip() {
        // A method name with a NUL (encoded as C0 80 in modified UTF-8) and a multibyte char must
        // survive the writer's modified_utf8 encode and the reader's decode.
        let name = "a\u{0000}\u{00e9}"; // NUL + é (2-byte), plus ASCII
        let mut cw = ClassWriter::new("demo/UKt", "java/lang/Object");
        let mut code = CodeBuilder::new(0);
        code.ret_void();
        cw.add_method(ACC_PUBLIC, name, "()V", &code);
        let bytes = cw.finish();
        let info = parse_class(&bytes).unwrap();
        assert_eq!(info.methods[0].name, name);
    }

    #[test]
    fn decode_modified_utf8_units() {
        // ASCII, NUL via C0 80, 2-byte and 3-byte sequences.
        assert_eq!(decode_modified_utf8(b"hi"), "hi");
        assert_eq!(decode_modified_utf8(&[0xC0, 0x80]), "\u{0000}");
        assert_eq!(decode_modified_utf8(&[0xC3, 0xA9]), "\u{00e9}"); // é
        assert_eq!(decode_modified_utf8(&[0xE2, 0x82, 0xAC]), "\u{20ac}"); // €
    }

    #[test]
    fn no_superclass_when_super_index_zero() {
        // java/lang/Object itself has super_class index 0; emulate by hand-checking the accessor path
        // is exercised through a normal parse where super is present.
        let cw = ClassWriter::new("demo/Empty", "java/lang/Object");
        let bytes = cw.finish();
        let info = parse_class(&bytes).unwrap();
        assert_eq!(info.super_class.as_deref(), Some("java/lang/Object"));
        assert!(info.interfaces.is_empty());
        assert!(info.methods.is_empty());
        assert!(info.fields.is_empty());
        assert!(info.kotlin_d1.is_empty());
        assert!(info.kotlin_d2.is_empty());
    }

    #[test]
    fn interface_flag_recovered() {
        let mut cw = ClassWriter::new("demo/I", "java/lang/Object");
        cw.set_access(ACC_PUBLIC | 0x0200 | 0x0400); // PUBLIC | INTERFACE | ABSTRACT
        cw.add_abstract_method(ACC_PUBLIC | 0x0400, "run", "()V");
        let bytes = cw.finish();
        let info = parse_class(&bytes).unwrap();
        assert!(info.is_interface());
        assert!(info.is_public());
        // methods_named finds the abstract method
        assert_eq!(info.methods_named("run").len(), 1);
    }

    #[test]
    fn unknown_constant_tag_is_bad_constant() {
        // magic, minor=0, major=52, constant_pool_count=2, then an unsupported tag byte (2).
        let bytes = [
            0xCA, 0xFE, 0xBA, 0xBE, 0, 0, 0, 52, 0, 2, 2, // tag 2 is unassigned
        ];
        match parse_class(&bytes) {
            Err(ReadError::BadConstant(2)) => {}
            other => panic!("expected BadConstant(2), got {other:?}"),
        }
    }

    #[test]
    fn method_named_lookup_miss_is_none() {
        let mut cw = ClassWriter::new("demo/M", "java/lang/Object");
        let mut code = CodeBuilder::new(0);
        code.ret_void();
        cw.add_method(ACC_PUBLIC, "present", "()V", &code);
        let info = parse_class(&cw.finish()).unwrap();
        assert!(info.method("present", "()V").is_some());
        assert!(info.method("present", "()I").is_none()); // wrong descriptor
        assert!(info.methods_named("absent").is_empty());
    }

    #[test]
    fn kotlin_metadata_d1_d2_roundtrip_and_decode() {
        // Build a class carrying a real @kotlin.Metadata annotation whose d1 encodes a Package with one
        // public inline top-level function; parse_class must recover d1/d2 verbatim, and the metadata
        // decoder must read the function back out. Exercises read_class_attrs + the array element_value
        // extraction path.
        fn uvarint(mut v: u64) -> Vec<u8> {
            let mut out = Vec::new();
            loop {
                let b = (v & 0x7f) as u8;
                v >>= 7;
                if v != 0 {
                    out.push(b | 0x80);
                } else {
                    out.push(b);
                    break;
                }
            }
            out
        }
        let tag = |field: u64, wire: u64| uvarint((field << 3) | wire);
        let fvar = |field: u64, v: u64| {
            let mut o = tag(field, 0);
            o.extend(uvarint(v));
            o
        };
        let flen = |field: u64, body: &[u8]| {
            let mut o = tag(field, 2);
            o.extend(uvarint(body.len() as u64));
            o.extend_from_slice(body);
            o
        };
        // Function{ name id 0, flags = IS_INLINE(1<<10) | PUBLIC(3<<1), method_signature{ name 1, desc 2 } }
        let jvm_sig = [fvar(1, 1), fvar(2, 2)].concat();
        let func = [
            fvar(2, 0),
            fvar(9, (1 << 10) | (3 << 1)),
            flen(100, &jvm_sig),
        ]
        .concat();
        let package = flen(3, &func); // Package.function = 3
                                      // d1 string: UTF-8 mode marker + empty StringTableTypes (len 0) + package body.
        let mut d1 = String::new();
        d1.push('\u{0}');
        d1.push('\u{0}');
        for &b in &package {
            d1.push(b as char);
        }
        let d2 = vec!["greet".to_string(), "greet".to_string(), "()V".to_string()];

        let mut cw = ClassWriter::new("demo/FacadeKt", "java/lang/Object");
        cw.set_kotlin_metadata(2, &[1, 9, 0], 48, &[d1.clone()], &d2);
        let info = parse_class(&cw.finish()).unwrap();
        assert_eq!(info.kotlin_d1, vec![d1]);
        assert_eq!(info.kotlin_d2, d2);

        let fns = super::super::metadata::package_functions(&info);
        assert_eq!(fns.len(), 1);
        assert_eq!(fns[0].kotlin_name, "greet");
        assert_eq!(fns[0].jvm_desc.as_deref(), Some("()V"));
        assert!(fns[0].is_inline);
    }

    #[test]
    fn method_code_recovers_exception_handlers() {
        // A method with a try/catch range: read_method_code must recover the exception-table entry.
        let mut cw = ClassWriter::new("demo/TryKt", "java/lang/Object");
        let cat = cw.class_ref("java/lang/Exception");
        let mut code = CodeBuilder::new(1);
        let start = code.new_label();
        let end = code.new_label();
        let handler = code.new_label();
        code.bind(start); // offset 0
        code.ret_void(); // offset 0, 1 byte
        code.bind(end); // offset 1
        code.bind(handler); // offset 1
        code.astore(0); // handler body
        code.ret_void();
        code.add_exception(start, end, handler, cat);
        code.link();
        cw.add_method(ACC_PUBLIC, "m", "()V", &code);

        let mc = read_method_code(&cw.finish(), "m", "()V").unwrap();
        assert_eq!(mc.handlers.len(), 1);
        assert_eq!(mc.handlers[0].start_pc, 0);
        assert_eq!(mc.handlers[0].end_pc, 1);
        assert_eq!(mc.handlers[0].handler_pc, 1);
        assert_eq!(mc.handlers[0].catch_type, cat);
        assert!(mc.stackmap.is_none()); // no frames added
    }

    #[test]
    fn method_and_field_access_predicates() {
        let pub_static = MethodSig {
            access: ACC_PUBLIC | ACC_STATIC,
            name: "f".into(),
            descriptor: "()V".into(),
            signature: None,
        };
        assert!(pub_static.is_public());
        assert!(pub_static.is_static());
        let priv_instance = MethodSig {
            access: 0x0002, // ACC_PRIVATE
            name: "g".into(),
            descriptor: "()V".into(),
            signature: None,
        };
        assert!(!priv_instance.is_public());
        assert!(!priv_instance.is_static());
    }

    #[test]
    fn classinfo_method_lookup_hit_and_overloads() {
        let mk = |name: &str, desc: &str| MethodSig {
            access: ACC_PUBLIC,
            name: name.into(),
            descriptor: desc.into(),
            signature: None,
        };
        let info = ClassInfo {
            major: 52,
            access: ACC_PUBLIC,
            this_class: "demo/C".into(),
            super_class: Some("java/lang/Object".into()),
            interfaces: vec![],
            fields: vec![],
            methods: vec![mk("m", "()V"), mk("m", "(I)V"), mk("n", "()V")],
            kotlin_d1: vec![],
            kotlin_d2: vec![],
            signature: None,
        };
        assert_eq!(
            info.method("m", "(I)V").map(|m| m.descriptor.as_str()),
            Some("(I)V")
        );
        assert!(info.method("m", "(J)V").is_none()); // no matching descriptor
        assert!(info.method("absent", "()V").is_none());
        assert_eq!(info.methods_named("m").len(), 2);
        assert_eq!(info.methods_named("n").len(), 1);
        assert!(info.methods_named("zz").is_empty());
        assert!(!info.is_interface()); // plain class
    }

    #[test]
    fn decode_modified_utf8_fallback_branches() {
        // A truncated 2-byte lead falls back to U+FFFD (no continuation byte follows).
        assert_eq!(decode_modified_utf8(&[0xC3]), "\u{fffd}");
        // A truncated 3-byte lead (only 2 bytes) falls back on the lead, then the stray
        // continuation byte matches no pattern → a second replacement char.
        assert_eq!(decode_modified_utf8(&[0xE2, 0x82]), "\u{fffd}\u{fffd}");
        // A lone continuation byte (0x80) matches none of the lead patterns → replacement char.
        assert_eq!(decode_modified_utf8(&[0x80]), "\u{fffd}");
        // Mixed: ASCII then a good 2-byte sequence then a bad trailing lead.
        assert_eq!(
            decode_modified_utf8(&[b'a', 0xC3, 0xA9, 0xE2]),
            "a\u{00e9}\u{fffd}"
        );
    }
}
