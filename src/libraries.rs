//! Library metadata shared by symbol sources.

pub use crate::types::Visibility;
use crate::types::{Ty, TypeName, TypeNameList};
use std::borrow::Cow;
use std::collections::HashMap;

/// A source default value captured without an AST/file identity. Providers attach these directly to
/// callable metadata so any selected source callable can be lowered without looking its declaration
/// up again. Unrepresentable defaults remain `None` and therefore cannot be silently miscompiled.
#[derive(Clone, Debug, PartialEq)]
pub enum DefaultValue {
    Int(i64),
    Long(i64),
    Double(f64),
    Float(f32),
    Bool(bool),
    Char(u16),
    Str(crate::kt_string::KtString),
    Null,
    Object(String),
}

impl DefaultValue {
    pub fn fills_param_ty(&self, ty: Ty) -> bool {
        match self {
            Self::Int(_) => ty.int_arithmetic_repr() == Ty::Int,
            Self::Long(_) => ty == Ty::Long,
            Self::Double(_) => ty == Ty::Double,
            Self::Float(_) => ty == Ty::Float,
            Self::Bool(_) => ty == Ty::Boolean,
            Self::Char(_) => ty == Ty::Char,
            Self::Str(_) => ty == Ty::String,
            Self::Null => ty.is_reference(),
            Self::Object(_) => false,
        }
    }
}

/// Declaration-level classifier access. Unlike [`Visibility`], this preserves JVM package-private
/// metadata; accessibility itself is computed by the resolver from this fact and its use context.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClassifierAccess {
    Public,
    Internal,
    Protected,
    Private,
    PackagePrivate,
}

/// Inheritance capabilities declared by one classifier. This is part of the classifier record, not a
/// second provider query; core consumes it while walking the class model.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClassifierInheritance {
    pub is_abstract: bool,
    pub is_extensible: bool,
    pub has_no_arg_constructor: bool,
}

impl Default for ClassifierInheritance {
    fn default() -> Self {
        Self {
            is_abstract: false,
            is_extensible: false,
            has_no_arg_constructor: true,
        }
    }
}

impl From<Visibility> for ClassifierAccess {
    fn from(visibility: Visibility) -> Self {
        match visibility {
            Visibility::Public => Self::Public,
            Visibility::Internal => Self::Internal,
            Visibility::Protected => Self::Protected,
            Visibility::Private => Self::Private,
        }
    }
}

impl ClassifierAccess {
    pub fn visibility(self) -> Visibility {
        match self {
            Self::Public => Visibility::Public,
            Self::Internal => Visibility::Internal,
            Self::Protected => Visibility::Protected,
            Self::Private | Self::PackagePrivate => Visibility::Private,
        }
    }
}

/// A parsed generic signature in Kotlin's logical shape: formal type-parameter names, an OPTIONAL
/// receiver, the value parameters, and the return. Every node is a plain [`Ty`] — a type variable is a
/// [`Ty::TyParam`] (name + `kotlin/Any` bound), a generic class carries its arguments in [`Ty::Obj`], a
/// function type is [`Ty::Fun`]. A backend parses its own signature format straight into `Ty`; call
/// resolution unifies and substitutes over it with [`crate::symbol_resolver::unify_ty`] /
/// [`crate::symbol_resolver::ty_subst`] without knowing which backend produced it. The receiver is an
/// ATTRIBUTE — never a value parameter — because at resolve/check level a member `A.foo(b): C` and an
/// extension `fun A.foo(b): C` are the same shape (receiver `A`, one param `b`, return `C`); that an
/// extension emits the receiver as a leading JVM argument, and a `suspend` fun emits a trailing
/// `Continuation`, are EMIT concerns the backend adds — they are absent here. `params` therefore holds
/// only the source value parameters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenericSig {
    pub formals: Vec<String>,
    /// Declared upper bounds, parallel to [`Self::formals`].
    pub formal_bounds: Vec<Vec<Ty>>,
    /// The dispatch/extension receiver's type (member self-type or extension receiver), if any.
    pub receiver: Option<Ty>,
    pub params: Vec<Ty>,
    pub ret: Ty,
    /// How the declaration asks consumers to realize a call-site substitution of [`Self::ret`].
    /// This records a semantic property of the signature rather than the provider or file format that
    /// supplied it: an unannotated reference return may remain null-capable after its outer method type
    /// parameter specializes to a source primitive. Exact source signatures use the default policy.
    pub return_policy: GenericReturnPolicy,
}

/// Post-substitution policy for a generic callable's return. Keeping this on [`GenericSig`] gives member,
/// static, and top-level resolution one authoritative fact; consumers do not need parallel provenance
/// flags or branches for a particular class-file/module provider.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GenericReturnPolicy {
    /// The substituted source type is the complete semantic result.
    #[default]
    Exact,
    /// An unannotated reference contract remains null-capable when its outer method type parameter binds
    /// to a primitive. The primitive is therefore carried in the platform's boxed reference form.
    FlexibleReference,
}

impl GenericSig {
    /// Apply the declaration's return policy after ordinary generic binding. Only a primitive needs a
    /// representation change: reference substitutions are already null-capable, while nested occurrences
    /// such as `Container<T>` keep their outer reference shape. The platform supplies wrapper identity so
    /// this shared model never names a target runtime class.
    pub fn apply_return_policy(&self, platform: &dyn SemanticPlatform, specialized: Ty) -> Ty {
        match self.return_policy {
            GenericReturnPolicy::Exact => specialized,
            GenericReturnPolicy::FlexibleReference => specialized
                .boxed_ref()
                .map(|boxed| platform.library_value_form(boxed))
                .unwrap_or(specialized),
        }
    }
}

pub use crate::types::{TypeParameterBounds, TypeParameterView, TypeParameters, TypeVariance};

/// Bit-packed boolean flags for a [`LibraryMember`], collapsing `ret_nullable`/`is_interface`/
/// `suspend`/`is_operator`/`is_extension`/`is_abstract` into one byte. Read through the `LibraryMember` accessors of the same
/// names; mutated through the matching `set_*` methods; built with the `with_*` chain. Headroom for
/// name; mutated through the matching `set_*` methods.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LmFlags(u8);

impl LmFlags {
    const RET_NULLABLE: u8 = 1 << 0;
    const IS_INTERFACE: u8 = 1 << 1;
    const SUSPEND: u8 = 1 << 2;
    /// The member is declared `operator`. Only `@Metadata` records it — the JVM has no such flag — and
    /// only a convention call site (`"x" { … }` for `operator fun String.invoke`) needs it, so it
    /// travels with the member rather than being re-derived from a name.
    const IS_OPERATOR: u8 = 1 << 3;
    /// The member is a member EXTENSION (`class DslScope { fun String.f() }`): its declaring class is
    /// the dispatch receiver and its FIRST JVM parameter is the extension receiver. Nothing in the
    /// descriptor distinguishes that from an ordinary member taking a parameter of the same type, and
    /// a call site must know which, since only a member extension needs its dispatch receiver in scope.
    const IS_EXTENSION: u8 = 1 << 4;
    const IS_ABSTRACT: u8 = 1 << 5;
    const IS_INFIX: u8 = 1 << 6;

    #[inline]
    const fn with(mut self, mask: u8, on: bool) -> Self {
        if on {
            self.0 |= mask;
        } else {
            self.0 &= !mask;
        }
        self
    }
    #[inline]
    const fn has(self, mask: u8) -> bool {
        self.0 & mask != 0
    }

    #[inline]
    pub const fn with_ret_nullable(self, on: bool) -> Self {
        self.with(Self::RET_NULLABLE, on)
    }
    #[inline]
    pub const fn with_is_interface(self, on: bool) -> Self {
        self.with(Self::IS_INTERFACE, on)
    }
    #[inline]
    pub const fn with_suspend(self, on: bool) -> Self {
        self.with(Self::SUSPEND, on)
    }
    #[inline]
    pub const fn with_is_operator(self, on: bool) -> Self {
        self.with(Self::IS_OPERATOR, on)
    }
    #[inline]
    pub const fn with_is_extension(self, on: bool) -> Self {
        self.with(Self::IS_EXTENSION, on)
    }
    #[inline]
    pub const fn with_is_abstract(self, on: bool) -> Self {
        self.with(Self::IS_ABSTRACT, on)
    }
    #[inline]
    pub const fn with_is_infix(self, on: bool) -> Self {
        self.with(Self::IS_INFIX, on)
    }
}

/// Provider-owned physical realization of a semantic member. Resolution carries this opaque handle
/// unchanged; only lowering interprets it after overload selection has committed the declaration.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MemberRealization {
    /// Ordinary dispatch through the semantic receiver.
    #[default]
    Dispatch,
    /// Direct provider call. Some realizations encode the semantic receiver as their first argument
    /// (value-class implementation methods); others address a singleton implementation directly.
    Direct { pass_receiver: bool },
    /// Compiler-supplied implementation attached to this exact semantic declaration. The provider
    /// identifies the declaration; common lowering only preserves the operation for each backend.
    Intrinsic(CompilerIntrinsic),
    /// A builtin range operator whose declaration is semantic but whose platform implementation is
    /// not an instance method on the receiver. The selected provider supplies the construction plan;
    /// lowering never infers this from a callable name.
    RangeConstruction { open_end: bool },
}

/// One member (constructor, member function/property accessor, or companion member) of a library
/// type, in Kotlin terms. `descriptor` is an opaque backend token (a JVM method descriptor) the
/// matching emitter consumes verbatim — the front end matches on `params`/`ret`, never parsing it.
#[derive(Clone, Debug)]
pub struct LibraryMember {
    /// The Kotlin/source name used for resolution (`CharSequence.get`, `Number.toInt`).
    pub name: String,
    /// Concrete platform owner when it differs from the receiver's resolved type.
    pub owner: Option<TypeName>,
    /// Physical method name when it differs from the Kotlin/source member name.
    pub physical_name: Option<String>,
    /// Declaration/ABI parameter types before call-site generic substitution. Resolution specializes
    /// [`Self::params`]; lowering consumes this stable parallel shape.
    pub physical_params: Vec<Ty>,
    pub params: Vec<Ty>,
    pub ret: Ty,
    pub physical_ret: Ty,
    pub descriptor: String,
    pub realization: MemberRealization,
    pub signature: Option<String>,
    /// The member's PARSED generic signature, if the provider has one — carries type-variable binding
    /// facts (a constructor's `(TA;TB;)V`) without making consumers parse backend signature strings.
    /// Used to infer a construction's type arguments against the enclosing type's [`LibraryType::type_params`].
    pub generic_sig: Option<GenericSig>,
    /// Bit-packed `ret_nullable`/`is_interface`/`suspend` (read via the accessors below; set via `set_*`).
    /// `ret_nullable` — Kotlin metadata return nullability (`T?`); descriptors erase this, but resolution
    /// needs it so nullable generic/member returns stay boxed/reference-like until a use site demands
    /// unboxing. `suspend` — the member is a `suspend fun`; a call site inside a suspend body must thread
    /// a `Continuation` into the emitted invoke (its CPS descriptor rebuilt by the coroutine pass) and
    /// treat the Object-erased result as `ret`.
    pub flags: LmFlags,
    pub inline: InlineKind,
    /// At least one member type parameter carries Kotlin's `reified` modifier.
    pub reified: bool,
    /// Structural expansion decoded from this exact member's inline body.
    pub inline_body_plan: Option<Box<InlineBodyPlan>>,
    /// The member's Kotlin visibility, from its bytecode access flags/`@Metadata`. A `Protected` member
    /// is surfaced (not dropped) so a subclass can reach an inherited classpath member; the emit is
    /// identical to a public one. `Public` by default.
    pub visibility: Visibility,
    /// Source call-shape (parameter names + default flags + `required`, lambda parameter types) — the same
    /// facts `CallSig` carries for functions. Lets a resolver member query drive a NAMED-argument member
    /// call and lambda-parameter typing without the removed receiver-indexed `functions()` seam. Default
    /// (empty) for a provider that records no source parameter metadata.
    pub call_sig: CallSig,
    /// Leading context-parameter count of this member's logical parameter list.
    pub context_count: usize,
    /// Declared source-level overload priority from metadata.
    pub low_priority: bool,
    /// Declared contract effects from metadata.
    pub contract: Option<std::sync::Arc<crate::contracts::Contract>>,
    /// File-independent values for source defaults, parallel to [`Self::params`]. Presence and named
    /// argument mapping remain in [`Self::call_sig`]; this payload is consumed only after selection.
    pub default_values: Vec<Option<DefaultValue>>,
    /// Opaque default-argument bridge coupled to this selected declaration.
    pub default_realization: Option<Box<DefaultCallRealization>>,
    /// The member's DECLARED (un-erased, pre-substitution) return type, straight from `@Metadata` —
    /// the return analogue of [`LibraryCallable::source_receiver`], and recorded with no value-class
    /// reasoning of its own.
    ///
    /// [`Self::ret`] cannot serve: it is the SUBSTITUTED type, so `List<TokenBox>.get` and
    /// `A.create(): A<String>` both present as "returns a value class, physically `Object`" even though
    /// the first hands back a BOX out of a generic slot and the second the erased carrier. Only the
    /// DECLARATION separates them — `get` declares the type parameter `E`, `create` declares `A`. The
    /// value-class pass reads this to decide the result's representation. `None` when the provider
    /// records no metadata return classifier.
    pub declared_ret: Option<Ty>,
    /// Language-defined classifier callable synthesized for this declaration. This is semantic
    /// identity, not a backend spelling: each backend decides how the selected operation is realized.
    /// Ordinary declared members carry `None`.
    pub implicit_classifier_callable: Option<ImplicitClassifierCallable>,
    /// Compiler-plugin expression implementation attached to this exact declaration. Ordinary
    /// declarations leave it unset; selection carries it unchanged to the plugin planning phase.
    pub plugin_expression: Option<PluginExpressionDeclaration>,
    /// Exact AST-backed member declaration selected from the current compilation. This is a handoff
    /// from selection to lowering, not an overload key. Dependency and synthesized members leave it
    /// unset.
    pub source_member: Option<SourceMember>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SourceMember {
    Class {
        file: u32,
        owner: u32,
        method: u32,
    },
    EnumEntry {
        file: u32,
        owner: u32,
        entry: u32,
        method: u32,
    },
    ClassProperty {
        file: u32,
        owner: u32,
        property: u32,
    },
}

impl SourceMember {
    pub fn file(self) -> u32 {
        match self {
            Self::Class { file, .. }
            | Self::EnumEntry { file, .. }
            | Self::ClassProperty { file, .. } => file,
        }
    }
}

/// Callable declarations contributed by the Kotlin language to a classifier rather than written in
/// its body. They participate in the ordinary classifier-callable overload set; the tag survives
/// selection so lowering never rediscovers the operation from its source name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImplicitClassifierCallable {
    EnumValues,
    EnumValueOf,
}

