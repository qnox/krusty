//! Library metadata shared by symbol sources.

pub use crate::types::Visibility;
use crate::types::{Ty, TypeName, TypeNameList};
use std::borrow::Cow;
use std::collections::HashMap;

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
#[derive(Clone, Debug)]
pub struct GenericSig {
    pub formals: Vec<String>,
    /// Declared upper bounds, parallel to [`Self::formals`].
    pub formal_bounds: Vec<Vec<Ty>>,
    /// The dispatch/extension receiver's type (member self-type or extension receiver), if any.
    pub receiver: Option<Ty>,
    pub params: Vec<Ty>,
    pub ret: Ty,
}

/// Bit-packed boolean flags for a [`LibraryMember`], collapsing `ret_nullable`/`is_interface`/
/// `suspend` into one byte. Read through the `LibraryMember` accessors of the same names; mutated
/// through the matching `set_*` methods; built with the `with_*` chain. Headroom for five more flags.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LmFlags(u8);

impl LmFlags {
    const RET_NULLABLE: u8 = 1 << 0;
    const IS_INTERFACE: u8 = 1 << 1;
    const SUSPEND: u8 = 1 << 2;

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
    pub params: Vec<Ty>,
    pub ret: Ty,
    pub physical_ret: Ty,
    pub descriptor: String,
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
    /// The member's Kotlin visibility, from its bytecode access flags/`@Metadata`. A `Protected` member
    /// is surfaced (not dropped) so a subclass can reach an inherited classpath member; the emit is
    /// identical to a public one. `Public` by default.
    pub visibility: Visibility,
    /// Source call-shape (parameter names + default flags + `required`, lambda parameter types) — the same
    /// facts `CallSig` carries for functions. Lets a resolver member query drive a NAMED-argument member
    /// call and lambda-parameter typing without the removed receiver-indexed `functions()` seam. Default
    /// (empty) for a provider that records no source parameter metadata.
    pub call_sig: CallSig,
}

/// A public static field and its optional compile-time constant.
#[derive(Clone, Debug)]
pub struct StaticFieldRef {
    pub owner: TypeName,
    pub name: String,
    pub descriptor: String,
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

