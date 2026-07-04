//! Platform-neutral library data shared by [`crate::symbol_source::SymbolSource`] providers. A source is
//! the common denominator a front end needs from compiled libraries: the type universe and the *shape* of
//! each type and top-level callable, whether the libraries are a JVM classpath (bytecode `.class` jars)
//! or a klib (IR).
//!
//! The resolver and IR lowering depend **only** on this trait, never on the JVM backend: every
//! `java/lang/…` name, descriptor parse, and classpath read lives behind a concrete implementation
//! (`jvm::jvm_libraries::JvmLibraries`). Swapping in a klib-backed `SymbolSource` would let the same
//! front end target Kotlin/JS without touching `resolve`/`ir_lower`.
//!
//! The surface is deliberately Kotlin-semantic — there is no "static" (a `Type.foo()` call is a
//! companion-object member; a top-level/extension call is a package-level callable). The JVM
//! realization of those (invokestatic on a facade, `@JvmStatic`, descriptors) lives in the impl.

use crate::types::Ty;
use std::collections::HashMap;

/// A parsed generic-signature node, platform neutral. A backend parses its own signature format into
/// this tree; call resolution unifies and substitutes over it without knowing which backend produced it.
#[derive(Clone, Debug)]
pub enum GSig {
    Var(String),
    Class(String, Vec<GSig>),
    Function { params: Vec<GSig>, ret: Box<GSig> },
    Arr(Box<GSig>),
    Prim(Ty),
}

/// A parsed method generic signature: formal names, parameter signature nodes, and return signature.
#[derive(Clone, Debug)]
pub struct GenericSig {
    pub formals: Vec<String>,
    pub params: Vec<GSig>,
    pub ret: GSig,
}

/// The type universe a symbol source contributes, resolved to internal names: every importable
/// simple name → its internal name, plus type aliases (`alias` → target simple/internal name).
#[derive(Default)]
pub struct LibrarySeed {
    pub class_names: HashMap<String, String>,
    pub type_aliases: HashMap<String, String>,
    /// Non-identity canonical internal-name aliases used for subtype identity checks
    /// (`kotlin/collections/List` -> `java/util/List`). Missing means identity.
    pub canonical_names: HashMap<String, String>,
}

/// One member (constructor, member function/property accessor, or companion member) of a library
/// type, in Kotlin terms. `descriptor` is an opaque backend token (a JVM method descriptor) the
/// matching emitter consumes verbatim — the front end matches on `params`/`ret`, never parsing it.
#[derive(Clone, Debug)]
pub struct LibraryMember {
    /// The Kotlin/source name used for resolution (`CharSequence.get`, `Number.toInt`).
    pub name: String,
    /// Concrete platform owner when it differs from the receiver's resolved type.
    pub owner: Option<String>,
    /// Physical method name when it differs from the Kotlin/source member name.
    pub physical_name: Option<String>,
    pub params: Vec<Ty>,
    pub ret: Ty,
    /// Kotlin metadata return nullability (`T?`). Descriptors erase this, but resolution needs it so
    /// nullable generic/member returns remain boxed/reference-like until a use site demands unboxing.
    pub ret_nullable: bool,
    pub physical_ret: Ty,
    pub descriptor: String,
    pub signature: Option<String>,
    pub is_interface: bool,
    pub inline: InlineKind,
    /// The member is a `suspend fun` — a call site inside a suspend body must thread a `Continuation`
    /// into the emitted invoke (its CPS descriptor rebuilt by the coroutine pass) and treat the
    /// Object-erased result as `ret`.
    pub suspend: bool,
}

/// Platform-provided accessor used by counted range/progression loop lowering. The name and descriptor
/// are backend tokens; common lowering only emits them back to the same backend.
#[derive(Clone, Debug)]
pub struct PlatformAccessor {
    pub name: String,
    pub descriptor: String,
}

/// The platform/library shape of a range-like type that can be iterated as a counted loop. This keeps
/// library class names, mangled value-class accessors, and backend descriptors out of common lowering.
#[derive(Clone, Debug)]
pub struct CountedLoopInfo {
    pub elem: Ty,
    pub first: PlatformAccessor,
    pub last: PlatformAccessor,
    /// `None` for unit-step ranges; `Some` for progressions whose step is read from the value.
    pub step: Option<(PlatformAccessor, Ty)>,
}

