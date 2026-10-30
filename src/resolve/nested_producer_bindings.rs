//! Projecting a nested generic producer's bindings into the enclosing call's constraint set.
//!
//! A postponed argument such as `emptyList()` in `eq(list, emptyList())` is itself a generic call.
//! Its result is one more lower bound for the outer variable, not an equation: when another
//! argument already supplied `MutableList<String>` for `T`, the producer's `List<E>` reaches `T`
//! through the `List` supertype face of that evidence, and `T` becomes the common supertype of both
//! bounds. The nested declaration's own upper bounds still constrain what the outer variable may be.

use super::*;
use crate::libraries::GenericSig;
use crate::types::TyKind;

/// The outer formal `declared` names when it is a bare variable of the call that no explicit type
/// argument fixes. Its current binding is then lower-bound evidence the other arguments supplied.
pub(super) fn open_variable<'a>(
    signature: &'a GenericSig,
    type_args: &[Ty],
    declared: Ty,
) -> Option<&'a str> {
    let TyKind::TyParam(name, _) = declared.non_null().kind() else {
        return None;
    };
    let index = signature
        .formals
        .iter()
        .position(|formal| *formal == name)?;
    let fixed = type_args
        .get(index)
        .is_some_and(|argument| *argument != Ty::Error);
    (!fixed).then(|| signature.formals[index].as_str())
}

/// Solve a nested producer passed for an open outer variable with lower bound `lower`.
///
/// When the expected result also bounds the variable from above, the producer's result must fit
/// under that `upper`, which is what solves `Unimpl<X>` in `listOf(real, Unimpl())` expected as
/// `List<Provider<Builder>>`: `X = Builder`. Otherwise the producer reaches the variable through
/// the supertype face of the existing lower bound, as `List<E>` reaches `MutableList<String>`.
pub(super) fn open_variable_bindings(
    source: &dyn crate::symbol_source::SymbolSource,
    generic_sig: &GenericSig,
    lower: Ty,
    upper: Option<Ty>,
    mut admits: impl FnMut(Ty, Ty) -> bool,
) -> Option<crate::symbol_resolver::GSigBinds> {
    let solves_result = |bindings: &crate::symbol_resolver::GSigBinds| {
        generic_sig.formals.iter().all(|formal| {
            bindings.contains_key(formal)
                || !crate::types::ty_mentions_param(generic_sig.ret, std::slice::from_ref(formal))
        })
    };
    upper
        .and_then(|upper| {
            crate::symbol_resolver::infer_generic_return_bindings_from_symbols(
                source,
                generic_sig,
                upper,
                &mut admits,
            )
        })
        .filter(solves_result)
        .or_else(|| {
            crate::symbol_resolver::infer_generic_return_bindings_from_lower_face(
                source,
                generic_sig,
                lower,
                &mut admits,
            )
        })
}

/// Join a nested producer's lower bounds into the outer call's existing bindings. Each outer
/// variable takes the common supertype of what the other arguments and the producer require; a
/// joined formal leaves `nested`, so a later merge cannot widen the join again.
///
/// A binding that came from the extension receiver is a lower bound only where the formal occurs
/// covariantly in the declared receiver. Through `Sink<in T>` it is an upper bound, and through an
/// invariant position an equation; those formals stay in `nested` for the receiver-aware merge.
pub(super) fn join_lower_bounds(
    source: &dyn crate::symbol_source::SymbolSource,
    signature: &GenericSig,
    receiver_bindings: &crate::symbol_resolver::GSigBinds,
    bindings: &mut crate::symbol_resolver::GSigBinds,
    nested: &mut crate::symbol_resolver::GSigBinds,
) {
    let receiver_lower_bound = |formal: &str| {
        !receiver_bindings.contains_key(formal)
            || signature.receiver.is_some_and(|receiver| {
                crate::symbol_resolver::formal_variance_in_type(source, receiver, formal)
                    == Some(crate::types::TypeVariance::Out)
            })
    };
    nested.retain(|formal, actual| {
        if !receiver_lower_bound(formal) {
            return true;
        }
        let Some(current) = bindings.get_mut(formal) else {
            return true;
        };
        *current =
            crate::symbol_resolver::merge_inferred_ty_from_symbols(Some(source), *current, *actual);
        false
    });
}