/// A property contributed by the Kotlin language to a classifier rather than declared on an
/// instance or companion object. Its source type is backend-neutral; a provider may additionally
/// attach the exact physical getter used by lowering.
#[derive(Clone, Debug)]
pub struct ClassifierProperty {
    pub ty: Ty,
    pub getter: Option<LibraryMember>,
}

/// A source-declared callable whose implementation is supplied by the compiler after ordinary symbol
/// and overload selection. Providers attach this to the exact declaration identity; lowering never
/// grants intrinsic behavior from a coincidental source name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompilerIntrinsic {
    ArraySize,
    CharCode,
    StringLength,
    StringPlus,
    NullableAnyToString,
    Assert,
    AssertFailsWith,
    Print,
    Println,
    StartCoroutine,
    CoroutineSuspended,
    SuspendCoroutine,
    SuspendCoroutineUninterceptedOrReturn,
    EnumValues,
    EnumValueOf,
    ForEach,
    ForEachIndexed,
    Map,
    FlatMap,
    IsEmpty,
    IsNotEmpty,
    Count,
    TrimIndent,
    TrimMargin,
}

/// A declaration-defined inline body whose source-independent control-flow shape must be expanded
/// before backend coroutine lowering. Providers decode this from the exact selected declaration's
/// compiled inline body; source spelling never participates.
#[derive(Clone, Debug)]
pub enum InlineBodyPlan {
    /// Invoke one function-typed parameter with values loaded from other callable parameters and return
    /// the invocation result.
    InvokeLambda {
        lambda_parameter: usize,
        argument_parameters: Vec<usize>,
        /// A callable parameter returned after the invocation (`apply` returns its receiver). `None`
        /// means the invocation result itself is returned (`let`, `run`, `with`).
        return_parameter: Option<usize>,
    },
    /// Invoke a suspending member on the extension receiver, invoke one lambda parameter, and invoke a
    /// cleanup member with the same state argument on normal and exceptional exits.
    SuspendBeforeLambdaFinally {
        lambda_parameter: usize,
        state_parameter: usize,
        state_default: DefaultValue,
        enter: Box<LibraryMember>,
        cleanup: Box<LibraryMember>,
    },
}

/// Opaque compiler-plugin implementation identity attached to one declared callable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PluginExpressionDeclaration {
    pub plugin: &'static str,
    pub operation: &'static str,
}

/// A public static field and its optional compile-time constant.
#[derive(Clone, Debug)]
pub struct StaticFieldRef {
    pub owner: TypeName,
    pub name: String,
    /// Opaque target field token when the provider knows the physical declaration. Source-only
    /// providers leave it absent; lowering then asks the selected target runtime to realize the
    /// checked semantic type. Keeping absence explicit avoids embedding a target descriptor or a
    /// sentinel string in the provider-neutral resolver.
    pub descriptor: Option<String>,
    pub ty: Ty,
    pub constant: Option<LibraryConst>,
}

/// One field declaration retained on a classifier shape. Keeping inaccessible and static declarations
/// is intentional: either declaration hides an equally named inherited instance field even though it
/// cannot itself realize a Kotlin instance-property read.
#[derive(Clone, Debug)]
pub struct LibraryField {
    pub name: String,
    /// Logical source type before receiver type arguments are substituted.
    pub ty: Ty,
    /// Erased type recovered from the physical descriptor. This is the safe result for a raw receiver
    /// whose declaration type still contains an unbound type parameter.
    pub erased_ty: Ty,
    /// Opaque backend token consumed only by the platform emitter.
    pub descriptor: String,
    pub visibility: Visibility,
    pub is_static: bool,
}

/// An exact readable instance-field declaration selected by the shared hierarchy walk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstanceFieldRef {
    pub owner: TypeName,
    pub name: String,
    pub ty: Ty,
    pub descriptor: String,
}

/// Source-level services exposed by compiled libraries.
/// A classpath `typealias`'s expansion, as a use site needs it.
#[derive(Clone, Debug, PartialEq)]
pub struct AliasExpansion {
    /// Stable qualified identity of the alias declaration. Source spelling is resolved to this
    /// identity before the template is selected.
    pub identity: TypeName,
    /// The alias's TARGET classifier. A template applies only when the spelling that named it
    /// actually resolved to this classifier — otherwise a same-named class, a user alias, or a
    /// different package's alias would inherit an expansion that does not describe it.
    pub target: TypeName,
    /// The alias's own type-parameter names, in declaration order — the substitution domain.
    pub formals: Vec<String>,
    /// The target applied to its own arguments, with the alias's parameters as `Ty::TyParam`.
    pub expansion: Ty,
}

pub trait SemanticPlatform: crate::symbol_source::SymbolSource {
    /// Semantic interface/class used by the platform libraries to model a function value of `arity`.
    fn function_type(&self, _arity: usize) -> Option<Ty> {
        None
    }

    /// The value-class underlying type for a semantic type, when this source knows it. The default has
    /// no value classes; a library provider recovers the underlying from its type metadata plus
    /// any builtins whose source type is not represented as `Ty::Obj` (`UInt` → `Int`).
    fn value_underlying(&self, _ty: Ty) -> Option<Ty> {
        None
    }

    /// A runtime-valued public static field on `internal` or its supertypes.
    fn static_field(&self, _internal: &str, _name: &str) -> Option<StaticFieldRef> {
        None
    }

    fn static_field_name(&self, _internal: TypeName, _name: &str) -> Option<StaticFieldRef> {
        None
    }

    /// The storage of a package-level `const val` (`kotlin.math.PI`). A `const` has no accessor — the
    /// platform holds it in a field on whichever package artifact owns the declaration — so it cannot be
    /// answered by the property namespace, which models properties by their accessors. Resolution of a
    /// bare imported name needs the OWNER, which only the platform knows.
    fn top_level_static_field(&self, _package: TypeName, _name: &str) -> Option<StaticFieldRef> {
        None
    }

    /// Normalize a semantic type to the identity used when comparing source call arguments against
    /// compiled-library signatures. This is not an emit descriptor; it is a semantic compatibility key.
    /// Implementations that do not need library-name normalization return the type unchanged.
    fn library_value_form(&self, ty: Ty) -> Ty {
        ty
    }

    fn library_value_form_name(&self, internal: TypeName) -> TypeName {
        self.library_value_form(Ty::obj_name(internal))
            .obj_internal()
            .unwrap_or(internal)
    }

    /// Convert a platform classifier name to the identity used by source resolution.
    fn canonical_source_type_name(&self, internal: TypeName) -> TypeName {
        internal
    }

    /// A classpath `typealias`'s EXPANSION. `typealias Lens<S, A> = PLens<S, S, A, A>` maps two
    /// arguments onto four positions, so a use site must substitute rather than paste its arguments
    /// onto the target. `None` when the identity is not a published alias declaration.
    fn type_alias_expansion(&self, _internal: TypeName) -> Option<AliasExpansion> {
        None
    }

    /// Whether a library owner belongs to the platform's default Kotlin library surface.
    fn is_default_library_owner(&self, _internal: TypeName) -> bool {
        false
    }

    /// Whether a resolved top-level callable is the platform's erased contract-declaration
    /// intrinsic. Consumers ask after ordinary import-scoped symbol resolution, so source/module
    /// shadowing and aliases are handled once by the resolver; the provider owns the physical
    /// callable identity and no facade/class name leaks into target-neutral checking code.
    fn is_erased_contract_callable(&self, _callable: &LibraryCallable) -> bool {
        false
    }

    /// Primitive represented by a platform wrapper type.
    fn boxed_primitive(&self, _ty: Ty) -> Option<Ty> {
        None
    }

    /// Primitive represented by a nullable source value or a platform wrapper reference. This is the
    /// single semantic query for checker/lowerer sites that accept either representation; centralizing the
    /// composition prevents equality, coercion, and emission from growing separate target-specific tests.
    fn reference_primitive(&self, ty: Ty) -> Option<Ty> {
        ty.nullable_primitive().or_else(|| self.boxed_primitive(ty))
    }

    /// The receiver-MRO RUNG of an extension whose declared receiver is `decl_recv`, for an actual receiver
    /// `recv`: `0` when the extension's receiver IS the receiver's own type, increasing up the receiver's
    /// supertype chain (with the platform's primitive/array/value-class widening — an `Int` widens through
    /// `Number`/`Comparable`/`Any`), so a `List` extension outranks an `Iterable` one. `None` when
    /// `decl_recv` is not in the receiver's MRO (the extension does not apply). This is the receiver-coupled
    /// "most specific receiver wins" order Kotlin overload resolution uses, recovered by the consumer for a
    /// receiver-agnostic `resolve_symbols` overload (which carries no rung). Default: apply only on an exact
    /// type match (a target with no supertype model).
    fn extension_receiver_rank(&self, recv: Ty, decl_recv: Ty) -> Option<u32> {
        (self.library_value_form(recv) == self.library_value_form(decl_recv)).then_some(0)
    }

    /// If values of this type can be invoked like a Kotlin function, return their arity. Plain
    /// `Ty::Fun` is handled by the default; platform providers can add callable runtime types such as
    /// property references without the checker knowing their class names.
    fn function_like_arity(&self, ty: Ty) -> Option<usize> {
        ty.fun_arity().map(usize::from)
    }

    /// The platform/library type used for a property reference with the given arity and mutability.
    /// Resolver needs this type so direct property-reference APIs (`get`, `name`) keep working, but the
    /// actual class name is provider-owned.
    /// `args` are the reference type's own type arguments in declaration order — `[V]` for a
    /// `KProperty0<V>`, `[Recv, V]` for a `KProperty1<Recv, V>`. Passing them is what makes
    /// `(::foo).get()` type as the property's own type instead of the erased upper bound.
    fn property_reference_type(&self, _arity: usize, _mutable: bool, _args: &[Ty]) -> Option<Ty> {
        None
    }

    /// Platform reflection classifier for an already-resolved callable-reference signature.
    /// Direction matters: the signature selects the classifier; consumers never parse a classifier
    /// name to reconstruct the signature.
    fn function_reference_type(&self, _function: Ty) -> Option<Ty> {
        None
    }

    /// The type produced by a class literal (`X::class`) on this target/platform.
    fn class_literal_type(&self) -> Option<Ty> {
        None
    }

    /// A platform property implemented by compiler lowering rather than an ordinary getter.
    fn intrinsic_property(&self, _receiver: Ty, _name: &str) -> Option<LibraryMember> {
        None
    }

    /// Platform constraints not represented in the ordinary semantic hierarchy.
    fn implicit_common_supertypes(&self, _types: &[Ty]) -> Vec<SemanticSupertype> {
        Vec::new()
    }

    /// Additional default wildcard-import packages contributed by this platform, in dotted Kotlin
    /// package syntax. Common Kotlin defaults live in the resolver; this hook is only for documented
    /// target additions such as JVM's `java.lang` and `kotlin.jvm`.
    fn platform_default_import_packages(&self) -> &'static [&'static str] {
        &[]
    }

    /// Platform spellings for physical zero-arg getters when declaration metadata is unavailable.
    /// Common resolution asks for a semantic property name; the target returns every physical spelling
    /// as one provider result because JVM uses JavaBean-style `getX`/`isX` while other targets need not.
    /// The candidates are most-conventional first
    /// (`id` → `getId`, `getID`; `urlPath` → `getUrlPath`, `getURLPath`) — the inverse of
    /// Kotlin's decapitalize-smart getter-to-property mapping.
    fn physical_property_getter_names(&self, _property: &str) -> Vec<String> {
        Vec::new()
    }

    /// Resolve a built-in type's SIMPLE name (`List`, `Map`, `Comparable`) to its front-end internal
    /// name, when the local type-reference resolver has no classpath/import context. The source owns
    /// this because built-in identity may come from compiled library metadata.
    /// `None` for a name that is not a platform/library built-in.
    fn builtin_type_internal(&self, _simple_name: &str) -> Option<String> {
        None
    }

    /// Source-to-physical member mappings required by an applied platform interface.
    fn mapped_interface_members(&self, _supertype: Ty) -> Vec<MappedInterfaceMember> {
        Vec::new()
    }

    /// The reified type-parameter formal NAMES a compiled generic signature declares, in order. The
    /// source parses its own metadata/signature format; consumers bind names without parsing backend text.
    fn signature_formal_names(&self, _signature: &str) -> Vec<String> {
        Vec::new()
    }

    /// Element type for platform iterable/range/progression values.
    fn iterable_element_type(&self, _internal: &str) -> Option<Ty> {
        None
    }

    fn iterable_element_type_name(&self, _internal: TypeName) -> Option<Ty> {
        None
    }
}

#[derive(Clone, Debug)]
pub struct MappedInterfaceMember {
    pub source_name: String,
    pub physical_name: String,
    pub params: Vec<Ty>,
    pub ret: Ty,
    pub is_property: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SemanticSupertype {
    pub name: TypeName,
    pub type_parameters: usize,
}

impl LibraryMember {
    pub fn new(name: String, params: Vec<Ty>, ret: Ty, descriptor: String) -> Self {
        LibraryMember {
            name,
            owner: None,
            physical_name: None,
            physical_params: params.clone(),
            params,
            ret,
            physical_ret: ret,
            descriptor,
            realization: MemberRealization::Dispatch,
            signature: None,
            generic_sig: None,
            flags: LmFlags::default(),
            inline: InlineKind::None,
            reified: false,
            inline_body_plan: None,
            visibility: Visibility::Public,
            call_sig: CallSig::default(),
            context_count: 0,
            low_priority: false,
            contract: None,
            default_values: Vec::new(),
            default_realization: None,
            declared_ret: None,
            implicit_classifier_callable: None,
            plugin_expression: None,
            source_member: None,
        }
    }

