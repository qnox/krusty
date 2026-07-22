//! A hand-written JVM class-file writer (the format is well-specified; no external crate).
//! Targets major version 50 (Java 6). Methods that create lambda objects (new $lambda$N) emit a
//! StackMapTable attribute so the type-checking verifier on Java 25+ accepts them.

use std::collections::HashMap;

pub const ACC_PUBLIC: u16 = 0x0001;
pub const ACC_PRIVATE: u16 = 0x0002;
pub const ACC_STATIC: u16 = 0x0008;
pub const ACC_FINAL: u16 = 0x0010;
pub const ACC_SUPER: u16 = 0x0020;
pub const ACC_INTERFACE: u16 = 0x0200;
pub const ACC_ABSTRACT: u16 = 0x0400;

// Major 52 = Java 8, matching kotlinc's default JVM target.
pub const MAJOR_JAVA8: u16 = 52;

/// JVM verification type for StackMapTable entries (JVMS §4.7.4).
#[derive(Clone, PartialEq)]
pub enum VerifType {
    Top,
    Integer,
    Float,
    Long,
    Double,
    Null,
    UninitializedThis, // `this` inside a constructor, before the `<init>`/`super(…)` call
    Object(u16),       // a `CONSTANT_Class` interned EAGERLY (its pool index)
    /// A `CONSTANT_Class` by NAME, interned LAZILY at StackMapTable write time — matching kotlinc, which
    /// interns a frame's class ONLY when a WRITTEN frame lists it. A `same_frame` drops its locals, so a
    /// class that appears only in dropped frames (e.g. a `copy$default` mask-branch param) is never
    /// interned — no orphan pool entry. The frame-record path (`verif_single`, the method-entry baseline)
    /// uses this; instruction-referenced classes stay `Object(idx)`.
    ObjectName(String),
}

fn write_verif_type(vt: &VerifType, out: &mut Vec<u8>, cp: &mut ConstPool) {
    match vt {
        VerifType::Top => out.push(0),
        VerifType::Integer => out.push(1),
        VerifType::Float => out.push(2),
        VerifType::Double => out.push(3),
        VerifType::Long => out.push(4),
        VerifType::Null => out.push(5),
        VerifType::UninitializedThis => out.push(6),
        VerifType::Object(idx) => {
            out.push(7);
            u2(out, *idx);
        }
        VerifType::ObjectName(name) => {
            out.push(7);
            u2(out, cp.class(name)); // intern NOW — only reached for a frame actually written
        }
    }
}

/// Two `VerifType`s equal for StackMapTable delta comparison — class types by canonical (JVM-mapped)
/// name, bridging `Object(idx)`/`ObjectName`; every other variant by identity. Allocation-free: this
/// runs per local per frame in `build_stackmap`.
fn verif_eq(a: &VerifType, b: &VerifType, cp: &ConstPool) -> bool {
    match (a, b) {
        // The pool dedups `CONSTANT_Class` entries (and `class()` canonicalizes the name first), so
        // equal indices ⇔ the same class.
        (VerifType::Object(i), VerifType::Object(j)) => i == j,
        (VerifType::ObjectName(x), VerifType::ObjectName(y)) => {
            super::jvm_class_map::to_jvm_internal(x) == super::jvm_class_map::to_jvm_internal(y)
        }
        (VerifType::Object(i), VerifType::ObjectName(n))
        | (VerifType::ObjectName(n), VerifType::Object(i)) => cp
            .class_name(*i)
            .is_some_and(|s| s == super::jvm_class_map::to_jvm_internal(n)),
        _ => a == b,
    }
}
/// One backing field, as the plain-class pool seeder sees it.
pub struct SeedField {
    pub name: String,
    pub desc: String,
    /// 0 = primitive (no annotation), 1 = non-null reference (`@NotNull` + a `checkNotNullParameter`
    /// guard), 2 = nullable reference (`@Nullable`, no guard).
    pub ann_kind: u8,
    /// `true` for a primary-constructor PARAMETER. Only a parameter carries a ctor parameter
    /// annotation or a null-check guard — a body property is initialized in `init_body`.
    pub is_ctor_param: bool,
    /// `true` when the constructor actually stores this field. A body property initialized to `null`
    /// has no store at all — the JVM already zero-initializes it — so its name and descriptor first
    /// appear at the getter's `getfield`, not at a `putfield`.
    pub stores_in_ctor: bool,
    /// A `String` literal initializer. kotlinc interns it as an `ldc` constant just before the
    /// property's store, so it lands ahead of the field's own name/descriptor.
    pub string_const: Option<String>,
}

/// Per-member JVM generic `Signature` strings for a plain class, in kotlinc's interning positions, passed
/// to [`ClassWriter::seed_plain_class_pool`]. `None`/empty entries mean "no `Signature`" (a member whose
/// erased descriptor already captures its type). `accessors` and `fields` are index-parallel to the
/// `seed_plain_class_pool` arguments of the same name.
pub struct MemberSignatures<'a> {
    /// The primary constructor's generic `Signature` (`(Ljava/util/List<Ljava/lang/String;>;)V`).
    pub ctor: Option<&'a str>,
    /// Per-accessor generic `Signature` (a getter/setter of a parameterized-type property).
    pub accessors: &'a [Option<String>],
    /// Per-field generic `Signature` (a parameterized-type backing field).
    pub fields: &'a [Option<String>],
}

/// Extra per-member data a `data class` needs when seeding [`ClassWriter::seed_data_class_pool`], all
/// index-parallel to its `fields`. Bundled to keep the seeder's arity in check.
pub struct DataMemberInfo<'a> {
    /// Per-field JVM `hashCode` owner override — an interface/collection field dispatches
    /// `java/lang/Object.hashCode`, not `<field-class>.hashCode`. `None` ⇒ derive from the descriptor.
    pub hashcode_owners: &'a [Option<String>],
    /// The `copy` method's generic `Signature` (`(Ljava/util/List<Ljava/lang/String;>;)Ldemo/D;`),
    /// interned right after the erased `copy` descriptor.
    pub copy_sig: Option<&'a str>,
    /// Per-field generic `Signature`, interned LATE (after all data-method entries, before `@Metadata`).
    pub field_sigs: &'a [Option<String>],
}

#[derive(PartialEq, Eq, Hash, Clone)]
enum Const {
    Utf8(String),
    Integer(i32),
    Float(u32), // bit pattern (f32 isn't Hash/Eq)
    Long(i64),
    Double(u64), // bit pattern (f64 isn't Hash/Eq)
    Class(u16),
    String(u16),
    NameAndType(u16, u16),
    Methodref(u16, u16),
    InterfaceMethodref(u16, u16),
    Fieldref(u16, u16),
    MethodHandle(u8, u16),   // reference_kind, reference_index
    MethodType(u16),         // descriptor (Utf8 index)
    InvokeDynamic(u16, u16), // bootstrap_method_attr_index, name_and_type_index
}

#[derive(Default)]
struct ConstPool {
    entries: Vec<Const>, // index 0 unused conceptually; we store 1-based via len()
    dedup: HashMap<Const, u16>,
    /// Wide (`Long`/`Double`, 2-slot) entry count — lets `slot_count`/`entry_at` skip the O(n) slot walk
    /// for the common all-narrow pool.
    wide_count: u16,
}

impl ConstPool {
    /// Number of slots used (long/double take 2). Pool count in the file = this + 1.
    fn slot_count(&self) -> u16 {
        self.entries.len() as u16 + self.wide_count
    }

    fn intern(&mut self, c: Const) -> u16 {
        if let Some(&i) = self.dedup.get(&c) {
            return i;
        }
        let idx = self.slot_count() + 1; // 1-based
        if matches!(c, Const::Long(_) | Const::Double(_)) {
            self.wide_count += 1;
        }
        self.entries.push(c.clone());
        self.dedup.insert(c, idx);
        idx
    }

    fn utf8(&mut self, s: &str) -> u16 {
        self.intern(Const::Utf8(s.to_string()))
    }
    /// Non-interning lookup of an existing `CONSTANT_Utf8` entry.
    fn lookup_utf8(&self, s: &str) -> Option<u16> {
        self.dedup.get(&Const::Utf8(s.to_string())).copied()
    }
    /// Non-interning lookup of an existing `CONSTANT_String` entry.
    fn lookup_string(&self, s: &str) -> Option<u16> {
        let n = self.dedup.get(&Const::Utf8(s.to_string())).copied()?;
        self.dedup.get(&Const::String(n)).copied()
    }
    /// The entry at 1-based pool index `idx` (long/double occupy 2 slots, so this is not a plain
    /// `entries[idx-1]` in general). Reverses a `CONSTANT_Class` index back to its name for frame
    /// comparison — called per mixed `Object(idx)`/`ObjectName` local per frame, so the common
    /// no-wide-constants pool takes the O(1) path.
    fn entry_at(&self, idx: u16) -> Option<&Const> {
        if self.wide_count == 0 {
            return self.entries.get(idx as usize - 1);
        }
        let mut slot = 1u16;
        for c in &self.entries {
            if slot == idx {
                return Some(c);
            }
            slot += if matches!(c, Const::Long(_) | Const::Double(_)) {
                2
            } else {
                1
            };
        }
        None
    }
    /// The internal name of the `CONSTANT_Class` at `idx` (via its `Utf8` name), if it is one.
    fn class_name(&self, idx: u16) -> Option<&str> {
        let Const::Class(utf8_idx) = self.entry_at(idx)? else {
            return None;
        };
        match self.entry_at(*utf8_idx)? {
            Const::Utf8(s) => Some(s),
            _ => None,
        }
    }
    fn class(&mut self, internal_name: &str) -> u16 {
        // Ty→bytecode boundary: a built-in type may reach here under its Kotlin name (`kotlin/Any`);
        // a `CONSTANT_Class` must carry the JVM name (`java/lang/Object`). Every bare class reference
        // (class_ref, method/field owner, super, interfaces) funnels through here, so this single
        // mapping keeps the rest of the compiler free of `java/lang/…` names.
        let n = self.utf8(super::jvm_class_map::to_jvm_internal(internal_name));
        self.intern(Const::Class(n))
    }
    fn string(&mut self, s: &str) -> u16 {
        let n = self.utf8(s);
        self.intern(Const::String(n))
    }
    /// Whether a `CONSTANT_Class` for `internal_name` is already in the pool (WITHOUT interning it).
    /// kotlinc emits an `InnerClasses` entry for a nested class only when it appears as a class
    /// constant (a `new`/`checkcast`/owner ref), not merely inside a descriptor string.
    fn has_class(&self, internal_name: &str) -> bool {
        let mapped = super::jvm_class_map::to_jvm_internal(internal_name);
        self.dedup
            .get(&Const::Utf8(mapped.to_string()))
            .is_some_and(|&u| self.dedup.contains_key(&Const::Class(u)))
    }
    fn integer(&mut self, v: i32) -> u16 {
        self.intern(Const::Integer(v))
    }
    fn long(&mut self, v: i64) -> u16 {
        self.intern(Const::Long(v))
    }
    fn float(&mut self, v: f32) -> u16 {
        self.intern(Const::Float(v.to_bits()))
    }
    fn double(&mut self, v: f64) -> u16 {
        self.intern(Const::Double(v.to_bits()))
    }
    fn name_and_type(&mut self, name: &str, desc: &str) -> u16 {
        let n = self.utf8(name);
        let d = self.utf8(desc);
        self.intern(Const::NameAndType(n, d))
    }
    fn methodref(&mut self, class: &str, name: &str, desc: &str) -> u16 {
        let c = self.class(class);
        let nt = self.name_and_type(name, desc);
        self.intern(Const::Methodref(c, nt))
    }
    fn interface_methodref(&mut self, class: &str, name: &str, desc: &str) -> u16 {
        let c = self.class(class);
        let nt = self.name_and_type(name, desc);
        self.intern(Const::InterfaceMethodref(c, nt))
    }
    fn fieldref(&mut self, class: &str, name: &str, desc: &str) -> u16 {
        let c = self.class(class);
        let nt = self.name_and_type(name, desc);
        self.intern(Const::Fieldref(c, nt))
    }
    /// A `CONSTANT_MethodHandle` of kind `invokestatic` (reference_kind 6) onto a `Methodref`.
    fn method_handle_static(&mut self, class: &str, name: &str, desc: &str) -> u16 {
        let r = self.methodref(class, name, desc);
        self.intern(Const::MethodHandle(6, r))
    }
    fn method_type(&mut self, desc: &str) -> u16 {
        let d = self.utf8(desc);
        self.intern(Const::MethodType(d))
    }
    fn invoke_dynamic(&mut self, bootstrap: u16, name: &str, desc: &str) -> u16 {
        let nt = self.name_and_type(name, desc);
        self.intern(Const::InvokeDynamic(bootstrap, nt))
    }

    fn serialize(&self, out: &mut Vec<u8>) {
        u2(out, self.slot_count() + 1);
        for c in &self.entries {
            match c {
                Const::Utf8(s) => {
                    out.push(1);
                    let b = crate::metadata::encoding::modified_utf8(s);
                    u2(out, b.len() as u16);
                    out.extend_from_slice(&b);
                }
                Const::Integer(v) => {
                    out.push(3);
                    u4(out, *v as u32);
                }
                Const::Float(bits) => {
                    out.push(4);
                    u4(out, *bits);
                }
                Const::Long(v) => {
                    out.push(5);
                    u4(out, (*v >> 32) as u32);
                    u4(out, *v as u32);
                }
                Const::Double(bits) => {
                    out.push(6);
                    u4(out, (*bits >> 32) as u32);
                    u4(out, *bits as u32);
                }
                Const::Class(n) => {
                    out.push(7);
                    u2(out, *n);
                }
                Const::String(n) => {
                    out.push(8);
                    u2(out, *n);
                }
                Const::Fieldref(c, nt) => {
                    out.push(9);
                    u2(out, *c);
                    u2(out, *nt);
                }
                Const::Methodref(c, nt) => {
                    out.push(10);
                    u2(out, *c);
                    u2(out, *nt);
                }
                Const::InterfaceMethodref(c, nt) => {
                    out.push(11);
                    u2(out, *c);
                    u2(out, *nt);
                }
                Const::NameAndType(n, d) => {
                    out.push(12);
                    u2(out, *n);
                    u2(out, *d);
                }
                Const::MethodHandle(kind, r) => {
                    out.push(15);
                    out.push(*kind);
                    u2(out, *r);
                }
                Const::MethodType(d) => {
                    out.push(16);
                    u2(out, *d);
                }
                Const::InvokeDynamic(b, nt) => {
                    out.push(18);
                    u2(out, *b);
                    u2(out, *nt);
                }
            }
        }
    }
}