    fn value_underlying_name(&self, internal: TypeName) -> Option<Ty> {
        self.resolve_type_name(internal)
            .and_then(|t| t.value_underlying)
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

    /// Whether a member has a physical target suitable for a callable reference.
    fn supports_member_reference(&self, _member: &LibraryMember) -> bool {
        true
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

    /// Platform spelling for a physical zero-arg getter when Kotlin property metadata is unavailable.
    /// Common resolution asks for a semantic property name first; this hook is a fallback owned by the
    /// target because JVM uses JavaBean-style `getX`/`isX` while other targets need not.
    /// Every plausible physical getter spelling for `property`, most-conventional first
    /// (`id` → `getId`, `getID`; `urlPath` → `getUrlPath`, `getURLPath`) — the inverse of
    /// Kotlin's decapitalize-smart getter-to-property mapping.
    fn physical_property_getter_names(&self, property: &str) -> Vec<String> {
        self.physical_property_getter_name(property)
            .into_iter()
            .collect()
    }

    fn physical_property_getter_name(&self, _property: &str) -> Option<String> {
        None
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
            params,
            ret,
            physical_ret: ret,
            descriptor,
            signature: None,
            generic_sig: None,
            flags: LmFlags::default(),
            inline: InlineKind::None,
            visibility: Visibility::Public,
            call_sig: CallSig::default(),
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
            params,
            ret,
            physical_ret,
            descriptor: descriptor.into(),
            suspend: false,
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
    pub params: Vec<Ty>,
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
    pub names: Vec<String>,
    pub defaults: Vec<bool>,
    /// Per parameter: whether its declared type is a RECEIVER function type (`Recv.() -> R`). The JVM
    /// descriptor and `Signature` both erase that to a plain `FunctionN`, so only the source-level
    /// metadata carries it — and without it a lambda argument gets no implicit receiver, leaving a bare
    /// member call inside unresolved. Empty when the origin records no per-parameter types.
    pub recv_fun: Vec<bool>,
    pub vararg: Option<usize>,
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
                        .unwrap_or_else(|| "?".to_string()),
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

    pub fn requires_all_args(&self, param_count: usize) -> bool {
        !self.vararg && self.required == param_count
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

    /// Build the source call shape for a non-extension Kotlin function decoded from metadata.
    /// Members and receiver-less top-level functions carry the same value-parameter semantics;
    /// keeping one constructor prevents either origin from silently dropping receiver-lambda or
    /// materialization facts. Extension functions first remove their callable receiver and are
    /// handled by [`Self::metadata_extension`].
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

    pub fn metadata_extension(
        physical_param_count: usize,
        names: Vec<String>,
        defaults: Vec<bool>,
        vararg_index: Option<usize>,
    ) -> Self {
        // Do not route extension declarations through `metadata_function`. Their callable receiver
        // is removed here, and extension resolution derives each lambda argument's full function
        // shape from the selected extension signature. Publishing the receiver-lambda marks through
        // this second channel would make one semantic fact origin-dependent and can re-check
        // receiver blocks through the member path, where labeled `this` lowering has different
        // ownership. Ordinary members and receiver-less top-level functions have no such competing
        // channel, so they intentionally share `metadata_function` instead.
        // The physical param count includes the extension receiver; the source VALUE params (with their
        // default flags — an `inline fun Mutex.withLock(owner: Any? = null, action)` needs them so an
        // omitted-default trailing-lambda call resolves) follow it.
        physical_param_count
            .checked_sub(1)
            .map(|param_count| CallSig::metadata_base(param_count, names, defaults, vararg_index))
            .unwrap_or_default()
    }

    fn metadata_base(
        param_count: usize,
        names: Vec<String>,
        defaults: Vec<bool>,
        vararg_index: Option<usize>,
    ) -> Self {
        let mut names = vec_for_arity(names, param_count);
        if names.iter().any(String::is_empty) {
            names.clear();
        }
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
            Some(meta) if meta.type_args().is_empty() && !fallback.type_args().is_empty() => {
                let name = meta.name();
                Ty::obj_args(&name, fallback.type_args())
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
    /// Number of leading context parameters in the logical parameter list.
    pub context_count: usize,
    /// Source declaration key for a callable from the current compilation module. Classpath callables
    /// leave this unset.
    pub source_key: Option<(u32, u32)>,
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
                    self.extension_value_params()
                } else {
                    &self.callable.params
                }
            },
            |signature| signature.params.as_slice(),
        )
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
                })
            },
            Cow::Borrowed,
        )
    }

    pub fn extension_value_params(&self) -> &[Ty] {
        self.callable.params.get(1..).unwrap_or(&[])
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
            context_count: 0,
            source_key: None,
        }
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
        member.owner = Some(self.callable.owner);
        member.physical_ret = self.callable.physical_ret;
        member.signature = self.callable.signature.clone();
        member.generic_sig = self.generic_sig.clone();
        member.inline = self.flags.inline;
        member.set_suspend(self.flags.suspend);
        // Keep source call shape coupled to the selected overload.
        member.call_sig = self.call_sig.clone();
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
    /// A NON-PUBLIC `@InlineOnly` function (`require`/`check`/`error`/`let`/…): there is no callable
    /// method to invoke, so the backend MUST splice the body — a failed splice skips the whole file
    /// (never an `invokestatic` on the private method → never an `IllegalAccessError`).
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
    /// `suspend` — decoded from `@Metadata` (the `IS_SUSPEND` function flag). A call to a suspend
    /// function is a coroutine suspension point (the JVM lowering threads a `Continuation`).
    pub suspend: bool,
    /// Kotlin's `operator` modifier. Call conventions such as `receiver(args)` must filter on this
    /// semantic flag; JVM method names alone cannot distinguish an explicit `.invoke()` declaration.
    pub operator: bool,
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
#[derive(Clone)]
pub struct PropertyInfo {
    pub kind: PropKind,
    /// The extension/member receiver type; `None` for a top-level property.
    pub receiver: Option<Ty>,
    /// The property's own formal type parameters (`val <T> List<T>.foo`); empty for a plain property.
    pub formals: Vec<String>,
    /// The property's declared type.
    pub ty: Ty,
    /// Number of leading context parameters in each accessor's parameter list.
    pub context_count: usize,
    /// The real getter — an opaque platform emit handle (the erased descriptor lives here).
    pub getter: LibraryCallable,
    /// The setter, present iff the property is a `var`.
    pub setter: Option<LibraryCallable>,
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

/// What a fully-qualified name resolves to in a [`crate::symbol_source::SymbolSource`] — the
/// platform-neutral namespace record (the spec's top-level memo value). Kotlin has TWO namespaces
/// (classifier vs callable) and one name can occupy both at once, so this is a RECORD: the `classifier`
/// (at most one) AND the `callables`. The resolver forms candidate FQNs from the import scope, queries
/// `resolve_symbols` per fqn, and selects by syntactic position (type → classifier; call → callables ∪
/// the classifier's constructors, then property-`invoke` fallback; value → property / object).
#[derive(Clone, Default)]
pub struct ResolvedSymbols {
    /// Shared with the type-name memo, so cloning a record never deep-clones the classifier.
    pub classifier: Option<std::rc::Rc<LibraryType>>,
    pub callables: Callables,
}

impl ResolvedSymbols {
    /// Nothing resolves this name (both namespaces empty).
    pub fn is_empty(&self) -> bool {
        self.classifier.is_none() && matches!(self.callables, Callables::None)
    }
}

/// The shape of a library type: enough for the front end to resolve member accesses against it
/// (publicness, kind, supertypes, constructors, instance members, and companion members) without
/// knowing the target ABI.
#[derive(Clone)]
pub struct LibraryType {
    pub is_public: bool,
    /// The declaration kind (class / interface / annotation / object). One field instead of parallel
    /// booleans — read it through the `is_*` accessors, which encode the JVM reality that an annotation
    /// is also an interface.
    pub kind: TypeKind,
    /// Internal names of the superclass + implemented interfaces (for the inherited-member walk).
    pub supertypes: TypeNameList,
    pub constructors: Vec<LibraryMember>,
    /// Field declarations owned by this classifier. Selection is deliberately not performed by the
    /// provider: the resolver walks these together with properties and supertypes, so source, module,
    /// and compiled classifiers obey one hiding and precedence rule.
    pub fields: Vec<LibraryField>,
    /// Instance members (member functions and property accessors).
    pub members: Vec<LibraryMember>,
    /// Companion-object members — accessed as `Type.member(…)` (the JVM realizes these as statics).
    pub companion: Vec<LibraryMember>,
    /// Compile-time constants exposed by the companion object (`Int.MAX_VALUE`, `Double.NaN`, …).
    /// Stored on the type shape so lowering consumes already-resolved library facts instead of making
    /// a platform-specific side query.
    pub companion_consts: HashMap<String, LibraryConst>,
    /// The single abstract method when this type is a functional interface. None for ordinary classes,
    /// non-SAM interfaces, and sources that do not provide SAM metadata.
    pub sam_method: Option<LibraryMember>,
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
    /// When this name is a `typealias`, the target internal it expands to (`kotlin/collections/ArrayList`
    /// → `java/util/ArrayList`); `None` for a real type. Name resolution records the target, so an alias
    /// resolves to the underlying type with no separate alias query.
    pub alias_target: Option<TypeName>,
    /// The type's own formal type parameters, in declaration order (`Pair` → `["A", "B"]`); empty for a
    /// non-generic type. With the constructors' [`LibraryMember::generic_sig`], lets a caller infer a
    /// construction's type arguments by unifying the ctor's generic parameter signatures against the
    /// actual argument types.
    pub type_params: Vec<String>,
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
    /// Whether a `@JvmInline value class`'s primary constructor is defaulted — kotlinc emits a
    /// `constructor-impl$default` synthetic exactly then, which realizes an all-defaulted `Id()`.
    pub value_ctor_has_default: bool,
    /// Constructor SOURCE parameter names plus per-parameter default flags from `@Metadata`.
    pub ctor_named_params: Vec<ParamList>,
    /// Properties whose JVM getter is value-class-`@JvmName`-mangled (`Holder(val id: Vid)` →
    /// `getId-<hash>`) and whose physical return erases to the value class's underlying, so ordinary
    /// getter resolution misses them. Keyed by SOURCE property name; the member carries the MANGLED getter
    /// name + physical descriptor but the LOGICAL value-class return type from `@Metadata`, so `h.id` types
    /// as the value class.
    pub value_class_properties: Vec<(String, LibraryMember)>,
    /// For a classpath annotation type: the `java.lang.annotation.RetentionPolicy` constant name of its
    /// `@Retention` (`"RUNTIME"` / `"CLASS"` / `"SOURCE"`), or `None` if absent. Drives whether a use of
    /// the annotation is emitted `RuntimeVisibleAnnotations` (RUNTIME) / `RuntimeInvisibleAnnotations`
    /// (CLASS = Kotlin BINARY) / dropped (SOURCE).
    pub retention: Option<String>,
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

