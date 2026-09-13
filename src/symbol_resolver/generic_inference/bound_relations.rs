//! Solving and checking a generic signature's DECLARATION BOUND relations.
//!
//! Call-site evidence (arguments, receiver, expected result) is the ordinary inference problem and
//! lives in the parent module. This module owns the separate relation a declaration states about
//! its own formals — `<T : Base<T>>`, `<R, T : R>`, `<T : X, X : Comparable<UInt>>` — which both
//! completes formals no argument reached and rejects bindings the declaration forbids.

use super::{
    merge_inferred_ty, ty_subst, ty_subst_keep_unbound, unify_ty_from_symbols,
    ExplicitTypeArgumentFixity, GSigBinds, GenericSig, SourceOracle, SymbolSource,
};
use crate::types::Ty;

/// Materialize a type argument that is observable only through a bound which another inferred
/// formal has made concrete. In `<T : KFunction<E>, E : Any> f(A<E>)`, the argument fixes `E`; `T`
/// has no independent input/result occurrence, so its unique solution is the now-concrete bound
/// `KFunction<E>`. This is different from an unconstrained `<T : Any> f()`: a static bound alone is
/// not call-site evidence and remains unbound.
pub(crate) fn complete_dependency_instantiated_bound_bindings(
    generic_sig: &GenericSig,
    bindings: &mut GSigBinds,
    explicit: impl ExplicitTypeArgumentFixity,
) {
    loop {
        let mut additions = Vec::new();
        for (index, (formal, bounds)) in generic_sig
            .formals
            .iter()
            .zip(&generic_sig.formal_bounds)
            .enumerate()
        {
            if explicit.fixes(index) || bindings.contains_key(formal) {
                continue;
            }
            let observable = generic_sig.receiver.is_some_and(|receiver| {
                crate::types::ty_mentions_param(receiver, std::slice::from_ref(formal))
            }) || generic_sig.params.iter().any(|parameter| {
                crate::types::ty_mentions_param(*parameter, std::slice::from_ref(formal))
            }) || crate::types::ty_mentions_param(
                generic_sig.ret,
                std::slice::from_ref(formal),
            );
            if observable {
                continue;
            }
            let [bound] = bounds.as_slice() else {
                continue;
            };
            let depends_on_inferred_formal = generic_sig.formals.iter().any(|dependency| {
                dependency != formal
                    && bindings.contains_key(dependency)
                    && crate::types::ty_mentions_param(*bound, std::slice::from_ref(dependency))
            });
            if !depends_on_inferred_formal {
                continue;
            }
            let solution = ty_subst_keep_unbound(*bound, bindings).projection_read_ty();
            if solution != Ty::Error
                && !solution.mentions_pending()
                && !solution.mentions_ty_param()
            {
                additions.push((formal.clone(), solution));
            }
        }
        if additions.is_empty() {
            break;
        }
        bindings.extend(additions);
    }
}