struct MethodInfo {
    access: u16,
    name: u16,
    desc: u16,
    max_stack: u16,
    max_locals: u16,
    /// `None` for an abstract method (no `Code` attribute).
    code: Option<Vec<u8>>,
    /// `Code` exception table: `(start_pc, end_pc, handler_pc, catch_type)` — `catch_type` is a
    /// constant-pool class index, or 0 for a catch-all.
    exceptions: Vec<(u16, u16, u16, u16)>,
    /// Pre-built StackMapTable attribute body (after name+length fields). `None` if no frames.
    stackmap: Option<Vec<u8>>,
    /// `Signature` attribute: constant-pool UTF8 index of the generic signature string, or `None`.
    signature: Option<u16>,
    /// `LineNumberTable` entries `(start_pc, line_number)`, or empty for no attribute. kotlinc emits
    /// this for every method; krusty currently fills it only for synthesized members (one entry at
    /// pc 0 → the class declaration line).
    lnt: Vec<(u16, u16)>,
    /// `LocalVariableTable` entries `(name_index, descriptor_index, slot, start_pc)`. `start_pc` is
    /// `None` for a local live for the whole method (the shape of every synthesized member's `this` +
    /// params) — written as `start_pc=0, length=code_len`. `Some(pc)` is a local that becomes live
    /// mid-method (e.g. a `hashCode` `result` accumulator, live from its first store) — written as
    /// `start_pc=pc, length=code_len-pc`.
    lvt: Vec<(u16, u16, u16, Option<u16>)>,
    /// Method-level `RuntimeInvisibleAnnotations` (each entry a pre-encoded annotation) — e.g. the
    /// `@org.jetbrains.annotations.NotNull` kotlinc puts on a non-null reference RETURN.
    invisible_anns: Vec<Vec<u8>>,
    /// `RuntimeInvisibleParameterAnnotations`: one entry per method parameter (in order), each a list
    /// of that parameter's pre-encoded annotations. Empty ⇒ no attribute; kotlinc annotates each
    /// non-null reference parameter with `@NotNull` (primitive params get an empty list).
    param_anns: Vec<Vec<Vec<u8>>>,
}

struct FieldInfo {
    access: u16,
    name: u16,
    desc: u16,
    /// `Signature` attribute: constant-pool UTF8 index of the generic signature (e.g. a type-parameter
    /// field `val a: A` → `TA;`), or `None`.
    signature: Option<u16>,
    /// `ConstantValue` attribute: constant-pool index of the compile-time constant (`const val`), or
    /// `None`. kotlinc emits this on a `const val` field (and leaves `<clinit>` empty); the JVM
    /// initializes the field from it.
    const_value: Option<u16>,
    /// Encoded `annotation` structures (each type_index + element_value_pairs) for this field's
    /// `RuntimeVisibleAnnotations` (RUNTIME retention) and `RuntimeInvisibleAnnotations` (BINARY).
    visible_anns: Vec<Vec<u8>>,
    invisible_anns: Vec<Vec<u8>>,
}

pub struct ClassWriter {
    cp: ConstPool,
    access: u16,
    this_class: u16,
    super_class: u16,
    interfaces: Vec<u16>,
    fields: Vec<FieldInfo>,
    methods: Vec<MethodInfo>,
    class_attributes: Vec<(u16, Vec<u8>)>, // (name_index, raw bytes)
    /// Constant-pool index of the class's generic `Signature` VALUE, when it has one.
    class_signature: Option<u16>,
    /// Encoded `annotation` structures (type_index + element_value_pairs, WITHOUT the outer count) for the
    /// class's single `RuntimeVisibleAnnotations` attribute — `@Metadata` and user annotations both append
    /// here so `finish` writes ONE attribute (two would be invalid per JVMS §4.7.16).
    runtime_annotations: Vec<Vec<u8>>,
    /// `BootstrapMethods` entries: `(method_handle_cp_index, static_argument_cp_indices)`.
    /// The index of an entry here is its `bootstrap_method_attr_index` (referenced by InvokeDynamic).
    bootstrap_methods: Vec<(u16, Vec<u16>)>,
    /// Whether the class itself carries a `Deprecated` attribute (from `@Deprecated`).
    class_deprecated: bool,
    /// `(name_index, desc_index)` of methods carrying a `Deprecated` attribute (from `@Deprecated`).
    deprecated_methods: std::collections::HashSet<(u16, u16)>,
    /// Candidate `InnerClasses` entries (the file's nested classes). `finish` emits only those whose
    /// `inner` is actually referenced as a class constant — kotlinc's rule.
    inner_class_candidates: Vec<InnerClassSpec>,
    /// Class-file major version to emit (default v52; set via [`ClassWriter::set_major`]).
    major: u16,
    /// Source-file simple name for the `SourceFile` attribute (set via [`ClassWriter::set_source_file`]).
    source_file: Option<String>,
    pub internal_name: String,
}

/// One candidate `InnerClasses` entry: the nested class, its enclosing class (`None` for an anonymous
/// local), its simple name (`None` when anonymous), and the entry's access flags.
#[derive(Clone)]
pub struct InnerClassSpec {
    pub inner: String,
    pub outer: Option<String>,
    pub name: Option<String>,
    pub access: u16,
}

impl ClassWriter {
    pub fn new(internal_name: &str, super_internal: &str) -> ClassWriter {
        ClassWriter::new_generic(internal_name, None, super_internal)
    }

    /// [`ClassWriter::new`] for a class carrying a generic `Signature`. kotlinc (ASM) visits
    /// `(name, signature, superName)` in that order, so the signature VALUE interns BETWEEN the class
    /// and superclass names — the attribute NAME is interned later, with the other attribute names.
    pub fn new_generic(
        internal_name: &str,
        signature: Option<&str>,
        super_internal: &str,
    ) -> ClassWriter {
        let mut cp = ConstPool::default();
        let this_class = cp.class(internal_name);
        if let Some(sig) = signature {
            cp.utf8(sig);
        }
        let super_class = cp.class(super_internal);
        ClassWriter {
            cp,
            access: ACC_PUBLIC | ACC_FINAL | ACC_SUPER,
            this_class,
            super_class,
            interfaces: Vec::new(),
            fields: Vec::new(),
            methods: Vec::new(),
            class_attributes: Vec::new(),
            class_signature: None,
            runtime_annotations: Vec::new(),
            bootstrap_methods: Vec::new(),
            class_deprecated: false,
            deprecated_methods: std::collections::HashSet::new(),
            inner_class_candidates: Vec::new(),
            major: MAJOR_JAVA8,
            source_file: None,
            internal_name: internal_name.to_string(),
        }
    }

    /// Set the class-file major version to emit (kotlinc maps `-jvm-target 25` ⇒ v69). Default v52.
    pub fn set_major(&mut self, major: u16) {
        self.major = major;
    }

    /// Set the source-file simple name for the `SourceFile` attribute (e.g. `Foo.kt`). `None` (the
    /// default) emits no attribute.
    pub fn set_source_file(&mut self, name: Option<String>) {
        self.source_file = name;
    }

    /// Register a candidate `InnerClasses` entry (a nested class in this file). `finish` emits it only
    /// if `inner` is referenced as a class constant. Register the whole file's nest on every writer —
    /// the per-class filter then yields exactly the entries kotlinc emits for that class.
    pub fn add_inner_class(&mut self, spec: InnerClassSpec) {
        self.inner_class_candidates.push(spec);
    }

    /// Mark the class itself as carrying a `Deprecated` attribute (kotlinc emits this for a `@Deprecated`
    /// declaration, e.g. a `@Serializable` class's HIDDEN-deprecated `$$serializer` object).
    pub fn set_deprecated(&mut self) {
        self.class_deprecated = true;
    }

    /// Mark a previously-added method (by name+descriptor) as carrying a `Deprecated` attribute.
    pub fn mark_method_deprecated(&mut self, name: &str, desc: &str) {
        let n = self.cp.utf8(name);
        let d = self.cp.utf8(desc);
        self.deprecated_methods.insert((n, d));
    }

    /// Override the class access flags (e.g. `ACC_PUBLIC | ACC_INTERFACE | ACC_ABSTRACT`).
    pub fn set_access(&mut self, access: u16) {
        self.access = access;
    }

    /// Attach a class-level generic `Signature` attribute (e.g. `<T:Ljava/lang/Object;>Ljava/lang/Object;`).
    /// Record the class's generic `Signature`. The VALUE is interned here (it dedups onto the slot
    /// [`ClassWriter::new_generic`] reserved between the class and superclass names); the attribute
    /// NAME is interned late, with the other attribute names, matching kotlinc.
    pub fn set_signature(&mut self, signature: &str) {
        let sig = self.cp.utf8(signature);
        self.class_signature = Some(sig);
    }

    /// Add an implemented interface / extended interface by internal name.
    pub fn add_interface(&mut self, internal: &str) {
        let c = self.cp.class(internal);
        self.interfaces.push(c);
    }

    /// Declare an abstract method (no `Code` attribute) — for interfaces.
    pub fn add_abstract_method(&mut self, access: u16, name: &str, desc: &str) {
        self.add_abstract_method_sig(access, name, desc, None);
    }

    /// Like [`add_abstract_method`], plus an optional generic `Signature` attribute string.
    pub fn add_abstract_method_sig(
        &mut self,
        access: u16,
        name: &str,
        desc: &str,
        signature: Option<&str>,
    ) {
        let n = self.cp.utf8(name);
        let d = self.cp.utf8(desc);
        let sig = signature.map(|s| self.cp.utf8(s));
        self.methods.push(MethodInfo {
            access: access | ACC_ABSTRACT,
            name: n,
            desc: d,
            max_stack: 0,
            max_locals: 0,
            code: None,
            exceptions: Vec::new(),
            stackmap: None,
            signature: sig,
            lnt: Vec::new(),
            lvt: Vec::new(),
            invisible_anns: Vec::new(),
            param_anns: Vec::new(),
        });
    }

    /// Declare a field (e.g. a backing field for a Kotlin property).
    pub fn add_field(&mut self, access: u16, name: &str, desc: &str) {
        self.add_field_sig(access, name, desc, None);
    }

    /// Like [`add_field`], plus an optional generic `Signature` attribute string (`TA;` for a field
    /// typed by a type parameter).
    pub fn add_field_sig(&mut self, access: u16, name: &str, desc: &str, signature: Option<&str>) {
        let n = self.cp.utf8(name);
        let d = self.cp.utf8(desc);
        let sig = signature.map(|s| self.cp.utf8(s));
        self.fields.push(FieldInfo {
            access,
            name: n,
            desc: d,
            signature: sig,
            const_value: None,
            visible_anns: Vec::new(),
            invisible_anns: Vec::new(),
        });
    }

    /// Add a field carrying a `ConstantValue` attribute (`const_idx` = a constant-pool index from
    /// `const_string`/`const_int`/… ). kotlinc emits this on a `const val`; the JVM initializes the
    /// field, so its `<clinit>` store is omitted.
    pub fn add_field_const(&mut self, access: u16, name: &str, desc: &str, const_idx: u16) {
        let n = self.cp.utf8(name);
        let d = self.cp.utf8(desc);
        self.fields.push(FieldInfo {
            access,
            name: n,
            desc: d,
            signature: None,
            const_value: Some(const_idx),
            visible_anns: Vec::new(),
            invisible_anns: Vec::new(),
        });
    }

    /// Attach user annotations to the most recently added field. RUNTIME-retained annotations go to
    /// `RuntimeVisibleAnnotations`, BINARY-retained to `RuntimeInvisibleAnnotations` — matching kotlinc.
    pub fn set_last_field_annotations(
        &mut self,
        visible: &[crate::ir::AppliedAnnotation],
        invisible: &[crate::ir::AppliedAnnotation],
    ) {
        let vis: Vec<Vec<u8>> = visible.iter().map(|a| self.encode_annotation(a)).collect();
        let invis: Vec<Vec<u8>> = invisible
            .iter()
            .map(|a| self.encode_annotation(a))
            .collect();
        if let Some(f) = self.fields.last_mut() {
            f.visible_anns = vis;
            f.invisible_anns = invis;
        }
    }

    /// Encode one `annotation` structure (type_index + element_value_pairs) to a fresh byte buffer.
    fn encode_annotation(&mut self, a: &crate::ir::AppliedAnnotation) -> Vec<u8> {
        let mut body = Vec::new();
        self.ev_annotation(&mut body, a);
        body
    }

    /// Attach a `@kotlin.Metadata` annotation (RuntimeVisibleAnnotations) describing the file facade.
    /// `d1`/`d2` are the encoded protobuf payload + string table.
    pub fn set_kotlin_metadata(
        &mut self,
        k: i32,
        mv: &[i32],
        xi: i32,
        d1: &[String],
        d2: &[String],
    ) {
        // kotlinc interns each element's KEY immediately before that element's VALUE constants (mv key
        // then the mv integers, then k key then its integer, …) rather than all keys up front — so the
        // constant pool interleaves keys and values. Match that by interning each key inline.
        let anno_type = self.cp.utf8("Lkotlin/Metadata;");
        // One `annotation` structure (type_index + element_value_pairs) — appended to the shared list so
        // `finish` writes a single `RuntimeVisibleAnnotations` attribute even alongside user annotations.
        let mut body = Vec::new();
        u2(&mut body, anno_type);
        u2(&mut body, 5); // element_value_pairs: mv, k, xi, d1, d2
        let n_mv = self.cp.utf8("mv");
        u2(&mut body, n_mv);
        self.ev_int_array(&mut body, mv);
        let n_k = self.cp.utf8("k");
        u2(&mut body, n_k);
        self.ev_int(&mut body, k);
        let n_xi = self.cp.utf8("xi");
        u2(&mut body, n_xi);
        self.ev_int(&mut body, xi);
        let n_d1 = self.cp.utf8("d1");
        u2(&mut body, n_d1);
        self.ev_str_array(&mut body, d1);
        let n_d2 = self.cp.utf8("d2");
        u2(&mut body, n_d2);
        self.ev_str_array(&mut body, d2);
        self.runtime_annotations.push(body);
    }

    fn ev_int(&mut self, out: &mut Vec<u8>, v: i32) {
        out.push(b'I');
        let idx = self.cp.integer(v);
        u2(out, idx);
    }
    fn ev_str(&mut self, out: &mut Vec<u8>, s: &str) {
        out.push(b's');
        let idx = self.cp.utf8(s);
        u2(out, idx);
    }
    fn ev_int_array(&mut self, out: &mut Vec<u8>, vs: &[i32]) {
        out.push(b'[');
        u2(out, vs.len() as u16);
        for &v in vs {
            self.ev_int(out, v);
        }
    }
    fn ev_str_array(&mut self, out: &mut Vec<u8>, ss: &[String]) {
        out.push(b'[');
        u2(out, ss.len() as u16);
        for s in ss {
            self.ev_str(out, s);
        }
    }

