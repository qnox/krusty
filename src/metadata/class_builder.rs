//! Build the `@kotlin.Metadata` `d1`/`d2` payload for a Kotlin *class* (kind=1), so a Kotlin
//! consumer recognizes krusty's emitted class as a genuine Kotlin class (property syntax, etc.).
//!
//! Schema reverse-engineered from kotlinc 1.9.24 for `class Point(val x: Int, var y: String)`
//! (see METADATA_NOTES.md). `ProtoBuf.Class`: f3=fq_name (a class-id string-table entry),
//! f6=supertype `Type`, f8=constructor, f10=property (repeated). `Type.class_name`=f6.
//! `Constructor`: f2=value_parameter, f100=JvmMethodSignature ext (desc). `Property`: f2=name,
//! f3=return_type, f11=flags (emitted as 1798 only for a `var`), f100=JvmPropertySignature
//! {f1=field (empty → derived), f3=getter, f4=setter}. `JvmMethodSignature`: f1=name, f2=desc.
//!
//! String table: a class id uses operation `DESC_TO_CLASS_ID` (Record.f3=2) over `Lpkg/Name;`;
//! builtin types use `predefined_index` (Record.f2); everything else is a verbatim d2 entry.

use crate::metadata::type_encoder::{
    encode_indexed_type_parameter, encode_metadata_type_parameter, encode_type,
    semantic_type_parameters, MetadataTypeParameter, StringTable, TypeParameters,
};
use crate::metadata::{property_flags, protobuf::Pb};
use crate::types::{Ty, Visibility};

/// Property descriptor for class metadata: name, type, mutability, and JVM accessor signatures.
pub struct PropMeta {
    pub name: String,
    pub ty: Ty,
    /// How SOURCE spelled the declared type and receiver — see [`FnMeta::spellings`].
    pub spellings: crate::spelling::DeclaredSpellings,
    pub is_var: bool,
    pub visibility: Visibility,
    /// The property has a compile-time constant initializer.
    pub has_constant: bool,
    /// Whether the property is declared `const`.
    pub is_const: bool,
    /// An ABSTRACT property (an interface member, or `abstract val`): kotlinc records the abstract
    /// modality in `Property.flags` and, since there is no backing field, omits the
    /// `JvmPropertySignature.field` entry entirely.
    pub is_abstract: bool,
    /// Whether this declaration owns a backing field. A concrete computed property has accessor code
    /// but no field, just like an abstract property has no field, so modality cannot encode this fact.
    pub has_backing_field: bool,
    /// Index of the class type parameter this property is declared as (`class C<T>(val a: T)` → 0).
    /// `None` for an ordinary type.
    pub tparam: Option<u32>,
    /// Extension-receiver type (`Property.receiver_type` = f5) for a MEMBER EXTENSION property
    /// (`object Tools { val Int.doubled get() }`). Its presence marks the record an extension —
    /// without it a consumer sees an ordinary member property that does not exist. `None` for an
    /// ordinary member.
    pub receiver: Option<Ty>,
    /// `(jvm name, jvm descriptor)` of the accessor, when one is emitted.
    pub getter: Option<(String, String)>,
    pub setter: Option<(String, String)>,
    /// An explicit `JvmFieldSignature.desc` for a backing field whose descriptor the reader cannot
    /// derive from the Kotlin type — a VALUE-CLASS-typed property, whose field holds the erased
    /// underlying (`val k: K` → `Ljava/lang/String;`). `None` leaves the field derived, which is what
    /// every ordinary property records. (The boxed-nullable-primitive and bare-type-parameter cases
    /// below are derived from the property itself, since the shape alone determines them.)
    pub field_desc: Option<String>,
    /// An explicit `JvmFieldSignature.name` when the PHYSICAL backing field's JVM name differs from
    /// the property name — an instance property mangled to dodge a same-named companion static
    /// (`result` → `result$1`). `None` leaves the name derived (the property name), which is what
    /// every ordinary property records.
    pub field_name: Option<String>,
    /// Annotations that landed on the PROPERTY (`Property.annotation` = f14). A Kotlin property has
    /// no class-file declaration, so their attribute lives on the synthetic marker named by
    /// [`Self::synthetic_method`]; this record is how a consumer gets from the property to them.
    pub annotations: Vec<crate::ir::AppliedAnnotation>,
    /// Annotations that landed on the BACKING FIELD (`@Target(FIELD)`) — recorded separately (f34),
    /// because the reader must not attribute a field annotation to the property.
    pub field_annotations: Vec<crate::ir::AppliedAnnotation>,
    /// `(name, descriptor)` of the `get<Name>$annotations()` marker method carrying
    /// [`Self::annotations`] — `JvmPropertySignature.syntheticMethod` (f2). `None` when the property
    /// has no property-targeted annotation.
    pub synthetic_method: Option<(String, String)>,
}

/// Member-function descriptor for class metadata (`Class.function` = f9). The JVM signature is usually
/// derivable, so no extension is emitted — EXCEPT when a param/return is a boxed nullable primitive
/// (`Int?` → `Integer`), where kotlinc records the descriptor via a `JvmMethodSignature` (f100).
pub struct FnMeta {
    pub name: String,
    pub params: Vec<(String, Ty)>,
    pub ret: Ty,
    /// Extension-receiver type (`Function.receiver_type` = f5) for a MEMBER EXTENSION
    /// (`class C { operator fun String.invoke(…) }`) — recorded separately from `params` (the
    /// LOGICAL value parameters, receiver excluded). `None` for an ordinary member.
    pub receiver: Option<Ty>,
    /// Function-owned type parameters. Class parameters are inherited from the enclosing class
    /// table; these are emitted on the function with ids following that inherited prefix.
    pub type_params: Vec<String>,
    /// Semantic identities parallel to `type_params`.
    pub semantic_type_params: Vec<String>,
    pub type_param_bounds: Vec<Vec<Ty>>,
    /// `Function.flags` (f9): e.g. operator (`componentN`) or the data-class `copy`. 0 ⇒ omitted.
    pub flags: u64,
    /// Mark every value parameter `DECLARES_DEFAULT_VALUE` (so a Kotlin caller may omit it) — used
    /// for the synthesized `copy`.
    pub params_have_defaults: bool,
    /// Per-parameter `DECLARES_DEFAULT_VALUE` for a DECLARED member (`fun f(a: Int, b: Int = 2)`),
    /// parallel to `params` (empty = none default). Composes with `params_have_defaults`.
    pub param_defaults: Vec<bool>,
    /// Index into `params` of a `vararg` parameter — emits `ValueParameter.vararg_element_type`
    /// (f4), the only place vararg-ness survives into metadata.
    pub vararg_index: Option<usize>,
    /// The JVM method descriptor for a `JvmMethodSignature` (f100), emitted only when the signature is
    /// not derivable from the proto types — a boxed nullable-primitive param/return on a synthesized
    /// `componentN`/`copy`, or a value class's `equals`/`hashCode`/`toString` (which dispatch to a
    /// differently-named static `-impl`). `None` ⇒ no extension.
    pub jvm_sig: Option<String>,
    /// The `JvmMethodSignature.name` (f1) when the JVM name differs from the Kotlin one — a value
    /// class's `equals` → `equals-impl`. `None` ⇒ name omitted (derivable), kotlinc's usual shape.
    pub jvm_sig_name: Option<String>,
    /// How SOURCE spelled this member's declared types, so a `typealias` becomes
    /// `Type.abbreviated_type` (see [`crate::spelling`]). Default for a SYNTHESIZED member
    /// (`componentN`, `copy`, `equals`), which has no source spelling at all.
    pub spellings: crate::spelling::DeclaredSpellings,
    /// BINARY/RUNTIME-retained annotations applied to the member, with their frontend-checked element
    /// values — emitted as `Function.annotation` (f12) and setting the `HAS_ANNOTATIONS` flag bit. The
    /// class file's `Runtime[In]VisibleAnnotations` attribute makes the annotation work at RUNTIME;
    /// this record is what a KOTLIN consumer (and `kotlin-reflect`) reads the declaration's
    /// annotations back from. SOURCE-retained annotations never enter this list.
    pub annotations: Vec<crate::ir::AppliedAnnotation>,
}

impl FnMeta {
    /// A plain member function (public, final, a declaration) — the common case, whose flags are
    /// kotlinc's default and therefore omitted from the proto.
    pub fn plain(name: String, params: Vec<(String, Ty)>, ret: Ty) -> FnMeta {
        FnMeta {
            name,
            params,
            ret,
            type_params: Vec::new(),
            semantic_type_params: Vec::new(),
            type_param_bounds: Vec::new(),
            flags: DEFAULT_FUNCTION_FLAGS,
            params_have_defaults: false,
            receiver: None,
            param_defaults: Vec::new(),
            vararg_index: None,
            jvm_sig: None,
            jvm_sig_name: None,
            spellings: crate::spelling::DeclaredSpellings::default(),
            annotations: Vec::new(),
        }
    }
}

