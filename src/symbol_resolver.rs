//! Call resolution — the binding layer that sits *above* a [`SymbolSource`].
//!
//! A [`SymbolSource`] is a pure, arg-INDEPENDENT metadata oracle: given a name (and optional receiver)
//! it returns every overload with its raw signature and flags ([`crate::libraries::FunctionSet`]). It
//! does no overload selection and no type-variable binding.
//!
//! [`SymbolResolver`] is the arg-DEPENDENT layer on top: given the actual argument types at a call site
//! it selects the right overload and binds the generic receiver/parameter/return types. It is platform
//! agnostic — it only ever talks to the oracle through the [`SymbolSource`] trait, so the same binding
//! logic serves every backend (JVM today, JS later). The platform-specific bits (parsing a backend's
//! generic-signature string into a signature `Ty`) live behind the trait; the binding *algorithm* over
//! those `Ty` nodes lives here.

use crate::libraries::{
    best_overload, CompilerPlatform, FnKind, FunctionInfo, FunctionSet, GenericSig, InlineKind,
    LibraryCallable, LibraryMember, Origin, PropKind,
};
use crate::symbol_source::SymbolSource;
use crate::types::Ty;

#[derive(Clone, Debug, Default)]
pub struct TopLevelLambdaShape {
    pub param_types: Option<Vec<Vec<Ty>>>,
    pub receivers: Option<Vec<Option<Ty>>>,
    pub materialized: Option<Vec<bool>>,
}

type GSigBinds = std::collections::HashMap<String, Ty>;

/// [`crate::assignable::TypeOracle`] over a federated [`SymbolSource`] (module ∪ classpath): the class
/// hierarchy walk the one assignability relation needs. Kotlin-name supertypes, no JVM canonicalization —
/// source-type space, as `source_receiver_rank` uses.
pub(crate) struct SourceOracle<'a>(pub &'a dyn SymbolSource);

impl crate::assignable::TypeOracle for SourceOracle<'_> {
    fn direct_supertypes(&self, internal: &str) -> Vec<String> {
        self.0
            .resolve_type(internal)
            .map(|t| t.supertypes)
            .unwrap_or_default()
    }
    fn value_underlying(&self, ty: Ty) -> Option<Ty> {
        self.0
            .resolve_type(ty.kotlin_class_internal()?)
            .and_then(|t| t.value_underlying)
    }
}

/// [`crate::assignable::TypeOracle`] over a [`CompilerPlatform`]: adds value-class underlying and the
/// target ABI class identity that library subtype checks rely on.
pub(crate) struct PlatformOracle<'a>(pub &'a dyn CompilerPlatform);

impl crate::assignable::TypeOracle for PlatformOracle<'_> {
    fn direct_supertypes(&self, internal: &str) -> Vec<String> {
        self.0
            .resolve_type(internal)
            .map(|t| t.supertypes)
            .unwrap_or_default()
    }
    fn value_underlying(&self, ty: Ty) -> Option<Ty> {
        self.0.value_underlying(ty)
    }
    fn canonical_class(&self, internal: &str) -> String {
        platform_class_identity(
            self.0
                .abi_value_form(Ty::obj(internal))
                .obj_internal()
                .unwrap_or(internal),
        )
    }
}

pub(crate) fn platform_class_identity(internal: &str) -> String {
    match internal.rfind('/') {
        Some(i) => format!("{}{}", &internal[..=i], internal[i + 1..].replace('.', "$")),
        None => internal.replace('.', "$"),
    }
}

/// The type arguments of a constructed generic type INFERRED from a construction's argument types
/// (`Pair(1, 2)` → `[Int, Int]`, so `Pair(1, 2)` types as `Pair<Int, Int>`). Each of the type's formal
/// parameters (`ty.type_params`) is bound by unifying the matching-arity constructor's parsed generic
/// parameter signatures against `arg_tys`; an unbound formal defaults to `Any`. `None` when the type is
/// non-generic or no constructor carries a generic signature to unify.
pub fn infer_constructor_type_args(
    ty: &crate::libraries::LibraryType,
    arg_tys: &[Ty],
) -> Option<Vec<Ty>> {
    if ty.type_params.is_empty() {
        return None;
    }
    let mut binds = GSigBinds::new();
    for ctor in &ty.constructors {
        let Some(gsig) = &ctor.generic_sig else {
            continue;
        };
        if gsig.params.len() != arg_tys.len() {
            continue;
        }
        for (p, a) in gsig.params.iter().zip(arg_tys) {
            unify_ty(*p, *a, &mut binds);
        }
        break;
    }
    if binds.is_empty() {
        return None;
    }
    Some(
        ty.type_params
            .iter()
            .map(|f| {
                binds
                    .get(f)
                    .copied()
                    .unwrap_or_else(|| Ty::obj("kotlin/Any"))
            })
            .collect(),
    )
}

/// Bind type variables by unifying a signature `Ty` (whose type variables are [`Ty::TyParam`]) against
/// an actual argument `Ty`.
pub(crate) fn unify_ty(sig: Ty, actual: Ty, binds: &mut GSigBinds) {
    match sig {
        Ty::TyParam(n, _) => {
            binds.entry(n.to_string()).or_insert(actual);
        }
        Ty::Fun(fsig) => {
            // A function parameter (`Function1<T, R>`) unifies against a lambda argument (`Ty::Fun`):
            // the parameter nodes bind the lambda's parameters and the return node binds its return, so
            // `map`'s `R` binds from the lambda body's type (`{ it * 2 }` → `Int`).
            if let Ty::Fun(afsig) = actual {
                // A SUSPEND SAM parameter (`suspend CoroutineScope.() -> T`) erases to
                // `Function2<CoroutineScope, Continuation<T>, Object>` — the RESULT type parameter `T`
                // lives inside the trailing `Continuation<T>`, and the JVM return node is `Object`. The
                // lambda argument, however, ERASES its own `Continuation` type argument (to `Any`) and
                // carries its real result in `afsig.ret`. Binding `T` from the erased `Continuation<Any>`
                // would fix it to `Any` (`runBlocking { … } : Any`, losing the block's type); bind it from
                // `afsig.ret` instead, and skip the `Continuation` param so it isn't double-unified.
                let value_params: &[Ty] = match fsig.params.last() {
                    Some(Ty::Obj(n, cargs))
                        if crate::types::same(n, crate::types::wk::continuation())
                            && !cargs.is_empty() =>
                    {
                        unify_ty(cargs[0], afsig.ret, binds);
                        &fsig.params[..fsig.params.len() - 1]
                    }
                    _ => &fsig.params,
                };
                for (a, p) in value_params.iter().zip(afsig.params.iter()) {
                    unify_ty(*a, *p, binds);
                }
                unify_ty(fsig.ret, afsig.ret, binds);
            }
        }
        Ty::Obj(_, args) => {
            // Unify the type arguments positionally against the actual's carried arguments, if any.
            if let Ty::Obj(_, targs) = actual {
                for (a, t) in args.iter().zip(targs.iter()) {
                    unify_ty(*a, *t, binds);
                }
            }
        }
        _ => {}
    }
}

/// Realize a signature `Ty` under the current bindings — a bound type variable substitutes to its
/// binding, an unbound one erases to `Any`; a class substitutes its carried type arguments in place.
pub(crate) fn ty_subst(sig: Ty, binds: &GSigBinds) -> Ty {
    match sig {
        Ty::TyParam(n, _) => binds
            .get(n)
            .copied()
            .unwrap_or_else(|| Ty::obj("kotlin/Any")),
        Ty::Fun(fsig) => Ty::fun(ty_subst_all(&fsig.params, binds), ty_subst(fsig.ret, binds)),
        Ty::Nullable(inner) => Ty::nullable(ty_subst(*inner, binds)),
        Ty::Obj(internal, args) if !args.is_empty() => {
            Ty::obj_args(internal, &ty_subst_all(args, binds))
        }
        _ => sig,
    }
}

pub(crate) fn ty_subst_all(sigs: &[Ty], binds: &GSigBinds) -> Vec<Ty> {
    sigs.iter().map(|s| ty_subst(*s, binds)).collect()
}

fn seeded_gsig_binds(gsig: &GenericSig, type_args: &[Ty]) -> GSigBinds {
    gsig.formals
        .iter()
        .cloned()
        .zip(type_args.iter().copied())
        .collect()
}

fn bind_gsig_return(
    gsig: &GenericSig,
    type_args: &[Ty],
    actuals: impl IntoIterator<Item = (Ty, Ty)>,
) -> Ty {
    let mut binds = seeded_gsig_binds(gsig, type_args);
    for (ps, a) in actuals {
        unify_ty(ps, a, &mut binds);
    }
    ty_subst(gsig.ret, &binds)
}

fn bind_ext_ret(gsig: &GenericSig, receiver: Ty, args: &[Ty], targs: &[Ty]) -> Ty {
    let mut binds = seeded_gsig_binds(gsig, targs);
    if let Some(recv_sig) = gsig.receiver {
        unify_ty(recv_sig, receiver, &mut binds);
    }
    for (ps, a) in gsig.params.iter().zip(args.iter().copied()) {
        unify_ty(*ps, a, &mut binds);
    }
    ty_subst(gsig.ret, &binds)
}

/// Bind an extension's generic return when OMITTED defaults leave the call args misaligned with the
/// signature's value params. With a trailing lambda the provided args are a prefix and the last arg fills
/// the LAST value-param (the omitted middle is skipped); otherwise the args are a leading prefix. Falls
/// back to the plain positional binder when the overload has no generic signature.
fn bind_defaulted_ext_ret(
    o: &FunctionInfo,
    receiver: Ty,
    args: &[Ty],
    targs: &[Ty],
    trailing_lambda: bool,
) -> Ty {
    let Some(gsig) = o.generic_sig.as_ref() else {
        return o.callable.ret;
    };
    let mut binds = seeded_gsig_binds(gsig, targs);
    if let Some(recv_sig) = gsig.receiver {
        unify_ty(recv_sig, receiver, &mut binds);
    }
    if trailing_lambda {
        let prefix = args.len().saturating_sub(1);
        for (ps, a) in gsig.params.iter().take(prefix).zip(args) {
            unify_ty(*ps, *a, &mut binds);
        }
        if let (Some(ls), Some(la)) = (gsig.params.last(), args.last()) {
            unify_ty(*ls, *la, &mut binds);
        }
    } else {
        for (ps, a) in gsig.params.iter().zip(args) {
            unify_ty(*ps, *a, &mut binds);
        }
    }
    ty_subst(gsig.ret, &binds)
}

/// If `sig` is a function type, the substituted types of its lambda parameters. Empty for anything else.
pub(crate) fn function_input_types(sig: Ty, binds: &GSigBinds) -> Vec<Ty> {
    match sig {
        Ty::Fun(fsig) => ty_subst_all(&fsig.params, binds),
        _ => Vec::new(),
    }
}

/// Whether argument `a` can be passed where parameter `p` is expected, in erased Kotlin terms: an
/// exact match, any argument into an erased `Any` parameter, or the *same erased class* (a parameter
/// `Pair` accepts an argument `Pair<Int, String>` — generic parameters erase to the raw type).
pub(crate) fn arg_fits(p: &Ty, a: &Ty) -> bool {
    // A lambda value fits a function-typed parameter when arities agree; its body result is handled by
    // the selected call's generic binding, not by erased descriptor matching. An erased `Any` parameter —
    // whether spelled `kotlin/Any` or its JVM form `java/lang/Object` (a generic vararg element erases to
    // it) — accepts any reference argument.
    p == a
        || matches!(p, Ty::Obj(n, _) if crate::types::same(n, crate::types::wk::any())
            || crate::types::same(n, crate::types::wk::java_object()))
        || matches!((p.fun_arity(), a.fun_arity()), (Some(pn), Some(an)) if pn == an)
        || matches!((p, a), (Ty::Obj(pi, _), Ty::Obj(ai, _)) if pi == ai)
}

fn arg_fits_platform(lib: &dyn CompilerPlatform, param: &Ty, arg: &Ty) -> bool {
    arg_fits(param, arg)
        || param
            .fun_arity()
            .zip(lib.function_like_arity(*arg))
            .is_some_and(|(p, a)| usize::from(p) == a)
}

/// Extension overloads of a receiver-filtered set, ordered most-specific-first by the SOURCE receiver rank
/// (the same `source_receiver_rank` the overload selector uses) rather than the provider's baked
/// `receiver_rank`. The provider ranks a primitive-array family by enumeration order — every `IntArray`/
/// `CharArray`/… overload ties at the array rung — so `IntArray.any`'s block parameter would tie with
/// `CharArray.any`'s and the wrong one (`(Char)->…`) could win. Ranking by the actual receiver drops the
/// non-applicable siblings (a `CharArray` extension does not apply to an `IntArray`) and keeps only the
/// exact match at rung 0.
fn ranked_extension_overloads_by_recv<'a>(
    src: &dyn SymbolSource,
    receiver: Ty,
    fs: &'a FunctionSet,
    allow_must_inline: bool,
) -> Vec<&'a FunctionInfo> {
    let mut out: Vec<(u32, &FunctionInfo)> = fs
        .overloads
        .iter()
        .filter(|o| {
            o.is_extension() && (o.public() || (allow_must_inline && o.flags.inline.must_inline()))
        })
        .filter_map(|o| {
            // The `functions()` provider labels every candidate with the QUERIED receiver, not its
            // own declared one — so `o.receiver` can't tell `IntArray.any` from `CharArray.any`. Rank by the
            // real declared receiver on the parsed generic signature; a candidate with no signature is
            // dropped (both callers gate on `generic_sig` anyway). This drops the non-applicable
            // primitive-array siblings that `o.receiver` would falsely tie at rung 0.
            let decl = o.generic_sig.as_ref().and_then(|g| g.receiver)?;
            source_receiver_rank(src, receiver, decl).map(|r| (r, o))
        })
        .collect();
    out.sort_by_key(|(r, _)| *r);
    out.into_iter().map(|(_, o)| o).collect()
}

