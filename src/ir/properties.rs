//! Property declarations and accessor contracts retained by common IR.

use super::*;

/// One inline property use. `accessor` is the declaration the checker selected; `substitutions`
/// already carry each type parameter's semantic name and reified flag.
#[derive(Clone, Debug, PartialEq)]
pub struct IrInlinePropertySplice {
    pub accessor: crate::fir::DeclarationId,
    pub substitutions: Vec<IrInlineTypeSubstitution>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct IrInlineTypeSubstitution {
    pub name: String,
    pub reified: bool,
    pub value: Ty,
}

/// Property-owned index used while checked inline accessors are materialized and spliced.
#[derive(Default)]
pub(crate) struct IrInlinePropertyAccess {
    pub(crate) accessor_functions: std::collections::HashMap<crate::fir::DeclarationId, FunId>,
    pub(crate) splices: std::collections::HashMap<ExprId, IrInlinePropertySplice>,
}

/// One source property declaration and the checked bodies that realize its language semantics.
/// Storage and accessor naming remain backend choices; stable property/class identities and final
/// types are already fixed here.
#[derive(Clone, Debug)]
pub struct IrCheckedProperty {
    pub declaration: crate::fir::DeclarationId,
    /// The declaration's stable source position, the key its accessors take in `fn_source_order`.
    pub source_order: u32,
    /// Source declaration line accepted while the bounded Pass-2 syntax unit is live. This is
    /// output metadata, not a source locator: property realization copies it to the common-IR
    /// declarations it creates after the syntax unit has already been dropped.
    pub decl_line: u32,
    /// For a primary-constructor property, the line its declaration starts on with its annotations
    /// included, else 0. Accepted like [`Self::decl_line`] and kept apart from it: the constructor's
    /// store of the property maps here, its accessors to `decl_line`.
    pub decl_start_line: u32,
    /// Lines of the getter and setter headers when each was written with a body (else 0), accepted
    /// like [`Self::decl_line`]. Such an accessor is a declaration of its own and anchors on its
    /// own line; any other accessor anchors on [`Self::decl_line`].
    pub getter_decl_line: u32,
    pub setter_decl_line: u32,
    /// Exact semantic position among the owning class's property initializers and `init` blocks.
    /// This is copied from the stable FIR declaration header, never reconstructed from source.
    pub initialization_order: Option<u32>,
    pub class: Option<ClassId>,
    pub name: String,
    pub ty: Ty,
    /// Checked explicit backing-field type, distinct from the public property/accessor type.
    pub storage_ty: Option<Ty>,
    pub visibility: crate::types::Visibility,
    pub flags: crate::fir::DeclarationFlags,
    pub initializer: Option<ExprId>,
    pub delegate: Option<ExprId>,
    pub delegate_plan: Option<crate::fir::FirPropertyDelegatePlan>,
    pub getter: Option<ExprId>,
    pub setter: Option<ExprId>,
    /// Accessor declarations and whether each was declared `inline`. Published with the property
    /// so later expansion does not rediscover the flag from the declaration index.
    pub getter_declaration: Option<crate::fir::DeclarationId>,
    pub setter_declaration: Option<crate::fir::DeclarationId>,
    pub getter_inline: bool,
    pub setter_inline: bool,
}

/// A property a class DECLARES. A property is a declaration, not a pair of methods: `val a: Int` is one
/// thing, and the `getA()` a target may emit for it is a realization of it. The front end lowers only
/// what is genuinely Kotlin — a source-written accessor's BODY — and leaves naming, descriptors and
/// dispatch to the backend.
/// One member EXTENSION property declaration ([`IrFile::member_ext_props`]): the SEMANTIC types
/// (checker-resolved, pre-erasure) plus the accessor realization, everything a class `Property`
/// metadata record needs.
#[derive(Clone, Debug)]
pub struct MemberExtProp {
    pub name: String,
    /// Declared extension receiver (`Int` in `val Int.doubled`).
    pub receiver: crate::types::Ty,
    /// Declared property type.
    pub ty: crate::types::Ty,
    pub is_var: bool,
    /// Whether this declaration has no accessor implementation and must be realized abstractly.
    pub is_abstract: bool,
    pub modifiers: IrPropertyModifiers,
    /// See [`IrProperty::delegate_field`].
    pub delegate_field: Option<u32>,
    /// Getter function id. Abstract properties point at a bodyless common-IR function.
    pub getter: u32,
    /// Setter function id, for a `var`.
    pub setter: Option<u32>,
    pub visibility: crate::types::Visibility,
    /// Declaration-owned generic parameters. Source names are metadata payload; semantic names are
    /// the stable identities used by `receiver` and `ty`.
    pub type_params: Vec<IrTypeParameter>,
}

/// How a member property is declared, as Kotlin metadata records it; no JVM shape encodes these.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct IrPropertyModifiers {
    pub modality: IrPropertyModality,
    /// Source declares the getter, or the property is delegated; an accessor the compiler
    /// supplies on its own is the default one.
    pub declared_getter: bool,
    /// Source wrote the setter's body, or the property is a delegated `var`. A bodiless `set`
    /// keeps the default implementation; whether it narrows visibility is recorded separately.
    pub declared_setter: bool,
    pub delegated: bool,
    pub lateinit: bool,
    pub member_kind: IrMemberKind,
}

