//! Pure application of normalized classifier parameters, bounds and use-site projections.

use crate::types::{Ty, TyKind};

/// Whether an application carries a decoder-placeholder star whose bound only its declaration knows.
pub(crate) fn has_placeholder_star(arguments: &[Ty]) -> bool {
    arguments.iter().copied().any(is_placeholder_star)
}

fn is_placeholder_star(argument: Ty) -> bool {
    matches!(argument.kind(), TyKind::StarProjection(inner) if inner == metadata_star_bound())
}

/// The nested type a decoder gives a star it read without one.
fn metadata_star_bound() -> Ty {
    Ty::nullable(Ty::obj("kotlin/Any"))
}

/// The arguments of an application of `classifier`, with every decoder-placeholder star replaced by
/// the star of its declared bound.
///
/// Kotlin metadata and JVM signatures record STAR without a nested type. Its readable type comes
/// from the corresponding declaration bound, specialized by the complete application. Reconstruct
/// it where declaration and application meet, for the members and for the supertypes alike, so a
/// supertype never sees the placeholder and rebuilds it from its own, wider bound. This also
/// handles dependent/F-bounds: `Rec<R, out T : Rec<R, T>>` applied as `Rec<*, *>` gives the second
/// star the readable bound `Rec<*, *>`, rather than the decoder placeholder `Any?`.
pub(crate) fn application_arguments_with_star_bounds(
    formals: &crate::types::TypeParameters<Vec<Vec<Ty>>>,
    arguments: &[Ty],
) -> Vec<Ty> {
    if !has_placeholder_star(arguments) {
        return arguments.to_vec();
    }
    let raw_bindings = formals
        .type_params()
        .iter()
        .cloned()
        .zip(arguments.iter().copied())
        .collect::<crate::fx_hash::HashMap<_, _>>();
    arguments
        .iter()
        .copied()
        .enumerate()
        .map(|(index, argument)| {
            if !is_placeholder_star(argument) {
                return argument;
            }
            let upper_bound = formals
                .type_param_bounds()
                .get(index)
                .and_then(|bounds| bounds.first())
                .copied()
                .map(|bound| crate::types::ty_subst_keep_unbound(bound, &raw_bindings))
                .unwrap_or_else(metadata_star_bound);
            Ty::star_projection(upper_bound)
        })
        .collect()
}

pub(crate) fn classifier_bindings(
    classifier: &crate::libraries::LibraryType,
    receiver: Ty,
) -> crate::types::TyParamBindings {
    let arguments =
        application_arguments_with_star_bounds(&classifier.type_parameters, receiver.type_args());
    classifier
        .type_params
        .iter()
        .map(crate::types::TyParamName::from)
        .zip(
            arguments
                .iter()
                .copied()
                .enumerate()
                .map(|(index, argument)| {
                    // Declaration-site variance makes the MATCHING use-site projection redundant:
                    // `interface List<out E>` means `List<out X>` — and so `List<*>` — simply is
                    // `List<X>`, so members see the plain argument and `List<*>.indexOf` takes
                    // `Any?`. An invariant classifier keeps the projection, which is what makes
                    // `MutableList<*>.add` collapse to `Nothing` and stay prohibited.
                    match (
                        classifier.type_param_variances.get(index).copied(),
                        argument.kind(),
                    ) {
                        (
                            Some(crate::types::TypeVariance::Out),
                            TyKind::OutProjection(inner) | TyKind::StarProjection(inner),
                        ) => inner,
                        (Some(crate::types::TypeVariance::In), TyKind::InProjection(inner)) => {
                            inner
                        }
                        _ => argument,
                    }
                }),
        )
        .collect()
}

/// The declared upper bound carried by each classifier type-parameter occurrence.
///
/// JVM `Signature` parsing discovers a class's formal declarations separately from a member's type
/// uses. Until those two records are joined, a `TT;` use carries only the parser's temporary `Any`
/// placeholder. Substitution must consume the declaration bound instead: in particular, an unqualified
/// Java `T extends Object` is platform-nullable, while Kotlin `T & Any` intentionally carries a
/// non-null occurrence bound.
pub(crate) fn classifier_type_parameter_bounds(
    classifier: &crate::libraries::LibraryType,
) -> crate::types::TyParamBindings {
    classifier
        .type_params
        .iter()
        .enumerate()
        .map(|(index, formal)| {
            let bound = classifier
                .type_param_bounds()
                .get(index)
                .and_then(|bounds| bounds.first())
                .copied()
                .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")));
            (crate::types::TyParamName::from(formal), bound)
        })
        .collect()
}