    #[inline]
    pub fn ret_nullable(&self) -> bool {
        self.flags.has(LmFlags::RET_NULLABLE)
    }
    #[inline]
    pub fn is_interface(&self) -> bool {
        self.flags.has(LmFlags::IS_INTERFACE)
    }
    #[inline]
    pub fn suspend(&self) -> bool {
        self.flags.has(LmFlags::SUSPEND)
    }
    #[inline]
    pub fn is_operator(&self) -> bool {
        self.flags.has(LmFlags::IS_OPERATOR)
    }
    #[inline]
    pub fn is_infix(&self) -> bool {
        self.flags.has(LmFlags::IS_INFIX)
    }
    #[inline]
    pub fn is_member_extension(&self) -> bool {
        self.flags.has(LmFlags::IS_EXTENSION)
    }
    #[inline]
    pub fn set_ret_nullable(&mut self, on: bool) {
        self.flags = self.flags.with_ret_nullable(on);
    }
    #[inline]
    pub fn set_is_interface(&mut self, on: bool) {
        self.flags = self.flags.with_is_interface(on);
    }
    #[inline]
    pub fn set_suspend(&mut self, on: bool) {
        self.flags = self.flags.with_suspend(on);
    }
    pub fn set_is_operator(&mut self, on: bool) {
        self.flags = self.flags.with_is_operator(on);
    }
    pub fn set_is_infix(&mut self, on: bool) {
        self.flags = self.flags.with_is_infix(on);
    }
    pub fn set_is_member_extension(&mut self, on: bool) {
        self.flags = self.flags.with_is_extension(on);
    }
    pub fn is_abstract(&self) -> bool {
        self.flags.has(LmFlags::IS_ABSTRACT)
    }
    pub fn set_is_abstract(&mut self, on: bool) {
        self.flags = self.flags.with_is_abstract(on);
    }
    pub fn owner_name(&self) -> Option<String> {
        self.owner.map(TypeName::render)
    }

    pub fn owner_name_or(&self, fallback: &str) -> String {
        self.owner_name().unwrap_or_else(|| fallback.to_string())
    }

    pub fn owner_type_or(&self, fallback: TypeName) -> TypeName {
        self.owner.unwrap_or(fallback)
    }
}

/// Which source a resolved callable came from — set by the source that resolves it, read by the
/// lowerer to choose the emit form: a current-module callable lowers to a same-file `Local`/cross-file
/// call, a library callable to an `invokestatic`/external call. `facade` is the module callable's
/// declaring facade internal name (the file/class it belongs to). Defaults to [`Origin::Library`].
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum Origin {
    #[default]
    Library,
    Module {
        facade: TypeName,
    },
}

impl LibraryCallable {
    pub fn library(
        owner: impl Into<TypeName>,
        name: impl Into<String>,
        params: Vec<Ty>,
        ret: Ty,
        physical_ret: Ty,
        descriptor: impl Into<String>,
    ) -> Self {
        LibraryCallable {
            owner: owner.into(),
            name: name.into(),
            compiler_intrinsic: None,
            plugin_expression: None,
            inline_body_plan: None,
            physical_params: params.clone(),
            params,
            ret,
            physical_ret,
            descriptor: descriptor.into(),
            suspend: false,
            is_abstract: false,
            owner_is_interface: false,
            member_realization: MemberRealization::Dispatch,
            inline: InlineKind::None,
            default_call: false,
            vararg_elem: None,
            vararg_index: None,
            signature: None,
            origin: Origin::Library,
            source_receiver: None,
            context_count: 0,
            contract: None,
            generic_sig: None,
            singleton_dispatch: None,
            default_realization: None,
            declared_ret: None,
        }
    }

    pub fn owner_name(&self) -> String {
        self.owner.render()
    }

    pub fn owner_type(&self) -> TypeName {
        self.owner
    }

    pub fn owner_matches(&self, internal: &str) -> bool {
        self.owner.matches(internal)
    }

    pub fn owner_starts_with(&self, prefix: &str) -> bool {
        self.owner.starts_with(prefix)
    }

    pub fn owner_contains(&self, needle: &str) -> bool {
        self.owner.contains(needle)
    }

    pub fn owner_package_matches(&self, package: &str) -> bool {
        self.owner.package_matches(package)
    }

    pub fn owner_package_matches_name(&self, package: TypeName) -> bool {
        self.owner.parent() == Some(package)
    }

    pub fn owner_package(&self) -> String {
        self.owner.package()
    }
}

/// A resolved companion-object function on a classpath value class (`Result.success`). The call lowers
/// to `getstatic <class>.<field>:L<companion>;` (the receiver) then an inline-splice of the companion
/// INSTANCE method carried by `callable` (its `this` is the loaded singleton).
#[derive(Clone, Debug)]
pub struct CompanionFn {
    /// The value-class declaring the companion (`kotlin/Result`).
    pub class_internal: TypeName,
    /// The companion object's internal name (`kotlin/Result$Companion`).
    pub companion_internal: TypeName,
    /// The static field on `class_internal` holding the singleton (`Companion`).
    pub companion_field: String,
    /// Selected companion method. Its `owner` is `companion_internal`; its name/descriptor are backend
    /// tokens, and its params/ret are the logical Kotlin call shape.
    pub callable: LibraryCallable,
}

/// A package-level callable: a top-level function (`listOf`), or an extension (its receiver is the
/// first parameter). `owner` is the internal name of the facade/declaring container for emit.
#[derive(Clone, Debug)]
pub struct LibraryCallable {
    pub owner: TypeName,
    /// Kotlin/source name used for selection.
    pub name: String,
    /// Compiler implementation attached to this exact declaration, if it has no ordinary callable
    /// body on the target. Selection remains completely ordinary; the backend consumes this tag only
    /// after the checker has committed the call target.
    pub compiler_intrinsic: Option<CompilerIntrinsic>,
    pub plugin_expression: Option<PluginExpressionDeclaration>,
    /// Structural expansion decoded from this exact declaration's inline body.
    pub inline_body_plan: Option<Box<InlineBodyPlan>>,
    /// Call-site-specialized source parameter types used for applicability and checking.
    pub params: Vec<Ty>,
    /// Declaration parameter types used by the platform ABI. Generic selection may specialize
    /// [`Self::params`], but it never changes this vector (`fun <T> id(x: T)` remains `Object`-erased
    /// when called as `id("x")`).
    pub physical_params: Vec<Ty>,
    /// The *logical* return type — for a generic callable, the substituted type (`listOf<Int>` →
    /// `List<Int>`, `first()` → the element). The checker reports this.
    pub ret: Ty,
    /// The *physical* (erased) return type the JVM signature actually produces (`Object` for an erased
    /// type parameter). The backend inserts the unbox/checkcast bridging `physical_ret` → `ret`.
    pub physical_ret: Ty,
    pub descriptor: String,
    /// The callee is a `suspend` fun/extension — a call to it inside a suspend body threads a
    /// `Continuation` (and a lambda whose body calls one becomes a coroutine state machine). The checker
    /// records this on the resolved callable so the lowerer never re-queries the library for it.
    pub suspend: bool,
    /// The semantic declaration has no implementation. This lives on the callable handle so
    /// property accessors and functions expose the same provider-normalized modality; physical JVM
    /// access flags are only one input at a provider boundary.
    pub is_abstract: bool,
    /// [`owner`](Self::owner) is an INTERFACE, so the call dispatches with `invokeinterface`. Carried on
    /// the selected callable because the owner's own declaration may not be re-readable at the call
    /// site: a mapped builtin's JVM owner (`java/util/List`) has no class file when no JDK is on the
    /// classpath, and the fact then exists only on the `.kotlin_builtins` member this callable was
    /// selected from. Dropping it emitted `invokevirtual` on an interface — an
    /// `IncompatibleClassChangeError` at class-load time.
    pub owner_is_interface: bool,
    /// A semantic member whose physical implementation is static and consumes the dispatch receiver
    /// as its leading argument. This remains an opaque realization fact; overload selection still
    /// treats the declaration as an ordinary member.
    pub member_realization: MemberRealization,
    /// The callee's inline-ness in one field (was `is_inline` + `must_inline`): [`InlineKind::CanInline`]
    /// for a Kotlin `inline` function the backend MAY splice instead of emitting an `invokestatic`,
    /// [`InlineKind::MustInline`] for a non-public `@InlineOnly` callee the backend MUST splice (no legal
    /// call site), [`InlineKind::None`] otherwise.
    pub inline: InlineKind,
    /// True when this resolves a `name$default` synthetic (a callable with defaulted parameters called
    /// with fewer arguments): `params` are the real parameters, and the backend appends zero/`null`
    /// placeholders for the omitted trailing ones, an `int` bit-mask (a bit set per omitted parameter),
    /// and a `null` marker — the JVM realization of default arguments. `false` for an ordinary call.
    pub default_call: bool,
    /// For a generic `vararg` callable resolved with a bound element type (`listOf<Long>(…)` →
    /// `Long`): the *logical* element type the trailing arguments adapt to. `None` for a non-vararg
    /// call or when the element type is not recovered. The backend uses it to coerce each packed
    /// element to that type before boxing (an integer literal in `listOf<Long>(3)` becomes a boxed
    /// `Long`, not `Integer`), since the JVM array element is erased to `Object`.
    pub vararg_elem: Option<Ty>,
    /// Source-level value-parameter slot occupied by [`Self::vararg_elem`], excluding an extension
    /// receiver. This must travel with the selected callable: a generic `vararg T` can specialize
    /// logically to `String` while its physical slot remains `Object[]`, so lowering cannot safely
    /// rediscover the slot by comparing element types. `None` for a non-element-form call.
    pub vararg_index: Option<usize>,
    /// The callee's generic `Signature` (an opaque backend token), kept so an arg-binding SELECTOR can
    /// recover the substituted return (`fold`'s `R` from the initial value, `let`'s `R` from the lambda)
    /// when picking this overload out of a [`FunctionSet`]. `None` when the callable has no generic
    /// signature. The front end never parses it — only the platform's resolution helpers do.
    pub signature: Option<String>,
    /// Which source produced this callable — the lowerer's cue for the emit form. [`Origin::Library`]
    /// for a classpath callable; [`Origin::Module`] (with its facade) for a current-/sibling-module one.
    pub origin: Origin,
    /// For an EXTENSION callable: its DECLARED receiver source type, un-erased (`fun Result<T>.getOrThrow`
    /// → `Some(Obj("kotlin/Result", …))`). A generic type-variable receiver (`fun <T> T.foo`) is `None` —
    /// it erases to `Object` and carries no value-class identity. `None` for a non-extension callable.
    /// The value-class pass reads this (via `IrFile::ext_call_source_receiver`) to decide whether a boxed
    /// extension receiver must unbox to the value class's underlying; `params[0]` is already erased and
    /// cannot make that distinction. This is the un-erased-source-type down payment on task B.
    pub source_receiver: Option<Ty>,
    /// The callee's DECLARED (un-erased, pre-substitution) return type — the return analogue of
    /// [`Self::source_receiver`], carried for the same reason and read by the same pass. See
    /// [`LibraryMember::declared_ret`]: [`Self::ret`] is the SUBSTITUTED type, which cannot say whether
    /// a value-class result is the erased CARRIER (declared to return it) or a BOX out of a generic
    /// slot. Only non-null declared returns are recorded; a nullable value class really is boxed.
    pub declared_ret: Option<Ty>,
    /// Number of LEADING context parameters (`context(a: A) fun f()`) in `params` — supplied
    /// implicitly by the caller, not positionally, so arity checks and argument mapping skip them.
    pub context_count: usize,
    /// The callable's declared contract, decoded from `@Metadata` — the effects the checker applies
    /// at call sites (`returns(…) implies …`, `callsInPlace`). `None` when it declares none.
    pub contract: Option<std::sync::Arc<crate::contracts::Contract>>,
    /// The metadata-primary generic signature (formal type parameters + receiver/param/return
    /// nodes), when the callable came from `@Metadata` with one. The checker reads this to bind a
    /// contract's type-parameter conclusions (`value is R`) at the call site — the JVM `Signature`
    /// in [`Self::signature`] is absent on krusty-emitted classes. Boxed: this struct rides the
    /// `ResolvedCall` enum, whose variant size must stay flat.
    pub generic_sig: Option<Box<GenericSig>>,
    /// The static field holding this callable's dispatch receiver, for a member declared inside an
    /// `object` / `companion object` and brought into scope by `import Owner.name`.
    ///
    /// Kotlin's rule is that importing an object's member carries the object along as the implicit
    /// dispatch receiver; for a member EXTENSION the use site supplies the extension receiver and the
    /// singleton is the dispatch. The JVM realization is not a facade `invokestatic` but a load of the
    /// singleton followed by an INSTANCE invoke — and where that singleton lives differs by shape: a
    /// plain `object` owns `INSTANCE`, while a companion's singleton is a field on the OUTER class whose
    /// name is the companion's (`Companion` unless it was named). Resolution already had to find that
    /// field to recognize the owner as an object, so it travels here rather than being re-derived from a
    /// name at emit. `None` for every other callable.
    ///
    /// Boxed for the same reason as [`Self::generic_sig`]: this struct rides the `ResolvedCall` enum,
    /// whose variant size must stay flat, and only a vanishing fraction of callables carry one.
    pub singleton_dispatch: Option<Box<StaticFieldRef>>,
    /// Opaque platform target for this declaration's default-argument bridge. It is attached to the
    /// selected source callable and never participates in name or overload resolution.
    pub default_realization: Option<Box<DefaultCallRealization>>,
}

#[derive(Clone, Debug)]
pub struct DefaultCallRealization {
    pub descriptor: String,
    pub real_params: Vec<Ty>,
    /// Number of platform mask words before the trailing marker. Zero denotes a marker-only
    /// realization; consumers must not infer this ABI fact from the source parameter count.
    pub mask_count: usize,
    pub ret: Ty,
    pub suspend: bool,
}

/// How a resolved function relates to the call's receiver — drives Kotlin overload precedence (a member
/// wins over an extension, both over a top-level function).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FnKind {
    /// A member of the receiver's type (or an inherited one).
    Member,
    /// An extension function on the receiver's type.
    Extension,
    /// A receiver-less top-level function.
    TopLevel,
}