/// Map each provided argument to a logical parameter index. Identity when the counts match; else, for a
/// call that omits leading defaulted parameters before a TRAILING lambda (`runBlocking { … }`), leading
/// args → leading params and the trailing lambda → the LAST parameter.
fn trailing_default_arg_indices(param_count: usize, arg_tys: &[Option<Ty>]) -> Option<Vec<usize>> {
    let n = arg_tys.len();
    if param_count == n {
        Some((0..n).collect())
    } else if param_count > n && n >= 1 && arg_tys[n - 1].is_none() {
        let mut map: Vec<usize> = (0..n - 1).collect();
        map.push(param_count - 1);
        Some(map)
    } else {
        None
    }
}

fn is_default_ctor_marker(ty: Ty) -> bool {
    matches!(
        ty,
        Ty::Obj("kotlin/jvm/internal/DefaultConstructorMarker", _)
    )
}

fn has_default_tail(params: &[Ty], mask_idx: usize, marker: impl FnOnce(Ty) -> bool) -> bool {
    params.len() == mask_idx + 2
        && params[mask_idx] == Ty::Int
        && params.get(mask_idx + 1).copied().is_some_and(marker)
}

fn callable_with_return(c: &LibraryCallable, ret: Ty, default_call: bool) -> LibraryCallable {
    LibraryCallable {
        ret,
        default_call,
        vararg_elem: None,
        ..c.clone()
    }
}

/// The arg-dependent binding layer over a [`SymbolSource`]: it selects overloads and binds generics for
/// a specific call site. Holds the oracle by reference — cheap to construct per query.
pub struct SymbolResolver<'a> {
    /// The classpath platform — used ONLY for `TargetRuntime`/emit concerns (descriptors, value-class
    /// underlying, receiver-rank). Symbol RESOLUTION never goes through it directly; it goes through `src`.
    lib: &'a dyn CompilerPlatform,
    /// The aggregated resolution source: the current MODULE over the CLASSPATH (module shadows a library
    /// declaration of the same name). Every `resolve_symbols`/`resolve_type` query federates both.
    src: crate::symbol_source::CompositeSource<'a>,
    /// The packages in scope for TOP-LEVEL function resolution (same-package, star/explicit imports,
    /// defaults). `None` disables the filter (a context with no import scope — signature inference).
    /// When `Some`, a top-level function resolves only if its facade's package is in scope, matching
    /// kotlinc: an unqualified top-level call binds ONLY to an imported/same-package/default function,
    /// not to any classpath function of that name.
    fn_scope: Option<&'a [String]>,
}

/// The receiver of a reference: a VALUE of some type (`x.name`), or a named TYPE (`Type(args)`,
/// `Type.name(args)`). Reports only the receiver the caller already resolved.
#[derive(Clone, Copy)]
pub enum SymRecv<'q> {
    Value(Ty),
    Type(&'q str),
    /// No receiver — a plain `name(args)` resolved against the import scope's top-level (and
    /// same-facade extension) functions.
    TopLevel,
}

/// What a name DENOTES on its receiver — the declared thing the resolver found, NOT how it is used.
/// [`SymbolResolver::resolve_symbol`] resolves a name to one of these; the CALLER then applies whatever
/// its syntax needs (invoke it, read it, write its setter, take a reference), including handling a
/// mismatch itself (`Test()` where `Test` is a property — the caller emits an `invoke`). The resolver
/// does not care whether the site is a call, a read, a write, or a reference.
/// The facets a `recv.name` member supports — see [`Symbol::Member`]. Boxed into the enum so a member
/// symbol stays pointer-sized.
pub struct MemberFacets {
    pub call: Option<ResolvedMember>,
    pub read: Option<ResolvedMember>,
    pub write: Option<LibraryCallable>,
    pub method_ref: Option<LibraryMember>,
    pub property_ref: Option<BoundPropertyRef>,
    /// Every overload named `name` applicable to the receiver — instance members, operators, AND in-scope
    /// extension functions with a matching receiver — most-derived/member-first. A caller inspecting the
    /// whole family (named-arg mapping, defaults, return agreement, member-vs-extension dispatch) filters
    /// this by [`FunctionInfo::kind`]/`receiver_rank`.
    pub overloads: Vec<FunctionInfo>,
}

pub enum Symbol {
    /// A member of a value receiver `recv.name`, with whichever facets the declaration supports. A name
    /// may support several at once — a Java zero-argument method (`list.size`, `str.length`) is both a
    /// property `read` and a `call`/method `reference` — so the resolver reports them all and the caller
    /// takes the one its syntax needs (`recv.name(args)` → `call`, `recv.name` → `read`, `recv.name = v`
    /// → `write`, `recv::name` → `method_ref`/`property_ref`).
    Member(Box<MemberFacets>),
    /// An object/companion instance member `Type.name(args)`.
    Instance(LibraryMember),
    /// A static/companion member `Type.name(args)`.
    Companion(LibraryMember),
    /// A constructor `Type(args)`.
    Constructor(LibraryMember),
    /// A synthesized (value-class / default-argument) constructor.
    SyntheticConstructor(SyntheticCtorCall),
}

impl Symbol {
    /// This name invoked as a method with the resolved arguments (`recv.name(args)`).
    pub fn call(self) -> Option<ResolvedMember> {
        match self {
            Symbol::Member(f) => f.call,
            _ => None,
        }
    }
    /// This name read as a property (`recv.name`).
    pub fn property(self) -> Option<ResolvedMember> {
        match self {
            Symbol::Member(f) => f.read,
            _ => None,
        }
    }
    /// The setter of this property (`recv.name = v`).
    pub fn property_setter(self) -> Option<LibraryCallable> {
        match self {
            Symbol::Member(f) => f.write,
            _ => None,
        }
    }
    /// A bound method reference to this name (`recv::name`).
    pub fn method_ref(self) -> Option<LibraryMember> {
        match self {
            Symbol::Member(f) => f.method_ref,
            _ => None,
        }
    }
    /// A bound property reference to this name (`recv::name`).
    pub fn property_ref(self) -> Option<BoundPropertyRef> {
        match self {
            Symbol::Member(f) => f.property_ref,
            _ => None,
        }
    }
    /// Every overload named this on the receiver — members, operators, and applicable in-scope extensions.
    pub fn overloads(self) -> Vec<FunctionInfo> {
        match self {
            Symbol::Member(f) => f.overloads,
            _ => Vec::new(),
        }
    }
    /// The object/companion instance member this resolved to (`Type.name(args)`).
    pub fn instance(self) -> Option<LibraryMember> {
        if let Symbol::Instance(m) = self {
            Some(m)
        } else {
            None
        }
    }
    /// The static/companion member this resolved to (`Type.name(args)`).
    pub fn companion(self) -> Option<LibraryMember> {
        if let Symbol::Companion(m) = self {
            Some(m)
        } else {
            None
        }
    }
    /// The constructor this resolved to (`Type(args)`).
    pub fn constructor(self) -> Option<LibraryMember> {
        if let Symbol::Constructor(m) = self {
            Some(m)
        } else {
            None
        }
    }
    /// The synthesized constructor this resolved to (`Type(args)`).
    pub fn synthetic_constructor(self) -> Option<SyntheticCtorCall> {
        if let Symbol::SyntheticConstructor(s) = self {
            Some(s)
        } else {
            None
        }
    }
}

impl<'a> SymbolResolver<'a> {
    pub fn new(lib: &'a dyn CompilerPlatform) -> Self {
        SymbolResolver {
            lib,
            src: crate::symbol_source::CompositeSource::new(vec![lib as &dyn SymbolSource]),
            fn_scope: None,
        }
    }

    /// A resolver whose top-level function resolution is restricted to `fn_scope`'s packages.
    pub fn new_scoped(lib: &'a dyn CompilerPlatform, fn_scope: &'a [String]) -> Self {
        SymbolResolver {
            lib,
            src: crate::symbol_source::CompositeSource::new(vec![lib as &dyn SymbolSource]),
            fn_scope: Some(fn_scope),
        }
    }

    /// The primary resolver: symbol resolution federates the current `module` over the classpath `lib`.
    pub fn new_scoped_with_module(
        lib: &'a dyn CompilerPlatform,
        module: &'a dyn SymbolSource,
        fn_scope: &'a [String],
    ) -> Self {
        SymbolResolver {
            lib,
            src: crate::symbol_source::CompositeSource::new(vec![module, lib as &dyn SymbolSource]),
            fn_scope: Some(fn_scope),
        }
    }

    /// Whether `internal` names a `@JvmInline value`/inline class — resolved through the FEDERATED source
    /// (the current module over the classpath), so an in-file value class and a classpath one answer alike.
    /// The one authority for value-class-ness; callers ask the resolver, not a `SymbolSource` directly.
    pub fn is_value(&self, internal: &str) -> bool {
        self.src.is_value(internal)
    }

    /// The single-field UNDERLYING type of the value class named `internal` (`Result` → `Object`), resolved
    /// through the federated source. `None` if not a value class this resolver knows.
    pub fn value_underlying(&self, internal: &str) -> Option<Ty> {
        self.src
            .resolve_type(internal)
            .and_then(|t| t.value_underlying)
    }

    /// The unqualified-name resolution loop for this resolver's import scope — `resolve_symbols` per
    /// candidate fqn `pkg/name` over the federated source. THE way to resolve an unqualified name: the
    /// caller extracts `classifier`, `callables.functions` (∪ classifier constructors, then `invoke`), or
    /// `callables.properties` from the records. Empty when there is no import scope (caller falls back).
    fn symbols_in_scope(&self, name: &str) -> Vec<(String, crate::libraries::ResolvedSymbols)> {
        self.fn_scope
            .map(|scope| resolve_symbols_in_scope(&self.src, name, scope))
            .unwrap_or_default()
    }

    /// Classify a type name — the ONE type query. `internal` → its [`LibraryType`] (a class/object/
    /// interface shape), or `None` for an unknown name. The type-side counterpart of [`resolve_symbol`].
    pub fn resolve_type(&self, internal: &str) -> Option<crate::libraries::LibraryType> {
        self.src.resolve_type(internal)
    }

    /// Resolve a name on a receiver to the thing it DENOTES — a member, a property, a companion/instance
    /// member, or a constructor — WITHOUT being told how the site uses it. The resolver does not care
    /// whether the caller is going to call it, read it, write it, or take a reference; it just says what
    /// the name is. The caller applies its own syntax to the returned [`Symbol`] (invoke the callable,
    /// read the property, use its setter, take a reference) and handles any mismatch itself (a `Type()`
    /// whose type has no constructor, an `invoke` on a property, …). `args` select a callable overload /
    /// constructor; they do not change WHAT the name is. This and [`resolve_type`] are the resolver's two
    /// resolution entry points.
    pub fn resolve_symbol(&self, recv: SymRecv, name: &str, args: &[Ty]) -> Option<Symbol> {
        match recv {
            SymRecv::Value(ty) => {
                // Resolve every facet the name supports on this receiver; a name can support several (a
                // Java zero-arg method is a property read AND a callable). Each facet is exactly the
                // former per-use resolution, so the caller's chosen facet behaves as before.
                let call = resolve_instance_member(self.lib, ty, name, args);
                let read = resolve_property_member(self.lib, ty, name);
                let write = resolve_property_setter(self.lib, ty, name);
                let method_ref = resolve_instance_ref(self.lib, ty, name);
                let property_ref = resolve_property_ref(self.lib, ty, name);
                // EVERY overload named `name` applicable to the receiver: instance members and operators
                // (the receiver-aware member query, federated over module + libraries) UNION the in-scope
                // extension functions whose declared receiver is in the receiver's supertype closure. This
                // is the whole candidate family `select_overload` picks from — a caller inspecting the set
                // (named-argument mapping, default-argument selection, return agreement, member-vs-extension
                // dispatch) reads it here and filters by `kind`/`receiver_rank` as it needs.
                let mut overloads = self.src.member_overloads(ty, name).overloads;
                if let Some(scope) = self.fn_scope {
                    overloads.extend(
                        function_set_from_symbols(resolve_symbols_in_scope(&self.src, name, scope))
                            .overloads
                            .into_iter()
                            .filter(|o| {
                                o.is_extension()
                                    && o.receiver
                                        .and_then(|dr| source_receiver_rank(&self.src, ty, dr))
                                        .is_some()
                            }),
                    );
                }
                if call.is_none()
                    && read.is_none()
                    && write.is_none()
                    && method_ref.is_none()
                    && property_ref.is_none()
                    && overloads.is_empty()
                {
                    return None;
                }
                Some(Symbol::Member(Box::new(MemberFacets {
                    call,
                    read,
                    write,
                    method_ref,
                    property_ref,
                    overloads,
                })))
            }
            SymRecv::TopLevel => {
                // A receiver-less name: its top-level (and same-facade extension) overloads, from the ONE
                // `resolve_symbols` seam over the import scope. The caller filters by `FnKind` and selects.
                let overloads = function_set_from_symbols(self.symbols_in_scope(name)).overloads;
                if overloads.is_empty() {
                    return None;
                }
                Some(Symbol::Member(Box::new(MemberFacets {
                    call: None,
                    read: None,
                    write: None,
                    method_ref: None,
                    property_ref: None,
                    overloads,
                })))
            }
            SymRecv::Type(internal) => {
                if name.is_empty() {
                    // `Type(args)` — the type's constructor, real or synthesized.
                    resolve_constructor(self.lib, internal, args)
                        .map(Symbol::Constructor)
                        .or_else(|| {
                            resolve_synthetic_constructor(self.lib, internal, args)
                                .map(Symbol::SyntheticConstructor)
                        })
                } else {
                    // `Type.name(args)` — an object/companion instance member, else a static/companion
                    // member. The resolver discovers which.
                    resolve_instance(self.lib, internal, name, args)
                        .map(Symbol::Instance)
                        .or_else(|| {
                            resolve_companion(self.lib, internal, name, args).map(Symbol::Companion)
                        })
                }
            }
        }
    }