/// Platform runtime class plus constructor descriptor used when common lowering must synthesize a
/// library runtime object. Both fields are backend tokens owned by the provider.
#[derive(Clone, Debug)]
pub struct PlatformCtor {
    pub internal: String,
    pub ctor_desc: String,
}

/// Platform-owned static field access. Used for target-specific singleton/companion realizations.
#[derive(Clone, Debug)]
pub struct PlatformField {
    pub owner: String,
    pub name: String,
    pub descriptor: String,
}

/// Platform construction plan for a Kotlin range value expression. `elem` is the semantic element
/// type operands coerce to; `through` constructs `a..b`; `until` realizes `a..<b` when supported.
#[derive(Clone, Debug)]
pub struct RangeConstruction {
    pub elem: Ty,
    pub result: Ty,
    pub through: PlatformRangeCtor,
    pub until: Option<LibraryCallable>,
}

/// Platform-owned range constructor tokens. `trailing_nulls` covers synthetic marker arguments such as
/// JVM unsigned range constructors without exposing those marker classes to common lowering.
#[derive(Clone, Debug)]
pub struct PlatformRangeCtor {
    pub internal: String,
    pub ctor_desc: String,
    pub trailing_nulls: usize,
}

/// Platform-owned runtime helper. The common lowerer can request a semantic helper and emit the
/// returned opaque callable without spelling target runtime classes or descriptors.
#[derive(Clone, Copy, Debug)]
pub enum RuntimeOp {
    UnsignedBox,
    UnsignedUnbox,
    UnsignedCompare,
    UnsignedDivide,
    UnsignedRemainder,
    UnsignedToString,
    UIntToLong,
    PrimitiveCompare,
    HashCode,
    ArrayToString,
    ArrayCopyOf,
    StartCoroutine,
    ThrowOnFailure,
    CoroutineSuspended,
}

#[derive(Clone, Copy, Debug)]
pub enum RuntimeCtor {
    IllegalStateException,
    AssertionError,
}

/// Target/runtime services used after resolution, mostly by common IR lowering. This is deliberately
/// separate from [`crate::symbol_source::SymbolSource`]: the resolver should see declarations and
/// semantic library metadata, while target runtime class names/descriptors live here.
pub trait TargetRuntime {
    /// Runtime interface/class used to represent a function value of `arity` on this platform.
    fn function_type(&self, _arity: usize) -> Option<Ty> {
        None
    }

    /// Runtime implementation class constructed for a property reference on this platform.
    fn property_reference_impl(&self, _arity: usize, _mutable: bool) -> Option<PlatformCtor> {
        None
    }

    /// Platform reflection signature stored in a synthesized property-reference object.
    fn property_reference_signature(&self, _getter_name: &str, _ret: Ty) -> Option<String> {
        None
    }

    /// Platform field/type descriptor for a lowered IR type.
    fn type_descriptor(&self, _ty: Ty) -> Option<String> {
        None
    }

    /// Platform field/type descriptor for a type already stored in IR representation. Most targets can
    /// treat this like [`TargetRuntime::type_descriptor`], but the JVM maps IR spellings such as
    /// `Obj("kotlin/Int")` back to primitive descriptor carriers in some ABI positions.
    fn ir_type_descriptor(&self, ty: Ty) -> Option<String> {
        self.type_descriptor(ty)
    }

    /// Platform method descriptor for lowered IR parameter and return types.
    fn method_descriptor(&self, _params: &[Ty], _ret: Ty) -> Option<String> {
        None
    }

    /// Runtime superclass used for synthesized function references on this platform.
    fn function_reference_impl_type(&self) -> Option<Ty> {
        None
    }

    /// Platform accessor used for built-in enum properties such as `ordinal` and `name`.
    fn enum_member_accessor(&self, _name: &str) -> Option<PlatformAccessor> {
        None
    }

    /// Platform static field for an object singleton value.
    fn object_instance_field(&self, _internal: &str) -> Option<PlatformField> {
        None
    }

    /// Platform static field for a class companion singleton value.
    fn companion_instance_field(
        &self,
        _class_internal: &str,
        _companion_internal: &str,
        _field_name: &str,
    ) -> Option<PlatformField> {
        None
    }

