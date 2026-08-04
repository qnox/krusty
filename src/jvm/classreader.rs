//! Minimal JVM `.class` reader: parses the constant pool, this/super class, fields and methods to
//! recover **public signatures**. This is how krusty resolves Java/JDK dependencies — read the
//! callee's `.class`, learn its method descriptors — instead of hardcoding intrinsics (Phase 6,
//! "java supported"). It reads enough to drive interop, not the full attribute set.
//!
//! Also reads the `@kotlin.Metadata` annotation (RuntimeVisibleAnnotations) to extract the `d2`
//! string table, which contains type-alias targets used by `classpath.rs` for type resolution.

use crate::types::{TypeName, TypeNameList};

pub const ACC_PUBLIC: u16 = 0x0001;
pub const ACC_PRIVATE: u16 = 0x0002;
pub const ACC_PROTECTED: u16 = 0x0004;
pub const ACC_STATIC: u16 = 0x0008;
pub const ACC_BRIDGE: u16 = 0x0040;
/// The final array parameter accepts Java vararg elements.
pub const ACC_VARARGS: u16 = 0x0080;
pub const ACC_ENUM: u16 = 0x4000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JavaNullability {
    NotNull,
    Nullable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MethodSig {
    pub access: u16,
    pub name: String,
    pub descriptor: String,
    /// The method's generic `Signature` attribute (JVM generics) if present, e.g. `listOf`'s
    /// `<T:Ljava/lang/Object;>([TT;)Ljava/util/List<TT;>;`. Carries the type parameters and how the
    /// parameter/return types use them — what the erased `descriptor` drops. `None` if non-generic.
    pub signature: Option<String>,
    /// JVM reference-parameter nullability in descriptor order.
    pub parameter_nullability: Vec<Option<JavaNullability>>,
}

impl MethodSig {
    pub fn is_public(&self) -> bool {
        self.access & ACC_PUBLIC != 0
    }
    /// A `protected` member (JVM `ACC_PROTECTED`) — reachable only from a subclass. Surfaced during the
    /// supertype member walk so a Kotlin subclass can call an inherited protected classpath member.
    pub fn is_protected(&self) -> bool {
        self.access & ACC_PROTECTED != 0
    }
    pub fn is_static(&self) -> bool {
        self.access & ACC_STATIC != 0
    }
    pub fn is_private(&self) -> bool {
        self.access & ACC_PRIVATE != 0
    }
    pub fn is_abstract(&self) -> bool {
        self.access & 0x0400 != 0
    }
    pub fn is_bridge(&self) -> bool {
        self.access & ACC_BRIDGE != 0
    }
    pub fn is_vararg(&self) -> bool {
        self.access & ACC_VARARGS != 0
    }
    pub fn has_same_parameter_descriptor(&self, other: &Self) -> bool {
        self.descriptor
            .split_once(')')
            .zip(other.descriptor.split_once(')'))
            .is_some_and(|((params, _), (other_params, _))| params == other_params)
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

impl FieldSig {
    pub fn is_private(&self) -> bool {
        self.access & ACC_PRIVATE != 0
    }
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

struct RawMember {
    access: u16,
    name: String,
    descriptor: String,
    attributes: MemberAttributes,
}

#[derive(Default)]
struct MemberAttributes {
    signature: Option<String>,
    const_value: Option<ConstVal>,
    parameter_nullability: Vec<Option<JavaNullability>>,
    parameter_access: Vec<u16>,
}

#[derive(Clone, Debug)]
pub struct ClassInfo {
    pub major: u16,
    /// class access flags (`ACC_PUBLIC`, …)
    pub access: u16,
    /// internal name, e.g. `java/lang/String`
    pub this_class: TypeName,
    pub super_class: Option<TypeName>,
    /// Directly-implemented interface internal names (e.g. `String` → `[java/lang/CharSequence, …]`).
    pub interfaces: TypeNameList,
    pub fields: Vec<FieldSig>,
    pub methods: Vec<MethodSig>,
    /// The class's `@kotlin.Metadata`, FULLY decoded at parse time — the packed `d1`/`d2` strings are
    /// not retained. [`crate::jvm::metadata::KotlinMeta::default`] (all-empty) for a plain Java class,
    /// so consumers read one model regardless of the class's source language.
    pub meta: crate::jvm::metadata::KotlinMeta,
    /// The class-level generic `Signature` attribute (JVM generics), e.g.
    /// `Lkotlin/ranges/IntProgression;Ljava/lang/Iterable<Ljava/lang/Integer;>;`. `None` if absent.
    pub signature: Option<String>,
    /// For an `annotation`/`@interface`: the `java.lang.annotation.RetentionPolicy` constant name of its
    /// `@Retention` meta-annotation (`"RUNTIME"` / `"CLASS"` / `"SOURCE"`), or `None` if absent. Kotlin
    /// maps `AnnotationRetention.{RUNTIME,BINARY,SOURCE}` → `RetentionPolicy.{RUNTIME,CLASS,SOURCE}`, so a
    /// use of this annotation is emitted `RuntimeVisibleAnnotations` for RUNTIME, `RuntimeInvisible…` for
    /// CLASS, and dropped for SOURCE.
    pub retention: Option<String>,
    pub inner_classes: Vec<InnerClassRef>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InnerClassRef {
    pub inner: String,
    pub outer: Option<String>,
    pub name: Option<String>,
    pub access: u16,
}

impl ClassInfo {
    pub fn this_class(&self) -> String {
        self.this_class.render()
    }

    pub fn this_class_matches(&self, internal: &str) -> bool {
        self.this_class.matches(internal)
    }

    /// This class's structural `InnerClasses` self entry, if it is a genuine member/local/anonymous
    /// class. Consumers use this relation for member-only flags and nesting metadata; they must not
    /// reconstruct it by splitting the encoded class name because `$` is legal in an identifier.
    pub fn inner_class_self(&self) -> Option<&InnerClassRef> {
        self.inner_classes
            .iter()
            .find(|entry| self.this_class_matches(&entry.inner))
    }

    pub fn super_class(&self) -> Option<String> {
        self.super_class.map(TypeName::render)
    }

    pub fn interfaces(&self) -> Vec<String> {
        self.interfaces.to_vec()
    }

    pub fn is_public(&self) -> bool {
        self.access & ACC_PUBLIC != 0
    }

    /// `ACC_INTERFACE` — call sites dispatch through it with `invokeinterface`, not `invokevirtual`.
    pub fn is_interface(&self) -> bool {
        self.access & 0x0200 != 0
    }

    /// `ACC_FINAL` — a subclass would fail verification (`cannot inherit from final class`).
    pub fn is_final(&self) -> bool {
        self.access & 0x0010 != 0
    }

    /// `ACC_ABSTRACT` class flag.
    pub fn is_abstract(&self) -> bool {
        self.access & 0x0400 != 0
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

    let read_members = |r: &mut Reader| -> Result<Vec<RawMember>, ReadError> {
        let n = r.u2()?;
        let mut v = Vec::new();
        for _ in 0..n {
            let access = r.u2()?;
            let name = utf8(r.u2()?);
            let descriptor = utf8(r.u2()?);
            let mut attributes = read_member_attributes(r, &cp)?;
            align_parameter_nullability(
                &descriptor,
                &mut attributes.parameter_nullability,
                &attributes.parameter_access,
            );
            v.push(RawMember {
                access,
                name,
                descriptor,
                attributes,
            });
        }
        Ok(v)
    };

    let fields = read_members(&mut r)?
        .into_iter()
        .map(|member| FieldSig {
            access: member.access,
            name: member.name,
            descriptor: member.descriptor,
            const_value: member.attributes.const_value,
            signature: member.attributes.signature,
        })
        .collect();
    let methods: Vec<MethodSig> = read_members(&mut r)?
        .into_iter()
        .map(|member| MethodSig {
            access: member.access,
            name: member.name,
            descriptor: member.descriptor,
            signature: member.attributes.signature,
            parameter_nullability: member.attributes.parameter_nullability,
        })
        .collect();

    // Read class-level attributes: @kotlin.Metadata → d1/d2 arrays, the generic `Signature` attr, and
    // (for an annotation class) its `@Retention` policy.
    let attrs = read_class_attrs(&mut r, &cp);

    let meta = crate::jvm::metadata::decode_metadata(
        &attrs.d1.unwrap_or_default(),
        &attrs.d2.unwrap_or_default(),
        attrs.k,
        &this_class,
        &methods,
    );
    Ok(ClassInfo {
        major,
        access,
        this_class: this_class.into(),
        super_class: super_class.map(Into::into),
        interfaces: interfaces.into(),
        fields,
        methods,
        meta,
        signature: attrs.signature,
        retention: attrs.retention,
        inner_classes: attrs.inner_classes,
    })
}

/// Accumulated class-level attributes from [`read_class_attrs`].
#[derive(Default)]
struct ClassAttrs {
    d1: Option<Vec<String>>,
    d2: Option<Vec<String>>,
    /// The `@kotlin.Metadata` `k` (kind) element: 1 class, 2 file facade, 4 multi-file facade,
    /// 5 multi-file part.
    k: Option<i32>,
    signature: Option<String>,
    retention: Option<String>,
    inner_classes: Vec<InnerClassRef>,
}

/// Parse class-level attributes: `RuntimeVisibleAnnotations` → @kotlin.Metadata `d1`/`d2` and (for an
/// annotation class) its `@Retention` policy; plus the generic `Signature` attribute. Accumulates all
/// (does not early-return) so none is missed.
fn read_class_attrs(r: &mut Reader, cp: &[C]) -> ClassAttrs {
    let utf8 = |i: u16| -> &str {
        match cp.get(i as usize) {
            Some(C::Utf8(s)) => s.as_str(),
            _ => "",
        }
    };
    let mut out = ClassAttrs::default();
    let Ok(n_attrs) = r.u2() else {
        return out;
    };
    for _ in 0..n_attrs {
        let Ok(ni) = r.u2() else { break };
        let name = utf8(ni).to_string();
        let Ok(len) = r.u4() else { break };
        let len = len as usize;
        let Ok(body) = r.take(len) else { break };
        let mut attr = Reader { b: body, i: 0 };
        if name == "Signature" {
            if let Ok(si) = attr.u2() {
                if let Some(C::Utf8(s)) = cp.get(si as usize) {
                    out.signature = Some(s.clone());
                }
            }
            continue;
        }
        if name == "InnerClasses" {
            let class_name = |index: u16| match cp.get(index as usize) {
                Some(C::Class(name_index)) => Some(utf8(*name_index).to_string()),
                _ => None,
            };
            let Ok(count) = attr.u2() else { continue };
            for _ in 0..count {
                let (Ok(inner), Ok(outer), Ok(simple_name), Ok(access)) =
                    (attr.u2(), attr.u2(), attr.u2(), attr.u2())
                else {
                    break;
                };
                let Some(inner) = class_name(inner) else {
                    continue;
                };
                out.inner_classes.push(InnerClassRef {
                    inner,
                    outer: class_name(outer),
                    name: (simple_name != 0).then(|| utf8(simple_name).to_string()),
                    access,
                });
            }
            continue;
        }
        if name != "RuntimeVisibleAnnotations" {
            continue;
        }
        // Parse annotations: @kotlin.Metadata (d1/d2) and @java.lang.annotation.Retention (policy).
        let Ok(n_ann) = attr.u2() else { continue };
        for _ in 0..n_ann {
            let Ok(ati) = attr.u2() else { break };
            let atype = utf8(ati);
            let is_kotlin_meta = atype == "Lkotlin/Metadata;";
            let is_retention = atype == "Ljava/lang/annotation/Retention;";
            let Ok(n_pairs) = attr.u2() else { break };
            for _ in 0..n_pairs {
                let Ok(eni) = attr.u2() else { break };
                let ename = utf8(eni);
                // `@Retention`'s `value` is an enum-constant element (`e` type_index const_index) — capture
                // the `RetentionPolicy` constant name; a valid classfile always uses the `e` tag here.
                if is_retention && ename == "value" {
                    let Ok(tag) = attr.u1() else { break };
                    if tag == b'e' {
                        let _ = attr.u2(); // enum type descriptor index
                        if let Ok(ci) = attr.u2() {
                            out.retention = Some(utf8(ci).to_string());
                        }
                    } else {
                        // Unexpected shape — stop parsing this class's attributes rather than desync.
                        return out;
                    }
                    continue;
                }
                // `@Metadata`'s `k` (kind) is an Int element (`I` tag, Integer constant) — it decides
                // how `d1` is read (protobuf vs a multi-file facade's part-name list).
                if is_kotlin_meta && ename == "k" {
                    let Ok(tag) = attr.u1() else { break };
                    if tag == b'I' {
                        if let Ok(vi) = attr.u2() {
                            if let Some(C::Integer(v)) = cp.get(vi as usize) {
                                out.k = Some(*v);
                            }
                        }
                    } else {
                        // Unexpected shape — stop parsing this class's attributes rather than desync.
                        return out;
                    }
                    continue;
                }
                let field = if is_kotlin_meta { ename } else { "" };
                let want = field == "d1" || field == "d2";
                match skip_element_value_extract_string_array(&mut attr, cp, want) {
                    Ok(Some(strings)) if field == "d1" => out.d1 = Some(strings),
                    Ok(Some(strings)) => out.d2 = Some(strings),
                    Ok(None) => {}
                    Err(_) => return out,
                }
            }
        }
    }
    out
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

fn read_member_attributes(r: &mut Reader, cp: &[C]) -> Result<MemberAttributes, ReadError> {
    let n = r.u2()?;
    let mut attributes = MemberAttributes::default();
    for _ in 0..n {
        let ni = r.u2()?;
        let len = r.u4()? as usize;
        match cp.get(ni as usize) {
            Some(C::Utf8(s)) if s == "Signature" && len == 2 => {
                let si = r.u2()?;
                if let Some(C::Utf8(s)) = cp.get(si as usize) {
                    attributes.signature = Some(s.clone());
                }
            }
            Some(C::Utf8(s)) if s == "ConstantValue" && len == 2 => {
                let ci = r.u2()? as usize;
                attributes.const_value = match cp.get(ci) {
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
            Some(C::Utf8(s)) if s == "MethodParameters" => {
                let mut body = Reader {
                    b: r.take(len)?,
                    i: 0,
                };
                let parameter_count = body.u1()? as usize;
                attributes.parameter_access.reserve(parameter_count);
                for _ in 0..parameter_count {
                    body.u2()?;
                    attributes.parameter_access.push(body.u2()?);
                }
            }
            Some(C::Utf8(s))
                if s == "RuntimeVisibleParameterAnnotations"
                    || s == "RuntimeInvisibleParameterAnnotations" =>
            {
                let body = r.take(len)?;
                read_parameter_nullability(body, cp, &mut attributes.parameter_nullability)?;
            }
            _ => {
                r.take(len)?;
            }
        }
    }
    Ok(attributes)
}

fn align_parameter_nullability(
    descriptor: &str,
    nullability: &mut Vec<Option<JavaNullability>>,
    parameter_access: &[u16],
) {
    const SYNTHETIC_OR_MANDATED: u16 = 0x1000 | 0x8000;

    let Some((parameters, _)) = crate::jvm::names::parse_method_descriptor(descriptor) else {
        return;
    };
    if nullability.is_empty() || nullability.len() == parameters.len() {
        return;
    }

    let source_parameters = parameter_access
        .iter()
        .enumerate()
        .filter_map(|(index, access)| (access & SYNTHETIC_OR_MANDATED == 0).then_some(index))
        .collect::<Vec<_>>();
    if parameter_access.len() == parameters.len() && source_parameters.len() == nullability.len() {
        let source_nullability = std::mem::take(nullability);
        nullability.resize(parameters.len(), None);
        for (index, value) in source_parameters.into_iter().zip(source_nullability) {
            nullability[index] = value;
        }
    }
}

fn read_parameter_nullability(
    body: &[u8],
    cp: &[C],
    out: &mut Vec<Option<JavaNullability>>,
) -> Result<(), ReadError> {
    let utf8 = |index: u16| match cp.get(index as usize) {
        Some(C::Utf8(value)) => value.as_str(),
        _ => "",
    };
    let mut r = Reader { b: body, i: 0 };
    let parameter_count = r.u1()? as usize;
    out.resize(out.len().max(parameter_count), None);
    for slot in out.iter_mut().take(parameter_count) {
        let annotation_count = r.u2()?;
        for _ in 0..annotation_count {
            let annotation = utf8(r.u2()?);
            let nullability = match annotation {
                "Lorg/jetbrains/annotations/NotNull;" => Some(JavaNullability::NotNull),
                "Lorg/jetbrains/annotations/Nullable;" => Some(JavaNullability::Nullable),
                _ => None,
            };
            let pair_count = r.u2()?;
            for _ in 0..pair_count {
                r.u2()?;
                skip_element_value_extract_string_array(&mut r, cp, false)?;
            }
            if let Some(nullable) = nullability {
                *slot = Some(nullable);
            }
        }
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
///
/// The encoding's units are UTF-16 code units, so a character outside the BMP arrives as its
/// surrogate PAIR — two 3-byte sequences that must be recombined, not decoded one at a time.
/// `String::from_utf16_lossy` does exactly that, and an UNPAIRED surrogate (which the encoding also
/// admits, and which no Rust `String` can hold) becomes U+FFFD. That last case is lossy: a library
/// `const val` whose value contains one reads back changed. See `docs/SPEC.md`.
fn decode_modified_utf8(bytes: &[u8]) -> String {
    let mut units: Vec<u16> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b & 0x80 == 0 {
            units.push(b as u16);
            i += 1;
        } else if b & 0xe0 == 0xc0 && i + 1 < bytes.len() {
            units.push((((b & 0x1f) as u16) << 6) | (bytes[i + 1] & 0x3f) as u16);
            i += 2;
        } else if b & 0xf0 == 0xe0 && i + 2 < bytes.len() {
            units.push(
                (((b & 0x0f) as u16) << 12)
                    | (((bytes[i + 1] & 0x3f) as u16) << 6)
                    | (bytes[i + 2] & 0x3f) as u16,
            );
            i += 3;
        } else {
            units.push(0xfffd);
            i += 1;
        }
    }
    String::from_utf16_lossy(&units)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jvm::classfile::*;

    #[test]
    fn decodes_a_supplementary_character_from_its_surrogate_pair() {
        // U+1F600 is encoded as two 3-byte surrogate sequences; decoding each on its own yields two
        // replacement characters instead of the emoji.
        let bytes = crate::metadata::encoding::modified_utf8("\u{1F600}");
        assert_eq!(decode_modified_utf8(&bytes), "\u{1F600}");
    }

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
        assert!(ci.this_class_matches("demo/RKt"));
        assert_eq!(ci.methods.len(), 1);
        assert_eq!(ci.methods[0].name, "add");
        assert_eq!(ci.methods[0].descriptor, "(II)I");
    }

    #[test]
    fn compares_method_parameters_independently_of_return_type() {
        let method = |descriptor: &str| MethodSig {
            access: super::ACC_PUBLIC,
            name: "call".to_string(),
            descriptor: descriptor.to_string(),
            signature: None,
            parameter_nullability: Vec::new(),
        };
        let concrete = method("(Ljava/lang/String;I)Ljava/lang/String;");
        let return_bridge = method("(Ljava/lang/String;I)Ljava/lang/Object;");
        let parameter_bridge = method("(Ljava/lang/Object;I)Ljava/lang/Object;");

        assert!(concrete.has_same_parameter_descriptor(&return_bridge));
        assert!(!concrete.has_same_parameter_descriptor(&parameter_bridge));
    }
}