    /// Resolve a receiver-less top-level library callable for a concrete call site. This is the
    /// compatibility boundary for the older arg-dependent selector while checker/lowerer are moved to
    /// `FunctionSet`-backed resolution.
    pub fn resolve_top_level_callable(
        &self,
        name: &str,
        args: &[Ty],
        type_args: &[Ty],
    ) -> Option<LibraryCallable> {
        // FQN-driven resolution: union `resolve_symbols`' callables over the in-scope packages (the ONE
        // query), then keep the top-level overloads below.
        let fs = FunctionSet {
            overloads: self
                .resolve_symbol(SymRecv::TopLevel, name, &[])
                .map(Symbol::overloads)
                .unwrap_or_default(),
        };
        self.pick_top_level(name, &fs, args, type_args)
    }

    /// Resolve a FULLY-QUALIFIED top-level call `pkg.name(args)` where `pkg` is a package path the source
    /// wrote explicitly (`kotlin.math.max`, `kotlinx.coroutines.runBlocking`). The name need NOT be in the
    /// import scope — a FQ reference names its package directly — so overloads come from `resolve_symbols`
    /// on the ONE `pkg` (the FQN seam), not from the in-scope union.
    pub fn resolve_top_level_callable_in_package(
        &self,
        name: &str,
        pkg: &str,
        args: &[Ty],
        type_args: &[Ty],
    ) -> Option<LibraryCallable> {
        let scope = [pkg.to_string()];
        let fs = function_set_from_symbols(resolve_symbols_in_scope(&self.src, name, &scope));
        self.pick_top_level(name, &fs, args, type_args)
    }

    /// Overload-resolve a top-level call against an already-built [`FunctionSet`] (from the in-scope union
    /// or an explicit FQ package). Shared tail of [`Self::resolve_top_level_callable`].
    fn pick_top_level(
        &self,
        name: &str,
        fs: &FunctionSet,
        args: &[Ty],
        type_args: &[Ty],
    ) -> Option<LibraryCallable> {
        let parsed: Vec<(&FunctionInfo, Vec<Ty>, Ty)> = fs
            .top_level()
            .filter(|o| o.public())
            .map(|o| (o, o.callable.params.clone(), o.callable.ret))
            .collect();
        let fits = |p: &Ty, a: &Ty| arg_fits_platform(self.lib, p, a);

        let pick = parsed
            .iter()
            .find(|(_, params, _)| {
                params.len() == args.len() && params.iter().zip(args).all(|(p, a)| fits(p, a))
            })
            .or_else(|| {
                parsed.iter().find(|(_, params, _)| {
                    if params.is_empty() {
                        return args.is_empty();
                    }
                    let fixed = params.len() - 1;
                    let Some(elem) = params[fixed].array_elem() else {
                        return false;
                    };
                    args.len() >= fixed
                        && params[..fixed].iter().zip(args).all(|(p, a)| fits(p, a))
                        && args[fixed..].iter().all(|a| fits(&elem, a))
                })
            });

        if pick.is_none() {
            if let Some(c) = self.resolve_top_level_default_callable(name, args, type_args) {
                crate::trace_compiler!(
                    "resolve",
                    "top-level {name} args={args:?} -> {}.{}{} default inline={:?}",
                    c.owner,
                    c.name,
                    c.descriptor,
                    c.inline
                );
                return Some(c);
            }
        }

        if let Some(c) = self.resolve_top_level_inline_only_callable(fs, args, type_args) {
            crate::trace_compiler!(
                "resolve",
                "top-level {name} args={args:?} -> {}.{}{} inline-only",
                c.owner,
                c.name,
                c.descriptor
            );
            return Some(c);
        }

        let (o, params, ret) = pick?;
        let c = &o.callable;
        if ret.obj_internal() == Some("kotlin/reflect/KType") {
            return None;
        }

        let mut vararg_elem = None;
        let ret_ty = o
            .generic_sig
            .as_ref()
            .map(|gsig| {
                let mut binds = seeded_gsig_binds(gsig, type_args);
                // A vararg call binds `T` from the ELEMENTS, not from the array param. Detect it by the
                // trailing array parameter receiving element-wise args — NOT merely by arity: a SINGLE
                // element (`listOf(pair)`) has `params.len() == args.len()`, yet still spreads into the
                // vararg, so a plain `zip` would unify `Array<T>` against the non-array `Pair` and leave
                // `T` unbound (→ `List<Any>`). A spread (`listOf(*arr)`) passes the array itself — same
                // arity AND the last arg IS the array param — so it is not a vararg here.
                let vararg = params.last().is_some_and(|p| p.array_elem().is_some())
                    && (params.len() != args.len() || args.last() != params.last());
                if vararg && !gsig.params.is_empty() {
                    let fixed = gsig.params.len() - 1;
                    for (i, ps) in gsig.params.iter().take(fixed).enumerate() {
                        if let Some(a) = args.get(i) {
                            unify_ty(*ps, *a, &mut binds);
                        }
                    }
                    if let Some(inner) = gsig.params[fixed].array_elem() {
                        for a in &args[fixed..] {
                            unify_ty(inner, *a, &mut binds);
                        }
                        vararg_elem = Some(ty_subst(inner, &binds));
                    }
                } else {
                    for (ps, a) in gsig.params.iter().zip(args) {
                        unify_ty(*ps, *a, &mut binds);
                    }
                }
                ty_subst(gsig.ret, &binds)
            })
            .unwrap_or(*ret);
        let ret_ty = o.ret.apply(if o.flags.suspend { c.ret } else { ret_ty });

        crate::trace_compiler!(
            "resolve",
            "top-level {name} args={args:?} -> {}.{}{} inline={:?}",
            c.owner,
            c.name,
            c.descriptor,
            c.inline
        );
        Some(LibraryCallable {
            params: params.clone(),
            ret: ret_ty,
            physical_ret: *ret,
            default_call: false,
            vararg_elem,
            ..c.clone()
        })
    }

    /// Resolve an extension library callable for a concrete receiver call site. The primary path uses
    /// the receiver-aware [`FunctionSet`] overloads; the compatibility fallback preserves the old
    /// descriptor/default-argument handling until those cases are represented directly in `FunctionInfo`.
    pub fn resolve_extension_callable(
        &self,
        name: &str,
        receiver: Ty,
        args: &[Ty],
        type_args: &[Ty],
    ) -> Option<LibraryCallable> {
        // One extension resolution admits `@InlineOnly` (`must_inline`) candidates: a regular and an
        // inline call resolve identically; only the emitter differs (it splices when `inline` is set).
        let o = select_overload(
            self.lib,
            receiver,
            name,
            args,
            type_args,
            FnKind::Extension,
            ExtCtx {
                allow_must_inline: true,
                fn_scope: self.fn_scope,
            },
        )?;
        // A same-module extension is emitted through the module-native path (the lowerer's `ext_fun_ids`
        // / IR inliner), NOT as a resolved LIBRARY callable — its facade is the file being compiled, which
        // has no emit owner here. Only a classpath extension yields a library callable for the emit seam.
        if matches!(o.callable.origin, Origin::Module { .. }) {
            return None;
        }
        self.build_extension_callable(name, receiver, args, type_args, &o)
    }

    /// Resolve an extension callable for the bytecode inliner. Same overload selection as ordinary extension
    /// calls, but also admits non-public `@InlineOnly` candidates (the caller splices, never emits a call).
    pub fn resolve_extension_inline_callable(
        &self,
        name: &str,
        receiver: Ty,
        args: &[Ty],
    ) -> Option<LibraryCallable> {
        let o = select_overload(
            self.lib,
            receiver,
            name,
            args,
            &[],
            FnKind::Extension,
            ExtCtx {
                allow_must_inline: true,
                fn_scope: self.fn_scope,
            },
        )?;
        // Same-module extensions emit through the module path, not the library callable seam (see
        // [`Self::resolve_extension_callable`]).
        if matches!(o.callable.origin, Origin::Module { .. }) {
            return None;
        }
        self.build_extension_callable(name, receiver, args, &[], &o)
    }

    /// Shape a selected extension overload into a [`LibraryCallable`] for the call site. An EXACT call binds
    /// the generic return directly. A call that OMITS trailing defaults picks the emit form by a Kotlin ABI
    /// fact — an `inline` function has no `$default` synthetic (kotlinc materializes defaults by inlining),
    /// so it becomes a MUST-INLINE splice; a non-`inline` one binds the `name$default` synthetic (the
    /// backend appends placeholders + a bit-mask).
    fn build_extension_callable(
        &self,
        name: &str,
        receiver: Ty,
        args: &[Ty],
        type_args: &[Ty],
        o: &FunctionInfo,
    ) -> Option<LibraryCallable> {
        let vparams = logical_value_params(self.lib, o, receiver, type_args);
        if vparams.len() == args.len() {
            let c = &o.callable;
            let ret_ty = o
                .generic_sig
                .as_ref()
                .map(|gsig| bind_ext_ret(gsig, receiver, args, type_args))
                .unwrap_or(c.ret);
            let ret_class = o
                .ret
                .class
                .filter(|meta| self.lib.value_underlying(*meta).is_some());
            let ret_ty2 = o.ret.apply_with_class(ret_class, ret_ty);
            crate::trace_compiler!(
                "resolve",
                "bind_extension_callable {}.{} gsig={} type_args={type_args:?} ret_ty={ret_ty:?} -> {ret_ty2:?}",
                c.owner,
                c.name,
                o.generic_sig.is_some()
            );
            return Some(callable_with_return(c, ret_ty2, false));
        }
        // Defaulted call — omitted trailing/middle params. Bind the return with default-aware alignment.
        let trailing_lambda = args.last().is_some_and(|a| matches!(a, Ty::Fun(_)));
        let ret_ty = o.ret.apply(bind_defaulted_ext_ret(
            o,
            receiver,
            args,
            type_args,
            trailing_lambda,
        ));
        // Prefer a real `name$default` synthetic when it exists — even for an `inline` function. Many
        // `inline` stdlib/coroutine functions (`Mutex.withLock`) also emit a `$default` callable (the
        // `$$forInline` variant is what kotlinc splices); calling `$default` threads the `Continuation`
        // through the ordinary suspend machinery instead of splicing a suspend body. Splice (MUST-INLINE)
        // only when there is NO `$default` synthetic — a genuine `@InlineOnly` callee with no call target.
        if let Some(c) = self.default_synthetic_callable(name, o, args) {
            crate::trace_compiler!(
                "resolve",
                "extension defaulted ($default) {name} recv={receiver:?} args={args:?} -> {}.{}{} ret={ret_ty:?}",
                c.owner,
                c.name,
                c.descriptor
            );
            return Some(callable_with_return(&c, ret_ty, true));
        }
        if o.flags.inline.can_inline() {
            let mut callable = callable_with_return(&o.callable, ret_ty, true);
            callable.inline = crate::libraries::InlineKind::MustInline;
            crate::trace_compiler!(
                "resolve",
                "extension defaulted (inline) {name} recv={receiver:?} args={args:?} -> {}.{}{} ret={ret_ty:?}",
                callable.owner,
                callable.name,
                callable.descriptor
            );
            return Some(callable);
        }
        None
    }

    /// Resolve a classpath/library extension property getter for `receiver.property`.
    /// The source supplies the platform getter spelling (`getProperty` on JVM); this layer then uses
    /// the same extension-call selector as ordinary extension calls and returns only read-value results.
    pub fn resolve_extension_property_getter(
        &self,
        property: &str,
        receiver: Ty,
    ) -> Option<LibraryCallable> {
        // Resolve the extension property through the ONE query — union `resolve_symbols`' property overloads
        // over the import scope. Its getter is the REAL `@Metadata` accessor (public-facade owner, exact
        // `JvmPropertySignature` name) — never a `getX` guess. Pick the most-specific applicable receiver
        // rung. The getter's `ret` is already the property's declared type (normalized — a primitive stays
        // `Ty::Int`, not a boxed `kotlin/Int`), so it is used directly.
        let p = self
            .symbols_in_scope(property)
            .into_iter()
            .flat_map(|(_, r)| match r.callables {
                crate::libraries::Callables::Properties(p) => p.overloads,
                _ => Vec::new(),
            })
            .filter(|p| p.kind == PropKind::Extension)
            .filter_map(|p| {
                let decl_recv = ty_subst(p.receiver?, &std::collections::HashMap::new());
                let rank = source_receiver_rank(&self.src, receiver, decl_recv)?;
                Some((rank, p))
            })
            .min_by_key(|(rank, _)| *rank)?
            .1;
        Some(p.getter).filter(|c| c.ret.is_read_value_result())
    }

    /// Find the `name$default` synthetic callable for a defaulted extension call — the emit-shaped callable
    /// (receiver at `params[0]`, all real params present) the backend fills with placeholders.
    fn default_synthetic_callable(
        &self,
        name: &str,
        base: &FunctionInfo,
        args: &[Ty],
    ) -> Option<LibraryCallable> {
        let trailing_lambda = args.last().is_some_and(|a| matches!(a, Ty::Fun(_)));
        // The `name$default` synthetic is a JVM static on a facade, reachable only through the static-method
        // index (NOT `resolve_type`, which reads a class's members not a facade's statics). Surface it via
        // the scope-pruned `top_level_function_set`, which truncates the trailing `(int mask, Object marker)`
        // so the emit shape is `[receiver, real…]`. Matching is by the base overload's leading RECEIVER
        // parameter (`params[0]`), NOT owner: a value-class receiver (`UIntArray`) erases its `$default` to
        // the UNDERLYING array facade (`ArraysKt.copyInto$default([I…)`, receiver `[I`) — the same erased
        // shape the base carries — so the plain-array `$default` binds and the value-class emit pass is not
        // engaged, exactly as the removed receiver-indexed `functions(…, Some(recv))` lookup resolved it.
        let fs = self
            .resolve_symbol(SymRecv::TopLevel, &format!("{name}$default"), &[])
            .map(Symbol::overloads)
            .unwrap_or_default();
        for o in &fs {
            if !o.public() && !o.flags.inline.must_inline() {
                continue;
            }
            let params = &o.callable.params;
            if params.is_empty() {
                continue;
            }
            if base.callable.params.first() != params.first() {
                continue;
            }
            let real_count = params.len() - 1;
            let fits = if trailing_lambda {
                let prefix_len = args.len() - 1;
                prefix_len < real_count
                    && matches!(params[real_count], Ty::Fun(_))
                    && params[1..1 + prefix_len]
                        .iter()
                        .zip(&args[..prefix_len])
                        .all(|(p, a)| self.arg_fits_or_subtype(p, a))
            } else {
                args.len() <= real_count
                    && params[1..1 + args.len()]
                        .iter()
                        .zip(args)
                        .all(|(p, a)| self.arg_fits_or_subtype(p, a))
            };
            if fits {
                return Some(o.callable.clone());
            }
        }
        None
    }