    /// Encode one `element_value` (JVMS §4.7.16.1) for a resolved annotation argument.
    fn ev_value(&mut self, out: &mut Vec<u8>, v: &crate::ir::AnnoValue) {
        use crate::ir::{AnnoValue, IrConst};
        match v {
            AnnoValue::Const(c) => match c {
                IrConst::Boolean(b) => {
                    out.push(b'Z');
                    let i = self.cp.integer(*b as i32);
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
                IrConst::String(s) => self.ev_str(out, s),
                IrConst::Null => self.ev_str(out, ""),
            },
            AnnoValue::Enum(ty, name) => {
                out.push(b'e');
                let ty = ty.render();
                let ti = self.cp.utf8(&format!("L{ty};"));
                u2(out, ti);
                let ni = self.cp.utf8(name);
                u2(out, ni);
            }
            AnnoValue::Class(internal) => {
                out.push(b'c');
                let internal = internal.render();
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
    fn ev_annotation(&mut self, out: &mut Vec<u8>, a: &crate::ir::AppliedAnnotation) {
        let internal = a.internal.render();
        let ti = self.cp.utf8(&format!("L{internal};"));
        u2(out, ti);
        u2(out, a.values.len() as u16);
        for (name, v) in &a.values {
            let ni = self.cp.utf8(name);
            u2(out, ni);
            self.ev_value(out, v);
        }
    }

    /// Queue the applied annotations for the class's `RuntimeVisibleAnnotations` (JVMS §4.7.16). They join
    /// any `@Metadata` in the shared list; `finish` writes exactly ONE attribute.
    pub fn set_runtime_annotations(&mut self, anns: &[crate::ir::AppliedAnnotation]) {
        for a in anns {
            let mut body = Vec::new();
            self.ev_annotation(&mut body, a);
            self.runtime_annotations.push(body);
        }
    }

    /// Intern helpers exposed for the emitter (Phase 4) to reference pool entries while building code.
    pub fn methodref(&mut self, class: &str, name: &str, desc: &str) -> u16 {
        self.cp.methodref(class, name, desc)
    }
    pub fn interface_methodref(&mut self, class: &str, name: &str, desc: &str) -> u16 {
        self.cp.interface_methodref(class, name, desc)
    }
    pub fn fieldref(&mut self, class: &str, name: &str, desc: &str) -> u16 {
        self.cp.fieldref(class, name, desc)
    }

    /// Pre-intern a plain property class's constant-pool entries in kotlinc/ASM's first-use order, so
    /// the natural emission that follows reuses these indices (interning dedups). kotlinc visits each
    /// method [name, descriptor, body refs, LVT strings] before the next, and interns backing-field
    /// name/descriptor lazily at the `putfield` — an order krusty's field-then-method emission does not
    /// otherwise reproduce. Call BEFORE any `add_field`/`add_method` for the class. `accessors` are the
    /// getters (and, for a `var`, setters) in declaration order as `(name, descriptor)`.
    pub fn seed_plain_class_pool(
        &mut self,
        this_internal: &str,
        super_internal: &str,
        ctor_desc: &str,
        fields: &[SeedField],
        // (name, descriptor, setter_kind): 0 = getter, 1 = primitive/other setter, 2 = non-null
        // reference setter (its `checkNotNullParameter` guard also interns a `<set-?>` String constant).
        accessors: &[(String, String, u8)],
        // Per-member generic `Signature`s (parameterized-type ctor/accessor/field members).
        sigs: &MemberSignatures,
    ) {
        // Primary constructor: name + descriptor are interned at method entry, before its body.
        self.cp.utf8("<init>");
        self.cp.utf8(ctor_desc);
        // The ctor's generic Signature (`(Ljava/util/List<Ljava/lang/String;>;)V`) — right after the desc.
        if let Some(s) = sigs.ctor {
            self.cp.utf8(s);
        }
        // The `@NotNull`/`@Nullable` annotation type(s), interned at the constructor's PARAMETER
        // annotations (kotlinc visits these before the body) in first-use order over the reference
        // parameters. Reused by every getter return / setter parameter annotation and guard.
        let mut seeded_notnull = false;
        let mut seeded_nullable = false;
        for f in fields.iter().filter(|f| f.is_ctor_param) {
            let kind = f.ann_kind;
            if kind == 1 && !seeded_notnull {
                self.cp.utf8("Lorg/jetbrains/annotations/NotNull;");
                seeded_notnull = true;
            } else if kind == 2 && !seeded_nullable {
                self.cp.utf8("Lorg/jetbrains/annotations/Nullable;");
                seeded_nullable = true;
            }
        }
        // Constructor body — a `checkNotNullParameter(param, "name")` guard per non-null reference param
        // (its name + a String constant), then, at the FIRST guard, the shared `Intrinsics` machinery.
        let mut seeded_intrinsics = false;
        for f in fields.iter().filter(|f| f.is_ctor_param) {
            if f.ann_kind == 1 {
                let name = &f.name;
                self.cp.utf8(name);
                self.cp.string(name);
                if !seeded_intrinsics {
                    self.cp.methodref(
                        "kotlin/jvm/internal/Intrinsics",
                        "checkNotNullParameter",
                        "(Ljava/lang/Object;Ljava/lang/String;)V",
                    );
                    seeded_intrinsics = true;
                }
            }
        }
        // `super()` call: `()V`, its NameAndType, the Methodref.
        self.cp.methodref(super_internal, "<init>", "()V");
        // One `putfield` per property-backed parameter: field name, descriptor, NameAndType, Fieldref.
        for f in fields.iter().filter(|f| f.stores_in_ctor) {
            // A body property's `String` initializer is pushed by `ldc` before its `putfield`.
            if let Some(sc) = &f.string_const {
                self.cp.string(sc);
            }
            self.cp.utf8(&f.name);
            self.cp.utf8(&f.desc);
            self.cp.fieldref(this_internal, &f.name, &f.desc);
        }
        // The constructor's LocalVariableTable strings (`this` and its type); the parameters reuse the
        // field name/descriptor entries interned just above.
        self.cp.utf8("this");
        self.cp.utf8(&format!("L{this_internal};"));
        // Each accessor: name + descriptor at entry (its body reuses the field Fieldref above). A setter
        // then interns `<set-?>` right after — its LocalVariableTable value-parameter name (kotlinc's
        // synthetic name), plus a `<set-?>` String constant for a non-null reference setter's
        // `checkNotNullParameter` guard. Interleaved per-setter (deduped) so it lands before the next
        // accessor, as kotlinc does — not batched at the end.
        for (i, (name, desc, setter_kind)) in accessors.iter().enumerate() {
            self.cp.utf8(name);
            self.cp.utf8(desc);
            // A getter/setter of a parameterized-type property carries a generic Signature, interned
            // right after its descriptor (kotlinc's order).
            if let Some(Some(s)) = sigs.accessors.get(i) {
                self.cp.utf8(s);
            }
            // A field the constructor never stores (a body property initialized to `null`) first
            // appears at its GETTER: kotlinc interns the return annotation, then the field's name and
            // descriptor at the `getfield`.
            if *setter_kind == 0 {
                if let Some(f) = fields.iter().find(|f| {
                    !f.stores_in_ctor && {
                        let mut ch = f.name.chars();
                        let cap = ch
                            .next()
                            .map(|c| c.to_uppercase().collect::<String>() + ch.as_str())
                            .unwrap_or_default();
                        *name == format!("get{cap}")
                    }
                }) {
                    match f.ann_kind {
                        1 => {
                            self.cp.utf8("Lorg/jetbrains/annotations/NotNull;");
                        }
                        2 => {
                            self.cp.utf8("Lorg/jetbrains/annotations/Nullable;");
                        }
                        _ => {}
                    }
                    self.cp.utf8(&f.name);
                    self.cp.utf8(&f.desc);
                    self.cp.fieldref(this_internal, &f.name, &f.desc);
                }
            }
            if *setter_kind >= 1 {
                self.cp.utf8("<set-?>");
            }
            if *setter_kind == 2 {
                self.cp.string("<set-?>");
            }
        }
        // Each parameterized-type FIELD's `Signature` value, in field order — kotlinc interns these after
        // all accessors, right before the class's `@Metadata` (a field's Signature attribute serializes
        // after the methods but its Utf8 lands here).
        for s in sigs.fields.iter().flatten() {
            self.cp.utf8(s);
        }
    }

    /// Seed a `data class`'s synthesized-method constant-pool entries in kotlinc's first-use order,
    /// AFTER [`seed_plain_class_pool`] (which seeds `<init>`/fields/accessors). `fields` is
    /// `(name, jvm_descriptor)` in declaration order. Mirrors the bodies kotlinc emits for
    /// `componentN`/`copy`/`copy$default`/`toString`/`hashCode`/`equals` so the natural emission reuses
    /// these indices. `simple` is the class's simple name (for the `toString` prefix `Simple(field=`).
    pub fn seed_data_class_pool(
        &mut self,
        this_internal: &str,
        ctor_desc: &str,
        simple: &str,
        fields: &[(String, String)],
        info: &DataMemberInfo,
    ) {
        let self_ref = format!("L{this_internal};");
        // The primary-ctor parameter descriptors (between the parens of `ctor_desc`).
        let params = &ctor_desc[1..ctor_desc.rfind(')').unwrap_or(1)];
        // The StringBuilder.append overload + the boxing-class hashCode for a field JVM descriptor.
        let append_desc = |d: &str| -> &'static str {
            match d {
                "I" | "S" | "B" => "(I)Ljava/lang/StringBuilder;",
                "J" => "(J)Ljava/lang/StringBuilder;",
                "F" => "(F)Ljava/lang/StringBuilder;",
                "D" => "(D)Ljava/lang/StringBuilder;",
                "Z" => "(Z)Ljava/lang/StringBuilder;",
                "C" => "(C)Ljava/lang/StringBuilder;",
                "Ljava/lang/String;" => "(Ljava/lang/String;)Ljava/lang/StringBuilder;",
                _ => "(Ljava/lang/Object;)Ljava/lang/StringBuilder;",
            }
        };
        // The `x.hashCode()` for a primitive field is `<Box>.hashCode(prim)`; for a reference it is a
        // virtual `hashCode()` on the field's own class.
        let hashcode_ref = |d: &str| -> Option<(&'static str, &'static str)> {
            match d {
                "I" => Some(("java/lang/Integer", "(I)I")),
                "J" => Some(("java/lang/Long", "(J)I")),
                "D" => Some(("java/lang/Double", "(D)I")),
                "F" => Some(("java/lang/Float", "(F)I")),
                "Z" => Some(("java/lang/Boolean", "(Z)I")),
                "C" => Some(("java/lang/Character", "(C)I")),
                "B" => Some(("java/lang/Byte", "(B)I")),
                "S" => Some(("java/lang/Short", "(S)I")),
                _ => None, // reference: `field.hashCode()` interned via its own class below
            }
        };
        let is_ref = |d: &str| d.starts_with('L') || d.starts_with('[');

        // componentN — each body is a field read; only the method name is new.
        for i in 1..=fields.len() {
            self.cp.utf8(&format!("component{i}"));
        }
        // copy — name, descriptor, its generic Signature (a parameterized ctor param — right after the
        // erased descriptor, kotlinc's order), @NotNull (return), then `new <self>(...)` (ctor Methodref).
        self.cp.utf8("copy");
        let copy_desc = format!("({}){self_ref}", params);
        self.cp.utf8(&copy_desc);
        if let Some(s) = info.copy_sig {
            self.cp.utf8(s);
        }
        self.cp.utf8("Lorg/jetbrains/annotations/NotNull;");
        self.cp.methodref(this_internal, "<init>", ctor_desc);
        // copy$default — its descriptor, then the Methodref back to `copy`.
        self.cp.utf8("copy$default");
        let copy_default_desc = format!("({self_ref}{}ILjava/lang/Object;){self_ref}", params);
        self.cp.utf8(&copy_default_desc);
        self.cp.methodref(this_internal, "copy", &copy_desc);
        // toString — StringBuilder chain: `"Simple(f0="` append, each field append, then `)` + toString.
        self.cp.utf8("toString");
        self.cp.utf8("()Ljava/lang/String;");
        self.cp
            .methodref("java/lang/StringBuilder", "<init>", "()V");
        for (i, (name, desc)) in fields.iter().enumerate() {
            let prefix = if i == 0 {
                format!("{simple}({name}=")
            } else {
                format!(", {name}=")
            };
            self.cp.string(&prefix);
            self.cp.methodref(
                "java/lang/StringBuilder",
                "append",
                append_desc("Ljava/lang/String;"),
            );
            if desc.starts_with('[') {
                // An ARRAY field content-prints via `java.util.Arrays.toString` (kotlinc's data-class
                // shape); its `String` result reuses the `append(String)` methodref above.
                let ts = match desc.as_str() {
                    "[Z" => "([Z)Ljava/lang/String;",
                    "[C" => "([C)Ljava/lang/String;",
                    "[B" => "([B)Ljava/lang/String;",
                    "[S" => "([S)Ljava/lang/String;",
                    "[I" => "([I)Ljava/lang/String;",
                    "[J" => "([J)Ljava/lang/String;",
                    "[F" => "([F)Ljava/lang/String;",
                    "[D" => "([D)Ljava/lang/String;",
                    _ => "([Ljava/lang/Object;)Ljava/lang/String;",
                };
                self.cp.methodref("java/util/Arrays", "toString", ts);
            } else {
                self.cp
                    .methodref("java/lang/StringBuilder", "append", append_desc(desc));
            }
        }
        self.cp.methodref(
            "java/lang/StringBuilder",
            "append",
            "(C)Ljava/lang/StringBuilder;",
        );
        self.cp.methodref(
            "java/lang/StringBuilder",
            "toString",
            "()Ljava/lang/String;",
        );
        // hashCode — kotlinc interns the method NAME and its `()I` descriptor together at method
        // entry (both no-ops when an `Int` getter already interned them), then the per-field hash
        // refs in body order: a primitive via its boxing class's static, an ARRAY via
        // `java.util.Arrays.hashCode` (content hash — kotlinc's data-class shape), a BOXED nullable
        // primitive via `Object.hashCode()` (its Kotlin type has no JVM class to name as owner),
        // and any other reference via a virtual `hashCode()` on the field's own class. A nullable
        // field's null guard is branches only — it interns nothing extra.
        self.cp.utf8("hashCode");
        self.cp.utf8("()I");
        // The `Arrays.hashCode` overload for an array field descriptor.
        let arrays_hash_desc = |d: &str| -> &'static str {
            match d {
                "[Z" => "([Z)I",
                "[C" => "([C)I",
                "[B" => "([B)I",
                "[S" => "([S)I",
                "[I" => "([I)I",
                "[J" => "([J)I",
                "[F" => "([F)I",
                "[D" => "([D)I",
                _ => "([Ljava/lang/Object;)I", // reference/nested arrays share the Object[] overload
            }
        };
        // A boxed-primitive field (`Int?` → `Ljava/lang/Integer;`) dispatches `Object.hashCode()`.
        let is_boxed_prim = |d: &str| {
            matches!(
                d,
                "Ljava/lang/Integer;"
                    | "Ljava/lang/Long;"
                    | "Ljava/lang/Double;"
                    | "Ljava/lang/Float;"
                    | "Ljava/lang/Short;"
                    | "Ljava/lang/Byte;"
                    | "Ljava/lang/Character;"
                    | "Ljava/lang/Boolean;"
            )
        };
        for (i, (_, desc)) in fields.iter().enumerate() {
            match hashcode_ref(desc) {
                Some((cls, d)) => {
                    self.cp.methodref(cls, "hashCode", d);
                }
                None if desc.starts_with('[') => {
                    self.cp
                        .methodref("java/util/Arrays", "hashCode", arrays_hash_desc(desc));
                }
                None if is_boxed_prim(desc) => {
                    self.cp.methodref("java/lang/Object", "hashCode", "()I");
                }
                None if is_ref(desc) => {
                    // The owner `field_hash` chose (interface/collection → `java/lang/Object`); fall back
                    // to the descriptor's class when unrecorded (a concrete class owns its `hashCode`).
                    let owner = info
                        .hashcode_owners
                        .get(i)
                        .and_then(|o| o.as_deref())
                        .unwrap_or(&desc[1..desc.len() - 1]);
                    self.cp.methodref(owner, "hashCode", "()I");
                }
                None => {}
            }
        }
        // A ≥2-field `hashCode` folds into a `result` accumulator local (kotlinc names it in the LVT,
        // typed `I`); a single-field `hashCode` is a bare `return h(f0)` with no local. Intern the name
        // and its descriptor here, right after the hash refs and before `equals`, to match kotlinc's
        // first-use position.
        if fields.len() >= 2 {
            self.cp.utf8("result");
            self.cp.utf8("I");
        }
        // equals — name, descriptor, @Nullable (param). kotlinc interns the equals BODY's per-field
        // comparison refs BEFORE the `other`/`Object` LVT names, in field order: a `Double`/`Float` field
        // compares via the IEEE-aware `<Box>.compare` (so `NaN`/`-0.0` match kotlinc), a reference via
        // `Intrinsics.areEqual`; the other primitives compare directly (`if_icmp*`/`lcmp`, no ref).
        self.cp.utf8("equals");
        self.cp.utf8("(Ljava/lang/Object;)Z");
        self.cp.utf8("Lorg/jetbrains/annotations/Nullable;");
        for (_, desc) in fields {
            match desc.as_str() {
                "D" => {
                    self.cp.methodref("java/lang/Double", "compare", "(DD)I");
                }
                "F" => {
                    self.cp.methodref("java/lang/Float", "compare", "(FF)I");
                }
                d if is_ref(d) => {
                    self.cp.methodref(
                        "kotlin/jvm/internal/Intrinsics",
                        "areEqual",
                        "(Ljava/lang/Object;Ljava/lang/Object;)Z",
                    );
                }
                _ => {}
            }
        }
        self.cp.utf8("other");
        self.cp.utf8("Ljava/lang/Object;");
        // Each parameterized-type FIELD's `Signature` value, LATE — after every data-method entry, right
        // before the class's `@Metadata` (kotlinc interns a data class's field signatures here, not with
        // the field/accessors like a plain class).
        for s in info.field_sigs.iter().flatten() {
            self.cp.utf8(s);
        }
    }

    pub fn const_string(&mut self, s: &str) -> u16 {
        self.cp.string(s)
    }
    pub fn const_int(&mut self, v: i32) -> u16 {
        self.cp.integer(v)
    }
    pub fn const_long(&mut self, v: i64) -> u16 {
        self.cp.long(v)
    }
    pub fn const_float(&mut self, v: f32) -> u16 {
        self.cp.float(v)
    }
    pub fn const_double(&mut self, v: f64) -> u16 {
        self.cp.double(v)
    }

    /// A `MethodType` constant from a method descriptor (e.g. `(Ljava/lang/Object;)Ljava/lang/Object;`).
    pub fn method_type(&mut self, desc: &str) -> u16 {
        self.cp.method_type(desc)
    }
    /// An `invokestatic` `MethodHandle` constant (reference_kind 6) onto a static method.
    pub fn method_handle_static(&mut self, class: &str, name: &str, desc: &str) -> u16 {
        self.cp.method_handle_static(class, name, desc)
    }
    /// Register a `BootstrapMethods` entry — `method_handle` is a `MethodHandle` cp index, `args` are
    /// the static-argument cp indices. Returns the `bootstrap_method_attr_index` (deduped).
    pub fn add_bootstrap(&mut self, method_handle: u16, args: Vec<u16>) -> u16 {
        if let Some(i) = self
            .bootstrap_methods
            .iter()
            .position(|e| e.0 == method_handle && e.1 == args)
        {
            return i as u16;
        }
        self.bootstrap_methods.push((method_handle, args));
        (self.bootstrap_methods.len() - 1) as u16
    }
    /// An `InvokeDynamic` constant binding a bootstrap entry to a call-site name+descriptor.
    pub fn invoke_dynamic(&mut self, bootstrap: u16, name: &str, desc: &str) -> u16 {
        self.cp.invoke_dynamic(bootstrap, name, desc)
    }

    /// Whether a method with exactly this name+descriptor has already been added (used to avoid
    /// emitting a bridge that would duplicate an existing method).
    pub fn has_method(&mut self, name: &str, desc: &str) -> bool {
        let n = self.cp.utf8(name);
        let d = self.cp.utf8(desc);
        self.methods.iter().any(|m| m.name == n && m.desc == d)
    }

    /// Pre-intern a method DESCRIPTOR so it lands before entries the method's body would otherwise
    /// intern first. kotlinc visits a method's signature before its code, so a body-only reference
    /// (e.g. the private ctor a synthetic accessor delegates to) must not claim the earlier slot.
    pub fn reserve_descriptor(&mut self, desc: &str) {
        self.cp.utf8(desc);
    }

    /// Pre-intern a method NAME, for the same reason as [`reserve_descriptor`]: kotlinc reaches a
    /// method's name before anything its body (or a field it writes) interns.
    pub fn reserve_method_name(&mut self, name: &str) {
        self.cp.utf8(name);
    }

    pub fn add_method(&mut self, access: u16, name: &str, desc: &str, code: &CodeBuilder) {
        self.add_method_sig(access, name, desc, code, None);
    }

    /// Like [`add_method`], plus an optional generic `Signature` attribute string.
    /// Append the StackMapTable verification type of each parameter in `desc` to `out` (a `long`/
    /// `double` occupies one verification-type slot here, matching the StackMapTable encoding).
    ///
    /// Returns `false` on a malformed descriptor WITHOUT completing `out` — the caller must then
    /// compress against no baseline (all `full_frame`s) rather than a silently wrong initial frame:
    /// a frame that falsely compared "same" against a mis-derived baseline would make the verifier
    /// (which derives the real one from the descriptor) reject the class.
    #[must_use]
    fn append_param_verif_types(desc: &str, out: &mut Vec<VerifType>) -> bool {
        let (Some(stripped), Some(end)) = (desc.strip_prefix('('), desc.find(')')) else {
            return false;
        };
        let params = &stripped.as_bytes()[..end - 1];
        let mut i = 0;
        while i < params.len() {
            match params[i] {
                b'I' | b'S' | b'B' | b'C' | b'Z' => {
                    out.push(VerifType::Integer);
                    i += 1;
                }
                b'J' => {
                    out.push(VerifType::Long);
                    i += 1;
                }
                b'F' => {
                    out.push(VerifType::Float);
                    i += 1;
                }
                b'D' => {
                    out.push(VerifType::Double);
                    i += 1;
                }
                b'L' => {
                    let start = i;
                    while i < params.len() && params[i] != b';' {
                        i += 1;
                    }
                    if i == params.len() {
                        return false; // unterminated `L…;`
                    }
                    let Ok(name) = std::str::from_utf8(&params[start + 1..i]) else {
                        return false;
                    };
                    // Deferred: record the name; `write_verif_type` interns it ONLY if a written frame
                    // lists it. A param whose frames all compress to `same_frame` is never interned — no
                    // orphan pool entry (the reason this baseline is not eagerly interned).
                    out.push(VerifType::ObjectName(name.to_string()));
                    i += 1; // skip ';'
                }
                b'[' => {
                    let start = i;
                    while i < params.len() && params[i] == b'[' {
                        i += 1;
                    }
                    if i < params.len() && params[i] == b'L' {
                        while i < params.len() && params[i] != b';' {
                            i += 1;
                        }
                        if i == params.len() {
                            return false; // unterminated `[L…;`
                        }
                        i += 1;
                    } else if i < params.len() {
                        i += 1; // primitive array element (`[I`, `[[Z`, …)
                    } else {
                        return false; // bare `[` with no element type
                    }
                    // An array type is a REFERENCE; its StackMapTable verification type is
                    // `Object_variable_info` referencing a `CONSTANT_Class` whose name is the array
                    // DESCRIPTOR itself (`[I`, `[Ljava/lang/String;`) — JVMS §4.7.4 / §4.4.1. Recorded by
                    // name and interned at write only if a written frame lists it (`to_jvm_internal`
                    // leaves descriptors untouched).
                    let Ok(descriptor) = std::str::from_utf8(&params[start..i]) else {
                        return false;
                    };
                    out.push(VerifType::ObjectName(descriptor.to_string()));
                }
                _ => return false, // not a JVM type descriptor character
            }
        }
        true
    }

    /// Intern a method's name, descriptor, generic `Signature` and annotation types NOW, before its
    /// body is emitted. kotlinc (ASM) visits a method header before its code, so those entries precede
    /// every constant the body introduces; krusty builds the body first, which would otherwise put them
    /// after. Interning is idempotent, so calling this and then adding the method is safe.
    pub fn reserve_method_pool(
        &mut self,
        name: &str,
        desc: &str,
        signature: Option<&str>,
        ann_types: &[&str],
    ) {
        self.cp.utf8(name);
        self.cp.utf8(desc);
        if let Some(s) = signature {
            self.cp.utf8(s);
        }
        for a in ann_types {
            self.cp.utf8(a);
        }
    }

    pub fn add_method_sig(
        &mut self,
        access: u16,
        name: &str,
        desc: &str,
        code: &CodeBuilder,
        signature: Option<&str>,
    ) {
        let n = self.cp.utf8(name);
        let d = self.cp.utf8(desc);
        let sig = signature.map(|s| self.cp.utf8(s));
        // The method-entry frame (StackMapTable frames are deltas from it): `this` (unless static;
        // `<init>` is UninitializedThis until super() runs) followed by each parameter's type. Only
        // computed when the method actually has frames — `append_param_verif_types` interns the
        // parameters' class types, which would otherwise perturb the pool of a branch-free method.
        let stackmap = if code.has_frames() {
            const ACC_STATIC: u16 = 0x0008;
            let mut initial_locals: Vec<VerifType> = Vec::new();
            if access & ACC_STATIC == 0 {
                initial_locals.push(if name == "<init>" {
                    VerifType::UninitializedThis
                } else {
                    VerifType::ObjectName(self.internal_name.clone())
                });
            }
            let baseline =
                Self::append_param_verif_types(desc, &mut initial_locals).then_some(initial_locals);
            code.build_stackmap(baseline.as_deref(), &mut self.cp)
        } else {
            None
        };
        self.methods.push(MethodInfo {
            access,
            name: n,
            desc: d,
            max_stack: code.max_stack,
            max_locals: code.max_locals,
            code: Some(code.bytes.clone()),
            exceptions: code.resolved_exceptions(),
            stackmap,
            signature: sig,
            lnt: Vec::new(),
            lvt: Vec::new(),
            invisible_anns: Vec::new(),
            param_anns: Vec::new(),
        });
    }

    /// Attach kotlinc's non-null annotations to a previously-added method (matched by name+descriptor):
    /// `@org.jetbrains.annotations.NotNull` / `@Nullable` on the return (a method-level
    /// `RuntimeInvisibleAnnotations`) and/or on individual parameters (`RuntimeInvisibleParameterAnnotations`).
    /// `ret` is the return annotation's type descriptor (e.g. `Lorg/jetbrains/annotations/NotNull;`) or
    /// `None`; `params` gives each parameter's annotation type or `None`, in parameter order. Interning
    /// the annotation types here fixes their constant-pool position. No-op if the method isn't found.
    pub fn set_method_nullability(
        &mut self,
        name: &str,
        desc: &str,
        ret: Option<&str>,
        params: &[Option<&str>],
    ) {
        // Resolve WITHOUT interning first, like `set_method_debug`: describing a method that was never
        // emitted (the accessors of a `private` property, which are read straight from the field) must
        // not perturb the constant pool — the name and descriptor would be orphan entries.
        let (Some(n), Some(d)) = (self.cp.lookup_utf8(name), self.cp.lookup_utf8(desc)) else {
            return;
        };
        if !self.methods.iter().any(|m| m.name == n && m.desc == d) {
            return;
        }
        // A parameterless annotation is `type_index(u2) + num_element_value_pairs(u2 = 0)`.
        let empty_ann = |cp: &mut ConstPool, ty: &str| -> Vec<u8> {
            let ti = cp.utf8(ty);
            vec![(ti >> 8) as u8, ti as u8, 0, 0]
        };
        let invisible_anns: Vec<Vec<u8>> = ret
            .map(|t| vec![empty_ann(&mut self.cp, t)])
            .unwrap_or_default();
        let has_param_ann = params.iter().any(|p| p.is_some());
        let param_anns: Vec<Vec<Vec<u8>>> = if has_param_ann {
            params
                .iter()
                .map(|p| {
                    p.map(|t| vec![empty_ann(&mut self.cp, t)])
                        .unwrap_or_default()
                })
                .collect()
        } else {
            Vec::new()
        };
        if let Some(m) = self.methods.iter_mut().find(|m| m.name == n && m.desc == d) {
            m.invisible_anns = invisible_anns;
            m.param_anns = param_anns;
        }
    }

    /// Attach `@NotNull` / `@Nullable` (a `RuntimeInvisibleAnnotations`) to a previously-added field by
    /// name — kotlinc annotates the backing field of a non-null reference property. No-op if not found.
    pub fn set_field_nullability(&mut self, name: &str, ann_type: &str) {
        let n = self.cp.utf8(name);
        let ti = self.cp.utf8(ann_type);
        let ann = vec![(ti >> 8) as u8, ti as u8, 0, 0];
        if let Some(f) = self.fields.iter_mut().find(|f| f.name == n) {
            f.invisible_anns = vec![ann];
        }
    }

    /// Attach kotlinc-style debug tables to a previously-added method (matched by name+descriptor):
    /// a `LineNumberTable` mapping pc 0 → `decl_line`, and a `LocalVariableTable` listing `locals`
    /// (`(name, jvm_descriptor, slot)`), each live for the whole method body. Interns the attribute
    /// names and each local's name/descriptor here, so the call ORDER fixes their constant-pool
    /// position (kotlinc adds them per method, ctor before accessors). No-op if the method isn't found.
    pub fn set_method_debug(
        &mut self,
        name: &str,
        desc: &str,
        // `Some((start_pc, line))` emits a LineNumberTable; `None` emits none — kotlinc gives a
        // LineNumberTable to `<init>`/accessors but NOT to a data class's synthesized methods
        // (component/copy/equals/hashCode/toString), which carry a LocalVariableTable only.
        lnt: Option<(u16, u32)>,
        locals: &[(String, String, u16)],
    ) {
        // Resolve WITHOUT interning first: describing a method that was never emitted (e.g. the ctor /
        // accessors of an `interface`, which has neither) must not perturb the constant pool.
        let (Some(n), Some(d)) = (self.cp.lookup_utf8(name), self.cp.lookup_utf8(desc)) else {
            return;
        };
        // An ABSTRACT method (an interface member, or `abstract fun`) has no Code attribute, so it
        // has nowhere to hang a LineNumberTable or LocalVariableTable — kotlinc emits neither.
        if !self
            .methods
            .iter()
            .any(|m| m.name == n && m.desc == d && m.code.is_some())
        {
            return;
        }
        let lvt: Vec<(u16, u16, u16, Option<u16>)> = locals
            .iter()
            .map(|(nm, ds, slot)| (self.cp.utf8(nm), self.cp.utf8(ds), *slot, None))
            .collect();
        if let Some(m) = self.methods.iter_mut().find(|m| m.name == n && m.desc == d) {
            m.lnt = lnt
                .map(|(pc, line)| (pc, line as u16))
                .into_iter()
                .collect();
            m.lvt = lvt;
        }
    }

    /// Replace a method's LineNumberTable with MULTIPLE `(start_pc, line)` entries. kotlinc gives a
    /// constructor one entry per source construct it runs: the super call on the class-declaration
    /// line, each body-property initializer on its own line, then the trailing `return` back on the
    /// class line. Lookup-only, like [`set_method_debug`] — never perturbs the constant pool.
    pub fn set_method_lines(&mut self, name: &str, desc: &str, entries: &[(u16, u32)]) {
        let (Some(n), Some(d)) = (self.cp.lookup_utf8(name), self.cp.lookup_utf8(desc)) else {
            return;
        };
        if let Some(m) = self
            .methods
            .iter_mut()
            .find(|m| m.name == n && m.desc == d && m.code.is_some())
        {
            m.lnt = entries.iter().map(|&(pc, l)| (pc, l as u16)).collect();
        }
    }

    /// A ≥2-field data class's `hashCode` LocalVariableTable: the `result` accumulator (`I`, slot 1,
    /// live from its first store to method end) listed BEFORE `this` (slot 0, whole method) — kotlinc's
    /// exact shape. No LineNumberTable (a synthesized data-class method gets none). `result`'s start is
    /// found by walking the emitted body to the first store into slot 1 (the `istore_1` that folds the
    /// first field's hash into the accumulator), so it is correct regardless of the first field's type.
    /// Byte length of the `ldc` loading `s` as a String constant — 2 (`ldc`, pool index ≤ 255) or
    /// 3 (`ldc_w`). Lookup-only (never interns); `None` when the constant is absent. Lets the
    /// debug-table pass compute a `checkNotNullParameter` prologue's real length instead of
    /// assuming the 2-byte form — a big class can push the param-name String past index 255.
    pub fn string_ldc_len(&self, s: &str) -> Option<u16> {
        self.cp
            .lookup_string(s)
            .map(|i| if i <= 255 { 2 } else { 3 })
    }

    pub fn set_hashcode_result_debug(&mut self, this_desc: &str) {
        let hn = self.cp.utf8("hashCode");
        let hd = self.cp.utf8("()I");
        let result_n = self.cp.utf8("result");
        let result_d = self.cp.utf8("I");
        let this_n = self.cp.utf8("this");
        let this_d = self.cp.utf8(this_desc);
        if let Some(m) = self
            .methods
            .iter_mut()
            .find(|m| m.name == hn && m.desc == hd)
        {
            let start = m
                .code
                .as_deref()
                .and_then(|c| first_store_end(c, 1))
                .unwrap_or(0) as u16;
            m.lnt = Vec::new();
            m.lvt = vec![
                (result_n, result_d, 1, Some(start)),
                (this_n, this_d, 0, None),
            ];
        }
    }

    pub fn finish(mut self) -> Vec<u8> {
        // kotlinc interns the `SourceFile` VALUE (the `.kt` name) right after the class annotations and
        // before the Code-attribute names, then the `SourceFile` attribute NAME later, and
        // `RuntimeVisibleAnnotations` last. Intern the value up front to match.
        let sourcefile_value = self.source_file.clone().map(|src| self.cp.utf8(&src));
        // Code-related attribute NAMES intern in kotlinc's real first-use order, which is driven by
        // its field-then-method visiting — NOT a fixed order. kotlinc visits fields first, so a field
        // annotation interns `RuntimeInvisibleAnnotations` BEFORE `Code`; then each method, in emit
        // order, contributes `LineNumberTable`, `LocalVariableTable`, its own method-level
        // `RuntimeInvisibleAnnotations`, `StackMapTable`, and `RuntimeInvisibleParameterAnnotations` on
        // first use. Two shapes make the difference visible:
        //   * plain `class C(val x: Int, var y: String)` — the `String` field carries `@NotNull`, so RIA
        //     interns first (before `Code`), and no method branches ⇒ no `StackMapTable`.
        //   * `data class D(val x: Int)` — only the synthesized methods carry annotations, so RIA interns
        //     AFTER the debug tables (from `copy`/`toString`), and `equals` (branchy) interns
        //     `StackMapTable` last.
        // A hard-coded order matches one shape and diverges on the other; walk the real first-use
        // sequence so both come out byte-identical.
        #[derive(PartialEq)]
        enum An {
            Lnt,
            Lvt,
            Ria,
            Smt,
            Ripa,
            Sig,
        }
        // A field's `Signature` attribute name interns BEFORE its `RuntimeInvisibleAnnotations` and before
        // `Code` — kotlinc visits fields first, and a field's `Signature` attribute precedes its
        // annotations (`class C(val xs: List<String>)` pool: `Signature`, `RuntimeInvisibleAnnotations`,
        // `Code`). The later `signature_attr_name` intern dedups onto this index.
        let field_sig_name = self
            .fields
            .iter()
            .any(|f| f.signature.is_some())
            .then(|| self.cp.utf8("Signature"));
        let field_has_invis = self.fields.iter().any(|f| !f.invisible_anns.is_empty());
        // Field-level RIA, if any field is annotated: interns before `Code` (fields precede methods).
        let field_ria = field_has_invis.then(|| self.cp.utf8("RuntimeInvisibleAnnotations"));
        // kotlinc interns `Code` only when a method actually has one — an `interface` with no bodies
        // has none, and an unused attribute name would diverge.
        let code_attr_name = if self.methods.iter().any(|m| m.code.is_some()) {
            self.cp.utf8("Code")
        } else {
            0
        };
        // First-use order of the per-method attribute names, in method emit order.
        let mut seq: Vec<An> = Vec::new();
        for m in &self.methods {
            if !m.lnt.is_empty() && !seq.contains(&An::Lnt) {
                seq.push(An::Lnt);
            }
            if !m.lvt.is_empty() && !seq.contains(&An::Lvt) {
                seq.push(An::Lvt);
            }
            // A method's own generic `Signature` — after its Code sub-attributes, before its
            // annotations (kotlinc's per-method attribute order).
            if m.signature.is_some() && !seq.contains(&An::Sig) {
                seq.push(An::Sig);
            }
            if !m.invisible_anns.is_empty() && !seq.contains(&An::Ria) {
                seq.push(An::Ria);
            }
            if m.stackmap.is_some() && !seq.contains(&An::Smt) {
                seq.push(An::Smt);
            }
            if !m.param_anns.is_empty() && !seq.contains(&An::Ripa) {
                seq.push(An::Ripa);
            }
        }
        let (mut lnt_attr_name, mut lvt_attr_name, mut stackmap_attr_name, mut ripa_attr_name) =
            (None, None, None, None);
        // A method-level RIA first use dedups onto the field-level index when both are present.
        let mut invis_ann_name = field_ria;
        for k in &seq {
            match k {
                An::Lnt => lnt_attr_name = Some(self.cp.utf8("LineNumberTable")),
                An::Lvt => lvt_attr_name = Some(self.cp.utf8("LocalVariableTable")),
                An::Ria => invis_ann_name = Some(self.cp.utf8("RuntimeInvisibleAnnotations")),
                An::Smt => stackmap_attr_name = Some(self.cp.utf8("StackMapTable")),
                An::Ripa => {
                    ripa_attr_name = Some(self.cp.utf8("RuntimeInvisibleParameterAnnotations"))
                }
                An::Sig => {
                    self.cp.utf8("Signature");
                }
            }
        }
        let method_invis_ann_name = invis_ann_name;
        // The `Signature` attribute name: reuse the early field-Signature index when a field carries one
        // (interned before `Code`), else intern here if a METHOD carries a signature. Only interned when
        // actually used — an unused entry would diverge from kotlinc's output for non-generic classes.
        let class_has_sig = self.class_signature.is_some();
        let signature_attr_name = field_sig_name.or_else(|| {
            (class_has_sig || self.methods.iter().any(|m| m.signature.is_some()))
                .then(|| self.cp.utf8("Signature"))
        });
        // Intern `ConstantValue` only if a `const val` field carries one.
        let constval_attr_name = if self.fields.iter().any(|f| f.const_value.is_some()) {
            Some(self.cp.utf8("ConstantValue"))
        } else {
            None
        };
        // Intern `Deprecated` only if the class or a method carries it.
        let deprecated_attr_name = if self.class_deprecated || !self.deprecated_methods.is_empty() {
            Some(self.cp.utf8("Deprecated"))
        } else {
            None
        };
        // Field annotation attribute names, interned only when a field actually carries them.
        let field_vis_ann_name = if self.fields.iter().any(|f| !f.visible_anns.is_empty()) {
            Some(self.cp.utf8("RuntimeVisibleAnnotations"))
        } else {
            None
        };
        // Field-level `RuntimeInvisibleAnnotations` reuses the name interned before `Code` (dedup).
        let field_invis_ann_name = if self.fields.iter().any(|f| !f.invisible_anns.is_empty()) {
            invis_ann_name
        } else {
            None
        };
        // Build the `BootstrapMethods` attribute body before serializing the pool (its name + any
        // remaining indices must already be interned). All handle/argument indices were interned
        // when `add_bootstrap` ran during code emission.
        // Each optional class attribute is BUILT here (interning its name/values before the pool is
        // serialized) but held in a local, then written in kotlinc's fixed class-attribute order below:
        //   InnerClasses, Signature, SourceFile, Deprecated, RuntimeVisibleAnnotations, BootstrapMethods.
        // (krusty does not yet emit InnerClasses / class-level Signature.)
        let bootstrap_attr = if !self.bootstrap_methods.is_empty() {
            let name = self.cp.utf8("BootstrapMethods");
            let mut body = Vec::new();
            u2(&mut body, self.bootstrap_methods.len() as u16);
            for (mh, args) in &self.bootstrap_methods {
                u2(&mut body, *mh);
                u2(&mut body, args.len() as u16);
                for &a in args {
                    u2(&mut body, a);
                }
            }
            Some((name, body))
        } else {
            None
        };
        // `SourceFile`: name_index + a 2-byte body = the CP index of the source-file UTF8 (its VALUE was
        // interned at the top of `finish`). kotlinc interns the `SourceFile` name BEFORE the
        // `RuntimeVisibleAnnotations` name, so build this attribute first.
        let sourcefile_attr = sourcefile_value.map(|file_idx| {
            let name = self.cp.utf8("SourceFile");
            let mut body = Vec::new();
            u2(&mut body, file_idx);
            (name, body)
        });
        // ONE `RuntimeVisibleAnnotations` attribute for all queued annotations (`@Metadata` + user ones);
        // its attribute name is interned LAST, as kotlinc does.
        let rva_attr = if !self.runtime_annotations.is_empty() {
            let name = self.cp.utf8("RuntimeVisibleAnnotations");
            let mut body = Vec::new();
            u2(&mut body, self.runtime_annotations.len() as u16);
            for a in &self.runtime_annotations {
                body.extend_from_slice(a);
            }
            Some((name, body))
        } else {
            None
        };
        // Class-level `Deprecated` (zero-length). Its name was interned above with the method one.
        let deprecated_attr = self
            .class_deprecated
            .then(|| (deprecated_attr_name.unwrap(), Vec::new()));
        // `InnerClasses` (kotlinc's first class attribute): one entry per registered nested class that
        // this class actually references as a class constant (the `has_class` filter), in registration
        // order. `inner` is already interned (that is why it passed the filter); `outer`/`name` intern
        // here — before the pool is serialized.
        let inner_classes_attr = {
            let referenced: Vec<InnerClassSpec> = self
                .inner_class_candidates
                .iter()
                .filter(|s| self.cp.has_class(&s.inner))
                .cloned()
                .collect();
            (!referenced.is_empty()).then(|| {
                let name = self.cp.utf8("InnerClasses");
                let mut body = Vec::new();
                u2(&mut body, referenced.len() as u16);
                for s in &referenced {
                    let inner_idx = self.cp.class(&s.inner);
                    let outer_idx = s.outer.as_deref().map_or(0, |o| self.cp.class(o));
                    let name_idx = s.name.as_deref().map_or(0, |n| self.cp.utf8(n));
                    u2(&mut body, inner_idx);
                    u2(&mut body, outer_idx);
                    u2(&mut body, name_idx);
                    u2(&mut body, s.access);
                }
                (name, body)
            })
        };
        let mut out = Vec::new();
        u4(&mut out, 0xCAFEBABE);
        u2(&mut out, 0); // minor
        u2(&mut out, self.major);
        self.cp.serialize(&mut out);
        u2(&mut out, self.access);
        u2(&mut out, self.this_class);
        u2(&mut out, self.super_class);
        u2(&mut out, self.interfaces.len() as u16);
        for &i in &self.interfaces {
            u2(&mut out, i);
        }
        u2(&mut out, self.fields.len() as u16);
        for f in &self.fields {
            u2(&mut out, f.access);
            u2(&mut out, f.name);
            u2(&mut out, f.desc);
            let nattr = f.signature.is_some() as u16
                + f.const_value.is_some() as u16
                + (!f.visible_anns.is_empty()) as u16
                + (!f.invisible_anns.is_empty()) as u16;
            u2(&mut out, nattr);
            // `ConstantValue` first (kotlinc's field-attribute order on a `const val`).
            if let Some(cv) = f.const_value {
                u2(&mut out, constval_attr_name.unwrap());
                u4(&mut out, 2);
                u2(&mut out, cv);
            }
            if let Some(si) = f.signature {
                u2(&mut out, signature_attr_name.unwrap());
                u4(&mut out, 2);
                u2(&mut out, si);
            }
            write_annotation_attr(&mut out, field_vis_ann_name, &f.visible_anns);
            write_annotation_attr(&mut out, field_invis_ann_name, &f.invisible_anns);
        }
        u2(&mut out, self.methods.len() as u16);
        for m in &self.methods {
            u2(&mut out, m.access);
            u2(&mut out, m.name);
            u2(&mut out, m.desc);
            let sig_attr: u16 = if m.signature.is_some() { 1 } else { 0 };
            let dep_attr: u16 = if self.deprecated_methods.contains(&(m.name, m.desc)) {
                1
            } else {
                0
            };
            // Method-level `RuntimeInvisibleAnnotations` (annotated return) and
            // `RuntimeInvisibleParameterAnnotations` (annotated params) each count as one attribute.
            let mria_attr: u16 = u16::from(!m.invisible_anns.is_empty());
            let ripa_attr: u16 = u16::from(!m.param_anns.is_empty());
            let ann_attr = mria_attr + ripa_attr;
            match &m.code {
                None => u2(&mut out, sig_attr + dep_attr + ann_attr), // abstract: optional Signature [+ Deprecated] [+ anns]
                Some(code) => {
                    u2(&mut out, 1 + sig_attr + dep_attr + ann_attr); // Code [+ Signature] [+ Deprecated] [+ anns]
                    u2(&mut out, code_attr_name);
                    let code_len = code.len();
                    let sm_overhead = match &m.stackmap {
                        None => 0,
                        Some(sm) => 2 + 4 + sm.len(), // name_idx + length + body
                    };
                    // LineNumberTable: name(2)+len(4)+count(2)+entries*(start_pc 2 + line 2).
                    let lnt_overhead = if m.lnt.is_empty() {
                        0
                    } else {
                        2 + 4 + 2 + m.lnt.len() * 4
                    };
                    // LocalVariableTable: name(2)+len(4)+count(2)+entries*(start 2+len 2+name 2+desc 2+slot 2).
                    let lvt_overhead = if m.lvt.is_empty() {
                        0
                    } else {
                        2 + 4 + 2 + m.lvt.len() * 10
                    };
                    let num_code_attrs: u16 = u16::from(m.stackmap.is_some())
                        + u16::from(!m.lnt.is_empty())
                        + u16::from(!m.lvt.is_empty());
                    // Code attr body: max_stack(2) + max_locals(2) + code_len(4) + code + exception_count(2) + exceptions + code_attrs_count(2) + [line/local/stackmap]
                    let attr_len = 2
                        + 2
                        + 4
                        + code_len
                        + 2
                        + m.exceptions.len() * 8
                        + 2
                        + lnt_overhead
                        + lvt_overhead
                        + sm_overhead;
                    u4(&mut out, attr_len as u32);
                    u2(&mut out, m.max_stack);
                    u2(&mut out, m.max_locals);
                    u4(&mut out, code_len as u32);
                    out.extend_from_slice(code);
                    u2(&mut out, m.exceptions.len() as u16); // exception_table_length
                    for &(start, end, handler, catch_type) in &m.exceptions {
                        u2(&mut out, start);
                        u2(&mut out, end);
                        u2(&mut out, handler);
                        u2(&mut out, catch_type);
                    }
                    u2(&mut out, num_code_attrs);
                    // kotlinc's Code sub-attribute order: StackMapTable, then LineNumberTable, then
                    // LocalVariableTable. (A synthesized branch-free member has no StackMapTable.)
                    if let Some(sm) = &m.stackmap {
                        u2(&mut out, stackmap_attr_name.unwrap());
                        u4(&mut out, sm.len() as u32);
                        out.extend_from_slice(sm);
                    }
                    if !m.lnt.is_empty() {
                        u2(&mut out, lnt_attr_name.unwrap());
                        u4(&mut out, (2 + m.lnt.len() * 4) as u32);
                        u2(&mut out, m.lnt.len() as u16);
                        for &(start_pc, line) in &m.lnt {
                            u2(&mut out, start_pc);
                            u2(&mut out, line);
                        }
                    }
                    if !m.lvt.is_empty() {
                        u2(&mut out, lvt_attr_name.unwrap());
                        u4(&mut out, (2 + m.lvt.len() * 10) as u32);
                        u2(&mut out, m.lvt.len() as u16);
                        for &(name_idx, desc_idx, slot, start) in &m.lvt {
                            // A whole-method local (`None`) spans `[0, code_len)`; a mid-method one
                            // (`Some(pc)`) spans `[pc, code_len)`.
                            let start_pc = start.unwrap_or(0);
                            u2(&mut out, start_pc);
                            u2(&mut out, code_len as u16 - start_pc); // length to method end
                            u2(&mut out, name_idx);
                            u2(&mut out, desc_idx);
                            u2(&mut out, slot);
                        }
                    }
                }
            }
            // `Signature` attribute (after `Code`): name_index, length=2, signature UTF8 index.
            if let Some(si) = m.signature {
                u2(&mut out, signature_attr_name.unwrap());
                u4(&mut out, 2);
                u2(&mut out, si);
            }
            // Method-level `RuntimeInvisibleAnnotations` (the annotated return), then
            // `RuntimeInvisibleParameterAnnotations` — kotlinc's order, after `Code`/`Signature`.
            if mria_attr == 1 {
                write_annotation_attr(&mut out, method_invis_ann_name, &m.invisible_anns);
            }
            if ripa_attr == 1 {
                u2(&mut out, ripa_attr_name.unwrap());
                // body: num_parameters(u1) + per-parameter [num_annotations(u2) + annotations].
                let body_len: usize = 1 + m
                    .param_anns
                    .iter()
                    .map(|p| 2 + p.iter().map(|a| a.len()).sum::<usize>())
                    .sum::<usize>();
                u4(&mut out, body_len as u32);
                out.push(m.param_anns.len() as u8);
                for p in &m.param_anns {
                    u2(&mut out, p.len() as u16);
                    for a in p {
                        out.extend_from_slice(a);
                    }
                }
            }
            // `Deprecated` attribute: a zero-length attribute (name_index, length=0).
            if dep_attr == 1 {
                u2(&mut out, deprecated_attr_name.unwrap());
                u4(&mut out, 0);
            }
        }
        // Assemble the class attribute table in kotlinc's fixed order. `self.class_attributes` is empty
        // in practice (nothing pushes to it outside `finish`); it is prepended to preserve the API.
        let mut ordered: Vec<(u16, Vec<u8>)> = std::mem::take(&mut self.class_attributes);
        if let Some(sig) = self.class_signature {
            let mut body = Vec::new();
            u2(&mut body, sig);
            ordered.push((signature_attr_name.unwrap(), body));
        }
        ordered.extend(
            [
                inner_classes_attr,
                sourcefile_attr,
                deprecated_attr,
                rva_attr,
                bootstrap_attr,
            ]
            .into_iter()
            .flatten(),
        );
        u2(&mut out, ordered.len() as u16);
        for (name, bytes) in &ordered {
            u2(&mut out, *name);
            u4(&mut out, bytes.len() as u32);
            out.extend_from_slice(bytes);
        }
        out
    }
}

fn u2(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_be_bytes());
}
fn u4(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_be_bytes());
}

