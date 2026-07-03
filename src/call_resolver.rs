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
    FnKind, FunctionInfo, FunctionSet, GSig, InlineKind, LibraryCallable, LibraryMember,
};
use crate::symbol_source::SymbolSource;
use crate::types::Ty;

/// Bind type variables by unifying a parameter signature node with an actual argument `Ty`.
pub(crate) fn unify_gsig(
    sig: &GSig,
    actual: Ty,
    binds: &mut std::collections::HashMap<String, Ty>,
) {
    match sig {
        GSig::Var(n) => {
            binds.entry(n.clone()).or_insert(actual);
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
                for (a, p) in params.iter().zip(fsig.params.iter()) {
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
pub(crate) fn gsig_to_ty(sig: &GSig, binds: &std::collections::HashMap<String, Ty>) -> Ty {
    match sig {
        GSig::Var(n) => binds
            .get(n)
            .copied()
            .unwrap_or_else(|| Ty::obj("kotlin/Any")),
        GSig::Prim(t) => *t,
        GSig::Arr(inner) => Ty::array(gsig_to_ty(inner, binds)),
        GSig::Function { params, ret } => {
            let ps: Vec<Ty> = params.iter().map(|a| gsig_to_ty(a, binds)).collect();
            Ty::fun(ps, gsig_to_ty(ret, binds))
        }
        GSig::Class(internal, args) => {
            if args.is_empty() {
                Ty::obj(internal)
            } else {
                let targs: Vec<Ty> = args.iter().map(|a| gsig_to_ty(a, binds)).collect();
                Ty::obj_args(internal, &targs)
            }
        }
    }
}

/// If `sig` is a function type, the substituted types of its lambda parameters. Empty for anything else.
pub(crate) fn function_input_types(
    sig: &GSig,
    binds: &std::collections::HashMap<String, Ty>,
) -> Vec<Ty> {
    if let GSig::Function { params, .. } = sig {
        return params.iter().map(|a| gsig_to_ty(a, binds)).collect();
    }
    Vec::new()
}

/// Whether argument `a` can be passed where parameter `p` is expected, in erased Kotlin terms: an
/// exact match, any argument into an erased `Any` parameter, or the *same erased class* (a parameter
/// `Pair` accepts an argument `Pair<Int, String>` — generic parameters erase to the raw type).
pub(crate) fn arg_fits(p: &Ty, a: &Ty) -> bool {
    if p == a || *p == Ty::obj("kotlin/Any") {
        return true;
    }
    // A lambda value fits a function-typed parameter when arities agree; its body result is handled by
    // the selected call's generic binding, not by erased descriptor matching.
    if let (Some(pn), Some(an)) = (p.fun_arity(), a.fun_arity()) {
        return pn == an;
    }
    matches!((p, a), (Ty::Obj(pi, _), Ty::Obj(ai, _)) if pi == ai)
}

fn is_function_param(t: &Ty) -> bool {
    matches!(t, Ty::Fun(_))
}

/// Map each provided argument to a parameter index for a top-level call carrying a lambda. Identity when
/// the counts match; else, for a call that omits leading defaulted parameters before a TRAILING lambda
/// (`runBlocking { … }`), leading args → leading params and the trailing lambda → the LAST parameter.
fn default_omit_lambda_param_indices(
    param_count: usize,
    arg_tys: &[Option<Ty>],
) -> Option<Vec<usize>> {
    let n = arg_tys.len();
    if param_count == n {
        return Some((0..n).collect());
    }
    if param_count > n && n >= 1 && arg_tys[n - 1].is_none() {
        let mut map: Vec<usize> = (0..n - 1).collect();
        map.push(param_count - 1);
        return Some(map);
    }
    None
}

fn metadata_ret_with_args(meta: Ty, fallback_args: &[Ty]) -> Ty {
    match meta {
        Ty::Obj(internal, args) if args.is_empty() && !fallback_args.is_empty() => {
            Ty::obj_args(internal, fallback_args)
        }
        other => other,
    }
}

fn logical_ret_from_metadata(ret_class: Option<Ty>, fallback: Ty) -> Ty {
    ret_class
        .map(|meta| metadata_ret_with_args(meta, fallback.type_args()))
        .unwrap_or(fallback)
}

fn selected_return_type(ret_class: Option<Ty>, ret_nullable: bool, fallback: Ty) -> Ty {
    nullable_return_type(logical_ret_from_metadata(ret_class, fallback), ret_nullable)
}

/// The arg-dependent binding layer over a [`SymbolSource`]: it selects overloads and binds generics for
/// a specific call site. Holds the oracle by reference — cheap to construct per query.
pub struct CallResolver<'a> {
    lib: &'a dyn SymbolSource,
}

impl<'a> CallResolver<'a> {
    pub fn new(lib: &'a dyn SymbolSource) -> Self {
        CallResolver { lib }
    }

    /// Whether `name` has an `inline` extension overload on `receiver`.
    pub fn extension_is_inline(&self, receiver: Ty, name: &str) -> bool {
        self.lib
            .functions(name, Some(receiver))
            .overloads
            .iter()
            .any(|o| o.kind == FnKind::Extension && o.flags.inline.can_inline())
    }

    /// Whether `name` has an `inline` top-level overload.
    pub fn toplevel_is_inline(&self, name: &str) -> bool {
        self.lib
            .functions(name, None)
            .overloads
            .iter()
            .any(|o| o.flags.inline.can_inline())
    }

    /// Whether `name` has a `suspend` top-level overload. The flag flows uniformly from the AST
    /// (same-module `suspend fun`, via `module_symbols`) and from `@Metadata` (classpath callees).
    pub fn toplevel_is_suspend(&self, name: &str) -> bool {
        self.lib
            .functions(name, None)
            .overloads
            .iter()
            .any(|o| o.flags.suspend)
    }

    /// Whether `name` has a `suspend` extension overload on `receiver`.
    pub fn extension_is_suspend(&self, receiver: Ty, name: &str) -> bool {
        self.lib
            .functions(name, Some(receiver))
            .overloads
            .iter()
            .any(|o| o.kind == FnKind::Extension && o.flags.suspend)
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
        let fs = self.lib.functions(name, None);
        let parsed: Vec<(&FunctionInfo, Vec<Ty>, Ty)> = fs
            .overloads
            .iter()
            .filter(|o| o.kind == FnKind::TopLevel && o.public)
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

        if let Some(c) = self.resolve_top_level_inline_only_callable(&fs, args, type_args) {
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
                let mut binds = std::collections::HashMap::new();
                for (f, t) in gsig.formals.iter().zip(type_args) {
                    binds.insert(f.clone(), *t);
                }
                let vararg = params.len() != args.len();
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
        let ret_ty = selected_return_type(
            o.ret_class,
            o.ret_nullable,
            if o.flags.suspend { c.ret } else { ret_ty },
        );

        crate::trace_compiler!(
            "resolve",
            "top-level {name} args={args:?} -> {}.{}{} inline={:?}",
            c.owner,
            c.name,
            c.descriptor,
            c.inline
        );
        Some(LibraryCallable {
            owner: c.owner.clone(),
            name: c.name.clone(),
            params: params.clone(),
            ret: ret_ty,
            physical_ret: *ret,
            descriptor: c.descriptor.clone(),
            inline: c.inline,
            default_call: false,
            vararg_elem,
            signature: c.signature.clone(),
            origin: c.origin.clone(),
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
        self.resolve_extension_callable_exact(name, receiver, args, type_args, false)
            .or_else(|| self.resolve_extension_default_callable(name, receiver, args, type_args))
    }

    /// Resolve an extension callable for the bytecode inliner. This uses the same overload selection as
    /// ordinary extension calls, but also admits non-public `@InlineOnly` candidates because callers must
    /// splice the result and never emit it as a JVM call.
    pub fn resolve_extension_inline_callable(
        &self,
        name: &str,
        receiver: Ty,
        args: &[Ty],
    ) -> Option<LibraryCallable> {
        self.resolve_extension_callable_exact(name, receiver, args, &[], true)
    }

    /// Resolve a classpath/library extension property getter for `receiver.property`.
    /// The source supplies the platform getter spelling (`getProperty` on JVM); this layer then uses
    /// the same extension-call selector as ordinary extension calls and returns only read-value results.
    pub fn resolve_extension_property_getter(
        &self,
        property: &str,
        receiver: Ty,
    ) -> Option<LibraryCallable> {
        let getter = self.lib.physical_property_getter_name(property)?;
        self.resolve_extension_callable(&getter, receiver, &[], &[])
            .filter(|c| c.ret.is_read_value_result())
    }

    fn resolve_extension_callable_exact(
        &self,
        name: &str,
        receiver: Ty,
        args: &[Ty],
        type_args: &[Ty],
        allow_must_inline: bool,
    ) -> Option<LibraryCallable> {
        let fs = self.lib.functions(name, Some(receiver));
        let mut ranks: Vec<u32> = fs
            .overloads
            .iter()
            .filter(|o| {
                o.kind == FnKind::Extension
                    && o.receiver_rank != u32::MAX
                    && (o.public || (allow_must_inline && o.flags.inline.must_inline()))
            })
            .map(|o| o.receiver_rank)
            .collect();
        ranks.sort_unstable();
        ranks.dedup();

        for rank in ranks {
            let mut matches: Vec<(&FunctionInfo, Vec<Ty>)> = fs
                .overloads
                .iter()
                .filter_map(|o| {
                    let logical = self.bound_logical_params(o, receiver, type_args);
                    (logical.len() == args.len() + 1).then_some((o, logical))
                })
                .filter(|o| {
                    o.0.kind == FnKind::Extension
                        && o.0.receiver_rank != u32::MAX
                        && (o.0.public || (allow_must_inline && o.0.flags.inline.must_inline()))
                        && o.0.receiver_rank == rank
                        && o.1[1..]
                            .iter()
                            .zip(args)
                            .all(|(p, a)| self.arg_fits_or_subtype(p, a))
                })
                .collect();
            if matches.is_empty() {
                continue;
            }
            matches.sort_by_key(|o| o.0.overload_rank);
            let specific_over = |a: &[Ty], b: &[Ty]| -> bool {
                a.iter()
                    .zip(b)
                    .all(|(pa, pb)| self.arg_fits_or_subtype(pb, pa))
            };
            let best = (0..matches.len())
                .find(|&i| {
                    (0..matches.len())
                        .all(|j| j == i || specific_over(&matches[i].1[1..], &matches[j].1[1..]))
                })
                .unwrap_or(0);
            let o = matches[best].0;
            crate::trace_compiler!(
                "resolve",
                "extension {name} recv={receiver:?} args={args:?} inline={} -> {}.{}{} ret={:?}",
                allow_must_inline,
                o.callable.owner,
                o.callable.name,
                o.callable.descriptor,
                o.callable.ret
            );
            return Some(self.bind_extension_callable(o, receiver, args, type_args));
        }
        crate::trace_compiler!(
            "resolve",
            "extension {name} recv={receiver:?} args={args:?} inline={} -> <none>",
            allow_must_inline
        );
        None
    }

    fn bound_logical_params(&self, o: &FunctionInfo, receiver: Ty, type_args: &[Ty]) -> Vec<Ty> {
        o.generic_sig
            .as_ref()
            .map(|gsig| {
                let mut binds = std::collections::HashMap::new();
                for (f, t) in gsig.formals.iter().zip(type_args) {
                    binds.insert(f.clone(), *t);
                }
                if let Some(recv_sig) = gsig.params.first() {
                    unify_gsig(recv_sig, receiver, &mut binds);
                }
                let mut out: Vec<Ty> = gsig.params.iter().map(|p| gsig_to_ty(p, &binds)).collect();
                // A VALUE-CLASS parameter is spelled as its ERASED underlying in the JVM `Signature`
                // (`Id` → `kotlin/String`), but the callable's LOGICAL parameter carries the value-class
                // type — prefer it so a value-class ARGUMENT (`getFor(id: Id)`) matches rather than being
                // compared against the erased underlying.
                for (i, p) in out.iter_mut().enumerate() {
                    if let Some(cp) = o.callable.params.get(i) {
                        if self.lib.value_underlying(*cp).is_some() {
                            *p = *cp;
                        }
                    }
                }
                out
            })
            .unwrap_or_else(|| o.callable.params.clone())
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
            .map(|gsig| {
                let mut binds = std::collections::HashMap::new();
                for (f, t) in gsig.formals.iter().zip(type_args) {
                    binds.insert(f.clone(), *t);
                }
                let actuals: Vec<Ty> = std::iter::once(receiver)
                    .chain(args.iter().copied())
                    .collect();
                for (ps, a) in gsig.params.iter().zip(&actuals) {
                    unify_gsig(ps, *a, &mut binds);
                }
                gsig_to_ty(&gsig.ret, &binds)
            })
            .unwrap_or(c.ret);
        let ret_class = o
            .ret_class
            .filter(|meta| self.lib.value_underlying(*meta).is_some());
        let ret_ty = selected_return_type(ret_class, o.ret_nullable, ret_ty);
        LibraryCallable {
            owner: c.owner.clone(),
            name: c.name.clone(),
            params: c.params.clone(),
            ret: ret_ty,
            physical_ret: c.physical_ret,
            descriptor: c.descriptor.clone(),
            inline: c.inline,
            default_call: false,
            vararg_elem: None,
            signature: c.signature.clone(),
            origin: c.origin.clone(),
        }
    }

    fn resolve_extension_default_callable(
        &self,
        name: &str,
        receiver: Ty,
        args: &[Ty],
        type_args: &[Ty],
    ) -> Option<LibraryCallable> {
        let fs = self
            .lib
            .functions(&format!("{name}$default"), Some(receiver));
        let mut ranks: Vec<u32> = fs
            .overloads
            .iter()
            .filter(|o| o.kind == FnKind::Extension && o.public)
            .map(|o| o.receiver_rank)
            .collect();
        ranks.sort_unstable();
        ranks.dedup();

        let trailing_lambda = args.last().is_some_and(|a| matches!(a, Ty::Fun(_)));
        for rank in ranks {
            for o in fs
                .overloads
                .iter()
                .filter(|o| o.kind == FnKind::Extension && o.public && o.receiver_rank == rank)
            {
                let c = &o.callable;
                let params = &c.params;
                if params.is_empty() {
                    continue;
                }
                let real_count = params.len() - 1;
                let fits = if trailing_lambda {
                    let prefix_len = args.len() - 1;
                    prefix_len < real_count
                        && is_function_param(&params[real_count])
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
                if !fits {
                    continue;
                }
                let ret_ty = o
                    .generic_sig
                    .as_ref()
                    .map(|gsig| {
                        let mut binds = std::collections::HashMap::new();
                        for (f, t) in gsig.formals.iter().zip(type_args) {
                            binds.insert(f.clone(), *t);
                        }
                        let actuals: Vec<Ty> = std::iter::once(receiver)
                            .chain(args.iter().copied())
                            .collect();
                        for (ps, a) in gsig.params.iter().zip(&actuals) {
                            unify_gsig(ps, *a, &mut binds);
                        }
                        gsig_to_ty(&gsig.ret, &binds)
                    })
                    .unwrap_or(c.ret);
                let ret_ty = selected_return_type(o.ret_class, o.ret_nullable, ret_ty);
                return Some(LibraryCallable {
                    owner: c.owner.clone(),
                    name: c.name.clone(),
                    params: params.clone(),
                    ret: ret_ty,
                    physical_ret: c.physical_ret,
                    descriptor: c.descriptor.clone(),
                    inline: c.inline,
                    default_call: true,
                    vararg_elem: None,
                    signature: c.signature.clone(),
                    origin: c.origin.clone(),
                });
            }
        }
        None
    }

    fn arg_fits_or_subtype(&self, param: &Ty, arg: &Ty) -> bool {
        self.arg_fits(param, arg)
            || self.value_class_arg_fits(param, arg)
            || self.reference_subtype(arg, param)
    }

    fn arg_fits(&self, param: &Ty, arg: &Ty) -> bool {
        arg_fits(param, arg)
            || param
                .fun_arity()
                .zip(self.lib.function_like_arity(*arg))
                .is_some_and(|(p, a)| usize::from(p) == a)
    }

    fn value_class_arg_fits(&self, param: &Ty, arg: &Ty) -> bool {
        self.lib
            .value_underlying(*arg)
            .is_some_and(|underlying| *param == underlying)
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
            if !o.public && !o.flags.inline.must_inline() {
                continue;
            }
            let params = &c.params;
            let Some(mapping) = self.default_arg_mapping(o, params, args) else {
                continue;
            };
            let ret_ty = o
                .generic_sig
                .as_ref()
                .map(|gsig| {
                    let mut binds = std::collections::HashMap::new();
                    for (f, t) in gsig.formals.iter().zip(type_args) {
                        binds.insert(f.clone(), *t);
                    }
                    for (param_i, arg_i) in &mapping {
                        if let Some(ps) = gsig.params.get(*param_i) {
                            unify_gsig(ps, args[*arg_i], &mut binds);
                        }
                    }
                    gsig_to_ty(&gsig.ret, &binds)
                })
                .unwrap_or(c.ret);
            let ret_ty = selected_return_type(o.ret_class, o.ret_nullable, ret_ty);
            return Some(LibraryCallable {
                owner: c.owner.clone(),
                name: c.name.clone(),
                params: params.clone(),
                ret: ret_ty,
                physical_ret: c.physical_ret,
                descriptor: c.descriptor.clone(),
                inline: c.inline,
                default_call: true,
                vararg_elem: None,
                signature: c.signature.clone(),
                origin: c.origin.clone(),
            });
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
                    let mut binds = std::collections::HashMap::new();
                    for (f, t) in gsig.formals.iter().zip(type_args) {
                        binds.insert(f.clone(), *t);
                    }
                    for (ps, a) in gsig.params.iter().zip(args) {
                        unify_gsig(ps, *a, &mut binds);
                    }
                    gsig_to_ty(&gsig.ret, &binds)
                })
                .unwrap_or(c.ret);
            let logical_ret = selected_return_type(o.ret_class, o.ret_nullable, recovered);
            return Some(LibraryCallable {
                owner: c.owner.clone(),
                name: c.name.clone(),
                params: params.clone(),
                ret: logical_ret,
                physical_ret: c.physical_ret,
                descriptor: c.descriptor.clone(),
                inline: InlineKind::MustInline,
                default_call: false,
                vararg_elem: None,
                signature: c.signature.clone(),
                origin: c.origin.clone(),
            });
        }
        None
    }

    /// Whether `name` has a top-level overload that MUST be inlined (`@InlineOnly`, no callable method).
    pub fn toplevel_has_must_inline(&self, name: &str) -> bool {
        self.lib
            .functions(name, None)
            .overloads
            .iter()
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
                if let Some(recv_sig) = gsig.params.first() {
                    unify_gsig(recv_sig, receiver, &mut binds);
                }
                gsig.params
                    .get(1)
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
        let fs = self.lib.functions(name, None);
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
                let map = default_omit_lambda_param_indices(gsig.params.len(), arg_tys)?;
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
        let fs = self.lib.functions(name, None);
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
                let map = default_omit_lambda_param_indices(recvs.len(), arg_tys)?;
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
        self.lib
            .functions(name, None)
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
            let mut ranks: Vec<u32> = fs
                .overloads
                .iter()
                .filter(|o| {
                    o.kind == FnKind::Extension
                        && o.receiver_rank != u32::MAX
                        && (o.public || (allow_must_inline && o.flags.inline.must_inline()))
                })
                .map(|o| o.receiver_rank)
                .collect();
            ranks.sort_unstable();
            ranks.dedup();

            for rank in ranks {
                for o in fs.overloads.iter().filter(|o| {
                    o.kind == FnKind::Extension
                        && o.receiver_rank == rank
                        && (o.public || (allow_must_inline && o.flags.inline.must_inline()))
                }) {
                    let Some(gsig) = o.generic_sig.as_ref() else {
                        continue;
                    };
                    if gsig.params.is_empty() {
                        continue;
                    }
                    let n_real = gsig.params.len() - 1;
                    let k = arg_tys.len();
                    let trailing_lambda = k >= 1 && arg_tys[k - 1].is_none();
                    let mapped: Vec<&GSig> = if n_real == k {
                        gsig.params[1..].iter().collect()
                    } else if trailing_lambda && n_real > k && k >= 1 {
                        let mut v: Vec<&GSig> = gsig.params[1..k].iter().collect();
                        v.push(&gsig.params[n_real]);
                        v
                    } else {
                        continue;
                    };
                    let mut binds = std::collections::HashMap::new();
                    unify_gsig(&gsig.params[0], receiver, &mut binds);
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
            let mut ranks: Vec<u32> = fs
                .overloads
                .iter()
                .filter(|o| {
                    o.kind == FnKind::Extension
                        && o.receiver_rank != u32::MAX
                        && (o.public || (allow_must_inline && o.flags.inline.must_inline()))
                })
                .map(|o| o.receiver_rank)
                .collect();
            ranks.sort_unstable();
            ranks.dedup();

            for rank in ranks {
                for o in fs.overloads.iter().filter(|o| {
                    o.kind == FnKind::Extension
                        && o.receiver_rank == rank
                        && (o.public || (allow_must_inline && o.flags.inline.must_inline()))
                }) {
                    let Some(gsig) = o.generic_sig.as_ref() else {
                        continue;
                    };
                    if gsig.params.is_empty() {
                        continue;
                    }
                    let n_real = gsig.params.len() - 1;
                    let k = arg_tys.len();
                    let trailing_lambda = k >= 1 && arg_tys[k - 1].is_none();
                    let mapped: Vec<(usize, &GSig)> = if n_real == k {
                        gsig.params[1..].iter().enumerate().collect()
                    } else if trailing_lambda && n_real > k && k >= 1 {
                        let mut v: Vec<(usize, &GSig)> =
                            gsig.params[1..k].iter().enumerate().collect();
                        v.push((n_real - 1, &gsig.params[n_real]));
                        v
                    } else {
                        continue;
                    };
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
        }
        None
    }
}

// --- Navigation helpers (member/constructor resolution expressed purely against the trait) --------
// The inherited-member walk over a library type's hierarchy — arg-dependent binding, so it lives in
// this layer (not the oracle). `resolve` and `ir_lower` share one implementation, backend-agnostic.

/// Resolve a constructor on a library type by argument types (with the type's own widening).
/// Whether a call argument `arg` fits a constructor parameter `param` after both are reduced to their
/// JVM-descriptor identity — accepting a reference argument that is a SUBTYPE of the parameter's JVM
/// interface (`java/util/List` argument → `java/util/Collection` parameter). Non-reference sides only
/// match on identity. The supertype closure is walked through the classpath (`resolve_type`); no
/// collection relationships are hardcoded.
fn ctor_arg_subtype_of_param(lib: &dyn SymbolSource, arg: Ty, param: Ty) -> bool {
    let pj = lib.jvm_descriptor_form(param).unwrap_or(param);
    let aj = lib.jvm_descriptor_form(arg).unwrap_or(arg);
    if aj == pj {
        return true;
    }
    // Only a reference argument can widen to a reference parameter through the type hierarchy.
    let (Ty::Obj(arg_internal, _), Ty::Obj(param_jvm, _)) = (arg, pj) else {
        return false;
    };
    let mut stack = vec![arg_internal.to_string()];
    let mut seen = std::collections::HashSet::new();
    while let Some(cur) = stack.pop() {
        if !seen.insert(cur.clone()) {
            continue;
        }
        // Compare each visited supertype in its JVM-descriptor identity (a Kotlin collection supertype
        // such as `kotlin/collections/Collection` erases to `java/util/Collection`).
        let cur_jvm = lib
            .jvm_descriptor_form(Ty::obj(&cur))
            .and_then(|t| t.obj_internal().map(str::to_string))
            .unwrap_or_else(|| cur.clone());
        if cur_jvm == param_jvm {
            return true;
        }
        if let Some(t) = lib.resolve_type(&cur) {
            stack.extend(t.supertypes);
        }
    }
    false
}

pub fn resolve_constructor(
    lib: &dyn SymbolSource,
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
    // A parameter typed as a Kotlin COLLECTION erases in the `<init>` descriptor to its single JVM
    // interface with the type argument dropped (`Set<String>` → `Ljava/util/Set;`), but the call passes
    // the Kotlin type itself (`Rule(setOf("a"))` → arg `kotlin/collections/Set<String>`). Retry matching
    // both parameter and argument in their JVM-descriptor form — the collection identity is bridged and
    // type arguments erased — so the exact-`Ty` compare above (which sees `java/util/Set` vs
    // `kotlin/collections/Set<String>`) can succeed. Normalizing BOTH sides keeps overloads distinct
    // (`java/util/List` ≠ `java/util/Set`) and never coerces a scalar parameter.
    let jvm_args: Vec<Ty> = args
        .iter()
        .map(|a| lib.jvm_descriptor_form(*a).unwrap_or(*a))
        .collect();
    if jvm_args != args {
        // First pass — EXACT JVM-descriptor form on every parameter. This is tried before the
        // subtype pass so a `Rule(List)` overload always wins over a `Rule(Collection)` one when the
        // call passes a `List` (the most-specific constructor).
        if let Some(m) = t.constructors.iter().find(|m| {
            m.params.len() == jvm_args.len()
                && m.params
                    .iter()
                    .zip(&jvm_args)
                    .all(|(p, a)| lib.jvm_descriptor_form(*p).unwrap_or(*p) == *a)
        }) {
            crate::trace_compiler!(
                "value_classes",
                "resolve_constructor {internal} matched via jvm-descriptor-form args {args:?} -> {jvm_args:?}"
            );
            return Some(m.clone());
        }
        // Second pass — a reference argument may be a SUBTYPE of the parameter's JVM identity
        // (`Rule(val c: Collection<String>)` called with `listOf(…)`: `java/util/List` is-a
        // `java/util/Collection`). Walk each argument's supertype closure (in the classpath, no
        // hardcoded relationships) to the parameter's erased JVM interface.
        if let Some(m) = t.constructors.iter().find(|m| {
            m.params.len() == jvm_args.len()
                && m.params
                    .iter()
                    .zip(args)
                    .all(|(p, a)| ctor_arg_subtype_of_param(lib, *a, *p))
        }) {
            crate::trace_compiler!(
                "value_classes",
                "resolve_constructor {internal} matched via jvm-subtype form args {args:?}"
            );
            return Some(m.clone());
        }
    }
    // A reference argument may be a plain NOMINAL SUBTYPE of the parameter (`Outer(s: Sub)` called with a
    // sealed/open subclass `Sub.U(…)`). No collection erasure is involved, so `jvm_args == args` and the
    // subtype pass inside that block above never ran; walk each argument's classpath supertype closure to
    // its parameter here. Runs only AFTER every exact match failed, so the most-specific constructor still
    // wins; `ctor_arg_subtype_of_param` restricts widening to reference (`Ty::Obj`) arg↔param pairs, so a
    // scalar parameter is never coerced.
    if let Some(m) = t.constructors.iter().find(|m| {
        m.params.len() == args.len()
            && m.params
                .iter()
                .zip(args)
                .all(|(p, a)| ctor_arg_subtype_of_param(lib, *a, *p))
    }) {
        crate::trace_compiler!(
            "value_classes",
            "resolve_constructor {internal} matched via nominal-subtype args {args:?}"
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
            && lib.value_class_ctor_has_default(internal);
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
    lib: &dyn SymbolSource,
    internal: &str,
    arity: usize,
) -> Option<(String, Vec<Ty>)> {
    let t = lib.resolve_type(internal)?;
    let is_marker = |ty: &Ty| matches!(ty, Ty::Obj(n, _) if *n == "kotlin/jvm/internal/DefaultConstructorMarker");
    let m = t.constructors.iter().find(|m| {
        m.params.len() == arity + 2 && is_marker(&m.params[arity + 1]) && m.params[arity] == Ty::Int
    })?;
    Some((m.descriptor.clone(), m.params[..arity].to_vec()))
}

/// The classpath default-value synthetic for a MEMBER — `name$default(Owner, <params…>, int mask,
/// Object marker): Ret` (a static, e.g. a data class's `copy$default`) — as `(descriptor, real_params,
/// ret)`, the parameter types being the source method's (WITHOUT the leading receiver and trailing
/// mask/marker). Lets a call omit a defaulted argument. `None` when the class has no such synthetic.
pub fn synthetic_default_member(
    lib: &dyn SymbolSource,
    owner: &str,
    name: &str,
    arity: usize,
) -> Option<(String, Vec<Ty>, Ty, bool)> {
    let t = lib.resolve_type(owner)?;
    let dname = format!("{name}$default");
    let is_continuation =
        |ty: &Ty| matches!(ty, Ty::Obj(n, _) if *n == "kotlin/coroutines/Continuation");
    // Shape `(Owner receiver, <real params…>, int mask, Object marker)`: exactly `arity` real params, an
    // `int` mask, and a reference marker. Match by `arity` (not just name) so an overloaded `name$default`
    // of a different parameter count can't be picked.
    if let Some(m) = t.companion.iter().find(|m| {
        m.name == dname
            && m.params.len() == arity + 3
            && m.params[arity + 1] == Ty::Int
            && m.params[arity + 2].is_reference()
    }) {
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
            && m.params.len() == arity + 4
            && is_continuation(&m.params[arity + 1])
            && m.params[arity + 2] == Ty::Int
            && m.params[arity + 3].is_reference()
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
    lib: &dyn SymbolSource,
    internal: &str,
    args: &[Ty],
) -> Option<SyntheticCtorCall> {
    let t = lib.resolve_type(internal)?;
    let is_marker = |ty: &Ty| matches!(ty, Ty::Obj(n, _) if *n == "kotlin/jvm/internal/DefaultConstructorMarker");
    // A value-class argument is passed as its erased underlying (`Vid` arg → `String` param).
    let erased: Vec<Ty> = args
        .iter()
        .map(|a| lib.value_underlying(*a).unwrap_or(*a))
        .collect();
    for m in &t.constructors {
        if m.params.last().is_none_or(|p| !is_marker(p)) {
            continue;
        }
        let leading = &m.params[..m.params.len() - 1];
        // Tell the default-mask shape (`…, int mask, marker`) from the value-class-param shape (`…, marker`):
        // a mask int is present iff dropping it leaves the params of a SIBLING non-marker ctor (the public
        // primary). Otherwise the trailing int is a real parameter.
        let (real_params, has_mask): (&[Ty], bool) = if leading.last() == Some(&Ty::Int)
            && !leading.is_empty()
            && t.constructors.iter().any(|s| {
                s.params.last().is_none_or(|p| !is_marker(p))
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
            crate::libraries::arg_assignable(p, a) || ctor_arg_subtype_of_param(lib, *a, *p)
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
    lib: &dyn SymbolSource,
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
    lib: &dyn SymbolSource,
    internal: &str,
    name: &str,
    args: &[Ty],
) -> Option<LibraryMember> {
    select_instance_info(lib, Ty::obj(internal), name, args).map(|o| {
        let ret = selected_return_type(o.ret_class, o.ret_nullable, o.callable.ret);
        let mut member = LibraryMember::new(
            o.callable.name,
            o.callable.params,
            ret,
            o.callable.descriptor,
        );
        member.owner = Some(o.callable.owner);
        member.physical_ret = o.callable.physical_ret;
        member.signature = o.callable.signature;
        member.ret_nullable = o.ret_nullable;
        member.inline = o.flags.inline;
        member.suspend = o.flags.suspend;
        member
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
    lib: &dyn SymbolSource,
    recv: Ty,
    name: &str,
    args: &[Ty],
) -> Option<ResolvedMember> {
    let o = select_instance_info(lib, recv, name, args)?;
    let mut member = LibraryMember::new(
        o.callable.name.clone(),
        o.callable.params.clone(),
        o.callable.ret,
        o.callable.descriptor.clone(),
    );
    member.owner = Some(o.callable.owner.clone());
    member.physical_ret = o.callable.physical_ret;
    member.signature = o.callable.signature.clone();
    member.ret_nullable = o.ret_nullable;
    member.inline = o.flags.inline;
    member.suspend = o.flags.suspend;
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
    let ret = selected_return_type(o.ret_class, o.ret_nullable, ret);
    Some(ResolvedMember {
        ret,
        member,
        suspend: o.flags.suspend,
    })
}

fn nullable_return_type(ret: Ty, ret_nullable: bool) -> Ty {
    if !ret_nullable || ret.is_nullable() {
        return ret;
    }
    if ret.boxed_ref().is_some() || ret.is_reference() {
        Ty::nullable(ret)
    } else {
        ret
    }
}

/// Resolve a zero-arg property read on `recv`. The semantic Kotlin property name is tried first; if
/// the source has only a physical getter method, the source supplies that fallback spelling.
pub fn resolve_property_member(
    lib: &dyn SymbolSource,
    recv: Ty,
    property: &str,
) -> Option<ResolvedMember> {
    resolve_instance_member(lib, recv, property, &[])
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
            let member = lib.value_class_property_member(internal, property)?;
            let ret = member.ret;
            Some(ResolvedMember {
                member,
                ret,
                suspend: false,
            })
        })
}

fn select_instance_info(
    lib: &dyn SymbolSource,
    recv: Ty,
    name: &str,
    args: &[Ty],
) -> Option<FunctionInfo> {
    let internal = recv.kotlin_class_internal()?;
    if !lib.resolve_type(internal)?.is_public {
        return None;
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
    let fs = lib.functions(name, Some(recv));
    let mut by_rank: std::collections::BTreeMap<u32, Vec<&FunctionInfo>> =
        std::collections::BTreeMap::new();
    for o in fs.overloads.iter().filter(|o| o.kind == FnKind::Member) {
        by_rank.entry(o.receiver_rank).or_default().push(o);
    }
    for members in by_rank.values() {
        if let Some(o) = best_member_overload(members.iter().copied(), name, args)
            .or_else(|| best_member_overload(members.iter().copied(), name, &widened))
        {
            return Some(o.clone());
        }
    }
    // Third pass — SUBTYPE-aware: an argument whose supertype closure includes the parameter type
    // (e.g. a `KSerializer` passed where `SerializationStrategy` is expected — `KSerializer<T> :
    // SerializationStrategy<T>`). The exact/widened passes above miss this because ordinary member
    // assignability only accepts an exact type or an erased `Any`.
    for members in by_rank.values() {
        if let Some(o) = members.iter().copied().find(|o| {
            o.callable.params.len() == args.len()
                && o.callable
                    .params
                    .iter()
                    .zip(args)
                    .all(|(p, a)| arg_subtype_assignable(lib, p, a))
        }) {
            return Some(o.clone());
        }
    }
    // Fourth pass — JVM-descriptor form on BOTH sides, mirroring the constructor path
    // (`resolve_constructor`). A parameter typed as a Kotlin COLLECTION erases in the method descriptor to
    // its single JVM interface with the type argument dropped (`List<String>` → `Ljava/util/List;`), but the
    // call passes the Kotlin type itself (`h.size(listOf("a"))` → arg `kotlin/collections/List<String>`).
    // The exact/widened/subtype passes above all see `java/util/List` vs `kotlin/collections/List<String>`
    // and miss. Normalizing both sides bridges the collection identity and erases type arguments, while
    // keeping distinct interfaces distinct (`java/util/List` ≠ `java/util/Set`) and never coercing a scalar.
    let jvm_args: Vec<Ty> = args
        .iter()
        .map(|a| lib.jvm_descriptor_form(*a).unwrap_or(*a))
        .collect();
    if jvm_args != args {
        for members in by_rank.values() {
            if let Some(o) = members.iter().copied().find(|o| {
                o.callable.params.len() == jvm_args.len()
                    && o.callable
                        .params
                        .iter()
                        .zip(&jvm_args)
                        .all(|(p, a)| lib.jvm_descriptor_form(*p).unwrap_or(*p) == *a)
            }) {
                crate::trace_compiler!(
                    "resolve",
                    "select_instance_info {} matched via jvm-descriptor-form args {args:?} -> {jvm_args:?}",
                    o.callable.name
                );
                return Some(o.clone());
            }
        }
    }
    None
}

fn best_member_overload<'a>(
    candidates: impl Iterator<Item = &'a FunctionInfo> + Clone,
    _name: &str,
    args: &[Ty],
) -> Option<&'a FunctionInfo> {
    candidates
        .clone()
        .find(|o| o.callable.params == *args)
        .or_else(|| {
            candidates.clone().find(|o| {
                o.callable.params.len() == args.len()
                    && o.callable.params.iter().zip(args).all(|(p, a)| {
                        p == a || *p == Ty::obj("kotlin/Any") || fun_arg_matches(p, a)
                    })
            })
        })
        .or_else(|| {
            candidates.clone().find(|o| {
                o.callable.params.len() >= args.len()
                    && o.callable.params[..args.len()]
                        .iter()
                        .zip(args)
                        .all(|(p, a)| p == a || fun_arg_matches(p, a))
            })
        })
}

/// A lambda argument (`Ty::Fun`) matches a function-typed parameter of the same arity. The parameter may
/// be a decoded `Ty::Fun` (whose return/parameter types differ from the lambda's — the body adapts) or an
/// erased `kotlin/jvm/functions/FunctionN` object; neither pairs with the argument under plain equality or
/// `Any` widening, so arity alone drives the match.
fn fun_arg_matches(param: &Ty, arg: &Ty) -> bool {
    let arg_arity = match arg.fun_arity() {
        Some(n) => n,
        None => return false,
    };
    let param = match param {
        Ty::Nullable(inner) => **inner,
        _ => *param,
    };
    if let Some(pn) = param.fun_arity() {
        return pn == arg_arity;
    }
    match param.obj_internal() {
        Some(p) => {
            p.strip_prefix("kotlin/jvm/functions/Function")
                .and_then(|d| d.parse::<u8>().ok())
                == Some(arg_arity)
        }
        None => false,
    }
}

/// Whether `arg` is assignable to `param` allowing a reference SUBTYPE (`arg`'s classpath supertype
/// closure contains `param`). Falls back to exact / `Any` for the trivial cases.
fn arg_subtype_assignable(lib: &dyn SymbolSource, param: &Ty, arg: &Ty) -> bool {
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
fn is_classpath_subtype(lib: &dyn SymbolSource, sub: &str, super_: &str, depth: u32) -> bool {
    if sub == super_ {
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
    use crate::libraries::{CallSig, FnFlags, FunctionSet, LibraryCallable, Origin, TypeKind};

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
            })
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
            kind: FnKind::TopLevel,
            receiver: None,
            ret_nullable: false,
            ret_class: Some(Ty::UInt),
            flags: FnFlags::default(),
            callable,
            public: true,
            receiver_rank: 0,
            overload_rank: 0,
            generic_sig: None,
            call_sig: CallSig {
                required: 0,
                param_defaults: vec![true],
                ..Default::default()
            },
        }
    }

    fn top_level_nullable_string_info() -> FunctionInfo {
        let callable = LibraryCallable {
            owner: "kotlin/FooKt".to_string(),
            name: "maybe".to_string(),
            params: vec![],
            ret: Ty::String,
            physical_ret: Ty::String,
            descriptor: "()Ljava/lang/String;".to_string(),
            inline: InlineKind::None,
            default_call: false,
            vararg_elem: None,
            signature: None,
            origin: Origin::Library,
        };
        FunctionInfo {
            kind: FnKind::TopLevel,
            receiver: None,
            ret_nullable: true,
            ret_class: None,
            flags: FnFlags::default(),
            callable,
            public: true,
            receiver_rank: 0,
            overload_rank: 0,
            generic_sig: None,
            call_sig: CallSig::default(),
        }
    }

    fn extension_nullable_string_info() -> FunctionInfo {
        let receiver = Ty::String;
        let callable = LibraryCallable {
            owner: "kotlin/text/StringsKt".to_string(),
            name: "maybeSuffix".to_string(),
            params: vec![receiver],
            ret: Ty::String,
            physical_ret: Ty::String,
            descriptor: "(Ljava/lang/String;)Ljava/lang/String;".to_string(),
            inline: InlineKind::None,
            default_call: false,
            vararg_elem: None,
            signature: None,
            origin: Origin::Library,
        };
        FunctionInfo {
            kind: FnKind::Extension,
            receiver: Some(receiver),
            ret_nullable: true,
            ret_class: None,
            flags: FnFlags::default(),
            callable,
            public: true,
            receiver_rank: 0,
            overload_rank: 0,
            generic_sig: None,
            call_sig: CallSig::default(),
        }
    }

    fn member_nullable_string_info() -> FunctionInfo {
        let receiver = Ty::obj("demo/Box");
        let callable = LibraryCallable {
            owner: "demo/Box".to_string(),
            name: "maybe".to_string(),
            params: vec![],
            ret: Ty::String,
            physical_ret: Ty::String,
            descriptor: "()Ljava/lang/String;".to_string(),
            inline: InlineKind::None,
            default_call: false,
            vararg_elem: None,
            signature: None,
            origin: Origin::Library,
        };
        FunctionInfo {
            kind: FnKind::Member,
            receiver: Some(receiver),
            ret_nullable: true,
            ret_class: None,
            flags: FnFlags::default(),
            callable,
            public: true,
            receiver_rank: 0,
            overload_rank: 0,
            generic_sig: None,
            call_sig: CallSig::default(),
        }
    }

    fn member_metadata_class_info() -> FunctionInfo {
        let receiver = Ty::obj("demo/Box");
        let callable = LibraryCallable {
            owner: "demo/Box".to_string(),
            name: "names".to_string(),
            params: vec![],
            ret: Ty::obj("kotlin/Any"),
            physical_ret: Ty::obj("kotlin/Any"),
            descriptor: "()Ljava/lang/Object;".to_string(),
            inline: InlineKind::None,
            default_call: false,
            vararg_elem: None,
            signature: None,
            origin: Origin::Library,
        };
        FunctionInfo {
            kind: FnKind::Member,
            receiver: Some(receiver),
            ret_nullable: false,
            ret_class: Some(Ty::obj_args("kotlin/collections/List", &[Ty::String])),
            flags: FnFlags::default(),
            callable,
            public: true,
            receiver_rank: 0,
            overload_rank: 0,
            generic_sig: None,
            call_sig: CallSig::default(),
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

    // --- Pure signature-tree helpers (unify_gsig / gsig_to_ty / …) ---------------------------

    use std::collections::HashMap;

    #[test]
    fn unify_gsig_var_binds_and_keeps_first() {
        let mut binds: HashMap<String, Ty> = HashMap::new();
        unify_gsig(&GSig::Var("T".into()), Ty::Int, &mut binds);
        assert_eq!(binds.get("T"), Some(&Ty::Int));
        // `or_insert` — a second unification of the same variable keeps the first binding.
        unify_gsig(&GSig::Var("T".into()), Ty::String, &mut binds);
        assert_eq!(binds.get("T"), Some(&Ty::Int));
    }

    #[test]
    fn unify_gsig_class_binds_positional_type_args() {
        // `List<T>` unified against `List<Int>` binds `T -> Int`.
        let mut binds: HashMap<String, Ty> = HashMap::new();
        let sig = GSig::Class(
            "kotlin/collections/List".into(),
            vec![GSig::Var("T".into())],
        );
        unify_gsig(
            &sig,
            Ty::obj_args("kotlin/collections/List", &[Ty::Int]),
            &mut binds,
        );
        assert_eq!(binds.get("T"), Some(&Ty::Int));

        // A non-`Obj` actual leaves the variable unbound (nothing to unify against).
        let mut empty: HashMap<String, Ty> = HashMap::new();
        unify_gsig(&sig, Ty::Int, &mut empty);
        assert!(empty.is_empty());
    }

    #[test]
    fn unify_gsig_arr_binds_element_only_for_arrays() {
        let mut binds: HashMap<String, Ty> = HashMap::new();
        let sig = GSig::Arr(Box::new(GSig::Var("E".into())));
        unify_gsig(&sig, Ty::array(Ty::String), &mut binds);
        assert_eq!(binds.get("E"), Some(&Ty::String));

        // A non-array actual binds nothing.
        let mut empty: HashMap<String, Ty> = HashMap::new();
        unify_gsig(&sig, Ty::String, &mut empty);
        assert!(empty.is_empty());
    }

    #[test]
    fn unify_gsig_function_binds_params_and_return() {
        // `Function1<A, R>` unified against `(Int) -> Boolean` binds both `A` and `R`.
        let mut binds: HashMap<String, Ty> = HashMap::new();
        let sig = GSig::Function {
            params: vec![GSig::Var("A".into())],
            ret: Box::new(GSig::Var("R".into())),
        };
        unify_gsig(&sig, Ty::fun(vec![Ty::Int], Ty::Boolean), &mut binds);
        assert_eq!(binds.get("A"), Some(&Ty::Int));
        assert_eq!(binds.get("R"), Some(&Ty::Boolean));

        // A non-function actual binds nothing.
        let mut empty: HashMap<String, Ty> = HashMap::new();
        unify_gsig(&sig, Ty::Int, &mut empty);
        assert!(empty.is_empty());
    }

    #[test]
    fn unify_gsig_prim_is_a_noop() {
        let mut binds: HashMap<String, Ty> = HashMap::new();
        unify_gsig(&GSig::Prim(Ty::Int), Ty::Long, &mut binds);
        assert!(binds.is_empty());
    }

    #[test]
    fn gsig_to_ty_realizes_each_arm() {
        let mut binds: HashMap<String, Ty> = HashMap::new();
        binds.insert("T".into(), Ty::Int);
        // Bound variable → its binding; unbound → erases to `Any`.
        assert_eq!(gsig_to_ty(&GSig::Var("T".into()), &binds), Ty::Int);
        assert_eq!(
            gsig_to_ty(&GSig::Var("U".into()), &binds),
            Ty::obj("kotlin/Any")
        );
        // Prim passes through.
        assert_eq!(gsig_to_ty(&GSig::Prim(Ty::Double), &binds), Ty::Double);
        // Array wraps its (substituted) element.
        assert_eq!(
            gsig_to_ty(&GSig::Arr(Box::new(GSig::Var("T".into()))), &binds),
            Ty::array(Ty::Int)
        );
        // Function realizes to a `Ty::Fun` with substituted params/return.
        let f = GSig::Function {
            params: vec![GSig::Var("T".into())],
            ret: Box::new(GSig::Prim(Ty::Boolean)),
        };
        assert_eq!(gsig_to_ty(&f, &binds), Ty::fun(vec![Ty::Int], Ty::Boolean));
        // Class with no args → bare obj; with args → obj_args carrying the substitutions.
        assert_eq!(
            gsig_to_ty(&GSig::Class("demo/Box".into(), vec![]), &binds),
            Ty::obj("demo/Box")
        );
        assert_eq!(
            gsig_to_ty(
                &GSig::Class(
                    "kotlin/collections/List".into(),
                    vec![GSig::Var("T".into())]
                ),
                &binds
            ),
            Ty::obj_args("kotlin/collections/List", &[Ty::Int])
        );
    }

    #[test]
    fn function_input_types_only_for_function_sigs() {
        let mut binds: HashMap<String, Ty> = HashMap::new();
        binds.insert("T".into(), Ty::Int);
        let f = GSig::Function {
            params: vec![GSig::Var("T".into()), GSig::Prim(Ty::String)],
            ret: Box::new(GSig::Prim(Ty::Unit)),
        };
        assert_eq!(function_input_types(&f, &binds), vec![Ty::Int, Ty::String]);
        // Anything that is not a function type yields no inputs.
        assert!(function_input_types(&GSig::Prim(Ty::Int), &binds).is_empty());
    }

    #[test]
    fn arg_fits_free_fn_covers_each_branch() {
        // Exact match and erased `Any` parameter always fit.
        assert!(arg_fits(&Ty::Int, &Ty::Int));
        assert!(arg_fits(&Ty::obj("kotlin/Any"), &Ty::String));
        // Function-typed parameter fits any lambda of the same arity (bodies bound elsewhere).
        assert!(arg_fits(
            &Ty::fun(vec![Ty::Int], Ty::Int),
            &Ty::fun(vec![Ty::String], Ty::Boolean)
        ));
        assert!(!arg_fits(
            &Ty::fun(vec![Ty::Int], Ty::Int),
            &Ty::fun(vec![], Ty::Int)
        ));
        // Same erased class fits regardless of type arguments.
        assert!(arg_fits(
            &Ty::obj("kotlin/collections/List"),
            &Ty::obj_args("kotlin/collections/List", &[Ty::Int])
        ));
        // Different classes and primitive mismatches do not fit.
        assert!(!arg_fits(&Ty::obj("demo/A"), &Ty::obj("demo/B")));
        assert!(!arg_fits(&Ty::Int, &Ty::String));
    }

    #[test]
    fn is_function_param_detects_fun_types() {
        assert!(is_function_param(&Ty::fun(vec![], Ty::Unit)));
        assert!(!is_function_param(&Ty::Int));
        assert!(!is_function_param(&Ty::obj("demo/Box")));
    }

    #[test]
    fn metadata_ret_with_args_fills_bare_obj_from_fallback() {
        // A bare metadata `Obj` with no args adopts the fallback's args (the descriptor-level args).
        assert_eq!(
            metadata_ret_with_args(Ty::obj("kotlin/collections/List"), &[Ty::Int]),
            Ty::obj_args("kotlin/collections/List", &[Ty::Int])
        );
        // Already-parameterized metadata is left untouched even when a fallback exists.
        assert_eq!(
            metadata_ret_with_args(
                Ty::obj_args("kotlin/collections/List", &[Ty::String]),
                &[Ty::Int]
            ),
            Ty::obj_args("kotlin/collections/List", &[Ty::String])
        );
        // No fallback args → pass through unchanged.
        assert_eq!(
            metadata_ret_with_args(Ty::obj("demo/Box"), &[]),
            Ty::obj("demo/Box")
        );
        // A non-`Obj` metadata is returned as-is.
        assert_eq!(metadata_ret_with_args(Ty::Int, &[Ty::String]), Ty::Int);
    }

    #[test]
    fn logical_ret_and_selected_return_type() {
        // With metadata: the bare obj is refined with the fallback's carried args.
        assert_eq!(
            logical_ret_from_metadata(
                Some(Ty::obj("kotlin/collections/List")),
                Ty::obj_args("kotlin/collections/List", &[Ty::Int])
            ),
            Ty::obj_args("kotlin/collections/List", &[Ty::Int])
        );
        // Without metadata: the fallback is used directly.
        assert_eq!(logical_ret_from_metadata(None, Ty::String), Ty::String);
        // selected_return_type layers nullability on top of the logical return.
        assert_eq!(
            selected_return_type(None, true, Ty::String),
            Ty::nullable(Ty::String)
        );
        assert_eq!(selected_return_type(None, false, Ty::String), Ty::String);
    }

    #[test]
    fn nullable_return_type_only_lifts_reference_like_returns() {
        // Not flagged nullable → unchanged.
        assert_eq!(nullable_return_type(Ty::String, false), Ty::String);
        // Already nullable → unchanged (no double wrap).
        let ns = Ty::nullable(Ty::String);
        assert_eq!(nullable_return_type(ns, true), ns);
        // A reference gains `?`.
        assert_eq!(
            nullable_return_type(Ty::String, true),
            Ty::nullable(Ty::String)
        );
        // A boxable primitive gains `?` (it boxes).
        assert_eq!(nullable_return_type(Ty::Int, true), Ty::nullable(Ty::Int));
        // A non-reference, non-boxable return (Unit) stays as-is even when flagged.
        assert_eq!(nullable_return_type(Ty::Unit, true), Ty::Unit);
    }
}
