//! A hand-written JVM class-file writer (the format is well-specified; no external crate).
//! Targets major version 50 (Java 6). Methods that create lambda objects (new $lambda$N) emit a
//! StackMapTable attribute so the type-checking verifier on Java 25+ accepts them.

use crate::kt_string::KtString;
use std::collections::HashMap;
use std::rc::Rc;

pub const ACC_PUBLIC: u16 = 0x0001;
pub const ACC_PRIVATE: u16 = 0x0002;
pub const ACC_PROTECTED: u16 = 0x0004;
pub const ACC_STATIC: u16 = 0x0008;
pub const ACC_FINAL: u16 = 0x0010;
pub const ACC_SUPER: u16 = 0x0020;
pub const ACC_INTERFACE: u16 = 0x0200;
pub const ACC_ABSTRACT: u16 = 0x0400;
pub const ACC_ANNOTATION: u16 = 0x2000;
pub const ACC_ENUM: u16 = 0x4000;

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
/// One pool entry a `super(…)` argument's evaluation interns before the super `<init>` Methodref,
/// in code order: a construction's Class ref, a string constant's CONSTANT_String, or a
/// constructor Methodref (`class Basic : Engine(Cfg(false), "basic")`).
pub enum SeedSuperArg {
    Class(String),
    Str(KtString),
    Ctor { owner: String, desc: String },
}

/// A primary constructor with defaulted parameters: kotlinc emits the `$default` `<init>` overload
/// right after the primary one, interning its marker descriptor, the default STRING constants and
/// the delegating own-`<init>` Methodref BEFORE the accessors — the seeder mirrors that window.
pub struct SeedCtorDefaults {
    pub marker_desc: String,
    pub string_consts: Vec<KtString>,
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
    pub string_const: Option<KtString>,
    /// `(value class internal name, `constructor-impl` descriptor)` when the initializer CONSTRUCTS a
    /// value class (`val k: K = K("OK")`). The store is then `ldc <const>; invokestatic
    /// K.constructor-impl; putfield`, so the factory's entries intern between the constant and the
    /// field — exactly where kotlinc puts them.
    pub value_class_ctor: Option<(String, String)>,
}

/// Primary-constructor JVM generic `Signature`, passed to
/// [`ClassWriter::seed_plain_class_pool`] at the constructor's interning position.
pub struct MemberSignatures<'a> {
    /// The primary constructor's generic `Signature` (`(Ljava/util/List<Ljava/lang/String;>;)V`).
    pub ctor: Option<&'a str>,
}

/// One declared data-class property accessor at its JVM method-header interning position.
pub struct DataAccessorInfo {
    pub name: String,
    pub desc: String,
    /// 0 = getter, 1 = unguarded setter, 2 = non-null reference setter.
    pub setter_kind: u8,
    pub signature: Option<String>,
}