/// The expectation a postponed producer is solved against at outer parameter `declared`. An open outer variable whose
/// declared bound depends on another, already bound variable (`S : T?` after `T = String`) is not
/// a rigid type for the producer: the producer may only produce what that bound admits, so the
/// variable stands for its substituted bound (`String?`) and later receives the producer's result.
/// A bound that depends on no other variable (`<S : Any>`, or the implicit `Any?`) is a declared
/// bound only, and does not on its own justify an inference result.
pub(super) fn producer_expectation(
    signature: &GenericSig,
    type_args: &[Ty],
    bindings: &crate::symbol_resolver::GSigBinds,
    declared: Ty,
) -> Ty {
    let mut dependent = crate::symbol_resolver::GSigBinds::default();
    for (index, formal) in signature.formals.iter().enumerate() {
        if bindings.contains_key(formal)
            || type_args
                .get(index)
                .is_some_and(|argument| *argument != Ty::Error)
            || !crate::types::ty_mentions_param(declared, std::slice::from_ref(formal))
        {
            continue;
        }
        let [bound] = signature
            .formal_bounds
            .get(index)
            .map(Vec::as_slice)
            .unwrap_or_default()
        else {
            continue;
        };
        if !crate::types::ty_mentions_param(*bound, &signature.formals) {
            continue;
        }
        let bound = crate::symbol_resolver::ty_subst_keep_unbound(*bound, bindings);
        if !crate::types::ty_mentions_param(bound, &signature.formals) {
            dependent.insert(formal.clone(), bound);
        }
    }
    let substituted = crate::symbol_resolver::ty_subst_keep_unbound(
        crate::symbol_resolver::ty_subst_keep_unbound(declared, bindings),
        &dependent,
    );
    // A still-open bare variable offers the producer its own bound.
    match substituted.kind() {
        TyKind::TyParam(_, bound) => crate::symbol_resolver::ty_subst_keep_unbound(bound, bindings),
        _ => substituted,
    }
}

/// A symbolic outer expectation may bind the nested producer's formal directly to an outer call
/// variable. That relation is valid only when the OUTER variable's own upper bound satisfies the
/// nested declaration's bound. For `<Y : Any> produce(): Box<Y>` passed to `<X> consume(Box<X>)`,
/// `Y = X` is not a solution while `X` still has its default `Any?` bound; the producer contributes
/// its denotable `Any` bound and thereby constrains `X` to non-null. Enforce that declaration bound
/// before projecting the nested result into the outer call.
pub(super) fn enforce_declared_bounds(
    generic_sig: &GenericSig,
    nested_bindings: &mut crate::symbol_resolver::GSigBinds,
    mut admits: impl FnMut(Ty, Ty) -> bool,
) {
    for (index, formal) in generic_sig.formals.iter().enumerate() {
        let Some(actual) = nested_bindings.get(formal).copied() else {
            continue;
        };
        let Some(actual_bound) = actual.non_null().ty_param_bound() else {
            continue;
        };
        let Some(required) = generic_sig
            .formal_bounds
            .get(index)
            .and_then(|bounds| bounds.first())
            .copied()
            .filter(|bound| {
                !generic_sig.formals.iter().any(|nested| {
                    crate::types::ty_mentions_param(*bound, std::slice::from_ref(nested))
                })
            })
        else {
            continue;
        };
        if actual.is_nullable() || !admits(actual_bound, required) {
            nested_bindings.insert(formal.clone(), required);
        }
    }
}