/// The source-level call shape of one overload — the call-site facts the CHECKER needs that the erased
/// emit `descriptor` drops. Parallel to the LOGICAL parameter list (the receiver is NOT included, even
/// for an extension whose `callable.params` prepends it). Empty/zero `Default` means "not provided by
/// this source"; the federated consumer falls back as it did before the consolidation.
#[derive(Clone, Default, Debug)]
pub struct CallSig {
    /// Parameter names, parallel to the logical params — maps named arguments (`f(x = 1)`) to positions.
    pub param_names: Vec<String>,
    /// Per logical param: whether it has a default value (so it may be omitted). Parallel to the params.
    pub param_defaults: Vec<bool>,
    /// Per logical parameter: the declared type is annotated with Kotlin's internal `@Exact`, so
    /// applicability requires equality after generic substitution rather than ordinary subtyping.
    pub exact_params: Vec<bool>,
    /// Per logical parameter, the resolved declaration carries
    /// `kotlin.internal.ImplicitIntegerCoercion`.
    pub implicit_integer_coercion: Vec<bool>,
    /// Per logical param: if it is a function type `(A, B) -> R`, its inner param types `[A, B]` (to type
    /// a lambda argument's `it`/params); otherwise empty. Parallel to the params.
    pub lambda_param_types: Vec<Vec<Ty>>,
    /// Per logical param: `Some(receiver)` when the parameter is a receiver function type
    /// `Receiver.(...) -> R`. The checker binds that receiver as lambda `this` while using
    /// `lambda_param_types` for the receiver/value parameters recovered from the generic signature.
    pub lambda_receivers: Vec<Option<Ty>>,
    /// Per logical param: whether it is a receiver function type, even when metadata cannot name a
    /// concrete receiver class because the receiver is a type parameter (`T.() -> R`).
    pub lambda_receiver_params: Vec<bool>,
    /// Leading context receiver count for each function-typed parameter.
    pub lambda_context_counts: Vec<usize>,
    /// Per logical param: whether it is `crossinline`/`noinline` — its lambda argument is MATERIALIZED
    /// (a real `FunctionN`/nested class) rather than inline-spliced, so a mutable local it captures must
    /// be `Ref`-boxed like an ordinary closure. Parallel to the params; all-false for a non-inline fn.
    pub lambda_materialized: Vec<bool>,
    /// Per logical Java parameter, whether nullable arguments are accepted.
    pub platform_nullable_params: Vec<bool>,
    /// Minimum arguments a caller must supply (params beyond this have defaults). 0 by default.
    pub required: usize,
    /// True if a logical param is `vararg` (callers pack values into its array).
    pub vararg: bool,
    pub vararg_index: Option<usize>,
}