/// The pc just past the FIRST store instruction that writes local `slot` — i.e. where a variable stored
/// there becomes live. Walks the bytecode opcode-by-opcode via [`super::inline::instruction_len`] (so
/// an operand byte that happens to equal a store opcode is skipped). `None` if no such store exists.
/// Covers both the compact (`istore_1`) and indexed (`istore <slot>`, `wide istore <slot>`) store forms.
fn first_store_end(code: &[u8], slot: u16) -> Option<usize> {
    let mut pc = 0usize;
    while pc < code.len() {
        let op = code[pc];
        let len = super::inline::instruction_len(code, pc)?;
        let stored = match op {
            // Indexed stores: `istore/lstore/fstore/dstore/astore <u1 index>`.
            0x36..=0x3a => u16::from(code[pc + 1]) == slot,
            // Compact stores: istore_0..3 (0x3b-0x3e), lstore_0..3 (0x3f-0x42), fstore_0..3 (0x43-0x46),
            // dstore_0..3 (0x47-0x4a), astore_0..3 (0x4b-0x4e) — slot = (op - base) % 4.
            0x3b..=0x4e => u16::from((op - 0x3b) % 4) == slot,
            // Wide store: `wide <istore..astore> <u2 index>`.
            0xc4 if matches!(code.get(pc + 1), Some(0x36..=0x3a)) => {
                u16::from_be_bytes(code.get(pc + 2..pc + 4)?.try_into().ok()?) == slot
            }
            _ => false,
        };
        if stored {
            return Some(pc + len);
        }
        pc += len;
    }
    None
}

