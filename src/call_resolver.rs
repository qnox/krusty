//! Call resolution — the binding layer that sits *above* a [`SymbolSource`].
//!
//! A [`SymbolSource`] is a pure, arg-INDEPENDENT metadata oracle: given a name (and optional receiver)
//! it returns every overload with its raw signature and flags ([`crate::libraries::FunctionSet`]). It
//! does no overload selection and no type-variable binding.
//!
//! [`CallResolver`] is the arg-DEPENDENT layer on top: given the actual argument types at a call site
//! it selects the right overload and binds the generic receiver/parameter/return types. It is platform
//! agnostic — it only ever talks to the oracle through the [`SymbolSource`] trait, so the same binding
//! logic serves every backend (JVM today, JS later). The platform-specific bits (parsing a backend's
//! generic-signature string into [`GSig`]) live behind the trait; the binding *algorithm* over [`GSig`]
//! lives here.

use crate::libraries::{
    CompilerPlatform, FnKind, FunctionInfo, FunctionSet, GSig, GenericSig, InlineKind,
    LibraryCallable, LibraryMember, Origin, PropKind,
};
use crate::types::Ty;

type GSigBinds = std::collections::HashMap<String, Ty>;

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
            unify_gsig(p, *a, &mut binds);
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

/// Bind type variables by unifying a parameter signature node with an actual argument `Ty`.
pub(crate) fn unify_gsig(sig: &GSig, actual: Ty, binds: &mut GSigBinds) {
    match sig {
        GSig::Var(n) => {
            binds.entry((*n).to_string()).or_insert(actual);
        }
        GSig::Arr(inner) => {
            if let Some(elem) = actual.array_elem() {
                unify_gsig(inner, elem, binds);
            }
        }
        GSig::Function { params, ret } => {
            // A function parameter (`Function1<T, R>`) unifies against a lambda argument (`Ty::Fun`):
            // the parameter nodes bind the lambda's parameters and the return node binds its return, so
            // `map`'s `R` binds from the lambda body's type (`{ it * 2 }` → `Int`).
            if let Ty::Fun(fsig) = actual {
                // A SUSPEND SAM parameter (`suspend CoroutineScope.() -> T`) erases to
                // `Function2<CoroutineScope, Continuation<T>, Object>` — the RESULT type parameter `T`
                // lives inside the trailing `Continuation<T>`, and the JVM return node is `Object`. The
                // lambda argument, however, ERASES its own `Continuation` type argument (to `Any`) and
                // carries its real result in `fsig.ret`. Binding `T` from the erased `Continuation<Any>`
                // would fix it to `Any` (`runBlocking { … } : Any`, losing the block's type); bind it from
                // `fsig.ret` instead, and skip the `Continuation` param so it isn't double-unified.
                let value_params: &[GSig] = match params.last() {
                    Some(GSig::Class(n, cargs))
                        if crate::types::same(n, crate::types::wk::continuation())
                            && !cargs.is_empty() =>
                    {
                        unify_gsig(&cargs[0], fsig.ret, binds);
                        &params[..params.len() - 1]
                    }
                    _ => params,
                };
                for (a, p) in value_params.iter().zip(fsig.params.iter()) {
                    unify_gsig(a, *p, binds);
                }
                unify_gsig(ret, fsig.ret, binds);
            }
        }
        GSig::Class(_, args) => {
            // Unify the type arguments positionally against the actual's carried arguments, if any.
            if let Ty::Obj(_, targs) = actual {
                for (a, t) in args.iter().zip(targs.iter()) {
                    unify_gsig(a, *t, binds);
                }
            }
        }
        GSig::Prim(_) => {}
    }
}

/// Realize a signature node to a `Ty` under the current bindings — an unbound variable erases to
/// `Any`, a class becomes `Ty::obj_args` carrying its (substituted) type arguments.
pub(crate) fn gsig_to_ty(sig: &GSig, binds: &GSigBinds) -> Ty {
    match sig {
        GSig::Var(n) => binds
            .get(*n)
            .copied()
            .unwrap_or_else(|| Ty::obj("kotlin/Any")),
        GSig::Prim(t) => *t,
        GSig::Arr(inner) => Ty::array(gsig_to_ty(inner, binds)),
        GSig::Function { params, ret } => Ty::fun(gsig_tys(params, binds), gsig_to_ty(ret, binds)),
        GSig::Class(internal, args) => {
            if args.is_empty() {
                Ty::obj(internal)
            } else {
                Ty::obj_args(internal, &gsig_tys(args, binds))
            }
        }
    }
}

pub(crate) fn gsig_tys(sigs: &[GSig], binds: &GSigBinds) -> Vec<Ty> {
    sigs.iter().map(|s| gsig_to_ty(s, binds)).collect()
}

fn seeded_gsig_binds(gsig: &GenericSig, type_args: &[Ty]) -> GSigBinds {
    gsig.formals
        .iter()
        .cloned()
        .zip(type_args.iter().copied())
        .collect()
}

fn bind_gsig_return<'a>(
    gsig: &GenericSig,
    type_args: &[Ty],
    actuals: impl IntoIterator<Item = (&'a GSig, Ty)>,
) -> Ty {
    let mut binds = seeded_gsig_binds(gsig, type_args);
    for (ps, a) in actuals {
        unify_gsig(ps, a, &mut binds);
    }
    gsig_to_ty(&gsig.ret, &binds)
}