#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub struct ParamList {
    pub visibility: Visibility,
    pub names: Vec<String>,
    pub defaults: Vec<bool>,
    /// Declared Kotlin parameter types, parallel to [`Self::names`]. Empty only when the provider cannot
    /// recover the semantic signature. Constructor selection must never replace these with storage types.
    pub types: Vec<Ty>,
    /// Per parameter: whether its declared type is a RECEIVER function type (`Recv.() -> R`). The JVM
    /// descriptor and `Signature` both erase that to a plain `FunctionN`, so only the source-level
    /// metadata carries it — and without it a lambda argument gets no implicit receiver, leaving a bare
    /// member call inside unresolved. Empty when the origin records no per-parameter types.
    pub recv_fun: Vec<bool>,
    pub vararg: Option<usize>,
    /// Present only when this is a provider-normalized annotation application shape rather than a
    /// constructor parameter list. The checker consumes the policy without asking whether the
    /// declaration came from source, Kotlin metadata, or a Java classfile.
    pub annotation: Option<AnnotationParameterPolicy>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnnotationParameterPolicy {
    /// Whether positional arguments use ordinary constructor order, are unavailable, or feed the
    /// array-typed `value` element as individual values.
    pub positional: AnnotationPositionalPolicy,
    /// Kotlin declaration `vararg val` materializes an omitted empty array; a classfile annotation
    /// element with a default must remain absent so its declaration default stands.
    pub materialize_omitted_vararg: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnnotationPositionalPolicy {
    Constructor,
    NamedOnly,
    /// The declaration's `value` parameter alone accepts one positional argument.
    Value,
    ValueVararg,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnnotationApplication {
    pub parameters: ParamList,
    pub policy: AnnotationParameterPolicy,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallArgMappingFailure {
    pub errors: Vec<CallArgMappingError>,
    /// First positional argument rejected after named arguments.
    pub recovery_argument: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallArgMappingError {
    NoParameterNamed { name: String, argument: usize },
    AlreadyPassed { argument: usize },
    PositionalAfterNamed { argument: usize },
    TooManyArguments { argument: usize, expected: usize },
    TrailingLambdaOnVararg { argument: usize },
    MissingRequired { name: String },
}

impl CallArgMappingError {
    pub(crate) fn highlights_callee(&self) -> bool {
        matches!(self, Self::MissingRequired { .. })
    }
}

impl std::fmt::Display for CallArgMappingError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoParameterNamed { name, .. } => {
                write!(formatter, "no parameter with name '{name}' found.")
            }
            Self::AlreadyPassed { .. } => {
                formatter.write_str("argument already passed for this parameter.")
            }
            Self::PositionalAfterNamed { .. } => {
                formatter.write_str(
                    "mixing named and positional arguments is not allowed unless the order of the arguments matches the order of the parameters.",
                )
            }
            Self::TooManyArguments { expected, .. } => {
                write!(formatter, "too many arguments: expected at most {expected}")
            }
            Self::TrailingLambdaOnVararg { .. } => formatter.write_str(
                "passing value as a vararg is allowed only inside a parenthesized argument list.",
            ),
            Self::MissingRequired { name } => {
                write!(formatter, "no value passed for parameter '{name}'.")
            }
        }
    }
}

// Call and parameter metadata are independent inputs to slot assignment.
#[allow(clippy::too_many_arguments)]
pub fn map_call_args<T: Copy>(
    args: &[T],
    names: Option<&[Option<String>]>,
    param_names: &[String],
    parameter_count: usize,
    required: usize,
    param_defaults: &[bool],
    vararg: Option<usize>,
    trailing_lambda: bool,
) -> Result<Vec<Option<T>>, CallArgMappingFailure> {
    let mut slots = vec![None; parameter_count];
    let mut positional = 0usize;
    let mut seen_named = false;
    let mut vararg_named = false;
    let mut named_order_matches = true;
    let mut errors = Vec::new();
    let mut recovery_argument = None;

    for (argument_index, &argument) in args.iter().enumerate() {
        match names
            .and_then(|names| names.get(argument_index))
            .and_then(Option::as_ref)
        {
            Some(name) => {
                seen_named = true;
                let Some(parameter_index) = param_names
                    .iter()
                    .take(parameter_count)
                    .position(|parameter| parameter == name)
                else {
                    errors.push(CallArgMappingError::NoParameterNamed {
                        name: name.clone(),
                        argument: argument_index,
                    });
                    continue;
                };
                named_order_matches &= parameter_index == argument_index;
                if slots[parameter_index].is_some() {
                    errors.push(CallArgMappingError::AlreadyPassed {
                        argument: argument_index,
                    });
                    continue;
                }
                if vararg == Some(parameter_index) {
                    vararg_named = true;
                }
                slots[parameter_index] = Some(argument);
            }
            None => {
                let is_trailing_lambda = trailing_lambda && argument_index + 1 == args.len();
                if is_trailing_lambda {
                    let last = parameter_count.checked_sub(1);
                    if vararg == last {
                        errors.push(CallArgMappingError::TrailingLambdaOnVararg {
                            argument: argument_index,
                        });
                    } else if let Some(parameter_index) =
                        last.filter(|&index| slots[index].is_none())
                    {
                        slots[parameter_index] = Some(argument);
                    } else {
                        errors.push(CallArgMappingError::TooManyArguments {
                            argument: argument_index,
                            expected: parameter_count,
                        });
                    }
                    continue;
                }

                if seen_named {
                    if named_order_matches && argument_index >= parameter_count {
                        // Overflow past the parameter list is legal when a vararg absorbs it
                        // (`f(a = 1, "x", "y")` — the extras are its elements, reconstructed by
                        // the slot-map contract), unless the vararg was already bound BY NAME.
                        if vararg.is_some() && !vararg_named {
                            continue;
                        }
                        errors.push(CallArgMappingError::TooManyArguments {
                            argument: argument_index,
                            expected: parameter_count,
                        });
                        continue;
                    }
                    let slot = if named_order_matches
                        && argument_index < parameter_count
                        && slots[argument_index].is_none()
                    {
                        Some(argument_index)
                    } else {
                        None
                    };
                    if let Some(parameter_index) = slot {
                        slots[parameter_index] = Some(argument);
                        named_order_matches &= parameter_index == argument_index;
                    } else {
                        recovery_argument.get_or_insert(argument_index);
                        errors.push(CallArgMappingError::PositionalAfterNamed {
                            argument: argument_index,
                        });
                    }
                } else {
                    if vararg == Some(positional) {
                        if slots[positional].is_none() {
                            slots[positional] = Some(argument);
                        }
                        continue;
                    }
                    if positional >= parameter_count {
                        errors.push(CallArgMappingError::TooManyArguments {
                            argument: argument_index,
                            expected: parameter_count,
                        });
                        continue;
                    }
                    slots[positional] = Some(argument);
                    positional += 1;
                }
            }
        }
    }

    if errors.is_empty()
        || (recovery_argument.is_some()
            && errors
                .iter()
                .all(|error| matches!(error, CallArgMappingError::PositionalAfterNamed { .. })))
    {
        for (parameter_index, slot) in slots.iter().enumerate() {
            let has_default = vararg == Some(parameter_index)
                || if param_defaults.is_empty() {
                    parameter_index >= required
                } else {
                    param_defaults
                        .get(parameter_index)
                        .copied()
                        .unwrap_or(false)
                };
            if slot.is_none() && !has_default {
                errors.push(CallArgMappingError::MissingRequired {
                    name: param_names
                        .get(parameter_index)
                        .cloned()
                        .unwrap_or_else(|| format!("p{parameter_index}")),
                });
            }
        }
    }

    if errors.is_empty() {
        Ok(slots)
    } else {
        Err(CallArgMappingFailure {
            errors,
            recovery_argument,
        })
    }
}

impl CallSig {
    /// Apply declaration-owned parameter constraints after generic substitution. Every overload
    /// path uses this predicate so `@Exact` cannot drift between candidate families.
    pub fn parameter_admits(&self, index: usize, expected: Ty, actual: Ty) -> bool {
        !self.exact_params.get(index).copied().unwrap_or(false) || expected == actual
    }

    /// The call shape after parameters supplied outside the source argument list have been removed.
    pub fn suffix(&self, start: usize) -> Self {
        let start = start.min(self.param_names.len());
        CallSig {
            param_names: self.param_names[start..].to_vec(),
            param_defaults: self
                .param_defaults
                .get(start..)
                .unwrap_or_default()
                .to_vec(),
            exact_params: self.exact_params.get(start..).unwrap_or_default().to_vec(),
            implicit_integer_coercion: self
                .implicit_integer_coercion
                .get(start..)
                .unwrap_or_default()
                .to_vec(),
            lambda_param_types: self
                .lambda_param_types
                .get(start..)
                .unwrap_or_default()
                .to_vec(),
            lambda_receivers: self
                .lambda_receivers
                .get(start..)
                .unwrap_or_default()
                .to_vec(),
            lambda_receiver_params: self
                .lambda_receiver_params
                .get(start..)
                .unwrap_or_default()
                .to_vec(),
            lambda_context_counts: self
                .lambda_context_counts
                .get(start..)
                .unwrap_or_default()
                .to_vec(),
            lambda_materialized: self
                .lambda_materialized
                .get(start..)
                .unwrap_or_default()
                .to_vec(),
            platform_nullable_params: self
                .platform_nullable_params
                .get(start..)
                .unwrap_or_default()
                .to_vec(),
            required: self.required.saturating_sub(start),
            vararg: self.vararg,
            vararg_index: self.vararg_index.and_then(|index| index.checked_sub(start)),
        }
    }

    pub fn has_param_names(&self) -> bool {
        !self.param_names.is_empty()
    }

    pub fn has_known_required_param(&self, mut range: std::ops::Range<usize>) -> bool {
        // A vararg slot is never required: it may always be empty (matching the slot mapper's
        // `vararg == Some(parameter_index)` rule in `map_call_args`), even though metadata never
        // sets its `declares_default_value` flag.
        !self.param_defaults.is_empty()
            && range.any(|i| !self.param_has_default(i) && self.vararg_index != Some(i))
    }

    pub fn param_has_default(&self, idx: usize) -> bool {
        self.param_defaults.get(idx).copied().unwrap_or(false)
    }

    pub fn can_map_omitted_args(&self, param_count: usize) -> bool {
        (self.required < param_count || self.vararg_index.is_some()) && self.has_param_names()
    }

    pub fn source(
        param_names: Vec<String>,
        param_defaults: Vec<bool>,
        lambda_param_types: Vec<Vec<Ty>>,
        lambda_recv: Vec<bool>,
        lambda_context_counts: Vec<usize>,
        required: usize,
        vararg_index: Option<usize>,
    ) -> Self {
        let lambda_receivers = lambda_recv
            .iter()
            .enumerate()
            .map(|(i, has_recv)| {
                if *has_recv {
                    lambda_param_types
                        .get(i)
                        .and_then(|parameters| {
                            parameters
                                .get(lambda_context_counts.get(i).copied().unwrap_or_default())
                        })
                        .copied()
                } else {
                    None
                }
            })
            .collect();
        CallSig {
            param_names,
            param_defaults,
            lambda_param_types,
            lambda_receivers,
            lambda_receiver_params: lambda_recv,
            lambda_context_counts,
            required,
            vararg: vararg_index.is_some(),
            vararg_index,
            ..Default::default()
        }
    }

    pub fn metadata_member(
        param_count: usize,
        names: Vec<String>,
        defaults: Vec<bool>,
        vararg_index: Option<usize>,
    ) -> Self {
        CallSig::metadata_base(param_count, names, defaults, vararg_index)
    }

    pub fn metadata_plain(param_count: usize) -> Self {
        CallSig::metadata_base(param_count, Vec::new(), Vec::new(), None)
    }

    /// Build the source call shape for a Kotlin function decoded from metadata. `param_count` is
    /// always the source VALUE-parameter count: dispatch and extension receivers are not call
    /// arguments and therefore never appear in this structure.
    #[allow(clippy::too_many_arguments)]
    pub fn metadata_function(
        param_count: usize,
        names: Vec<String>,
        defaults: Vec<bool>,
        lambda_receivers: Vec<Option<Ty>>,
        lambda_receiver_params: Vec<bool>,
        lambda_materialized: Vec<bool>,
        vararg_index: Option<usize>,
    ) -> Self {
        let mut sig = CallSig::metadata_base(param_count, names, defaults, vararg_index);
        sig.set_lambda_receiver_shape(param_count, lambda_receivers, lambda_receiver_params);
        sig.lambda_materialized = vec_for_arity(lambda_materialized, param_count);
        sig
    }

    /// Record which value parameters are RECEIVER function types (`Recv.() -> R`, from
    /// `@Metadata`'s `@ExtensionFunctionType`): the receiver's type per parameter
    /// (`lambda_receivers`) and the plain flag (`lambda_receiver_params`). Each list is kept only
    /// when it aligns with `param_count`, so consumers can index positionally without
    /// re-validating arity.
    fn set_lambda_receiver_shape(
        &mut self,
        param_count: usize,
        lambda_receivers: Vec<Option<Ty>>,
        lambda_receiver_params: Vec<bool>,
    ) {
        self.lambda_receivers = vec_for_arity(lambda_receivers, param_count);
        self.lambda_receiver_params = vec_for_arity(lambda_receiver_params, param_count);
    }

    fn metadata_base(
        param_count: usize,
        names: Vec<String>,
        defaults: Vec<bool>,
        vararg_index: Option<usize>,
    ) -> Self {
        // Legacy context receivers are unnamed but precede ordinary value parameters. Retain their
        // empty slots so the named suffix stays positionally aligned; consumers already hide the
        // leading context slots from explicit argument mapping.
        let names = vec_for_arity(names, param_count);
        let defaults = vec_for_arity(defaults, param_count);
        let defaults = if defaults.iter().any(|d| *d) {
            defaults
        } else {
            Vec::new()
        };
        CallSig {
            required: required_arity(param_count, &defaults),
            param_names: names,
            param_defaults: defaults,
            vararg: vararg_index.is_some(),
            vararg_index,
            ..Default::default()
        }
    }
}

pub fn required_arity(param_count: usize, defaults: &[bool]) -> usize {
    if defaults.is_empty() {
        param_count
    } else {
        defaults.iter().filter(|d| !**d).count()
    }
}

fn vec_for_arity<T>(items: Vec<T>, param_count: usize) -> Vec<T> {
    if items.len() == param_count {
        items
    } else {
        Vec::new()
    }
}

#[derive(Clone, Copy, Default)]
pub struct ReturnInfo {
    pub nullable: bool,
    pub class: Option<Ty>,
}

impl ReturnInfo {
    pub fn new(nullable: bool, class: Option<Ty>) -> Self {
        ReturnInfo { nullable, class }
    }

    pub fn apply(self, fallback: Ty) -> Ty {
        self.apply_with_class(self.class, fallback)
    }

    pub fn apply_with_class(self, class: Option<Ty>, fallback: Ty) -> Ty {
        let ret = match class {
            Some(meta) if !fallback.type_args().is_empty() => {
                let specialized = Ty::obj_args(&meta.name(), fallback.type_args());
                if matches!(meta, Ty::PlatformNullable(_)) {
                    Ty::platform_nullable(specialized)
                } else {
                    specialized
                }
            }
            Some(meta) => meta,
            None => fallback,
        };
        if self.nullable && !ret.is_nullable() && (ret.boxed_ref().is_some() || ret.is_reference())
        {
            Ty::nullable(ret)
        } else {
            ret
        }
    }
}

/// One overload in a [`FunctionSet`]: the full platform-neutral shape of a single function the front end
/// needs, in ONE place — no follow-up metadata calls. `callable` is the opaque emit handle (the platform
/// emitter consumes it; the front end never inspects it).
#[derive(Clone)]
pub struct FunctionInfo {
    pub kind: FnKind,
    /// The extension/member receiver type; `None` for a top-level function.
    pub receiver: Option<Ty>,
    pub ret: ReturnInfo,
    /// `inline`, `@InlineOnly` (`inline_only`), and friends — from `@Metadata`.
    pub flags: FnFlags,
    /// The opaque platform callable (owner/name/descriptor on JVM) + its resolved `params`/`ret`. Reuses
    /// [`LibraryCallable`]; the front end reads `params`/`ret` and passes the whole thing to the emitter.
    pub callable: LibraryCallable,
    /// The callee's Kotlin visibility. The pre-context resolver treated non-`Public` as "no legal call
    /// site" (an `@InlineOnly` is included only when it will SPLICE); the context-aware `accessible(...)`
    /// gate refines that for `protected`/`internal`. Read `public()` for the legacy public-only predicate.
    pub visibility: Visibility,
    /// For a member or extension overload, the receiver-MRO rung it was found at (0 = the receiver's
    /// own type, increasing up the supertype chain). Top-level overloads use 0; `u32::MAX` marks a
    /// candidate that must never preempt a real rung.
    pub receiver_rank: u32,
    /// Provider-specific tie-break key within an otherwise applicable overload set. Lower is preferred.
    /// Consumers treat it as opaque selection data.
    pub overload_rank: u32,
    /// Parsed generic signature, if the provider has one. Carries type-variable binding facts with the
    /// overload instead of making consumers parse backend signature strings after selection.
    pub generic_sig: Option<GenericSig>,
    pub projected_return_hazard: bool,
    /// The source-level call shape (defaults, named params, lambda param types, vararg) the checker needs
    /// beyond the erased descriptor. `Default` (empty) when the source doesn't provide it.
    pub call_sig: CallSig,
    /// File-independent values for source defaults, parallel to the callable's value parameters.
    pub default_values: Vec<Option<DefaultValue>>,
    /// Number of leading context parameters in the logical parameter list.
    pub context_count: usize,
    /// Source declaration key for a callable from the current compilation module. Classpath callables
    /// leave this unset.
    pub source_key: Option<(u32, u32)>,
    /// Exact AST-backed member declaration selected from the current compilation. This is distinct
    /// from [`Self::source_key`], whose second component is a top-level declaration arena id; a member
    /// is owned by its classifier and is identified by its stable signature start.
    pub source_member: Option<SourceMember>,
    /// Language-defined callable contributed by the classifier itself rather than by its companion
    /// value. It travels on the same candidate structure so the checker can combine both facets and
    /// run overload selection once.
    pub implicit_classifier_callable: Option<ImplicitClassifierCallable>,
    /// Declaration-owned function packages used to resolve the iterator protocol inside this exact
    /// callable's compiler-provided inline body. Empty means that the callable has no synthesized
    /// iteration body. Providers attach this semantic capability to the ordinary candidate; callers'
    /// imports never participate in the body's convention lookup.
    pub iterator_protocol_scope: Vec<TypeName>,
}

impl FunctionInfo {
    pub fn is_extension(&self) -> bool {
        self.kind == FnKind::Extension
    }

    pub fn semantic_receiver(&self) -> Option<Ty> {
        self.generic_sig
            .as_ref()
            .and_then(|signature| signature.receiver)
            .or(self.receiver)
    }

    pub fn semantic_params(&self) -> &[Ty] {
        self.generic_sig.as_ref().map_or_else(
            || {
                if self.is_extension() {
                    self.callable.params.get(1..).unwrap_or(&[])
                } else {
                    &self.callable.params
                }
            },
            |signature| signature.params.as_slice(),
        )
    }

    /// Parameters written at the call site. Extension receivers and leading context parameters are
    /// supplied independently, so neither belongs in source argument mapping.
    pub fn value_params(&self) -> &[Ty] {
        let params = self.semantic_params();
        &params[self.context_count.min(params.len())..]
    }

    /// Parameters after overload selection has applied receiver, argument, and expected-result
    /// constraints. Raw declaration parameters remain available through [`Self::semantic_params`].
    pub fn applied_params(&self) -> &[Ty] {
        if self.is_extension() {
            self.callable.params.get(1..).unwrap_or(&[])
        } else {
            &self.callable.params
        }
    }

    pub fn semantic_signature(&self) -> Cow<'_, GenericSig> {
        self.generic_sig.as_ref().map_or_else(
            || {
                Cow::Owned(GenericSig {
                    formals: Vec::new(),
                    formal_bounds: Vec::new(),
                    receiver: self.receiver,
                    params: self.semantic_params().to_vec(),
                    ret: self.callable.ret,
                    return_policy: GenericReturnPolicy::Exact,
                })
            },
            Cow::Borrowed,
        )
    }

    pub fn plain(kind: FnKind, receiver: Option<Ty>, callable: LibraryCallable) -> Self {
        FunctionInfo {
            kind,
            receiver,
            ret: ReturnInfo::default(),
            flags: FnFlags::default(),
            callable,
            visibility: Visibility::Public,
            receiver_rank: 0,
            overload_rank: 0,
            generic_sig: None,
            projected_return_hazard: false,
            call_sig: CallSig::default(),
            default_values: Vec::new(),
            context_count: 0,
            source_key: None,
            source_member: None,
            implicit_classifier_callable: None,
            iterator_protocol_scope: Vec::new(),
        }
    }

    /// Normalize one callable exposed through a classifier (`Owner.name`) into the same candidate
    /// structure used by package, module, and lexical sources. The classifier record owns the source
    /// declaration facts; this conversion copies them once into the selected-call handle so import
    /// scope and qualified syntax cannot grow separate reconstruction paths.
    pub fn classifier_member(kind: FnKind, owner: TypeName, member: LibraryMember) -> Self {
        let physical_name = member
            .physical_name
            .clone()
            .unwrap_or_else(|| member.name.clone());
        let mut callable = LibraryCallable::library(
            member.owner.unwrap_or(owner),
            physical_name,
            member.params.clone(),
            member.ret,
            member.physical_ret,
            member.descriptor.clone(),
        );
        callable.signature = member.signature.clone();
        callable.generic_sig = member.generic_sig.clone().map(Box::new);
        callable.inline = member.inline;
        callable.inline_body_plan = member.inline_body_plan.clone();
        callable.suspend = member.suspend();
        callable.owner_is_interface = member.is_interface();
        callable.member_realization = member.realization;
        callable.default_realization = member.default_realization.clone();
        callable.declared_ret = member.declared_ret;
        callable.context_count = member.context_count;
        callable.contract = member.contract.clone();
        callable.plugin_expression = member.plugin_expression;

        let mut candidate = FunctionInfo::plain(kind, None, callable);
        candidate.ret = ReturnInfo::new(member.ret_nullable(), member.declared_ret);
        candidate.visibility = member.visibility;
        candidate.generic_sig = member.generic_sig.clone();
        candidate.call_sig = member.call_sig.clone();
        candidate.context_count = member.context_count;
        candidate.flags.inline = member.inline;
        candidate.flags.reified = member.reified;
        candidate.flags.suspend = member.suspend();
        candidate.flags.operator = member.is_operator();
        candidate.flags.infix = member.is_infix();
        candidate.flags.is_abstract = member.is_abstract();
        candidate.flags.low_priority = member.low_priority;
        candidate.default_values = member.default_values.clone();
        candidate.source_member = member.source_member;
        candidate.implicit_classifier_callable = member.implicit_classifier_callable;
        candidate
    }

    /// The legacy public-only accessibility predicate (`visibility == Public`) — what the resolver's
    /// pre-context filters used. The context-aware `accessible(...)` gate supersedes this per call site.
    pub fn public(&self) -> bool {
        self.visibility.is_public()
    }

    /// Materialize this selected overload as an instance-member emit handle with a caller-chosen logical
    /// return. Metadata flags that affect emission stay coupled to the selected overload.
    pub fn member_with_return(&self, ret: Ty) -> LibraryMember {
        let mut member = LibraryMember::new(
            self.callable.name.clone(),
            self.callable.params.clone(),
            ret,
            self.callable.descriptor.clone(),
        );
        member.physical_params = self.callable.physical_params.clone();
        member.owner = Some(self.callable.owner);
        member.physical_ret = self.callable.physical_ret;
        // Preserve the selected declaration's pre-substitution return when a generic `FunctionInfo`
        // is materialized as an instance-member emit handle. The logical `ret` above is deliberately
        // caller-specialized, so it cannot replace this fact: `Factory.invoke(): TokenBox<String>` and
        // `List<TokenBox<String>>.get()` may both specialize to `TokenBox<String>` and physically return
        // `Object`, while only the declaration says the former hands back an unboxed carrier. Dropping
        // the fact here makes every downstream consumer—including the operator-invoke path—guess from
        // indistinguishable substituted/physical types and unbox a real carrier as though it were a box.
        member.declared_ret = self.callable.declared_ret;
        member.signature = self.callable.signature.clone();
        member.default_values = self.default_values.clone();
        member.default_realization = self.callable.default_realization.clone();
        member.generic_sig = self.generic_sig.clone();
        member.inline = self.flags.inline;
        member.reified = self.flags.reified;
        member.visibility = self.visibility;
        member.set_suspend(self.flags.suspend);
        member.set_is_operator(self.flags.operator);
        member.set_is_infix(self.flags.infix);
        member.set_is_abstract(self.flags.is_abstract);
        // Interface-ness travels with the selected overload for the same reason `suspend` does: it is a
        // fact about the DECLARATION, and the emit site may have no way to re-derive it (a mapped
        // builtin's JVM owner has no class file on a JDK-less classpath). Round-tripping the member
        // through `FunctionInfo` must not lose it, or the call emits `invokevirtual` on an interface.
        member.set_is_interface(self.callable.owner_is_interface);
        member.realization = self.callable.member_realization;
        // Keep source call shape coupled to the selected overload.
        member.call_sig = self.call_sig.clone();
        member.context_count = self.context_count;
        member.low_priority = self.flags.low_priority;
        member.contract = self.callable.contract.clone();
        member.implicit_classifier_callable = self.implicit_classifier_callable;
        member.plugin_expression = self.callable.plugin_expression;
        member.source_member = self.source_member;
        member
    }
}

/// How a callable relates to bytecode inlining — the single state that replaces the old
/// `inline` + `inline_only`/`must_inline` boolean pairs (one per layer: [`FnFlags`],
/// [`LibraryCallable`], and `ir::Callee::Static`). Ordered weakest→strongest; the splice obligation
/// strengthens as you go down.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum InlineKind {
    /// Not an `inline` function — emit an ordinary call (`invokestatic`/`invokevirtual`).
    #[default]
    None,
    /// A Kotlin `inline` function (per its `@Metadata`): the JVM backend MAY splice its compiled body
    /// at the call site, but a real call is a legal fallback (the callee is a public method).
    CanInline,
    /// No legal direct-call fallback. This includes a NON-PUBLIC `@InlineOnly` function
    /// (`require`/`check`/`error`/`let`/…) whose method is inaccessible and a reified source body
    /// whose erased method may exist only to publish inline code. The backend MUST splice the body;
    /// a failed splice skips the whole file rather than emitting an inaccessible or unspecialized
    /// `invokestatic`.
    MustInline,
}

