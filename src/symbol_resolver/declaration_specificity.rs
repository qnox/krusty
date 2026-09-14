//! Declaration-side most-specific selection and the genericity tiebreaker.

use crate::symbol_source::SymbolSource;
use crate::types::Ty;

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
    parameter_shapes: &[&[Ty]],
    generic: &[bool],
) -> Vec<usize> {
    assert_eq!(
        parameter_shapes.len(),
        generic.len(),
        "every declaration-specificity shape must carry its genericity fact"
    );
    let classifier_shapes = parameter_shapes
        .iter()
        .map(|parameters| {
            parameters
                .iter()
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
/// Shapes arrive aligned to source arguments, so named/default/vararg mapping is never reconstructed.
pub(crate) fn retain_most_specific_declarations<T>(
    src: &dyn SymbolSource,
    candidates: &mut Vec<T>,
    shape: impl Fn(&T) -> (&[Ty], bool),
) {
    if candidates.len() < 2 {
        return;
    }
    let declarations = candidates.iter().map(shape).collect::<Vec<_>>();
    let parameter_shapes = declarations
        .iter()
        .map(|(parameters, _)| *parameters)
        .collect::<Vec<_>>();
    let generic = declarations
        .iter()
        .map(|(_, generic)| *generic)
        .collect::<Vec<_>>();
    let retained = declaration_maxima(src, &parameter_shapes, &generic)
        .into_iter()
        .collect::<std::collections::HashSet<_>>();
    let mut index = 0;
    candidates.retain(|_| {
        let keep = retained.contains(&index);
        index += 1;
        keep
    });
}
