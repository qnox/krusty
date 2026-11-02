//! Declaration-side most-specific selection and the genericity tiebreaker.

use crate::symbol_source::SymbolSource;
use crate::types::{Ty, TypeName};

pub(super) fn most_specific_indices(
    parameter_shapes: &[&[Ty]],
    at_least_as_specific: impl Fn(usize, Ty, Ty) -> bool,
) -> Vec<usize> {
    parameter_shapes
        .iter()
        .enumerate()
        .filter_map(|(index, params)| {
            let dominated = parameter_shapes
                .iter()
                .enumerate()
                .any(|(other_index, other)| {
                    index != other_index
                        && other.len() == params.len()
                        && other.iter().zip(*params).enumerate().all(
                            |(position, (&left, &right))| {
                                at_least_as_specific(position, left, right)
                            },
                        )
                        && !params.iter().zip(*other).enumerate().all(
                            |(position, (&left, &right))| {
                                at_least_as_specific(position, left, right)
                            },
                        )
                });
            (!dominated).then_some(index)
        })
        .collect()
}

fn declaration_maxima(
    src: &dyn SymbolSource,
    receivers: &[Option<Ty>],
    parameter_shapes: &[&[Ty]],
    generic: &[bool],
) -> Vec<usize> {
    assert_eq!(
        (receivers.len(), parameter_shapes.len()),
        (generic.len(), generic.len()),
        "every declaration-specificity shape must carry its genericity fact"
    );
    let classifier_shapes = receivers
        .iter()
        .zip(parameter_shapes)
        .map(|(receiver, parameters)| classifier_shape(*receiver, parameters))
        .collect::<Vec<_>>();
    let borrowed = classifier_shapes
        .iter()
        .map(Vec::as_slice)
        .collect::<Vec<_>>();
    let mut maximal = most_specific_indices(&borrowed, |_, left, right| {
        classifier_at_least_as_specific(src, left, right)
    });
    if maximal.iter().any(|index| !generic[*index]) {
        maximal.retain(|index| !generic[*index]);
    }
    maximal
}

/// A declaration's receiver and value parameters as the classifiers specificity compares.
fn classifier_shape(receiver: Option<Ty>, parameters: &[Ty]) -> Vec<Ty> {
    receiver
        .iter()
        .chain(parameters.iter())
        .map(|parameter| {
            let parameter = parameter.non_null();
            parameter
                .kotlin_class_internal()
                .map_or(parameter, Ty::obj_name)
        })
        .collect()
}

fn classifier_at_least_as_specific(src: &dyn SymbolSource, left: Ty, right: Ty) -> bool {
    left == right || super::resolution_subtype(src, left, right)
}

/// Whether two declarations are each at least as specific as the other, the only relation in
/// which Kotlin's omitted-default and vararg discrimination may choose between them. Each side's
/// type parameters are open variables while the other side is checked against it, so
/// `(Int) -> T1` and `(Int) -> T2` are equally specific; type arguments are compared in full, so
/// `(Int) -> String` and `(Int) -> Int` are not.
pub(crate) fn declarations_equally_specific(
    src: &dyn SymbolSource,
    (left_receiver, left): (Option<Ty>, &[Ty]),
    (right_receiver, right): (Option<Ty>, &[Ty]),
) -> bool {
    let left = left_receiver
        .iter()
        .chain(left)
        .copied()
        .collect::<Vec<_>>();
    let right = right_receiver
        .iter()
        .chain(right)
        .copied()
        .collect::<Vec<_>>();
    let open = crate::fx_hash::HashMap::default();
    let at_least_as_specific = |specific: Ty, general: Ty| {
        specific == general
            || super::resolution_subtype(src, specific, crate::types::ty_subst(general, &open))
    };
    left.len() == right.len()
        && left
            .iter()
            .zip(&right)
            .all(|(&l, &r)| at_least_as_specific(l, r) && at_least_as_specific(r, l))
}