impl InlineKind {
    /// Build from the legacy `(inline, must_inline)` boolean pair. `must_inline` is the stronger
    /// signal (no callable fallback), so it wins regardless of the `inline` bit — which the `@Metadata`
    /// `inline` flag can read back as `false` for a `@JvmName`-mangled private callee even though it
    /// must still be spliced.
    pub fn from_flags(inline: bool, must_inline: bool) -> InlineKind {
        if must_inline {
            InlineKind::MustInline
        } else if inline {
            InlineKind::CanInline
        } else {
            InlineKind::None
        }
    }
    /// True when the backend may attempt to splice the body (`inline` OR `@InlineOnly`).
    pub fn can_inline(self) -> bool {
        self != InlineKind::None
    }
    /// True when splicing is mandatory — the callee has no legal call site to fall back to.
    pub fn must_inline(self) -> bool {
        self == InlineKind::MustInline
    }
}

/// Function metadata flags, decoded once from `@Metadata`.
#[derive(Clone, Copy, Default, Debug)]
pub struct FnFlags {
    /// `inline` / non-public `@InlineOnly` inline-ness, in one field (was `inline` + `inline_only`).
    pub inline: InlineKind,
    /// At least one callable type parameter carries Kotlin's `reified` modifier. This is distinct
    /// from [`InlineKind::MustInline`], which also covers non-reified `@InlineOnly` declarations.
    pub reified: bool,
    /// `suspend` — decoded from `@Metadata` (the `IS_SUSPEND` function flag). A call to a suspend
    /// function is a coroutine suspension point (the JVM lowering threads a `Continuation`).
    pub suspend: bool,
    /// Kotlin's `operator` modifier. Call conventions such as `receiver(args)` must filter on this
    /// semantic flag; JVM method names alone cannot distinguish an explicit `.invoke()` declaration.
    pub operator: bool,
    /// Kotlin's `infix` modifier. Infix syntax admits only declarations carrying this flag; an
    /// ordinary same-named member remains callable through dot syntax but does not shadow an infix
    /// extension.
    pub infix: bool,
    /// The declaration has no implementation. Ordinary virtual calls may select it because dispatch
    /// reaches a concrete override; a non-virtual `super` call must continue to another direct
    /// supertype instead.
    pub is_abstract: bool,
    /// `@LowPriorityInOverloadResolution`: discard this declaration whenever an ordinary candidate
    /// is applicable at the same callable-tower level.
    pub low_priority: bool,
}

/// All overloads of one function name applicable to a call — members AND extensions AND top-level, in one
/// query, each tagged with its [`FnKind`] so the caller applies Kotlin precedence and picks (e.g. by the
/// lambda's return type for `@OverloadResolutionByLambdaReturnType`). The consolidation that replaces the
/// scattered callable / `is_inline` / return-overload / nullable lookups.
#[derive(Clone, Default)]
pub struct FunctionSet {
    pub overloads: Vec<FunctionInfo>,
}

impl FunctionSet {
    pub fn top_level(&self) -> impl Iterator<Item = &FunctionInfo> {
        self.overloads.iter().filter(|o| o.kind == FnKind::TopLevel)
    }

    pub fn into_top_level(self) -> impl Iterator<Item = FunctionInfo> {
        self.overloads
            .into_iter()
            .filter(|o| o.kind == FnKind::TopLevel)
    }

    pub fn into_single_top_level(self) -> Option<FunctionInfo> {
        let mut top_level = self.into_top_level();
        top_level.next().filter(|_| top_level.next().is_none())
    }

    pub fn into_top_level_with_param_names(self) -> impl Iterator<Item = FunctionInfo> {
        self.into_top_level()
            .filter(|o| o.call_sig.has_param_names())
    }

    pub fn has_top_level_arity(&self, arity: usize) -> bool {
        self.top_level()
            .any(|overload| overload.semantic_params().len() == arity)
    }
}

/// How a resolved PROPERTY relates to the access's receiver — the property analogue of [`FnKind`]
/// (member wins over extension; a top-level property has no receiver).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PropKind {
    /// A property of the receiver's type (or an inherited one).
    Member,
    /// An extension property on the receiver's type.
    Extension,
    /// A receiver-less top-level property.
    TopLevel,
}

/// One property declaration a source exposes, arg-independent — the property analogue of
/// [`FunctionInfo`], so a resolver can query properties symmetrically with `functions`.
///
/// The type is carried as a Kotlin-level [`Ty`] (a type variable is a [`Ty::TyParam`]): the resolver
/// reads the Kotlin type; erasure to a descriptor happens only at the emit boundary, inside the opaque
/// accessor [`LibraryCallable`]s.
#[derive(Clone, Debug)]
pub struct PropertyInfo {
    /// Declared Kotlin property name. Lookup spelling may be an import alias and accessor names are
    /// physical call targets, so neither can recover this identity after selection.
    pub name: String,
    pub kind: PropKind,
    /// The extension/member receiver type; `None` for a top-level property.
    pub receiver: Option<Ty>,
    /// The property's own formal type parameters (`val <T> List<T>.foo`); empty for a plain property.
    pub formals: Vec<String>,
    /// The property's declared type.
    pub ty: Ty,
    /// Number of leading context parameters in each accessor's parameter list.
    pub context_count: usize,
    /// Source names parallel to the context parameters, retained for diagnostics.
    pub context_param_names: Vec<String>,
    /// The real getter — an opaque platform emit handle (the erased descriptor lives here).
    pub getter: LibraryCallable,
    /// The setter, present iff the property is a `var`.
    pub setter: Option<LibraryCallable>,
    /// The setter's own visibility. Accessors can differ, so property visibility cannot stand in for
    /// this fact.
    pub setter_visibility: Visibility,
    /// `const val` — a compile-time constant whose value use sites inline.
    pub is_const: bool,
    /// The property's Kotlin visibility.
    pub visibility: Visibility,
    /// The declaring type's internal name — for the resolver's access check (`protected`/`private`).
    pub owner: TypeName,
    /// For an [`PropKind::Extension`], the receiver-MRO rung it was found at (0 = the receiver's own
    /// type); `0` for member/top-level. Mirrors [`FunctionInfo::receiver_rank`].
    pub receiver_rank: u32,
    /// Source declaration key for a property from the current compilation module.
    pub source_key: Option<(u32, u32)>,
    /// Exact AST-backed member property from the current compilation module.
    pub source_member: Option<SourceMember>,
}

/// ALL properties of one name applicable to an access — members AND extensions AND top-level, in one
/// query, each tagged with its [`PropKind`]. The property analogue of [`FunctionSet`].
#[derive(Clone, Default)]
pub struct PropertySet {
    pub overloads: Vec<PropertyInfo>,
}

impl PropertyInfo {
    pub fn owner_name(&self) -> String {
        self.owner.render()
    }

    pub fn owner_name_or(&self, fallback: &str) -> String {
        let rendered = self.owner.render();
        if rendered.is_empty() {
            fallback.to_string()
        } else {
            rendered
        }
    }

    pub fn owner_type_or(&self, fallback: TypeName) -> TypeName {
        if self.owner.matches("") {
            fallback
        } else {
            self.owner
        }
    }
}

/// The callable half of a [`ResolvedSymbols`].
#[derive(Clone, Default)]
pub enum Callables {
    #[default]
    None,
    Functions(FunctionSet),
    Properties(PropertySet),
    Both {
        functions: FunctionSet,
        properties: PropertySet,
    },
}

impl Callables {
    pub fn from_parts(functions: FunctionSet, properties: PropertySet) -> Self {
        match (
            functions.overloads.is_empty(),
            properties.overloads.is_empty(),
        ) {
            (true, true) => Self::None,
            (false, true) => Self::Functions(functions),
            (true, false) => Self::Properties(properties),
            (false, false) => Self::Both {
                functions,
                properties,
            },
        }
    }

    pub fn into_parts(self) -> (FunctionSet, PropertySet) {
        match self {
            Self::None => (FunctionSet::default(), PropertySet::default()),
            Self::Functions(functions) => (functions, PropertySet::default()),
            Self::Properties(properties) => (FunctionSet::default(), properties),
            Self::Both {
                functions,
                properties,
            } => (functions, properties),
        }
    }

    pub fn functions(&self) -> &[FunctionInfo] {
        match self {
            Self::Functions(functions) | Self::Both { functions, .. } => &functions.overloads,
            Self::None | Self::Properties(_) => &[],
        }
    }

    pub fn properties(&self) -> &[PropertyInfo] {
        match self {
            Self::Properties(properties) | Self::Both { properties, .. } => &properties.overloads,
            Self::None | Self::Functions(_) => &[],
        }
    }
}

/// What a fully-qualified name resolves to in a [`crate::symbol_source::SymbolSource`] — the
/// platform-neutral namespace record (the spec's top-level memo value). Kotlin has TWO namespaces
/// (classifier vs callable) and one name can occupy both at once, so this is a RECORD: the `classifier`
/// (at most one) AND the `callables`. The resolver forms candidate FQNs from the import scope, queries
/// `resolve_symbols` per fqn, and selects by syntactic position (type → classifier; call → callables ∪
/// the classifier's constructors, then property-`invoke` fallback; value → property / object).
#[derive(Clone, Default)]
pub struct ResolvedSymbols {
    /// Resolved classifier identity. This is carried by the record instead of reconstructed from the
    /// queried spelling: a lexical scope may expose `Local` whose declaration identity is
    /// `pkg/Owner$Local`, and a typealias spelling denotes its target. Providers and scopes therefore
    /// return exactly the same record and the selection loop does not need an origin-specific branch.
    pub classifier_name: Option<TypeName>,
    /// Shared with the type-name memo, so cloning a record never deep-clones the classifier.
    pub classifier: Option<std::sync::Arc<LibraryType>>,
    pub callables: Callables,
}

impl ResolvedSymbols {
    /// Nothing resolves this name (both namespaces empty).
    pub fn is_empty(&self) -> bool {
        self.classifier_name.is_none()
            && self.classifier.is_none()
            && matches!(self.callables, Callables::None)
    }
}

/// The shape of a library type: enough for the front end to resolve member accesses against it
/// (publicness, kind, supertypes, constructors, instance members, and companion members) without
/// knowing the target ABI.
#[derive(Clone)]
pub struct LibraryType {
    pub access: ClassifierAccess,
    /// Source file that declares this classifier in the current compilation module. Dependency and
    /// platform classifiers leave it unset. Core accessibility uses this for top-level `private`.
    pub source_file: Option<u32>,
    /// True when this classifier is structurally declared inside another classifier. Core inherited
    /// nested-class lookup uses this instead of asking a provider to repeat the lookup policy.
    pub is_nested: bool,
    /// Enclosing instance captured by an `inner` classifier. A plain nested classifier has
    /// `is_nested = true` and no outer instance; constructor-reference shape prepends this type only
    /// when the declaration actually captures it.
    pub outer_instance: Option<TypeName>,
    /// The declaration kind (class / interface / annotation / object). One field instead of parallel
    /// booleans — read it through the `is_*` accessors, which encode the JVM reality that an annotation
    /// is also an interface.
    pub kind: TypeKind,
    /// Modality and construction facts of this declaration.
    pub inheritance: ClassifierInheritance,
    /// Internal names of the superclass + implemented interfaces (for the inherited-member walk).
    pub supertypes: TypeNameList,
    /// Direct supertype signatures with the classifier's own type parameters still symbolic. Core
    /// substitutes an applied receiver into these templates before its single hierarchy BFS.
    pub supertype_templates: Vec<Ty>,
    pub constructors: Vec<LibraryMember>,
    /// Field declarations owned by this classifier. Selection is deliberately not performed by the
    /// provider: the resolver walks these together with properties and supertypes, so source, module,
    /// and compiled classifiers obey one hiding and precedence rule.
    pub fields: Vec<LibraryField>,
    /// Exact source-level callable/property declarations keyed by source name. Providers populate this
    /// once with the classifier signature; core applies receiver type arguments and walks inheritance.
    pub declared_callables: HashMap<String, Callables>,
    /// Instance members (member functions and property accessors).
    pub members: Vec<LibraryMember>,
    /// Companion-object members — accessed as `Type.member(…)` (the JVM realizes these as statics).
    pub companion: Vec<LibraryMember>,
    /// Compile-time constants declared by this classifier. For `Int.Companion.MAX_VALUE`, this map is
    /// on the `Int.Companion` classifier; the outer classifier merely points at that companion.
    pub constants: HashMap<String, LibraryConst>,
    /// The single abstract method when this type is a functional interface. None for ordinary classes,
    /// non-SAM interfaces, and sources that do not provide SAM metadata.
    pub sam_method: Option<LibraryMember>,
    /// Function signature implemented by values of this classifier. This is classifier metadata, not
    /// a convention inferred from its name; core substitutes the classifier's applied type arguments.
    pub callable_signature: Option<Ty>,
    /// The companion-object INSTANCE, if this class has one: `(field_name, companion_type_internal)`.
    /// A Kotlin `class C { companion object [Name] }` compiles to a `public static final C$Name`
    /// field on `C` (default name `Companion`, e.g. `Json.Default: Json$Default`). A bare reference to
    /// `C` in value position is that companion instance — `getstatic C.field:LcompanionType;`. Lets the
    /// resolver resolve `Json.encodeToString(…)` (an instance method on the companion's type).
    pub companion_object: Option<(String, TypeName)>,
    /// Public inline companion functions on a classpath value class whose bytecode method is private but
    /// callable per metadata (`Result.success`). Lowering loads the companion object and splices the
    /// method body; ordinary companion members stay in `companion`.
    pub value_companion_fns: Vec<CompanionFn>,
    /// For a classpath `@JvmInline value class`, the erased underlying type it represents on the JVM
    /// (`UInt` → `Int`, `Result` → `Any`); `None` for an ordinary class. The JVM backend erases the value
    /// class to this everywhere (like a user value class), reproducing kotlinc's unboxed representation.
    pub value_underlying: Option<Ty>,
    /// Source name of a value class's sole underlying property. Kept beside its underlying type because
    /// together they are the complete semantic value-class shape used by the JVM representation pass.
    pub value_underlying_property: Option<String>,
    /// When this name is a `typealias`, the target internal it expands to (`kotlin/collections/ArrayList`
    /// → `java/util/ArrayList`); `None` for a real type. Name resolution records the target, so an alias
    /// resolves to the underlying type with no separate alias query.
    pub alias_target: Option<TypeName>,
    /// The type's own formal type parameters, in declaration order (`Pair` → `["A", "B"]`); empty for a
    /// non-generic type. With the constructors' [`LibraryMember::generic_sig`], lets a caller infer a
    /// construction's type arguments by unifying the ctor's generic parameter signatures against the
    /// actual argument types.
    pub type_parameters: TypeParameters<Vec<Vec<Ty>>>,
    /// Declared upper bounds parallel to [`Self::type_params`]. Star projections are expanded from
    /// this classifier metadata (`Box<*>` for `Box<T : CharSequence>` reads as the bound), never from
    /// a provider-specific query or an unconditional `Any?` fallback.
    /// The direct subclasses (JVM internal names) of a `sealed` type, from its `@Metadata`; empty for a
    /// non-sealed type. Lets an exhaustive `when` over a classpath sealed subject be proven exhaustive.
    pub sealed_subclasses: TypeNameList,
    /// The enum entry names this type declares (`Kind` → `["PENDING", "DONE"]`); empty for a non-enum.
    /// Lets `EnumName.ENTRY` resolve for a classpath enum as it does for a source enum.
    pub enum_entries: Vec<String>,
    /// Exact physical realization of Kotlin's synthetic `EnumType.entries` property, when this symbol
    /// provider exposes a direct accessor. This is deliberately separate from [`Self::companion`]:
    /// the accessor is not a source-callable `getEntries()` function, and keeping a dedicated fact
    /// prevents consumers from rediscovering it by provider-specific names or descriptors.
    pub enum_entries_accessor: Option<LibraryMember>,
    /// Provider-normalized named application parameter lists. Constructor declarations use
    /// [`ParamList::annotation`] = `None`; a declaration format without a constructor can publish
    /// its annotation element list with the application policy attached.
    pub named_parameter_lists: Vec<ParamList>,
    /// For a classpath annotation type: the `java.lang.annotation.RetentionPolicy` constant name of its
    /// `@Retention` (`"RUNTIME"` / `"CLASS"` / `"SOURCE"`), or `None` if absent. Drives whether a use of
    /// the annotation is emitted `RuntimeVisibleAnnotations` (RUNTIME) / `RuntimeInvisibleAnnotations`
    /// (CLASS = Kotlin BINARY) / dropped (SOURCE).
    pub retention: Option<String>,
}