/// Write a `Runtime[In]VisibleAnnotations` attribute: `name_index`, `length`, `num_annotations`, then
/// the pre-encoded `annotation` structures. No-op when there are no annotations.
fn write_annotation_attr(out: &mut Vec<u8>, name_index: Option<u16>, anns: &[Vec<u8>]) {
    if anns.is_empty() {
        return;
    }
    u2(
        out,
        name_index.expect("annotation attr name interned when a field carries annotations"),
    );
    let body_len = 2 + anns.iter().map(|a| a.len()).sum::<usize>();
    u4(out, body_len as u32);
    u2(out, anns.len() as u16);
    for a in anns {
        out.extend_from_slice(a);
    }
}

// ---- CodeBuilder: opcode emission with automatic max_stack/max_locals tracking ----------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Label(u32);

pub struct CodeBuilder {
    pub bytes: Vec<u8>,
    pub max_stack: u16,
    pub max_locals: u16,
    cur_stack: i32,
    labels: Vec<usize>, // label id -> bound byte offset (usize::MAX until bound)
    fixups: Vec<(usize, u32)>, // (operand position, label id) to patch in link()
    /// Exception-table entries by label: `(start, end, handler, catch_type)`, resolved in `link()`.
    exceptions: Vec<(Label, Label, Label, u16)>,
    /// Whether this method creates a lambda object (new $ClassName$lambda$N). When true, we must
    /// emit a StackMapTable so the Java 25 type-checking verifier accepts the class.
    pub needs_stackmap: bool,
    /// Frames to include in the StackMapTable: (label_id, locals, stack).
    /// Added via `add_frame_if_new`; first registration for a given label wins.
    frames: Vec<(u32, Vec<VerifType>, Vec<VerifType>)>,
}