/// `Function.flags` kotlinc emits for a data class's synthesized `componentN` (public final
/// operator member) and `copy` (public final member). Reverse-engineered from kotlinc 1.9.24.
pub const COMPONENT_FN_FLAGS: u64 = 454;
pub const COPY_FN_FLAGS: u64 = 198;
/// `Function.flags` for the data-class-synthesized `equals`/`hashCode`/`toString` (public final member,
/// overriding a supertype member — hence the higher bits). From kotlinc 2.4.0.
pub const EQUALS_FN_FLAGS: u64 = 0x101d6;
pub const HASHCODE_TOSTRING_FN_FLAGS: u64 = 0x100d6;
/// `Function.flags` (f9) for a plain `public final` declared member — kotlinc's DEFAULT, so the field
/// is OMITTED at this exact value (mirrors [`DEFAULT_CLASS_FLAGS`]).
pub const DEFAULT_FUNCTION_FLAGS: u64 = 6;
/// `Function.flags` bit 13 — `suspend`. The rest of a suspend function's proto is its DECLARED
/// signature (no `Continuation` parameter, the source return type); this bit is what tells a reader
/// the JVM method is the CPS form.
pub const FN_IS_SUSPEND: u64 = 8192;
/// `Class.flags` (f1) for a plain `public final class` — kotlinc's DEFAULT, so the field is OMITTED at
/// this exact value (an `internal class` writes an explicit `0`, visibility INTERNAL being 0).
pub const DEFAULT_CLASS_FLAGS: u64 = 6;
/// `Constructor.flags` (f1) for a DECLARED (public) secondary constructor — visibility PUBLIC (6) plus
/// the `IS_SECONDARY` bit (16). From kotlinc 2.4.0 (`class Dual { constructor(a: Int, …) }` → 22).
pub const SECONDARY_CTOR_FLAGS: u64 = 22;
/// `Constructor.flags` (f1) for a sealed class's primary constructor — kotlinc marks it PROTECTED.
pub const SEALED_CTOR_FLAGS: u64 = 4;
/// `Constructor.flags` (f1) for an `object`'s primary constructor — kotlinc marks it PRIVATE
/// (visibility bits 1-3 = 1). Instances come only from the static `INSTANCE` field.
pub const OBJECT_CTOR_FLAGS: u64 = 2;
/// `Constructor.flags` (f1) for a plain PUBLIC constructor — the proto's DEFAULT, so the field is
/// omitted at this value and callers pass 0 to mean it. Written explicitly once another bit forces
/// the field out.
const PUBLIC_CTOR_FLAGS: u64 = 6;
/// `Constructor.flags` bit 0 — the declaration carries annotation records.
const HAS_ANNOTATIONS: u64 = 1;
/// `ValueParameter.flags` bit for `DECLARES_DEFAULT_VALUE`.
const DECLARES_DEFAULT_VALUE: u64 = 2;

fn property_flags(prop: &PropMeta) -> u64 {
    let visibility = match prop.visibility {
        Visibility::Internal => 0,
        Visibility::Private => 2,
        Visibility::Protected => 4,
        Visibility::Public => 6,
    };
    (property_flags::DEFAULT & !property_flags::VISIBILITY_MASK)
        | visibility
        | if prop.is_var {
            property_flags::IS_VAR | property_flags::HAS_SETTER
        } else {
            0
        }
        | if prop.has_constant {
            property_flags::HAS_CONSTANT
        } else {
            0
        }
        | if prop.is_const {
            property_flags::IS_CONST
        } else {
            0
        }
        | if prop.is_abstract {
            property_flags::MODALITY_ABSTRACT
        } else {
            0
        }
}

fn type_pb(st: &mut StringTable, t: Ty, type_parameters: &TypeParameters) -> Pb {
    encode_type(st, t, type_parameters)
        .unwrap_or_else(|error| panic!("invalid emitted metadata type: {error}"))
}

/// [`type_pb`] for a DECLARED type, carrying how source spelled it so a `typealias` becomes
/// `Type.abbreviated_type` (field 13).
fn type_pb_declared(
    st: &mut StringTable,
    t: Ty,
    spelled: &crate::spelling::Spelled,
    type_parameters: &TypeParameters,
) -> Pb {
    crate::metadata::type_encoder::encode_declared_type(st, t, spelled, type_parameters)
        .unwrap_or_else(|error| panic!("invalid emitted metadata type: {error}"))
}

/// [`type_pb`], but a `Some(id)` encodes the type as `Type.typeParameter` (f7) — a bare type parameter
/// (`val a: T`), which kotlinc records by INDEX rather than by the erased `java/lang/Object` class name.
fn type_pb_tp(
    st: &mut StringTable,
    t: Ty,
    tparam: Option<u32>,
    spelled: &crate::spelling::Spelled,
    type_parameters: &TypeParameters,
) -> Pb {
    match tparam {
        // A bare type parameter is recorded by INDEX and has no classifier to abbreviate.
        Some(index) => encode_indexed_type_parameter(st, t, index),
        None => {
            crate::metadata::type_encoder::encode_declared_type(st, t, spelled, type_parameters)
        }
    }
    .unwrap_or_else(|error| panic!("invalid emitted metadata type: {error}"))
}

/// Build one `Class.constructor` message: `flags` (f1, omitted if 0), value parameters (f2), and the
/// JvmProtoBuf constructor signature (f100, name `<init>` + `desc`).
struct CtorShape<'a> {
    params: &'a [(String, Ty)],
    /// How SOURCE spelled each parameter's type, parallel to `params` — a primary constructor's
    /// come from the class header (`class Holder(val p: Cargo)`). Empty leaves them unabbreviated,
    /// which is right for a SECONDARY constructor until its own spellings are threaded.
    param_spellings: &'a [crate::spelling::Spelled],
    desc: &'a str,
    flags: u64,
    param_defaults: &'a [bool],
    param_tparams: &'a [Option<u32>],
    sig_name: Option<&'a str>,
    emit_jvm_signature: bool,
    /// Index into `params` of a `vararg` parameter — emits `ValueParameter.vararg_element_type` (f4),
    /// the only place ctor vararg-ness survives into metadata.
    vararg_index: Option<usize>,
    /// Applied annotations → `Constructor.annotation` (f3) + the `HAS_ANNOTATIONS` flag bit.
    annotations: &'a [crate::ir::AppliedAnnotation],
}

fn build_ctor(st: &mut StringTable, shape: CtorShape<'_>, type_parameters: &TypeParameters) -> Pb {
    let mut ctor = Pb::new();
    // `HAS_ANNOTATIONS` (bit 0) follows from the records below, exactly like a function's. Setting it
    // forces the flags field to be WRITTEN, so the proto default the caller was relying on has to be
    // materialized first: a public primary constructor carries 0 here precisely because 6 (visibility
    // PUBLIC) is the default and the field is omitted at that value — OR-ing bit 0 onto the 0 would
    // write 1 (visibility INTERNAL) where kotlinc writes 7.
    let flags = if shape.annotations.is_empty() {
        shape.flags
    } else {
        (if shape.flags == 0 {
            PUBLIC_CTOR_FLAGS
        } else {
            shape.flags
        }) | HAS_ANNOTATIONS
    };
    if flags != 0 {
        ctor.field_varint(1, flags); // Constructor.flags = 1
    }
    for (i, (pname, pty)) in shape.params.iter().enumerate() {
        let mut vp = Pb::new();
        // `ValueParameter.flags` (f1) with DECLARES_DEFAULT_VALUE for a param that declares a default —
        // written before the name, matching kotlinc.
        if shape.param_defaults.get(i).copied().unwrap_or(false) {
            vp.field_varint(1, DECLARES_DEFAULT_VALUE);
        }
        vp.field_varint(2, st.local(pname) as u64); // ValueParameter.name = 2
        let ty = type_pb_tp(
            st,
            *pty,
            shape.param_tparams.get(i).copied().flatten(),
            shape
                .param_spellings
                .get(i)
                .unwrap_or(crate::spelling::Spelled::NONE),
            type_parameters,
        );
        vp.field_message(3, &ty); // ValueParameter.type = 3
                                  // A vararg parameter records its ELEMENT type as `vararg_element_type` (f4) — the declared
                                  // type stays the array, exactly as the package-function writer does.
        if shape.vararg_index == Some(i) {
            let elem = pty
                .array_elem()
                .or_else(|| pty.type_args().first().copied());
            if let Some(elem) = elem {
                let et = type_pb_declared(
                    st,
                    elem,
                    shape
                        .param_spellings
                        .get(i)
                        .unwrap_or(crate::spelling::Spelled::NONE)
                        .arg(0),
                    type_parameters,
                );
                vp.field_message(4, &et); // ValueParameter.vararg_element_type = 4
            }
        }
        ctor.repeated_message(2, &vp); // Constructor.value_parameter = 2
    }
    // The JVM signature INTERNS first (kotlinc's serializer writes the extension before folding the
    // annotations, so `()V` precedes `Lp/Mark;` in d2) while the annotation SERIALIZES first — the
    // proto fields stay in ascending order. Building both messages before appending either keeps the
    // two orders independent.
    let sig = shape
        .emit_jvm_signature
        .then(|| jvm_method_sig(st, Some(shape.sig_name.unwrap_or("<init>")), shape.desc));
    // Constructor.annotation = 3 — after the value parameters, in kotlinc's ascending field order.
    let annotations: Vec<Pb> = shape
        .annotations
        .iter()
        .map(|annotation| crate::metadata::builder::annotation_pb(st, annotation))
        .collect();
    for annotation in &annotations {
        ctor.repeated_message(3, annotation);
    }
    if let Some(sig) = &sig {
        ctor.field_message(100, sig); // JvmProtoBuf.constructorSignature = 100
    }
    ctor
}

fn jvm_method_sig(st: &mut StringTable, name: Option<&str>, desc: &str) -> Pb {
    let mut p = Pb::new();
    if let Some(n) = name {
        p.field_varint(1, st.local(n) as u64); // JvmMethodSignature.name = 1
    }
    p.field_varint(2, st.local(desc) as u64); // JvmMethodSignature.desc = 2
    p
}

