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
        .map(|(receiver, parameters)| {
            receiver
                .iter()
                .chain(parameters.iter())
                .map(|parameter| {
                    let parameter = parameter.non_null();
                    parameter
                        .kotlin_class_internal()
                        .map_or(parameter, Ty::obj_name)
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let borrowed = classifier_shapes
        .iter()
        .map(Vec::as_slice)
        .collect::<Vec<_>>();
    let mut maximal = most_specific_indices(&borrowed, |_, left, right| {
        left == right || super::resolution_subtype(src, left, right)
    });
    if maximal.iter().any(|index| !generic[*index]) {
        maximal.retain(|index| !generic[*index]);
    }
    maximal
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
        .collect::<std::collections::HashSet<_>>();
    let mut index = 0;
    candidates.retain(|_| {
        let keep = retained.contains(&index);
        index += 1;
        keep
    });
    retain_earliest_intersection_member(intersection_receiver, candidates, |candidate| {
        let (_, parameters, _, owner) = shape(candidate);
        owner.map(|owner| (owner, parameters.to_vec()))
    });
}

/// Several intersection components can declare one member slot, including distinct default
/// arguments. Specificity does not choose among them. The canonical component order is the type's
/// identity, so the earliest component that declares the slot supplies the call, defaults included.
/// A real overload family — different parameter shapes, or two survivors on that earliest
/// component — stays intact for the ordinary ambiguity diagnostic.
fn retain_earliest_intersection_member<T>(
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
        let owner = part.kotlin_class_internal()?;
        owners.contains(&owner).then_some(owner)
    }) else {
        return;
    };
    let mut index = 0;
    candidates.retain(|_| {
        let keep = shapes[index]
            .as_ref()
            .is_some_and(|(owner, _)| *owner == preferred);
        index += 1;
        keep
    });
}