    fn arg_fits_or_subtype(&self, param: &Ty, arg: &Ty) -> bool {
        arg_fits_platform(self.lib, param, arg)
            || crate::assignable::is_assignable(
                &crate::assignable::TyCtx::new(),
                &SourceOracle(&self.src),
                *arg,
                *param,
            )
    }

    fn default_arg_mapping(
        &self,
        info: &FunctionInfo,
        params: &[Ty],
        args: &[Ty],
    ) -> Option<Vec<(usize, usize)>> {
        let real_count = params.len();
        let sig = &info.call_sig;
        if args.len() > real_count {
            return None;
        }
        let fits = |p: &Ty, a: &Ty| arg_fits_platform(self.lib, p, a);
        let trailing_lambda = args.last().is_some_and(|a| matches!(a, Ty::Fun(_)));
        if trailing_lambda && args.len() < real_count {
            let last_param = real_count.checked_sub(1)?;
            if !fits(&params[last_param], args.last().unwrap()) {
                return None;
            }
            let prefix_len = args.len() - 1;
            if !params[..prefix_len]
                .iter()
                .zip(&args[..prefix_len])
                .all(|(p, a)| fits(p, a))
            {
                return None;
            }
            if sig.has_known_required_param(prefix_len..last_param) {
                return None;
            }
            let mut mapping: Vec<(usize, usize)> = (0..prefix_len).map(|i| (i, i)).collect();
            mapping.push((last_param, args.len() - 1));
            return Some(mapping);
        }
        if !params[..args.len()]
            .iter()
            .zip(args)
            .all(|(p, a)| fits(p, a))
        {
            return None;
        }
        if sig.has_known_required_param(args.len()..real_count) {
            return None;
        }
        Some((0..args.len()).map(|i| (i, i)).collect())
    }

    fn resolve_top_level_default_callable(
        &self,
        name: &str,
        args: &[Ty],
        type_args: &[Ty],
    ) -> Option<LibraryCallable> {
        let fsd = FunctionSet {
            overloads: self
                .resolve_symbol(SymRecv::TopLevel, &format!("{name}$default"), &[])
                .map(Symbol::overloads)
                .unwrap_or_default(),
        };
        for o in fsd.top_level() {
            let c = &o.callable;
            if !o.public() && !o.flags.inline.must_inline() {
                continue;
            }
            let params = &c.params;
            let Some(mapping) = self.default_arg_mapping(o, params, args) else {
                continue;
            };
            // A `$default` synthetic usually carries NO generic `Signature` (it isn't API), so binding the
            // return type parameter off it fails and the erased `Object` return leaks (`runBlocking { … }`
            // → `Any`, losing the block's result type). Fall back to the BASE function's gsig — its leading
            // real parameters (and their type-parameter positions) align with the `$default`'s, so unifying
            // the provided args against it recovers `T` (`runBlocking<T>(block: () -> T): T` → `T = Ch`).
            let base_gsig = o.generic_sig.clone().or_else(|| {
                // The `$default` (krusty models it with the REAL params, no mask/marker) shares its base
                // function's parameter shape, so a SAME-ARITY base overload's generic signature applies.
                // Among same-arity candidates, prefer one whose return is a bare type PARAMETER (the
                // generic `fun <T> …(): T` form we need to bind), so a same-name/same-arity non-generic
                // sibling doesn't cross-bind.
                let bases: Vec<FunctionInfo> = FunctionSet {
                    overloads: self
                        .resolve_symbol(SymRecv::TopLevel, name, &[])
                        .map(Symbol::overloads)
                        .unwrap_or_default(),
                }
                .into_top_level()
                .filter(|b| b.generic_sig.is_some() && b.callable.params.len() == params.len())
                .collect();
                bases
                    .iter()
                    .find(|b| b.generic_sig.as_ref().is_some_and(|g| g.ret.is_ty_param()))
                    .or_else(|| bases.first())
                    .and_then(|b| b.generic_sig.clone())
            });
            let ret_ty = base_gsig
                .as_ref()
                .map(|gsig| {
                    bind_gsig_return(
                        gsig,
                        type_args,
                        mapping.iter().filter_map(|(param_i, arg_i)| {
                            gsig.params.get(*param_i).map(|ps| (*ps, args[*arg_i]))
                        }),
                    )
                })
                .unwrap_or(c.ret);
            crate::trace_compiler!(
                "resolve",
                "top_level_default {name} base_gsig={} mapping={mapping:?} -> ret={ret_ty:?}",
                base_gsig.is_some()
            );
            let ret_ty = o.ret.apply(ret_ty);
            return Some(callable_with_return(c, ret_ty, true));
        }
        None
    }

    fn resolve_top_level_inline_only_callable(
        &self,
        fs: &FunctionSet,
        args: &[Ty],
        type_args: &[Ty],
    ) -> Option<LibraryCallable> {
        for o in fs.top_level() {
            let c = &o.callable;
            let params = &c.params;
            if !c.inline.must_inline() {
                continue;
            }
            if params.len() != args.len()
                || !params
                    .iter()
                    .zip(args)
                    .all(|(p, a)| self.arg_fits_or_subtype(p, a))
            {
                continue;
            }
            let recovered = o
                .generic_sig
                .as_ref()
                .map(|gsig| {
                    bind_gsig_return(
                        gsig,
                        type_args,
                        gsig.params.iter().copied().zip(args.iter().copied()),
                    )
                })
                .unwrap_or(c.ret);
            let logical_ret = o.ret.apply(recovered);
            let mut callable = callable_with_return(c, logical_ret, false);
            callable.inline = InlineKind::MustInline;
            return Some(callable);
        }
        None
    }

    /// Resolve a single-selector `@OverloadResolutionByLambdaReturnType` call (`sumOf { … }`): pick the
    /// overload on `receiver` whose return type equals the lambda's return type. The candidate set (with
    /// its per-overload disambiguation) comes entirely from the one `functions` query.
    /// Resolve `receiver.name(lambda)` where the return type binds from the lambda's return. Returns the
    /// callable plus `is_member` — `true` ⇒ an instance member (lower as `invokevirtual` with the
    /// receiver as the dispatch receiver), `false` ⇒ an extension (lower as a static call with the
    /// receiver as the first argument).
    pub fn resolve_lambda_return_overload(
        &self,
        receiver: Ty,
        name: &str,
        lambda_ret: Ty,
        arg_tys: &[Ty],
    ) -> Option<(LibraryCallable, bool)> {
        if arg_tys.len() != 1 {
            return None;
        }
        // The matched overload's KIND decides how the caller lowers it: an EXTENSION's receiver is the
        // first argument of a static method (`Callee::Static`, receiver as `args[0]`), but an instance
        // MEMBER's receiver is the dispatch receiver (`Callee::Virtual`, `invokevirtual`). Conflating
        // them — emitting a member static with the receiver as an argument — leaves the receiver on the
        // operand stack (`VerifyError: Inconsistent stackmap frames`), which is exactly what a classpath
        // instance member taking a trailing lambda hit. Return the kind so the caller branches.
        // Candidates from the scope-pruned `resolve_symbols` seam, NOT the plain `functions(name, …)`
        // walk: a `@OverloadResolutionByLambdaReturnType` family carries a `@JvmName` (`sumOf` →
        // `sumOfInt`) that the Kotlin name never matches, so the walk returns nothing and the call
        // bails ("not yet supported by the IR backend"). This mirrors `lambda_return_overload_param_types`.
        // Fall back to the receiver-indexed `functions()` only when there is no import scope.
        FunctionSet { overloads: self.resolve_symbol(SymRecv::TopLevel, name, &[]).map(Symbol::overloads).unwrap_or_default() }
            .overloads
            .into_iter()
            .find(|o| {
                matches!(o.kind, FnKind::Member | FnKind::Extension)
                    && !matches!(o.callable.origin, Origin::Module { .. })
                    && o.callable.ret == lambda_ret
                    && match o.kind {
                        FnKind::Extension => o.receiver.is_none_or(|dr| {
                            source_receiver_rank(&self.src, receiver, dr).is_some()
                        }),
                        _ => true,
                    }
            })
            .map(|o| {
                crate::trace_compiler!(
                    "resolve",
                    "lambda-return {name} recv={receiver:?} lambda_ret={lambda_ret:?} -> {}.{}{} kind={:?}",
                    o.callable.owner,
                    o.callable.name,
                    o.callable.descriptor,
                    o.kind
                );
                let is_member = o.kind == FnKind::Member;
                (o.callable, is_member)
            })
    }

    /// Parameter types for the lambda argument of a call selected by lambda return type
    /// (`Iterable<T>.sumOf { … }`), read from the selected overload family.
    pub fn lambda_return_overload_param_types(&self, receiver: Ty, name: &str) -> Option<Vec<Ty>> {
        FunctionSet {
            overloads: self
                .resolve_symbol(SymRecv::TopLevel, name, &[])
                .map(Symbol::overloads)
                .unwrap_or_default(),
        }
        .overloads
        .iter()
        .filter(|o| {
            o.is_extension()
                && o.receiver
                    .is_none_or(|dr| source_receiver_rank(self.lib, receiver, dr).is_some())
        })
        .find_map(|o| {
            let gsig = o.generic_sig.as_ref()?;
            let mut binds = std::collections::HashMap::new();
            if let Some(recv_sig) = gsig.receiver {
                unify_ty(recv_sig, receiver, &mut binds);
            }
            gsig.params
                .first()
                .map(|selector| function_input_types(*selector, &binds))
                .filter(|params| !params.is_empty())
        })
    }

    /// Lambda call-shape facts for a receiver-less top-level call. This is arg-dependent because a
    /// generic HOF can bind lambda parameter types from already-typed non-lambda arguments
    /// (`applyIt(5) { it + 1 }`). Providers expose parsed generic signatures and metadata call-shape on
    /// `FunctionInfo`; this resolver aligns those facts for the concrete partial call in one pass.
    pub fn top_level_lambda_shape(
        &self,
        name: &str,
        arg_tys: &[Option<Ty>],
    ) -> Option<TopLevelLambdaShape> {
        // Lambda-SHAPE info for a name the caller already validated resolves (via import scope OR an
        // explicit FQ package). UNSCOPED over the federated source so a fully-qualified, unimported callee
        // (`kotlinx.coroutines.runBlocking { … }`) still yields its lambda facts.
        let fs = FunctionSet {
            overloads: self
                .resolve_symbol(SymRecv::TopLevel, name, &[])
                .map(Symbol::overloads)
                .unwrap_or_default(),
        };
        // The default-omitted trailing-lambda alignment (`runBlocking { … }`) applies ONLY when NO overload
        // of this name matches the provided argument count exactly. A name WITH an exact-arity overload
        // (`run { … }`) always uses that overload's own parameter positions — never an alignment against a
        // wider overload — so a legitimately-empty lambda-parameter result is not shadowed by one.
        let has_exact = fs.has_top_level_arity(arg_tys.len());
        let mut shape = TopLevelLambdaShape::default();
        for o in fs.top_level() {
            if shape.param_types.is_none() {
                if let Some(gsig) = o.generic_sig.as_ref() {
                    if !(has_exact && gsig.params.len() != arg_tys.len()) {
                        if let Some(map) = trailing_default_arg_indices(gsig.params.len(), arg_tys)
                        {
                            let mut binds = std::collections::HashMap::new();
                            for (ai, at) in arg_tys.iter().enumerate() {
                                if let (Some(t), Some(ps)) = (at, gsig.params.get(map[ai])) {
                                    unify_ty(*ps, *t, &mut binds);
                                }
                            }
                            let out: Vec<Vec<Ty>> = map
                                .iter()
                                .map(|&pi| {
                                    gsig.params
                                        .get(pi)
                                        .map(|ps| function_input_types(*ps, &binds))
                                        .unwrap_or_default()
                                })
                                .collect();
                            if out
                                .iter()
                                .zip(arg_tys)
                                .any(|(v, at)| at.is_none() && !v.is_empty())
                            {
                                shape.param_types = Some(out);
                            }
                        }
                    }
                }
            }

            if shape.receivers.is_none() {
                let recvs = &o.call_sig.lambda_receivers;
                if !(has_exact && recvs.len() != arg_tys.len()) {
                    if let Some(map) = trailing_default_arg_indices(recvs.len(), arg_tys) {
                        let out: Vec<Option<Ty>> = map
                            .iter()
                            .map(|&pi| recvs.get(pi).cloned().flatten())
                            .collect();
                        if out.iter().any(Option::is_some) {
                            shape.receivers = Some(out);
                        }
                    }
                }
            }

            if shape.materialized.is_none() {
                let m = &o.call_sig.lambda_materialized;
                if m.len() == arg_tys.len() && m.iter().any(|b| *b) {
                    shape.materialized = Some(m.clone());
                }
            }

            if shape.param_types.is_some()
                && shape.receivers.is_some()
                && shape.materialized.is_some()
            {
                break;
            }
        }
        (shape.param_types.is_some() || shape.receivers.is_some() || shape.materialized.is_some())
            .then_some(shape)
    }

