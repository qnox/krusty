//! What a class's metadata record is built from.

use crate::metadata::version_requirements::VersionRequirement;
use crate::types::{Ty, TypeName, Visibility};

/// A property's declaration in class metadata. How a target realizes it (the JVM's accessors,
/// field and annotation marker) is the target's own record.
pub struct PropMeta {
    pub name: String,
    pub ty: Ty,
    /// Named context parameters, emitted as `Property.context_parameter` (field 17).
    pub context_params: Vec<(String, crate::types::ContextParameterKind, Ty)>,
    /// How SOURCE spelled the declared type and receiver — see [`FnMeta::spellings`].
    pub spellings: crate::spelling::DeclaredSpellings,
    pub is_var: bool,
    pub visibility: Visibility,
    /// The property has a compile-time constant initializer.
    pub has_constant: bool,
    /// Whether the property is declared `const`.
    pub is_const: bool,
    /// Modality, declared accessors, delegation and `lateinit`, recorded in `Property.flags` and
    /// the accessor flag words.
    pub modifiers: crate::ir::IrPropertyModifiers,
    /// The setter's resolved visibility: its own modifier, else the property's.
    pub setter_visibility: Visibility,
    /// Kotlin return-value status, recorded in `Property.flags` bits 17-18.
    pub return_value_status: crate::types::ReturnValueStatus,
    /// Index of the class type parameter this property is declared as (`class C<T>(val a: T)` → 0).
    /// `None` for an ordinary type.
    pub tparam: Option<u32>,
    /// Extension-receiver type (`Property.receiver_type` = f5) for a MEMBER EXTENSION property
    /// (`object Tools { val Int.doubled get() }`). Its presence marks the record an extension —
    /// without it a consumer sees an ordinary member property that does not exist. `None` for an
    /// ordinary member.
    pub receiver: Option<Ty>,
    /// Type parameters this property declares, whether it is an ordinary member or a member
    /// extension (`var <X, Y> ctx`, `val <T> T.id`).
    pub type_params: Vec<crate::ir::IrTypeParameter>,
    /// Exact source identity of an explicitly named custom setter parameter. An implicit setter
    /// has no source parameter declaration and therefore omits `Property.setter_value_parameter`.
    pub setter_parameter_name: Option<String>,
    /// Annotations that landed on the PROPERTY (`Property.annotation` = f14).
    pub annotations: crate::metadata::MetadataAnnotations,
    /// Annotations that landed on the BACKING FIELD (`@Target(FIELD)`) — recorded separately (f34),
    /// because the reader must not attribute a field annotation to the property.
    pub field_annotations: crate::metadata::MetadataAnnotations,
    /// The accessors' own annotations (`@A get`, `@A set`, `set(@A v)`): each accessor word's
    /// `HAS_ANNOTATIONS` bit and the `getter_annotation`/`setter_annotation` records (f15/f16).
    pub accessor_annotations: crate::metadata::AccessorMetadataAnnotations,
    /// A `companion { … }` block property: a static member of this class (`Property.flags` bit 19).
    pub companion: bool,
}