impl CodeBuilder {
    pub fn new(arg_locals: u16) -> CodeBuilder {
        CodeBuilder {
            bytes: Vec::new(),
            max_stack: 0,
            max_locals: arg_locals,
            cur_stack: 0,
            labels: Vec::new(),
            fixups: Vec::new(),
            exceptions: Vec::new(),
            needs_stackmap: false,
            frames: Vec::new(),
        }
    }

    /// Mark that this method creates a lambda object. Causes a StackMapTable to be emitted.
    pub fn set_needs_stackmap(&mut self) {
        self.needs_stackmap = true;
    }

    /// Whether this method has any registered StackMapTable frames (⇒ `build_stackmap` emits one).
    pub fn has_frames(&self) -> bool {
        !self.frames.is_empty()
    }

    /// The recorded frames resolved to byte offsets: `(offset, locals, stack)` for each bound label.
    /// Used to relocate a spliced lambda body's own frames into the host method. Unbound labels (offset
    /// `usize::MAX`) are dropped.
    pub fn resolved_frames(&self) -> Vec<(usize, Vec<VerifType>, Vec<VerifType>)> {
        self.frames
            .iter()
            .filter_map(|(lid, locals, stack)| {
                let off = self.labels.get(*lid as usize).copied()?;
                (off != usize::MAX).then(|| (off, locals.clone(), stack.clone()))
            })
            .collect()
    }

    /// Record the frame at `label` (given locals + stack) if not already recorded.
    /// First registration wins — early callers capture the "outer" scope before inner vars appear.
    /// `stack` is the operand-stack verification types at this label (empty in most cases).
    pub fn add_frame_if_new(
        &mut self,
        label: Label,
        locals: Vec<VerifType>,
        stack: Vec<VerifType>,
    ) {
        let lid = label.0;
        if !self.frames.iter().any(|(id, _, _)| *id == lid) {
            self.frames.push((lid, locals, stack));
        }
    }

    /// Build the StackMapTable attribute body. Returns `None` when no frames are needed.
    ///
    /// `initial_locals` is the method-entry frame the first entry compresses against; `None` means
    /// the baseline could not be derived (malformed descriptor) — then the first entry is written
    /// as a `full_frame`, which is always verifiable, instead of risking a false "same" match
    /// against a wrong baseline.
    fn build_stackmap(
        &self,
        initial_locals: Option<&[VerifType]>,
        cp: &mut ConstPool,
    ) -> Option<Vec<u8>> {
        if self.frames.is_empty() {
            return None;
        }
        // Resolve label ids to bytecode offsets and sort by offset.
        let code_len = self.bytes.len();
        let mut entries: Vec<(u32, &Vec<VerifType>, &Vec<VerifType>)> = self
            .frames
            .iter()
            .map(|(lid, locals, stack)| (self.labels[*lid as usize] as u32, locals, stack))
            // Drop frames whose offset is outside the bytecode (e.g. an `end` label bound one past
            // the last `ireturn`/`athrow` when every branch of a `when` diverges). The JVM verifier
            // rejects StackMapTable entries with out-of-range offsets.
            .filter(|(off, _, _)| (*off as usize) < code_len)
            .collect();
        entries.sort_by_key(|&(off, _, _)| off);
        // Multiple labels may be bound at the same offset (e.g. `next` and `end` in an all-diverging
        // `when`). Keep only the first frame at each offset; duplicates would underflow the delta.
        entries.dedup_by_key(|(off, _, _)| *off);

        let mut body = Vec::new();
        u2(&mut body, entries.len() as u16);

        // Emit each frame in kotlinc's COMPRESSED form vs the previous frame (initial frame = the
        // method-entry locals): same/same_extended, same_locals_1_stack_item[/extended], chop, append,
        // or full_frame. Offset deltas: first = offset; subsequent = offset - prev_offset - 1.
        // The `as u16` delta casts cannot truncate: a `Code` attribute's code array is capped at
        // 65535 bytes (JVMS §4.7.3) and every entry offset was filtered to `< code_len` above.
        let mut prev_off: i64 = -1;
        // `None` = no usable baseline yet (malformed descriptor): the first frame is forced to
        // `full_frame`. Borrows (the baseline, then each emitted frame's locals) — no per-frame clone.
        let mut prev_locals: Option<&[VerifType]> = initial_locals;
        fn full(
            body: &mut Vec<u8>,
            delta: u16,
            locals: &[VerifType],
            stack: &[VerifType],
            cp: &mut ConstPool,
        ) {
            body.push(255);
            u2(body, delta);
            u2(body, locals.len() as u16);
            for vt in locals {
                write_verif_type(vt, body, cp);
            }
            u2(body, stack.len() as u16);
            for vt in stack {
                write_verif_type(vt, body, cp);
            }
        }
        for (offset, locals, stack) in entries {
            let delta = if prev_off < 0 {
                offset
            } else {
                offset - prev_off as u32 - 1
            } as u16;
            prev_off = offset as i64;
            let (same_locals, shares_prefix, p) = match prev_locals {
                Some(prev) => {
                    let common = locals.len().min(prev.len());
                    let prefix_eq = locals[..common]
                        .iter()
                        .zip(&prev[..common])
                        .all(|(c, p)| verif_eq(p, c, cp));
                    (
                        locals.len() == prev.len() && prefix_eq,
                        prefix_eq,
                        prev.len(),
                    )
                }
                None => (false, false, 0),
            };
            let n = locals.len();
            if stack.is_empty() && same_locals {
                if delta <= 63 {
                    body.push(delta as u8); // same_frame
                } else {
                    body.push(251); // same_frame_extended
                    u2(&mut body, delta);
                }
            } else if stack.is_empty() && shares_prefix && n > p && n - p <= 3 {
                body.push((251 + (n - p)) as u8); // append_frame
                u2(&mut body, delta);
                for vt in &locals[p..] {
                    write_verif_type(vt, &mut body, cp);
                }
            } else if stack.is_empty() && shares_prefix && p > n && p - n <= 3 {
                body.push((251 - (p - n)) as u8); // chop_frame
                u2(&mut body, delta);
            } else if stack.len() == 1 && same_locals {
                if delta <= 63 {
                    body.push(64 + delta as u8); // same_locals_1_stack_item
                } else {
                    body.push(247); // same_locals_1_stack_item_frame_extended
                    u2(&mut body, delta);
                }
                write_verif_type(&stack[0], &mut body, cp);
            } else {
                full(&mut body, delta, locals, stack, cp);
            }
            prev_locals = Some(locals);
        }
        Some(body)
    }