    /// Platform runtime holder type used when a mutable local of `elem` is captured by a closure.
    fn mutable_local_ref_type(&self, _elem: Ty) -> Option<Ty> {
        None
    }

    /// The target's scalar carrier for a semantic value type, when it has one. Signed primitives usually
    /// carry themselves; target-owned value primitives may carry another type (`UInt` as `Int` on JVM).
    /// Common lowering uses this to decide boxing/coercion shape without spelling a backend primitive set.
    fn scalar_value_repr(&self, _ty: Ty) -> Option<Ty> {
        None
    }

    /// The boxed library value-class/object type for an unsigned integer semantic type, when the target has
    /// such a representation (`UInt` -> `kotlin/UInt` on JVM). Common lowering uses this for box/unbox and
    /// `is UInt` shapes without spelling target class names.
    fn unsigned_integer_box_type(&self, _ty: Ty) -> Option<Ty> {
        None
    }

    /// If `internal` is a platform range/progression type that can be emitted as a counted loop,
    /// describe its source element type and platform accessors. The default keeps non-platform sources
    /// on the ordinary iterator path.
    fn counted_loop_info(&self, _internal: &str) -> Option<CountedLoopInfo> {
        None
    }

    /// Platform construction shape for a Kotlin range expression with operands `lo` and `hi`.
    /// The provider owns range runtime class names, constructor descriptors, synthetic marker slots,
    /// and helper facades.
    fn range_construction(&self, _lo: Ty, _hi: Ty) -> Option<RangeConstruction> {
        None
    }

    /// Physical call descriptor for invoking a suspend callable whose current descriptor is still the
    /// logical source signature. The provider owns continuation descriptors and return erasure.
    fn suspend_cps_descriptor(&self, _logical_descriptor: &str) -> Option<String> {
        None
    }

    /// Platform callable for a runtime helper. Common lowering selects the semantic helper; the
    /// provider owns target runtime classes, method names, and descriptors.
    fn runtime_callable(&self, _op: RuntimeOp, _ty: Ty) -> Option<LibraryCallable> {
        None
    }

    /// Platform constructor for a runtime support object. Common lowering selects the semantic
    /// constructor; the provider owns target runtime classes and descriptors.
    fn runtime_ctor(&self, _ctor: RuntimeCtor) -> Option<PlatformCtor> {
        None
    }

    /// Whether a selected library callable has the semantics of Kotlin's defaulted reified
    /// `assertFailsWith<T> { ... }` helper. Such helpers cannot be called directly when their platform
    /// realization is private inline-only bytecode; common lowering can still realize the semantic shape
    /// as `try/catch` IR when the target identifies it.
    fn is_reified_assert_fails_with_default(&self, _callable: &LibraryCallable) -> bool {
        false
    }
}

pub trait CompilerPlatform: crate::symbol_source::SymbolSource + TargetRuntime {}

impl<T> CompilerPlatform for T where T: crate::symbol_source::SymbolSource + TargetRuntime {}

impl LibraryMember {
    pub fn new(name: String, params: Vec<Ty>, ret: Ty, descriptor: String) -> Self {
        LibraryMember {
            name,
            owner: None,
            physical_name: None,
            params,
            ret,
            ret_nullable: false,
            physical_ret: ret,
            descriptor,
            signature: None,
            is_interface: false,
            inline: InlineKind::None,
            suspend: false,
        }
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
        facade: String,
    },
}

impl LibraryCallable {
    pub fn library(
        owner: impl Into<String>,
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
            inline: InlineKind::None,
            default_call: false,
            vararg_elem: None,
            signature: None,
            origin: Origin::Library,
        }
    }
}