    /// Lambda parameter types for an extension call before lambda bodies are typed. This binds the
    /// selected extension's generic signature from the receiver plus already-typed non-lambda args
    /// (`fold(0) { acc, x -> ... }` binds the accumulator from `0`). Public candidates are preferred;
    /// non-public `@InlineOnly` candidates are considered only as a fallback for scope functions.
    pub fn extension_lambda_param_types(
        &self,
        receiver: Ty,
        name: &str,
        arg_tys: &[Option<Ty>],
    ) -> Option<Vec<Vec<Ty>>> {
        let fs = FunctionSet {
            overloads: self
                .resolve_symbol(SymRecv::Value(receiver), name, &[])
                .map(Symbol::overloads)
                .unwrap_or_default()
                .into_iter()
                .filter(FunctionInfo::is_extension)
                .collect(),
        };
        for allow_must_inline in [false, true] {
            for o in ranked_extension_overloads_by_recv(&self.src, receiver, &fs, allow_must_inline)
            {
                let Some(gsig) = o.generic_sig.as_ref() else {
                    continue;
                };
                let Some(param_indices) = trailing_default_arg_indices(gsig.params.len(), arg_tys)
                else {
                    continue;
                };
                let mapped: Vec<Ty> = param_indices.iter().map(|&i| gsig.params[i]).collect();
                let mut binds = std::collections::HashMap::new();
                if let Some(recv_sig) = gsig.receiver {
                    unify_ty(recv_sig, receiver, &mut binds);
                }
                for (ps, at) in mapped.iter().zip(arg_tys) {
                    if let Some(t) = at {
                        unify_ty(*ps, *t, &mut binds);
                    }
                }
                let out: Vec<Vec<Ty>> = mapped
                    .iter()
                    .map(|ps| function_input_types(*ps, &binds))
                    .collect();
                if out.iter().any(|v| !v.is_empty()) {
                    return Some(out);
                }
            }
        }
        None
    }

    pub fn extension_lambda_receivers(
        &self,
        receiver: Ty,
        name: &str,
        arg_tys: &[Option<Ty>],
    ) -> Option<Vec<Option<Ty>>> {
        let fs = FunctionSet {
            overloads: self
                .resolve_symbol(SymRecv::Value(receiver), name, &[])
                .map(Symbol::overloads)
                .unwrap_or_default()
                .into_iter()
                .filter(FunctionInfo::is_extension)
                .collect(),
        };
        for allow_must_inline in [false, true] {
            for o in ranked_extension_overloads_by_recv(&self.src, receiver, &fs, allow_must_inline)
            {
                let Some(gsig) = o.generic_sig.as_ref() else {
                    continue;
                };
                if gsig.params.is_empty() {
                    continue;
                }
                let Some(param_indices) =
                    trailing_default_arg_indices(gsig.params.len() - 1, arg_tys)
                else {
                    continue;
                };
                let mapped: Vec<(usize, Ty)> = param_indices
                    .iter()
                    .map(|&i| (i, gsig.params[i + 1]))
                    .collect();
                let mut binds = std::collections::HashMap::new();
                unify_ty(gsig.params[0], receiver, &mut binds);
                for ((_, ps), at) in mapped.iter().zip(arg_tys) {
                    if let Some(t) = at {
                        unify_ty(*ps, *t, &mut binds);
                    }
                }
                let out: Vec<Option<Ty>> = mapped
                    .iter()
                    .map(|(logical_idx, ps)| {
                        if let Some(recv) = o
                            .call_sig
                            .lambda_receivers
                            .get(*logical_idx)
                            .copied()
                            .flatten()
                        {
                            return Some(recv);
                        }
                        if o.call_sig
                            .lambda_receiver_params
                            .get(*logical_idx)
                            .copied()
                            .unwrap_or(false)
                        {
                            return function_input_types(*ps, &binds).first().copied();
                        }
                        None
                    })
                    .collect();
                if out.iter().any(Option::is_some) {
                    return Some(out);
                }
            }
        }
        None
    }
}

// --- Navigation helpers (member/constructor resolution expressed purely against the trait) --------
// The inherited-member walk over a library type's hierarchy — arg-dependent binding, so it lives in
// this layer (not the oracle). `resolve` and `ir_lower` share one implementation, backend-agnostic.

fn abi_form_args(lib: &dyn CompilerPlatform, args: &[Ty]) -> Option<Vec<Ty>> {
    let out: Vec<Ty> = args.iter().map(|a| lib.abi_value_form(*a)).collect();
    (out.as_slice() != args).then_some(out)
}

fn params_match_abi_form(lib: &dyn CompilerPlatform, params: &[Ty], args: &[Ty]) -> bool {
    params.len() == args.len()
        && params
            .iter()
            .zip(args)
            .all(|(p, a)| lib.abi_value_form(*p) == *a)
}

fn platform_subtype(lib: &dyn CompilerPlatform, sub: Ty, sup: Ty) -> bool {
    crate::assignable::is_subtype(
        &crate::assignable::TyCtx::new(),
        &PlatformOracle(lib),
        sub,
        sup,
    )
}

/// Whether `arg` fits `param` after both are reduced to target ABI identity. The shared subtype
/// relation handles identity plus reference widening through the symbol source.
fn abi_arg_subtype_of_param(lib: &dyn CompilerPlatform, arg: Ty, param: Ty) -> bool {
    platform_subtype(lib, lib.abi_value_form(arg), lib.abi_value_form(param))
}

fn value_erased_args(lib: &dyn CompilerPlatform, args: &[Ty]) -> Vec<Ty> {
    args.iter()
        .map(|&a| lib.value_underlying(a).unwrap_or(a))
        .collect()
}

/// Resolve a constructor on a library type by argument types (with the type's own widening).
fn resolve_constructor(
    lib: &dyn CompilerPlatform,
    internal: &str,
    args: &[Ty],
) -> Option<LibraryMember> {
    let Some(t) = lib.resolve_type(internal) else {
        crate::trace_compiler!(
            "value_classes",
            "resolve_constructor {internal} resolve_type=None args={args:?}"
        );
        return None;
    };
    crate::trace_compiler!(
        "value_classes",
        "resolve_constructor {internal} ctors={:?} args={args:?}",
        t.constructors.iter().map(|m| &m.params).collect::<Vec<_>>()
    );
    if let Some(m) = t.ctor(args) {
        return Some(m.clone());
    }
    // A constructor PARAMETER of value-class type erases to its underlying in the JVM `<init>` descriptor
    // (`class Rec(val id: Vid, val n: Int)` → `<init>(Ljava/lang/String;I)V` for `Vid(String)`), but the
    // call passes the value-class type itself (`Rec(Vid("x"), 1)` → arg `Vid`). Retry with each value-class
    // argument erased to its underlying, mirroring the ABI the descriptor-read `ctor` params already carry.
    let erased = value_erased_args(lib, args);
    if erased != args {
        if let Some(m) = t.ctor(&erased) {
            crate::trace_compiler!(
                "value_classes",
                "resolve_constructor {internal} matched via value-class-erased args {args:?} -> {erased:?}"
            );
            return Some(m.clone());
        }
    }
    // ABI-form matching bridges target collection identity and drops type arguments without
    // hardcoding collection relationships. Exact ABI identity runs before subtype widening so the
    // most-specific overload still wins.
    let abi_args = abi_form_args(lib, args);
    if let Some(abi_args) = &abi_args {
        if let Some(m) = t
            .constructors
            .iter()
            .find(|m| params_match_abi_form(lib, &m.params, abi_args))
        {
            crate::trace_compiler!(
                "value_classes",
                "resolve_constructor {internal} matched via abi-form args {args:?} -> {abi_args:?}"
            );
            return Some(m.clone());
        }
    }
    if let Some(m) = t.constructors.iter().find(|m| {
        m.params.len() == args.len()
            && m.params
                .iter()
                .zip(args)
                .all(|(p, a)| abi_arg_subtype_of_param(lib, *a, *p))
    }) {
        let mode = abi_args
            .as_ref()
            .map_or("nominal-subtype", |_| "abi-subtype");
        crate::trace_compiler!(
            "value_classes",
            "resolve_constructor {internal} matched via {mode} args {args:?}"
        );
        return Some(m.clone());
    }
    // A classpath `@JvmInline value class` exposes only a PRIVATE `<init>` (its public surface is the
    // static `box-impl`/`constructor-impl`), so `ctor` finds nothing. Construction is `X(u)` over the
    // single underlying value `u`; synthesize that constructor so the call type-checks. The
    // value-classes lowering pass realizes it as the unboxed underlying / `constructor-impl`.
    if let Some(underlying) = t.value_underlying {
        // `X(u)` over the single underlying value — reference (`RoleId(String)`) or scalar
        // (`Count(Int)`); both erase to the underlying through the value-classes pass. (`null` only fits a
        // reference underlying.)
        let fits = matches!(args, [arg]
            if *arg == underlying || (matches!(*arg, Ty::Null) && underlying.is_reference()));
        // A ZERO-arg construction `Id()` when the sole underlying param is DEFAULTED — kotlinc realizes
        // it through the `constructor-impl$default` synthetic (which fills the default itself). Accept it
        // ONLY when that synthetic exists on the classpath, AND the underlying is a REFERENCE: the lowering
        // passes `null` for the dummy underlying slot, which fits only a reference (a scalar would need a
        // typed zero). A mandatory-param value class stays unresolved (no synthetic → no phantom call).
        let all_default = args.is_empty()
            && underlying.is_reference()
            && lib
                .resolve_type(internal)
                .is_some_and(|t| t.value_ctor_has_default);
        crate::trace_compiler!(
            "value_classes",
            "resolve_constructor {internal} value-class underlying={underlying:?} args={args:?} fits={fits} all_default={all_default}"
        );
        if fits {
            // Descriptor is unused on this path (the checker only needs the type; the lowerer lowers the
            // construction itself), so it stays empty — no JVM detail leaks into the resolver.
            return Some(LibraryMember::new(
                "<init>".to_string(),
                vec![underlying],
                Ty::obj(internal),
                String::new(),
            ));
        }
        if all_default {
            return Some(LibraryMember::new(
                "<init>".to_string(),
                vec![],
                Ty::obj(internal),
                String::new(),
            ));
        }
    }
    None
}

/// A construction routed through kotlinc's SYNTHETIC `<init>` overload carrying a trailing
/// `DefaultConstructorMarker` — two shapes krusty must fill at the call site:
///   * a VALUE-CLASS-typed parameter forces `<init>(<erased-params…>, DefaultConstructorMarker)` (the
///     real `<init>` is private), and the caller passes every arg plus a `null` marker (`mask: None`);
///   * an omitted DEFAULT parameter uses `<init>(<params…>, int mask, DefaultConstructorMarker)`, and the
///     caller passes the provided args, a placeholder per omitted param, the `mask`, then the `null` marker.
pub struct SyntheticCtorCall {
    /// The synthetic `<init>` descriptor to invoke.
    pub descriptor: String,
    /// The REAL (source) parameter types in descriptor form — a value-class param appears here as its
    /// erased underlying. Provided args coerce to the leading `provided` of these; the rest are omitted.
    pub real_params: Vec<Ty>,
    /// Number of args the caller supplies (a prefix of `real_params`).
    pub provided: usize,
    /// The default bitmask (bit `i` set = param `i` omitted), present only in the default-arg shape.
    pub mask: Option<i32>,
}

/// The classpath default-value synthetic constructor `<init>(<params…>, int mask, DefaultConstructorMarker)`
/// for `internal`, as `(descriptor, real_params)` — the (erased) parameter types BEFORE the mask+marker.
/// Matched by `arity` (the source parameter count): the default synthetic has exactly `arity` real params
/// then an `int` mask then the marker (`arity + 2` total). Matching by arity — not by a public non-marker
/// sibling — is required because a class with a VALUE-CLASS parameter has a PRIVATE primary constructor
/// (absent from the public `constructors`) and ALSO a separate value-class marker overload
/// `<init>(<params…>, marker)` (no mask); only the `arity + 2` shape is the default synthetic.
pub fn synthetic_default_ctor(
    lib: &dyn CompilerPlatform,
    internal: &str,
    arity: usize,
) -> Option<(String, Vec<Ty>)> {
    let t = lib.resolve_type(internal)?;
    let m = t
        .constructors
        .iter()
        .find(|m| has_default_tail(&m.params, arity, is_default_ctor_marker))?;
    Some((m.descriptor.clone(), m.params[..arity].to_vec()))
}

/// The classpath default-value synthetic for a MEMBER — `name$default(Owner, <params…>, int mask,
/// Object marker): Ret` (a static, e.g. a data class's `copy$default`) — as `(descriptor, real_params,
/// ret)`, the parameter types being the source method's (WITHOUT the leading receiver and trailing
/// mask/marker). Lets a call omit a defaulted argument. `None` when the class has no such synthetic.
pub fn synthetic_default_member(
    lib: &dyn CompilerPlatform,
    owner: &str,
    name: &str,
    arity: usize,
) -> Option<(String, Vec<Ty>, Ty, bool)> {
    let t = lib.resolve_type(owner)?;
    let dname = format!("{name}$default");
    // Shape `(Owner receiver, <real params…>, int mask, Object marker)`: exactly `arity` real params, an
    // `int` mask, and a reference marker. Match by `arity` (not just name) so an overloaded `name$default`
    // of a different parameter count can't be picked.
    if let Some(m) = t
        .companion
        .iter()
        .find(|m| m.name == dname && has_default_tail(&m.params, arity + 1, Ty::is_reference))
    {
        return Some((
            m.descriptor.clone(),
            m.params[1..arity + 1].to_vec(),
            m.ret,
            false,
        ));
    }
    // A `suspend` method's `$default` carries the `Continuation` as a real trailing parameter of the
    // original method, so its shape is `(Owner, <real params…>, Continuation, int mask, Object marker)` —
    // one longer, with the `Continuation` BEFORE the mask/marker. The descriptor already spells the
    // continuation in place; the coroutine pass threads the value there (see `append_continuation`).
    let m = t.companion.iter().find(|m| {
        m.name == dname
            && m.params
                .get(arity + 1)
                .copied()
                .is_some_and(|p| matches!(p, Ty::Obj("kotlin/coroutines/Continuation", _)))
            && has_default_tail(&m.params, arity + 2, Ty::is_reference)
    })?;
    Some((
        m.descriptor.clone(),
        m.params[1..arity + 1].to_vec(),
        m.ret,
        true,
    ))
}

