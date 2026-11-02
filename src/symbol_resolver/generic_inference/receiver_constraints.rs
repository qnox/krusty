//! Binding a callable's formals from an extension receiver. A formal can occur more than once in
//! the declared receiver; each occurrence is one constraint at the variance the receiver reads it
//! at, so a formal every occurrence reads the same way is solved from all of them, not from the
//! first one the unifier meets.

use super::*;
use crate::types::TypeVariance;

/// Unify `declared` with the call's `receiver` into `bindings`, then solve each formal that the
/// receiver reaches more than once and only covariantly or only contravariantly from all of its
/// occurrences: `Duo<T, T>` (both `out`) on a `Duo<A, B>` joins its lower bounds at their common
/// supertype, and `Inputs<T, T>` (both `in`) on an `Inputs<Any, String>` meets its upper bounds at
/// `String`. A formal already bound before the receiver (an explicit type argument) keeps its
/// binding, and one with an invariant occurrence keeps the unifier's equality.
pub(crate) fn unify_receiver_from_symbols(
    source: &dyn SymbolSource,
    declared: Ty,
    receiver: Ty,
    bindings: &mut GSigBinds,
) {
    let bound_before = bindings.keys().cloned().collect::<Vec<_>>();
    unify_ty_from_symbols(source, declared, receiver, bindings);
    let mut occurrences = Vec::new();
    receiver_occurrence_constraints(
        source,
        declared,
        receiver,
        TypeVariance::Out,
        &mut occurrences,
    );
    let mut formals = Vec::<&String>::new();
    for (formal, _, _) in &occurrences {
        if !formals.contains(&formal) && !bound_before.contains(formal) {
            formals.push(formal);
        }
    }
    let mut solved = GSigBinds::default();
    for formal in formals {
        let constraints = occurrences
            .iter()
            .filter(|(occurrence, _, _)| occurrence == formal)
            .collect::<Vec<_>>();
        if constraints.len() < 2 {
            continue;
        }
        let types = constraints.iter().map(|(_, _, ty)| *ty).collect::<Vec<_>>();
        let reads = |variance| constraints.iter().all(|(_, read, _)| *read == variance);
        let solution = if reads(TypeVariance::Out) {
            merge_inferred_lower_bounds_from_symbols(source, &types)
        } else if reads(TypeVariance::In) {
            greatest_lower_bound(source, &types)
        } else {
            continue;
        };
        solved.insert(formal.clone(), solution);
    }
    bindings.extend(solved);
}

/// Collect one constraint per type-parameter occurrence in a declared receiver: the type the actual
/// receiver supplies at that position and the variance the receiver reads it at, from use-site
/// projections and declaration-site variance. An actual receiver of a subclass is read through its
/// applied supertype of the declared classifier.
fn receiver_occurrence_constraints(
    source: &dyn SymbolSource,
    declared: Ty,
    actual: Ty,
    variance: TypeVariance,
    out: &mut Vec<(String, TypeVariance, Ty)>,
) {
    match declared.non_null().kind() {
        TyKind::TyParam(name, _) => {
            let mut solution = GSigBinds::default();
            unify_ty_from_symbols(source, declared, actual, &mut solution);
            if let Some(&ty) = solution.get(name.as_str()) {
                out.push((name.to_string(), variance, ty));
            }
        }
        TyKind::Obj(owner, declared_args) if !declared_args.is_empty() => {
            let oracle = SourceOracle(source);
            let Some(applied) = crate::assignable::applied_supertype(
                &oracle,
                actual.non_null(),
                Ty::obj_name(owner),
            ) else {
                return;
            };
            let classifier = source.classifier(owner);
            for (index, (&declared_arg, &actual_arg)) in
                declared_args.iter().zip(applied.type_args()).enumerate()
            {
                let (declared_arg, own) = match declared_arg.kind() {
                    TyKind::InProjection(inner) => (inner, TypeVariance::In),
                    TyKind::OutProjection(inner) => (inner, TypeVariance::Out),
                    TyKind::StarProjection(_) => continue,
                    _ => (
                        declared_arg,
                        classifier
                            .as_ref()
                            .and_then(|classifier| {
                                classifier.type_param_variances.get(index).copied()
                            })
                            .unwrap_or(TypeVariance::Invariant),
                    ),
                };
                if matches!(actual_arg.kind(), TyKind::StarProjection(_)) {
                    continue;
                }
                let actual_arg = actual_arg.projection_inner().unwrap_or(actual_arg);
                let composed = match (variance, own) {
                    (TypeVariance::Invariant, _) | (_, TypeVariance::Invariant) => {
                        TypeVariance::Invariant
                    }
                    (TypeVariance::Out, own) => own,
                    (TypeVariance::In, TypeVariance::Out) => TypeVariance::In,
                    (TypeVariance::In, TypeVariance::In) => TypeVariance::Out,
                };
                receiver_occurrence_constraints(source, declared_arg, actual_arg, composed, out);
            }
        }
        _ => {}
    }
}

/// The most general type below every upper constraint: one of them when it is a subtype of all
/// the others, otherwise their intersection.
fn greatest_lower_bound(source: &dyn SymbolSource, upper: &[Ty]) -> Ty {
    upper
        .iter()
        .copied()
        .find(|&candidate| {
            upper
                .iter()
                .all(|&other| resolution_subtype(source, candidate, other))
        })
        .unwrap_or_else(|| {
            let mut parts = Vec::new();
            for &ty in upper {
                if !parts.contains(&ty) {
                    parts.push(ty);
                }
            }
            Ty::intersection(&parts)
        })
}