/// A member function's declaration in class metadata (`Class.function` = f9).
pub struct FnMeta {
    pub name: String,
    pub params: Vec<(String, Ty)>,
    /// Number of LEADING entries in `params` that are context parameters. They ride
    /// `Function.context_parameter` (field 13) instead of `Function.value_parameter` (field 6), so a
    /// consuming compiler fills them from the enclosing context rather than demanding them
    /// positionally. `0` for an ordinary member.
    pub context_count: usize,
    /// Exact source role of each leading context entry in `params`.
    pub context_parameter_kinds: Vec<crate::types::ContextParameterKind>,
    pub ret: Ty,
    /// Extension-receiver type (`Function.receiver_type` = f5) for a MEMBER EXTENSION
    /// (`class C { operator fun String.invoke(…) }`) — recorded separately from `params` (the
    /// LOGICAL value parameters, receiver excluded). `None` for an ordinary member.
    pub receiver: Option<Ty>,
    /// Function-owned type parameters in order `(name, reified)`. Class parameters are inherited
    /// from the enclosing class table; these are emitted on the function with ids following that
    /// inherited prefix.
    pub type_params: Vec<(String, bool)>,
    /// Semantic identities parallel to `type_params`.
    pub semantic_type_params: Vec<String>,
    pub type_param_bounds: Vec<Vec<Ty>>,
    /// `Function.flags` (f9): e.g. operator (`componentN`) or the data-class `copy`. 0 ⇒ omitted.
    pub flags: u64,
    /// The checker's fact that a value parameter (context parameters excluded) or the extension
    /// receiver has a function type. `false` for a synthesized member.
    pub has_function_typed_parameter: bool,
    /// Mark every value parameter `DECLARES_DEFAULT_VALUE` (so a Kotlin caller may omit it) — used
    /// for the synthesized `copy`.
    pub params_have_defaults: bool,
    /// What each parameter of a DECLARED member wrote (`fun f(a: Int, b: Int = 2)`), parallel to
    /// `params` (empty = nothing). Composes with `params_have_defaults`.
    pub param_modifiers: Vec<crate::metadata::DeclaredValueParameter>,
    /// Index into `params` of a `vararg` parameter — emits `ValueParameter.vararg_element_type`
    /// (f4), the only place vararg-ness survives into metadata.
    pub vararg_index: Option<usize>,
    /// How SOURCE spelled this member's declared types, so a `typealias` becomes
    /// `Type.abbreviated_type` (see [`crate::spelling`]). Default for a SYNTHESIZED member
    /// (`componentN`, `copy`, `equals`), which has no source spelling at all.
    pub spellings: crate::spelling::DeclaredSpellings,
    /// BINARY/RUNTIME-retained annotations applied to the member, with their frontend-checked element
    /// values — emitted as `Function.annotation` (f12) and setting the `HAS_ANNOTATIONS` flag bit. The
    /// class file's `Runtime[In]VisibleAnnotations` attribute makes the annotation work at RUNTIME;
    /// this record is what a KOTLIN consumer (and `kotlin-reflect`) reads the declaration's
    /// annotations back from. SOURCE-retained annotations never enter this list.
    pub annotations: crate::metadata::MetadataAnnotations,
    /// User annotations on each value parameter (`fun f(@Mark a: Int)`), parallel to `params`. Empty
    /// (or a short list) ⇒ the missing parameters carry none. Recorded regardless of retention:
    /// `@Metadata` is the Kotlin-level record, so a BINARY-retained annotation appears here too.
    pub param_annotations: Vec<crate::metadata::MetadataAnnotations>,
    /// Kotlin type-use inference policy, parallel to `params`.
    pub no_infer_params: Vec<bool>,
    /// The member's declared `contract { … }` (`Function.contract` = 32).
    pub contract: Option<std::sync::Arc<crate::contracts::Contract>>,
    /// Whether the member has a source declaration, which a KLIB names the file of. A data class's
    /// generated `equals`/`hashCode`/`toString` have none; its `componentN` and `copy` take the
    /// constructor's.
    pub has_source: bool,
}

