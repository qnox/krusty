//! A hand-written JVM class-file writer (the format is well-specified; no external crate).
//! Targets major version 50 (Java 6). Methods that create lambda objects (new $lambda$N) emit a
//! StackMapTable attribute so the type-checking verifier on Java 25+ accepts them.

use crate::kt_string::KtString;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

mod annotation_values;
pub(crate) mod bytecode_analysis;
mod checkpoint;
mod class_pool_seed;
mod codegen_markers;
mod constant_pool_queries;
mod control_flow;
mod copied_class;
mod coroutine_markers;
mod coroutine_transform;
mod debug_metadata;
mod descriptor_mentions;
mod enclosing_method;
mod inner_classes;
mod late_fields;
mod line_numbers;
mod local_variables;
mod member_mapping;
mod method_parameters;
mod method_rewrite;
mod pool_layout;
mod stack_maps;
mod utf8_pool;

use descriptor_mentions::DescriptorMentionCache;

pub use coroutine_markers::{markers_in, CoroutineMarker, MARKER_LEN};
pub(crate) use coroutine_transform::{CoroutineOutcome, CoroutineRequest, TransformedCoroutine};
pub(crate) use {copied_class::CopyError, inner_classes::DeclarationPaths};

pub const ACC_PUBLIC: u16 = 0x0001;
pub const ACC_PRIVATE: u16 = 0x0002;
pub const ACC_PROTECTED: u16 = 0x0004;
pub const ACC_STATIC: u16 = 0x0008;
pub const ACC_FINAL: u16 = 0x0010;
pub const ACC_SUPER: u16 = 0x0020;
pub const ACC_BRIDGE: u16 = 0x0040;
pub const ACC_VARARGS: u16 = 0x0080;
pub const ACC_INTERFACE: u16 = 0x0200;
pub const ACC_ABSTRACT: u16 = 0x0400;
pub const ACC_SYNTHETIC: u16 = 0x1000;
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

/// One property-backed primary-constructor parameter, as the plain-class pool seeder sees it: the
/// annotations the constructor's header visit interns for it.
pub struct SeedCtorParameter {
    /// 0 = primitive (no annotation), 1 = non-null reference (`@NotNull`), 2 = nullable reference
    /// (`@Nullable`).
    pub ann_kind: u8,
    /// USER annotation type descriptors on this constructor parameter (`class C(@Mark val x: Int)`),
    /// split by the attribute each retention selects. kotlinc writes the whole
    /// `RuntimeVisibleParameterAnnotations` before `RuntimeInvisible…`, so every parameter's `visible`
    /// entries intern before any `invisible` one — and both before the synthesized `@NotNull`.
    pub visible_ann_types: Vec<String>,
    pub invisible_ann_types: Vec<String>,
}

/// Primary-constructor JVM generic `Signature`, passed to
/// [`ClassWriter::seed_plain_class_pool`] at the constructor's interning position.
pub struct MemberSignatures<'a> {
    /// The primary constructor's generic `Signature` (`(Ljava/util/List<Ljava/lang/String;>;)V`).
    pub ctor: Option<&'a str>,
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
    /// `CONSTANT_Utf8` slots, keyed by text so a repeat lookup does not allocate a key.
    utf8_index: utf8_pool::Utf8Pool,
    /// The entry occupying each pool slot, by `slot - 1`. A `Long`/`Double` takes two slots, so
    /// once one is interned slots and entries no longer line up; its second slot holds
    /// [`Self::UNUSABLE_SLOT`], which names no entry.
    slot_entries: Vec<u32>,
}

impl ConstPool {
    const UNUSABLE_SLOT: u32 = u32::MAX;

    /// Number of slots used (long/double take 2). Pool count in the file = this + 1.
    fn slot_count(&self) -> u16 {
        self.slot_entries.len() as u16
    }