/// Retain declarations after semantic specificity, then apply non-genericity only as a tiebreaker.
/// Value shapes arrive aligned to source arguments; an extension's resolved declared receiver is a
/// separate leading position. Named/default/vararg mapping is never reconstructed here.
pub(crate) fn retain_most_specific_declarations<T>(
    src: &dyn SymbolSource,
    intersection_receiver: Option<Ty>,
    candidates: &mut Vec<T>,
    shape: impl Fn(&T) -> (Option<Ty>, &[Ty], bool, Option<TypeName>),
) {
    if candidates.len() < 2 {
        return;
    }
    let declarations = candidates.iter().map(&shape).collect::<Vec<_>>();
    let receivers = declarations
        .iter()
        .map(|(receiver, _, _, _)| *receiver)
        .collect::<Vec<_>>();
    let parameter_shapes = declarations
        .iter()
        .map(|(_, parameters, _, _)| *parameters)
        .collect::<Vec<_>>();
    let generic = declarations
        .iter()
        .map(|(_, _, generic, _)| *generic)
        .collect::<Vec<_>>();
    let retained = declaration_maxima(src, &receivers, &parameter_shapes, &generic)
        .into_iter()
        .collect::<crate::fx_hash::HashSet<_>>();
    let mut index = 0;
    candidates.retain(|_| {
        let keep = retained.contains(&index);
        index += 1;
        keep
    });
    retain_earliest_intersection_member(src, intersection_receiver, candidates, |candidate| {
        let (_, parameters, _, owner) = shape(candidate);
        owner.map(|owner| (owner, parameters.to_vec()))
    });
}

/// Several intersection components can contribute one member slot, including inherited
/// declarations with distinct default arguments. Specificity does not choose among them. The
/// canonical component order is the type's identity, so the earliest component whose hierarchy
/// declares the slot supplies the call, defaults included.
/// A real overload family — different parameter shapes, or two survivors on that earliest
/// component — stays intact for the ordinary ambiguity diagnostic.
fn retain_earliest_intersection_member<T>(
    src: &dyn SymbolSource,
    receiver: Option<Ty>,
    candidates: &mut Vec<T>,
    member: impl Fn(&T) -> Option<(TypeName, Vec<Ty>)>,
) {
    if candidates.len() < 2 {
        return;
    }
    let Some(parts) = receiver.and_then(super::hierarchy_projection::intersection_components)
    else {
        return;
    };
    let shapes = candidates.iter().map(&member).collect::<Vec<_>>();
    let Some(parameters) = shapes
        .first()
        .and_then(|shape| shape.as_ref().map(|(_, parameters)| parameters.clone()))
    else {
        return;
    };
    if shapes.iter().any(|shape| {
        shape
            .as_ref()
            .is_none_or(|(_, candidate_parameters)| candidate_parameters != &parameters)
    }) {
        return;
    }
    let owners = shapes
        .iter()
        .filter_map(|shape| shape.as_ref().map(|(owner, _)| *owner))
        .collect::<Vec<_>>();
    let Some(preferred) = parts.iter().find_map(|part| {
        let reachable = super::hierarchy_projection::receiver_hierarchy(src, *part)
            .into_iter()
            .filter_map(|(applied, depth)| {
                let owner = applied.kotlin_class_internal()?;
                owners.contains(&owner).then_some((owner, depth))
            })
            .collect::<Vec<_>>();
        let nearest = reachable.iter().map(|(_, depth)| *depth).min()?;
        Some(
            reachable
                .into_iter()
                .filter_map(|(owner, depth)| (depth == nearest).then_some(owner))
                .collect::<crate::fx_hash::HashSet<_>>(),
        )
    }) else {
        return;
    };
    let mut index = 0;
    candidates.retain(|_| {
        let keep = shapes[index]
            .as_ref()
            .is_some_and(|(owner, _)| preferred.contains(owner));
        index += 1;
        keep
    });
}