    /// Whether an enum entry named `name` is declared on this type — lets `EnumName.ENTRY` resolve.
    pub fn is_enum_entry(&self, name: &str) -> bool {
        self.enum_entries.iter().any(|e| e == name)
    }

    /// Constructor source parameter names/default flags for a named call with `min_arity` supplied args.
    pub fn constructor_named_params(&self, min_arity: usize) -> Option<ParamList> {
        self.ctor_named_params
            .iter()
            .find(|params| {
                params.names.len() >= min_arity
                    && params.names.len() == params.defaults.len()
                    && !params.names.iter().any(String::is_empty)
            })
            .cloned()
    }

    /// The value-class-typed property `property`'s member (mangled getter + logical value-class return),
    /// or `None` for an ordinary property.
    pub fn value_class_property(&self, property: &str) -> Option<&LibraryMember> {
        self.value_class_properties
            .iter()
            .find_map(|(p, m)| (p == property).then_some(m))
    }
}

impl LibraryType {
    /// Annotation members `(name, Ty)` — the no-argument accessors of an `@interface`.
    pub fn annotation_members(&self) -> Option<Vec<(String, Ty)>> {
        if !self.is_annotation() {
            return None;
        }
        let mut out = Vec::new();
        for m in &self.members {
            if m.params.is_empty() && m.name != "<init>" {
                if m.ret == Ty::Error {
                    return None; // a member type we can't model — skip the whole annotation
                }
                out.push((m.name.clone(), m.ret));
            }
        }
        Some(out)
    }
}

/// A library field's compile-time constant.
#[derive(Clone, Debug, PartialEq)]
pub enum LibConst {
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    Str(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct LibraryConst {
    pub ty: Ty,
    pub value: LibConst,
}

/// A compiled-library source: a [`SymbolSource`] (its type universe, overloads, and type shapes) PLUS
/// the backend extras needed while deciding whether a selected call can be emitted. The federatable half
/// is `SymbolSource`; these extras are consulted only after ordinary symbol selection, never across the
/// source federation.
/// A recognized `kotlin.coroutines` compiler intrinsic. These are `@InlineOnly` stdlib declarations the
/// reference compiler replaces by name with dedicated codegen rather than calling/inlining (their stub
/// bodies just `throw`). Platform-neutral language concept; backend codegen is keyed on this variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoroutineIntrinsic {
    /// `COROUTINE_SUSPENDED` — the suspension sentinel (typed `Any`).
    CoroutineSuspended,
    /// `suspendCoroutineUninterceptedOrReturn { c -> … }` — inline the block with the enclosing
    /// function's own continuation bound as the parameter; its `Any?` result becomes the result.
    SuspendCoroutineUninterceptedOrReturn,
    /// `suspendCoroutine { c -> … }` — the stdlib inline wrapper: build a `SafeContinuation` over the
    /// enclosing continuation, run the block with it, return `safe.getOrThrow()`.
    SuspendCoroutine,
    /// `startCoroutine` — start a coroutine with a completion continuation (extension on a suspend
    /// function type).
    StartCoroutine,
    /// `createCoroutine` — build (but don't start) a coroutine, returning the initial continuation.
    CreateCoroutine,
}

pub fn coroutine_intrinsic(name: &str) -> Option<CoroutineIntrinsic> {
    match name {
        "COROUTINE_SUSPENDED" => Some(CoroutineIntrinsic::CoroutineSuspended),
        "suspendCoroutineUninterceptedOrReturn" => {
            Some(CoroutineIntrinsic::SuspendCoroutineUninterceptedOrReturn)
        }
        "suspendCoroutine" => Some(CoroutineIntrinsic::SuspendCoroutine),
        "startCoroutine" => Some(CoroutineIntrinsic::StartCoroutine),
        "createCoroutine" => Some(CoroutineIntrinsic::CreateCoroutine),
        _ => None,
    }
}

/// A symbol source with no external libraries — compiling a self-contained source set with no classpath.
pub struct EmptySymbolSource;

impl crate::symbol_source::SymbolSource for EmptySymbolSource {}
impl SemanticPlatform for EmptySymbolSource {}

#[cfg(test)]
mod tests {
    use super::{map_call_args, CallSig, InlineKind, ParamList, Visibility};

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
            is_public: true,
            kind: super::TypeKind::Class,
            supertypes: crate::types::TypeNameList::new(),
            constructors: vec![],
            fields: vec![],
            members: vec![],
            companion: vec![],
            companion_consts: std::collections::HashMap::new(),
            sam_method: None,
            companion_object: None,
            value_companion_fns: vec![],
            value_underlying: None,
            alias_target: None,
            type_params: vec![],
            sealed_subclasses: crate::types::TypeNameList::new(),
            enum_entries: vec![],
            enum_entries_accessor: None,
            value_ctor_has_default: false,
            ctor_named_params: vec![],
            value_class_properties: vec![],
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
            names: vec!["host".into(), "port".into()],
            defaults: vec![false, true],
            recv_fun: vec![false, false],
            vararg: None,
        };
        let t = ty_with(|t| {
            t.ctor_named_params = vec![expected.clone()];
        });
        assert_eq!(t.constructor_named_params(1), Some(expected));
        assert!(t.constructor_named_params(3).is_none());

        let bad = ty_with(|t| {
            t.ctor_named_params = vec![ParamList {
                names: vec!["".into()],
                defaults: vec![false],
                recv_fun: vec![false],
                vararg: None,
            }];
        });
        assert!(bad.constructor_named_params(0).is_none());
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
    fn positional_argument_mapping_does_not_require_parameter_names() {
        let arguments = [7u8];

        assert_eq!(
            map_call_args(&arguments, None, &[], 1, 1, &[], None, false),
            Ok(vec![Some(arguments[0])])
        );
    }

    #[test]
    fn library_type_value_class_property_lookup_by_source_name() {
        let member = super::LibraryMember::new(
            "getId-abc123".into(),
            vec![],
            crate::types::Ty::obj("lib/Vid"),
            "()Ljava/lang/String;".into(),
        );
        let t = ty_with(|t| t.value_class_properties = vec![("id".into(), member)]);
        assert_eq!(
            t.value_class_property("id").map(|m| m.name.as_str()),
            Some("getId-abc123")
        );
        assert!(t.value_class_property("missing").is_none());
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