    /// Register a `try` range `[start, end)` guarded by a handler at `handler`, catching `catch_type`
    /// (a constant-pool class index, or 0 for catch-all).
    pub fn add_exception(&mut self, start: Label, end: Label, handler: Label, catch_type: u16) {
        self.exceptions.push((start, end, handler, catch_type));
    }

    /// Resolve the exception table to byte offsets (call after all labels are bound, e.g. in `link`).
    /// Drops degenerate ranges where `start >= end` (an empty protected region — e.g. an empty `try`
    /// body — protects nothing, and an empty range is an illegal `Code` exception-table entry).
    pub fn resolved_exceptions(&self) -> Vec<(u16, u16, u16, u16)> {
        self.exceptions
            .iter()
            .map(|&(s, e, h, t)| {
                (
                    self.labels[s.0 as usize] as u16,
                    self.labels[e.0 as usize] as u16,
                    self.labels[h.0 as usize] as u16,
                    t,
                )
            })
            .filter(|&(start, end, _, _)| start < end)
            .collect()
    }

    /// The current (linearly tracked) operand-stack height.
    pub fn stack_height(&self) -> i32 {
        self.cur_stack
    }

    /// Append a pre-assembled, pool-relocated, **branchless** inline body (from `inline::splice_branchless`)
    /// at the call site. The arguments are already on the stack (`arg_words` slots); the body's prologue
    /// stores them into locals `base..top_local`, runs, and leaves `ret_words` slots. `body_stack` is the
    /// body's own peak operand height. No StackMapTable frame is recorded (the bytes contain no branch).
    pub fn splice_inline(
        &mut self,
        bytes: &[u8],
        body_stack: u16,
        top_local: u16,
        arg_words: i32,
        ret_words: i32,
    ) {
        let baseline = self.cur_stack - arg_words; // stack height once the prologue consumes the args
        if top_local > self.max_locals {
            self.max_locals = top_local;
        }
        // Peak is the larger of the args-present prologue height and the body's internal peak.
        let peak = (baseline + arg_words).max(baseline + body_stack as i32);
        if peak > self.max_stack as i32 {
            self.max_stack = peak as u16;
        }
        self.bytes.extend_from_slice(bytes);
        self.cur_stack = baseline + ret_words;
        if self.cur_stack > self.max_stack as i32 {
            self.max_stack = self.cur_stack as u16;
        }
    }

    /// Force the current operand-stack height (e.g. an exception handler is entered with the caught
    /// exception already on the stack). Keeps `max_stack` correct across non-linear control flow.
    pub fn set_stack(&mut self, n: u16) {
        self.cur_stack = n as i32;
        if n > self.max_stack {
            self.max_stack = n;
        }
    }

    // ---- branches & labels ----
    pub fn new_label(&mut self) -> Label {
        let id = self.labels.len() as u32;
        self.labels.push(usize::MAX);
        Label(id)
    }
    pub fn bind(&mut self, l: Label) {
        self.labels[l.0 as usize] = self.bytes.len();
    }
    /// Bind a label at an explicit byte offset (used to attach a relocated StackMapTable frame to a
    /// position inside a spliced inline body, which is appended as raw bytes).
    pub fn bind_at(&mut self, l: Label, offset: usize) {
        self.labels[l.0 as usize] = offset;
    }
    fn branch(&mut self, opcode: u8, l: Label, delta: i32) {
        self.bytes.push(opcode);
        let pos = self.bytes.len();
        self.fixups.push((pos, l.0));
        self.bytes.extend_from_slice(&[0, 0]);
        self.adjust(delta);
    }
    pub fn goto(&mut self, l: Label) {
        self.branch(0xa7, l, 0);
    }
    pub fn ifeq(&mut self, l: Label) {
        self.branch(0x99, l, -1);
    }
    pub fn ifne(&mut self, l: Label) {
        self.branch(0x9a, l, -1);
    }
    pub fn if_icmpeq(&mut self, l: Label) {
        self.branch(0x9f, l, -2);
    }
    pub fn if_icmpne(&mut self, l: Label) {
        self.branch(0xa0, l, -2);
    }
    pub fn if_icmplt(&mut self, l: Label) {
        self.branch(0xa1, l, -2);
    }
    pub fn if_icmpge(&mut self, l: Label) {
        self.branch(0xa2, l, -2);
    }
    pub fn if_icmpgt(&mut self, l: Label) {
        self.branch(0xa3, l, -2);
    }
    pub fn if_icmple(&mut self, l: Label) {
        self.branch(0xa4, l, -2);
    }
    pub fn lcmp(&mut self) {
        self.op(0x94, -3);
    }
    pub fn dcmpg(&mut self) {
        self.op(0x98, -3);
    }
    pub fn dcmpl(&mut self) {
        self.op(0x97, -3);
    }
    pub fn ifnull(&mut self, l: Label) {
        self.branch(0xc6, l, -1);
    }
    pub fn ifnonnull(&mut self, l: Label) {
        self.branch(0xc7, l, -1);
    }
    pub fn iflt(&mut self, l: Label) {
        self.branch(0x9b, l, -1);
    }
    pub fn ifge(&mut self, l: Label) {
        self.branch(0x9c, l, -1);
    }
    pub fn ifgt(&mut self, l: Label) {
        self.branch(0x9d, l, -1);
    }
    pub fn ifle(&mut self, l: Label) {
        self.branch(0x9e, l, -1);
    }

    /// Resolve all branch offsets. Call once after the method body is built.
    pub fn link(&mut self) {
        for &(pos, lid) in &self.fixups {
            let target = self.labels[lid as usize];
            debug_assert!(target != usize::MAX, "unbound label {lid}");
            let off = target as i64 - (pos - 1) as i64; // opcode is 1 byte before operand
            let b = (off as i16).to_be_bytes();
            self.bytes[pos] = b[0];
            self.bytes[pos + 1] = b[1];
        }
    }

    /// Ensure the local-variable table is at least `n` slots.
    pub fn ensure_locals(&mut self, n: u16) {
        if n > self.max_locals {
            self.max_locals = n;
        }
    }

    fn adjust(&mut self, delta: i32) {
        self.cur_stack += delta;
        if self.cur_stack < 0 {
            self.cur_stack = 0; // defensive; a real bug would surface in the verifier
        }
        if self.cur_stack as u16 > self.max_stack {
            self.max_stack = self.cur_stack as u16;
        }
    }

    fn op(&mut self, byte: u8, stack_delta: i32) {
        self.bytes.push(byte);
        self.adjust(stack_delta);
    }
    fn op_u1(&mut self, byte: u8, arg: u8, stack_delta: i32) {
        self.bytes.push(byte);
        self.bytes.push(arg);
        self.adjust(stack_delta);
    }
    fn op_u2(&mut self, byte: u8, arg: u16, stack_delta: i32) {
        self.bytes.push(byte);
        self.bytes.extend_from_slice(&arg.to_be_bytes());
        self.adjust(stack_delta);
    }

    // loads (push) — `wide` slots (long/double) push 2 but JVM stack words; we count words.
    pub fn iload(&mut self, idx: u16) {
        self.load(0x15, idx, 1);
    }
    pub fn lload(&mut self, idx: u16) {
        self.load(0x16, idx, 2);
    }
    pub fn fload(&mut self, idx: u16) {
        self.load(0x17, idx, 1);
    }
    pub fn dload(&mut self, idx: u16) {
        self.load(0x18, idx, 2);
    }
    pub fn aload(&mut self, idx: u16) {
        self.load(0x19, idx, 1);
    }
    fn load(&mut self, base: u8, idx: u16, words: i32) {
        // Slots 0-3 use the compact single-byte form (`iload_0`..`aload_3` = 0x1a + (base-0x15)*4 +
        // idx), matching kotlinc; slots 4-255 use the generic `<op> <u1 index>` form; slots >= 256
        // don't fit one byte and need a `wide` (0xc4) prefix + u2 index (else the index truncates,
        // aliasing a low slot — a VerifyError).
        if idx <= 3 {
            self.op(0x1a + (base - 0x15) * 4 + idx as u8, words);
        } else if idx <= 0xff {
            self.op_u1(base, idx as u8, words);
        } else {
            self.op_wide(base, idx, words);
        }
    }

    pub fn istore(&mut self, idx: u16) {
        self.store(0x36, idx, 1);
    }
    pub fn lstore(&mut self, idx: u16) {
        self.store(0x37, idx, 2);
    }
    pub fn fstore(&mut self, idx: u16) {
        self.store(0x38, idx, 1);
    }
    pub fn dstore(&mut self, idx: u16) {
        self.store(0x39, idx, 2);
    }
    pub fn astore(&mut self, idx: u16) {
        self.store(0x3a, idx, 1);
    }
    fn store(&mut self, base: u8, idx: u16, words: i32) {
        // Slots 0-3 use the compact single-byte form (`istore_0`..`astore_3` = 0x3b + (base-0x36)*4 +
        // idx), matching kotlinc; slots 4-255 use the generic `<op> <u1 index>` form; slots >= 256
        // need a `wide` (0xc4) prefix + u2 index (else `idx as u8` truncates to a low live slot).
        if idx <= 3 {
            self.op(0x3b + (base - 0x36) * 4 + idx as u8, -words);
        } else if idx <= 0xff {
            self.op_u1(base, idx as u8, -words);
        } else {
            self.op_wide(base, idx, -words);
        }
        self.ensure_locals(idx + words as u16);
    }

    /// `wide <op> <u2 index>` (JVMS §6.5 `wide`): the `wide`-prefixed form of a local load/store for a
    /// slot index that doesn't fit one byte (>= 256).
    fn op_wide(&mut self, op: u8, idx: u16, stack_delta: i32) {
        self.bytes.push(0xc4);
        self.bytes.push(op);
        self.bytes.extend_from_slice(&idx.to_be_bytes());
        self.adjust(stack_delta);
    }

    // int constants
    pub fn push_int(&mut self, v: i32, cw: &mut ClassWriter) {
        match v {
            -1..=5 => self.op((0x03i16 + v as i16) as u8, 1), // iconst_m1..iconst_5 = 0x02..0x08
            -128..=127 => self.op_u1(0x10, v as u8, 1),       // bipush
            -32768..=32767 => self.op_u2(0x11, v as u16, 1),  // sipush
            _ => {
                let i = cw.const_int(v);
                self.ldc(i);
            }
        }
    }
    pub fn push_long(&mut self, v: i64, cw: &mut ClassWriter) {
        if v == 0 {
            self.op(0x09, 2); // lconst_0
        } else if v == 1 {
            self.op(0x0a, 2); // lconst_1
        } else {
            let i = cw.const_long(v);
            self.op_u2(0x14, i, 2); // ldc2_w
        }
    }
    pub fn push_float(&mut self, v: f32, cw: &mut ClassWriter) {
        let i = cw.const_float(v);
        self.ldc(i); // float is one slot
    }
    pub fn push_double(&mut self, v: f64, cw: &mut ClassWriter) {
        let i = cw.const_double(v);
        self.op_u2(0x14, i, 2); // ldc2_w
    }
    pub fn push_string(&mut self, s: &str, cw: &mut ClassWriter) {
        let i = cw.const_string(s);
        self.ldc(i);
    }
    /// `ldc <class>` — push a `Class` constant (e.g. `A.class`).
    pub fn ldc_class(&mut self, internal: &str, cw: &mut ClassWriter) {
        let i = cw.class_ref(internal);
        self.ldc(i);
    }
    fn ldc(&mut self, idx: u16) {
        if idx <= 255 {
            self.op_u1(0x12, idx as u8, 1); // ldc
        } else {
            self.op_u2(0x13, idx, 1); // ldc_w
        }
    }

    // arithmetic (pop 2 push 1 => -1 for int/ref words; long/double pop 4 push 2 => -2)
    pub fn iadd(&mut self) {
        self.op(0x60, -1);
    }
    pub fn isub(&mut self) {
        self.op(0x64, -1);
    }
    pub fn imul(&mut self) {
        self.op(0x68, -1);
    }
    pub fn idiv(&mut self) {
        self.op(0x6c, -1);
    }
    pub fn irem(&mut self) {
        self.op(0x70, -1);
    }
    pub fn ladd(&mut self) {
        self.op(0x61, -2);
    }
    pub fn lsub(&mut self) {
        self.op(0x65, -2);
    }
    pub fn lmul(&mut self) {
        self.op(0x69, -2);
    }
    pub fn ldiv(&mut self) {
        self.op(0x6d, -2);
    }
    pub fn lrem(&mut self) {
        self.op(0x71, -2);
    }
    pub fn dadd(&mut self) {
        self.op(0x63, -2);
    }
    pub fn dsub(&mut self) {
        self.op(0x67, -2);
    }
    pub fn dmul(&mut self) {
        self.op(0x6b, -2);
    }
    pub fn ddiv(&mut self) {
        self.op(0x6f, -2);
    }
    pub fn drem(&mut self) {
        self.op(0x73, -2);
    }
    pub fn fadd(&mut self) {
        self.op(0x62, -1);
    }
    pub fn fsub(&mut self) {
        self.op(0x66, -1);
    }
    pub fn fmul(&mut self) {
        self.op(0x6a, -1);
    }
    pub fn fdiv(&mut self) {
        self.op(0x6e, -1);
    }
    pub fn frem(&mut self) {
        self.op(0x72, -1);
    }
    /// `fcmpg`: pops two floats, pushes an int (-1/0/1).
    pub fn fcmpg(&mut self) {
        self.op(0x96, -1);
    }
    pub fn fcmpl(&mut self) {
        self.op(0x95, -1);
    }