/// How a class member came to be, as Kotlin metadata's `MEMBER_KIND` records it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum IrMemberKind {
    #[default]
    Declaration,
    /// A forwarder to an interface delegate (`class C(d: I) : I by d`).
    Delegation,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum IrPropertyModality {
    #[default]
    Final,
    /// `open`, an `override` not marked `final`, or an interface member with a getter.
    Open,
    /// `abstract`, or an interface member without a getter.
    Abstract,
}

#[derive(Clone, Debug)]
pub struct IrProperty {
    pub name: String,
    /// Named context parameters in source order. Metadata records these separately from ordinary
    /// value parameters, and checked call sites supply their operands implicitly.
    pub context_params: Vec<(String, crate::types::ContextParameterKind, Ty)>,
    /// Source byte offset and 1-based declaration line. These remain attached to the declaration so
    /// a backend can order/debug synthesized accessors without rebinding the property by spelling.
    pub source_order: u32,
    pub decl_line: u32,
    /// The property's language-level type. This is also the type exposed by its accessors.
    pub ty: Ty,
    /// Type parameters this property declares (`var <X, Y> ctx: Map<X, Y>`). They are not the
    /// enclosing class's parameters. Metadata and each accessor's generic signature publish them.
    pub type_params: Vec<IrTypeParameter>,
    /// Kotlin declaration visibility. Backends consume this semantic fact when choosing the
    /// visibility of an accessor or a target-specific storage realization; they must not recover it
    /// from a rendered owner/property-name key.
    pub visibility: crate::types::Visibility,
    /// Checked Kotlin return-value status, inherited from the property this one overrides.
    pub return_value_status: crate::types::ReturnValueStatus,
    /// Resolved Kotlin annotation identities. Backends interpret annotations in their own namespace;
    /// common lowering does not turn them into storage or calling-convention choices.
    pub annotations: Box<[TypeName]>,
    /// The declaration initializer after common lowering, before any backend chooses storage.
    /// `None` means the declaration has no initializer (or its source shape is not represented),
    /// which is distinct from an explicit nullable initializer lowered to `IrConst::Null`.
    /// Keeping this on the declaration lets a backend relocate storage without re-reading the AST
    /// or mistaking a later assignment in an `init` block for the declaration initializer.
    pub initializer: Option<ExprId>,
    /// The declared type of an explicit backing field when it differs from the property's public type.
    /// The JVM value-class pass may erase the physical [`IrField`] to its carrier, so retaining this
    /// semantic storage boundary lets the backend box/unbox at the accessor without resolving anything.
    pub storage_ty: Option<Ty>,
    /// Index into [`IrClass::fields`] for the backing field, `None` for a computed/delegated property
    /// (which stores nothing).
    pub backing_field: Option<u32>,
    pub is_var: bool,
    /// Non-final: the accessor a backend emits for it must be overridable.
    pub is_open: bool,
    pub modifiers: IrPropertyModifiers,
    /// Index into [`IrClass::fields`] of a delegated property's `x$delegate` field, which Kotlin
    /// metadata names as the property's JVM field.
    pub delegate_field: Option<u32>,
    /// A `private` property. kotlinc emits NO accessor for one — in-class reads go straight to the
    /// backing field — so a use from outside the declaring class has nothing to call, and whichever
    /// path is lowering it does not own the access.
    pub is_private: bool,
    /// The resolved visibility of a `var`'s setter: its own modifier (`private set`, `protected set`),
    /// else the property's. This is declaration visibility, not a JVM flag: every backend must
    /// preserve it when realizing a default setter or the storage it guards.
    pub setter_visibility: crate::types::Visibility,
    /// The lowered body of a source-written getter/setter (a computed, `field`-using, or delegated
    /// property). `None` for a plain backing-field property, whose accessor has no source body at all.
    pub getter: Option<FunId>,
    pub setter: Option<FunId>,
    /// The JVM name a backend must use for the synthesized accessor when the plain spelling is wrong —
    /// a value-class-typed property's accessor is `@JvmName`-mangled. Stamped by the pass that knows the
    /// value classes; `None` means the ordinary spelling applies.
    pub getter_jvm_name: Option<String>,
    pub setter_jvm_name: Option<String>,
    /// Some use of this PRIVATE property reaches it from outside the declaring class — an `inline`
    /// function's body, spliced into its caller. The declaring class must then expose a synthetic
    /// accessor for it (`access$get<X>$p` on the JVM); without one the splice would be illegal, and
    /// silently degrading the `inline` call instead would change what the program does.
    pub needs_access_bridge: bool,
    /// Annotations written on this property's accessors, whether source declared each accessor or
    /// a backend realizes its default.
    pub accessor_annotations: super::AccessorAnnotations,
}

