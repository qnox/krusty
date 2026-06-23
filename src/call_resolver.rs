//! Call resolution — the binding layer that sits *above* a [`LibrarySet`].
//!
//! A [`LibrarySet`] is a pure, arg-INDEPENDENT metadata oracle: given a name (and optional receiver)
//! it returns every overload with its raw signature and flags ([`crate::libraries::FunctionSet`]). It
//! does no overload selection and no type-variable binding.
//!
//! [`CallResolver`] is the arg-DEPENDENT layer on top: given the actual argument types at a call site
//! it selects the right overload and binds the generic receiver/parameter/return types. It is platform
//! agnostic — it only ever talks to the oracle through the [`LibrarySet`] trait, so the same binding
//! logic serves every backend (JVM today, JS later). The platform-specific bits (parsing a backend's
//! generic-signature string into [`GSig`]) live behind the trait; the binding *algorithm* over [`GSig`]
//! lives here.

use crate::libraries::{FnKind, LibraryCallable, LibraryMember, LibrarySet};
use crate::types::Ty;

/// A parsed generic-signature node, platform neutral. A backend parses its own signature format into
/// this tree (the JVM reads a `Signature` attribute); the binding algorithm below unifies and
/// substitutes over it without knowing which backend produced it.
#[derive(Clone, Debug)]
pub(crate) enum GSig {
    Var(String),
    Class(String, Vec<GSig>),
    Arr(Box<GSig>),
    Prim(Ty),
}

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
        GSig::Class(internal, args) if internal.starts_with("kotlin/jvm/functions/Function") => {
            // A function parameter (`Function1<T, R>`) unifies against a lambda argument (`Ty::Fun`):
            // the leading type arguments bind the lambda's parameters, the last binds its return —
            // so `map`'s `R` binds from the lambda body's type (`{ it * 2 }` → `Int`).
            if let Ty::Fun(fsig) = actual {
                if let Some((ret_sig, in_sigs)) = args.split_last() {
                    for (a, p) in in_sigs.iter().zip(fsig.params.iter()) {
                        unify_gsig(a, *p, binds);
                    }
                    unify_gsig(ret_sig, fsig.ret, binds);
                }
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

/// If `sig` is a `kotlin/jvm/functions/FunctionN` type, the (substituted) types of its lambda
/// parameters — its first N type arguments (the last is the return type). Empty for anything else.
pub(crate) fn function_input_types(
    sig: &GSig,
    binds: &std::collections::HashMap<String, Ty>,
) -> Vec<Ty> {
    if let GSig::Class(internal, targs) = sig {
        if internal.starts_with("kotlin/jvm/functions/Function") && !targs.is_empty() {
            // A `FunctionN` is generic, so a primitive-typed lambda parameter appears boxed in the
            // signature (`(index: Int, …)` → `Function2<Integer, …>`). The Kotlin lambda parameter is
            // the *unboxed* primitive, so map a known wrapper type argument back to it.
            return targs[..targs.len() - 1]
                .iter()
                .map(|a| unbox_wrapper(gsig_to_ty(a, binds)))
                .collect();
        }
    }
    Vec::new()
}

/// Map a JVM boxed-primitive wrapper type back to its primitive (`java/lang/Integer` → `Int`); a no-op
/// for any other type. Recovers unboxed Kotlin lambda-parameter types from an erased `FunctionN`
/// signature (whose type arguments are always boxed).
pub(crate) fn unbox_wrapper(t: Ty) -> Ty {
    match t.obj_internal() {
        Some("java/lang/Integer") => Ty::Int,
        Some("java/lang/Long") => Ty::Long,
        Some("java/lang/Short") => Ty::Short,
        Some("java/lang/Byte") => Ty::Byte,
        Some("java/lang/Character") => Ty::Char,
        Some("java/lang/Boolean") => Ty::Boolean,
        Some("java/lang/Double") => Ty::Double,
        Some("java/lang/Float") => Ty::Float,
        _ => t,
    }
}

/// Whether argument `a` can be passed where parameter `p` is expected, in erased Kotlin terms: an
/// exact match, any argument into an erased `Any` parameter, or the *same erased class* (a parameter
/// `Pair` accepts an argument `Pair<Int, String>` — generic parameters erase to the raw type).
pub(crate) fn arg_fits(p: &Ty, a: &Ty) -> bool {
    if p == a || *p == Ty::obj("kotlin/Any") {
        return true;
    }
    // A lambda (`Ty::Fun`) is passed where a `kotlin/jvm/functions/FunctionN` is expected.
    if let (Ty::Obj(pi, _), Ty::Fun(_)) = (p, a) {
        return pi.starts_with("kotlin/jvm/functions/Function");
    }
    // A property reference (`C::n` → `KProperty1`, `obj::n` → `KProperty0`) is itself a function:
    // `PropertyReference{1,0}Impl` implements the matching `FunctionN` (`invoke = get`). Accept it for
    // a `FunctionN` parameter of the matching arity (`Function1` ← `KProperty1`, `Function0` ← `KProperty0`).
    if let (Ty::Obj(pi, _), Ty::Obj(ai, _)) = (p, a) {
        if let Some(arity) = pi
            .strip_prefix("kotlin/jvm/functions/Function")
            .and_then(|n| n.parse::<usize>().ok())
        {
            let prop_arity = match *ai {
                "kotlin/reflect/KProperty1" | "kotlin/reflect/KMutableProperty1" => Some(1),
                "kotlin/reflect/KProperty0" | "kotlin/reflect/KMutableProperty0" => Some(0),
                _ => None,
            };
            if prop_arity == Some(arity) {
                return true;
            }
        }
    }
    matches!((p, a), (Ty::Obj(pi, _), Ty::Obj(ai, _)) if pi == ai)
}

/// The arg-dependent binding layer over a [`LibrarySet`]: it selects overloads and binds generics for
/// a specific call site. Holds the oracle by reference — cheap to construct per query.
pub struct CallResolver<'a> {
    lib: &'a dyn LibrarySet,
}

impl<'a> CallResolver<'a> {
    pub fn new(lib: &'a dyn LibrarySet) -> Self {
        CallResolver { lib }
    }

    /// Whether `name` has an `inline` extension overload on `receiver`.
    pub fn extension_is_inline(&self, receiver: Ty, name: &str) -> bool {
        self.lib
            .functions(name, Some(receiver))
            .overloads
            .iter()
            .any(|o| o.kind == FnKind::Extension && o.flags.inline)
    }

    /// Whether `name` has an `inline` top-level overload.
    pub fn toplevel_is_inline(&self, name: &str) -> bool {
        self.lib
            .functions(name, None)
            .overloads
            .iter()
            .any(|o| o.flags.inline)
    }

    /// Whether `name` has a top-level overload that MUST be inlined (`@InlineOnly`, no callable method).
    pub fn toplevel_has_must_inline(&self, name: &str) -> bool {
        self.lib
            .functions(name, None)
            .overloads
            .iter()
            .any(|o| o.flags.inline_only)
    }

    /// Resolve a single-selector `@OverloadResolutionByLambdaReturnType` call (`sumOf { … }`): pick the
    /// overload on `receiver` whose return type equals the lambda's return type. The candidate set (with
    /// its per-overload disambiguation) comes entirely from the one `functions` query.
    pub fn resolve_lambda_return_overload(
        &self,
        receiver: Ty,
        name: &str,
        lambda_ret: Ty,
        arg_tys: &[Ty],
    ) -> Option<LibraryCallable> {
        if arg_tys.len() != 1 {
            return None;
        }
        self.lib
            .functions(name, Some(receiver))
            .overloads
            .into_iter()
            .find(|o| o.callable.ret == lambda_ret)
            .map(|o| o.callable)
    }
}

// --- Navigation helpers (member/constructor resolution expressed purely against the trait) --------
// The inherited-member walk over a library type's hierarchy — arg-dependent binding, so it lives in
// this layer (not the oracle). `resolve` and `ir_lower` share one implementation, backend-agnostic.

/// Resolve a constructor on a library type by argument types (with the type's own widening).
pub fn resolve_constructor(
    lib: &dyn LibrarySet,
    internal: &str,
    args: &[Ty],
) -> Option<LibraryMember> {
    lib.resolve_type(internal)?.ctor(args).cloned()
}

/// Resolve a companion member `Type.name(args)` (the receiver type must be public).
pub fn resolve_companion(
    lib: &dyn LibrarySet,
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
/// member may be inherited from a (possibly non-public) supertype, so walk the chain breadth-first.
pub fn resolve_instance(
    lib: &dyn LibrarySet,
    internal: &str,
    name: &str,
    args: &[Ty],
) -> Option<LibraryMember> {
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
    let mut seen = std::collections::HashSet::new();
    let mut q = std::collections::VecDeque::new();
    q.push_back(internal.to_string());
    while let Some(cur) = q.pop_front() {
        if !seen.insert(cur.clone()) {
            continue;
        }
        let Some(t) = lib.resolve_type(&cur) else {
            continue;
        };
        if let Some(m) = t
            .instance_member(name, args)
            .or_else(|| t.instance_member(name, &widened))
        {
            return Some(m.clone());
        }
        q.extend(t.supertypes);
    }
    None
}