impl FnMeta {
    /// A plain member function (public, final, a declaration) — the common case, whose flags are
    /// kotlinc's default and therefore omitted from the proto.
    pub fn plain(name: String, params: Vec<(String, Ty)>, ret: Ty) -> FnMeta {
        FnMeta {
            context_count: 0,
            context_parameter_kinds: Vec::new(),
            name,
            params,
            ret,
            type_params: Vec::new(),
            semantic_type_params: Vec::new(),
            type_param_bounds: Vec::new(),
            flags: DEFAULT_FUNCTION_FLAGS,
            has_function_typed_parameter: false,
            params_have_defaults: false,
            receiver: None,
            param_modifiers: Vec::new(),
            vararg_index: None,
            spellings: crate::spelling::DeclaredSpellings::default(),
            annotations: Default::default(),
            param_annotations: Vec::new(),
            no_infer_params: Vec::new(),
            contract: None,
            has_source: true,
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
pub(super) const PUBLIC_CTOR_FLAGS: u64 = 6;
/// Bit 0 of both `Constructor.flags` and `ValueParameter.flags` — the declaration (or the parameter)
/// carries annotations. kotlinc keeps it set when the record-emission source feature is disabled;
/// a reader trusts it.
pub(crate) const HAS_ANNOTATIONS: u64 = 1;
/// `ValueParameter.flags` bit for `DECLARES_DEFAULT_VALUE`.
pub(super) const DECLARES_DEFAULT_VALUE: u64 = 2;
/// A secondary constructor for class metadata (`Class.constructor` = f8, repeated after the primary).
pub struct CtorMeta<'a> {
    pub params: &'a [(String, Ty)],
    /// Source spellings parallel to `params`; metadata records aliases from this declaration fact.
    pub param_spellings: &'a [crate::spelling::Spelled],
    /// Per-parameter `DECLARES_DEFAULT_VALUE` flags in the same source order as `params`.
    pub param_defaults: &'a [bool],
    /// Index into `params` of a `vararg` parameter — emits `ValueParameter.vararg_element_type`
    /// (f4), the record a consumer needs to admit `C(a, b, c)` against `vararg` (without it the
    /// parameter reads as a plain array and the call resolves to nothing).
    pub vararg_index: Option<usize>,
    /// `Constructor.flags` (f8's f1) — e.g. 22 for a plain secondary ctor. 0 ⇒ omitted (the primary).
    pub flags: u64,
    /// BINARY/RUNTIME-retained annotations applied to the constructor — `Constructor.annotation`
    /// (f3), the constructor analogue of [`FnMeta::annotations`].
    pub annotations: &'a crate::metadata::MetadataAnnotations,
}

/// Source declaration order across the protobuf's separately stored function/property lists. The
/// message fields remain grouped by field number, but kotlinc interns their shared string table in
/// source order.
#[derive(Clone, Copy)]
pub enum ClassMemberOrder {
    Property(usize),
    Function(usize),
    TypeAlias(usize),
    EnumEntry(usize),
}

/// The type parameters a class captures from enclosing declarations, and how kotlinc numbers them.
#[derive(Clone, Copy, Debug)]
pub enum CapturedTypeParameters<'a> {
    /// A nested class's: every enclosing class's parameters, outermost first, hold the ids before
    /// its own. An inner class addresses the ones it captures by those ids.
    Reserved(&'a [String]),
    /// A local or anonymous class's: its own parameters come first, and each captured one takes
    /// the next id on first use (see `TypeParameters`).
    NumberedOnUse(&'a [String]),
}

impl Default for CapturedTypeParameters<'_> {
    fn default() -> Self {
        Self::Reserved(&[])
    }
}

pub struct ClassTail<'a> {
    /// Checked approximation of a non-denotable intersection at the metadata boundary. The
    /// serializer has no declaration source and must never guess a classifier's variance.
    pub intersection_approximation: Option<&'a dyn Fn(Ty) -> Option<Ty>>,
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
    /// Type aliases declared directly in this classifier. They share source-order string interning
    /// with functions and properties, then serialize as `Class.type_alias` (field 11).
    pub type_aliases: &'a [crate::metadata::builder::TypeAliasMeta],
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
    /// Per-primary-constructor-parameter user annotations (`class C(@Mark val x: Int)`), parallel to
    /// `ctor_params`. Empty ⇒ no parameter carries one.
    pub ctor_param_annotations: &'a [crate::metadata::MetadataAnnotations],
    /// A `@JvmInline value class`'s sole underlying property `(name, type)` → `Class`
    /// `inlineClassUnderlyingPropertyName` (f17, the name's string-table id) +
    /// `inlineClassUnderlyingType` (f18, an inline `Type`). kotlinc records the type only when the
    /// property is not public API, since a reader otherwise finds it on the property itself, so
    /// the type is `None` for a public or protected property. `None` for an ordinary class.
    pub inline_underlying: Option<(&'a str, Option<Ty>)>,
    /// A compiler-version requirement attached to the class. `-jvm-default=no-compatibility`
    /// requires compiler 1.4.0 so older consumers do not interpret the interface under the legacy
    /// `$DefaultImpls` rules.
    pub compiler_version_requirement: Option<VersionRequirement>,
    /// Whether parameter null checks are generated (`-Xno-param-assertions` clears it). An inline
    /// member with a functional parameter then requires compiler 1.3.50.
    pub param_assertions: bool,
    /// Index of a `vararg` PRIMARY-ctor parameter (into `ctor_params`), for its
    /// `vararg_element_type` record. `None` ⇒ no vararg parameter.
    pub ctor_vararg_index: Option<usize>,
    /// Whether the class HAS a primary constructor at all — an `interface` has none, so `Class` carries
    /// no `constructor` (f8) entry. Defaults to true (every other kind).
    pub emit_primary_ctor: bool,
    /// `Constructor.flags` (f1) for the PRIMARY constructor — 0 (omitted) for an ordinary class; a
    /// sealed class's primary ctor is PROTECTED, which kotlinc records.
    pub primary_ctor_flags: u64,
    /// Declared type-parameter names in order (`class C<T>` → `["T"]`), recorded as
    /// `Class.typeParameter` so the metadata describes the class as generic.
    pub type_params: &'a [String],
    pub type_param_bounds: &'a [crate::ir::IrTypeParameter],
    /// Enclosing declaration parameters referenced by this class's members. Kotlin metadata does
    /// not repeat their declarations.
    pub captured_type_params: CapturedTypeParameters<'a>,
    /// Resolved identities of direct sealed subtypes.
    pub sealed_subclasses: &'a [TypeName],
    /// Declared semantic supertypes, including applied type arguments. Physical erasure belongs to
    /// the classfile `super_class`/`interfaces` entries; Kotlin metadata must retain `H<A>` so
    /// reflection and downstream type substitution do not see a raw `H`.
    pub supertypes: &'a [Ty],
    /// BINARY/RUNTIME-retained annotations attached to the class declaration.
    pub annotations: &'a crate::metadata::MetadataAnnotations,
    /// BINARY/RUNTIME-retained annotations declared on the PRIMARY constructor — `Constructor.annotation`
    /// (f3) of the primary record, the counterpart of [`CtorMeta::annotations`] for the secondaries.
    pub primary_ctor_annotations: &'a crate::metadata::MetadataAnnotations,
    /// The file's local classifiers (declared in executable code or nested in one). The string
    /// table names each by its raw internal name, marked local, wherever it appears.
    pub local_classifiers: &'a std::collections::HashSet<TypeName>,
    /// The local classifiers whose ids keep their `pkg/Outer.Inner` spelling (enum entry bodies),
    /// together with the classes nested in them.
    pub enum_entry_bodies: &'a std::collections::HashSet<TypeName>,
    /// Whether the class is an `enum class`, whose implicit `Enum<E>` supertype metadata omits.
    pub is_enum: bool,
    /// kotlinc's `LanguageFeature.AnnotationsInMetadata` (since 2.4): `false` keeps every
    /// `HAS_ANNOTATIONS` flag bit but writes none of the annotation RECORDS (`Class.annotation`,
    /// `Constructor.annotation`, `Property.annotation`, `Function.annotation`, the value-parameter
    /// and enum-entry records). Selected from the finalized source-language feature set.
    pub annotations_in_metadata: bool,
}

static NO_LOCAL_CLASSIFIERS: std::sync::LazyLock<std::collections::HashSet<TypeName>> =
    std::sync::LazyLock::new(Default::default);

impl Default for ClassTail<'_> {
    fn default() -> Self {
        ClassTail {
            intersection_approximation: None,
            supertype_spellings: &[],
            spellings: crate::spelling::DeclaredSpellings::default(),
            flags: DEFAULT_CLASS_FLAGS,
            companion: None,
            nested: &[],
            member_order: &[],
            type_aliases: &[],
            secondary_ctors: &[],
            ctor_param_defaults: &[],
            ctor_param_tparams: &[],
            ctor_param_annotations: &[],
            inline_underlying: None,
            compiler_version_requirement: None,
            param_assertions: true,
            ctor_vararg_index: None,
            emit_primary_ctor: true,
            primary_ctor_flags: 0,
            type_params: &[],
            type_param_bounds: &[],
            captured_type_params: CapturedTypeParameters::default(),
            sealed_subclasses: &[],
            supertypes: &[],
            annotations: &crate::metadata::NO_ANNOTATIONS,
            primary_ctor_annotations: &crate::metadata::NO_ANNOTATIONS,
            local_classifiers: &NO_LOCAL_CLASSIFIERS,
            enum_entry_bodies: &NO_LOCAL_CLASSIFIERS,
            is_enum: false,
            annotations_in_metadata: true,
        }
    }
}

/// One `enum` constant as `@Metadata` records it: its name and the annotations applied to it.
///
/// An enum constant has no property of its own, so its annotations live on the entry itself —
/// `@SerialName("active") ACTIVE` is `EnumEntry { name, annotation }`, and dropping the annotation
/// leaves metadata that describes a differently-named constant to every reflective reader.
pub struct EnumEntryMeta<'a> {
    pub name: &'a str,
    pub annotations: crate::metadata::MetadataAnnotations,
}