/// Complete a method type parameter whose only call-site information is the readable upper bound
/// of a star-captured receiver parameter.
///
/// `Entity<*>.value<T : S>(): T` is not the same as an unconstrained `value<T>(): T`: selecting the
/// member has already supplied the capture for the owner's `S`. Kotlin reads that capture through
/// its upper bound and approximates the recursive, non-denotable part back to a star before the
/// result enters a join or checked FIR. A source star retains that identity while carrying its
/// readable bound, so recursively approximating `S` produces a denotable `Entity<*>` rather than
/// inventing an explicit `out Any?` projection.
///
/// This deliberately applies only to a return-only method formal with one direct owner-formal bound.
/// Ordinary `<T : Any> value(): T`, input-constrained formals, and multi-bound intersections remain
/// unbound until the normal constraint system supplies evidence.
pub(crate) fn complete_return_only_captured_receiver_bindings(
    generic_sig: &GenericSig,
    receiver_bindings: &GSigBinds,
    bindings: &mut GSigBinds,
    explicit: impl ExplicitTypeArgumentFixity,
) {
    for (index, (formal, bounds)) in generic_sig
        .formals
        .iter()
        .zip(&generic_sig.formal_bounds)
        .enumerate()
    {
        if explicit.fixes(index)
            || bindings.contains_key(formal)
            || !crate::types::ty_mentions_param(generic_sig.ret, std::slice::from_ref(formal))
            || generic_sig.params.iter().any(|parameter| {
                crate::types::ty_mentions_param(*parameter, std::slice::from_ref(formal))
            })
        {
            continue;
        }
        let [bound] = bounds.as_slice() else {
            continue;
        };
        let star = Ty::star_projection(Ty::nullable(Ty::obj("kotlin/Any")));
        let mut recursive_approximation = GSigBinds::new();
        for (receiver_formal, captured) in receiver_bindings {
            if captured.projection_inner().is_some()
                && crate::types::ty_mentions_param(*bound, std::slice::from_ref(receiver_formal))
            {
                recursive_approximation.insert(receiver_formal.clone(), star);
            }
        }
        let captured_upper = match bound.non_null() {
            Ty::TyParam(receiver_formal, _) => receiver_bindings
                .get(receiver_formal)
                .copied()
                .filter(|captured| captured.projection_inner().is_some())
                .map(Ty::projection_read_ty),
            projected @ (Ty::InProjection(_) | Ty::OutProjection(_) | Ty::StarProjection(_)) => {
                Some(projected.projection_read_ty())
            }
            _ if !recursive_approximation.is_empty() => Some(*bound),
            _ => None,
        };
        let Some(captured_upper) = captured_upper else {
            continue;
        };
        let solution =
            ty_subst_keep_unbound(captured_upper, &recursive_approximation).projection_read_ty();
        let retains_method_formal = generic_sig
            .formals
            .iter()
            .any(|formal| crate::types::ty_mentions_param(solution, std::slice::from_ref(formal)));
        // A class or caller type parameter is a denotable part of the selected result and may cross
        // into checked FIR (`Entity<U, *>`). Only a still-unresolved formal owned by this method
        // would make the completion circular. The old blanket `mentions_ty_param` test rejected the
        // valid caller-owned `U` together with the captured recursive `S` that was already
        // approximated above.
        if solution != Ty::Error && !solution.mentions_pending() && !retains_method_formal {
            bindings.insert(formal.clone(), solution);
        }
    }
}