/// A resolved companion-object function on a classpath value class (`Result.success`). The call lowers
/// to `getstatic <class>.<field>:L<companion>;` (the receiver) then an inline-splice of the companion
/// INSTANCE method carried by `callable` (its `this` is the loaded singleton).
#[derive(Clone, Debug)]
pub struct CompanionFn {
    /// The value-class declaring the companion (`kotlin/Result`).
    pub class_internal: String,
    /// The companion object's internal name (`kotlin/Result$Companion`).
    pub companion_internal: String,
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
    pub owner: String,
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
    /// The callee's generic `Signature` (an opaque backend token), kept so an arg-binding SELECTOR can
    /// recover the substituted return (`fold`'s `R` from the initial value, `let`'s `R` from the lambda)
    /// when picking this overload out of a [`FunctionSet`]. `None` when the callable has no generic
    /// signature. The front end never parses it — only the platform's resolution helpers do.
    pub signature: Option<String>,
    /// Which source produced this callable — the lowerer's cue for the emit form. [`Origin::Library`]
    /// for a classpath callable; [`Origin::Module`] (with its facade) for a current-/sibling-module one.
    pub origin: Origin,
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
#[derive(Clone, Default)]
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
    /// Per logical param: whether it is `crossinline`/`noinline` — its lambda argument is MATERIALIZED
    /// (a real `FunctionN`/nested class) rather than inline-spliced, so a mutable local it captures must
    /// be `Ref`-boxed like an ordinary closure. Parallel to the params; all-false for a non-inline fn.
    pub lambda_materialized: Vec<bool>,
    /// Minimum arguments a caller must supply (params beyond this have defaults). 0 by default.
    pub required: usize,
    /// True if the last logical param is `vararg` (callers pack trailing args into its array).
    pub vararg: bool,
}