/// Build `(d1 bytes, d2 strings)` for a class. `class_internal` is e.g. `demo/Point`;
/// `ctor_params` are the primary-constructor `(name, type)` pairs; `ctor_desc` its JVM descriptor.
/// `Class.flags` values kotlinc emits: a plain class = 0 (omitted), `data class` = 1030,
/// `object` = 326. Passed in by the caller.
/// Class-level metadata beyond the members: the `Class.flags`, the companion object's simple name (if
/// any), and the nested class simple names — kept in one struct so [`build_class`] stays within the
/// argument-count limit.
/// A secondary constructor for class metadata (`Class.constructor` = f8, repeated after the primary).
pub struct CtorMeta<'a> {
    pub params: &'a [(String, Ty)],
    pub desc: &'a str,
    /// Index into `params` of a `vararg` parameter — emits `ValueParameter.vararg_element_type`
    /// (f4), the record a consumer needs to admit `C(a, b, c)` against `vararg` (without it the
    /// parameter reads as a plain array and the call resolves to nothing).
    pub vararg_index: Option<usize>,
    /// `Constructor.flags` (f8's f1) — e.g. 22 for a plain secondary ctor. 0 ⇒ omitted (the primary).
    pub flags: u64,
    /// BINARY/RUNTIME-retained annotations applied to the constructor — `Constructor.annotation`
    /// (f3), the constructor analogue of [`FnMeta::annotations`].
    pub annotations: &'a [crate::ir::AppliedAnnotation],
}

/// Source declaration order across the protobuf's separately stored function/property lists. The
/// message fields remain grouped by field number, but kotlinc interns their shared string table in
/// source order.
#[derive(Clone, Copy)]
pub enum ClassMemberOrder {
    Property(usize),
    Function(usize),
}

pub struct ClassTail<'a> {
    /// How SOURCE spelled the CLASS HEADER's types: primary-constructor parameters and
    /// type-parameter bounds. Members carry their own on [`FnMeta`]/[`PropMeta`].
    pub spellings: crate::spelling::DeclaredSpellings,
    /// Supertype spellings ALREADY ALIGNED to [`Self::supertypes`] — see
    /// [`DeclaredSpellings::supertype_spellings`](crate::spelling::DeclaredSpellings::supertype_spellings).
    /// Only the emitter knows whether that list reserves a leading slot for an undeclared
    /// superclass, so the alignment happens there rather than here.
    pub supertype_spellings: &'a [crate::spelling::Spelled],
    pub flags: u64,
    pub companion: Option<&'a str>,
    pub nested: &'a [&'a str],
    pub member_order: &'a [ClassMemberOrder],
    /// The `-module-name` value → `Class.classModuleName` (f101, a JvmProtoBuf extension). kotlinc
    /// omits it for the default module `main`; downstream builds always set `-module-name`.
    pub module_name: Option<&'a str>,
    /// Secondary constructors (after the primary), each `Class.constructor` (f8). They intern their
    /// strings right after the primary ctor, before properties/functions.
    pub secondary_ctors: &'a [CtorMeta<'a>],
    /// Per-primary-ctor-parameter `DECLARES_DEFAULT_VALUE` flags (parallel to `ctor_params`). A param
    /// with a default (`routes: List<String> = emptyList()`) gets the flag, as kotlinc emits. Empty ⇒
    /// no param has a default.
    pub ctor_param_defaults: &'a [bool],
    /// Per-primary-constructor-parameter class type-parameter index, for a parameter declared as a
    /// bare type parameter (`class C<T>(val a: T)` → `[Some(0)]`).
    pub ctor_param_tparams: &'a [Option<u32>],
    /// A `@JvmInline value class`'s sole underlying property `(name, type)` → `Class`
    /// `inlineClassUnderlyingPropertyName` (f17, the name's string-table id) +
    /// `inlineClassUnderlyingType` (f18, an inline `Type`). `None` for an ordinary class.
    pub inline_underlying: Option<(&'a str, Ty)>,
    /// `Class` JvmProtoBuf extension field 104 (`jvmClassFlags`) — kotlinc emits `3` for an interface.
    /// `None` for every other kind (field omitted).
    pub jvm_class_flags: Option<u64>,
    /// A compiler-version requirement attached to the class. `-jvm-default=no-compatibility`
    /// requires compiler 1.4.0 so older consumers do not interpret the interface under the legacy
    /// `$DefaultImpls` rules. The tuple is `(major, minor, patch)`.
    pub compiler_version_requirement: Option<(u8, u8, u8)>,
    /// Index of a `vararg` PRIMARY-ctor parameter (into `ctor_params`), for its
    /// `vararg_element_type` record. `None` ⇒ no vararg parameter.
    pub ctor_vararg_index: Option<usize>,
    /// Whether the class HAS a primary constructor at all — an `interface` has none, so `Class` carries
    /// no `constructor` (f8) entry. Defaults to true (every other kind).
    pub emit_primary_ctor: bool,
    /// `Constructor.flags` (f1) for the PRIMARY constructor — 0 (omitted) for an ordinary class; a
    /// sealed class's primary ctor is PROTECTED, which kotlinc records.
    pub primary_ctor_flags: u64,
    /// Whether the primary declaration has a JVM constructor realization. Kotlin annotation
    /// classes expose a language-level constructor in metadata, but their classfile is an
    /// annotation interface and therefore has no `<init>` method.
    pub primary_ctor_jvm_signature: bool,
    /// The primary constructor's `JvmMethodSignature` NAME — a value class's primary ctor is realized as
    /// the static `constructor-impl`, not `<init>`. `None` ⇒ `<init>` (the ordinary shape).
    pub ctor_sig_name: Option<&'a str>,
    /// Declared type-parameter names in order (`class C<T>` → `["T"]`), recorded as
    /// `Class.typeParameter` so the metadata describes the class as generic.
    pub type_params: &'a [String],
    pub type_param_bounds: &'a [crate::ir::IrTypeParameter],
    /// Enclosing declaration parameters referenced by this class's members. Kotlin metadata does
    /// not repeat their declarations, but reserves their IDs before this class's own parameters.
    pub captured_type_params: &'a [String],
    /// Direct sealed subtypes as JVM descriptors.
    pub sealed_subclasses: &'a [&'a str],
    /// Declared semantic supertypes, including applied type arguments. Physical erasure belongs to
    /// the classfile `super_class`/`interfaces` entries; Kotlin metadata must retain `H<A>` so
    /// reflection and downstream type substitution do not see a raw `H`.
    pub supertypes: &'a [Ty],
    /// BINARY/RUNTIME-retained annotations attached to the class declaration.
    pub annotations: &'a [crate::ir::AppliedAnnotation],
    /// BINARY/RUNTIME-retained annotations declared on the PRIMARY constructor — `Constructor.annotation`
    /// (f3) of the primary record, the counterpart of [`CtorMeta::annotations`] for the secondaries.
    pub primary_ctor_annotations: &'a [crate::ir::AppliedAnnotation],
}

impl Default for ClassTail<'_> {
    fn default() -> Self {
        ClassTail {
            supertype_spellings: &[],
            spellings: crate::spelling::DeclaredSpellings::default(),
            flags: DEFAULT_CLASS_FLAGS,
            companion: None,
            nested: &[],
            member_order: &[],
            module_name: None,
            secondary_ctors: &[],
            ctor_param_defaults: &[],
            ctor_param_tparams: &[],
            inline_underlying: None,
            ctor_sig_name: None,
            jvm_class_flags: None,
            compiler_version_requirement: None,
            ctor_vararg_index: None,
            emit_primary_ctor: true,
            primary_ctor_flags: 0,
            primary_ctor_jvm_signature: true,
            type_params: &[],
            type_param_bounds: &[],
            captured_type_params: &[],
            sealed_subclasses: &[],
            supertypes: &[],
            annotations: &[],
            primary_ctor_annotations: &[],
        }
    }
}