/// A property declared by another file of the module, as the file's code selects it.
#[derive(Clone, Debug, PartialEq)]
pub struct IrModuleProperty {
    pub source: IrModuleSource,
    pub name: String,
    pub ty: Ty,
    pub context_parameters: Vec<Ty>,
    pub extension_receiver: Option<Ty>,
    pub mutable: bool,
    pub owner: Option<TypeName>,
    /// Source-level kind of `owner`. Common IR retains the Kotlin declaration fact; a target backend
    /// decides whether that kind uses interface dispatch, singleton storage, or another physical form.
    pub owner_kind: Option<IrClassifierKind>,
    pub companion_associated: bool,
    /// Outer classifier whose companion object owns this declaration. This is the Kotlin
    /// singleton-association edge; it says nothing about target storage.
    pub companion_owner: Option<TypeName>,
    pub visibility: crate::types::Visibility,
    pub setter_visibility: crate::types::Visibility,
    /// The setter's value parameter, as the declaration names it; `None` for a `val`.
    pub setter_parameter: Option<crate::fir::ResolvedParameterIdentity>,
    /// Resolved Kotlin annotation identities. A target backend may interpret annotations in its
    /// namespace; common lowering never turns one into a physical access kind.
    pub annotations: Box<[TypeName]>,
    pub flags: crate::fir::DeclarationFlags,
    /// Provider-normalized payload of a `const val`. Uses consume the value without reopening the
    /// declaration or selecting target storage by owner/spelling.
    pub compile_time_constant: Option<IrConst>,
    /// Where the property lives when `owner` is absent.
    pub placement: IrStaticPlacement,
    /// The own, unapplied types of the properties this one overrides, as the override edges its
    /// classifier published them (see [`super::IrModuleCallable::overridden_results`]).
    pub overridden_types: Box<[Ty]>,
}