impl std::ops::Deref for LibraryType {
    type Target = TypeParameters<Vec<Vec<Ty>>>;

    fn deref(&self) -> &Self::Target {
        &self.type_parameters
    }
}

impl LibraryType {
    pub fn type_params(&self) -> &Vec<String> {
        &self.type_parameters.type_params
    }

    pub fn type_param_bounds(&self) -> &Vec<Vec<Ty>> {
        &self.type_parameters.type_param_bounds
    }

    pub fn type_param_variances(&self) -> &Vec<TypeVariance> {
        &self.type_parameters.type_param_variances
    }
}

/// What a library type *is*. Mutually exclusive at the source level; at the JVM level an `Annotation`
/// also carries `ACC_INTERFACE`, which `is_interface()` reflects.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TypeKind {
    Class,
    Interface,
    Annotation,
    Enum,
    /// A Kotlin `object` (singleton) — has a `public static final INSTANCE` field of its own type, read
    /// as `getstatic <Type>.INSTANCE` when the object is referenced as a value.
    Object,
}

impl LibraryType {
    /// A declaration header used before a provider has constructed the full classifier signature.
    /// It commits only classifier existence; later phases replace it with their ordinary immutable
    /// record. Keeping even this bootstrap fact in the classifier API avoids a parallel existence
    /// query whose answer could disagree with the record.
    pub fn declaration_header() -> Self {
        Self {
            access: ClassifierAccess::Public,
            source_file: None,
            is_nested: false,
            outer_instance: None,
            kind: TypeKind::Class,
            inheritance: Default::default(),
            supertypes: TypeNameList::new(),
            supertype_templates: Vec::new(),
            constructors: Vec::new(),
            fields: Vec::new(),
            declared_callables: HashMap::new(),
            members: Vec::new(),
            companion: Vec::new(),
            constants: HashMap::new(),
            sam_method: None,
            callable_signature: None,
            companion_object: None,
            value_companion_fns: Vec::new(),
            value_underlying: None,
            value_underlying_property: None,
            alias_target: None,
            type_parameters: crate::types::TypeParameters::default(),
            sealed_subclasses: TypeNameList::new(),
            enum_entries: Vec::new(),
            enum_entries_accessor: None,
            named_parameter_lists: Vec::new(),
            retention: None,
        }
    }

    pub fn is_public(&self) -> bool {
        self.access == ClassifierAccess::Public
    }
    pub fn is_interface(&self) -> bool {
        matches!(self.kind, TypeKind::Interface | TypeKind::Annotation)
    }
    pub fn is_annotation(&self) -> bool {
        self.kind == TypeKind::Annotation
    }
    pub fn is_object(&self) -> bool {
        self.kind == TypeKind::Object
    }
    pub fn is_enum(&self) -> bool {
        self.kind == TypeKind::Enum
    }

    /// Classifier-callable overloads visible as `Type.name(...)`. Besides declarations supplied by
    /// the symbol source, every concrete enum receives Kotlin's two implicit functions. Keeping the
    /// contribution here makes module and dependency classifiers expose one structure and keeps the
    /// resolver out of language-declaration synthesis.
    pub fn classifier_callables(&self, owner: TypeName) -> Vec<LibraryMember> {
        let mut callables = self.companion.clone();
        if !self.is_enum() {
            return callables;
        }

        let mut values = LibraryMember::new(
            "values".to_string(),
            Vec::new(),
            Ty::array(Ty::obj_name(owner)),
            String::new(),
        );
        values.owner = Some(owner);
        values.implicit_classifier_callable = Some(ImplicitClassifierCallable::EnumValues);

        let mut value_of = LibraryMember::new(
            "valueOf".to_string(),
            vec![Ty::String],
            Ty::obj_name(owner),
            String::new(),
        );
        value_of.owner = Some(owner);
        value_of.implicit_classifier_callable = Some(ImplicitClassifierCallable::EnumValueOf);

        for implicit in [values, value_of] {
            if let Some(physical) = callables.iter_mut().find(|member| {
                member.name == implicit.name
                    && member.params == implicit.params
                    && member.ret == implicit.ret
            }) {
                physical.implicit_classifier_callable = implicit.implicit_classifier_callable;
            } else {
                callables.push(implicit);
            }
        }
        callables
    }

    /// Language-defined properties visible on the classifier itself. These are deliberately not
    /// inserted into instance or companion members: `EnumType.entries` has a classifier receiver,
    /// and `EnumType::entries` is consequently a bound zero-argument property reference.
    pub fn classifier_property(&self, owner: TypeName, name: &str) -> Option<ClassifierProperty> {
        (self.is_enum() && name == "entries").then(|| ClassifierProperty {
            ty: Ty::obj_args("kotlin/enums/EnumEntries", &[Ty::obj_name(owner)]),
            getter: self.enum_entries_accessor.clone(),
        })
    }

    /// Whether an enum entry named `name` is declared on this type — lets `EnumName.ENTRY` resolve.
    pub fn is_enum_entry(&self, name: &str) -> bool {
        self.enum_entries.iter().any(|e| e == name)
    }

    /// Constructor source parameter names/default flags for a named call with `min_arity` supplied args.
    pub fn constructor_named_params(&self, min_arity: usize) -> Option<ParamList> {
        self.named_parameter_lists
            .iter()
            .find(|params| {
                params.annotation.is_none()
                    && params.names.len() >= min_arity
                    && params.names.len() == params.defaults.len()
                    && (params.types.is_empty() || params.names.len() == params.types.len())
                    && !params.names.iter().any(String::is_empty)
            })
            .cloned()
    }
}

impl LibraryType {
    /// Complete annotation application shape normalized at the declaration-provider boundary.
    /// Kotlin annotation constructors already carry the ordinary positional/default/vararg facts;
    /// providers for constructor-less declaration formats attach an explicit annotation policy.
    pub fn annotation_application(&self) -> Option<AnnotationApplication> {
        if !self.is_annotation() {
            return None;
        }
        let parameters = if let Some(parameters) = self
            .named_parameter_lists
            .iter()
            .find(|parameters| parameters.annotation.is_some())
        {
            if parameters.names.len() != parameters.defaults.len()
                || parameters.names.len() != parameters.types.len()
                || parameters.names.iter().any(String::is_empty)
                || parameters.types.contains(&Ty::Error)
            {
                return None;
            }
            parameters.clone()
        } else {
            self.constructor_named_params(0)?
        };
        let policy = parameters.annotation.unwrap_or(AnnotationParameterPolicy {
            positional: AnnotationPositionalPolicy::Constructor,
            materialize_omitted_vararg: parameters.vararg.is_some(),
        });
        Some(AnnotationApplication { parameters, policy })
    }
}

/// A library field's compile-time constant.
#[derive(Clone, Debug, PartialEq)]
pub enum LibConst {
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    /// A platform-neutral Kotlin string constant. The semantic library boundary carries UTF-16
    /// units so a classpath provider cannot corrupt values that Rust `String` cannot represent.
    Str(crate::kt_string::KtString),
}

#[derive(Clone, Debug, PartialEq)]
pub struct LibraryConst {
    pub ty: Ty,
    pub value: LibConst,
}

/// A symbol source with no external libraries — compiling a self-contained source set with no
/// classpath. Kotlin's language-level builtin classifiers still exist: this provider publishes their
/// common hierarchy and `Any` declarations so the resolver never needs a builtin-name fallback.
pub struct EmptySymbolSource;

pub(crate) fn add_core_builtin_declarations(classifier: &mut LibraryType, owner: TypeName) {
    fn member_property(
        classifier: &mut LibraryType,
        owner: TypeName,
        name: &str,
        ty: Ty,
        intrinsic: CompilerIntrinsic,
    ) {
        if let Some(callables) = classifier.declared_callables.get_mut(name) {
            let (functions, mut properties) = std::mem::take(callables).into_parts();
            if !properties.overloads.is_empty() {
                for property in &mut properties.overloads {
                    property.getter.compiler_intrinsic = Some(intrinsic);
                }
                *callables = Callables::from_parts(functions, properties);
                return;
            }
            *callables = Callables::from_parts(functions, properties);
        }
        let mut getter = LibraryCallable::library(owner, name, Vec::new(), ty, ty, "");
        getter.compiler_intrinsic = Some(intrinsic);
        let property = PropertyInfo {
            name: name.to_string(),
            kind: PropKind::Member,
            receiver: Some(Ty::obj_name(owner)),
            formals: Vec::new(),
            ty,
            context_count: 0,
            context_param_names: Vec::new(),
            getter,
            setter: None,
            setter_visibility: Visibility::Private,
            is_const: false,
            visibility: Visibility::Public,
            owner,
            receiver_rank: 0,
            source_key: None,
            source_member: None,
        };
        classifier
            .declared_callables
            .entry(name.to_string())
            .and_modify(|callables| {
                let (functions, mut properties) = std::mem::take(callables).into_parts();
                properties.overloads.push(property.clone());
                *callables = Callables::from_parts(functions, properties);
            })
            .or_insert_with(|| {
                Callables::Properties(PropertySet {
                    overloads: vec![property],
                })
            });
    }

    if owner.matches("kotlin/String") {
        member_property(
            classifier,
            owner,
            "length",
            Ty::Int,
            CompilerIntrinsic::StringLength,
        );
    }
    if owner.matches("kotlin/Array") || Ty::primitive_array_element(owner.segment_ref()).is_some() {
        member_property(
            classifier,
            owner,
            "size",
            Ty::Int,
            CompilerIntrinsic::ArraySize,
        );
    }
}

impl EmptySymbolSource {
    fn builtin_classifier(name: &str, internal: TypeName) -> Option<LibraryType> {
        let known = Ty::from_name(name).is_some()
            || Ty::primitive_array_element(name).is_some()
            || matches!(name, "Array" | "Enum" | "Function");
        if !known {
            return None;
        }

        let mut classifier = LibraryType::declaration_header();
        if name == "Function" {
            classifier.kind = TypeKind::Interface;
            classifier.type_parameters =
                TypeParameters::invariant(vec!["R".to_string()], vec![Vec::new()]);
        }
        if name == "Enum" {
            // `enum class E` has the implicit semantic supertype `Enum<E>` even with an empty
            // target classpath. Publish the corresponding core declaration through the same symbol
            // provider as the other language builtins, so checked IR never needs an emitter-only
            // classifier exception merely to format that already-recorded type.
            classifier.type_parameters =
                TypeParameters::invariant(vec!["E".to_string()], vec![Vec::new()]);
        }
        if name == "Unit" {
            classifier.kind = TypeKind::Object;
        }
        if name != "Any" && name != "Nothing" {
            classifier.supertypes.push_name(crate::types::wk::any());
            classifier
                .supertype_templates
                .push(Ty::obj_name(crate::types::wk::any()));
        }
        if name == "Nothing" {
            // `Nothing` has no instances, but its expressions have every non-null type as a
            // supertype; `Any` is the only declaration owner needed for member lookup here.
            classifier.supertypes.push_name(crate::types::wk::any());
            classifier
                .supertype_templates
                .push(Ty::obj_name(crate::types::wk::any()));
        }
        if name == "Any" {
            classifier.constructors.push(LibraryMember::new(
                "<init>".to_string(),
                Vec::new(),
                Ty::Unit,
                "()V".to_string(),
            ));
            let declarations = [
                (
                    "equals",
                    vec![Ty::nullable(Ty::obj_name(internal))],
                    Ty::Boolean,
                ),
                ("hashCode", Vec::new(), Ty::Int),
                ("toString", Vec::new(), Ty::String),
            ];
            for (name, params, ret) in declarations {
                let callable = LibraryCallable::library(internal, name, params, ret, ret, "");
                classifier.declared_callables.insert(
                    name.to_string(),
                    Callables::Functions(FunctionSet {
                        overloads: vec![FunctionInfo::plain(FnKind::Member, None, callable)],
                    }),
                );
            }
        }
        add_core_builtin_declarations(&mut classifier, internal);
        Some(classifier)
    }