/// Resolve a classpath construction that a plain [`resolve_constructor`] can't match because it needs a
/// synthetic `DefaultConstructorMarker` overload (a value-class param, or omitted defaults). See
/// [`SyntheticCtorCall`]. `None` when no marker overload fits.
fn resolve_synthetic_constructor(
    lib: &dyn CompilerPlatform,
    internal: &str,
    args: &[Ty],
) -> Option<SyntheticCtorCall> {
    let t = lib.resolve_type(internal)?;
    let erased = value_erased_args(lib, args);
    for m in &t.constructors {
        if m.params
            .last()
            .copied()
            .is_none_or(|p| !is_default_ctor_marker(p))
        {
            continue;
        }
        let leading = &m.params[..m.params.len() - 1];
        // Tell the default-mask shape (`…, int mask, marker`) from the value-class-param shape (`…, marker`):
        // a mask int is present iff dropping it leaves the params of a SIBLING non-marker ctor (the public
        // primary). Otherwise the trailing int is a real parameter.
        let (real_params, has_mask): (&[Ty], bool) = if leading.last() == Some(&Ty::Int)
            && !leading.is_empty()
            && t.constructors.iter().any(|s| {
                s.params
                    .last()
                    .copied()
                    .is_none_or(|p| !is_default_ctor_marker(p))
                    && s.params == leading[..leading.len() - 1]
            }) {
            (&leading[..leading.len() - 1], true)
        } else {
            (leading, false)
        };
        if erased.len() > real_params.len() {
            continue;
        }
        // No mask ⇒ no defaults ⇒ every parameter must be supplied.
        if !has_mask && erased.len() != real_params.len() {
            continue;
        }
        // A reference argument may be a NOMINAL SUBTYPE of its parameter (`Outer(id: Vid, a: A, b: B)`
        // constructed with `A.X(…)`/`B.Y(…)`, sealed subclasses) — the same widening `resolve_constructor`
        // allows for a plain constructor, composed with the value-class-erased synthetic-marker ctor.
        if !erased
            .iter()
            .zip(real_params)
            .all(|(a, p)| *p == Ty::obj("kotlin/Any") || abi_arg_subtype_of_param(lib, *a, *p))
        {
            continue;
        }
        let mask = has_mask.then(|| (erased.len()..real_params.len()).map(|j| 1i32 << j).sum());
        crate::trace_compiler!(
            "value_classes",
            "resolve_synthetic_constructor {internal} desc={} real={real_params:?} provided={} mask={mask:?}",
            m.descriptor,
            erased.len()
        );
        return Some(SyntheticCtorCall {
            descriptor: m.descriptor.clone(),
            real_params: real_params.to_vec(),
            provided: erased.len(),
            mask,
        });
    }
    None
}

/// Resolve a companion member `Type.name(args)` (the receiver type must be public).
fn resolve_companion(
    lib: &dyn CompilerPlatform,
    internal: &str,
    name: &str,
    args: &[Ty],
) -> Option<LibraryMember> {
    let t = lib.resolve_type(internal)?;
    if !t.is_public {
        return None;
    }
    best_overload(t.companion.iter(), name, args).cloned()
}

/// Resolve an instance member `recv.name(args)` — the receiver's static type must be public, but the
/// member may be inherited from a (possibly non-public) supertype. Candidates come from the consolidated
/// `functions` query, whose Member overloads carry the breadth-first `receiver_rank`; the closest rung's
/// best overload wins (most-derived first), exactly the inherited-member walk this used to do by hand.
fn resolve_instance(
    lib: &dyn CompilerPlatform,
    internal: &str,
    name: &str,
    args: &[Ty],
) -> Option<LibraryMember> {
    select_instance_info(lib, Ty::obj(internal), name, args).map(|o| {
        let ret = o.ret.apply(o.callable.ret);
        o.member_with_return(ret)
    })
}

/// Resolve a library instance member for a BOUND callable reference (`"KOTLIN"::get`) — where there are
/// no call arguments to drive overload resolution. Returns the UNIQUE fixed-arity overload of `name` on
/// `internal`, or `None` when the member is absent, defaulted/vararg, or ambiguous.
fn resolve_instance_ref(lib: &dyn CompilerPlatform, recv: Ty, name: &str) -> Option<LibraryMember> {
    let mut fixed = lib
        .member_overloads(recv, name)
        .overloads
        .into_iter()
        .filter(|o| o.call_sig.requires_all_args(o.callable.params.len()));
    let o = fixed.next()?;
    // Duplicate facts for the same signature are not ambiguous; distinct signatures are.
    if fixed.any(|other| {
        other.callable.params != o.callable.params || other.callable.ret != o.callable.ret
    }) {
        return None;
    }
    // A member inherited from `java/lang/Object` (`toString`/`equals`/`hashCode`) is the one set kotlinc
    // null-guards for a nullable/type-parameter receiver (`null::toString` yields "null"); a direct
    // `invokevirtual` on a captured null would NPE. The erased receiver (an unbounded `T`) reads as a
    // non-null `Any` here, so the receiver-type guard cannot catch it — reject on the resolved OWNER.
    if o.callable.owner.as_str() == crate::types::wk::java_object() {
        return None;
    }
    let ret = o.ret.apply(o.callable.ret);
    Some(o.member_with_return(ret))
}

/// A bound PROPERTY reference on a library receiver (`"kotlin"::length`), fully resolved to what the
/// backend emits: the getter's owner + physical name + the property type. Every classification and
/// emittability decision is made HERE — the checker and lowerer just consume this, never re-deriving
/// value-class-ness, interface-ness, or property-vs-function from the platform themselves.
pub struct BoundPropertyRef {
    pub owner: String,
    pub getter_name: String,
    pub prop_ty: Ty,
}

/// Resolve a bound property reference on `recv` (`"kotlin"::length`) to its emittable getter descriptor,
/// or `None` when it is not a plainly-emittable read of a property:
/// - `name` must be a PROPERTY, not a zero-arg method (`iterator()::next`) — both otherwise resolve to a
///   readable zero-arg member, so this consults the authoritative property classifier.
/// - a NULLABLE / type-parameter / bare-`Any` receiver may be null and would NPE at `get()`;
/// - the getter must dispatch with a plain `invokevirtual`: a concrete non-interface, non-value-class
///   owner and an unmangled getter (a value-class-typed property's `getX-<hash>` lives on an erased owner).
fn resolve_property_ref(
    lib: &dyn CompilerPlatform,
    recv: Ty,
    name: &str,
) -> Option<BoundPropertyRef> {
    if matches!(recv, Ty::TyParam(..) | Ty::Nullable(..))
        || recv.kotlin_class_internal() == Some(crate::types::wk::any())
    {
        return None;
    }
    if !lib.member_is_property(recv, name) {
        return None;
    }
    let m = resolve_property_member(lib, recv, name)?;
    if m.suspend {
        return None;
    }
    let owner = m.member.owner?;
    if m.member.name.contains('-')
        || lib.is_value(&owner)
        || lib.resolve_type(&owner).is_some_and(|t| t.is_interface())
    {
        return None;
    }
    Some(BoundPropertyRef {
        owner,
        getter_name: m.member.name,
        prop_ty: m.ret,
    })
}

#[derive(Clone, Debug)]
pub struct ResolvedMember {
    pub member: LibraryMember,
    pub ret: Ty,
    /// The resolved member is a `suspend fun` — the caller (a suspend body) must thread a
    /// `Continuation` into the emitted call and treat the (Object-erased) result as `ret`.
    pub suspend: bool,
}

/// Resolve an instance member and carry the logical return selected for this call. Generic member
/// returns may bind from the receiver (`List<Int>.get(Int): Int`) or, for erased-`Any` returns, from
/// the call arguments (`decodeFromString(serializer, text): T`).
fn resolve_instance_member(
    lib: &dyn CompilerPlatform,
    recv: Ty,
    name: &str,
    args: &[Ty],
) -> Option<ResolvedMember> {
    let o = select_instance_info(lib, recv, name, args)?;
    let ret = if o.callable.physical_ret == Ty::obj("kotlin/Any") {
        o.generic_sig
            .as_ref()
            .map(|gsig| {
                let mut binds = std::collections::HashMap::new();
                for (ps, a) in gsig.params.iter().zip(args) {
                    unify_ty(*ps, *a, &mut binds);
                }
                let arg_bound = ty_subst(gsig.ret, &binds);
                if arg_bound == Ty::obj("kotlin/Any") && o.callable.ret != Ty::obj("kotlin/Any") {
                    o.callable.ret
                } else {
                    arg_bound
                }
            })
            .unwrap_or(o.callable.ret)
    } else {
        o.callable.ret
    };
    let ret = o.ret.apply(ret);
    let member = o.member_with_return(o.callable.ret);
    Some(ResolvedMember {
        ret,
        member,
        suspend: o.flags.suspend,
    })
}

/// The property's getter resolved by its REAL name from the source's `properties` query — replacing the
/// `getX`/`is`-Boolean/`@JvmName` getter-name GUESSING with the authoritative metadata spelling. The
/// member itself is still built through `resolve_instance_member`, so the full member metadata (return
/// nullability, generic signature) is recovered exactly as before. `None` when no source exposes it as a
/// property, or the resolved getter isn't a read-value member.
fn property_getter_via_query(
    lib: &dyn CompilerPlatform,
    recv: Ty,
    property: &str,
) -> Option<ResolvedMember> {
    // A value-class-typed property's getter is `@JvmName`-mangled (`getId-<hash>`) and erases its return
    // to the underlying type; resolving it as a plain member would type the read as the underlying, not
    // the value class. Leave those to the value-class fallback, which recovers the logical type.
    let getter = lib
        .property_members(recv, property)
        .overloads
        .into_iter()
        .min_by_key(|p| p.receiver_rank)
        .map(|p| p.getter.name)
        .filter(|getter| !getter.contains('-'))?;
    resolve_instance_member(lib, recv, &getter, &[]).filter(|m| m.ret.is_read_value_result())
}

/// Resolve a zero-arg property read on `recv`. The `@Metadata` `properties` query supplies the real
/// getter name first (no guessing); then the fallbacks — the semantic Kotlin name (a
/// computed/builtin member), a `getX` physical getter, and a value-class-mangled getter.
fn resolve_property_member(
    lib: &dyn CompilerPlatform,
    recv: Ty,
    property: &str,
) -> Option<ResolvedMember> {
    property_getter_via_query(lib, recv, property)
        .or_else(|| resolve_instance_member(lib, recv, property, &[]))
        .filter(|m| m.ret.is_read_value_result())
        .or_else(|| {
            let getter = lib.physical_property_getter_name(property)?;
            resolve_instance_member(lib, recv, &getter, &[])
                .filter(|m| m.ret.is_read_value_result())
        })
        .or_else(|| {
            // A property whose declared type is a `@JvmInline value class`: its getter is
            // `@JvmName`-mangled (`getId-<hash>`) and the physical return erases to the underlying, so
            // the plain lookups above miss it. Recover the mangled getter + logical value-class type.
            let internal = recv.kotlin_class_internal()?;
            let member = lib
                .resolve_type(internal)?
                .value_class_property(property)
                .cloned()?;
            let ret = member.ret;
            Some(ResolvedMember {
                member,
                ret,
                suspend: false,
            })
        })
}

/// Resolve a `var` property's SETTER by its real `@Metadata` name — the write analogue of
/// [`property_getter_via_query`]. Returns the setter `LibraryCallable` (its `owner`/`descriptor` drive
/// the emitted `setX(v)` call, `params[0]` is the value type the write is checked against). `None` when
/// the property is read-only (`val`, no setter), no source exposes it as a member property, or the
/// setter is value-class `@JvmName`-mangled (`setId-<hash>` — left to the value-class path, which knows
/// the logical type).
fn resolve_property_setter(
    lib: &dyn CompilerPlatform,
    recv: Ty,
    property: &str,
) -> Option<LibraryCallable> {
    let setter = lib
        .property_members(recv, property)
        .overloads
        .into_iter()
        .min_by_key(|p| p.receiver_rank)
        .and_then(|p| p.setter)?;
    if setter.name.contains('-') {
        return None;
    }
    // A real setter takes exactly one parameter (the value). Anything else is malformed metadata —
    // treat it as absent so the checker and lowerer agree (both consult `params[0]`) rather than the
    // checker accepting permissively while the lowerer falls back to the inferred value type.
    if setter.params.len() != 1 {
        return None;
    }
    Some(setter)
}

fn select_instance_info(
    lib: &dyn CompilerPlatform,
    recv: Ty,
    name: &str,
    args: &[Ty],
) -> Option<FunctionInfo> {
    select_overload(
        lib,
        recv,
        name,
        args,
        &[],
        FnKind::Member,
        ExtCtx {
            allow_must_inline: false,
            fn_scope: None,
        },
    )
}