/// Extra per-member data a `data class` needs when seeding [`ClassWriter::seed_data_class_pool`], all
/// index-parallel to its `fields`. Bundled to keep the seeder's arity in check.
pub struct DataMemberInfo<'a> {
    /// Declared property accessors in emission order. These precede `componentN` in a data class.
    pub accessors: &'a [DataAccessorInfo],
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
    /// A `CONSTANT_Utf8` whose value is a string CONSTANT that no Rust `String` can spell — one
    /// containing an unpaired surrogate. `CONSTANT_Utf8` is modified UTF-8, which encodes every
    /// UTF-16 code unit (including a lone surrogate) as its own sequence, so the class-file format
    /// carries these fine; only the in-compiler `String` cannot.
    ///
    /// Disjoint from `Utf8` by construction — a value reachable as a `String` is always interned as
    /// `Utf8` — so the two never split one value across two pool entries.
    Utf8Units(Vec<u16>),
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
        let physical = super::names::classfile_internal_name(internal_name);
        let n = self.utf8(&physical);
        self.intern(Const::Class(n))
    }
    fn string(&mut self, s: &str) -> u16 {
        let n = self.utf8(s);
        self.intern(Const::String(n))
    }
    /// Intern the `CONSTANT_Utf8` for a Kotlin string VALUE, keeping its code units.
    fn utf8_kt(&mut self, s: &KtString) -> u16 {
        match s.as_str() {
            Some(text) => self.utf8(text),
            None => self.intern(Const::Utf8Units(s.units().collect())),
        }
    }
    fn string_kt(&mut self, s: &KtString) -> u16 {
        let n = self.utf8_kt(s);
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
    fn class_names(&self) -> Vec<String> {
        self.entries
            .iter()
            .filter_map(|entry| {
                let Const::Class(name_index) = entry else {
                    return None;
                };
                match self.entry_at(*name_index) {
                    Some(Const::Utf8(name)) => Some(name.clone()),
                    _ => None,
                }
            })
            .collect()
    }
    fn utf8_value(&self, index: u16) -> Option<&str> {
        match self.entry_at(index) {
            Some(Const::Utf8(value)) => Some(value),
            _ => None,
        }
    }

    /// Whether a typed constant-pool descriptor mentions `internal`. Arbitrary `Utf8` entries are
    /// deliberately excluded: a source string literal can itself spell `Lowner/Nested;`.
    fn typed_descriptor_mentions(&self, internal: &str) -> bool {
        let needle = format!("L{internal};");
        self.entries.iter().any(|entry| {
            let descriptor = match entry {
                Const::NameAndType(_, descriptor) | Const::MethodType(descriptor) => *descriptor,
                _ => return false,
            };
            self.utf8_value(descriptor)
                .is_some_and(|value| value.contains(&needle))
        })
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
                Const::Utf8Units(units) => {
                    out.push(1);
                    let b = crate::metadata::encoding::modified_utf8_units(units.iter().copied());
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

/// `(name_idx, desc_idx, slot, start, length)` for `LocalVariableTable`.
type LvtEntry = (u16, u16, u16, Option<u16>, Option<u16>);

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
    lvt: Vec<LvtEntry>,
    /// Method-level `RuntimeVisibleAnnotations` (each entry a pre-encoded annotation) — a
    /// RUNTIME-retained user annotation applied to the function (`@Deprecated`, `@Marker(...)`).
    visible_anns: Vec<Vec<u8>>,
    /// Method-level `RuntimeInvisibleAnnotations` (each entry a pre-encoded annotation) — e.g. the
    /// `@org.jetbrains.annotations.NotNull` kotlinc puts on a non-null reference RETURN, and
    /// BINARY-retained user annotations.
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

/// A field whose constant-pool interning is DEFERRED to the field-table visit: kotlinc's writer
/// visits methods first and fields last, so a field entry the method bodies never introduced (a
/// `const val`'s name + `ConstantValue`, a facade backing field) interns AFTER every method window.
/// Realized into a [`FieldInfo`] (appended after the eagerly-added fields) by `intern_late_fields`.
struct LateField {
    access: u16,
    name: String,
    desc: String,
    signature: Option<String>,
    /// The `ConstantValue` payload, interned at realization (`None` for a `<clinit>`-initialized field).
    const_value: Option<crate::ir::IrConst>,
    /// BINARY-retention nullability annotation type descriptor (`Lorg/jetbrains/annotations/NotNull;`).
    ann: Option<String>,
    /// USER annotations on the field (`@Target(FIELD)` on the property), split by retention. Held
    /// unencoded so their types intern in the field-table window, where kotlinc interns them —
    /// before the class's own `@Metadata`, not after the methods.
    user_visible: Vec<crate::ir::AppliedAnnotation>,
    user_invisible: Vec<crate::ir::AppliedAnnotation>,
    /// `true` ⇒ the realized field LEADS the field table (before the eagerly-added fields) — the
    /// `Companion` field's position — while its pool entries still intern late.
    lead: bool,
}

/// Partition retained declaration annotations into the two class-file attributes the JVM splits them
/// across: `RuntimeVisibleAnnotations` (Kotlin's RUNTIME, the default) then `RuntimeInvisibleAnnotations`
/// (BINARY). Common IR carries one list per declaration; this boundary is the only place the split
/// exists. SOURCE-retained applications never reach the IR.
fn split_declaration_annotations(
    annotations: &crate::ir::DeclarationAnnotations,
) -> (
    Vec<crate::ir::AppliedAnnotation>,
    Vec<crate::ir::AppliedAnnotation>,
) {
    use crate::types::AnnotationRetention;
    let of = |keep: fn(&AnnotationRetention) -> bool| {
        annotations
            .iter()
            .filter(|retained| keep(&retained.retention))
            .map(|retained| retained.annotation.clone())
            .collect()
    };
    (
        of(|retention| {
            matches!(
                retention,
                AnnotationRetention::Default | AnnotationRetention::Runtime
            )
        }),
        of(|retention| matches!(retention, AnnotationRetention::Binary)),
    )
}

pub struct ClassWriter {
    cp: ConstPool,
    /// Emit (and therefore seed the pool for) `Intrinsics.checkNotNullParameter` guards. Cleared by
    /// `-Xno-param-assertions`.
    param_assertions: bool,
    access: u16,
    this_class: u16,
    super_class: u16,
    interfaces: Vec<u16>,
    fields: Vec<FieldInfo>,
    late_fields: Vec<LateField>,
    methods: Vec<MethodInfo>,
    class_attributes: Vec<(u16, Vec<u8>)>, // (name_index, raw bytes)
    /// Constant-pool index of the class's generic `Signature` VALUE, when it has one.
    class_signature: Option<u16>,
    /// Encoded `annotation` structures (type_index + element_value_pairs, WITHOUT the outer count) for the
    /// class's single `RuntimeVisibleAnnotations` attribute — `@Metadata` and user annotations both append
    /// here so `finish` writes ONE attribute (two would be invalid per JVMS §4.7.16).
    runtime_annotations: Vec<Vec<u8>>,
    invisible_annotations: Vec<Vec<u8>>,
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
    inner_class_resolver: Option<InnerClassResolver>,
    /// Internal names of every ANNOTATION type this class applies (class/field/method/parameter).
    /// An applied annotation appears only as a descriptor string inside the annotation attribute —
    /// never as a class constant — yet kotlinc still gives a nested one an `InnerClasses` entry, so
    /// these count as references alongside the pool's class constants.
    annotation_class_refs: std::collections::HashSet<String>,
    /// Entries for the `PermittedSubclasses` attribute.
    permitted_subclasses: Vec<String>,
    /// Class-file major version to emit (default v52; set via [`ClassWriter::set_major`]).
    major: u16,
    /// Source-file simple name for the `SourceFile` attribute (set via [`ClassWriter::set_source_file`]).
    source_file: Option<String>,
    /// Owner, method name, and descriptor for the `EnclosingMethod` attribute.
    enclosing_method: Option<(String, String, String)>,
    pub internal_name: String,
}

/// One candidate `InnerClasses` entry: the nested class, its enclosing class (`None` for an anonymous
/// local), its simple name (`None` when anonymous), and the entry's access flags.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InnerClassSpec {
    pub inner: String,
    pub outer: Option<String>,
    pub name: Option<String>,
    pub access: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InnerClassDetails {
    pub outer: Option<String>,
    pub name: Option<String>,
    pub access: u16,
}

pub type InnerClassResolver = Rc<dyn Fn(&str) -> Option<InnerClassDetails>>;

impl ClassWriter {
    /// Whether a declared member or typed constant-pool descriptor references `internal`.
    fn descriptor_mentions(&self, internal: &str) -> bool {
        let needle = format!("L{internal};");
        self.fields
            .iter()
            .map(|field| field.desc)
            .chain(self.methods.iter().map(|method| method.desc))
            .any(|descriptor| {
                self.cp
                    .utf8_value(descriptor)
                    .is_some_and(|value| value.contains(&needle))
            })
            || self.cp.typed_descriptor_mentions(internal)
    }

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
            param_assertions: true,
            access: ACC_PUBLIC | ACC_FINAL | ACC_SUPER,
            this_class,
            super_class,
            interfaces: Vec::new(),
            fields: Vec::new(),
            late_fields: Vec::new(),
            methods: Vec::new(),
            class_attributes: Vec::new(),
            class_signature: None,
            runtime_annotations: Vec::new(),
            invisible_annotations: Vec::new(),
            bootstrap_methods: Vec::new(),
            class_deprecated: false,
            deprecated_methods: std::collections::HashSet::new(),
            inner_class_candidates: Vec::new(),
            inner_class_resolver: None,
            annotation_class_refs: std::collections::HashSet::new(),
            permitted_subclasses: Vec::new(),
            major: MAJOR_JAVA8,
            source_file: None,
            enclosing_method: None,
            internal_name: internal_name.to_string(),
        }
    }

    /// Set the class-file major version to emit (kotlinc maps `-jvm-target 25` ⇒ v69). Default v52.
    pub fn set_major(&mut self, major: u16) {
        self.major = major;
    }
    /// The class-file major version (52 = Java 8, 53 = Java 9, …). Codegen that is gated on the target
    /// — e.g. `invokedynamic` string concatenation, which kotlinc emits only for Java 9+ — reads it.
    pub fn major(&self) -> u16 {
        self.major
    }

    /// Set the source-file simple name for the `SourceFile` attribute (e.g. `Foo.kt`). `None` (the
    /// default) emits no attribute.
    pub fn set_source_file(&mut self, name: Option<String>) {
        self.source_file = name;
    }

    /// Set the enclosing class and method for a local class.
    pub fn set_enclosing_method(&mut self, owner: &str, method: &str, descriptor: &str) {
        self.enclosing_method = Some((
            owner.to_string(),
            method.to_string(),
            descriptor.to_string(),
        ));
    }

    /// Set only the enclosing CLASS. The JVM spec allows `method_index = 0` — "not immediately
    /// enclosed by a method or constructor" — and the attribute's presence is what makes reflection
    /// treat the class as local rather than top-level, which is what decides `simpleName`. Used
    /// where the enclosing method's descriptor is not reconstructable; a wrong one would make
    /// `Class.getEnclosingMethod()` throw, while absent is well-defined.
    pub fn set_enclosing_class(&mut self, owner: &str) {
        self.enclosing_method = Some((owner.to_string(), String::new(), String::new()));
    }

    /// Register a candidate `InnerClasses` entry (a nested class in this file). `finish` emits it only
    /// if `inner` is referenced as a class constant. Register the whole file's nest on every writer —
    /// the per-class filter then yields exactly the entries kotlinc emits for that class.
    pub fn add_inner_class(&mut self, spec: InnerClassSpec) {
        // Preserve the first registration because its order affects byte identity.
        if self
            .inner_class_candidates
            .iter()
            .any(|s| s.inner == spec.inner)
        {
            return;
        }
        self.inner_class_candidates.push(spec);
    }

    pub fn set_inner_class_resolver(&mut self, resolver: Option<InnerClassResolver>) {
        self.inner_class_resolver = resolver;
    }

    /// Set the `PermittedSubclasses` entries in emission order.
    pub fn set_permitted_subclasses(&mut self, subclasses: Vec<String>) {
        self.permitted_subclasses = subclasses;
    }

    /// Intern a class constant before natural first use.
    /// Whether `Intrinsics.checkNotNullParameter` machinery is seeded into the pool for non-null
    /// reference constructor parameters. `-Xno-param-assertions` emits no guards, so seeding their
    /// methodref and `String` constants would leave a pool referencing nothing and shift every later
    /// index away from kotlinc's.
    pub fn set_param_assertions(&mut self, enabled: bool) {
        self.param_assertions = enabled;
    }

    pub fn seed_class(&mut self, internal: &str) {
        self.cp.class(internal);
    }

    /// Intern a UTF-8 constant before natural first use.
    pub fn seed_utf8(&mut self, s: &str) {
        self.cp.utf8(s);
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
            visible_anns: Vec::new(),
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

    /// Declare a field whose pool entries intern at the FIELD-TABLE visit (after every method) —
    /// kotlinc's writer order. Use for a field the method bodies don't introduce; a field whose
    /// name/descriptor the bodies DO intern can use either form (the table interning dedups).
    pub fn add_field_late(
        &mut self,
        access: u16,
        name: &str,
        desc: &str,
        const_value: Option<crate::ir::IrConst>,
        ann: Option<&str>,
    ) {
        self.add_field_late_sig(access, name, desc, None, const_value, ann);
    }

    /// Deferred field declaration with an optional generic `Signature` value.
    pub fn add_field_late_sig(
        &mut self,
        access: u16,
        name: &str,
        desc: &str,
        signature: Option<&str>,
        const_value: Option<crate::ir::IrConst>,
        ann: Option<&str>,
    ) {
        self.late_fields.push(LateField {
            access,
            name: name.to_string(),
            desc: desc.to_string(),
            user_visible: Vec::new(),
            user_invisible: Vec::new(),
            signature: signature.map(str::to_string),
            const_value,
            ann: ann.map(str::to_string),
            lead: false,
        });
    }

    /// [`add_field_late`], but the realized field LEADS the field table (kotlinc puts a class's
    /// `Companion` field before the instance fields, while interning it with the field visit).
    pub fn add_field_late_leading(&mut self, access: u16, name: &str, desc: &str) {
        self.late_fields.push(LateField {
            access,
            name: name.to_string(),
            desc: desc.to_string(),
            signature: None,
            const_value: None,
            // The `Companion` field is a non-null reference — kotlinc annotates it.
            ann: Some("Lorg/jetbrains/annotations/NotNull;".to_string()),
            user_visible: Vec::new(),
            user_invisible: Vec::new(),
            lead: true,
        });
    }

    /// Realize every [`LateField`] into the field table, interning name/descriptor/`ConstantValue`/
    /// annotation NOW — called at the earliest class-attribute interning point (`set_kotlin_metadata`
    /// or `finish`), so the entries land after the method windows like kotlinc's field visit.
    fn intern_late_fields(&mut self) {
        let mut lead_at = 0usize;
        for lf in std::mem::take(&mut self.late_fields) {
            let n = self.cp.utf8(&lf.name);
            let d = self.cp.utf8(&lf.desc);
            let signature = lf.signature.as_ref().map(|value| self.cp.utf8(value));
            let cv = lf.const_value.as_ref().and_then(|c| {
                use crate::ir::IrConst;
                Some(match c {
                    IrConst::Boolean(b) => self.const_int(*b as i32),
                    IrConst::Byte(v) => self.const_int(*v as i32),
                    IrConst::Short(v) => self.const_int(*v as i32),
                    IrConst::Int(v) => self.const_int(*v),
                    IrConst::Char(ch) => self.const_int(*ch as i32),
                    IrConst::Long(v) => self.const_long(*v),
                    IrConst::Float(v) => self.const_float(*v),
                    IrConst::Double(v) => self.const_double(*v),
                    IrConst::String(s) => self.const_string_kt(s),
                    IrConst::Null => return None,
                })
            });
            // A user annotation interns before the nullability one, matching the attribute order
            // (`RuntimeVisibleAnnotations` precedes `RuntimeInvisibleAnnotations`).
            let visible_anns: Vec<Vec<u8>> = lf
                .user_visible
                .iter()
                .map(|annotation| self.encode_annotation(annotation))
                .collect();
            let mut invisible_anns: Vec<Vec<u8>> = lf
                .user_invisible
                .iter()
                .map(|annotation| self.encode_annotation(annotation))
                .collect();
            invisible_anns.extend(lf.ann.as_ref().map(|a| {
                let ti = self.cp.utf8(a);
                vec![(ti >> 8) as u8, ti as u8, 0, 0]
            }));
            let info = FieldInfo {
                access: lf.access,
                name: n,
                desc: d,
                signature,
                const_value: cv,
                visible_anns,
                invisible_anns,
            };
            if lf.lead {
                self.fields.insert(lead_at, info);
                lead_at += 1;
            } else {
                self.fields.push(info);
            }
        }
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

    /// Attach user annotations to the most recently added field. The JVM representation boundary
    /// maps semantic retention onto visible/invisible class-file attributes.
    pub fn set_last_field_annotations(&mut self, annotations: &crate::ir::DeclarationAnnotations) {
        let (vis, invis) = self.encode_declaration_annotations(annotations);
        if let Some(f) = self.fields.last_mut() {
            f.visible_anns = vis;
            f.invisible_anns = invis;
        }
    }

    /// Attach user annotations to the most recently DEFERRED field ([`Self::add_field_late_sig`]),
    /// which realizes them when the field table interns.
    pub fn set_last_late_field_annotations(
        &mut self,
        annotations: &crate::ir::DeclarationAnnotations,
    ) {
        // Kept UNENCODED: a deferred field's annotation types must intern in the field-table window,
        // which `intern_late_fields` opens, not here. Only the retention → attribute split happens now.
        let (visible, invisible) = split_declaration_annotations(annotations);
        if let Some(field) = self.late_fields.last_mut() {
            field.user_visible = visible;
            field.user_invisible = invisible;
        }
    }

    /// Encode one `annotation` structure (type_index + element_value_pairs) to a fresh byte buffer.
    fn encode_annotation(&mut self, a: &crate::ir::AppliedAnnotation) -> Vec<u8> {
        let mut body = Vec::new();
        self.ev_annotation(&mut body, a);
        body
    }

    /// Encode retained declaration annotations in class-file attribute order: all visible entries,
    /// then all invisible entries. Common IR carries no JVM attribute split.
    fn encode_declaration_annotations(
        &mut self,
        annotations: &crate::ir::DeclarationAnnotations,
    ) -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
        let (visible, invisible) = split_declaration_annotations(annotations);
        let visible = visible
            .iter()
            .map(|annotation| self.encode_annotation(annotation))
            .collect();
        let invisible = invisible
            .iter()
            .map(|annotation| self.encode_annotation(annotation))
            .collect();
        (visible, invisible)
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
        // The field-table visit precedes the class annotations in kotlinc's writer.
        self.intern_late_fields();
        // kotlinc interns each element's KEY immediately before that element's VALUE constants (mv key
        // then the mv integers, then k key then its integer, …) rather than all keys up front — so the
        // constant pool interleaves keys and values. Match that by interning each key inline.
        let anno_type = self.cp.utf8("Lkotlin/Metadata;");
        // One `annotation` structure (type_index + element_value_pairs) — appended to the shared list so
        // `finish` writes a single `RuntimeVisibleAnnotations` attribute even alongside user annotations.
        let mut body = Vec::new();
        u2(&mut body, anno_type);
        let has_payload = !d1.is_empty() || !d2.is_empty();
        u2(&mut body, if has_payload { 5 } else { 3 });
        let n_mv = self.cp.utf8("mv");
        u2(&mut body, n_mv);
        self.ev_int_array(&mut body, mv);
        let n_k = self.cp.utf8("k");
        u2(&mut body, n_k);
        self.ev_int(&mut body, k);
        let n_xi = self.cp.utf8("xi");
        u2(&mut body, n_xi);
        self.ev_int(&mut body, xi);
        if has_payload {
            let n_d1 = self.cp.utf8("d1");
            u2(&mut body, n_d1);
            self.ev_str_array(&mut body, d1);
            let n_d2 = self.cp.utf8("d2");
            u2(&mut body, n_d2);
            self.ev_str_array(&mut body, d2);
        }
        self.runtime_annotations.push(body);
    }

    /// Add the runtime-visible `DebugMetadata` annotation for a suspend continuation.
    #[allow(clippy::too_many_arguments)]
    pub fn set_debug_metadata(
        &mut self,
        f: &str,
        l: &[i32],
        nl: &[i32],
        i: &[i32],
        s: &[String],
        n: &[String],
        m: &str,
        c: &str,
        v: i32,
    ) {
        let anno_type = self
            .cp
            .utf8("Lkotlin/coroutines/jvm/internal/DebugMetadata;");
        let mut body = Vec::new();
        u2(&mut body, anno_type);
        u2(&mut body, 9); // element_value_pairs: f, l, nl, i, s, n, m, c, v
        let n_f = self.cp.utf8("f");
        u2(&mut body, n_f);
        self.ev_str(&mut body, f);
        let n_l = self.cp.utf8("l");
        u2(&mut body, n_l);
        self.ev_int_array(&mut body, l);
        let n_nl = self.cp.utf8("nl");
        u2(&mut body, n_nl);
        self.ev_int_array(&mut body, nl);
        let n_i = self.cp.utf8("i");
        u2(&mut body, n_i);
        self.ev_int_array(&mut body, i);
        let n_s = self.cp.utf8("s");
        u2(&mut body, n_s);
        self.ev_str_array(&mut body, s);
        let n_n = self.cp.utf8("n");
        u2(&mut body, n_n);
        self.ev_str_array(&mut body, n);
        let n_m = self.cp.utf8("m");
        u2(&mut body, n_m);
        self.ev_str(&mut body, m);
        let n_c = self.cp.utf8("c");
        u2(&mut body, n_c);
        self.ev_str(&mut body, c);
        let n_v = self.cp.utf8("v");
        u2(&mut body, n_v);
        self.ev_int(&mut body, v);
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
    /// An annotation `element_value` holding a Kotlin string VALUE (see [`KtString`]).
    fn ev_str_kt(&mut self, out: &mut Vec<u8>, s: &KtString) {
        out.push(b's');
        let idx = self.cp.utf8_kt(s);
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
                let internal = super::jvm_class_map::to_jvm_type_name(*internal).render();
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
    fn ev_annotation(&mut self, out: &mut Vec<u8>, a: &crate::ir::AppliedAnnotation) {
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

    /// Queue the applied annotations for the class's `RuntimeVisibleAnnotations` (JVMS §4.7.16). They join
    /// any `@Metadata` in the shared list; `finish` writes exactly ONE attribute.
    pub fn set_runtime_annotations(&mut self, anns: &[crate::ir::AppliedAnnotation]) {
        for a in anns {
            let mut body = Vec::new();
            self.ev_annotation(&mut body, a);
            self.runtime_annotations.push(body);
        }
    }

    /// Queue user annotations on a class. Semantic retention chooses the physical class-file
    /// attribute here, once, for every class kind.
    pub fn set_class_annotations(&mut self, annotations: &crate::ir::DeclarationAnnotations) {
        let (visible, invisible) = self.encode_declaration_annotations(annotations);
        self.runtime_annotations.extend(visible);
        self.invisible_annotations.extend(invisible);
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
    /// otherwise reproduce. Call BEFORE any `add_field`/`add_method` for the class. Declared methods
    /// and property accessors then intern at their own exact emission sites.
    #[allow(clippy::too_many_arguments)]
    pub fn seed_plain_class_pool(
        &mut self,
        this_internal: &str,
        super_internal: &str,
        ctor_descs: (&str, &str),
        fields: &[SeedField],
        // Per-member generic `Signature`s (parameterized-type ctor/accessor/field members).
        sigs: &MemberSignatures,
        // The primary ctor's `$default` overload entries (marker desc, default string constants,
        // delegating `<init>` ref) — interned between the ctor and the accessors, kotlinc's order.
        ctor_defaults: Option<&SeedCtorDefaults>,
        // Entries the `super(…)` call's arguments intern in code order, BEFORE the super `<init>`
        // Methodref (`class Basic : Engine(Cfg(false), "basic")`).
        super_arg_entries: &[SeedSuperArg],
    ) {
        let (ctor_desc, super_ctor_desc) = ctor_descs;
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
            if f.ann_kind == 1 && self.param_assertions {
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
        for entry in super_arg_entries {
            match entry {
                SeedSuperArg::Class(internal) => {
                    self.cp.class(internal);
                }
                SeedSuperArg::Str(s) => {
                    self.cp.string_kt(s);
                }
                SeedSuperArg::Ctor { owner, desc } => {
                    self.cp.methodref(owner, "<init>", desc);
                }
            }
        }
        self.cp.methodref(super_internal, "<init>", super_ctor_desc);
        // One `putfield` per property-backed parameter: field name, descriptor, NameAndType, Fieldref.
        for f in fields.iter().filter(|f| f.stores_in_ctor) {
            // A body property's `String` initializer is pushed by `ldc` before its `putfield`.
            if let Some(sc) = &f.string_const {
                self.cp.string_kt(sc);
            }
            if let Some((owner, desc)) = &f.value_class_ctor {
                self.cp.methodref(owner, "constructor-impl", desc);
            }
            self.cp.utf8(&f.name);
            self.cp.utf8(&f.desc);
            self.cp.fieldref(this_internal, &f.name, &f.desc);
        }
        // The constructor's LocalVariableTable strings (`this` and its type); the parameters reuse the
        // field name/descriptor entries interned just above.
        self.cp.utf8("this");
        self.cp.utf8(&format!("L{this_internal};"));
        // The `$default` ctor overload follows the primary immediately: its marker descriptor, the
        // default STRING constants its body `ldc`s (in parameter order), then the NameAndType +
        // Methodref of the delegating `invokespecial` to the real `<init>`.
        if let Some(d) = ctor_defaults {
            self.cp.utf8(&d.marker_desc);
            for s in &d.string_consts {
                self.cp.string_kt(s);
            }
            self.cp.methodref(this_internal, "<init>", ctor_desc);
        }
    }

    /// Seed a `data class`'s synthesized-method constant-pool entries in kotlinc's first-use order,
    /// AFTER [`seed_plain_class_pool`] (which seeds `<init>` and its field stores). It first seeds the
    /// data class's declared property accessors, then mirrors the synthesized bodies kotlinc emits for
    /// `componentN`/`copy`/`copy$default`/`toString`/`hashCode`/`equals`. `fields` is
    /// `(name, jvm_descriptor)` in declaration order; `simple` is the class's simple name.
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

        // Declared property accessors precede the synthesized data members. Ordinary classes intern
        // accessors at their exact declaration sites, but a data class's synthetic-member seeder must
        // preserve this boundary before it interns `componentN`/`copy`/the Object overrides.
        for accessor in info.accessors {
            self.cp.utf8(&accessor.name);
            self.cp.utf8(&accessor.desc);
            if let Some(signature) = &accessor.signature {
                self.cp.utf8(signature);
            }
            if accessor.setter_kind >= 1 {
                self.cp.utf8("<set-?>");
            }
            if accessor.setter_kind == 2 {
                self.cp.string("<set-?>");
            }
        }

        // componentN — each body is a field read; only the method name is new.
        for i in 1..=fields.len() {
            self.cp.utf8(&format!("component{i}"));
        }
        // copy — name, descriptor, its generic Signature (a parameterized ctor param — right after the
        // erased descriptor, kotlinc's order), @NotNull (return), then `new <self>(...)` (ctor Methodref).
        // A `data object` gets none of it: kotlinc synthesizes no `copy`/`copy$default` for a singleton,
        // and seeding the names alone would leave them in the constant pool of a class that has no such
        // method. `fields` here is already sliced to the PRIMARY-CONSTRUCTOR properties, so an empty
        // list is exactly a data declaration with none — which can only be an object.
        if !fields.is_empty() {
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
        }
        // toString. kotlinc's shape depends on the target: `invokedynamic makeConcatWithConstants`
        // (JVM 9+) or a `StringBuilder` chain (below). The body emitter picks the same fork on the
        // class major; seed to match so the pool positions line up.
        self.cp.utf8("toString");
        self.cp.utf8("()Ljava/lang/String;");
        let arrays_to_string_desc = |d: &str| -> &'static str {
            match d {
                "[Z" => "([Z)Ljava/lang/String;",
                "[C" => "([C)Ljava/lang/String;",
                "[B" => "([B)Ljava/lang/String;",
                "[S" => "([S)Ljava/lang/String;",
                "[I" => "([I)Ljava/lang/String;",
                "[J" => "([J)Ljava/lang/String;",
                "[F" => "([F)Ljava/lang/String;",
                "[D" => "([D)Ljava/lang/String;",
                _ => "([Ljava/lang/Object;)Ljava/lang/String;",
            }
        };
        if self.major >= 53 {
            // The recipe: literal segments with a `\u{1}` where each field value interpolates. An
            // array field is rendered by `Arrays.toString` first (interned in field order, before the
            // bootstrap), so its argument type is `String` like any other value.
            let mut recipe = String::new();
            let mut arg_descs = String::new();
            for (i, (name, desc)) in fields.iter().enumerate() {
                recipe.push_str(&if i == 0 {
                    format!("{simple}({name}=")
                } else {
                    format!(", {name}=")
                });
                recipe.push('\u{1}');
                if desc.starts_with('[') {
                    self.cp
                        .methodref("java/util/Arrays", "toString", arrays_to_string_desc(desc));
                    arg_descs.push_str("Ljava/lang/String;");
                } else {
                    arg_descs.push_str(desc);
                }
            }
            recipe.push(')');
            let recipe_idx = self.const_string(&recipe);
            let mh = self.method_handle_static(
                "java/lang/invoke/StringConcatFactory",
                "makeConcatWithConstants",
                "(Ljava/lang/invoke/MethodHandles$Lookup;Ljava/lang/String;\
                 Ljava/lang/invoke/MethodType;Ljava/lang/String;[Ljava/lang/Object;)\
                 Ljava/lang/invoke/CallSite;",
            );
            let bsm = self.add_bootstrap(mh, vec![recipe_idx]);
            self.invoke_dynamic(
                bsm,
                "makeConcatWithConstants",
                &format!("({arg_descs})Ljava/lang/String;"),
            );
        } else {
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
                    // An ARRAY field content-prints via `java.util.Arrays.toString`; its `String`
                    // result reuses the `append(String)` methodref above.
                    self.cp
                        .methodref("java/util/Arrays", "toString", arrays_to_string_desc(desc));
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
        }
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
    /// `CONSTANT_String` for a Kotlin string VALUE (see [`KtString`]).
    pub fn const_string_kt(&mut self, s: &KtString) -> u16 {
        self.cp.string_kt(s)
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
        self.reserve_method_pool_with_annotations(
            name,
            desc,
            signature,
            ann_types,
            &crate::ir::DeclarationAnnotations::default(),
        );
    }

    /// [`Self::reserve_method_pool`] plus the DECLARED annotations' constants. kotlinc interns a
    /// method's header — name, descriptor, `Signature`, its own annotations, then the compiler's
    /// `@NotNull`/`@Nullable` types — before visiting the body, so the payload of a user annotation
    /// must be reserved here rather than when the annotation is attached after code generation.
    /// Encoding is pure interning plus a discarded buffer; attaching later re-encodes and finds the
    /// same entries.
    pub fn reserve_method_pool_with_annotations(
        &mut self,
        name: &str,
        desc: &str,
        signature: Option<&str>,
        ann_types: &[&str],
        annotations: &crate::ir::DeclarationAnnotations,
    ) {
        self.cp.utf8(name);
        self.cp.utf8(desc);
        if let Some(s) = signature {
            self.cp.utf8(s);
        }
        let _ = self.encode_declaration_annotations(annotations);
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
            // `<init>`/`<clinit>` line tables are CURATED after the fact (`set_method_debug` /
            // `set_method_lines` — the class-decl-line super-call entry, per-initializer entries,
            // trailing return). Marks that leaked in through nested initializer blocks would
            // deactivate those "only when empty" fallbacks and ship a partial table — drop them.
            lnt: if name == "<init>" || name == "<clinit>" {
                Vec::new()
            } else {
                code.line_marks().to_vec()
            },
            lvt: if name == "<init>" || name == "<clinit>" {
                Vec::new()
            } else {
                code.local_entries()
                    .iter()
                    // A `LocalVariableTable` `start_pc` must index the code array (JVMS §4.7.13) —
                    // HotSpot's class-file parser rejects the whole class otherwise. A local DECLARED
                    // in a region the emitter dropped as unreachable (`val y: Int = boom() ?: 1`) has
                    // its start recorded past the last instruction and describes no live range, so it
                    // goes with the code. A later live branch target can grow the method past that
                    // offset, so also discard a zero-length entry: its initializing store and entire
                    // source range were dropped even though `start_pc` now happens to index resumed
                    // code. This keeps debug metadata tied to emitted ranges, never offset coincidence.
                    .filter(|(start, len, ..)| {
                        (*start as usize) < code.bytes.len() && *len != Some(0)
                    })
                    .map(|(start, len, slot, nm, ds)| {
                        (
                            self.cp.utf8(nm),
                            self.cp.utf8(ds),
                            *slot,
                            Some(*start),
                            *len,
                        )
                    })
                    .collect()
            },
            visible_anns: Vec::new(),
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

    /// Attach USER annotations to a previously-added method (matched by name+descriptor), split by
    /// retention: RUNTIME → `RuntimeVisibleAnnotations`, BINARY → `RuntimeInvisibleAnnotations` —
    /// the method analogue of [`Self::set_last_field_annotations`]. Interning the annotation types
    /// here fixes their constant-pool position. No-op if the method isn't found.
    pub fn set_method_annotations(
        &mut self,
        name: &str,
        desc: &str,
        annotations: &crate::ir::DeclarationAnnotations,
    ) {
        // Resolve WITHOUT interning first (as `set_method_nullability` does): describing a method
        // that was never emitted must not leave orphan name/descriptor entries in the pool.
        let (Some(n), Some(d)) = (self.cp.lookup_utf8(name), self.cp.lookup_utf8(desc)) else {
            return;
        };
        if !self.methods.iter().any(|m| m.name == n && m.desc == d) {
            return;
        }
        let (vis, invis) = self.encode_declaration_annotations(annotations);
        if let Some(m) = self.methods.iter_mut().find(|m| m.name == n && m.desc == d) {
            m.visible_anns = vis;
            // A DECLARED annotation precedes the compiler's own `@NotNull`/`@Nullable` on the
            // return, whichever order the two setters ran in — kotlinc writes the user's first.
            m.invisible_anns.splice(0..0, invis);
        }
    }

    /// Mark a previously-added method `ACC_SYNTHETIC` (matched by name+descriptor). kotlinc emits a
    /// `@Deprecated(level = HIDDEN)` declaration's realization synthetic: it exists for binary
    /// compatibility and no source-level call may resolve to it. No-op if the method isn't found.
    pub fn set_method_synthetic(&mut self, name: &str, desc: &str) {
        let (Some(n), Some(d)) = (self.cp.lookup_utf8(name), self.cp.lookup_utf8(desc)) else {
            return;
        };
        if let Some(m) = self.methods.iter_mut().find(|m| m.name == n && m.desc == d) {
            m.access |= 0x1000;
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
        // Fill only debug tables that body emission did not produce.
        let (needs_lnt, needs_lvt) = match self.methods.iter().find(|m| m.name == n && m.desc == d)
        {
            Some(m) => (m.lnt.is_empty(), m.lvt.is_empty()),
            None => return,
        };
        if !needs_lnt && !needs_lvt {
            return;
        }
        let lvt: Vec<LvtEntry> = if needs_lvt {
            locals
                .iter()
                .map(|(nm, ds, slot)| (self.cp.utf8(nm), self.cp.utf8(ds), *slot, None, None))
                .collect()
        } else {
            Vec::new()
        };
        if let Some(m) = self.methods.iter_mut().find(|m| m.name == n && m.desc == d) {
            if needs_lnt {
                m.lnt = lnt
                    .map(|(pc, line)| (pc, line as u16))
                    .into_iter()
                    .collect();
            }
            if needs_lvt {
                m.lvt = lvt;
            }
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
                (result_n, result_d, 1, Some(start), None),
                (this_n, this_d, 0, None, None),
            ];
        }
    }

    fn resolve_inner_classes(&mut self) {
        if let Some(resolve) = self.inner_class_resolver.clone() {
            // Class constants first, then annotation types (an applied annotation is a reference
            // even though only its descriptor string reaches the pool). kotlinc's writer sorts the
            // final table by inner name, so collection order does not leak into the attribute.
            let mut referenced = self.cp.class_names();
            let mut annotation_refs: Vec<String> =
                self.annotation_class_refs.iter().cloned().collect();
            annotation_refs.sort();
            referenced.extend(annotation_refs);
            for inner in referenced {
                if self
                    .inner_class_candidates
                    .iter()
                    .any(|candidate| candidate.inner == inner)
                {
                    continue;
                }
                let Some(details) = resolve(&inner) else {
                    continue;
                };
                if let Some(outer) = &details.outer {
                    self.cp.class(outer);
                }
                if let Some(name) = &details.name {
                    self.cp.utf8(name);
                }
                self.add_inner_class(InnerClassSpec {
                    inner,
                    outer: details.outer,
                    name: details.name,
                    access: details.access,
                });
            }
        }
        // kotlinc writes the complete table sorted by inner internal name (`C$Companion`,
        // `C$NestObj`, `C$Nested` — case-sensitive), including classpath-discovered entries.
        self.inner_class_candidates
            .sort_by(|a, b| a.inner.cmp(&b.inner));
    }

    pub fn finish(mut self) -> Vec<u8> {
        // A class that never attached `@Metadata` still realizes its deferred fields first —
        // kotlinc's field visit precedes every class-attribute window.
        self.intern_late_fields();
        self.resolve_inner_classes();
        // The `EnclosingMethod` refs (owner Class, method NameAndType) intern BEFORE the
        // `InnerClasses` entries' refs — kotlinc's attribute visit order on an anonymous class.
        // (The attribute NAME interns later, with the other attribute names.)
        if let Some((owner, method, desc)) = self.enclosing_method.clone() {
            self.cp.class(&owner);
            if !method.is_empty() {
                self.cp.name_and_type(&method, &desc);
            }
        }
        // Every EMITTED `InnerClasses` entry's refs (outer Class, simple name) intern here — before
        // the `SourceFile` value and the attribute names (kotlinc visits the InnerClasses table
        // ahead of both; a nested class's own entry otherwise interned its outer at serialization,
        // after everything else). Only candidates whose inner class is ALREADY a pool constant
        // qualify — an unreferenced candidate emits no entry, and interning it here would falsely
        // mark it referenced.
        let referenced: std::collections::HashSet<String> =
            self.cp.class_names().into_iter().collect();
        let inner_specs: Vec<(String, Option<String>, Option<String>)> = self
            .inner_class_candidates
            .iter()
            .filter(|candidate| {
                candidate.outer.as_deref() == Some(self.internal_name.as_str())
                    || referenced.contains(&candidate.inner)
                    || self.annotation_class_refs.contains(&candidate.inner)
                    || self.descriptor_mentions(&candidate.inner)
            })
            .map(|candidate| {
                (
                    candidate.inner.clone(),
                    candidate.outer.clone(),
                    candidate.name.clone(),
                )
            })
            .collect();
        for (inner, outer, name) in inner_specs {
            self.cp.class(&inner);
            if let Some(outer) = outer {
                self.cp.class(&outer);
            }
            if let Some(name) = name {
                self.cp.utf8(&name);
            }
        }
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
            Dep,
            Ria,
            Rva,
            Smt,
            Ripa,
            Sig,
        }
        // A field's `Signature` attribute name interns BEFORE its `RuntimeInvisibleAnnotations` and before
        // `Code` — kotlinc visits fields first, and a field's `Signature` attribute precedes its
        // annotations (`class C(val xs: List<String>)` pool: `Signature`, `RuntimeInvisibleAnnotations`,
        // `Code`). The later `signature_attr_name` intern dedups onto this index.
        // `ConstantValue` and field-level `RuntimeInvisibleAnnotations` intern in the field-first
        // window too — in PER-FIELD first-use order over the field table (a leading `const val`
        // interns `ConstantValue` before any annotation name; a facade whose consts come LAST
        // interns the annotation name first).
        let mut field_sig_name: Option<u16> = None;
        let mut constval_attr_name: Option<u16> = None;
        let mut field_ria: Option<u16> = None;
        // A field's own `RuntimeVisibleAnnotations` name interns in this same field-first window; the
        // method/class-level uses below dedup onto it.
        let mut field_rva: Option<u16> = None;
        for i in 0..self.fields.len() {
            if self.fields[i].signature.is_some() && field_sig_name.is_none() {
                field_sig_name = Some(self.cp.utf8("Signature"));
            }
            if self.fields[i].const_value.is_some() && constval_attr_name.is_none() {
                constval_attr_name = Some(self.cp.utf8("ConstantValue"));
            }
            if !self.fields[i].visible_anns.is_empty() && field_rva.is_none() {
                field_rva = Some(self.cp.utf8("RuntimeVisibleAnnotations"));
            }
            if !self.fields[i].invisible_anns.is_empty() && field_ria.is_none() {
                field_ria = Some(self.cp.utf8("RuntimeInvisibleAnnotations"));
            }
        }
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
            // ASM interns StackMapTable during code emission, before debug attributes.
            if m.stackmap.is_some() && !seq.contains(&An::Smt) {
                seq.push(An::Smt);
            }
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
            // `Deprecated` interns with the method that carries it, before that method's
            // annotation attribute names — kotlinc's per-method order.
            if self.deprecated_methods.contains(&(m.name, m.desc)) && !seq.contains(&An::Dep) {
                seq.push(An::Dep);
            }
            if !m.visible_anns.is_empty() && !seq.contains(&An::Rva) {
                seq.push(An::Rva);
            }
            if !m.invisible_anns.is_empty() && !seq.contains(&An::Ria) {
                seq.push(An::Ria);
            }
            if !m.param_anns.is_empty() && !seq.contains(&An::Ripa) {
                seq.push(An::Ripa);
            }
        }
        let (mut lnt_attr_name, mut lvt_attr_name, mut stackmap_attr_name, mut ripa_attr_name) =
            (None, None, None, None);
        // A method-level `RuntimeVisibleAnnotations` shares its attribute-name entry with the class
        // annotations when both are present; the class table is written later, so intern on first
        // METHOD use here and let that later write reuse the index.
        let mut vis_ann_name: Option<u16> = field_rva;
        // `Deprecated` interned from the per-method sequence above; the class-level fallback below
        // dedups onto it when a method already introduced the name.
        let mut method_dep_name: Option<u16> = None;
        // A method-level RIA first use dedups onto the field-level index when both are present.
        let mut invis_ann_name = field_ria;
        for k in &seq {
            match k {
                An::Lnt => lnt_attr_name = Some(self.cp.utf8("LineNumberTable")),
                An::Lvt => lvt_attr_name = Some(self.cp.utf8("LocalVariableTable")),
                An::Ria => invis_ann_name = Some(self.cp.utf8("RuntimeInvisibleAnnotations")),
                An::Rva => vis_ann_name = Some(self.cp.utf8("RuntimeVisibleAnnotations")),
                An::Dep => method_dep_name = Some(self.cp.utf8("Deprecated")),
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
        let method_vis_ann_name = vis_ann_name;
        // The `Signature` attribute name: reuse the early field-Signature index when a field carries one
        // (interned before `Code`), else intern here if a METHOD carries a signature. Only interned when
        // actually used — an unused entry would diverge from kotlinc's output for non-generic classes.
        let class_has_sig = self.class_signature.is_some();
        let signature_attr_name = field_sig_name.or_else(|| {
            (class_has_sig || self.methods.iter().any(|m| m.signature.is_some()))
                .then(|| self.cp.utf8("Signature"))
        });
        // Intern `Deprecated` only if the class or a method carries it; a method's own use already
        // interned it in the per-method sequence above.
        let deprecated_attr_name = method_dep_name.or_else(|| {
            (self.class_deprecated || !self.deprecated_methods.is_empty())
                .then(|| self.cp.utf8("Deprecated"))
        });
        // Field annotation attribute names, interned only when a field actually carries them.
        let field_vis_ann_name = field_rva;
        // Field-level `RuntimeInvisibleAnnotations` reuses the name interned before `Code` (dedup).
        let field_invis_ann_name = if self.fields.iter().any(|f| !f.invisible_anns.is_empty()) {
            invis_ann_name
        } else {
            None
        };
        // Attribute construction must finish before serializing the constant pool.
        let inner_classes_attr = {
            // A class must declare its OWN member classes even when its code never mentions them: the
            // JVM cross-checks the outer's and the inner's attributes and throws
            // `IncompatibleClassChangeError: … disagree on InnerClasses attribute` from
            // `getEnclosingClass`/`getDeclaringClass` when only one side carries the entry. The
            // reference filter below is right for every OTHER entry (a nested class this file merely
            // uses), and kotlinc emits both sides for its own nest too.
            let own_member =
                |spec: &InnerClassSpec| spec.outer.as_deref() == Some(self.internal_name.as_str());
            let referenced: Vec<InnerClassSpec> = self
                .inner_class_candidates
                .iter()
                .filter(|s| {
                    own_member(s)
                        || self.cp.has_class(&s.inner)
                        || self.descriptor_mentions(&s.inner)
                })
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
        // `SourceFile`: name_index + a 2-byte body = the CP index of the source-file UTF8 (its VALUE was
        // The `EnclosingMethod` attribute NAME interns between `InnerClasses` and `SourceFile`
        // (kotlinc's anonymous-class attribute order); its refs were interned at the top of
        // `finish`, so this build only adds the name.
        let enclosing_method_attr = self.enclosing_method.take().map(|(owner, method, desc)| {
            let name = self.cp.utf8("EnclosingMethod");
            let class_idx = self.cp.class(&owner);
            // An empty method name is the class-only form: `method_index = 0`.
            let nat_idx = if method.is_empty() {
                0
            } else {
                self.cp.name_and_type(&method, &desc)
            };
            let mut body = Vec::new();
            u2(&mut body, class_idx);
            u2(&mut body, nat_idx);
            (name, body)
        });
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
        // ONE `RuntimeInvisibleAnnotations` for the BINARY-retained class annotations, written directly
        // after the visible ones — the order kotlinc emits them in.
        let ria_attr = if !self.invisible_annotations.is_empty() {
            let name = self.cp.utf8("RuntimeInvisibleAnnotations");
            let mut body = Vec::new();
            u2(&mut body, self.invisible_annotations.len() as u16);
            for a in &self.invisible_annotations {
                body.extend_from_slice(a);
            }
            Some((name, body))
        } else {
            None
        };
        // `BootstrapMethods` — its name interns AFTER `SourceFile`/`RuntimeVisibleAnnotations` (kotlinc's
        // order); handle/argument indices were already interned by `add_bootstrap` during emission.
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
        // Class-level `Deprecated` (zero-length). Its name was interned above with the method one.
        let deprecated_attr = self
            .class_deprecated
            .then(|| (deprecated_attr_name.unwrap(), Vec::new()));
        // `InnerClasses` (kotlinc's first class attribute): one entry per registered nested class that
        // this class actually references as a class constant (the `has_class` filter), in registration
        // order. `inner` is already interned (that is why it passed the filter); `outer`/`name` intern
        // here — before the pool is serialized.
        let permitted_attr = (!self.permitted_subclasses.is_empty()).then(|| {
            let name = self.cp.utf8("PermittedSubclasses");
            let mut body = Vec::new();
            u2(&mut body, self.permitted_subclasses.len() as u16);
            for sub in &self.permitted_subclasses {
                let idx = self.cp.class(sub);
                u2(&mut body, idx);
            }
            (name, body)
        });
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
            let mrva_attr: u16 = u16::from(!m.visible_anns.is_empty());
            let mria_attr: u16 = u16::from(!m.invisible_anns.is_empty());
            let ripa_attr: u16 = u16::from(!m.param_anns.is_empty());
            let ann_attr = mrva_attr + mria_attr + ripa_attr;
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
                        for &(name_idx, desc_idx, slot, start, length) in &m.lvt {
                            // Missing bounds extend from method start or to method end.
                            let start_pc = start.unwrap_or(0);
                            u2(&mut out, start_pc);
                            u2(&mut out, length.unwrap_or(code_len as u16 - start_pc));
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
            // `Deprecated` (a zero-length attribute) precedes the annotation attributes, as
            // kotlinc writes them: `Code`, `Signature`, `Deprecated`, then the annotations.
            if dep_attr == 1 {
                u2(&mut out, deprecated_attr_name.unwrap());
                u4(&mut out, 0);
            }
            // Method-level `RuntimeVisibleAnnotations` (declared user annotations), then
            // `RuntimeInvisibleAnnotations` (the annotated return + BINARY-retained user
            // annotations), then `RuntimeInvisibleParameterAnnotations` — kotlinc's order.
            if mrva_attr == 1 {
                write_annotation_attr(&mut out, method_vis_ann_name, &m.visible_anns);
            }
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
                enclosing_method_attr,
                sourcefile_attr,
                deprecated_attr,
                rva_attr,
                ria_attr,
                permitted_attr,
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
    /// `LineNumberTable` marks recorded during emission: `(start_pc, line)`. See [`Self::mark_line`].
    line_marks: Vec<(u16, u16)>,
    /// `(start_pc, length, slot, name, descriptor)` entries in scope-close order.
    local_entries: Vec<(u16, Option<u16>, u16, String, String)>,
    /// Whether the instruction stream is currently UNREACHABLE: an unconditional terminator
    /// (`goto`/`athrow`/a `*return`) has been emitted and no label has been bound since. Instructions
    /// appended in that state are dead code the type-checking verifier rejects — it demands a
    /// stack-map frame at the first instruction after a terminator ("Expecting a stack map frame"),
    /// and the tracked operand height there is meaningless ("Operand stack overflow"). Dead
    /// instructions are therefore DROPPED rather than emitted.
    ///
    /// This is what makes a diverging expression usable in VALUE position generically: every
    /// construct that consumes a value (a local's store, an outer call's `invoke`, a method's
    /// implicit `return`) emits its consuming opcodes after the value, and when the value diverges
    /// those opcodes are exactly this dead straight-line region. No per-construct divergence check is
    /// needed at any consuming site.
    ///
    /// Reachability resumes only where control can actually ARRIVE: a label some ALREADY-EMITTED
    /// branch targets ([`Self::bind`] checks `fixups`), or an exception handler
    /// ([`Self::bind_handler`], reachable via the exception edge rather than a branch). Binding a
    /// label whose only branches were themselves dropped does NOT revive — that is what keeps a
    /// branchy sub-expression inside a dead region (`g(boom(), if (b) 1 else 2)`) from having its
    /// tail resurrected around the hole where its condition used to be. All other bookkeeping
    /// (operand-height tracking, `max_stack`, `max_locals`) runs unchanged while dead, so a revival
    /// point sees exactly the state it saw before this suppression existed.
    dead: bool,
    /// Labels bound while `dead` and NOT revived, by label id. They sit at the end of a dropped
    /// region, which is also where the next live instruction lands — so their frames would collide
    /// with (and, being registered first, out-rank) the live label's frame in `build_stackmap`'s
    /// same-offset dedup. Their frames are dropped instead. Indexed like `labels`; `false` for a
    /// label bound normally, and for one never bound at all.
    dead_bound: Vec<bool>,
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
            line_marks: Vec::new(),
            local_entries: Vec::new(),
            dead: false,
            dead_bound: Vec::new(),
        }
    }

    /// Whether `label` was bound inside a dropped dead region (see `dead_bound`).
    fn is_dead_bound(&self, label: u32) -> bool {
        self.dead_bound
            .get(label as usize)
            .copied()
            .unwrap_or(false)
    }

    /// Record a local; a missing length extends to method end.
    pub fn add_local_entry(
        &mut self,
        start: u16,
        length: Option<u16>,
        slot: u16,
        name: &str,
        desc: &str,
    ) {
        self.local_entries
            .push((start, length, slot, name.to_string(), desc.to_string()));
    }

    pub fn local_entries(&self) -> &[(u16, Option<u16>, u16, String, String)] {
        &self.local_entries
    }

    /// Record a `LineNumberTable` entry for `line` starting at the CURRENT pc. Deduped: a re-mark
    /// of the line already in effect is dropped; a second mark at the same pc overwrites (the
    /// statement that actually begins an instruction wins, matching kotlinc's per-statement entries).
    pub fn mark_line(&mut self, line: u32) {
        if self.dead {
            return; // the statement it would mark is dropped dead code (see `dead`)
        }
        if self.bytes.len() > u16::MAX as usize {
            return; // past the classfile pc range — an entry would silently wrap
        }
        let line = line.min(u16::MAX as u32) as u16;
        let pc = self.bytes.len() as u16;
        match self.line_marks.last_mut() {
            Some((lpc, ll)) if *lpc == pc => *ll = line,
            Some((_, ll)) if *ll == line => {}
            _ => self.line_marks.push((pc, line)),
        }
    }

    /// The recorded `LineNumberTable` marks (empty for a body emitted without line info).
    pub fn line_marks(&self) -> &[(u16, u16)] {
        &self.line_marks
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
    /// `usize::MAX`) and labels bound inside a dropped dead region are dropped.
    pub fn resolved_frames(&self) -> Vec<(usize, Vec<VerifType>, Vec<VerifType>)> {
        self.frames
            .iter()
            .filter(|(lid, _, _)| !self.is_dead_bound(*lid))
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
            // A label bound inside a DROPPED dead region sits at the same offset as the next live
            // instruction. Its frame describes state that no longer exists there, and being registered
            // first it would win the same-offset dedup below over the live label's frame.
            .filter(|(lid, _, _)| !self.is_dead_bound(*lid))
            .map(|(lid, locals, stack)| (self.labels[*lid as usize] as u32, locals, stack))
            // Drop frames whose offset is outside the bytecode (e.g. an `end` label bound one past
            // the last `ireturn`/`athrow` when every branch of a `when` diverges). The JVM verifier
            // rejects StackMapTable entries with out-of-range offsets.
            .filter(|(off, _, _)| (*off as usize) < code_len)
            .collect();
        entries.sort_by_key(|&(off, _, _)| off);
        // Several labels can be bound at the SAME offset — a loop's `end` and the following
        // statement's `start`, or `next`/`end` in an all-diverging `when`. One frame is emitted for
        // that offset, and it must hold on EVERY edge reaching it, so the frames are MERGED: the
        // locals are their common prefix, everything past the first divergence reverting to `top`.
        //
        // Keeping the first (a plain dedup) silently claimed a local that a later edge does not have.
        // `for (v in …) …` immediately followed by `while (…) …` binds the loop's end and the while's
        // head at one offset; the `for` frame still named its synthetic index, the `while`'s back edge
        // chopped it, and the back edge became narrower than its own target — a class that fails
        // verification ("Inconsistent stackmap frames").
        let merged: Vec<(u32, Vec<VerifType>, Vec<VerifType>)> = {
            let mut out: Vec<(u32, Vec<VerifType>, Vec<VerifType>)> = Vec::new();
            for (off, locals, stack) in entries {
                match out.last_mut() {
                    Some((previous_off, previous_locals, previous_stack))
                        if *previous_off == off =>
                    {
                        let common = previous_locals
                            .iter()
                            .zip(locals.iter())
                            .take_while(|(a, b)| verif_eq(a, b, cp))
                            .count();
                        previous_locals.truncate(common);
                        // An operand stack that differs between edges into one offset is a lowering
                        // bug, not something a frame can reconcile; keep the shorter so the entry
                        // stays describable rather than asserting one edge's view over the other.
                        if stack.len() < previous_stack.len() {
                            previous_stack.clone_from(stack);
                        }
                    }
                    _ => out.push((off, locals.clone(), stack.clone())),
                }
            }
            out
        };
        let entries = merged;

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
        // Owned: each entry's locals are moved out of `entries` as it is emitted, so the previous
        // frame cannot be a borrow into that vector.
        let mut prev_locals: Option<Vec<VerifType>> = initial_locals.map(<[VerifType]>::to_vec);
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
            let (same_locals, shares_prefix, p) = match prev_locals.as_deref() {
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
                full(&mut body, delta, &locals, &stack, cp);
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
            // An UNBOUND label means the region it delimits was dropped as dead code (`bind_at` is a
            // no-op while dead), so the entry describes bytes that do not exist. Without this the
            // `usize::MAX as u16` truncation below would fabricate offset 65535.
            .filter(|&&(s, e, h, _)| {
                [s, e, h]
                    .iter()
                    .all(|l| self.labels[l.0 as usize] != usize::MAX)
            })
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
                                                   // A splice inside a dropped region goes with it. Its relocated frames are bound INSIDE the
                                                   // body, never at its first byte, so emitting it while dead would leave an unreachable region
                                                   // whose entry has no frame ("Expecting a stack map frame"); and its prologue consumes
                                                   // arguments that the dropped code never pushed. `bind_at` already left its labels unbound, so
                                                   // the frames and handlers registered for it are dropped too. Height bookkeeping still runs.
        if self.dead {
            self.cur_stack = baseline + ret_words;
            return;
        }
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
        self.dead_bound.push(false);
        Label(id)
    }
    /// Bind `l` here. Inside a dropped dead region this revives emission only if control can actually
    /// arrive: some branch to `l` was ALREADY EMITTED (it recorded a fixup). A branch emitted while
    /// dead was itself dropped and left no fixup, so its target stays dead and the rest of that
    /// construct is dropped with it. A backward target (a loop head) is bound before its back-edge and
    /// so never revives — correct, because reaching the head while dead means the whole loop is
    /// unreachable. For an entry point control reaches WITHOUT a branch, see [`Self::bind_handler`].
    pub fn bind(&mut self, l: Label) {
        self.labels[l.0 as usize] = self.bytes.len();
        if self.dead {
            if self.fixups.iter().any(|&(_, lid)| lid == l.0) {
                self.dead = false;
            } else {
                self.dead_bound[l.0 as usize] = true;
            }
        }
    }
    /// Bind `l` as an EXCEPTION HANDLER entry guarding `protects` (`[start, end)` label pairs, already
    /// bound). A handler is reached over the exception edge rather than by a branch, so `bind` can't
    /// see that control arrives; it revives whenever some guarded range actually holds live emitted
    /// bytes. That is the `try` whose body diverges — the stream is dead exactly at the handler, yet
    /// the handler runs. A range that is empty, or whose start was itself bound inside a dropped
    /// region, guards nothing: the whole `try` was dead code and the handler goes with it.
    pub fn bind_handler(&mut self, l: Label, protects: &[(Label, Label)]) {
        self.labels[l.0 as usize] = self.bytes.len();
        let guards_live_code = protects.iter().any(|&(s, e)| {
            let (s_off, e_off) = (self.labels[s.0 as usize], self.labels[e.0 as usize]);
            s_off != usize::MAX && s_off < e_off && !self.is_dead_bound(s.0)
        });
        if guards_live_code {
            self.dead = false;
        } else if self.dead {
            self.dead_bound[l.0 as usize] = true;
        }
    }
    /// Bind a label at an explicit byte offset (used to attach a relocated StackMapTable frame to a
    /// position inside a spliced inline body, which is appended as raw bytes). While dead the splice
    /// itself is dropped ([`Self::splice_inline`]), so the label is left UNBOUND: every consumer
    /// (`resolved_frames`, `build_stackmap`, `resolved_exceptions`) drops entries for an unbound
    /// label, which is exactly the right outcome for a frame or handler inside dropped bytes.
    pub fn bind_at(&mut self, l: Label, offset: usize) {
        if self.dead {
            return;
        }
        self.labels[l.0 as usize] = offset;
    }
    fn branch(&mut self, opcode: u8, l: Label, delta: i32) {
        if self.dead {
            self.adjust(delta); // dropped dead code; height bookkeeping stays as it was
            return;
        }
        self.bytes.push(opcode);
        let pos = self.bytes.len();
        self.fixups.push((pos, l.0));
        self.bytes.extend_from_slice(&[0, 0]);
        self.adjust(delta);
    }
    pub fn goto(&mut self, l: Label) {
        self.branch(0xa7, l, 0);
        self.dead = true; // unconditional transfer: what follows is unreachable
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
        if !self.dead {
            self.bytes.push(byte);
        }
        self.adjust(stack_delta);
    }
    fn op_u1(&mut self, byte: u8, arg: u8, stack_delta: i32) {
        if !self.dead {
            self.bytes.push(byte);
            self.bytes.push(arg);
        }
        self.adjust(stack_delta);
    }
    fn op_u2(&mut self, byte: u8, arg: u16, stack_delta: i32) {
        if !self.dead {
            self.bytes.push(byte);
            self.bytes.extend_from_slice(&arg.to_be_bytes());
        }
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
        if !self.dead {
            self.bytes.push(0xc4);
            self.bytes.push(op);
            self.bytes.extend_from_slice(&idx.to_be_bytes());
        }
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
    /// `ldc <string>` for a Kotlin string VALUE (see [`KtString`]).
    pub fn push_string_kt(&mut self, s: &KtString, cw: &mut ClassWriter) {
        let i = cw.const_string_kt(s);
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
        if self.dead {
            self.ensure_locals(idx + 1);
            return;
        }
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

    // returns — every one ends the path, so what follows is unreachable (see `dead`).
    pub fn ireturn(&mut self) {
        self.op(0xac, -1);
        self.dead = true;
    }
    pub fn lreturn(&mut self) {
        self.op(0xad, -2);
        self.dead = true;
    }
    pub fn freturn(&mut self) {
        self.op(0xae, -1);
        self.dead = true;
    }
    pub fn dreturn(&mut self) {
        self.op(0xaf, -2);
        self.dead = true;
    }
    pub fn areturn(&mut self) {
        self.op(0xb0, -1);
        self.dead = true;
    }
    pub fn ret_void(&mut self) {
        self.op(0xb1, 0);
        self.dead = true;
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
        if !self.dead {
            self.bytes.push(0xb9);
            self.bytes.extend_from_slice(&iref.to_be_bytes());
            self.bytes.push((arg_words + 1) as u8); // count includes the receiver
            self.bytes.push(0);
        }
        self.adjust(ret_words - arg_words - 1);
    }
    /// `invokedynamic <indy-const> 0 0` — pops `arg_words`, pushes the call-site result (`ret_words`).
    pub fn invokedynamic(&mut self, indy_index: u16, arg_words: i32, ret_words: i32) {
        if !self.dead {
            self.bytes.push(0xba);
            self.bytes.extend_from_slice(&indy_index.to_be_bytes());
            self.bytes.push(0);
            self.bytes.push(0);
        }
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
    pub fn ineg(&mut self) {
        self.op(0x74, 0);
    }
    pub fn lneg(&mut self) {
        self.op(0x75, 0);
    }
    pub fn fneg(&mut self) {
        self.op(0x76, 0);
    }
    pub fn dneg(&mut self) {
        self.op(0x77, 0);
    }
    pub fn athrow(&mut self) {
        self.op(0xbf, -1);
        self.dead = true; // the path transfers to a handler: what follows is unreachable
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
        // A nested class of ANOTHER owner that this class does not reference as a class constant
        // emits no entry. (Its own nest is a different rule — see `own_nest_members_always_emitted`.)
        let mut unref = ClassWriter::new("C", "java/lang/Object");
        unref.add_inner_class(InnerClassSpec {
            inner: "dep/Outer$Nested".to_string(),
            outer: Some("dep/Outer".to_string()),
            name: Some("Nested".to_string()),
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
    fn inner_class_descriptor_references_are_typed() {
        let candidate = InnerClassSpec {
            inner: "dep/Outer$Nested".to_string(),
            outer: Some("dep/Outer".to_string()),
            name: Some("Nested".to_string()),
            access: ACC_PUBLIC | ACC_STATIC | ACC_FINAL,
        };

        let mut field_ref = ClassWriter::new("Use", "java/lang/Object");
        field_ref.add_inner_class(candidate.clone());
        field_ref.add_field(ACC_PUBLIC, "nested", "Ldep/Outer$Nested;");
        let info = crate::jvm::classreader::parse_class(&field_ref.finish()).expect("parse class");
        assert_eq!(info.inner_classes.len(), 1);

        // The same bytes as an ordinary string constant are not a descriptor reference.
        let mut string_literal = ClassWriter::new("Use", "java/lang/Object");
        string_literal.add_inner_class(candidate);
        string_literal.const_string("Ldep/Outer$Nested;");
        let info =
            crate::jvm::classreader::parse_class(&string_literal.finish()).expect("parse class");
        assert!(info.inner_classes.is_empty());
    }

    /// kotlinc emits an `InnerClasses` entry for a class's OWN member classes whether or not its code
    /// mentions them (verified against the reference compiler for both `class C { class Nested }` and
    /// `class D { companion object }`). The JVM requires it: it cross-checks the outer's and the
    /// inner's attributes and throws `IncompatibleClassChangeError: … disagree on InnerClasses` from
    /// `getEnclosingClass` when only the inner carries the entry.
    #[test]
    fn own_nest_members_always_emitted() {
        let mut writer = ClassWriter::new("C", "java/lang/Object");
        writer.add_inner_class(InnerClassSpec {
            inner: "C$Nested".to_string(),
            outer: Some("C".to_string()),
            name: Some("Nested".to_string()),
            access: ACC_PUBLIC | ACC_STATIC | ACC_FINAL,
        });
        let bytes = writer.finish();
        let has = |n: &[u8]| bytes.windows(n.len()).any(|w| w == n);
        assert!(has(b"InnerClasses"));
        assert!(has(b"C$Nested"));
    }

    #[test]
    fn referenced_inner_classes_are_resolved_from_metadata() {
        let seen = Rc::new(std::cell::RefCell::new(Vec::new()));
        let seen_by_resolver = seen.clone();
        let resolver: InnerClassResolver = Rc::new(move |internal| {
            seen_by_resolver.borrow_mut().push(internal.to_string());
            (internal == "dep/Nested").then(|| InnerClassDetails {
                outer: Some("dep/Outer".to_string()),
                name: Some("Nested".to_string()),
                access: ACC_PUBLIC | ACC_STATIC | ACC_FINAL,
            })
        });

        let mut writer = ClassWriter::new("Use", "java/lang/Object");
        writer.set_inner_class_resolver(Some(resolver));
        writer.class_ref("dep/Nested");
        let info = crate::jvm::classreader::parse_class(&writer.finish()).expect("parse class");

        assert_eq!(
            info.inner_classes,
            vec![crate::jvm::classreader::InnerClassRef {
                inner: "dep/Nested".to_string(),
                outer: Some("dep/Outer".to_string()),
                name: Some("Nested".to_string()),
                access: ACC_PUBLIC | ACC_STATIC | ACC_FINAL,
            }]
        );
        assert!(!seen.borrow().iter().any(|name| name == "dep/Outer"));
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

    #[test]
    fn dead_emission_revives_only_at_an_emitted_branch_target() {
        let mut cw = ClassWriter::new("Scratch", "java/lang/Object");
        let mut code = CodeBuilder::new(0);
        let live = code.new_label();
        let dead_only = code.new_label();

        code.goto(live);
        let terminator_end = code.bytes.len();
        code.push_int(7, &mut cw);
        code.ifeq(dead_only); // dropped, so it records no arrival edge
        code.bind(dead_only);
        code.push_int(8, &mut cw);
        assert_eq!(code.bytes.len(), terminator_end);

        code.bind(live); // the emitted `goto` proves arrival here
        code.ret_void();
        assert_eq!(code.bytes.last(), Some(&0xb1));
        assert_eq!(code.bytes.len(), terminator_end + 1);
    }

    #[test]
    fn zero_length_local_from_dropped_code_is_not_attached_to_resumed_code() {
        let mut cw = ClassWriter::new("DeadLocalKt", "java/lang/Object");
        let mut code = CodeBuilder::new(0);
        let live = code.new_label();
        code.goto(live);
        let dropped_start = code.bytes.len() as u16;
        code.add_local_entry(dropped_start, Some(0), 0, "dropped", "I");
        code.bind(live);
        code.ret_void();

        cw.add_method(ACC_PUBLIC | ACC_STATIC, "m", "()V", &code);
        assert!(cw.methods[0].lvt.is_empty());
    }
}
