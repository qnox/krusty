//! Generic return specialization for already-selected extension callables.

use super::*;

pub(super) fn bind_ext_ret(
    source: &dyn SymbolSource,
    gsig: &GenericSig,
    receiver: Ty,
    args: &[Ty],
    targs: &[Ty],
) -> Ty {
    bind_ext_ret_tracking(source, gsig, receiver, args, targs).0
}

/// [`bind_ext_ret`] plus the bindings it made. A caller that reports the result as a TYPE — rather
/// than emitting a call with it — needs to know whether the arguments actually determined the
/// variables: an unbound one silently specializes to its bound, and `Any` is indistinguishable from
/// a legitimately-inferred `Any` once the binding is discarded.
pub(super) fn bind_ext_ret_tracking(
    source: &dyn SymbolSource,
    gsig: &GenericSig,
    receiver: Ty,
    args: &[Ty],
    targs: &[Ty],
) -> (Ty, GSigBinds) {
    let mut binds = extension_receiver_bindings(gsig, receiver, targs);
    for (parameter, argument) in gsig.params.iter().zip(args.iter().copied()) {
        unify_ty(*parameter, argument, &mut binds);
    }
    finish_extension_return_bindings(source, gsig, binds, targs)
}

fn extension_receiver_bindings(gsig: &GenericSig, receiver: Ty, targs: &[Ty]) -> GSigBinds {
    let mut binds = seeded_gsig_binds(gsig, targs);
    if let Some(recv_sig) = gsig.receiver {
        unify_ty(recv_sig, receiver, &mut binds);
        // A selected extension on `Owner<T>` must preserve a caller-owned symbolic `T` in its
        // result just like a selected member does. General unification intentionally ignores the
        // identity pair so later concrete evidence can still solve it; at final return binding,
        // however, leaving it absent erases `T` to its `Any?` bound (`Map.Entry<K, V>.component1()`
        // inside a postponed `buildMap<K, V>` lambda then incorrectly returns `Any`).
        preserve_receiver_identity_bindings(recv_sig, receiver, &mut binds);
    }
    binds
}

fn finish_extension_return_bindings(
    source: &dyn SymbolSource,
    gsig: &GenericSig,
    mut binds: GSigBinds,
    targs: &[Ty],
) -> (Ty, GSigBinds) {
    complete_bottom_constraint_bindings(gsig, &mut binds, targs.len());
    // A projection is a generic-argument constraint, never an expression value. Consume it while
    // materializing the selected callable's output: `Iterable<out Range>.first()` returns `Range`,
    // not the invalid top-level type `out Range`.
    let ret = specialize_final_signature_output_type(source, gsig.ret, &binds);
    (ret, binds)
}

/// Whether the receiver and arguments PIN every type variable the signature declares to one
/// concrete type.
///
/// Presence in the binding map is not the question. A variable can be "bound" to ITSELF — a `T?`
/// receiver unified against a `T?` declaration self-binds — and it can be bound TWICE to types that
/// disagree (`fun <T> Src.pick(a: T, b: T)` called with a `String` and an `Int`), where the first
/// argument wins and the join is never taken. Both produce a confident-looking type that the full
/// checker will not agree with, and a caller reporting one as a property's inferred type makes the
/// compiler contradict itself. Emission is unaffected either way: an unbound variable erases to its
/// bound, which is exactly what the call site emits.
pub(super) fn extension_bindings_are_determinate(
    semantic: &GenericSig,
    receiver: Ty,
    args: &[Ty],
    binds: &GSigBinds,
) -> bool {
    // Deliberately STRUCTURAL: `unify_ty` without a source performs no hierarchy walk and no SAM
    // conversion, so a constraint that needs one (an `Iterable<T>` parameter answered by a `List<
    // String>`) simply does not bind here and the call reports nothing. That is the safe direction —
    // it costs an inferred type, never an incorrect one.
    let names_a_variable = |ty: Ty| crate::types::ty_mentions_any_param(ty);
    if semantic.formals.iter().any(|formal| {
        binds
            .get(formal.as_str())
            .is_none_or(|&bound| names_a_variable(bound))
    }) {
        return false;
    }
    // Unify each position on its own: a formal reached from two positions must reach the same type
    // from both, which the accumulated map cannot show once the first binding has taken.
    let mut settled: GSigBinds = GSigBinds::new();
    let positions = semantic
        .receiver
        .map(|shape| (shape, receiver))
        .into_iter()
        .chain(semantic.params.iter().copied().zip(args.iter().copied()));
    for (shape, actual) in positions {
        let mut one = GSigBinds::new();
        unify_ty(shape, actual, &mut one);
        for (formal, bound) in one {
            if let Some(&previous) = settled.get(formal.as_str()) {
                if previous != bound {
                    return false;
                }
            } else {
                settled.insert(formal, bound);
            }
        }
    }
    true
}