/// The shared unqualified-name resolution LOOP (spec § Resolution): form a candidate FQN `pkg/name` for
/// each in-scope `packages` entry and query [`crate::symbol_source::SymbolSource::resolve_symbols`] once
/// per candidate, returning each `(fqn, record)` whose namespace record is non-empty. The helper does
/// ONLY the loop — it does not decide anything. Because the record keeps the two namespaces SEPARATE
/// (`classifier` vs `callables`), each caller applies its own selection rules organically: a type
/// position reads `classifier` under level-precedence + within-level ambiguity; a call position flattens
/// `callables` and runs overload resolution. The `fqn` is returned so a classifier caller can name the
/// resolved internal (a non-alias classifier's internal name IS its fqn).
/// The rung of `decl_recv` in `recv`'s SOURCE-type supertype closure (0 = same class), or `None` if the
/// extension's declared receiver is neither `recv` nor a supertype of it. Uses `erased_recv` Kotlin-level
/// keys + `resolve_type` supertypes — NO JVM descriptors — so `kotlin/UInt` ≠ `kotlin/Int` ≠ `kotlin/Result`
/// are distinct by their class, a generic value-class receiver (`Result<T>`) binds a concrete one
/// (`Result<String>` — `erased_recv` drops type arguments), and `UInt` never binds an `Int` extension.
/// Replaces the descriptor-based `extension_receiver_rank`, whose value-class special-case existed only
/// because the erased `I`/`Object` descriptors tied distinct value classes together.
/// Whether the declared receiver's type arguments are consistent with the actual receiver's, position by
/// position, under Kotlin's COVARIANT reading of a receiver position: each actual argument must be
/// assignable to the declared one (`source_receiver_rank` reaching from actual to declared). A declared
/// argument that is a type variable or `Any`/`Object` is a wildcard (an `Iterable<T>` / erased
/// `Iterable<Any>` extension binds any element). This rejects the `@JvmName` reduction variant whose
/// element does not match (`Iterable<Byte>.averageOfByte` against a `List<Double>` — `Double` is not
/// assignable to `Byte`) while accepting a nested-generic supertype (`Iterable<Iterable<T>>.flatten`
/// against `List<List<Int>>` — `List<Int>` IS assignable to `Iterable<Any>`). The erased supertype walk
/// in `source_receiver_rank` alone keys on the outer class only, so it would tie the reduction variants.
fn receiver_type_args_match(src: &dyn SymbolSource, decl_recv: Ty, recv: Ty) -> bool {
    // Each actual argument must be assignable to the declared one under Kotlin's covariant receiver
    // reading. A declared argument that is a type variable or erased `Any` is a WILDCARD — the metadata
    // decode drops the nullability flag, so a `T?` receiver element reads as bare `Any`, and a nullable
    // actual (`Int?`) must still match it (`is_assignable(Int?, Any)` is correctly `false` under strict
    // Kotlin, but here `Any` stands for the erased variable, not the type `Any`).
    let cx = crate::assignable::TyCtx::new();
    let oracle = SourceOracle(src);
    let wildcard = |t: Ty| {
        t.is_ty_param()
            || matches!(t.non_null(), Ty::Obj(n, _)
                if crate::types::same(n, crate::types::wk::any())
                    || crate::types::same(n, crate::types::wk::java_object()))
    };
    decl_recv
        .type_args()
        .iter()
        .zip(recv.type_args().iter())
        .all(|(&d, &r)| {
            wildcard(d) || wildcard(r) || crate::assignable::is_assignable(&cx, &oracle, r, d)
        })
}

fn source_receiver_rank(src: &dyn SymbolSource, recv: Ty, decl_recv: Ty) -> Option<u32> {
    // Same source type — rung 0. Plain `Ty` equality (interned, NO erasure): the exact receiver an
    // extension is declared on. This is the ONLY rank an ARRAY receiver (`IntArray.sum()`) can carry
    // besides the universal `Any` — an array has no class-name key for the supertype walk below, and its
    // element type must be matched exactly (an `IntArray` extension must not bind an `Array<String>`).
    if recv.non_null() == decl_recv.non_null() {
        return Some(0);
    }
    // A concrete type argument on the declared receiver must match the actual receiver's — the
    // `@JvmName` reduction families (`average`/`sum`) declare one overload per element (`averageOfByte`
    // on `Iterable<Byte>`, `…OfDouble` on `Iterable<Double>`), all erasing to the same class, so the
    // supertype walk below would tie them and pick the first. `Iterable<Double>` binds a `List<Double>`
    // receiver; `Iterable<Byte>` does not. A type-variable argument (`Iterable<T>.map`, projected to
    // `Any`) matches anything, so generic extensions are untouched.
    if !receiver_type_args_match(src, decl_recv, recv) {
        return None;
    }
    let want = decl_recv.erased_recv().kotlin_class_internal();
    if let (Some(want), Some(start)) = (want, recv.erased_recv().kotlin_class_internal()) {
        let mut frontier = vec![start.to_string()];
        let mut seen: std::collections::HashSet<String> =
            std::iter::once(start.to_string()).collect();
        let mut rung = 0u32;
        while !frontier.is_empty() {
            if frontier.iter().any(|t| t == want) {
                return Some(rung);
            }
            let mut next = Vec::new();
            for t in &frontier {
                if let Some(lt) = src.resolve_type(t) {
                    for s in lt.supertypes {
                        if seen.insert(s.clone()) {
                            next.push(s);
                        }
                    }
                }
            }
            frontier = next;
            rung += 1;
        }
    }
    // A universal `Any`-receiver extension (`<T> T.let`) applies to every receiver — arrays included — at
    // lowest precedence.
    (want == Some("kotlin/Any")).then_some(u32::MAX - 1)
}

pub(crate) fn resolve_symbols_in_scope(
    src: &dyn SymbolSource,
    name: &str,
    packages: &[String],
) -> Vec<(String, crate::libraries::ResolvedSymbols)> {
    let lib = src;
    packages
        .iter()
        .filter_map(|pkg| {
            let fqn = if pkg.is_empty() {
                name.to_string()
            } else {
                format!("{pkg}/{name}")
            };
            let r = lib.resolve_symbols(&fqn);
            (!r.is_empty()).then_some((fqn, r))
        })
        .collect()
}

fn function_set_from_symbols(
    symbols: impl IntoIterator<Item = (String, crate::libraries::ResolvedSymbols)>,
) -> FunctionSet {
    FunctionSet {
        overloads: symbols
            .into_iter()
            .flat_map(|(_, r)| match r.callables {
                crate::libraries::Callables::Functions(f) => f.overloads,
                _ => Vec::new(),
            })
            .collect(),
    }
}

/// Whether callable overload `o` is visible for an UNQUALIFIED (top-level or extension) call given the
/// in-scope packages `fn_scope`. A same-module callable ([`Origin::Module`]) is always visible — module
/// visibility is resolved separately, and its facade owner may be package-less. Only a CLASSPATH
/// ([`Origin::Library`]) callable must have its facade's package imported (same-package / star / explicit
/// / default), matching kotlinc. `None` scope keeps everything (a context with no import scope).
fn fn_in_scope(o: &FunctionInfo, fn_scope: Option<&[String]>) -> bool {
    if !matches!(o.callable.origin, Origin::Library) {
        return true;
    }
    match fn_scope {
        None => true,
        Some(scope) => {
            let pkg = o.callable.owner.rsplit_once('/').map_or("", |(p, _)| p);
            scope.iter().any(|p| p == pkg)
        }
    }
}

/// Extension-selection context for [`select_overload`]: whether non-public `@InlineOnly` candidates are
/// admitted (the bytecode inliner), and the packages in scope for an extension (`None` = unscoped). Both
/// only affect EXTENSION selection — a member is always visible on its type.
#[derive(Clone, Copy)]
struct ExtCtx<'a> {
    allow_must_inline: bool,
    fn_scope: Option<&'a [String]>,
}

/// The single call-overload selector for a receiver call `recv.name(args)`. It is parameterized by
/// [`FnKind`] — MEMBER and EXTENSION resolution differ only in the *calling convention* the backend emits
/// (invokevirtual with `this` vs invokestatic with the receiver as the leading arg), NOT in how the best
/// overload is chosen. The receiver is always an ATTRIBUTE, never `params[0]`: candidates are matched
/// against their LOGICAL value parameters (a member's `callable.params` are value-only; an extension's
/// prepend the receiver in the JVM emit shape, so [`logical_value_params`] strips it). Overloads are tried
/// closest-receiver-rank first, and within a rank by the ordered applicability passes below.
fn select_overload(
    lib: &dyn CompilerPlatform,
    recv: Ty,
    name: &str,
    args: &[Ty],
    type_args: &[Ty],
    kind: FnKind,
    ext: ExtCtx,
) -> Option<FunctionInfo> {
    // A MEMBER call needs a public class receiver; an EXTENSION resolves on any receiver (primitives,
    // type variables, nullable types) that may have no `resolve_type` entry, so gate only members.
    if kind == FnKind::Member {
        let internal = recv.kotlin_class_internal()?;
        if !lib.resolve_type(internal)?.is_public {
            return None;
        }
    }
    let allow_must_inline = ext.allow_must_inline;
    // EXTENSION candidates come from the ONE query — union `resolve_symbols`' function callables over the
    // in-scope packages (scope-pruned, tree-driven), so an unqualified extension binds only when its
    // facade's package is imported. No import scope → the whole-classpath `functions()` fallback
    // (removed once every consumer is scoped — task A). MEMBERS are always visible on their type.
    let fs = match kind {
        // A MEMBER's return can be RECEIVER-COUPLED (`Repo<Cfg>.byId(): Cfg`, a suspend `Continuation<T>`
        // bound from the receiver's type argument) — recovery the receiver-agnostic `resolve_type` cannot
        // do — so member candidates come from the platform's receiver-aware member query. EXTENSIONS come
        // from the scope-pruned `resolve_symbols` seam (empty when there is no import scope).
        FnKind::Member => lib.member_overloads(recv, name),
        FnKind::Extension => ext
            .fn_scope
            .map(|scope| function_set_from_symbols(resolve_symbols_in_scope(lib, name, scope)))
            .unwrap_or_default(),
        FnKind::TopLevel => FunctionSet::default(),
    };
    // Candidates from the scoped query are IN-SCOPE by construction: each came from a `resolve_symbols`
    // over an imported package, so its declared package is in scope even when `@JvmPackageName` relocated
    // its facade to a different JVM package (`kotlin.collections`'s `UArraysKt` → `kotlin/collections/
    // unsigned/`). Re-deriving scope from the JVM owner (`fn_in_scope`) would wrongly drop those, so trust
    // the query.
    let pre_scoped = kind == FnKind::Extension && ext.fn_scope.is_some();
    crate::trace_compiler!(
        "resolve",
        "select_overload name={name} recv={recv:?} kind={kind:?} scope={:?} cands={}",
        ext.fn_scope.map(<[String]>::len),
        fs.overloads.len(),
    );
    for o in &fs.overloads {
        crate::trace_compiler!(
            "resolve",
            "  raw {name} kind={:?} recv={:?} pub={} rank={} origin={:?} owner={}",
            o.kind,
            o.receiver,
            o.public(),
            o.receiver_rank,
            o.callable.origin,
            o.callable.owner,
        );
    }
    // Candidates as `(overload, logical value params)`, grouped by receiver rank. An extension admits only
    // public overloads unless the caller is the bytecode inliner (which splices non-public `@InlineOnly`).
    let mut by_rank: std::collections::BTreeMap<u32, Vec<(&FunctionInfo, Vec<Ty>)>> =
        std::collections::BTreeMap::new();
    for o in fs.overloads.iter().filter(|o| {
        o.kind == kind
            && (kind != FnKind::Extension
                || (o.receiver_rank != u32::MAX
                    && (o.public() || (allow_must_inline && o.flags.inline.must_inline()))
                    && (pre_scoped || fn_in_scope(o, ext.fn_scope))))
    }) {
        // A receiver-agnostic `resolve_symbols` extension carries rank `0`; recover the real receiver-MRO
        // rung from the actual receiver so most-specific selection (a `List` extension over an `Iterable`
        // one) still holds. A candidate whose declared receiver is NOT in the receiver's supertype closure
        // does not apply — drop it. Members and lambda-return (`u32::MAX`) keep their provider rank.
        let rank = if kind == FnKind::Extension {
            match o
                .receiver
                .and_then(|dr| source_receiver_rank(lib, recv, dr))
            {
                Some(r) => r,
                None => {
                    crate::trace_compiler!(
                        "resolve",
                        "  drop {name} decl_recv={:?} (not in recv MRO)",
                        o.receiver
                    );
                    continue;
                }
            }
        } else {
            o.receiver_rank
        };
        let lp = logical_value_params(lib, o, recv, type_args);
        crate::trace_compiler!(
            "resolve",
            "  cand {name} rank={rank} logical_params={lp:?} owner={}",
            o.callable.owner
        );
        by_rank.entry(rank).or_default().push((o, lp));
    }
    for cands in by_rank.values() {
        if let Some(o) = best_by_args(lib, cands, args) {
            return Some(o.clone());
        }
    }
    // Platform assignability pass: subtype closure, erased `Any`, and value-class underlying matching.
    // The ordered applicability pass above stays stricter so exact/defaulted calls still win first.
    for cands in by_rank.values() {
        if let Some((o, _)) = cands.iter().find(|(_, lp)| {
            lp.len() == args.len()
                && lp
                    .iter()
                    .zip(args)
                    .all(|(p, a)| platform_arg_assignable(lib, p, a))
        }) {
            return Some((*o).clone());
        }
    }
    // ABI-form pass, shared with constructor resolution: bridge target collection identity and
    // erase type arguments after exact, widened, and source-level subtype matching have failed.
    if let Some(abi_args) = abi_form_args(lib, args) {
        for cands in by_rank.values() {
            if let Some((o, _)) = cands
                .iter()
                .find(|(_, lp)| params_match_abi_form(lib, lp, &abi_args))
            {
                crate::trace_compiler!(
                    "resolve",
                    "select_overload {} matched via abi-form args {args:?} -> {abi_args:?}",
                    o.callable.name
                );
                return Some((*o).clone());
            }
        }
    }
    None
}

/// LOGICAL value parameters of an overload — what a call site's arguments are matched against, with the
/// receiver excluded (it is an attribute). Member/top-level `callable.params` are already value-only; an
/// extension's `callable.params` prepend the receiver in the JVM emit shape, so bind the generic signature
/// to `recv` and drop the leading receiver, preferring each parameter's value-class LOGICAL type over its
/// erased underlying (`Id` over `kotlin/String`).
fn logical_value_params(
    lib: &dyn CompilerPlatform,
    o: &FunctionInfo,
    recv: Ty,
    type_args: &[Ty],
) -> Vec<Ty> {
    if !o.is_extension() {
        return o.callable.params.clone();
    }
    match o.generic_sig.as_ref() {
        Some(gsig) => {
            let mut binds = seeded_gsig_binds(gsig, type_args);
            if let Some(recv_sig) = gsig.receiver {
                unify_ty(recv_sig, recv, &mut binds);
            }
            let mut out = ty_subst_all(&gsig.params, &binds);
            for (i, p) in out.iter_mut().enumerate() {
                // `callable.params[0]` is the receiver in the emit shape, so value params start at `+1`.
                if let Some(cp) = o.callable.params.get(i + 1) {
                    if lib.value_underlying(*cp).is_some() {
                        *p = *cp;
                    }
                }
            }
            out
        }
        None => o.extension_value_params().to_vec(),
    }
}