impl CallSig {
    pub fn source(
        param_names: Vec<String>,
        param_defaults: Vec<bool>,
        lambda_param_types: Vec<Vec<Ty>>,
        lambda_recv: Vec<bool>,
        required: usize,
        vararg: bool,
    ) -> Self {
        let lambda_receivers = lambda_recv
            .iter()
            .enumerate()
            .map(|(i, has_recv)| {
                if *has_recv {
                    lambda_param_types.get(i).and_then(|v| v.first()).copied()
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
            required,
            vararg,
            ..Default::default()
        }
    }

    pub fn metadata_member(
        param_count: usize,
        names: Option<Vec<String>>,
        defaults: Vec<bool>,
    ) -> Self {
        let defaults = metadata_defaults(defaults, param_count);
        CallSig {
            required: required_arity(param_count, &defaults),
            param_names: metadata_names(names, param_count),
            param_defaults: defaults,
            ..Default::default()
        }
    }

    pub fn metadata_top_level(
        param_count: usize,
        names: Option<Vec<String>>,
        defaults: Vec<bool>,
        lambda_receivers: Vec<Option<Ty>>,
        lambda_receiver_params: Vec<bool>,
        lambda_materialized: Vec<bool>,
    ) -> Self {
        let defaults = metadata_defaults(defaults, param_count);
        CallSig {
            required: required_arity(param_count, &defaults),
            param_names: metadata_names(names, param_count),
            param_defaults: defaults,
            lambda_receivers: vec_for_arity(lambda_receivers, param_count),
            lambda_receiver_params: vec_for_arity(lambda_receiver_params, param_count),
            lambda_materialized: vec_for_arity(lambda_materialized, param_count),
            ..Default::default()
        }
    }

    pub fn metadata_extension(physical_param_count: usize, names: Option<Vec<String>>) -> Self {
        physical_param_count
            .checked_sub(1)
            .map(|param_count| {
                let names = metadata_names(names, param_count);
                CallSig {
                    required: names.len(),
                    param_names: names,
                    ..Default::default()
                }
            })
            .unwrap_or_default()
    }
}

fn metadata_names(names: Option<Vec<String>>, param_count: usize) -> Vec<String> {
    names
        .filter(|names| names.len() == param_count && !names.iter().any(String::is_empty))
        .unwrap_or_default()
}

fn metadata_defaults(defaults: Vec<bool>, param_count: usize) -> Vec<bool> {
    let defaults = vec_for_arity(defaults, param_count);
    if defaults.iter().any(|d| *d) {
        defaults
    } else {
        Vec::new()
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
        let ret = class
            .map(|meta| {
                if meta.type_args().is_empty() && !fallback.type_args().is_empty() {
                    Ty::obj_args(meta.name(), fallback.type_args())
                } else {
                    meta
                }
            })
            .unwrap_or(fallback);
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
    /// Whether the callee is PUBLIC. A non-public callable has no legal call site (`@InlineOnly`); an
    /// arg-binding selector includes it only when it will SPLICE (never emits an `invokestatic`).
    pub public: bool,
    /// For an [`FnKind::Extension`] overload, the receiver-MRO RUNG it was found at (0 = the receiver's
    /// own type, increasing up the supertype chain). An arg-binding selector groups candidates by this
    /// rank and processes rungs most-specific-first, so a `List` extension wins over an `Iterable` one —
    /// the same receiver precedence the classpath lookup gives, preserved through the consolidated query.
    /// `0` for members/top-level (precedence there is by [`FnKind`], not rung); `u32::MAX` marks a
    /// candidate that must never preempt a real rung (the `@OverloadResolutionByLambdaReturnType` family).
    pub receiver_rank: u32,
    /// Provider-specific tie-break key within an otherwise applicable overload set. Lower is preferred.
    /// Consumers treat it as opaque selection data.
    pub overload_rank: u32,
    /// Parsed generic signature, if the provider has one. Carries type-variable binding facts with the
    /// overload instead of making consumers parse backend signature strings after selection.
    pub generic_sig: Option<GenericSig>,
    /// The source-level call shape (defaults, named params, lambda param types, vararg) the checker needs
    /// beyond the erased descriptor. `Default` (empty) when the source doesn't provide it.
    pub call_sig: CallSig,
}

impl FunctionInfo {
    pub fn plain(kind: FnKind, receiver: Option<Ty>, callable: LibraryCallable) -> Self {
        FunctionInfo {
            kind,
            receiver,
            ret: ReturnInfo::default(),
            flags: FnFlags::default(),
            callable,
            public: true,
            receiver_rank: 0,
            overload_rank: 0,
            generic_sig: None,
            call_sig: CallSig::default(),
        }
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
        member.owner = Some(self.callable.owner.clone());
        member.physical_ret = self.callable.physical_ret;
        member.signature = self.callable.signature.clone();
        member.ret_nullable = self.ret.nullable;
        member.inline = self.flags.inline;
        member.suspend = self.flags.suspend;
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
}

/// All overloads of one function name applicable to a call — members AND extensions AND top-level, in one
/// query, each tagged with its [`FnKind`] so the caller applies Kotlin precedence and picks (e.g. by the
/// lambda's return type for `@OverloadResolutionByLambdaReturnType`). The consolidation that replaces the
/// scattered callable / `is_inline` / return-overload / nullable lookups.
#[derive(Clone, Default)]
pub struct FunctionSet {
    pub overloads: Vec<FunctionInfo>,
}

/// The shape of a library type: enough for the front end to resolve member accesses against it
/// (publicness, kind, supertypes, constructors, instance members, and companion members) without
/// knowing the target ABI.
pub struct LibraryType {
    pub is_public: bool,
    /// The declaration kind (class / interface / annotation / object). One field instead of parallel
    /// booleans — read it through the `is_*` accessors, which encode the JVM reality that an annotation
    /// is also an interface.
    pub kind: TypeKind,
    /// Internal names of the superclass + implemented interfaces (for the inherited-member walk).
    pub supertypes: Vec<String>,
    pub constructors: Vec<LibraryMember>,
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
    pub companion_object: Option<(String, String)>,
    /// Public inline companion functions on a classpath value class whose bytecode method is private but
    /// callable per metadata (`Result.success`). Lowering loads the companion object and splices the
    /// method body; ordinary companion members stay in `companion`.
    pub value_companion_fns: Vec<CompanionFn>,
    /// For a classpath `@JvmInline value class`, the erased underlying type it represents on the JVM
    /// (`UInt` → `Int`, `Result` → `Any`); `None` for an ordinary class. The JVM backend erases the value
    /// class to this everywhere (like a user value class), reproducing kotlinc's unboxed representation.
    pub value_underlying: Option<Ty>,
}

/// What a library type *is*. Mutually exclusive at the source level; at the JVM level an `Annotation`
/// also carries `ACC_INTERFACE`, which `is_interface()` reflects.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TypeKind {
    Class,
    Interface,
    Annotation,
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
}

/// Whether a member's parameter list matches `args` as a prefix — the loose match the JVM resolver
/// used (a call's argument descriptors prefixing the method's). One `Ty` → one descriptor token, so
/// a `Ty`-slice prefix is equivalent to a descriptor prefix.
fn params_prefix(member_params: &[Ty], args: &[Ty]) -> bool {
    member_params.len() >= args.len() && member_params[..args.len()] == *args
}

/// Whether `arg` can be passed where `param` is expected, in erased Kotlin terms: an exact `Ty`
/// match, or any argument into an erased generic (`Any`) parameter — a primitive boxes into it
/// (`List<Int>.add(E)` → `add(Object)`, calling with `Int` boxes to `Integer`), a reference passes
/// directly. This is what lets a primitive argument select the erased `(Object)` overload instead of
/// falling through to a longer-arity overload it happens to prefix.
pub(crate) fn arg_assignable(param: &Ty, arg: &Ty) -> bool {
    param == arg || *param == Ty::obj("kotlin/Any")
}

/// The best overload named `name` among `candidates` for `args`: an exact-arity exact-`Ty` match,
/// else an exact-arity match with autoboxing into erased `Any` parameters, else a prefix match (the
/// loose fallback covering varargs/defaulted trailing parameters).
pub(crate) fn best_overload<'a>(
    candidates: impl Iterator<Item = &'a LibraryMember> + Clone,
    name: &str,
    args: &[Ty],
) -> Option<&'a LibraryMember> {
    let named = candidates.filter(|m| m.name == name);
    named
        .clone()
        .find(|m| m.params == *args)
        .or_else(|| {
            named.clone().find(|m| {
                m.params.len() == args.len()
                    && m.params.iter().zip(args).all(|(p, a)| arg_assignable(p, a))
            })
        })
        .or_else(|| named.clone().find(|m| params_prefix(&m.params, args)))
}

impl LibraryType {
    /// A constructor callable with `args` — exact arity, then a widening pass that erases each
    /// reference argument to `Any` (a JDK type may only expose the `(Object)` overload).
    pub fn ctor(&self, args: &[Ty]) -> Option<&LibraryMember> {
        if let Some(m) = self.constructors.iter().find(|m| m.params == *args) {
            return Some(m);
        }
        // A `null` argument matches any reference parameter (exact on the other positions). Lets a
        // constructor with a reference parameter be called with an explicit `null`
        // (e.g. `PluginGeneratedSerialDescriptor(name, null, count)`), which the exact compare misses.
        if args.iter().any(|a| matches!(a, Ty::Null)) {
            if let Some(m) = self.constructors.iter().find(|m| {
                m.params.len() == args.len()
                    && m.params
                        .iter()
                        .zip(args)
                        .all(|(p, a)| p == a || (matches!(a, Ty::Null) && p.is_reference()))
            }) {
                return Some(m);
            }
        }
        // A constructor of a GENERIC class has erased `Object`/`Any` parameters; a reference arg widens to
        // `Any`, and a PRIMITIVE arg boxes to `Any` too (`Pair(1, 2)` → `Pair(Object, Object)`). Match the
        // erased ctor with both widenings (the exact-match check above already handled primitive-param
        // ctors like `Foo(Int)`).
        let widened: Vec<Ty> = args
            .iter()
            .map(|t| {
                if t.is_reference()
                    || matches!(
                        t,
                        Ty::Int
                            | Ty::Byte
                            | Ty::Short
                            | Ty::Long
                            | Ty::Float
                            | Ty::Double
                            | Ty::Boolean
                            | Ty::Char
                            | Ty::UInt
                            | Ty::ULong
                    )
                {
                    Ty::obj("kotlin/Any")
                } else {
                    *t
                }
            })
            .collect();
        self.constructors.iter().find(|m| m.params == widened)
    }

    /// The best companion member named `name` for `args` (exact, then boxing, then prefix).
    pub fn companion_member(&self, name: &str, args: &[Ty]) -> Option<&LibraryMember> {
        best_overload(self.companion.iter(), name, args)
    }

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

/// A primitive constant value read from a library (a `const`/`static final` field's compile-time
/// value), platform-agnostic so the front end can inline it like the reference compiler does.
/// A primitive constant value read from a library (a `const`/`static final` field's compile-time
/// value), platform-agnostic so the front end can inline it like the reference compiler does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LibConst {
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
}

#[derive(Clone, Copy, Debug, PartialEq)]
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
impl TargetRuntime for EmptySymbolSource {}

#[cfg(test)]
mod tests {
    use super::InlineKind;

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
}