    // conversions
    pub fn i2l(&mut self) {
        self.op(0x85, 1);
    }
    pub fn i2d(&mut self) {
        self.op(0x87, 1);
    }
    pub fn l2d(&mut self) {
        self.op(0x8a, 0);
    }
    pub fn i2f(&mut self) {
        self.op(0x86, 0);
    }
    pub fn l2f(&mut self) {
        self.op(0x89, -1);
    }
    pub fn f2d(&mut self) {
        self.op(0x8d, 1);
    }
    pub fn l2i(&mut self) {
        self.op(0x88, -1);
    }
    pub fn f2i(&mut self) {
        self.op(0x8b, 0);
    }
    pub fn f2l(&mut self) {
        self.op(0x8c, 1);
    }
    pub fn d2i(&mut self) {
        self.op(0x8e, -1);
    }
    pub fn d2l(&mut self) {
        self.op(0x8f, 0);
    }
    pub fn d2f(&mut self) {
        self.op(0x90, -1);
    }
    /// `iinc index, const` — increment a local int in place (no stack effect). A slot index >= 256
    /// needs the `wide` (0xc4) form (`wide iinc <u2 index> <s2 const>`).
    pub fn iinc(&mut self, idx: u16, delta: i8) {
        if idx <= 0xff {
            self.bytes.push(0x84);
            self.bytes.push(idx as u8);
            self.bytes.push(delta as u8);
        } else {
            self.bytes.push(0xc4);
            self.bytes.push(0x84);
            self.bytes.extend_from_slice(&idx.to_be_bytes());
            self.bytes.extend_from_slice(&(delta as i16).to_be_bytes());
        }
        self.ensure_locals(idx + 1);
    }
    pub fn i2b(&mut self) {
        self.op(0x91, 0);
    }
    pub fn i2c(&mut self) {
        self.op(0x92, 0);
    }
    pub fn i2s(&mut self) {
        self.op(0x93, 0);
    }

    // returns
    pub fn ireturn(&mut self) {
        self.op(0xac, -1);
    }
    pub fn lreturn(&mut self) {
        self.op(0xad, -2);
    }
    pub fn freturn(&mut self) {
        self.op(0xae, -1);
    }
    pub fn dreturn(&mut self) {
        self.op(0xaf, -2);
    }
    pub fn areturn(&mut self) {
        self.op(0xb0, -1);
    }
    pub fn ret_void(&mut self) {
        self.op(0xb1, 0);
    }

    // calls / fields. `arg_words`/`ret_words` describe the stack effect from the descriptor.
    pub fn invokestatic(&mut self, methodref: u16, arg_words: i32, ret_words: i32) {
        self.op_u2(0xb8, methodref, ret_words - arg_words);
    }
    pub fn invokevirtual(&mut self, methodref: u16, arg_words: i32, ret_words: i32) {
        // pops receiver + args, pushes return
        self.op_u2(0xb6, methodref, ret_words - arg_words - 1);
    }
    /// `invokeinterface <iface-methodref> <count> 0` — `count` = receiver + arg words.
    pub fn invokeinterface(&mut self, iref: u16, arg_words: i32, ret_words: i32) {
        self.bytes.push(0xb9);
        self.bytes.extend_from_slice(&iref.to_be_bytes());
        self.bytes.push((arg_words + 1) as u8); // count includes the receiver
        self.bytes.push(0);
        self.adjust(ret_words - arg_words - 1);
    }
    /// `invokedynamic <indy-const> 0 0` — pops `arg_words`, pushes the call-site result (`ret_words`).
    pub fn invokedynamic(&mut self, indy_index: u16, arg_words: i32, ret_words: i32) {
        self.bytes.push(0xba);
        self.bytes.extend_from_slice(&indy_index.to_be_bytes());
        self.bytes.push(0);
        self.bytes.push(0);
        self.adjust(ret_words - arg_words);
    }
    pub fn getstatic(&mut self, fieldref: u16, words: i32) {
        self.op_u2(0xb2, fieldref, words);
    }
    pub fn putstatic(&mut self, fieldref: u16, words: i32) {
        self.op_u2(0xb3, fieldref, -words);
    }
    /// `getfield`: pops objectref, pushes the field value (`words` wide).
    pub fn getfield(&mut self, fieldref: u16, words: i32) {
        self.op_u2(0xb4, fieldref, words - 1);
    }
    /// `putfield`: pops objectref + value (`words` wide).
    pub fn putfield(&mut self, fieldref: u16, words: i32) {
        self.op_u2(0xb5, fieldref, -(1 + words));
    }
    pub fn pop(&mut self) {
        self.op(0x57, -1);
    }
    pub fn pop2(&mut self) {
        self.op(0x58, -2);
    }
    pub fn dup(&mut self) {
        self.op(0x59, 1);
    }

    // ---- arrays ----
    /// `arraylength`: pops arrayref, pushes int.
    pub fn arraylength(&mut self) {
        self.op(0xbe, 0);
    }
    /// `newarray <atype>`: pops count, pushes a primitive arrayref. (boolean=4 char=5 float=6
    /// double=7 byte=8 short=9 int=10 long=11)
    pub fn newarray(&mut self, atype: u8) {
        self.op_u1(0xbc, atype, 0);
    }
    /// `anewarray <class>`: pops count, pushes a reference arrayref.
    pub fn anewarray(&mut self, class_index: u16) {
        self.op_u2(0xbd, class_index, 0);
    }
    /// Array load `Xaload`: pops arrayref + index, pushes a value `words` wide.
    pub fn array_load(&mut self, opcode: u8, words: i32) {
        self.op(opcode, words - 2);
    }
    /// Array store `Xastore`: pops arrayref + index + value (`words` wide).
    pub fn array_store(&mut self, opcode: u8, words: i32) {
        self.op(opcode, -(2 + words));
    }
    pub fn ixor(&mut self) {
        self.op(0x82, -1);
    }
    pub fn iand(&mut self) {
        self.op(0x7e, -1);
    }
    pub fn ior(&mut self) {
        self.op(0x80, -1);
    }
    pub fn ishl(&mut self) {
        self.op(0x78, -1);
    }
    pub fn ishr(&mut self) {
        self.op(0x7a, -1);
    }
    pub fn iushr(&mut self) {
        self.op(0x7c, -1);
    }
    // Long bitwise/shift: `and`/`or`/`xor` pop two longs (push one) → -2; shifts take long + int → -1.
    pub fn land(&mut self) {
        self.op(0x7f, -2);
    }
    pub fn lor(&mut self) {
        self.op(0x81, -2);
    }
    pub fn lxor(&mut self) {
        self.op(0x83, -2);
    }
    pub fn lshl(&mut self) {
        self.op(0x79, -1);
    }
    pub fn lshr(&mut self) {
        self.op(0x7b, -1);
    }
    pub fn lushr(&mut self) {
        self.op(0x7d, -1);
    }
    pub fn aconst_null(&mut self) {
        self.op(0x01, 1);
    }
    pub fn lconst_0(&mut self) {
        self.op(0x09, 2);
    }
    pub fn fconst_0(&mut self) {
        self.op(0x0b, 1);
    }
    pub fn dconst_0(&mut self) {
        self.op(0x0e, 2);
    }
    pub fn athrow(&mut self) {
        self.op(0xbf, -1);
    }

    /// `instanceof <class>` (pops ref, pushes int 0/1).
    pub fn instance_of(&mut self, class_index: u16) {
        self.op_u2(0xc1, class_index, 0);
    }
    /// `checkcast <class>` (ref -> ref).
    pub fn checkcast(&mut self, class_index: u16) {
        self.op_u2(0xc0, class_index, 0);
    }
    /// `if_acmpeq` — branch if two refs ARE the same object.
    pub fn if_acmpeq(&mut self, l: Label) {
        self.branch(0xa5, l, -2);
    }
    /// `if_acmpne` — branch if two refs are not the same object.
    pub fn if_acmpne(&mut self, l: Label) {
        self.branch(0xa6, l, -2);
    }

    /// `new <class>` (push uninitialized ref).
    pub fn new_obj(&mut self, class_index: u16) {
        self.op_u2(0xbb, class_index, 1);
    }
    pub fn invokespecial(&mut self, methodref: u16, arg_words: i32, ret_words: i32) {
        self.op_u2(0xb7, methodref, ret_words - arg_words - 1);
    }
}

/// Constant-pool index of a `Class` entry, exposed for `new`.
impl ClassWriter {
    pub fn class_ref(&mut self, internal: &str) -> u16 {
        self.cp.class(internal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inner_classes_emitted_only_when_referenced() {
        // A registered nested class that is NOT referenced as a class constant emits no entry.
        let mut unref = ClassWriter::new("C", "java/lang/Object");
        unref.add_inner_class(InnerClassSpec {
            inner: "C$Companion".to_string(),
            outer: Some("C".to_string()),
            name: Some("Companion".to_string()),
            access: ACC_PUBLIC | ACC_STATIC | ACC_FINAL,
        });
        let bytes = unref.finish();
        assert!(!bytes.windows(12).any(|w| w == b"InnerClasses"));

        // Once referenced (a class constant for the nested class exists), the entry appears.
        let mut refd = ClassWriter::new("C", "java/lang/Object");
        refd.add_inner_class(InnerClassSpec {
            inner: "C$Companion".to_string(),
            outer: Some("C".to_string()),
            name: Some("Companion".to_string()),
            access: ACC_PUBLIC | ACC_STATIC | ACC_FINAL,
        });
        let _ = refd.class_ref("C$Companion"); // reference it as a class constant
        let bytes = refd.finish();
        let has = |n: &[u8]| bytes.windows(n.len()).any(|w| w == n);
        assert!(has(b"InnerClasses"));
        assert!(has(b"C$Companion"));
        assert!(has(b"Companion"));
    }

    #[test]
    fn header_and_version() {
        let cw = ClassWriter::new("FooKt", "java/lang/Object");
        let bytes = cw.finish();
        assert_eq!(&bytes[0..4], &[0xCA, 0xFE, 0xBA, 0xBE]);
        assert_eq!(u16::from_be_bytes([bytes[6], bytes[7]]), MAJOR_JAVA8);
    }

    #[test]
    fn jvm_target_sets_class_major_version() {
        let mut cw = ClassWriter::new("FooKt", "java/lang/Object");
        cw.set_major(69); // -jvm-target 25
        let bytes = cw.finish();
        assert_eq!(u16::from_be_bytes([bytes[6], bytes[7]]), 69);
    }

    #[test]
    fn source_file_attribute_emitted_and_ordered() {
        let mut cw = ClassWriter::new("FooKt", "java/lang/Object");
        cw.set_source_file(Some("Foo.kt".to_string()));
        let bytes = cw.finish();
        // The `SourceFile` name and the source basename are both interned.
        let has = |needle: &[u8]| bytes.windows(needle.len()).any(|w| w == needle);
        assert!(has(b"SourceFile"));
        assert!(has(b"Foo.kt"));

        // Default (no source set) emits no SourceFile.
        let plain = ClassWriter::new("FooKt", "java/lang/Object").finish();
        assert!(!plain.windows(6).any(|w| w == b"Foo.kt"));
    }

    #[test]
    fn add_method_builds() {
        let mut cw = ClassWriter::new("FooKt", "java/lang/Object");
        let mut code = CodeBuilder::new(2); // (II) => 2 locals
        code.iload(0);
        code.iload(1);
        code.iadd();
        code.ireturn();
        assert_eq!(code.max_stack, 2);
        assert_eq!(code.max_locals, 2);
        cw.add_method(ACC_PUBLIC | ACC_STATIC | ACC_FINAL, "add", "(II)I", &code);
        let bytes = cw.finish();
        // methods_count is the u16 right after fields_count(0); just sanity-check non-trivial size.
        assert!(bytes.len() > 40);
    }

    #[test]
    fn constant_pool_dedups() {
        let mut cp = ConstPool::default();
        let a = cp.utf8("X");
        let b = cp.utf8("X");
        assert_eq!(a, b);
    }

    #[test]
    fn long_takes_two_slots() {
        let mut cp = ConstPool::default();
        let _l = cp.long(5);
        let after = cp.utf8("next");
        // long consumed 2 slots (indices 1,2), so next utf8 is index 3
        assert_eq!(after, 3);
    }

    #[test]
    fn local_index_over_255_uses_wide_prefix() {
        // A local slot >= 256 doesn't fit a one-byte operand: the JVM requires a `wide` (0xc4)
        // prefix + u2 index. Without it the index truncates (`256 as u8` == 0), silently
        // aliasing slot 0 and corrupting a live local (VerifyError "Bad local variable type").
        let mut code = CodeBuilder::new(300);
        let start = code.bytes.len();
        code.astore(256);
        // wide astore 256: 0xc4, 0x3a, 0x01, 0x00
        assert_eq!(&code.bytes[start..], &[0xc4, 0x3a, 0x01, 0x00]);

        let start = code.bytes.len();
        code.aload(256);
        assert_eq!(&code.bytes[start..], &[0xc4, 0x19, 0x01, 0x00]);

        // Slots that still fit a byte keep the compact single-byte form.
        let start = code.bytes.len();
        code.astore(255);
        assert_eq!(&code.bytes[start..], &[0x3a, 0xff]);

        // `iinc` on a wide slot also needs the prefix (0xc4, 0x84, u2 index, s2 const).
        let start = code.bytes.len();
        code.iinc(300, 1);
        assert_eq!(&code.bytes[start..], &[0xc4, 0x84, 0x01, 0x2c, 0x00, 0x01]);
    }

    /// A method/class marked deprecated must carry the zero-length `Deprecated` attribute — kotlinc
    /// emits it for a `@Serializable` class's `$$serializer` object and `get<Prop>$annotations()`
    /// markers, and ASM surfaces it as `ACC_DEPRECATED` (0x20000), which the downstream ABI gate compares.
    #[test]
    fn deprecated_attribute_emitted_on_marked_method_and_class() {
        fn contains(hay: &[u8], needle: &[u8]) -> bool {
            hay.windows(needle.len()).any(|w| w == needle)
        }

        // No deprecation ⇒ the `Deprecated` attribute name is never interned.
        let mut plain = ClassWriter::new("FooKt", "java/lang/Object");
        let mut code = CodeBuilder::new(0);
        code.ret_void();
        plain.add_method(ACC_PUBLIC | ACC_STATIC, "m", "()V", &code);
        assert!(!contains(&plain.finish(), b"Deprecated"));

        // Marking the method and the class both intern + emit the attribute.
        let mut cw = ClassWriter::new("FooKt", "java/lang/Object");
        let mut code = CodeBuilder::new(0);
        code.ret_void();
        cw.add_method(ACC_PUBLIC | ACC_STATIC, "m", "()V", &code);
        cw.mark_method_deprecated("m", "()V");
        cw.set_deprecated();
        assert!(contains(&cw.finish(), b"Deprecated"));
    }

    #[test]
    fn stack_tracking_for_constants() {
        let mut cw = ClassWriter::new("FooKt", "java/lang/Object");
        let mut code = CodeBuilder::new(0);
        code.push_int(1000, &mut cw); // sipush (+1)
        code.push_int(7, &mut cw); // iconst-ish (+1) => stack 2
        code.iadd(); // -1 => 1
        code.ireturn();
        assert_eq!(code.max_stack, 2);
    }
}