pub fn build_class(
    class_internal: &str,
    ctor_params: &[(String, Ty)],
    ctor_desc: &str,
    props: &[PropMeta],
    methods: &[FnMeta],
    enum_entries: &[String],
    tail: &ClassTail,
) -> (Vec<u8>, Vec<String>) {
    let class_flags = tail.flags;
    let companion_name = tail.companion;
    let nested_class_names = tail.nested;
    let mut st = StringTable::default();

    // STRINGS ARE INTERNED IN kotlinc's ORDER (fq_name, supertype, constructors, properties'
    // JVM signatures, functions, enum entries, then the companion + nested names LAST) even though the
    // proto writes fields in field-number order below — so the d2 indices match. Build every sub-message
    // first (interning), then assemble the `Class` message.

    // f3 = fq_name: a class-id derived from the `L...;` descriptor.
    let fq = st.class_id_from_desc(&format!("L{class_internal};"));

    // f5 = typeParameter: `{ id, name }` per declared parameter, in order. kotlinc interns the names
    // right after the fq_name, before any member signature.
    assert_eq!(
        tail.type_param_bounds.len(),
        tail.type_params.len(),
        "metadata class type parameters require semantic identities"
    );
    let captured_count = tail.captured_type_params.len();
    let mut class_type_parameters = TypeParameters::new();
    for (index, semantic) in tail.captured_type_params.iter().enumerate() {
        class_type_parameters.insert(
            semantic.clone(),
            index as u64 | crate::metadata::type_encoder::CAPTURED_TYPE_PARAMETER,
        );
    }
    for (index, (source, parameter)) in tail
        .type_params
        .iter()
        .zip(tail.type_param_bounds)
        .enumerate()
    {
        let id = (captured_count + index) as u64;
        class_type_parameters.insert(source.clone(), id);
        class_type_parameters.insert(parameter.semantic_name.clone(), id);
    }
    let tparam_msgs: Vec<Pb> = tail
        .type_param_bounds
        .iter()
        .enumerate()
        .map(|(i, parameter)| {
            encode_metadata_type_parameter(
                &mut st,
                captured_count + i,
                &MetadataTypeParameter {
                    name: parameter.name.clone(),
                    reified: false,
                    variance: parameter.variance,
                    upper_bounds: parameter.bounds.iter().map(|(bound, _)| *bound).collect(),
                    upper_bound_spellings: tail
                        .spellings
                        .type_param_bounds
                        .get(i)
                        .cloned()
                        .unwrap_or_default(),
                },
                &class_type_parameters,
            )
            .unwrap_or_else(|error| panic!("invalid emitted metadata type parameter: {error}"))
        })
        .collect();

    // Enums use `Enum<E>`; classes without declarations use `Any`.
    let mut supertype_msgs: Vec<Pb> = Vec::new();
    if !enum_entries.is_empty() {
        supertype_msgs.push(type_pb(
            &mut st,
            Ty::obj_args("kotlin/Enum", &[Ty::obj(class_internal)]),
            &class_type_parameters,
        ));
    } else if tail.supertypes.is_empty() {
        supertype_msgs.push(type_pb(
            &mut st,
            Ty::obj("kotlin/Any"),
            &class_type_parameters,
        ));
    } else {
        for (index, supertype) in tail.supertypes.iter().enumerate() {
            supertype_msgs.push(type_pb_declared(
                &mut st,
                *supertype,
                tail.supertype_spellings
                    .get(index)
                    .unwrap_or(crate::spelling::Spelled::NONE),
                &class_type_parameters,
            ));
        }
    }

    // f8 = constructors: the primary (flags 0), then any secondary constructors — each interning in
    // order (kotlinc emits the ctor JVM name `<init>` explicitly, not omitted).
    let ctor_param_tparams = tail
        .ctor_param_tparams
        .iter()
        .map(|parameter| parameter.map(|index| index + captured_count as u32))
        .collect::<Vec<_>>();
    let mut ctor_msgs = if tail.emit_primary_ctor {
        vec![build_ctor(
            &mut st,
            CtorShape {
                params: ctor_params,
                // A primary-constructor parameter's spelling is part of the CLASS HEADER.
                param_spellings: &tail.spellings.params,
                desc: ctor_desc,
                flags: tail.primary_ctor_flags,
                param_defaults: tail.ctor_param_defaults,
                param_tparams: &ctor_param_tparams,
                sig_name: tail.ctor_sig_name,
                emit_jvm_signature: tail.primary_ctor_jvm_signature,
                vararg_index: tail.ctor_vararg_index,
                annotations: tail.primary_ctor_annotations,
            },
            &class_type_parameters,
        )]
    } else {
        Vec::new()
    };
    for sc in tail.secondary_ctors {
        ctor_msgs.push(build_ctor(
            &mut st,
            CtorShape {
                params: sc.params,
                param_spellings: &[],
                desc: sc.desc,
                flags: sc.flags,
                param_defaults: &[],
                param_tparams: &[],
                sig_name: None,
                emit_jvm_signature: true,
                vararg_index: sc.vararg_index,
                annotations: sc.annotations,
            },
            &class_type_parameters,
        ));
    }

    let build_prop = |st: &mut StringTable, p: &PropMeta| {
        let mut prop = Pb::new();
        prop.field_varint(2, st.local(&p.name) as u64); // Property.name = 2
        let ty = type_pb_tp(
            st,
            p.ty,
            p.tparam.map(|index| index + captured_count as u32),
            &p.spellings.ret,
            &class_type_parameters,
        );
        prop.field_message(3, &ty); // Property.return_type = 3
        if let Some(recv) = p.receiver {
            // Property.receiver_type = 5 — a member EXTENSION property's declared receiver;
            // its presence is what makes the record an extension.
            let rt = type_pb_declared(st, recv, &p.spellings.receiver, &class_type_parameters);
            prop.field_message(5, &rt);
        }
        // `HAS_ANNOTATIONS` (bit 0) is a function of the annotation records below, on EITHER use
        // site: kotlinc sets it for a field-targeted annotation too.
        let annotated = !p.annotations.is_empty() || !p.field_annotations.is_empty();
        let pflags = property_flags(p) | u64::from(annotated);
        // An accessor's flags word is emitted when it differs from the DEFAULT one, which kotlinc
        // derives from the PROPERTY (its `hasAnnotations` bit included). An annotated property whose
        // getter carries no annotation of its own therefore writes its plain accessor word out.
        let accessor_flags = property_flags::DEFAULT_ACCESSOR;
        if annotated {
            prop.field_varint(7, accessor_flags); // Property.getter_flags = 7
            if p.setter.is_some() {
                prop.field_varint(8, accessor_flags); // Property.setter_flags = 8
            }
        }
        if pflags != property_flags::DEFAULT {
            prop.field_varint(11, pflags); // Property.flags = 11
        }
        let mut jvm = Pb::new();
        // A nullable PRIMITIVE property (`Int?`, `Double?`, …) has a BOXED backing field
        // (`Ljava/lang/Integer;`, `Ljava/lang/Double;`), which the reader can't derive from the
        // nullable-primitive return type — so kotlinc records an explicit `JvmFieldSignature.desc`
        // (the boxed descriptor = the getter's return type). Every other property leaves the field
        // empty (the reader derives it). kotlinc interns the getter/setter strings BEFORE the field
        // descriptor (even though the proto writes `field` (f1) first), so build them in that order.
        // The marker interns BEFORE the getter, exactly as it serializes (f2 before f3).
        let synthetic_method = p
            .synthetic_method
            .as_ref()
            .map(|(name, desc)| jvm_method_sig(st, Some(name), desc));
        let getter = p
            .getter
            .as_ref()
            .map(|(gn, gd)| jvm_method_sig(st, Some(gn), gd));
        let setter = p
            .setter
            .as_ref()
            .map(|(sn, sd)| jvm_method_sig(st, Some(sn), sd));
        let boxed_field_desc = p.field_desc.clone().or(match p.ty {
            Ty::Nullable(inner)
                if matches!(
                    *inner,
                    Ty::Int
                        | Ty::Long
                        | Ty::Double
                        | Ty::Float
                        | Ty::Byte
                        | Ty::Short
                        | Ty::Char
                        | Ty::Boolean
                ) =>
            {
                p.getter
                    .as_ref()
                    .and_then(|(_, d)| d.rsplit(')').next().map(str::to_string))
            }
            // A bare type-parameter property erases to `Ljava/lang/Object;`, which the reader
            // cannot derive from the type `T` — so kotlinc records the descriptor explicitly, the
            // same way it does for a boxed nullable primitive.
            _ if p.tparam.is_some() => p
                .getter
                .as_ref()
                .and_then(|(_, d)| d.rsplit(')').next().map(str::to_string)),
            _ => None,
        });
        let mut field = Pb::new();
        if let Some(n) = &p.field_name {
            field.field_varint(1, st.local(n) as u64); // JvmFieldSignature.name = 1
        }
        if let Some(d) = &boxed_field_desc {
            field.field_varint(2, st.local(d) as u64); // JvmFieldSignature.desc = 2
        }
        // An abstract property has no backing field at all — kotlinc omits the entry rather than
        // writing an empty one (which is what a concrete property's derived field looks like).
        // Property.annotation = 14 / the backing field's = 34, both interning after the signature's
        // strings (kotlinc's serializer writes the JVM extension first).
        let annotations: Vec<Pb> = p
            .annotations
            .iter()
            .map(|annotation| crate::metadata::builder::annotation_pb(st, annotation))
            .collect();
        let field_annotations: Vec<Pb> = p
            .field_annotations
            .iter()
            .map(|annotation| crate::metadata::builder::annotation_pb(st, annotation))
            .collect();
        for annotation in &annotations {
            prop.repeated_message(14, annotation); // Property.annotation = 14
        }
        for annotation in &field_annotations {
            prop.repeated_message(34, annotation); // Property.backingFieldAnnotation = 34
        }
        if p.has_backing_field {
            jvm.field_message(1, &field); // field (empty → derived; boxed primitive → explicit desc)
        }
        if let Some(synthetic_method) = &synthetic_method {
            jvm.field_message(2, synthetic_method); // JvmPropertySignature.syntheticMethod = 2
        }
        if let Some(getter) = &getter {
            jvm.field_message(3, getter); // JvmPropertySignature.getter = 3
        }
        if let Some(setter) = &setter {
            jvm.field_message(4, setter); // JvmPropertySignature.setter = 4
        }
        prop.field_message(100, &jvm); // JvmProtoBuf.propertySignature = 100
        prop
    };

    // Member functions (name f2, return_type f3, value_parameter f6, flags f9; JVM sig derivable).
    let build_func = |st: &mut StringTable, m: &FnMeta| {
        let mut func = Pb::new();
        func.field_varint(2, st.local(&m.name) as u64);
        let mut function_type_parameters = class_type_parameters.clone();
        assert_eq!(
            m.semantic_type_params.len(),
            m.type_params.len(),
            "metadata member type parameters require semantic identities"
        );
        let semantic_names = &m.semantic_type_params;
        let own_type_parameters = semantic_type_parameters(
            m.type_params.iter().map(String::as_str),
            semantic_names.iter().map(String::as_str),
        );
        for (index, name) in m.type_params.iter().enumerate() {
            let id = captured_count + tail.type_params.len() + index;
            for key in own_type_parameters
                .iter()
                .filter_map(|(key, own)| (*own == index as u64).then_some(key))
            {
                function_type_parameters.insert(key.clone(), id as u64);
            }
            let parameter = encode_metadata_type_parameter(
                st,
                id,
                &MetadataTypeParameter {
                    name: name.clone(),
                    reified: false,
                    variance: crate::types::TypeVariance::Invariant,
                    upper_bounds: m.type_param_bounds.get(index).cloned().unwrap_or_default(),
                    upper_bound_spellings: m
                        .spellings
                        .type_param_bounds
                        .get(index)
                        .cloned()
                        .unwrap_or_default(),
                },
                &function_type_parameters,
            )
            .unwrap_or_else(|error| panic!("invalid emitted metadata type parameter: {error}"));
            func.repeated_message(4, &parameter);
        }
        let ret = crate::metadata::type_encoder::encode_declared_type(
            st,
            m.ret,
            &m.spellings.ret,
            &function_type_parameters,
        )
        .unwrap_or_else(|error| {
            panic!(
                "invalid emitted metadata return type for '{class_internal}.{}': {error}",
                m.name
            )
        });
        func.field_message(3, &ret);
        if let Some(recv) = m.receiver {
            // Function.receiver_type = 5 — a MEMBER EXTENSION's receiver, restored from the
            // physical `params[0]` realization so consumers see the LOGICAL shape.
            let rt = crate::metadata::type_encoder::encode_declared_type(
                st,
                recv,
                &m.spellings.receiver,
                &function_type_parameters,
            )
            .unwrap_or_else(|error| {
                panic!(
                    "invalid emitted metadata receiver for '{class_internal}.{}': {error}",
                    m.name
                )
            });
            func.field_message(5, &rt);
        }
        for (i, (pname, pty)) in m.params.iter().enumerate() {
            let mut vp = Pb::new();
            if m.params_have_defaults || m.param_defaults.get(i).copied().unwrap_or(false) {
                vp.field_varint(1, DECLARES_DEFAULT_VALUE); // ValueParameter.flags = 1
            }
            vp.field_varint(2, st.local(pname) as u64);
            // A `vararg` parameter is SPELLED as its element but RECORDED as the array; see the
            // package-function writer for why the spelling is lifted rather than applied.
            let declared_spelling = if m.vararg_index == Some(i) {
                m.spellings.param(i).as_array_element()
            } else {
                m.spellings.param(i).clone()
            };
            let ty = crate::metadata::type_encoder::encode_declared_type(
                st,
                *pty,
                &declared_spelling,
                &function_type_parameters,
            )
            .unwrap_or_else(
                    |error| {
                        panic!(
                            "invalid emitted metadata parameter '{pname}' for '{class_internal}.{}': {error}",
                            m.name
                        )
                    },
                );
            vp.field_message(3, &ty);
            if m.vararg_index == Some(i) {
                // ValueParameter.vararg_element_type = 4 — the ELEMENT next to the array type.
                let elem = pty
                    .array_elem()
                    .or_else(|| pty.type_args().first().copied());
                if let Some(elem) = elem {
                    let et = crate::metadata::type_encoder::encode_declared_type(
                        st,
                        elem,
                        m.spellings.param(i),
                        &function_type_parameters,
                    )
                    .unwrap_or_else(|error| {
                        panic!(
                            "invalid emitted metadata vararg element for \
                                     '{class_internal}.{}': {error}",
                            m.name
                        )
                    });
                    vp.field_message(4, &et);
                }
            }
            func.repeated_message(6, &vp); // Function.value_parameter = 6
        }
        // An annotated declaration sets `HAS_ANNOTATIONS` (bit 0) on top of whatever the caller
        // derived — the bit is a function OF the records below, never an independent input.
        let flags = m.flags | u64::from(!m.annotations.is_empty());
        // Omitted at the public-final-declaration default, exactly like `Class.flags`.
        if flags != DEFAULT_FUNCTION_FLAGS {
            func.field_varint(9, flags); // Function.flags = 9
        }
        // The `JvmMethodSignature` (f100) INTERNS before the annotations even though it SERIALIZES
        // after them — kotlinc's serializer writes the extension first, so a suspend member's
        // descriptor precedes `Lp/Mark;` in d2. Build both, then append in field order.
        let sig = m
            .jvm_sig
            .as_ref()
            // desc only (name derivable) unless a mangled realization renamed the method — a boxed
            // nullable-primitive signature kotlinc records because the proto types alone don't pin
            // the JVM descriptor.
            .map(|sig| jvm_method_sig(st, m.jvm_sig_name.as_deref(), sig));
        // Function.annotation = 12 — the applied annotations, each an `Annotation.id` (f1) naming the
        // annotation class through the string table's `DESC_TO_CLASS_ID` form, plus its arguments.
        let annotations: Vec<Pb> = m
            .annotations
            .iter()
            .map(|annotation| crate::metadata::builder::annotation_pb(st, annotation))
            .collect();
        for annotation in &annotations {
            func.repeated_message(12, annotation);
        }
        if let Some(sig) = &sig {
            func.field_message(100, sig);
        }
        func
    };

    let mut prop_msgs: Vec<Option<Pb>> = (0..props.len()).map(|_| None).collect();
    let mut func_msgs: Vec<Option<Pb>> = (0..methods.len()).map(|_| None).collect();
    for member in tail.member_order {
        match *member {
            ClassMemberOrder::Property(index)
                if index < props.len() && prop_msgs[index].is_none() =>
            {
                prop_msgs[index] = Some(build_prop(&mut st, &props[index]));
            }
            ClassMemberOrder::Function(index)
                if index < methods.len() && func_msgs[index].is_none() =>
            {
                func_msgs[index] = Some(build_func(&mut st, &methods[index]));
            }
            _ => {}
        }
    }
    // Unscheduled records are compiler/plugin synthetics or callers using the legacy/default shape.
    // Preserve their established property-then-function order after all explicitly ordered members.
    for (index, prop) in props.iter().enumerate() {
        if prop_msgs[index].is_none() {
            prop_msgs[index] = Some(build_prop(&mut st, prop));
        }
    }
    for (index, function) in methods.iter().enumerate() {
        if func_msgs[index].is_none() {
            func_msgs[index] = Some(build_func(&mut st, function));
        }
    }
    let prop_msgs: Vec<Pb> = prop_msgs
        .into_iter()
        .map(|message| message.expect("every property metadata record is built"))
        .collect();
    let func_msgs: Vec<Pb> = func_msgs
        .into_iter()
        .map(|message| message.expect("every function metadata record is built"))
        .collect();

    // f13 = enum entries (`EnumEntry { name = f1 }`).
    let enum_msgs: Vec<Pb> = enum_entries
        .iter()
        .map(|entry| {
            let mut ee = Pb::new();
            ee.field_varint(1, st.local(entry) as u64);
            ee
        })
        .collect();

    // Class.annotation = f25. Build after members so annotation names/values follow accessor
    // signatures in the shared string table, matching kotlinc's declaration order.
    let annotation_msgs: Vec<Pb> = tail
        .annotations
        .iter()
        .map(|annotation| crate::metadata::builder::annotation_pb(&mut st, annotation))
        .collect();

    // A `@JvmInline value class`'s underlying property name + type (`Class` f17/f18). Interned with the
    // members (before the companion/nested tail) so the d2 order matches kotlinc.
    let inline_underlying: Option<(u32, Pb)> = tail
        .inline_underlying
        .map(|(name, ty)| (st.local(name), type_pb(&mut st, ty, &class_type_parameters)));

    // Nested + companion class names intern LAST (kotlinc's d2 places them after all members) —
    // NESTED names first, then the companion's, even though the companionObjectName FIELD serializes
    // before the nested list (kotlinc registers the strings in that order).
    let nested_idxs: Vec<u32> = nested_class_names.iter().map(|n| st.local(n)).collect();
    let companion_idx = companion_name.map(|c| st.local(c));
    // Sealed subclass IDs precede the module name in kotlinc's string table.
    let sealed_idxs: Vec<u32> = tail
        .sealed_subclasses
        .iter()
        .map(|d| st.class_id_from_desc(d))
        .collect();
    // The module name (f101) interns LAST — kotlinc places it at the end of d2.
    let module_idx = tail.module_name.map(|m| st.local(m));

    // Assemble the `Class` message in FIELD order: f1 flags, f3 fq_name, f4 companionObjectName,
    // f6 supertype, f7 nestedClassName (packed repeated int32), f8 ctors, f9 functions, f10 properties,
    // f13 enum entries.
    let mut class = Pb::new();
    // kotlinc writes `flags` only when it differs from the public-final-class default.
    if class_flags != DEFAULT_CLASS_FLAGS {
        class.field_varint(1, class_flags);
    }
    class.field_varint(3, fq as u64);
    for tp in &tparam_msgs {
        class.repeated_message(5, tp); // Class.type_parameter = 5
    }
    if let Some(ci) = companion_idx {
        class.field_varint(4, ci as u64); // Class.companion_object_name = 4
    }
    for st_msg in &supertype_msgs {
        class.repeated_message(6, st_msg); // Class.supertype = 6 (repeated)
    }
    if !nested_idxs.is_empty() {
        let mut packed = Pb::new();
        for &n in &nested_idxs {
            packed.varint(n as u64);
        }
        class.field_bytes(7, packed.as_bytes()); // Class.nested_class_name = 7 (packed)
    }
    for ctor in &ctor_msgs {
        class.repeated_message(8, ctor); // Class.constructor = 8
    }
    for func in &func_msgs {
        class.repeated_message(9, func); // Class.function = 9
    }
    for prop in &prop_msgs {
        class.repeated_message(10, prop); // Class.property = 10
    }
    for ee in &enum_msgs {
        class.repeated_message(13, ee); // Class.enum_entry = 13
    }
    if !sealed_idxs.is_empty() {
        let mut packed = Pb::new();
        for &idx in &sealed_idxs {
            packed.varint(idx as u64);
        }
        class.field_bytes(16, packed.as_bytes());
    }
    if let Some((name_id, ty_pb)) = &inline_underlying {
        class.field_varint(17, *name_id as u64); // Class.inlineClassUnderlyingPropertyName = 17
        class.field_message(18, ty_pb); // Class.inlineClassUnderlyingType = 18
    }
    for annotation in &annotation_msgs {
        class.repeated_message(25, annotation); // Class.annotation = 25
    }
    if let Some((major, minor, patch)) = tail.compiler_version_requirement {
        // Class.versionRequirement (f31) indexes Class.versionRequirementTable (f32). The compact
        // version word is `major:3 | minor:4 | patch:7`; larger components use versionFull instead.
        // VersionKind.COMPILER_VERSION is enum value 1 (LANGUAGE_VERSION is the omitted default).
        let mut requirement = Pb::new();
        if major <= 7 && minor <= 15 && patch <= 127 {
            requirement.field_varint(
                1,
                u64::from(major) | (u64::from(minor) << 3) | (u64::from(patch) << 7),
            );
        } else {
            requirement.field_varint(
                2,
                u64::from(major) | (u64::from(minor) << 8) | (u64::from(patch) << 16),
            );
        }
        requirement.field_varint(6, 1);
        let mut table = Pb::new();
        table.repeated_message(1, &requirement);
        class.field_varint(31, 0);
        class.field_message(32, &table);
    }
    // Extensions are written in ASCENDING field number, like every other field: 101 before 104.
    if let Some(mi) = module_idx {
        class.field_varint(101, mi as u64); // JvmProtoBuf.classModuleName = 101
    }
    if let Some(v) = tail.jvm_class_flags {
        class.field_varint(104, v); // JvmProtoBuf.classFlags = 104 (interfaces carry 3)
    }

    let stt = st.serialize_types();
    let mut bytes = vec![0x00u8]; // UTF8 mode marker
    let mut prefix = Pb::new();
    prefix.varint(stt.as_bytes().len() as u64); // writeDelimitedTo length prefix
    bytes.extend_from_slice(&prefix.into_bytes());
    bytes.extend_from_slice(stt.as_bytes());
    bytes.extend_from_slice(class.as_bytes());
    (bytes, st.into_strings())
}