fn platform_arg_assignable(lib: &dyn CompilerPlatform, param: &Ty, arg: &Ty) -> bool {
    (*arg == Ty::Null && param.is_reference())
        || crate::assignable::is_assignable(
            &crate::assignable::TyCtx::new(),
            &PlatformOracle(lib),
            *arg,
            *param,
        )
}

/// Pick the best overload whose logical value parameters accept `args`, in Kotlin applicability order:
/// exact, then `Any`-widened / function-arity, then a prefix under-application (omitted trailing params
/// must be optional), then a trailing-lambda call that omits leading DEFAULTED params (`m.withLock { … }`).
fn best_by_args<'a>(
    lib: &dyn CompilerPlatform,
    cands: &[(&'a FunctionInfo, Vec<Ty>)],
    args: &[Ty],
) -> Option<&'a FunctionInfo> {
    // The DEFAULT-omitting passes accept a reference SUBTYPE / value-class-underlying argument (a
    // `joinToString(separator: CharSequence = …)` call with a `String`), matching the assignability the
    // exact-arity subtype pass in `select_overload` applies — the exact/`Any`-widened passes above stay
    // stricter so an exact call still prefers its precise overload.
    let fits = |p: &Ty, a: &Ty| {
        fun_arg_matches(lib, p, a)
            || platform_arg_assignable(lib, p, a)
            // A function-shaped argument that IS-A `FunctionN` by supertype (a `KProperty1` fits a
            // `(T) -> R` param) — matched by arity, since it is neither a `Ty::Fun` nor equal to the param.
            || p.fun_arity()
                .zip(lib.function_like_arity(*a))
                .is_some_and(|(pn, an)| usize::from(pn) == an)
    };
    cands
        .iter()
        .find(|(_, lp)| *lp == args)
        .or_else(|| {
            cands.iter().find(|(_, lp)| {
                lp.len() == args.len()
                    && lp.iter().zip(args).all(|(p, a)| {
                        p == a || *p == Ty::obj("kotlin/Any") || fun_arg_matches(lib, p, a)
                    })
            })
        })
        .or_else(|| {
            cands.iter().find(|(o, lp)| {
                lp.len() >= args.len()
                    && (lp.len() == args.len()
                        || o.call_sig.required == 0
                        || o.call_sig.required <= args.len())
                    && lp[..args.len()].iter().zip(args).all(|(p, a)| fits(p, a))
            })
        })
        .or_else(|| {
            // Trailing-lambda call omitting leading defaulted params: the last arg (a lambda) fills the LAST
            // value param, the leading args a prefix, and every omitted MIDDLE param must be defaulted.
            if !matches!(args.last(), Some(Ty::Fun(_))) {
                return None;
            }
            cands.iter().find(|(o, lp)| {
                let Some(last) = lp.len().checked_sub(1) else {
                    return false;
                };
                let prefix = args.len() - 1;
                prefix <= last
                    && fun_arg_matches(lib, &lp[last], args.last().unwrap())
                    && ((prefix..last).all(|i| o.call_sig.param_has_default(i))
                        || o.call_sig.required <= prefix)
                    && lp[..prefix.min(lp.len())]
                        .iter()
                        .zip(&args[..prefix])
                        .all(|(p, a)| fits(p, a))
            })
        })
        .map(|(o, _)| *o)
}

/// A lambda argument (`Ty::Fun`) matches a function-typed parameter of the same arity. The parameter may
/// be a decoded `Ty::Fun` (whose return/parameter types differ from the lambda's — the body adapts) or an
/// erased `kotlin/jvm/functions/FunctionN` object; neither pairs with the argument under plain equality or
/// `Any` widening, so arity alone drives the match.
fn fun_arg_matches(lib: &dyn CompilerPlatform, param: &Ty, arg: &Ty) -> bool {
    let Some(arg_arity) = arg.fun_arity() else {
        return false;
    };
    let param = match param {
        Ty::Nullable(inner) => **inner,
        _ => *param,
    };
    let arity_ok = param.fun_arity().is_some_and(|pn| pn == arg_arity)
        || param
            .obj_internal()
            .and_then(|p| p.strip_prefix("kotlin/jvm/functions/Function"))
            .and_then(|d| d.parse::<u8>().ok())
            == Some(arg_arity);
    arity_ok && fun_return_compatible(lib, param, *arg)
}

/// A function-typed argument fits a function-typed parameter's RETURN. A parameter `(T) -> R` with a
/// CONCRETE `R` (`sumOfInt`'s `(T) -> Int`) accepts ONLY a lambda whose body returns that `R` — this is
/// how a `@OverloadResolutionByLambdaReturnType` group (whose overloads share value params and differ only
/// in the selector's return) is resolved: the lambda's return is just another parameter of the check. A
/// type-variable / erased-`Any` parameter return (an ordinary generic HOF `(T) -> R`), or an unresolved
/// lambda body, stays permissive so normal HOFs keep matching.
fn fun_return_compatible(lib: &dyn CompilerPlatform, param: Ty, arg: Ty) -> bool {
    let (Some(pr), Some(ar)) = (param.fun_ret(), arg.fun_ret()) else {
        return true;
    };
    if matches!(pr, Ty::TyParam(..) | Ty::Error)
        || pr.non_null().obj_internal() == Some("kotlin/Any")
    {
        return true;
    }
    if matches!(ar, Ty::Error) {
        return true;
    }
    if pr.non_null() == ar.non_null() {
        return true;
    }
    // A CONCRETE REFERENCE return is covariant: a lambda whose body returns a SUBTYPE (`String`) fits a
    // `(T) -> CharSequence` transform parameter (`joinToString`). Primitive returns stay INVARIANT — the
    // `@OverloadResolutionByLambdaReturnType` families (`sumOf { Int } / { Double }`) differ only by their
    // exact primitive return and must not cross-match.
    if let (Some(p), Some(a)) = (
        pr.non_null().kotlin_class_internal(),
        ar.non_null().kotlin_class_internal(),
    ) {
        if pr.is_reference() && ar.is_reference() {
            return platform_subtype(lib, Ty::obj(a), Ty::obj(p));
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::libraries::{CallSig, FunctionSet, LibraryCallable, Origin, TypeKind};
    use crate::symbol_source::SymbolSource;

    struct FakeSource {
        name: &'static str,
        receiver: Option<Ty>,
        info: FunctionInfo,
    }

    impl SymbolSource for FakeSource {
        fn member_overloads(&self, recv: Ty, name: &str) -> FunctionSet {
            if self.receiver == Some(recv) && name == self.name {
                FunctionSet {
                    overloads: vec![self.info.clone()],
                }
            } else {
                FunctionSet::default()
            }
        }

        fn resolve_symbols(&self, fqn: &str) -> crate::libraries::ResolvedSymbols {
            // The fake's name is package-less, so a scoped resolver queries it as the bare fqn.
            if fqn == self.name {
                crate::libraries::ResolvedSymbols {
                    classifier: None,
                    callables: crate::libraries::Callables::Functions(FunctionSet {
                        overloads: vec![self.info.clone()],
                    }),
                }
            } else {
                crate::libraries::ResolvedSymbols::default()
            }
        }

        fn resolve_type(&self, internal: &str) -> Option<crate::libraries::LibraryType> {
            matches!(internal, "kotlin/UInt" | "demo/Box").then(|| crate::libraries::LibraryType {
                is_public: true,
                kind: TypeKind::Class,
                supertypes: vec!["kotlin/Any".to_string()],
                constructors: vec![],
                members: vec![],
                companion: vec![],
                companion_consts: std::collections::HashMap::new(),
                sam_method: None,
                companion_object: None,
                value_companion_fns: Vec::new(),
                value_underlying: (internal == "kotlin/UInt").then_some(Ty::Int),
                alias_target: None,
                type_params: Vec::new(),
                sealed_subclasses: Vec::new(),
                enum_entries: Vec::new(),
                value_ctor_has_default: false,
                ctor_named_params: Vec::new(),
                value_class_properties: Vec::new(),
                retention: None,
            })
        }
    }

    impl crate::libraries::TargetRuntime for FakeSource {
        fn value_underlying(&self, ty: Ty) -> Option<Ty> {
            self.resolve_type(ty.obj_internal()?)
                .and_then(|t| t.value_underlying)
        }
    }

    fn top_level_default_uint_info() -> FunctionInfo {
        let callable = LibraryCallable {
            owner: "kotlin/UIntKt".to_string(),
            name: "make$default".to_string(),
            params: vec![Ty::Int],
            ret: Ty::Int,
            physical_ret: Ty::Int,
            descriptor: "(I)I".to_string(),
            suspend: false,
            inline: InlineKind::None,
            default_call: true,
            vararg_elem: None,
            signature: None,
            origin: Origin::Library,
            source_receiver: None,
        };
        FunctionInfo {
            ret: crate::libraries::ReturnInfo::new(false, Some(Ty::UInt)),
            call_sig: CallSig {
                required: 0,
                param_defaults: vec![true],
                ..Default::default()
            },
            ..FunctionInfo::plain(FnKind::TopLevel, None, callable)
        }
    }

    fn top_level_nullable_string_info() -> FunctionInfo {
        let callable = LibraryCallable::library(
            "kotlin/FooKt",
            "maybe",
            vec![],
            Ty::String,
            Ty::String,
            "()Ljava/lang/String;",
        );
        FunctionInfo {
            ret: crate::libraries::ReturnInfo::new(true, None),
            ..FunctionInfo::plain(FnKind::TopLevel, None, callable)
        }
    }

    fn extension_nullable_string_info() -> FunctionInfo {
        let receiver = Ty::String;
        let callable = LibraryCallable::library(
            "kotlin/text/StringsKt",
            "maybeSuffix",
            vec![receiver],
            Ty::String,
            Ty::String,
            "(Ljava/lang/String;)Ljava/lang/String;",
        );
        FunctionInfo {
            ret: crate::libraries::ReturnInfo::new(true, None),
            ..FunctionInfo::plain(FnKind::Extension, Some(receiver), callable)
        }
    }

    fn member_nullable_string_info() -> FunctionInfo {
        let receiver = Ty::obj("demo/Box");
        let callable = LibraryCallable::library(
            "demo/Box",
            "maybe",
            vec![],
            Ty::String,
            Ty::String,
            "()Ljava/lang/String;",
        );
        FunctionInfo {
            ret: crate::libraries::ReturnInfo::new(true, None),
            ..FunctionInfo::plain(FnKind::Member, Some(receiver), callable)
        }
    }

    fn member_metadata_class_info() -> FunctionInfo {
        let receiver = Ty::obj("demo/Box");
        let callable = LibraryCallable::library(
            "demo/Box",
            "names",
            vec![],
            Ty::obj("kotlin/Any"),
            Ty::obj("kotlin/Any"),
            "()Ljava/lang/Object;",
        );
        FunctionInfo {
            ret: crate::libraries::ReturnInfo::new(
                false,
                Some(Ty::obj_args("kotlin/collections/List", &[Ty::String])),
            ),
            ..FunctionInfo::plain(FnKind::Member, Some(receiver), callable)
        }
    }

    #[test]
    fn top_level_default_callable_preserves_metadata_return_type() {
        let source = FakeSource {
            name: "make$default",
            receiver: None,
            info: top_level_default_uint_info(),
        };
        let scope = vec![String::new()];
        let resolver = SymbolResolver::new_scoped(&source, &scope);
        let call = resolver
            .resolve_top_level_callable("make", &[], &[])
            .expect("default callable should resolve");
        assert!(call.default_call);
        assert_eq!(call.ret, Ty::UInt);
        assert_eq!(call.physical_ret, Ty::Int);
    }

    #[test]
    fn top_level_callable_preserves_nullable_metadata_return() {
        let source = FakeSource {
            name: "maybe",
            receiver: None,
            info: top_level_nullable_string_info(),
        };
        let scope = vec![String::new()];
        let resolver = SymbolResolver::new_scoped(&source, &scope);
        let call = resolver
            .resolve_top_level_callable("maybe", &[], &[])
            .expect("nullable callable should resolve");
        assert_eq!(call.ret, Ty::nullable(Ty::String));
        assert_eq!(call.physical_ret, Ty::String);
    }

    #[test]
    fn extension_callable_preserves_nullable_metadata_return() {
        let source = FakeSource {
            name: "maybeSuffix",
            receiver: Some(Ty::String),
            info: extension_nullable_string_info(),
        };
        let scope = vec![String::new()];
        let resolver = SymbolResolver::new_scoped(&source, &scope);
        let call = resolver
            .resolve_extension_callable("maybeSuffix", Ty::String, &[], &[])
            .expect("nullable extension callable should resolve");
        assert_eq!(call.ret, Ty::nullable(Ty::String));
        assert_eq!(call.physical_ret, Ty::String);
    }

    #[test]
    fn instance_member_preserves_nullable_metadata_return() {
        let source = FakeSource {
            name: "maybe",
            receiver: Some(Ty::obj("demo/Box")),
            info: member_nullable_string_info(),
        };
        let resolved = resolve_instance_member(&source, Ty::obj("demo/Box"), "maybe", &[])
            .expect("nullable member should resolve");
        assert_eq!(resolved.ret, Ty::nullable(Ty::String));
        assert_eq!(resolved.member.physical_ret, Ty::String);
    }

    #[test]
    fn instance_member_preserves_metadata_return_class() {
        let source = FakeSource {
            name: "names",
            receiver: Some(Ty::obj("demo/Box")),
            info: member_metadata_class_info(),
        };
        let resolved = resolve_instance_member(&source, Ty::obj("demo/Box"), "names", &[])
            .expect("member with metadata return class should resolve");
        assert_eq!(
            resolved.ret,
            Ty::obj_args("kotlin/collections/List", &[Ty::String])
        );
        assert_eq!(resolved.member.physical_ret, Ty::obj("kotlin/Any"));
    }
}