/// Materialize type arguments implied solely by a bound between declaration formals. Applicability
/// has always used this relation (`<T1 : C, T2 : T1>` plus `T2 = C` entails `T1 = C`); completing
/// the caller-owned map here ensures the selected call publishes the same solution to FIR.
pub(super) fn complete_dependent_bound_bindings(
    generic_sig: &GenericSig,
    bindings: &mut GSigBinds,
    explicit: impl ExplicitTypeArgumentFixity,
) {
    loop {
        let mut changed = false;
        for (formal, bounds) in generic_sig.formals.iter().zip(&generic_sig.formal_bounds) {
            let Some(actual) = bindings.get(formal).copied() else {
                continue;
            };
            for bound in bounds {
                let Ty::TyParam(bound_formal, _) = bound.non_null() else {
                    continue;
                };
                let Some(bound_index) = generic_sig
                    .formals
                    .iter()
                    .position(|candidate| candidate == bound_formal)
                else {
                    continue;
                };
                if !explicit.fixes(bound_index) && !bindings.contains_key(bound_formal) {
                    bindings.insert(bound_formal.to_string(), actual);
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
}

/// Complete a written underscore from its one concrete upper bound after stronger constraints have
/// run. An omitted type-argument list is deliberately unaffected.
fn complete_explicit_hole_bound_bindings(
    generic_sig: &GenericSig,
    bindings: &mut GSigBinds,
    explicit: impl ExplicitTypeArgumentFixity,
) {
    loop {
        let mut additions = Vec::new();
        for (index, (formal, bounds)) in generic_sig
            .formals
            .iter()
            .zip(&generic_sig.formal_bounds)
            .enumerate()
        {
            if !explicit.inferred_hole(index) || bindings.contains_key(formal) {
                continue;
            }
            let [bound] = bounds.as_slice() else {
                continue;
            };
            let solution = ty_subst_keep_unbound(*bound, bindings);
            if solution != Ty::Error && !solution.mentions_ty_param() {
                additions.push((formal.clone(), solution));
            }
        }
        if additions.is_empty() {
            break;
        }
        bindings.extend(additions);
    }
}

/// Solve a formal from the declaration's own bound relation: one that no argument reached, and one
/// whose argument-derived binding its OWN bound forbids.
///
/// An argument can pin a type variable to something its declared bound rules out. For
/// `fun <T : Base<T>, C : T> f(self: C, subs: Iterable<T>)` called as `f(Auth(), listOf(Login()))`,
/// the element type pins `T = Login` — but `Login` is a `Base<Cmd>`, not a `Base<Login>`, so that
/// binding cannot be what the call means. Kotlin solves `T` from the same recursive bound: the
/// application of `Base` in `Login`'s hierarchy is `Base<Cmd>`, so `T = Cmd`. Keeping the violating
/// binding instead makes the parameter `Iterable<Login>` and the call is reported inapplicable.
///
/// A concrete binding may also expose another formal through an applied generic supertype:
/// `<P, C : Component<P, *>>` with `C = MyComponent<X>` entails `P = MyProps<X>` when that is the
/// `Component` supertype of `MyComponent<X>`. Only unbound, non-explicit formals are filled through
/// this relation. A caller with no symbol source — or a bound the walk cannot reach — leaves every
/// binding alone.
pub(crate) fn resolve_bound_violating_bindings(
    source: Option<&dyn SymbolSource>,
    generic_sig: &GenericSig,
    bindings: &mut GSigBinds,
    explicit: impl ExplicitTypeArgumentFixity,
) {
    let Some(source) = source else {
        complete_explicit_hole_bound_bindings(generic_sig, bindings, explicit);
        return;
    };
    let oracle = SourceOracle(source);
    // Apply each concrete formal binding to its declaration bounds, then unify the actual applied
    // supertype with the still-symbolic bound. This is ordinary constraint propagation through the
    // class graph, not subtype guessing: the provider supplies the exact applied supertype and the
    // normal unifier extracts only declaration-owned variables from matching positions.
    loop {
        let mut additions = GSigBinds::new();
        for (formal, bounds) in generic_sig.formals.iter().zip(&generic_sig.formal_bounds) {
            let Some(actual) = bindings.get(formal).copied() else {
                continue;
            };
            for bound in bounds {
                let Some(target) = bound.kotlin_class_internal() else {
                    continue;
                };
                let Some(applied) =
                    crate::assignable::applied_supertype(&oracle, actual, Ty::obj_name(target))
                else {
                    continue;
                };
                let mut inferred = GSigBinds::new();
                unify_ty_from_symbols(source, *bound, applied, &mut inferred);
                for (candidate, solution) in inferred {
                    let Some(index) = generic_sig
                        .formals
                        .iter()
                        .position(|declared| declared == &candidate)
                    else {
                        continue;
                    };
                    if candidate == *formal
                        || explicit.fixes(index)
                        || bindings.contains_key(&candidate)
                        || solution == Ty::Error
                        || solution.mentions_ty_param()
                    {
                        continue;
                    }
                    additions
                        .entry(candidate)
                        .and_modify(|known| {
                            *known = merge_inferred_ty(Some(*known), solution);
                        })
                        .or_insert(solution);
                }
            }
        }
        if additions.is_empty() {
            break;
        }
        bindings.extend(additions);
    }
    // A written `_` is an explicit request to infer this position. After argument and applied-bound
    // propagation have had priority, a single concrete declared upper bound is the remaining
    // solution Kotlin publishes (`<P, C : Component<P, *>>` with explicit `P` and `C = _`). Do not
    // apply this to an omitted type-argument list: a wholly unconstrained ordinary call still
    // receives its normal not-enough-information treatment.
    complete_explicit_hole_bound_bindings(generic_sig, bindings, explicit);
    // A formal can be reachable ONLY through another's bound: `<T : Base<T>, C : T>` mentions `T` in
    // no parameter position of `f(self: C, subs: Array<out T>)` once the argument is a vararg
    // element. `C`'s value answers it through the same recursive bound.
    for (index, bounds) in generic_sig.formal_bounds.iter().enumerate() {
        if explicit.fixes(index) {
            continue;
        }
        let Some(actual) = generic_sig
            .formals
            .get(index)
            .and_then(|formal| bindings.get(formal))
            .copied()
        else {
            continue;
        };
        for bound in bounds {
            let Ty::TyParam(open, _) = bound.non_null() else {
                continue;
            };
            let Some(open_index) = generic_sig
                .formals
                .iter()
                .position(|candidate| candidate == open)
            else {
                continue;
            };
            if explicit.fixes(open_index) || bindings.contains_key(open) {
                continue;
            }
            let Some(open_bounds) = generic_sig.formal_bounds.get(open_index) else {
                continue;
            };
            for open_bound in open_bounds {
                let Some(position) = open_bound.type_args().iter().position(
                    |argument| matches!(argument.non_null(), Ty::TyParam(name, _) if name == open),
                ) else {
                    continue;
                };
                let Some(target) = open_bound.kotlin_class_internal() else {
                    continue;
                };
                let Some(applied) =
                    crate::assignable::applied_supertype(&oracle, actual, Ty::obj_name(target))
                else {
                    continue;
                };
                let Some(&solution) = applied.type_args().get(position) else {
                    continue;
                };
                if solution != Ty::Error && !solution.mentions_ty_param() {
                    bindings.insert(open.to_string(), solution);
                    break;
                }
            }
        }
    }
    for (index, (formal, bounds)) in generic_sig
        .formals
        .iter()
        .zip(&generic_sig.formal_bounds)
        .enumerate()
    {
        if explicit.fixes(index) {
            continue;
        }
        let Some(actual) = bindings.get(formal).copied() else {
            continue;
        };
        for bound in bounds {
            // Only a bound that mentions the formal ITSELF can be re-solved this way: it is what
            // ties the variable to a position in its own hierarchy.
            let Some(position) = bound.type_args().iter().position(
                |argument| matches!(argument.non_null(), Ty::TyParam(name, _) if name == formal),
            ) else {
                continue;
            };
            let applied_bound = ty_subst_keep_unbound(*bound, bindings);
            if crate::assignable::is_assignable(
                &crate::assignable::TyCtx::new(),
                &oracle,
                actual,
                applied_bound,
            ) {
                continue;
            }
            let Some(target) = bound.kotlin_class_internal() else {
                continue;
            };
            let Some(applied) =
                crate::assignable::applied_supertype(&oracle, actual, Ty::obj_name(target))
            else {
                continue;
            };
            let Some(&solution) = applied.type_args().get(position) else {
                continue;
            };
            if solution != Ty::Error && !solution.mentions_ty_param() && solution != actual {
                bindings.insert(formal.clone(), solution);
                break;
            }
        }
    }
}

pub(crate) fn generic_bindings_satisfy_bounds(
    generic_sig: &GenericSig,
    bindings: &GSigBinds,
    mut admits: impl FnMut(Ty, Ty) -> bool,
) -> bool {
    // A bound can constrain another formal: `<T : X, X : Comparable<UInt>>`. When an argument binds
    // `T` but no argument mentions `X`, Kotlin infers the most specific `X` from that subtype
    // constraint. Complete those relationships before substituting/checking the bound graph; leaving
    // `X` open erases it to its first bound and incorrectly asks whether `UInt` is the raw
    // `Comparable` parameter type at the call site.
    let mut bindings = bindings.clone();
    complete_dependent_bound_bindings(generic_sig, &mut bindings, 0);
    generic_sig
        .formals
        .iter()
        .zip(&generic_sig.formal_bounds)
        .all(|(formal, bounds)| {
            let Some(actual) = bindings.get(formal).copied() else {
                return true;
            };
            let actual = actual.projection_inner().unwrap_or(actual);
            bounds
                .iter()
                .all(|bound| admits(actual, ty_subst(*bound, &bindings)))
        })
}

/// Validate final call bindings while preserving Kotlin's expected-result intersection rule.
/// Every ordinary bound must hold. The sole exception is a conflicting binding contributed by the
/// expected result for a return-only formal; that binding denotes `expected & declared bounds`, not
/// an unconstrained replacement of the declared bound.
pub(crate) fn generic_bindings_admit_expected_return_intersection(
    generic_sig: &GenericSig,
    bindings: &GSigBinds,
    expected_bindings: Option<&GSigBinds>,
    mut admits: impl FnMut(Ty, Ty) -> bool,
) -> bool {
    generic_sig
        .formals
        .iter()
        .zip(&generic_sig.formal_bounds)
        .all(|(formal, bounds)| {
            let Some(actual) = bindings.get(formal).copied() else {
                return true;
            };
            let actual = actual.projection_inner().unwrap_or(actual);
            bounds.iter().all(|bound| {
                let declared_bound = *bound;
                let bound = ty_subst(declared_bound, bindings);
                if admits(actual, bound) {
                    return true;
                }
                let formal_slice = std::slice::from_ref(formal);
                expected_bindings
                    .and_then(|expected| expected.get(formal))
                    .is_some_and(|expected| expected.non_null() == actual.non_null())
                    && !crate::types::ty_mentions_param(declared_bound, formal_slice)
                    && crate::types::ty_mentions_param(generic_sig.ret, formal_slice)
                    && generic_sig.receiver.is_none_or(|receiver| {
                        !crate::types::ty_mentions_param(receiver, formal_slice)
                    })
                    && generic_sig
                        .params
                        .iter()
                        .all(|parameter| !crate::types::ty_mentions_param(*parameter, formal_slice))
                    && actual.is_reference()
                    && bound.is_reference()
            })
        })
}