/// Build the `(d1, d2)` payload for an ANONYMOUS class (`object : P2 {}` inside a function):
/// kotlinc's record is `Class { flags = LOCAL visibility (10), fq_name = <raw internal, marked
/// localName in the string table>, supertype* }` — no members, no constructor record.
pub fn build_anonymous_class(internal: &str, supertypes: &[Ty]) -> (Vec<u8>, Vec<String>) {
    let mut st = StringTable::default();
    let mut class = Pb::new();
    class.field_varint(1, 10); // flags: visibility LOCAL (5 << 1), final, kind CLASS
    let self_idx = st.local_class_id(internal);
    class.field_varint(3, self_idx as u64); // Class.fq_name = 3
    for &supertype in supertypes {
        let sup = type_pb(&mut st, supertype, &TypeParameters::new());
        class.field_message(6, &sup); // Class.supertype = 6
    }
    let stt = st.serialize_types();
    let mut bytes = vec![0x00u8]; // UTF8 mode marker
    let mut prefix = Pb::new();
    prefix.varint(stt.as_bytes().len() as u64); // writeDelimitedTo length prefix
    bytes.extend_from_slice(&prefix.into_bytes());
    bytes.extend_from_slice(stt.as_bytes());
    bytes.extend_from_slice(class.as_bytes());
    (bytes, st.into_strings())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn const_property_flags_preserve_visibility() {
        let flags = |visibility| {
            property_flags(&PropMeta {
                spellings: crate::spelling::DeclaredSpellings::default(),
                name: "x".into(),
                ty: Ty::Int,
                is_var: false,
                visibility,
                has_constant: true,
                is_const: true,
                is_abstract: false,
                has_backing_field: true,
                tparam: None,
                receiver: None,
                getter: None,
                setter: None,
                field_desc: None,
                field_name: None,
                annotations: Vec::new(),
                field_annotations: Vec::new(),
                synthetic_method: None,
            })
        };

        assert_eq!(flags(Visibility::Internal), 10752);
        assert_eq!(flags(Visibility::Private), 10754);
        assert_eq!(flags(Visibility::Protected), 10756);
        assert_eq!(flags(Visibility::Public), 10758);
    }

    // Ground truth: kotlinc 2.4.0 `package demo; class E` → @Metadata mv=[2,4,0] k=1 xi=48, and this
    // exact d1 protobuf (mUTF-8-decoded to raw bytes) + d2 string table. Drives byte-for-byte parity.
    #[test]
    fn empty_class_metadata_byte_matches_kotlinc() {
        let (d1, d2) = build_class("demo/E", &[], "()V", &[], &[], &[], &ClassTail::default());
        assert_eq!(
            d2,
            vec![
                "Ldemo/E;".to_string(),
                "".to_string(),
                "<init>".to_string(),
                "()V".to_string(),
            ],
            "d2 string table",
        );
        assert_eq!(
            d1,
            vec![
                0x00, 0x0c, 0x0a, 0x02, 0x18, 0x02, 0x0a, 0x02, 0x10, 0x00, 0x0a, 0x02, 0x08, 0x02,
                0x18, 0x00, 0x32, 0x02, 0x30, 0x01, 0x42, 0x07, 0xa2, 0x06, 0x04, 0x08, 0x02, 0x10,
                0x03,
            ],
            "d1 protobuf",
        );
    }

    // Ground truth: kotlinc 2.4.0 `package demo; class C(val x: Int)` — one ctor-param property.
    #[test]
    fn one_property_class_metadata_byte_matches_kotlinc() {
        let (d1, d2) = build_class(
            "demo/C",
            &[("x".into(), Ty::Int)],
            "(I)V",
            &[PropMeta {
                spellings: crate::spelling::DeclaredSpellings::default(),
                name: "x".into(),
                ty: Ty::Int,
                is_var: false,
                has_constant: false,
                is_const: false,
                visibility: Visibility::Public,
                is_abstract: false,
                has_backing_field: true,
                tparam: None,
                receiver: None,
                getter: Some(("getX".into(), "()I".into())),
                setter: None,
                field_desc: None,
                field_name: None,
                annotations: Vec::new(),
                field_annotations: Vec::new(),
                synthetic_method: None,
            }],
            &[],
            &[],
            &ClassTail::default(),
        );
        assert_eq!(
            d2,
            vec!["Ldemo/C;", "", "x", "", "<init>", "(I)V", "getX", "()I"]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>(),
            "d2 string table",
        );
        assert_eq!(
            d1,
            vec![
                0x00, 0x12, 0x0a, 0x02, 0x18, 0x02, 0x0a, 0x02, 0x10, 0x00, 0x0a, 0x00, 0x0a, 0x02,
                0x10, 0x08, 0x0a, 0x02, 0x08, 0x04, 0x18, 0x00, 0x32, 0x02, 0x30, 0x01, 0x42, 0x0f,
                0x12, 0x06, 0x10, 0x02, 0x1a, 0x02, 0x30, 0x03, 0xa2, 0x06, 0x04, 0x08, 0x04, 0x10,
                0x05, 0x52, 0x11, 0x10, 0x02, 0x1a, 0x02, 0x30, 0x03, 0xa2, 0x06, 0x08, 0x0a, 0x00,
                0x1a, 0x04, 0x08, 0x06, 0x10, 0x07,
            ],
            "d1 protobuf",
        );
    }

    // Ground truth: kotlinc 2.4.0 `package demo; data class Point(val x: Int, var y: String)` — the
    // full data-class shape (6 synthesized methods + IS_DATA flag + a var property).
    #[test]
    fn data_class_metadata_byte_matches_kotlinc() {
        let any_q = Ty::nullable(Ty::obj("kotlin/Any"));
        let methods = vec![
            FnMeta {
                spellings: crate::spelling::DeclaredSpellings::default(),
                name: "component1".into(),
                params: vec![],
                ret: Ty::Int,
                type_params: Vec::new(),
                semantic_type_params: Vec::new(),
                type_param_bounds: Vec::new(),
                flags: COMPONENT_FN_FLAGS,
                params_have_defaults: false,
                receiver: None,
                param_defaults: Vec::new(),
                vararg_index: None,
                jvm_sig: None,
                jvm_sig_name: None,
                annotations: Vec::new(),
            },
            FnMeta {
                spellings: crate::spelling::DeclaredSpellings::default(),
                name: "component2".into(),
                params: vec![],
                ret: Ty::String,
                type_params: Vec::new(),
                semantic_type_params: Vec::new(),
                type_param_bounds: Vec::new(),
                flags: COMPONENT_FN_FLAGS,
                params_have_defaults: false,
                receiver: None,
                param_defaults: Vec::new(),
                vararg_index: None,
                jvm_sig: None,
                jvm_sig_name: None,
                annotations: Vec::new(),
            },
            FnMeta {
                spellings: crate::spelling::DeclaredSpellings::default(),
                name: "copy".into(),
                params: vec![("x".into(), Ty::Int), ("y".into(), Ty::String)],
                ret: Ty::obj("demo/Point"),
                type_params: Vec::new(),
                semantic_type_params: Vec::new(),
                type_param_bounds: Vec::new(),
                flags: COPY_FN_FLAGS,
                params_have_defaults: true,
                receiver: None,
                param_defaults: Vec::new(),
                vararg_index: None,
                jvm_sig: None,
                jvm_sig_name: None,
                annotations: Vec::new(),
            },
            FnMeta {
                spellings: crate::spelling::DeclaredSpellings::default(),
                name: "equals".into(),
                params: vec![("other".into(), any_q)],
                ret: Ty::Boolean,
                type_params: Vec::new(),
                semantic_type_params: Vec::new(),
                type_param_bounds: Vec::new(),
                flags: EQUALS_FN_FLAGS,
                params_have_defaults: false,
                receiver: None,
                param_defaults: Vec::new(),
                vararg_index: None,
                jvm_sig: None,
                jvm_sig_name: None,
                annotations: Vec::new(),
            },
            FnMeta {
                spellings: crate::spelling::DeclaredSpellings::default(),
                name: "hashCode".into(),
                params: vec![],
                ret: Ty::Int,
                type_params: Vec::new(),
                semantic_type_params: Vec::new(),
                type_param_bounds: Vec::new(),
                flags: HASHCODE_TOSTRING_FN_FLAGS,
                params_have_defaults: false,
                receiver: None,
                param_defaults: Vec::new(),
                vararg_index: None,
                jvm_sig: None,
                jvm_sig_name: None,
                annotations: Vec::new(),
            },
            FnMeta {
                spellings: crate::spelling::DeclaredSpellings::default(),
                name: "toString".into(),
                params: vec![],
                ret: Ty::String,
                type_params: Vec::new(),
                semantic_type_params: Vec::new(),
                type_param_bounds: Vec::new(),
                flags: HASHCODE_TOSTRING_FN_FLAGS,
                params_have_defaults: false,
                receiver: None,
                param_defaults: Vec::new(),
                vararg_index: None,
                jvm_sig: None,
                jvm_sig_name: None,
                annotations: Vec::new(),
            },
        ];
        let props = vec![
            PropMeta {
                spellings: crate::spelling::DeclaredSpellings::default(),
                name: "x".into(),
                ty: Ty::Int,
                is_var: false,
                has_constant: false,
                is_const: false,
                visibility: Visibility::Public,
                is_abstract: false,
                has_backing_field: true,
                tparam: None,
                receiver: None,
                getter: Some(("getX".into(), "()I".into())),
                setter: None,
                field_desc: None,
                field_name: None,
                annotations: Vec::new(),
                field_annotations: Vec::new(),
                synthetic_method: None,
            },
            PropMeta {
                spellings: crate::spelling::DeclaredSpellings::default(),
                name: "y".into(),
                ty: Ty::String,
                is_var: true,
                has_constant: false,
                is_const: false,
                visibility: Visibility::Public,
                is_abstract: false,
                has_backing_field: true,
                tparam: None,
                receiver: None,
                getter: Some(("getY".into(), "()Ljava/lang/String;".into())),
                setter: Some(("setY".into(), "(Ljava/lang/String;)V".into())),
                field_desc: None,
                field_name: None,
                annotations: Vec::new(),
                field_annotations: Vec::new(),
                synthetic_method: None,
            },
        ];
        let (d1, _d2) = build_class(
            "demo/Point",
            &[("x".into(), Ty::Int), ("y".into(), Ty::String)],
            "(ILjava/lang/String;)V",
            &props,
            &methods,
            &[],
            &ClassTail {
                // public + final + IS_DATA, as `class_metadata_flags` derives for a `data class`.
                flags: 1030,
                ..Default::default()
            },
        );
        assert_eq!(
            d1,
            vec![
                0x00, 0x20, 0x0a, 0x02, 0x18, 0x02, 0x0a, 0x02, 0x10, 0x00, 0x0a, 0x00, 0x0a, 0x02,
                0x10, 0x08, 0x0a, 0x00, 0x0a, 0x02, 0x10, 0x0e, 0x0a, 0x02, 0x08, 0x0c, 0x0a, 0x02,
                0x10, 0x0b, 0x0a, 0x02, 0x08, 0x03, 0x08, 0x86, 0x08, 0x18, 0x00, 0x32, 0x02, 0x30,
                0x01, 0x42, 0x17, 0x12, 0x06, 0x10, 0x02, 0x1a, 0x02, 0x30, 0x03, 0x12, 0x06, 0x10,
                0x04, 0x1a, 0x02, 0x30, 0x05, 0xa2, 0x06, 0x04, 0x08, 0x06, 0x10, 0x07, 0x4a, 0x09,
                0x10, 0x0e, 0x1a, 0x02, 0x30, 0x03, 0x48, 0xc6, 0x03, 0x4a, 0x09, 0x10, 0x0f, 0x1a,
                0x02, 0x30, 0x05, 0x48, 0xc6, 0x03, 0x4a, 0x1d, 0x10, 0x10, 0x1a, 0x02, 0x30, 0x00,
                0x32, 0x08, 0x08, 0x02, 0x10, 0x02, 0x1a, 0x02, 0x30, 0x03, 0x32, 0x08, 0x08, 0x02,
                0x10, 0x04, 0x1a, 0x02, 0x30, 0x05, 0x48, 0xc6, 0x01, 0x4a, 0x14, 0x10, 0x11, 0x1a,
                0x02, 0x30, 0x12, 0x32, 0x08, 0x10, 0x13, 0x1a, 0x04, 0x18, 0x01, 0x30, 0x01, 0x48,
                0xd6, 0x83, 0x04, 0x4a, 0x0a, 0x10, 0x14, 0x1a, 0x02, 0x30, 0x03, 0x48, 0xd6, 0x81,
                0x04, 0x4a, 0x0a, 0x10, 0x15, 0x1a, 0x02, 0x30, 0x05, 0x48, 0xd6, 0x81, 0x04, 0x52,
                0x11, 0x10, 0x02, 0x1a, 0x02, 0x30, 0x03, 0xa2, 0x06, 0x08, 0x0a, 0x00, 0x1a, 0x04,
                0x08, 0x08, 0x10, 0x09, 0x52, 0x1a, 0x10, 0x04, 0x1a, 0x02, 0x30, 0x05, 0x58, 0x86,
                0x0e, 0xa2, 0x06, 0x0e, 0x0a, 0x00, 0x1a, 0x04, 0x08, 0x0a, 0x10, 0x0b, 0x22, 0x04,
                0x08, 0x0c, 0x10, 0x0d,
            ],
            "d1 protobuf",
        );
    }

    // A generic property (`List<String>`) + a defaulted ctor param — the shape real production domain
    // models use. Verified byte-identical to kotlinc 2.4.0 on a real config data class; this pins the
    // pieces it needs: `List` encoded as a `predefinedIndex` builtin (NOT a class-id descriptor), the
    // `Type.argument` (String), and the `DECLARES_DEFAULT_VALUE` ctor-param flag.
    #[test]
    fn generic_property_and_default_ctor_param() {
        let list_string = Ty::obj_args("kotlin/collections/List", &[Ty::String]);
        let (_d1, d2) = build_class(
            "demo/D",
            &[("r".into(), list_string)],
            "(Ljava/util/List;)V",
            &[PropMeta {
                spellings: crate::spelling::DeclaredSpellings::default(),
                name: "r".into(),
                ty: list_string,
                is_var: false,
                has_constant: false,
                is_const: false,
                visibility: Visibility::Public,
                is_abstract: false,
                has_backing_field: true,
                tparam: None,
                receiver: None,
                getter: Some(("getR".into(), "()Ljava/util/List;".into())),
                setter: None,
                field_desc: None,
                field_name: None,
                annotations: Vec::new(),
                field_annotations: Vec::new(),
                synthetic_method: None,
            }],
            &[],
            &[],
            &ClassTail {
                ctor_param_defaults: &[true],
                ..Default::default()
            },
        );
        // `List` is a builtin (predefinedIndex 32) → an EMPTY d2 slot, never the literal descriptor.
        assert!(
            !d2.iter()
                .any(|s| s == "Ljava/util/List;" || s == "Lkotlin/collections/List;"),
            "List must encode as a builtin predefinedIndex, not a class-id descriptor: {d2:?}",
        );
        // The ctor value parameter carries `DECLARES_DEFAULT_VALUE` (f1=2) — the `08 02` prefix inside
        // the constructor's value_parameter, before its name. Its absence would drop the flag.
        assert!(
            _d1.windows(2).any(|w| w == [0x08, 0x02]),
            "the defaulted ctor param must encode DECLARES_DEFAULT_VALUE",
        );
    }

    // Ground truth: kotlinc 2.4.0 `package demo; class S { fun f(n: Int): Int = n }` — a regular
    // (non-synthesized) member function. A plain public-final member has metadata flags omitted (0).
    #[test]
    fn regular_method_class_metadata_byte_matches_kotlinc() {
        let (d1, _d2) = build_class(
            "demo/S",
            &[],
            "()V",
            &[],
            &[FnMeta::plain(
                "f".into(),
                vec![("n".into(), Ty::Int)],
                Ty::Int,
            )],
            &[],
            &ClassTail::default(),
        );
        assert_eq!(
            d1,
            vec![
                0x00, 0x12, 0x0a, 0x02, 0x18, 0x02, 0x0a, 0x02, 0x10, 0x00, 0x0a, 0x02, 0x08, 0x03,
                0x0a, 0x02, 0x10, 0x08, 0x0a, 0x00, 0x18, 0x00, 0x32, 0x02, 0x30, 0x01, 0x42, 0x07,
                0xa2, 0x06, 0x04, 0x08, 0x02, 0x10, 0x03, 0x4a, 0x0e, 0x10, 0x04, 0x1a, 0x02, 0x30,
                0x05, 0x32, 0x06, 0x10, 0x06, 0x1a, 0x02, 0x30, 0x05,
            ],
            "d1 protobuf",
        );
    }

    // Ground truth: kotlinc 2.4.0 `package demo; class C { companion object }`. The companion object
    // adds `companionObjectName` (f4) + a `nestedClassName` (f7), both referencing `Companion`, interned
    // after the ctor.
    #[test]
    fn companion_object_metadata_byte_matches_kotlinc() {
        let (d1, d2) = build_class(
            "demo/C",
            &[],
            "()V",
            &[],
            &[],
            &[],
            &ClassTail {
                companion: Some("Companion"),
                nested: &["Companion"],
                ..Default::default()
            },
        );
        assert_eq!(
            d2,
            vec!["Ldemo/C;", "", "<init>", "()V", "Companion"]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>(),
            "d2",
        );
        assert_eq!(
            d1,
            vec![
                0x00, 0x0c, 0x0a, 0x02, 0x18, 0x02, 0x0a, 0x02, 0x10, 0x00, 0x0a, 0x02, 0x08, 0x03,
                0x18, 0x00, 0x20, 0x04, 0x32, 0x02, 0x30, 0x01, 0x3a, 0x01, 0x04, 0x42, 0x07, 0xa2,
                0x06, 0x04, 0x08, 0x02, 0x10, 0x03,
            ],
            "d1 protobuf",
        );
    }

    // Ground truth from kotlinc 2.4.10's `FirJvmSerializerExtension`: an interface compiled with
    // `-jvm-default=no-compatibility` references requirement-table slot 0 (Class f31), whose single
    // entry requires compiler 1.4.0 and has VersionKind.COMPILER_VERSION (Class f32). The JVM class
    // flags extension follows it in field order.
    #[test]
    fn no_compatibility_compiler_requirement_matches_kotlinc() {
        let (d1, _) = build_class(
            "demo/I",
            &[],
            "()V",
            &[],
            &[],
            &[],
            &ClassTail {
                flags: 102,
                emit_primary_ctor: false,
                jvm_class_flags: Some(1),
                compiler_version_requirement: Some((1, 4, 0)),
                ..Default::default()
            },
        );
        let requirement_and_flags = [
            0xf8, 0x01, 0x00, // Class.versionRequirement = table index 0
            0x82, 0x02, 0x06, 0x0a, 0x04, 0x08, 0x21, 0x30, 0x01, // table: compiler 1.4.0
            0xc0, 0x06, 0x01, // JvmProtoBuf.jvmClassFlags = 1
        ];
        assert!(
            d1.windows(requirement_and_flags.len())
                .any(|window| window == requirement_and_flags),
            "missing no-compatibility requirement: {d1:02x?}"
        );
    }

    // Ground truth: kotlinc 2.4.0 `class C(val x: Int)` compiled with `-module-name mymod`. Adds
    // `classModuleName` (f101) = the module name, interned last.
    #[test]
    fn module_name_metadata_byte_matches_kotlinc() {
        let (d1, d2) = build_class(
            "demo/C",
            &[("x".into(), Ty::Int)],
            "(I)V",
            &[PropMeta {
                spellings: crate::spelling::DeclaredSpellings::default(),
                name: "x".into(),
                ty: Ty::Int,
                is_var: false,
                has_constant: false,
                is_const: false,
                visibility: Visibility::Public,
                is_abstract: false,
                has_backing_field: true,
                tparam: None,
                receiver: None,
                getter: Some(("getX".into(), "()I".into())),
                setter: None,
                field_desc: None,
                field_name: None,
                annotations: Vec::new(),
                field_annotations: Vec::new(),
                synthetic_method: None,
            }],
            &[],
            &[],
            &ClassTail {
                module_name: Some("mymod"),
                ..Default::default()
            },
        );
        assert_eq!(
            d2,
            vec!["Ldemo/C;", "", "x", "", "<init>", "(I)V", "getX", "()I", "mymod"]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>(),
            "d2",
        );
        assert_eq!(
            d1,
            vec![
                0x00, 0x12, 0x0a, 0x02, 0x18, 0x02, 0x0a, 0x02, 0x10, 0x00, 0x0a, 0x00, 0x0a, 0x02,
                0x10, 0x08, 0x0a, 0x02, 0x08, 0x05, 0x18, 0x00, 0x32, 0x02, 0x30, 0x01, 0x42, 0x0f,
                0x12, 0x06, 0x10, 0x02, 0x1a, 0x02, 0x30, 0x03, 0xa2, 0x06, 0x04, 0x08, 0x04, 0x10,
                0x05, 0x52, 0x11, 0x10, 0x02, 0x1a, 0x02, 0x30, 0x03, 0xa2, 0x06, 0x08, 0x0a, 0x00,
                0x1a, 0x04, 0x08, 0x06, 0x10, 0x07, 0xa8, 0x06, 0x08,
            ],
            "d1 protobuf",
        );
    }

    // Ground truth: kotlinc 2.4.0 `class C(val x: Int) { constructor() : this(0) }` — a second
    // (secondary) constructor. `Class.constructor` (f8) is repeated; the secondary carries flags 22.
    #[test]
    fn secondary_ctor_metadata_byte_matches_kotlinc() {
        let (d1, d2) = build_class(
            "demo/C",
            &[("x".into(), Ty::Int)],
            "(I)V",
            &[PropMeta {
                spellings: crate::spelling::DeclaredSpellings::default(),
                name: "x".into(),
                ty: Ty::Int,
                is_var: false,
                has_constant: false,
                is_const: false,
                visibility: Visibility::Public,
                is_abstract: false,
                has_backing_field: true,
                tparam: None,
                receiver: None,
                getter: Some(("getX".into(), "()I".into())),
                setter: None,
                field_desc: None,
                field_name: None,
                annotations: Vec::new(),
                field_annotations: Vec::new(),
                synthetic_method: None,
            }],
            &[],
            &[],
            &ClassTail {
                secondary_ctors: &[CtorMeta {
                    params: &[],
                    desc: "()V",
                    vararg_index: None,
                    flags: 22,
                    annotations: &[],
                }],
                ..Default::default()
            },
        );
        assert_eq!(
            d2,
            vec!["Ldemo/C;", "", "x", "", "<init>", "(I)V", "()V", "getX", "()I"]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>(),
            "d2",
        );
        assert_eq!(
            d1,
            vec![
                0x00, 0x12, 0x0a, 0x02, 0x18, 0x02, 0x0a, 0x02, 0x10, 0x00, 0x0a, 0x00, 0x0a, 0x02,
                0x10, 0x08, 0x0a, 0x02, 0x08, 0x05, 0x18, 0x00, 0x32, 0x02, 0x30, 0x01, 0x42, 0x0f,
                0x12, 0x06, 0x10, 0x02, 0x1a, 0x02, 0x30, 0x03, 0xa2, 0x06, 0x04, 0x08, 0x04, 0x10,
                0x05, 0x42, 0x09, 0x08, 0x16, 0xa2, 0x06, 0x04, 0x08, 0x04, 0x10, 0x06, 0x52, 0x11,
                0x10, 0x02, 0x1a, 0x02, 0x30, 0x03, 0xa2, 0x06, 0x08, 0x0a, 0x00, 0x1a, 0x04, 0x08,
                0x07, 0x10, 0x08,
            ],
            "d1 protobuf",
        );
    }

    #[test]
    fn class_metadata_has_expected_strings() {
        let (_d1, d2) = build_class(
            "demo/Point",
            &[("x".into(), Ty::Int), ("y".into(), Ty::String)],
            "(ILjava/lang/String;)V",
            &[
                PropMeta {
                    spellings: crate::spelling::DeclaredSpellings::default(),
                    name: "x".into(),
                    ty: Ty::Int,
                    is_var: false,
                    has_constant: false,
                    is_const: false,
                    visibility: Visibility::Public,
                    is_abstract: false,
                    has_backing_field: true,
                    tparam: None,
                    receiver: None,
                    getter: Some(("getX".into(), "()I".into())),
                    setter: None,
                    field_desc: None,
                    field_name: None,
                    annotations: Vec::new(),
                    field_annotations: Vec::new(),
                    synthetic_method: None,
                },
                PropMeta {
                    spellings: crate::spelling::DeclaredSpellings::default(),
                    name: "y".into(),
                    ty: Ty::String,
                    is_var: true,
                    has_constant: false,
                    is_const: false,
                    visibility: Visibility::Public,
                    is_abstract: false,
                    has_backing_field: true,
                    tparam: None,
                    receiver: None,
                    getter: Some(("getY".into(), "()Ljava/lang/String;".into())),
                    setter: Some(("setY".into(), "(Ljava/lang/String;)V".into())),
                    field_desc: None,
                    field_name: None,
                    annotations: Vec::new(),
                    field_annotations: Vec::new(),
                    synthetic_method: None,
                },
            ],
            &[],
            &[],
            &ClassTail::default(),
        );
        // The class id descriptor and the JVM signatures must all appear verbatim in d2.
        assert!(d2.contains(&"Ldemo/Point;".to_string()));
        assert!(d2.contains(&"getX".to_string()));
        assert!(d2.contains(&"setY".to_string()));
        assert!(d2.contains(&"(ILjava/lang/String;)V".to_string()));
    }
}