pub(super) fn specialized_extension_return(
    lib: &dyn SemanticPlatform,
    o: &FunctionInfo,
    inferred: Ty,
) -> Ty {
    let provider = o.ret.apply(o.callable.ret);
    let Some(signature) = o.generic_sig.as_ref() else {
        return o.ret.apply(inferred);
    };
    let inferred = signature.apply_return_policy(lib, inferred);
    let direct_method_return = signature
        .ret
        .non_null()
        .ty_param_name()
        .is_some_and(|name| signature.formals.iter().any(|formal| formal == name));
    if direct_method_return {
        inferred
    } else {
        merge_specialized_return(provider, inferred)
    }
}

pub(super) fn bind_defaulted_ext_ret(
    source: &dyn SymbolSource,
    o: &FunctionInfo,
    receiver: Ty,
    args: &[Ty],
    targs: &[Ty],
    trailing_lambda: bool,
) -> Ty {
    let semantic = o.semantic_signature();
    let mut binds = seeded_gsig_binds(&semantic, targs);
    if let Some(recv_sig) = semantic.receiver {
        unify_ty(recv_sig, receiver, &mut binds);
    }
    if trailing_lambda {
        let prefix = args.len().saturating_sub(1);
        for (index, (ps, a)) in semantic.params.iter().take(prefix).zip(args).enumerate() {
            if !o.call_sig.parameter_contributes_to_inference(index) {
                continue;
            }
            unify_ty(*ps, *a, &mut binds);
        }
        if let (Some(ls), Some(la)) = (semantic.params.last(), args.last()) {
            let index = semantic.params.len().saturating_sub(1);
            if o.call_sig.parameter_contributes_to_inference(index) {
                unify_ty(*ls, *la, &mut binds);
            }
        }
    } else {
        for (index, (ps, a)) in semantic.params.iter().zip(args).enumerate() {
            if !o.call_sig.parameter_contributes_to_inference(index) {
                continue;
            }
            unify_ty(*ps, *a, &mut binds);
        }
    }
    specialize_final_signature_output_type(source, semantic.ret, &binds)
}

pub(super) fn bind_defaulted_ext_ret_slots(
    source: &dyn SymbolSource,
    o: &FunctionInfo,
    receiver: Ty,
    slots: &[Option<Ty>],
    targs: &[Ty],
) -> Ty {
    let semantic = o.semantic_signature();
    let mut binds = seeded_gsig_binds(&semantic, targs);
    if let Some(recv_sig) = semantic.receiver {
        unify_ty(recv_sig, receiver, &mut binds);
    }
    for (index, (ps, slot)) in semantic.params.iter().zip(slots).enumerate() {
        if !o.call_sig.parameter_contributes_to_inference(index) {
            continue;
        }
        if let Some(arg) = slot {
            unify_ty(*ps, *arg, &mut binds);
        }
    }
    specialize_final_signature_output_type(source, semantic.ret, &binds)
}