    fn builtin_char_property(name: &str) -> Callables {
        if name != "code" {
            return Callables::None;
        }
        let owner = crate::types::type_name("kotlin/CharKt");
        let mut getter =
            LibraryCallable::library(owner, "getCode", vec![Ty::Char], Ty::Int, Ty::Int, "");
        getter.compiler_intrinsic = Some(CompilerIntrinsic::CharCode);
        Callables::Properties(PropertySet {
            overloads: vec![PropertyInfo {
                name: name.to_string(),
                kind: PropKind::Extension,
                receiver: Some(Ty::Char),
                formals: Vec::new(),
                ty: Ty::Int,
                context_count: 0,
                context_param_names: Vec::new(),
                getter,
                setter: None,
                setter_visibility: Visibility::Private,
                is_const: false,
                visibility: Visibility::Public,
                owner,
                receiver_rank: 0,
                source_key: None,
                source_member: None,
            }],
        })
    }

    fn builtin_text_callables(name: &str) -> Callables {
        let signatures: &[(&[Ty], Ty)] = match name {
            "substring" => &[(&[Ty::Int], Ty::String), (&[Ty::Int, Ty::Int], Ty::String)],
            "indexOf" => &[(&[Ty::String], Ty::Int)],
            "trimIndent" | "trimMargin" => &[(&[], Ty::String)],
            _ => return Callables::None,
        };
        let receiver = Ty::String;
        let owner = crate::types::type_name("kotlin/text/StringsKt");
        let overloads = signatures
            .iter()
            .map(|(params, ret)| {
                let mut callable = LibraryCallable::library(
                    owner,
                    name,
                    std::iter::once(receiver)
                        .chain(params.iter().copied())
                        .collect(),
                    *ret,
                    *ret,
                    "",
                );
                callable.compiler_intrinsic = match name {
                    "trimIndent" => Some(CompilerIntrinsic::TrimIndent),
                    "trimMargin" => Some(CompilerIntrinsic::TrimMargin),
                    _ => None,
                };
                FunctionInfo::plain(FnKind::Extension, Some(receiver), callable)
            })
            .collect();
        Callables::Functions(FunctionSet { overloads })
    }

    fn builtin_console_callables(name: &str) -> Callables {
        let arities: &[usize] = match name {
            "print" => &[1],
            "println" => &[0, 1],
            _ => return Callables::None,
        };
        let owner = crate::types::type_name("kotlin/io/ConsoleKt");
        let intrinsic = if name == "print" {
            CompilerIntrinsic::Print
        } else {
            CompilerIntrinsic::Println
        };
        let message = Ty::nullable(Ty::obj_name(crate::types::wk::any()));
        let overloads = arities
            .iter()
            .map(|arity| {
                let params = if *arity == 0 {
                    Vec::new()
                } else {
                    vec![message]
                };
                let mut callable =
                    LibraryCallable::library(owner, name, params, Ty::Unit, Ty::Unit, "");
                callable.compiler_intrinsic = Some(intrinsic);
                FunctionInfo::plain(FnKind::TopLevel, None, callable)
            })
            .collect();
        Callables::Functions(FunctionSet { overloads })
    }

    fn builtin_coroutine_intrinsic_callables(name: &str) -> Callables {
        if name != "COROUTINE_SUSPENDED" {
            return Callables::None;
        }
        let ty = Ty::obj_name(crate::types::wk::any());
        let mut getter = LibraryCallable::library(
            crate::types::type_name("kotlin/coroutines/intrinsics/IntrinsicsKt"),
            "getCOROUTINE_SUSPENDED",
            Vec::new(),
            ty,
            ty,
            "()Ljava/lang/Object;",
        );
        getter.compiler_intrinsic = Some(CompilerIntrinsic::CoroutineSuspended);
        Callables::Properties(PropertySet {
            overloads: vec![PropertyInfo {
                name: name.to_string(),
                kind: PropKind::TopLevel,
                receiver: None,
                formals: Vec::new(),
                ty,
                context_count: 0,
                context_param_names: Vec::new(),
                getter,
                setter: None,
                setter_visibility: Visibility::Private,
                is_const: false,
                visibility: Visibility::Public,
                owner: crate::types::type_name("kotlin/coroutines/intrinsics/IntrinsicsKt"),
                receiver_rank: 0,
                source_key: None,
                source_member: None,
            }],
        })
    }
}

impl crate::symbol_source::SymbolSource for EmptySymbolSource {
    fn package_exists(&self, parent: TypeName, name: &str) -> bool {
        (parent == TypeName::ROOT && name == "kotlin")
            || (parent.matches("kotlin") && matches!(name, "coroutines" | "io" | "text"))
            || (parent.matches("kotlin/coroutines") && name == "intrinsics")
    }

    fn symbols(
        &self,
        namespace: crate::symbol_source::SymbolNamespace,
        name: &str,
    ) -> std::rc::Rc<ResolvedSymbols> {
        let crate::symbol_source::SymbolNamespace::Package(package) = namespace else {
            return std::rc::Rc::new(ResolvedSymbols::default());
        };
        if package.matches("kotlin/text") {
            return std::rc::Rc::new(ResolvedSymbols {
                callables: Self::builtin_text_callables(name),
                ..ResolvedSymbols::default()
            });
        }
        if package.matches("kotlin/io") {
            return std::rc::Rc::new(ResolvedSymbols {
                callables: Self::builtin_console_callables(name),
                ..ResolvedSymbols::default()
            });
        }
        if package.matches("kotlin/coroutines/intrinsics") {
            return std::rc::Rc::new(ResolvedSymbols {
                callables: Self::builtin_coroutine_intrinsic_callables(name),
                ..ResolvedSymbols::default()
            });
        }
        if !package.matches("kotlin") {
            return std::rc::Rc::new(ResolvedSymbols::default());
        }
        let callables = Self::builtin_char_property(name);
        let Some(internal) = namespace.existing_classifier(name) else {
            return std::rc::Rc::new(ResolvedSymbols {
                callables,
                ..ResolvedSymbols::default()
            });
        };
        let Some(classifier) = Self::builtin_classifier(name, internal) else {
            return std::rc::Rc::new(ResolvedSymbols {
                callables,
                ..ResolvedSymbols::default()
            });
        };
        std::rc::Rc::new(ResolvedSymbols {
            classifier_name: Some(internal),
            classifier: Some(std::sync::Arc::new(classifier)),
            callables,
        })
    }
}
impl SemanticPlatform for EmptySymbolSource {}

#[cfg(test)]
mod tests {
    use super::{
        map_call_args, AnnotationApplication, AnnotationParameterPolicy,
        AnnotationPositionalPolicy, CallSig, InlineKind, ParamList, TypeKind, Visibility,
    };
    use crate::types::Ty;

    #[test]
    fn empty_platform_publishes_the_implicit_enum_supertype_declaration() {
        let classifier = crate::symbol_source::SymbolSource::classifier(
            &super::EmptySymbolSource,
            crate::types::type_name("kotlin/Enum"),
        )
        .expect("Enum is a core classifier even without a target classpath");
        assert_eq!(classifier.type_params(), &["E"]);
        assert_eq!(
            classifier.type_param_variances(),
            &[crate::types::TypeVariance::Invariant]
        );
    }

    #[test]
    fn visibility_from_metadata_maps_the_kotlin_enum() {
        // kotlin-metadata Flags.VISIBILITY order: INTERNAL=0, PRIVATE=1, PROTECTED=2, PUBLIC=3,
        // PRIVATE_TO_THIS=4, LOCAL=5. Everything past PUBLIC folds conservatively to Private.
        assert_eq!(Visibility::from_metadata(0), Visibility::Internal);
        assert_eq!(Visibility::from_metadata(1), Visibility::Private);
        assert_eq!(Visibility::from_metadata(2), Visibility::Protected);
        assert_eq!(Visibility::from_metadata(3), Visibility::Public);
        assert_eq!(Visibility::from_metadata(4), Visibility::Private); // PRIVATE_TO_THIS
        assert_eq!(Visibility::from_metadata(5), Visibility::Private); // LOCAL → never widens
    }

    #[test]
    fn visibility_is_public_matches_the_old_bool() {
        // The pre-context filters used `is_public`; only Public satisfies it.
        assert!(Visibility::Public.is_public());
        assert!(!Visibility::Internal.is_public());
        assert!(!Visibility::Protected.is_public());
        assert!(!Visibility::Private.is_public());
        // `from_public` round-trips the coarse bool (protected can't occur on its callers).
        assert!(Visibility::from_public(true).is_public());
        assert!(!Visibility::from_public(false).is_public());
    }

    #[test]
    fn inline_kind_from_flags_collapses_the_pair() {
        // (inline, must_inline) → the single ordered state.
        assert_eq!(InlineKind::from_flags(false, false), InlineKind::None);
        assert_eq!(InlineKind::from_flags(true, false), InlineKind::CanInline);
        assert_eq!(InlineKind::from_flags(true, true), InlineKind::MustInline);
        // `must_inline` wins even when the metadata `inline` bit read back false (a `@JvmName`-mangled
        // private `@InlineOnly` callee): it must still be spliced.
        assert_eq!(InlineKind::from_flags(false, true), InlineKind::MustInline);
    }

    #[test]
    fn inline_kind_accessors_match_the_old_bools() {
        // `can_inline()` == old `inline || must_inline`; `must_inline()` == old `must_inline`.
        assert!(!InlineKind::None.can_inline());
        assert!(!InlineKind::None.must_inline());
        assert!(InlineKind::CanInline.can_inline());
        assert!(!InlineKind::CanInline.must_inline());
        assert!(InlineKind::MustInline.can_inline());
        assert!(InlineKind::MustInline.must_inline());
    }

    #[test]
    fn inline_kind_default_is_none() {
        assert_eq!(InlineKind::default(), InlineKind::None);
    }

    fn ty_with<F: FnOnce(&mut super::LibraryType)>(f: F) -> super::LibraryType {
        let mut t = super::LibraryType {
            access: super::ClassifierAccess::Public,
            source_file: None,
            is_nested: false,
            outer_instance: None,
            kind: super::TypeKind::Class,
            inheritance: Default::default(),
            supertypes: crate::types::TypeNameList::new(),
            supertype_templates: Vec::new(),
            constructors: vec![],
            fields: vec![],
            declared_callables: std::collections::HashMap::new(),
            members: vec![],
            companion: vec![],
            constants: std::collections::HashMap::new(),
            sam_method: None,
            callable_signature: None,
            companion_object: None,
            value_companion_fns: vec![],
            value_underlying: None,
            value_underlying_property: None,
            alias_target: None,
            type_parameters: crate::types::TypeParameters::default(),
            sealed_subclasses: crate::types::TypeNameList::new(),
            enum_entries: vec![],
            enum_entries_accessor: None,
            named_parameter_lists: vec![],
            retention: None,
        };
        f(&mut t);
        t
    }

    #[test]
    fn library_type_is_enum_entry_reads_the_entries() {
        let t = ty_with(|t| t.enum_entries = vec!["PENDING".into(), "DONE".into()]);
        assert!(t.is_enum_entry("PENDING"));
        assert!(t.is_enum_entry("DONE"));
        assert!(!t.is_enum_entry("MISSING"));
        assert!(!ty_with(|_| {}).is_enum_entry("PENDING"));
    }

    #[test]
    fn library_type_constructor_named_params_picks_long_enough_and_valid() {
        let expected = ParamList {
            visibility: Visibility::Public,
            names: vec!["host".into(), "port".into()],
            defaults: vec![false, true],
            types: Vec::new(),
            recv_fun: vec![false, false],
            vararg: None,
            annotation: None,
        };
        let t = ty_with(|t| {
            t.named_parameter_lists = vec![expected.clone()];
        });
        assert_eq!(t.constructor_named_params(1), Some(expected));
        assert!(t.constructor_named_params(3).is_none());

        let bad = ty_with(|t| {
            t.named_parameter_lists = vec![ParamList {
                visibility: Visibility::Public,
                names: vec!["".into()],
                defaults: vec![false],
                types: Vec::new(),
                recv_fun: vec![false],
                vararg: None,
                annotation: None,
            }];
        });
        assert!(bad.constructor_named_params(0).is_none());
    }

    #[test]
    fn normalized_annotation_parameters_never_become_a_constructor() {
        let policy = AnnotationParameterPolicy {
            positional: AnnotationPositionalPolicy::NamedOnly,
            materialize_omitted_vararg: false,
        };
        let parameters = ParamList {
            visibility: Visibility::Public,
            names: vec!["text".into()],
            defaults: vec![false],
            types: vec![Ty::String],
            recv_fun: vec![false],
            vararg: None,
            annotation: Some(policy),
        };
        let annotation = ty_with(|ty| {
            ty.kind = TypeKind::Annotation;
            ty.named_parameter_lists = vec![parameters.clone()];
        });
        assert!(annotation.constructor_named_params(0).is_none());
        assert_eq!(
            annotation.annotation_application(),
            Some(AnnotationApplication { parameters, policy })
        );
    }

    #[test]
    fn metadata_call_signature_retains_vararg_position() {
        let signature = CallSig::metadata_member(
            3,
            ["first", "values", "tail"].map(str::to_string).to_vec(),
            vec![false; 3],
            Some(1),
        );

        assert!(signature.vararg);
        assert_eq!(signature.vararg_index, Some(1));
    }

    #[test]
    fn metadata_call_signature_retains_names_after_unnamed_context_receiver() {
        let signature = CallSig::metadata_member(
            3,
            vec![String::new(), "first".into(), "second".into()],
            vec![false; 3],
            None,
        );

        assert_eq!(signature.param_names, ["", "first", "second"]);
    }

    #[test]
    fn positional_argument_mapping_does_not_require_parameter_names() {
        let arguments = [7u8];

        assert_eq!(
            map_call_args(&arguments, None, &[], 1, 1, &[], None, false),
            Ok(vec![Some(arguments[0])])
        );
    }

    #[test]
    fn semantic_platform_does_not_require_runtime_abi() {
        struct SemanticOnly;
        impl crate::symbol_source::SymbolSource for SemanticOnly {}
        impl super::SemanticPlatform for SemanticOnly {
            fn platform_default_import_packages(&self) -> &'static [&'static str] {
                &["kotlin"]
            }
        }

        let source: &dyn super::SemanticPlatform = &SemanticOnly;
        assert_eq!(source.platform_default_import_packages(), &["kotlin"]);
        assert_eq!(
            source.boxed_primitive(crate::types::Ty::obj("platform/Box")),
            None
        );
    }
}