fn bind_ext_ret(gsig: &GenericSig, receiver: Ty, args: &[Ty], targs: &[Ty]) -> Ty {
    let mut binds = seeded_gsig_binds(gsig, targs);
    if let Some(recv_sig) = &gsig.receiver {
        unify_gsig(recv_sig, receiver, &mut binds);
    }
    for (ps, a) in gsig.params.iter().zip(args.iter().copied()) {
        unify_gsig(ps, a, &mut binds);
    }
    gsig_to_ty(&gsig.ret, &binds)
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
    if let Some(recv_sig) = &gsig.receiver {
        unify_gsig(recv_sig, receiver, &mut binds);
    }
    if trailing_lambda {
        let prefix = args.len().saturating_sub(1);
        for (ps, a) in gsig.params.iter().take(prefix).zip(args) {
            unify_gsig(ps, *a, &mut binds);
        }
        if let (Some(ls), Some(la)) = (gsig.params.last(), args.last()) {
            unify_gsig(ls, *la, &mut binds);
        }
    } else {
        for (ps, a) in gsig.params.iter().zip(args) {
            unify_gsig(ps, *a, &mut binds);
        }
    }
    gsig_to_ty(&gsig.ret, &binds)
}

/// If `sig` is a function type, the substituted types of its lambda parameters. Empty for anything else.
pub(crate) fn function_input_types(sig: &GSig, binds: &GSigBinds) -> Vec<Ty> {
    match sig {
        GSig::Function { params, .. } => gsig_tys(params, binds),
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

fn ranked_extension_overloads(fs: &FunctionSet, allow_must_inline: bool) -> Vec<&FunctionInfo> {
    let mut out: Vec<&FunctionInfo> = fs
        .overloads
        .iter()
        .filter(|o| {
            o.kind == FnKind::Extension
                && o.receiver_rank != u32::MAX
                && (o.public() || (allow_must_inline && o.flags.inline.must_inline()))
        })
        .collect();
    out.sort_by_key(|o| o.receiver_rank);
    out
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
pub struct CallResolver<'a> {
    lib: &'a dyn CompilerPlatform,
    /// The packages in scope for TOP-LEVEL function resolution (same-package, star/explicit imports,
    /// defaults). `None` disables the filter (a context with no import scope — signature inference).
    /// When `Some`, a top-level function resolves only if its facade's package is in scope, matching
    /// kotlinc: an unqualified top-level call binds ONLY to an imported/same-package/default function,
    /// not to any classpath function of that name.
    fn_scope: Option<&'a [String]>,
}

impl<'a> CallResolver<'a> {
    pub fn new(lib: &'a dyn CompilerPlatform) -> Self {
        CallResolver {
            lib,
            fn_scope: None,
        }
    }

    /// A resolver whose top-level function resolution is restricted to `fn_scope`'s packages.
    pub fn new_scoped(lib: &'a dyn CompilerPlatform, fn_scope: &'a [String]) -> Self {
        CallResolver {
            lib,
            fn_scope: Some(fn_scope),
        }
    }

    /// The unqualified-name resolution loop for this resolver's import scope — `resolve_symbols` per
    /// candidate fqn `pkg/name`. THE way to resolve an unqualified name: the caller extracts `classifier`,
    /// `callables.functions` (∪ classifier constructors, then `invoke`), or `callables.properties` from the
    /// returned namespace records. Empty when there is no import scope (the caller falls back separately).
    fn symbols_in_scope(&self, name: &str) -> Vec<(String, crate::libraries::ResolvedSymbols)> {
        self.fn_scope
            .map(|scope| resolve_symbols_in_scope(self.lib, name, scope))
            .unwrap_or_default()
    }

    /// Extension overloads of `name` APPLICABLE to `recv`, resolved through the ONE `resolve_symbols` seam:
    /// the scoped extension callables whose declared receiver is in `recv`'s supertype closure (the same
    /// `extension_receiver_rank` applicability [`select_overload`] uses). This replaces the receiver-indexed
    /// `functions(name, Some(recv))` extension lookup — `resolve_symbols` surfaces every extension (including
    /// value-class ones) by its LOGICAL `@Metadata` name + receiver, so no per-mangling special-case is
    /// needed here (the mangling is a jvm-emit concern). Falls back to the legacy `functions()` extension
    /// slice only when there is no import scope.
    pub(crate) fn receiver_extensions(&self, recv: Ty, name: &str) -> Vec<FunctionInfo> {
        let applies = |o: &FunctionInfo| {
            o.kind == FnKind::Extension
                && o.receiver
                    .and_then(|dr| self.lib.extension_receiver_rank(recv, dr))
                    .is_some()
        };
        if self.fn_scope.is_some() {
            self.symbols_in_scope(name)
                .into_iter()
                .flat_map(|(_, r)| match r.callables {
                    crate::libraries::Callables::Functions(f) => f.overloads,
                    _ => Vec::new(),
                })
                .filter(|o| applies(o))
                .collect()
        } else {
            self.lib
                .functions(name, Some(recv))
                .overloads
                .into_iter()
                .filter(|o| o.kind == FnKind::Extension)
                .collect()
        }
    }

    /// Whether `name` has an `inline` extension overload on `receiver`.
    pub fn extension_is_inline(&self, receiver: Ty, name: &str) -> bool {
        self.receiver_extensions(receiver, name)
            .iter()
            .any(|o| o.flags.inline.can_inline())
    }

    /// All TOP-LEVEL (and same-facade extension) function overloads of `name`, resolved through the ONE
    /// `resolve_symbols` seam over this resolver's import scope. Falls back to the legacy whole-classpath
    /// `functions()` query ONLY when there is no import scope — the last `functions()` users are being
    /// migrated here (task A), after which the fallback and `functions()` itself are deleted. Callers filter
    /// by [`FnKind`] as they need (`TopLevel` for a plain call, etc.).
    fn top_level_function_set(&self, name: &str) -> FunctionSet {
        FunctionSet {
            overloads: if self.fn_scope.is_some() {
                self.symbols_in_scope(name)
                    .into_iter()
                    .flat_map(|(_, r)| match r.callables {
                        crate::libraries::Callables::Functions(f) => f.overloads,
                        _ => Vec::new(),
                    })
                    .collect()
            } else {
                self.lib
                    .functions(name, None)
                    .overloads
                    .into_iter()
                    .filter(|o| fn_in_scope(o, self.fn_scope))
                    .collect()
            },
        }
    }

    /// Whether `name` has an `inline` top-level overload.
    pub fn toplevel_is_inline(&self, name: &str) -> bool {
        self.top_level_function_set(name)
            .overloads
            .iter()
            .filter(|o| o.kind == FnKind::TopLevel)
            .any(|o| o.flags.inline.can_inline())
    }

    /// Whether `name` has a `suspend` top-level overload. The flag flows uniformly from the AST
    /// (same-module `suspend fun`, via `module_symbols`) and from `@Metadata` (classpath callees).
    pub fn toplevel_is_suspend(&self, name: &str) -> bool {
        self.top_level_function_set(name)
            .overloads
            .iter()
            .filter(|o| o.kind == FnKind::TopLevel)
            .any(|o| o.flags.suspend)
    }

    /// True when `receiver.name(...)` binds a `suspend` EXTENSION (e.g. `Mutex.withLock`). The member
    /// query in the lowerer only sees instance members; a suspend extension is invisible to it, so the
    /// coroutine pass would miss the suspension point without this. Mirrors [`Self::toplevel_is_suspend`].
    pub fn extension_is_suspend(&self, name: &str, receiver: Ty) -> bool {
        self.receiver_extensions(receiver, name)
            .iter()
            .any(|o| o.flags.suspend)
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
        let fs = self.top_level_function_set(name);
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
        let fs = FunctionSet {
            overloads: resolve_symbols_in_scope(self.lib, name, &[pkg.to_string()])
                .into_iter()
                .flat_map(|(_, r)| match r.callables {
                    crate::libraries::Callables::Functions(f) => f.overloads,
                    _ => Vec::new(),
                })
                .collect(),
        };
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
            .overloads
            .iter()
            .filter(|o| o.kind == FnKind::TopLevel && o.public())
            .map(|o| (o, o.callable.params.clone(), o.callable.ret))
            .collect();

        let pick = parsed
            .iter()
            .find(|(_, params, _)| {
                params.len() == args.len()
                    && params.iter().zip(args).all(|(p, a)| self.arg_fits(p, a))
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
                        && params[..fixed]
                            .iter()
                            .zip(args)
                            .all(|(p, a)| self.arg_fits(p, a))
                        && args[fixed..].iter().all(|a| self.arg_fits(&elem, a))
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
                            unify_gsig(ps, *a, &mut binds);
                        }
                    }
                    if let GSig::Arr(inner) = &gsig.params[fixed] {
                        for a in &args[fixed..] {
                            unify_gsig(inner, *a, &mut binds);
                        }
                        vararg_elem = Some(gsig_to_ty(inner, &binds));
                    }
                } else {
                    for (ps, a) in gsig.params.iter().zip(args) {
                        unify_gsig(ps, *a, &mut binds);
                    }
                }
                gsig_to_ty(&gsig.ret, &binds)
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
        let o = select_overload(
            self.lib,
            receiver,
            name,
            args,
            type_args,
            FnKind::Extension,
            ExtCtx {
                allow_must_inline: false,
                fn_scope: self.fn_scope,
            },
        )?;
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
            return Some(self.bind_extension_callable(o, receiver, args, type_args));
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
        if let Some(c) = self.default_synthetic_callable(name, receiver, args) {
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
                let decl_recv = gsig_to_ty(p.receiver.as_ref()?, &std::collections::HashMap::new());
                let rank = self.lib.extension_receiver_rank(receiver, decl_recv)?;
                Some((rank, p))
            })
            .min_by_key(|(rank, _)| *rank)
            .map(|(_, p)| p)?;
        Some(p.getter).filter(|c| c.ret.is_read_value_result())
    }

    fn bind_extension_callable(
        &self,
        o: &FunctionInfo,
        receiver: Ty,
        args: &[Ty],
        type_args: &[Ty],
    ) -> LibraryCallable {
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
        callable_with_return(c, ret_ty2, false)
    }

    /// Find the `name$default` synthetic callable applicable to a defaulted extension call — the emit-shaped
    /// callable (receiver at `params[0]`, all real params present) the backend fills with placeholders.
    fn default_synthetic_callable(
        &self,
        name: &str,
        receiver: Ty,
        args: &[Ty],
    ) -> Option<LibraryCallable> {
        let fs = self
            .lib
            .functions(&format!("{name}$default"), Some(receiver));
        let trailing_lambda = args.last().is_some_and(|a| matches!(a, Ty::Fun(_)));
        for o in ranked_extension_overloads(&fs, false) {
            let params = &o.callable.params;
            if params.is_empty() {
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
        self.arg_fits(param, arg)
            || self
                .lib
                .value_underlying(*arg)
                .is_some_and(|underlying| *param == underlying)
            || self.reference_subtype(arg, param)
    }

    fn arg_fits(&self, param: &Ty, arg: &Ty) -> bool {
        arg_fits(param, arg)
            || param
                .fun_arity()
                .zip(self.lib.function_like_arity(*arg))
                .is_some_and(|(p, a)| usize::from(p) == a)
    }

    fn reference_subtype(&self, arg: &Ty, param: &Ty) -> bool {
        let Some(target) = param.kotlin_class_internal() else {
            return false;
        };
        let Some(start) = arg.kotlin_class_internal() else {
            return false;
        };
        if start == target {
            return true;
        }
        let mut seen = std::collections::HashSet::new();
        let mut queue = std::collections::VecDeque::new();
        queue.push_back(start.to_string());
        while let Some(internal) = queue.pop_front() {
            if !seen.insert(internal.clone()) {
                continue;
            }
            let Some(t) = self.lib.resolve_type(&internal) else {
                continue;
            };
            for sup in t.supertypes {
                if sup == target {
                    return true;
                }
                queue.push_back(sup);
            }
        }
        false
    }

    fn default_arg_mapping(
        &self,
        info: &FunctionInfo,
        params: &[Ty],
        args: &[Ty],
    ) -> Option<Vec<(usize, usize)>> {
        let real_count = params.len();
        if args.len() > real_count {
            return None;
        }
        let trailing_lambda = args.last().is_some_and(|a| matches!(a, Ty::Fun(_)));
        if trailing_lambda && args.len() < real_count {
            let last_param = real_count.checked_sub(1)?;
            if !self.arg_fits(&params[last_param], args.last().unwrap()) {
                return None;
            }
            let prefix_len = args.len() - 1;
            if !params[..prefix_len]
                .iter()
                .zip(&args[..prefix_len])
                .all(|(p, a)| self.arg_fits(p, a))
            {
                return None;
            }
            if !info.call_sig.param_defaults.is_empty()
                && (prefix_len..last_param).any(|i| {
                    !info
                        .call_sig
                        .param_defaults
                        .get(i)
                        .copied()
                        .unwrap_or(false)
                })
            {
                return None;
            }
            let mut mapping: Vec<(usize, usize)> = (0..prefix_len).map(|i| (i, i)).collect();
            mapping.push((last_param, args.len() - 1));
            return Some(mapping);
        }
        if !params[..args.len()]
            .iter()
            .zip(args)
            .all(|(p, a)| self.arg_fits(p, a))
        {
            return None;
        }
        if !info.call_sig.param_defaults.is_empty()
            && (args.len()..real_count).any(|i| {
                !info
                    .call_sig
                    .param_defaults
                    .get(i)
                    .copied()
                    .unwrap_or(false)
            })
        {
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
        let fsd = self.lib.functions(&format!("{name}$default"), None);
        for o in fsd.overloads.iter().filter(|o| o.kind == FnKind::TopLevel) {
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
                let bases: Vec<FunctionInfo> = self
                    .top_level_function_set(name)
                    .overloads
                    .into_iter()
                    .filter(|b| {
                        b.kind == FnKind::TopLevel
                            && b.generic_sig.is_some()
                            && b.callable.params.len() == params.len()
                    })
                    .collect();
                bases
                    .iter()
                    .find(|b| matches!(b.generic_sig.as_ref().map(|g| &g.ret), Some(GSig::Var(_))))
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
                            gsig.params.get(*param_i).map(|ps| (ps, args[*arg_i]))
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
        for o in fs.overloads.iter().filter(|o| o.kind == FnKind::TopLevel) {
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
                        gsig.params.iter().zip(args.iter().copied()),
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

    /// Whether `name` has a top-level overload that MUST be inlined (`@InlineOnly`, no callable method).
    pub fn toplevel_has_must_inline(&self, name: &str) -> bool {
        self.top_level_function_set(name)
            .overloads
            .iter()
            .filter(|o| o.kind == FnKind::TopLevel)
            .any(|o| o.flags.inline.must_inline())
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
        self.lib
            .functions(name, Some(receiver))
            .overloads
            .into_iter()
            .find(|o| {
                matches!(o.kind, FnKind::Extension | FnKind::Member) && o.callable.ret == lambda_ret
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
                (o.callable, o.kind == FnKind::Member)
            })
    }

    /// Parameter types for the lambda argument of a call selected by lambda return type
    /// (`Iterable<T>.sumOf { … }`). The special candidate family is represented in `FunctionSet` with
    /// `receiver_rank = u32::MAX`; bind the receiver into the generic signature and read the function
    /// parameter's input types from that selected family instead of asking the provider a second time.
    pub fn lambda_return_overload_param_types(&self, receiver: Ty, name: &str) -> Option<Vec<Ty>> {
        self.lib
            .functions(name, Some(receiver))
            .overloads
            .iter()
            .filter(|o| o.kind == FnKind::Extension && o.receiver_rank == u32::MAX)
            .find_map(|o| {
                let gsig = o.generic_sig.as_ref()?;
                let mut binds = std::collections::HashMap::new();
                if let Some(recv_sig) = &gsig.receiver {
                    unify_gsig(recv_sig, receiver, &mut binds);
                }
                gsig.params
                    .first()
                    .map(|selector| function_input_types(selector, &binds))
                    .filter(|params| !params.is_empty())
            })
    }

    /// Lambda parameter types for a receiver-less top-level call. This is arg-dependent because a
    /// generic HOF can bind lambda parameter types from already-typed non-lambda arguments
    /// (`applyIt(5) { it + 1 }`). Providers expose parsed generic signatures on `FunctionInfo`; this
    /// resolver binds them for the concrete partial call.
    pub fn top_level_lambda_param_types(
        &self,
        name: &str,
        arg_tys: &[Option<Ty>],
    ) -> Option<Vec<Vec<Ty>>> {
        let fs = self.top_level_function_set(name);
        // The default-omitted trailing-lambda alignment (`runBlocking { … }`) applies ONLY when NO overload
        // of this name matches the provided argument count exactly. A name WITH an exact-arity overload
        // (`run { … }`) always uses that overload's own parameter positions — never an alignment against a
        // wider overload — so a legitimately-empty lambda-parameter result is not shadowed by one.
        let has_exact = fs
            .overloads
            .iter()
            .any(|o| o.kind == FnKind::TopLevel && o.callable.params.len() == arg_tys.len());
        fs.overloads
            .iter()
            .filter(|o| o.kind == FnKind::TopLevel)
            .find_map(|o| {
                let gsig = o.generic_sig.as_ref()?;
                if has_exact && gsig.params.len() != arg_tys.len() {
                    return None;
                }
                let map = trailing_default_arg_indices(gsig.params.len(), arg_tys)?;
                let mut binds = std::collections::HashMap::new();
                for (ai, at) in arg_tys.iter().enumerate() {
                    if let (Some(t), Some(ps)) = (at, gsig.params.get(map[ai])) {
                        unify_gsig(ps, *t, &mut binds);
                    }
                }
                let out: Vec<Vec<Ty>> = map
                    .iter()
                    .map(|&pi| {
                        gsig.params
                            .get(pi)
                            .map(|ps| function_input_types(ps, &binds))
                            .unwrap_or_default()
                    })
                    .collect();
                out.iter()
                    .zip(arg_tys)
                    .any(|(v, at)| at.is_none() && !v.is_empty())
                    .then_some(out)
            })
    }

    /// Receiver type for each top-level function parameter that is a receiver function type
    /// (`Recv.(...) -> R`). This is source call-shape data stored on `CallSig`; the resolver only aligns
    /// it with the concrete arity before the checker binds lambda `this`.
    pub fn top_level_lambda_receivers(
        &self,
        name: &str,
        arg_tys: &[Option<Ty>],
    ) -> Option<Vec<Option<Ty>>> {
        let fs = self.top_level_function_set(name);
        // Same rule as `top_level_lambda_param_types`: only fall back to the default-omitted trailing-lambda
        // alignment (`runBlocking { … }` binds `this: CoroutineScope`) when NO overload matches the argument
        // count exactly, so an exact-arity call never mis-binds a receiver from a wider overload.
        let has_exact = fs
            .overloads
            .iter()
            .any(|o| o.kind == FnKind::TopLevel && o.callable.params.len() == arg_tys.len());
        fs.overloads
            .iter()
            .filter(|o| o.kind == FnKind::TopLevel)
            .find_map(|o| {
                let recvs = &o.call_sig.lambda_receivers;
                if has_exact && recvs.len() != arg_tys.len() {
                    return None;
                }
                let map = trailing_default_arg_indices(recvs.len(), arg_tys)?;
                let out: Vec<Option<Ty>> = map
                    .iter()
                    .map(|&pi| recvs.get(pi).cloned().flatten())
                    .collect();
                out.iter().any(|o| o.is_some()).then_some(out)
            })
    }

    /// Per-param `crossinline`/`noinline` flags for a top-level function (its lambda argument is
    /// MATERIALIZED, so a mutable capture must be `Ref`-boxed rather than inline-spliced). `None` when
    /// no matching overload carries such a parameter.
    pub fn top_level_lambda_materialized(
        &self,
        name: &str,
        arg_tys: &[Option<Ty>],
    ) -> Option<Vec<bool>> {
        self.top_level_function_set(name)
            .overloads
            .iter()
            .filter(|o| o.kind == FnKind::TopLevel)
            .find_map(|o| {
                let m = &o.call_sig.lambda_materialized;
                (m.len() == arg_tys.len() && m.iter().any(|b| *b)).then(|| m.clone())
            })
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
        let fs = self.lib.functions(name, Some(receiver));
        for allow_must_inline in [false, true] {
            for o in ranked_extension_overloads(&fs, allow_must_inline) {
                let Some(gsig) = o.generic_sig.as_ref() else {
                    continue;
                };
                let Some(param_indices) = trailing_default_arg_indices(gsig.params.len(), arg_tys)
                else {
                    continue;
                };
                let mapped: Vec<&GSig> = param_indices.iter().map(|&i| &gsig.params[i]).collect();
                let mut binds = std::collections::HashMap::new();
                if let Some(recv_sig) = &gsig.receiver {
                    unify_gsig(recv_sig, receiver, &mut binds);
                }
                for (ps, at) in mapped.iter().zip(arg_tys) {
                    if let Some(t) = at {
                        unify_gsig(ps, *t, &mut binds);
                    }
                }
                let out: Vec<Vec<Ty>> = mapped
                    .iter()
                    .map(|ps| function_input_types(ps, &binds))
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
        let fs = self.lib.functions(name, Some(receiver));
        for allow_must_inline in [false, true] {
            for o in ranked_extension_overloads(&fs, allow_must_inline) {
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
                let mapped: Vec<(usize, &GSig)> = param_indices
                    .iter()
                    .map(|&i| (i, &gsig.params[i + 1]))
                    .collect();
                let mut binds = std::collections::HashMap::new();
                unify_gsig(&gsig.params[0], receiver, &mut binds);
                for ((_, ps), at) in mapped.iter().zip(arg_tys) {
                    if let Some(t) = at {
                        unify_gsig(ps, *t, &mut binds);
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
                            return function_input_types(ps, &binds).first().copied();
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

fn descriptor_form_args(lib: &dyn CompilerPlatform, args: &[Ty]) -> Option<Vec<Ty>> {
    let out: Vec<Ty> = args.iter().map(|a| lib.jvm_descriptor_form(*a)).collect();
    (out.as_slice() != args).then_some(out)
}

fn params_match_descriptor_form(lib: &dyn CompilerPlatform, params: &[Ty], args: &[Ty]) -> bool {
    params.len() == args.len()
        && params
            .iter()
            .zip(args)
            .all(|(p, a)| lib.jvm_descriptor_form(*p) == *a)
}

/// Whether a call argument `arg` fits a parameter `param` after both are reduced to their platform
/// descriptor identity — accepting a reference argument that is a SUBTYPE of the parameter's descriptor
/// interface (`java/util/List` argument → `java/util/Collection` parameter). Non-reference sides only
/// match on identity. The supertype closure is walked through the symbol source; no collection
/// relationships are hardcoded here.
fn descriptor_arg_subtype_of_param(lib: &dyn CompilerPlatform, arg: Ty, param: Ty) -> bool {
    let pj = lib.jvm_descriptor_form(param);
    let aj = lib.jvm_descriptor_form(arg);
    if aj == pj {
        return true;
    }
    // Only a reference argument can widen to a reference parameter through the type hierarchy.
    let (Ty::Obj(arg_internal, _), Some(param_internal)) = (arg, pj.obj_internal()) else {
        return false;
    };
    is_classpath_subtype(lib, arg_internal, param_internal, 0)
}

/// Resolve a constructor on a library type by argument types (with the type's own widening).
pub fn resolve_constructor(
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
    let erased: Vec<Ty> = args
        .iter()
        .map(|a| lib.value_underlying(*a).unwrap_or(*a))
        .collect();
    if erased != args {
        if let Some(m) = t.ctor(&erased) {
            crate::trace_compiler!(
                "value_classes",
                "resolve_constructor {internal} matched via value-class-erased args {args:?} -> {erased:?}"
            );
            return Some(m.clone());
        }
    }
    // Descriptor-form matching bridges Kotlin collection identity and drops type arguments without
    // hardcoding collection relationships. Exact descriptor identity runs before subtype widening so the
    // most-specific overload still wins.
    let jvm_args = descriptor_form_args(lib, args);
    if let Some(jvm_args) = &jvm_args {
        if let Some(m) = t
            .constructors
            .iter()
            .find(|m| params_match_descriptor_form(lib, &m.params, jvm_args))
        {
            crate::trace_compiler!(
                "value_classes",
                "resolve_constructor {internal} matched via jvm-descriptor-form args {args:?} -> {jvm_args:?}"
            );
            return Some(m.clone());
        }
    }
    if let Some(m) = t.constructors.iter().find(|m| {
        m.params.len() == args.len()
            && m.params
                .iter()
                .zip(args)
                .all(|(p, a)| descriptor_arg_subtype_of_param(lib, *a, *p))
    }) {
        let mode = jvm_args
            .as_ref()
            .map_or("nominal-subtype", |_| "jvm-subtype");
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
        let fits = args.len() == 1
            && (args[0] == underlying
                || (matches!(args[0], Ty::Null) && underlying.is_reference()));
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
pub fn resolve_synthetic_constructor(
    lib: &dyn CompilerPlatform,
    internal: &str,
    args: &[Ty],
) -> Option<SyntheticCtorCall> {
    let t = lib.resolve_type(internal)?;
    // A value-class argument is passed as its erased underlying (`Vid` arg → `String` param).
    let erased: Vec<Ty> = args
        .iter()
        .map(|a| lib.value_underlying(*a).unwrap_or(*a))
        .collect();
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
        // allows for a plain constructor, here composed with the value-class-erased synthetic-marker ctor
        // (which a plain subtype pass skips because of the trailing marker parameter).
        if !erased.iter().zip(real_params).all(|(a, p)| {
            crate::libraries::arg_assignable(p, a) || descriptor_arg_subtype_of_param(lib, *a, *p)
        }) {
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
pub fn resolve_companion(
    lib: &dyn CompilerPlatform,
    internal: &str,
    name: &str,
    args: &[Ty],
) -> Option<LibraryMember> {
    let t = lib.resolve_type(internal)?;
    if !t.is_public {
        return None;
    }
    t.companion_member(name, args).cloned()
}

/// Resolve an instance member `recv.name(args)` — the receiver's static type must be public, but the
/// member may be inherited from a (possibly non-public) supertype. Candidates come from the consolidated
/// `functions` query, whose Member overloads carry the breadth-first `receiver_rank`; the closest rung's
/// best overload wins (most-derived first), exactly the inherited-member walk this used to do by hand.
pub fn resolve_instance(
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
pub fn resolve_instance_member(
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
                    unify_gsig(ps, *a, &mut binds);
                }
                let arg_bound = gsig_to_ty(&gsig.ret, &binds);
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
    let getter = lib
        .properties(property, Some(recv))
        .overloads
        .into_iter()
        .filter(|p| p.kind == PropKind::Member)
        .min_by_key(|p| p.receiver_rank)
        .map(|p| p.getter.name)?;
    // A value-class-typed property's getter is `@JvmName`-mangled (`getId-<hash>`) and erases its return
    // to the underlying type; resolving it as a plain member would type the read as the underlying, not
    // the value class. Leave those to the value-class fallback, which recovers the logical type.
    if getter.contains('-') {
        return None;
    }
    resolve_instance_member(lib, recv, &getter, &[]).filter(|m| m.ret.is_read_value_result())
}

/// Resolve a zero-arg property read on `recv`. The `@Metadata` `properties` query supplies the real
/// getter name first (no guessing); then the legacy fallbacks — the semantic Kotlin name (a
/// computed/builtin member), a `getX` physical getter, and a value-class-mangled getter.
pub fn resolve_property_member(
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
pub fn resolve_property_setter(
    lib: &dyn CompilerPlatform,
    recv: Ty,
    property: &str,
) -> Option<LibraryCallable> {
    let setter = lib
        .properties(property, Some(recv))
        .overloads
        .into_iter()
        .filter(|p| p.kind == PropKind::Member)
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
pub(crate) fn resolve_symbols_in_scope(
    lib: &dyn CompilerPlatform,
    name: &str,
    packages: &[String],
) -> Vec<(String, crate::libraries::ResolvedSymbols)> {
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
    // EXTENSION candidates come from the shared FQN seam — union `resolve_symbols` over the in-scope
    // packages (the spec's scope-pruned, tree-driven extension lookup) — so an unqualified extension binds
    // only when its facade's package is imported, without the whole-classpath eager index. MEMBERS are
    // always visible on their type (no scope), so they keep the direct `functions(name, receiver)` walk.
    // EXTENSION candidates come from the ONE query — union `resolve_symbols`' function callables over the
    // in-scope packages (scope-pruned, tree-driven), so an unqualified extension binds only when its
    // facade's package is imported. No import scope → the legacy whole-classpath `functions()` fallback
    // (removed once every consumer is scoped — task A). MEMBERS are always visible on their type.
    let fs = match (kind, ext.fn_scope) {
        (FnKind::Extension, Some(scope)) => FunctionSet {
            overloads: resolve_symbols_in_scope(lib, name, scope)
                .into_iter()
                .flat_map(|(_, r)| match r.callables {
                    crate::libraries::Callables::Functions(f) => f.overloads,
                    _ => Vec::new(),
                })
                .collect(),
        },
        _ => lib.functions(name, Some(recv)),
    };
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
                    && fn_in_scope(o, ext.fn_scope)))
    }) {
        // A receiver-agnostic `resolve_symbols` extension carries rank `0`; recover the real receiver-MRO
        // rung from the actual receiver so most-specific selection (a `List` extension over an `Iterable`
        // one) still holds. A candidate whose declared receiver is NOT in the receiver's supertype closure
        // does not apply — drop it. Members and lambda-return (`u32::MAX`) keep their provider rank.
        let rank = if kind == FnKind::Extension {
            match o
                .receiver
                .and_then(|dr| lib.extension_receiver_rank(recv, dr))
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
    // A generic method erases its type-parameter arguments to `Any` (`List<E>.add(E)` → `add(Object)`),
    // so a reference argument matches against an `Any` parameter — try the exact args, then widened.
    let widened: Vec<Ty> = args
        .iter()
        .map(|t| {
            if t.is_reference() {
                Ty::obj("kotlin/Any")
            } else {
                *t
            }
        })
        .collect();
    for cands in by_rank.values() {
        if let Some(o) =
            best_by_args(lib, cands, args).or_else(|| best_by_args(lib, cands, &widened))
        {
            return Some(o.clone());
        }
    }
    // SUBTYPE / value-class-underlying pass: an argument whose supertype closure includes the parameter
    // type (a `KSerializer` where `SerializationStrategy` is expected), or a value-class argument matching
    // its erased underlying. The exact/widened passes miss these (only exact or erased `Any`).
    for cands in by_rank.values() {
        if let Some((o, _)) = cands.iter().find(|(_, lp)| {
            lp.len() == args.len() && lp.iter().zip(args).all(|(p, a)| arg_assignable(lib, p, a))
        }) {
            return Some((*o).clone());
        }
    }
    // Descriptor-form pass, shared with constructor resolution: bridge Kotlin collection identity and
    // erase type arguments after exact, widened, and source-level subtype matching have failed.
    if let Some(jvm_args) = descriptor_form_args(lib, args) {
        for cands in by_rank.values() {
            if let Some((o, _)) = cands
                .iter()
                .find(|(_, lp)| params_match_descriptor_form(lib, lp, &jvm_args))
            {
                crate::trace_compiler!(
                    "resolve",
                    "select_overload {} matched via jvm-descriptor-form args {args:?} -> {jvm_args:?}",
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
    if o.kind != FnKind::Extension {
        return o.callable.params.clone();
    }
    match o.generic_sig.as_ref() {
        Some(gsig) => {
            let mut binds = seeded_gsig_binds(gsig, type_args);
            if let Some(recv_sig) = &gsig.receiver {
                unify_gsig(recv_sig, recv, &mut binds);
            }
            let mut out = gsig_tys(&gsig.params, &binds);
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
        None => o
            .callable
            .params
            .get(1..)
            .map(<[Ty]>::to_vec)
            .unwrap_or_default(),
    }
}

/// Whether `arg` is assignable to `param` allowing a reference SUBTYPE or a value-class argument matching
/// its erased underlying — the union of the source-level subtype rule and value-class unboxing.
fn arg_assignable(lib: &dyn CompilerPlatform, param: &Ty, arg: &Ty) -> bool {
    arg_subtype_assignable(lib, param, arg)
        || lib.value_underlying(*arg).is_some_and(|u| *param == u)
}

/// `arg`'s class transitively extends/implements `param`'s class, mapping a function-TYPE parameter
/// (`Ty::Fun`) to its `kotlin/FunctionN` class — so a `KProperty1` (which implements `Function1`) fits a
/// `(T) -> R` parameter. Uses `kotlin_class_internal` on BOTH sides (which resolves `Ty::Fun`), unlike
/// [`arg_subtype_assignable`]'s `obj_internal` (which is `None` for a function type).
fn ref_subtype_fits(lib: &dyn CompilerPlatform, param: &Ty, arg: &Ty) -> bool {
    match (param.kotlin_class_internal(), arg.kotlin_class_internal()) {
        (Some(target), Some(start)) => is_classpath_subtype(lib, start, target, 0),
        _ => false,
    }
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
        *p == *a
            || fun_arg_matches(p, a)
            || arg_assignable(lib, p, a)
            || ref_subtype_fits(lib, p, a)
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
                        p == a || *p == Ty::obj("kotlin/Any") || fun_arg_matches(p, a)
                    })
            })
        })
        .or_else(|| {
            cands.iter().find(|(o, lp)| {
                lp.len() >= args.len()
                    // A prefix match (fewer args than params) is only a valid UNDER-application when the
                    // omitted trailing parameters are optional — i.e. the call still supplies every REQUIRED
                    // parameter. Otherwise a 1-arg call would spuriously bind a 2-required-param member
                    // (`getFor(id, t)`), shadowing the genuine 1-param extension overload and erasing its
                    // generic return to `Any`. `required == 0` (no metadata) keeps the legacy behaviour.
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
                    && fun_arg_matches(&lp[last], args.last().unwrap())
                    && (prefix..last)
                        .all(|i| o.call_sig.param_defaults.get(i).copied().unwrap_or(false))
                    && lp[..prefix]
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
fn fun_arg_matches(param: &Ty, arg: &Ty) -> bool {
    let Some(arg_arity) = arg.fun_arity() else {
        return false;
    };
    let param = match param {
        Ty::Nullable(inner) => **inner,
        _ => *param,
    };
    param.fun_arity().is_some_and(|pn| pn == arg_arity)
        || param
            .obj_internal()
            .and_then(|p| p.strip_prefix("kotlin/jvm/functions/Function"))
            .and_then(|d| d.parse::<u8>().ok())
            == Some(arg_arity)
}

/// Whether `arg` is assignable to `param` allowing a reference SUBTYPE (`arg`'s classpath supertype
/// closure contains `param`). Falls back to exact / `Any` for the trivial cases.
fn arg_subtype_assignable(lib: &dyn CompilerPlatform, param: &Ty, arg: &Ty) -> bool {
    if param == arg || *param == Ty::obj("kotlin/Any") {
        return true;
    }
    // A builtin reference arg (`Ty::String`) isn't an `Obj`, so map it to its class internal name
    // (`kotlin/String`, whose classpath supertypes include `java/lang/CharSequence`/`Comparable`) — so
    // `Regex("…").matches(s: String)` matches the `CharSequence` parameter via the supertype walk.
    // Only a NON-NULLABLE reference arg maps this way: passing `String?` where `CharSequence` (non-null)
    // is expected is a null-safety error kotlinc rejects, so it must not select the overload via subtype.
    let arg_internal = if arg.is_reference() && !arg.is_nullable() {
        arg.kotlin_class_internal()
    } else {
        None
    };
    match (param.obj_internal(), arg_internal) {
        (Some(p), Some(a)) => is_classpath_subtype(lib, a, p, 0),
        _ => false,
    }
}

/// `sub` is `super_` or transitively extends/implements it (via the classpath supertype walk). `depth`
/// bounds the recursion: real class hierarchies are shallow, and the bound also guarantees termination
/// on a malformed (cyclic) classpath rather than overflowing the stack.
fn is_classpath_subtype(lib: &dyn CompilerPlatform, sub: &str, super_: &str, depth: u32) -> bool {
    let sub_desc = lib.jvm_descriptor_form(Ty::obj(sub));
    let super_desc = lib.jvm_descriptor_form(Ty::obj(super_));
    if sub == super_ || sub_desc == super_desc {
        return true;
    }
    if depth > 64 {
        return false;
    }
    if let Some(t) = lib.resolve_type(sub) {
        return t
            .supertypes
            .iter()
            .any(|s| is_classpath_subtype(lib, s, super_, depth + 1));
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
        fn functions(&self, name: &str, receiver: Option<Ty>) -> FunctionSet {
            if receiver == self.receiver && name == self.name {
                FunctionSet {
                    overloads: vec![self.info.clone()],
                }
            } else {
                FunctionSet::default()
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
            inline: InlineKind::None,
            default_call: true,
            vararg_elem: None,
            signature: None,
            origin: Origin::Library,
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
        let resolver = CallResolver::new(&source);
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
        let resolver = CallResolver::new(&source);
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
        let resolver = CallResolver::new(&source);
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