    fn intern(&mut self, c: Const) -> u16 {
        if let Const::Utf8(text) = &c {
            if let Some(index) = self.utf8_index.get(text) {
                return index;
            }
        } else if let Some(&index) = self.dedup.get(&c) {
            return index;
        }
        let idx = self.slot_count() + 1; // 1-based
        self.slot_entries.push(self.entries.len() as u32);
        if matches!(c, Const::Long(_) | Const::Double(_)) {
            self.slot_entries.push(Self::UNUSABLE_SLOT);
        }
        self.entries.push(c.clone());
        match c {
            Const::Utf8(text) => self.utf8_index.insert(text, idx),
            other => {
                self.dedup.insert(other, idx);
            }
        }
        idx
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
    /// Feed every descriptor the pool carries in a typed position to `record`, except those of the
    /// `copied` `NameAndType` entries.
    fn record_typed_descriptor_names(&self, copied: &HashSet<u16>, record: &mut impl FnMut(&str)) {
        for (slot, &entry) in self.slot_entries.iter().enumerate() {
            let descriptor = match self.entries.get(entry as usize) {
                Some(Const::NameAndType(_, _)) if copied.contains(&(slot as u16 + 1)) => continue,
                Some(Const::NameAndType(_, descriptor) | Const::MethodType(descriptor)) => {
                    *descriptor
                }
                _ => continue,
            };
            if let Some(value) = self.utf8_value(descriptor) {
                record(value);
            }
        }
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

/// How many parameters a JVM method descriptor `(…)ret` declares — one per top-level type, an
/// `L…;` or `[…` counting as one.
fn descriptor_param_count(descriptor: &str) -> usize {
    let bytes = descriptor.as_bytes();
    let Some(end) = descriptor.find(')') else {
        return 0;
    };
    let (mut i, mut count) = (1, 0);
    while i < end {
        while i < end && bytes[i] == b'[' {
            i += 1;
        }
        if i < end && bytes[i] == b'L' {
            while i < end && bytes[i] != b';' {
                i += 1;
            }
        }
        i += 1;
        count += 1;
    }
    count
}

struct MethodInfo {
    access: u16,
    name: u16,
    desc: u16,
    max_stack: u16,
    max_locals: u16,
    /// `None` for an abstract method (no `Code` attribute).
    code: Option<Vec<u8>>,
    /// Bytecode position of the implicit void return appended by declared-function emission. This
    /// is producer provenance, not a guess from the final byte stream; explicit returns and throws
    /// leave it absent.
    implicit_void_return_pc: Option<u16>,
    /// What kotlinc's bytecode rewrites need to reshape this method when the class is written.
    rewrite_source: Option<Box<method_rewrite::RewriteSource>>,
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
    /// `RuntimeVisibleParameterAnnotations`: RUNTIME-retained USER annotations written on the
    /// parameters (`fun f(@Mark a: Int)`). A separate list from [`MethodInfo::param_anns`] because the
    /// two land in different attributes.
    visible_param_anns: Vec<Vec<Vec<u8>>>,
    /// BINARY-retained USER parameter annotations. Kept apart from `param_anns` (which the nullability
    /// pass owns and OVERWRITES) so the two can be attached in either order; `finish` concatenates
    /// them per parameter, user annotations first — kotlinc's order.
    user_invisible_param_anns: Vec<Vec<Vec<u8>>>,
    /// The default this annotation element declares.
    annotation_default: Option<annotation_values::ElementDefault>,
    /// `MethodParameters` entries `(name_index, access_flags)`, one per parameter in descriptor order.
    /// Empty ⇒ no attribute. kotlinc writes this only under `-java-parameters`, and only for methods
    /// that HAVE a declaration: a `$default` bridge or a synthetic marker constructor gets none.
    method_parameters: Vec<(u16, u16)>,
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

/// Partition retained declaration annotations into the two class-file attributes the JVM splits them
/// across: `RuntimeVisibleAnnotations` (Kotlin's RUNTIME, the default) then `RuntimeInvisibleAnnotations`
/// (BINARY). Common IR carries one list per declaration; this boundary is the only place the split
/// exists. SOURCE-retained applications never reach the IR.
/// kotlinc's synthesized non-null annotation type.
const NOT_NULL: &str = "Lorg/jetbrains/annotations/NotNull;";
/// kotlinc's synthesized nullable annotation type.
const NULLABLE: &str = "Lorg/jetbrains/annotations/Nullable;";

pub(crate) fn split_declaration_annotations(
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
    /// Every internal class name mentioned in class-type position by a field/method descriptor, a
    /// pool descriptor, or a generic signature, with the sizes it was computed from.
    ///
    /// `InnerClasses` retention asks "does any descriptor mention this class?" once per candidate
    /// row, inside a FIXPOINT loop, and the old answer re-scanned every descriptor with a freshly
    /// formatted `L…;` needle each time. On a module of generated clients that search dominated the
    /// whole compile. The set answers the same question in one lookup, and the recorded sizes make
    /// a stale answer impossible: anything appended invalidates it.
    mentioned_names: std::cell::RefCell<Option<DescriptorMentionCache>>,
    /// Which member references only code copied from an inline function names.
    members: member_mapping::MappedMembers,
    /// The frames computed so far for this class's method bodies (see [`stack_maps`]).
    computed_bodies: stack_maps::ComputedBodies,
    /// Emit (and therefore seed the pool for) `Intrinsics.checkNotNullParameter` guards. Cleared by
    /// `-Xno-param-assertions`.
    param_assertions: bool,
    /// Write kotlinc's synthesized `@NotNull`/`@Nullable` on this class's declarations. Cleared for a
    /// local class (see [`Self::set_nullability_annotations`]).
    nullability_annotations: bool,
    access: u16,
    this_class: u16,
    super_class: u16,
    interfaces: Vec<u16>,
    fields: Vec<FieldInfo>,
    late_fields: Vec<late_fields::LateField>,
    /// Methods (name, descriptor) whose line table [`Self::omit_method_lines`] drops.
    lineless_methods: Vec<(String, String)>,
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
    inner_class_table: inner_classes::InnerClassTable,
    value_classes: Rc<crate::jvm::bytecode_passes::redundant_boxing::ValueClassDescriptors>,
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
    /// The JSR-045 source map for a class that contains inlined code, accumulated as each inline
    /// body is spliced and rendered into `SourceDebugExtension` at the end. A class with nothing
    /// inlined carries no attribute. See [`crate::jvm::source_map`].
    source_map: crate::jvm::source_map::SourceMap,
    /// The methods the coroutine transformer rewrites when the class is written, and its results.
    coroutines: coroutine_transform::Coroutines,
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
            mentioned_names: std::cell::RefCell::new(None),
            members: member_mapping::MappedMembers::default(),
            computed_bodies: stack_maps::ComputedBodies::default(),
            param_assertions: true,
            nullability_annotations: true,
            access: ACC_PUBLIC | ACC_FINAL | ACC_SUPER,
            this_class,
            super_class,
            interfaces: Vec::new(),
            fields: Vec::new(),
            late_fields: Vec::new(),
            lineless_methods: Vec::new(),
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
            inner_class_table: inner_classes::InnerClassTable::default(),
            value_classes: Rc::default(),
            annotation_class_refs: std::collections::HashSet::new(),
            permitted_subclasses: Vec::new(),
            major: MAJOR_JAVA8,
            source_file: None,
            source_map: crate::jvm::source_map::SourceMap::default(),
            coroutines: coroutine_transform::Coroutines::default(),
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
    /// The class's source map, started from the class's own source file the first time an inline
    /// body is spliced into it. Splicing a dependency's `inline fun` puts that dependency's source
    /// into this class, and the map is what tells a debugger which file an inlined line belongs to.
    ///
    /// `lines` is the highest line the class's own code can claim. `None` when the class has no
    /// `SourceFile` to map against, in which case its inlined lines cannot be described at all.
    pub fn source_map_for_inlining(
        &mut self,
        lines: u16,
    ) -> Option<&mut crate::jvm::source_map::SourceMap> {
        if self.source_map.is_unstarted() {
            let source_file = self.source_file.clone()?;
            let path = self.internal_name.clone();
            self.source_map = crate::jvm::source_map::SourceMap::new(&source_file, &path, lines);
        }
        Some(&mut self.source_map)
    }

    /// The class's own source file, for a synthesized class that belongs to it.
    pub fn source_file_name(&self) -> Option<String> {
        self.source_file.clone()
    }

    pub fn set_source_file(&mut self, name: Option<String>) {
        self.source_file = name;
    }

    pub(crate) fn set_value_classes(
        &mut self,
        value_classes: Rc<crate::jvm::bytecode_passes::redundant_boxing::ValueClassDescriptors>,
    ) {
        self.value_classes = value_classes;
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

    /// Whether this class's declarations carry the synthesized `@NotNull`/`@Nullable` annotations.
    /// kotlinc's annotation writer skips them on every declaration of a local class (a class declared
    /// in executable code, an anonymous object, a lambda's class, and anything nested in one): no code
    /// outside the enclosing body can call it, so there is no caller for the contract to inform. The
    /// class then never interns the two annotation types, so the policy is the writer's: every path
    /// that attaches or seeds one consults it. The `checkNotNullParameter` guards are unaffected.
    pub fn set_nullability_annotations(&mut self, enabled: bool) {
        self.nullability_annotations = enabled;
    }

    /// `ty` unless it is a synthesized nullability annotation this class does not write.
    fn written_annotation<'a>(&self, ty: &'a str) -> Option<&'a str> {
        (self.nullability_annotations || (ty != NOT_NULL && ty != NULLABLE)).then_some(ty)
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

    /// Append an empty (no-argument) RUNTIME-visible marker annotation to a previously-added method —
    /// the `@java.lang.Deprecated` kotlinc puts on each `enable`-mode `$DefaultImpls` forward.
    /// Lookup-only for the method key; the annotation TYPE is interned here (seed it earlier when
    /// pool order matters). No-op if the method isn't found.
    pub fn add_method_visible_marker_annotation(&mut self, name: &str, desc: &str, ann_type: &str) {
        let (Some(n), Some(d)) = (self.cp.lookup_utf8(name), self.cp.lookup_utf8(desc)) else {
            return;
        };
        let ti = self.cp.utf8(ann_type);
        let ann = vec![(ti >> 8) as u8, ti as u8, 0, 0];
        if let Some(m) = self.methods.iter_mut().find(|m| m.name == n && m.desc == d) {
            m.visible_anns.push(ann);
        }
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
            implicit_void_return_pc: None,
            rewrite_source: None,
            exceptions: Vec::new(),
            stackmap: None,
            signature: sig,
            lnt: Vec::new(),
            lvt: Vec::new(),
            visible_anns: Vec::new(),
            invisible_anns: Vec::new(),
            param_anns: Vec::new(),
            visible_param_anns: Vec::new(),
            user_invisible_param_anns: Vec::new(),
            annotation_default: None,
            method_parameters: Vec::new(),
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

    /// Declare a field ahead of every field declared so far (a suspend lambda's spill fields,
    /// which the coroutine transformer adds after the class's own).
    pub(super) fn add_leading_field(&mut self, access: u16, name: &str, desc: &str) {
        let n = self.cp.utf8(name);
        let d = self.cp.utf8(desc);
        self.fields.insert(
            0,
            FieldInfo {
                access,
                name: n,
                desc: d,
                signature: None,
                const_value: None,
                visible_anns: Vec::new(),
                invisible_anns: Vec::new(),
            },
        );
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
    /// Read back the `(owner, name, descriptor)` of a method reference already in the pool.
    ///
    /// A transform that runs over instructions ALREADY relocated into this class has no source pool
    /// left to consult, and interning every method it might want to recognize would add entries the
    /// class does not otherwise carry. `None` when the index is not a method reference.
    pub fn methodref_parts(&self, index: u16) -> Option<(&str, &str, &str)> {
        self.cp.methodref_parts(index)
    }

    /// The internal name of the `CONSTANT_Class` at `index`, for the same reason.
    pub fn class_name_at(&self, index: u16) -> Option<&str> {
        self.cp.class_name(index)
    }

    /// The descriptor of the field reference at `index`, for the same reason.
    pub fn fieldref_descriptor_at(&self, index: u16) -> Option<&str> {
        self.cp.fieldref_descriptor(index)
    }

    /// The descriptor of the `invokedynamic` call site at `index`, for the same reason.
    pub fn invokedynamic_descriptor_at(&self, index: u16) -> Option<&str> {
        self.cp.invokedynamic_descriptor(index)
    }

    /// The verification type an `ldc` of the constant at `index` pushes, for the same reason.
    pub fn loadable_constant_type_at(&self, index: u16) -> Option<VerifType> {
        self.cp.loadable_constant_type(index)
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
    /// Intern a `CONSTANT_MethodHandle` of ANY reference kind onto an already-interned member ref.
    /// Relocating a bootstrap method from another class needs every kind, not only `invokestatic`.
    pub fn method_handle_ref(&mut self, kind: u8, member: u16) -> u16 {
        self.cp.intern(Const::MethodHandle(kind, member))
    }

    /// Intern a `CONSTANT_MethodType` for `descriptor`.
    pub fn method_type_ref(&mut self, descriptor: &str) -> u16 {
        self.cp.method_type(descriptor)
    }

    /// Intern a `CONSTANT_InvokeDynamic` naming a `BootstrapMethods` entry already registered here.
    pub fn invoke_dynamic_ref(&mut self, bootstrap: u16, name: &str, descriptor: &str) -> u16 {
        self.cp.invoke_dynamic(bootstrap, name, descriptor)
    }

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
    /// Whether the class already declares `name` `desc`, without interning either.
    pub fn declares_method(&self, name: &str, desc: &str) -> bool {
        let (Some(n), Some(d)) = (self.cp.lookup_utf8(name), self.cp.lookup_utf8(desc)) else {
            return false;
        };
        self.methods.iter().any(|m| m.name == n && m.desc == d)
    }

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
            &[],
        );
    }

    /// [`Self::reserve_method_pool`] plus the DECLARED annotations' constants and the
    /// `MethodParameters` names (empty when the method gets no such attribute). kotlinc interns a
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
        parameters: &[(Option<String>, u16)],
    ) {
        self.cp.utf8(name);
        self.cp.utf8(desc);
        if let Some(s) = signature {
            self.cp.utf8(s);
        }
        // `MethodParameters` names sit between the header and the annotation types: ASM visits
        // `visitParameter` before `visitParameterAnnotation` and before the code, so a parameter name
        // precedes both the `@NotNull`/`@Nullable` descriptors and every constant the body introduces.
        for (parameter, _) in parameters {
            if let Some(parameter) = parameter {
                self.cp.utf8(parameter);
            }
        }
        let _ = self.encode_declaration_annotations(annotations);
        for a in ann_types {
            if let Some(a) = self.written_annotation(a) {
                self.cp.utf8(a);
            }
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
        // The last gate before a body becomes a class file's `Code`. A coroutine marker is
        // `impdep1`, reserved by JVMS §6.2 for an implementation's own use and rejected by nothing —
        // it would simply run, over a continuation slot the declining path never assigned. The walk
        // is instruction-by-instruction because `0xfe` also occurs inside operands.
        debug_assert!(
            markers_in(&code.bytes).is_none_or(|found| found.is_empty()),
            "a coroutine marker reached the class file in {name}{desc}",
        );
        let n = self.cp.utf8(name);
        let d = self.cp.utf8(desc);
        let sig = signature.map(|s| self.cp.utf8(s));
        self.intern_local_table(code);
        // The table the instructions imply: its classes intern now, where kotlinc's writer interns
        // them, and `finish` writes it computed over the final body. A non-empty emitted body must
        // be understood by the one authoritative frame computation.
        let body = stack_maps::Body {
            access,
            name,
            descriptor: desc,
            code: &code.bytes,
            exceptions: &code.resolved_exceptions(),
            labels: stack_maps::builder_labels(
                code.line_marks(),
                code.local_entries(),
                code.bytes.len(),
            ),
        };
        let computed = (!code.bytes.is_empty()).then(|| {
            self.compute_frames(&body).unwrap_or_else(|decline| {
                panic!("cannot compute JVM frames for {name}{desc}: {decline:?}")
            })
        });
        let lvt = if name == "<init>" || name == "<clinit>" {
            Vec::new()
        } else {
            self.local_table(code)
        };
        self.methods.push(MethodInfo {
            access,
            name: n,
            desc: d,
            max_stack: code.max_stack,
            max_locals: code.max_locals,
            code: Some(code.bytes.clone()),
            implicit_void_return_pc: code.implicit_void_return_pc,
            // kotlinc's bytecode rewrites run when the class is written: several of this method's
            // tables are attached after it is added, and the rewrite depends on them.
            rewrite_source: (!code.bytes.is_empty())
                .then(|| method_rewrite::RewriteSource::new(access, name, desc, code)),
            exceptions: code.resolved_exceptions(),
            stackmap: None,
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
            lvt,
            visible_anns: Vec::new(),
            invisible_anns: Vec::new(),
            param_anns: Vec::new(),
            visible_param_anns: Vec::new(),
            user_invisible_param_anns: Vec::new(),
            annotation_default: None,
            method_parameters: Vec::new(),
        });
        if let Some(computed) = &computed {
            self.intern_frame_classes(&body, computed);
        }
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
        if !self.nullability_annotations {
            return;
        }
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
        if !self.nullability_annotations {
            return;
        }
        let n = self.cp.utf8(name);
        let ti = self.cp.utf8(ann_type);
        let ann = vec![(ti >> 8) as u8, ti as u8, 0, 0];
        if let Some(f) = self.fields.iter_mut().find(|f| f.name == n) {
            f.invisible_anns = vec![ann];
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

    pub fn finish(self) -> Vec<u8> {
        let (bytes, coroutines) = self.finish_with_coroutines();
        assert!(
            coroutines.is_empty(),
            "a class with transformed coroutines is finished through `finish_with_coroutines`"
        );
        bytes
    }

    /// [`Self::finish`] for a class whose suspend functions the coroutine transformer rewrites:
    /// the class, and what each transformation found, which its continuation class is built from.
    pub(crate) fn finish_with_coroutines(
        mut self,
    ) -> (Vec<u8>, Vec<coroutine_transform::TransformedCoroutine>) {
        // Every method's tables are final now: the coroutine transformer runs over the suspend
        // functions it was asked for, then kotlinc's bytecode rewrites over the rest.
        let coroutines = self.transform_coroutines();
        (self.write(), coroutines)
    }

    fn write(mut self) -> Vec<u8> {
        let relaid = self.rewrite_methods();
        // Every body is final now: write the frames it implies.
        self.compute_stack_maps();
        // A class that never attached `@Metadata` still realizes its deferred fields first —
        // kotlinc's field visit precedes every class-attribute window.
        self.intern_late_fields();
        self.resolve_inner_classes();
        self.intern_enclosing_method_refs();
        // Every EMITTED `InnerClasses` entry's refs (outer Class, simple name) intern here — before
        // the `SourceFile` value and the attribute names (kotlinc visits the InnerClasses table
        // ahead of both; a nested class's own entry otherwise interned its outer at serialization,
        // after everything else). Use the same retention predicate as early name seeding and final
        // attribute construction: interning a rejected candidate here would falsely make it a
        // referenced class.
        self.intern_retained_inner_rows();
        // kotlinc interns the `SourceFile` VALUE (the `.kt` name) right after the class annotations and
        // before the Code-attribute names, then the `SourceFile` attribute NAME later, and
        // `RuntimeVisibleAnnotations` last. Intern the value up front to match.
        let sourcefile_value = self.source_file.clone().map(|src| self.cp.utf8(&src));
        // The source map is also published as a BINARY-retained annotation, which is how a Kotlin
        // consumer reads it back without parsing the class file's own attribute. kotlinc visits it
        // when the class is done, after the `SourceFile` value and before the method attribute names.
        let smap_annotation = self.source_map.render().map(|smap| {
            let mut body = Vec::new();
            let annotation = self.cp.utf8("Lkotlin/jvm/internal/SourceDebugExtension;");
            u2(&mut body, annotation);
            u2(&mut body, 1); // one element pair
            let name = self.cp.utf8("value");
            u2(&mut body, name);
            body.push(b'['); // an array of one string, which is how kotlinc spells it
            u2(&mut body, 1);
            body.push(b's');
            let value = self.cp.utf8(&smap);
            u2(&mut body, value);
            body
        });
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
            Code,
            Lnt,
            Lvt,
            Dep,
            Ria,
            Rva,
            Smt,
            Rvpa,
            Ripa,
            Sig,
        }
        /// A method's `RuntimeInvisibleParameterAnnotations` entries: the USER (BINARY-retained)
        /// annotations first, then the synthesized `@NotNull`/`@Nullable` — kotlinc's order within a
        /// parameter. Empty ⇒ the method has no such attribute.
        ///
        /// BINARY-retained user annotations are rare, so the overwhelmingly common case (only the
        /// nullability pass wrote anything) borrows `param_anns` instead of rebuilding it — this runs
        /// per method, twice, on every emitted class.
        fn invisible_param_anns(m: &MethodInfo) -> std::borrow::Cow<'_, [Vec<Vec<u8>>]> {
            if m.user_invisible_param_anns.is_empty() {
                return std::borrow::Cow::Borrowed(&m.param_anns);
            }
            let arity = m.user_invisible_param_anns.len().max(m.param_anns.len());
            let merged: Vec<Vec<Vec<u8>>> = (0..arity)
                .map(|i| {
                    let mut anns = m
                        .user_invisible_param_anns
                        .get(i)
                        .cloned()
                        .unwrap_or_default();
                    anns.extend(m.param_anns.get(i).cloned().unwrap_or_default());
                    anns
                })
                .collect();
            if merged.iter().all(Vec::is_empty) {
                std::borrow::Cow::Owned(Vec::new())
            } else {
                std::borrow::Cow::Owned(merged)
            }
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
        // First-use order of the per-method attribute names, in method emit order.
        let mut seq: Vec<An> = Vec::new();
        for m in &self.methods {
            // `Code` interns with the first method that has a body, ahead of its sub-attributes: an
            // interface whose first method is abstract interns that method's `Signature` or
            // annotation names before `Code` (ASM's per-method `computeMethodInfoSize`).
            if m.code.is_some() && !seq.contains(&An::Code) {
                seq.push(An::Code);
            }
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
            if !m.visible_param_anns.is_empty() && !seq.contains(&An::Rvpa) {
                seq.push(An::Rvpa);
            }
            if !invisible_param_anns(m).is_empty() && !seq.contains(&An::Ripa) {
                seq.push(An::Ripa);
            }
        }
        let (mut lnt_attr_name, mut lvt_attr_name, mut stackmap_attr_name, mut ripa_attr_name) =
            (None, None, None, None);
        // Unused when no method has a body: an unused attribute name would diverge from kotlinc.
        let mut code_attr_name = 0;
        // A method-level `RuntimeVisibleAnnotations` shares its attribute-name entry with the class
        // annotations when both are present; the class table is written later, so intern on first
        // METHOD use here and let that later write reuse the index.
        let mut vis_ann_name: Option<u16> = field_rva;
        // `Deprecated` interned from the per-method sequence above; the class-level fallback below
        // dedups onto it when a method already introduced the name.
        let mut method_dep_name: Option<u16> = None;
        let mut rvpa_attr_name = None;
        // A method-level RIA first use dedups onto the field-level index when both are present.
        let mut invis_ann_name = field_ria;
        for k in &seq {
            match k {
                An::Code => code_attr_name = self.cp.utf8("Code"),
                An::Lnt => lnt_attr_name = Some(self.cp.utf8("LineNumberTable")),
                An::Lvt => lvt_attr_name = Some(self.cp.utf8("LocalVariableTable")),
                An::Ria => invis_ann_name = Some(self.cp.utf8("RuntimeInvisibleAnnotations")),
                An::Rva => vis_ann_name = Some(self.cp.utf8("RuntimeVisibleAnnotations")),
                An::Dep => method_dep_name = Some(self.cp.utf8("Deprecated")),
                An::Smt => stackmap_attr_name = Some(self.cp.utf8("StackMapTable")),
                An::Rvpa => {
                    rvpa_attr_name = Some(self.cp.utf8("RuntimeVisibleParameterAnnotations"))
                }
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
            self.methods
                .iter()
                .any(|m| m.signature.is_some())
                .then(|| self.cp.utf8("Signature"))
        });
        // `MethodParameters` (written only under `-java-parameters`) interns once, when some method
        // carries one.
        let method_parameters_attr_name = self
            .methods
            .iter()
            .any(|method| !method.method_parameters.is_empty())
            .then(|| self.cp.utf8("MethodParameters"));
        let mut annotation_default_attrs = HashMap::new();
        for default in self
            .methods
            .iter()
            .filter_map(|m| m.annotation_default.as_ref())
        {
            let name = default.attribute_name();
            if !annotation_default_attrs.contains_key(name) {
                annotation_default_attrs.insert(name, self.cp.utf8(name));
            }
        }
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
            let referenced: Vec<InnerClassSpec> = self
                .inner_class_candidates
                .iter()
                .filter(|spec| self.retains_inner_class(spec))
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
        let enclosing_method_attr = self.enclosing_method_attribute();
        // A class-only `Signature` interns its name after `InnerClasses` and `EnclosingMethod`, the
        // order kotlinc writes the three in.
        let signature_attr_name =
            signature_attr_name.or_else(|| class_has_sig.then(|| self.cp.utf8("Signature")));
        // interned at the top of `finish`). kotlinc interns the `SourceFile` name BEFORE the
        // `RuntimeVisibleAnnotations` name, so build this attribute first.
        let sourcefile_attr = sourcefile_value.map(|file_idx| {
            let name = self.cp.utf8("SourceFile");
            let mut body = Vec::new();
            u2(&mut body, file_idx);
            (name, body)
        });
        // The source map follows `SourceFile`: it names the same thing, one file deeper.
        let smap_attr = self.source_map.render().map(|smap| {
            let name = self.cp.utf8("SourceDebugExtension");
            // JVMS 4.7.11: the attribute body is the modified-UTF-8 bytes themselves, with no
            // length prefix and no constant-pool entry of their own.
            (name, smap.into_bytes())
        });
        // Intern `Deprecated` only if the class or a method carries it; a method's own use already
        // interned it in the per-method sequence above. A CLASS-level one interns here — after
        // `InnerClasses` and `SourceFile`, before `RuntimeVisibleAnnotations` — which is kotlinc's
        // order; interning it with the method names put it ahead of both.
        let deprecated_attr_name = method_dep_name.or_else(|| {
            (self.class_deprecated || !self.deprecated_methods.is_empty())
                .then(|| self.cp.utf8("Deprecated"))
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
        // The source map's annotation joins the class's invisible ones last.
        if let Some(body) = smap_annotation {
            self.invisible_annotations.push(body);
        }
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
            let invisible_params = invisible_param_anns(m);
            let rvpa_attr: u16 = u16::from(!m.visible_param_anns.is_empty());
            let ripa_attr: u16 = u16::from(!invisible_params.is_empty());
            let mp_attr: u16 = u16::from(!m.method_parameters.is_empty());
            let ann_attr = mrva_attr + mria_attr + rvpa_attr + ripa_attr + mp_attr;
            let default_attr = u16::from(m.annotation_default.is_some());
            match &m.code {
                None => u2(&mut out, sig_attr + dep_attr + ann_attr + default_attr), // abstract: optional Signature [+ Deprecated] [+ anns/default]
                Some(code) => {
                    u2(&mut out, 1 + sig_attr + dep_attr + ann_attr + default_attr); // Code [+ Signature] [+ Deprecated] [+ anns/default]
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
            if let Some(default) = &m.annotation_default {
                u2(&mut out, annotation_default_attrs[default.attribute_name()]);
                u4(&mut out, default.bytes().len() as u32);
                out.extend_from_slice(default.bytes());
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
            // A parameter-annotation attribute body: num_parameters(u1) + per-parameter
            // [num_annotations(u2) + annotations].
            let write_param_anns = |out: &mut Vec<u8>, name: u16, params: &[Vec<Vec<u8>>]| {
                u2(out, name);
                let body_len: usize = 1 + params
                    .iter()
                    .map(|p| 2 + p.iter().map(|a| a.len()).sum::<usize>())
                    .sum::<usize>();
                u4(out, body_len as u32);
                out.push(params.len() as u8);
                for p in params {
                    u2(out, p.len() as u16);
                    for a in p {
                        out.extend_from_slice(a);
                    }
                }
            };
            if rvpa_attr == 1 {
                write_param_anns(&mut out, rvpa_attr_name.unwrap(), &m.visible_param_anns);
            }
            if ripa_attr == 1 {
                write_param_anns(&mut out, ripa_attr_name.unwrap(), &invisible_params);
            }
            // `MethodParameters` comes after every annotation attribute — kotlinc's order.
            if mp_attr == 1 {
                u2(
                    &mut out,
                    method_parameters_attr_name
                        .expect("a method carrying parameters interns the attribute name"),
                );
                u4(&mut out, 1 + m.method_parameters.len() as u32 * 4);
                out.push(m.method_parameters.len() as u8);
                for (parameter, flags) in &m.method_parameters {
                    u2(&mut out, *parameter);
                    u2(&mut out, *flags);
                }
            }
        }
        // Assemble the class attribute table in kotlinc's fixed order. `self.class_attributes` is empty
        // in practice (nothing pushes to it outside `finish`); it is prepended to preserve the API.
        let mut ordered: Vec<(u16, Vec<u8>)> = std::mem::take(&mut self.class_attributes);
        // `InnerClasses`, then `EnclosingMethod`, then `Signature` — kotlinc's order for a generic
        // class, an enum and a generated local class alike (verified against each). Writing the
        // signature first left them transposed on every class with a nested member and a generic
        // supertype.
        ordered.extend(inner_classes_attr);
        ordered.extend(enclosing_method_attr);
        if let Some(sig) = self.class_signature {
            let mut body = Vec::new();
            u2(&mut body, sig);
            ordered.push((signature_attr_name.unwrap(), body));
        }
        ordered.extend(
            [
                sourcefile_attr,
                smap_attr,
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
        pool_layout::relayout(out, &relaid, self.unnamed_entries())
    }
}

fn u2(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_be_bytes());
}
fn u4(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_be_bytes());
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

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Label {
    builder: u64,
    index: u32,
}

static NEXT_CODE_BUILDER_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
pub struct CodeBuilder {
    id: u64,
    pub bytes: Vec<u8>,
    pub max_stack: u16,
    pub max_locals: u16,
    cur_stack: i32,
    labels: Vec<usize>, // label id -> bound byte offset (usize::MAX until bound)
    fixups: Vec<(usize, Label)>, // (operand position, destination) to patch in link()
    /// Switch branch operands: `(operand position, opcode position, destination)`. A switch's offsets
    /// are FOUR bytes and are measured from the OPCODE rather than from the operand, so they cannot
    /// share the two-byte `fixups` list.
    switch_fixups: Vec<(usize, usize, Label)>,
    /// Exception-table entries by label: `(start, end, handler, catch_type)`, resolved in `link()`.
    exceptions: Vec<(Label, Label, Label, u16)>,
    /// `LineNumberTable` marks recorded during emission: `(start_pc, line)`. See [`Self::mark_line`].
    line_marks: Vec<(u16, u16)>,
    /// A bytecode offset whose last recorded line mark must be KEPT when another mark lands on the
    /// same offset: the next one appends after it instead of replacing it. See
    /// [`CodeBuilder::mark_line_retained`].
    retained_line_mark: Option<usize>,
    /// `(start_pc, length, slot, name, descriptor)` entries in scope-close order.
    local_entries: Vec<(u16, Option<u16>, u16, String, String)>,
    /// Offset of the implicit void return appended by declared-function emission. Ordinary
    /// `ret_void` calls intentionally do not populate it.
    implicit_void_return_pc: Option<u16>,
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
    /// region, which is also where the next live instruction lands. Rewrites and side-table
    /// resolution must ignore them rather than attach dead ranges to that live instruction.
    /// Indexed like `labels`; `false` for a label bound normally, and for one never bound at all.
    dead_bound: Vec<bool>,
    /// When each label was last bound, by label id (0 = never): the order labels bound at one offset
    /// stand in, which a bytecode rewrite that inserts an instruction between them needs.
    bind_sequence: Vec<u32>,
    next_bind: u32,
    /// Pending line-mark obligations (see [`line_numbers::PendingLines`]).
    pending_lines: line_numbers::PendingLines,
}

impl CodeBuilder {
    pub fn new(arg_locals: u16) -> CodeBuilder {
        CodeBuilder {
            id: NEXT_CODE_BUILDER_ID.fetch_add(1, Ordering::Relaxed),
            bytes: Vec::new(),
            max_stack: 0,
            max_locals: arg_locals,
            cur_stack: 0,
            labels: Vec::new(),
            fixups: Vec::new(),
            switch_fixups: Vec::new(),
            exceptions: Vec::new(),
            line_marks: Vec::new(),
            retained_line_mark: None,
            local_entries: Vec::new(),
            implicit_void_return_pc: None,
            dead: false,
            dead_bound: Vec::new(),
            bind_sequence: Vec::new(),
            next_bind: 0,
            pending_lines: Default::default(),
        }
    }

    /// Whether `label` was bound inside a dropped dead region (see `dead_bound`).
    fn is_dead_bound(&self, label: u32) -> bool {
        self.dead_bound
            .get(label as usize)
            .copied()
            .unwrap_or(false)
    }

    fn label_index(&self, label: Label) -> usize {
        assert_eq!(
            label.builder, self.id,
            "a label can only be bound by its owning bytecode builder"
        );
        label.index as usize
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
                    .all(|&label| self.labels[self.label_index(label)] != usize::MAX)
            })
            .map(|&(s, e, h, t)| {
                (
                    self.labels[self.label_index(s)] as u16,
                    self.labels[self.label_index(e)] as u16,
                    self.labels[self.label_index(h)] as u16,
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

    /// Declare the operand-stack height at a block this builder cannot infer linearly.
    ///
    /// The tracker follows the instruction stream, so a block entered only by a branch inherits the
    /// height left by whatever was emitted before it — which is not what reaches it. A coroutine
    /// machine's dispatch is built of such blocks: each ends in a `goto` with the resumed value on
    /// the stack, and the one that follows starts empty.
    pub fn set_stack_height(&mut self, height: i32) {
        self.cur_stack = height;
    }

    pub(crate) fn can_fall_through(&self) -> bool {
        !self.dead
    }

    /// Force the current operand-stack height (e.g. an exception handler is entered with the caught
    /// exception already on the stack). Keeps `max_stack` correct across non-linear control flow.
    pub fn set_stack(&mut self, n: u16) {
        self.cur_stack = n as i32;
        if n > self.max_stack {
            self.max_stack = n;
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
        // kotlinc (ASM `InstructionAdapter`) emits the short const ops for the EXACT bit patterns
        // of 0.0f/1.0f/2.0f — a bit test, so `-0.0f` (sign bit set) stays an `ldc` constant.
        match v.to_bits() {
            0x0000_0000 => self.op(0x0b, 1), // fconst_0
            0x3f80_0000 => self.op(0x0c, 1), // fconst_1
            0x4000_0000 => self.op(0x0d, 1), // fconst_2
            _ => {
                let i = cw.const_float(v);
                self.ldc(i); // float is one slot
            }
        }
    }
    pub fn push_double(&mut self, v: f64, cw: &mut ClassWriter) {
        // Same bit-pattern rule as `push_float`; doubles only have `dconst_0`/`dconst_1`.
        match v.to_bits() {
            0x0000_0000_0000_0000 => self.op(0x0e, 2), // dconst_0
            0x3ff0_0000_0000_0000 => self.op(0x0f, 2), // dconst_1
            _ => {
                let i = cw.const_double(v);
                self.op_u2(0x14, i, 2); // ldc2_w
            }
        }
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

    pub fn implicit_ret_void(&mut self) {
        if self.dead {
            return;
        }
        let pc = u16::try_from(self.bytes.len()).expect("a JVM method body fits in u16");
        assert!(
            self.implicit_void_return_pc.replace(pc).is_none(),
            "a method has only one implicit void return"
        );
        self.ret_void();
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

    #[test]
    fn inner_class_name_seeding_keeps_annotation_only_references() {
        let nested = "dep/Outer$Nested";
        let mut writer = ClassWriter::new("Use", "java/lang/Object");
        writer.add_inner_class(InnerClassSpec {
            inner: nested.to_owned(),
            outer: Some("dep/Outer".to_owned()),
            name: Some("Nested".to_owned()),
            access: ACC_PUBLIC | ACC_STATIC | ACC_FINAL,
        });
        writer.set_runtime_annotations(&[crate::ir::AppliedAnnotation {
            internal: crate::types::type_name(nested),
            values: Vec::new(),
        }]);
        assert!(!writer.cp.has_class(nested));

        writer.seed_inner_class_names();

        assert!(writer.cp.has_class("dep/Outer"));
        assert!(writer.cp.lookup_utf8("Nested").is_some());
        let info = crate::jvm::classreader::parse_class(&writer.finish()).expect("parse class");
        assert_eq!(info.inner_classes.len(), 1);
        assert_eq!(info.inner_classes[0].inner, nested);
    }

    #[test]
    fn inner_class_name_seeding_sorts_late_resolver_rows_with_source_rows() {
        let resolver: InnerClassResolver = Rc::new(|internal| {
            (internal == "aa/Outer$Alpha").then(|| InnerClassDetails {
                outer: Some("aa/Outer".to_owned()),
                name: Some("Alpha".to_owned()),
                access: ACC_PUBLIC | ACC_STATIC | ACC_FINAL,
            })
        });
        let mut writer = ClassWriter::new("zz/Use", "java/lang/Object");
        writer.add_inner_class(InnerClassSpec {
            inner: "zz/Use$Zulu".to_owned(),
            outer: Some("zz/Use".to_owned()),
            name: Some("Zulu".to_owned()),
            access: ACC_PUBLIC | ACC_STATIC | ACC_FINAL,
        });
        writer.set_inner_class_resolver(Some(resolver));
        writer.class_ref("aa/Outer$Alpha");

        writer.seed_inner_class_names();

        assert!(writer.cp.lookup_utf8("Alpha") < writer.cp.lookup_utf8("Zulu"));
        let info = crate::jvm::classreader::parse_class(&writer.finish()).expect("parse class");
        assert_eq!(
            info.inner_classes
                .iter()
                .map(|entry| entry.inner.as_str())
                .collect::<Vec<_>>(),
            ["aa/Outer$Alpha", "zz/Use$Zulu"]
        );
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
    fn a_pool_index_past_a_wide_constant_names_the_entry_in_that_slot() {
        let mut cp = ConstPool::default();
        let first = cp.utf8("first");
        let wide = cp.long(5);
        let double = cp.double(2.5);
        let next = cp.utf8("next");
        let class = cp.class("pkg/Owner");
        assert_eq!((first, wide, double, next, class), (1, 2, 4, 6, 8));
        assert_eq!(cp.slot_count(), 8);
        assert_eq!(cp.utf8_at(first), Some("first"));
        assert!(matches!(cp.entry_at(wide), Some(Const::Long(5))));
        assert!(matches!(cp.entry_at(double), Some(Const::Double(_))));
        assert_eq!(cp.utf8_at(next), Some("next"));
        assert_eq!(cp.class_name(class), Some("pkg/Owner"));
        // The second slot of a wide constant, and any slot past the pool, name no entry.
        assert!(cp.entry_at(wide + 1).is_none());
        assert!(cp.entry_at(double + 1).is_none());
        assert!(cp.entry_at(0).is_none());
        assert!(cp.entry_at(class + 1).is_none());
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
        cw.rewrite_methods(); // the empty entry goes as the method is written
        assert!(cw.methods[0].lvt.is_empty());
    }
}
