//! The common supertype of several applications of one classifier (`C<A1, ...>`, ..., `C<An, ...>`).
//!
//! Every common-supertype join reaches this once it has chosen a shared classifier: the classifier
//! stays, and each type argument is joined by the declaration's variance, as kotlinc's common
//! supertype calculator does.
//!
//! - Equal arguments stay as they are.
//! - Arguments that differ only by one top-level platform wrapper keep the platform form: its
//!   flexibility carries the null-assertion obligation guarded positions depend on.
//! - A star is the unknown argument, and `C<*>` is the supertype of every `C<X>`, so any star makes
//!   the joined argument a star.
//! - Readable (out or invariant) arguments join covariantly: the joined argument is the arguments'
//!   common supertype, out-projected unless the parameter is declared `out`.
//! - Writable (in) arguments meet at the intersection of the most specific ones: `In<A>` and
//!   `In<AB>`, `AB : A`, give `In<AB>`; unrelated `In<A>` and `In<B>` give `In<A & B>`. A built-in
//!   scalar shares no denotable intersection with another argument, so `Comparable<UInt>` and
//!   `Comparable<ULong>` join at `Comparable<*>`.
//! - Mixed in and out projections join at the star.
//!
//! The element join is the caller's: it is the same common-supertype operation one level down,
//! with that caller's recursion guard. When it declines, the join is recursive (`Enum<A>` and
//! `Enum<B>` lead back to the join of `A` and `B`) and the argument is a star, as kotlinc cuts it:
//! two enum classes join at `Enum<*>`.

use crate::assignable::TypeOracle;
use crate::symbol_source::SymbolSource;
use crate::types::{Ty, TyKind, TypeName, TypeVariance};

use crate::symbol_resolver::{ty_subst_keep_unbound, GSigBinds};

/// Why a classifier's applications have no join.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ClassifierJoinFailure {
    /// The classifier's declaration is unknown, or its type parameters do not match the
    /// applications' arity: a missing declaration is not evidence of any variance.
    UnknownShape,
    /// The element join declined to give the star bound of a parameter.
    Declined,
}

/// Join `applications`, every one an application of `owner`.
pub(crate) fn join_classifier_applications(
    source: &dyn SymbolSource,
    oracle: &dyn TypeOracle,
    owner: TypeName,
    applications: &[Ty],
    element_join: &mut dyn FnMut(&[Ty]) -> Option<Ty>,
) -> Result<Ty, ClassifierJoinFailure> {
    let mut argument_lists = Vec::with_capacity(applications.len());
    for application in applications {
        match application.non_null().kind() {
            TyKind::Obj(name, arguments) if name == owner => argument_lists.push(arguments),
            _ => return Err(ClassifierJoinFailure::UnknownShape),
        }
    }
    let Some(first) = argument_lists.first() else {
        return Err(ClassifierJoinFailure::UnknownShape);
    };
    let arity = first.len();
    if argument_lists
        .iter()
        .any(|arguments| arguments.len() != arity)
    {
        return Err(ClassifierJoinFailure::UnknownShape);
    }
    if arity == 0 {
        return Ok(Ty::obj_name(owner));
    }
    let classifier = source
        .classifier(owner)
        .ok_or(ClassifierJoinFailure::UnknownShape)?;
    if classifier.type_params().len() != arity || classifier.type_param_variances().len() != arity {
        return Err(ClassifierJoinFailure::UnknownShape);
    }
    let mut formals = vec![GSigBinds::default(); argument_lists.len()];
    let mut joined_arguments = Vec::with_capacity(arity);
    for index in 0..arity {
        let arguments = argument_lists
            .iter()
            .map(|arguments| arguments[index])
            .collect::<Vec<_>>();
        let declared_bound = classifier
            .type_param_bounds()
            .get(index)
            .and_then(|bounds| bounds.first())
            .copied()
            .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")));
        let bounds = formals
            .iter()
            .map(|bindings| ty_subst_keep_unbound(declared_bound, bindings))
            .collect::<Vec<_>>();
        let joined = join_arguments(
            oracle,
            classifier.type_param_variances()[index],
            &arguments,
            &bounds,
            element_join,
        )?
        .unwrap_or_else(|| {
            // The join of these arguments is the one in progress: a recursive bound
            // (`E : Enum<E>`) leads `Enum<A>` and `Enum<B>` back to the join of `A` and `B`.
            // kotlinc cuts that recursion at a star, whose readable bound is the declared one
            // read through the arguments joined so far and a star for this and every later
            // parameter, exactly as a written `Enum<*>` reads.
            let unbounded_star = Ty::star_projection(Ty::nullable(Ty::obj("kotlin/Any")));
            let applied = classifier
                .type_params()
                .iter()
                .enumerate()
                .map(|(position, formal)| {
                    (
                        crate::types::TyParamName::new(formal),
                        joined_arguments
                            .get(position)
                            .copied()
                            .unwrap_or(unbounded_star),
                    )
                })
                .collect::<GSigBinds>();
            Ty::star_projection(crate::types::ty_subst_applied_arguments(
                declared_bound,
                &applied,
            ))
        });
        let formal = &classifier.type_params()[index];
        for (bindings, argument) in formals.iter_mut().zip(&arguments) {
            bindings.insert(
                crate::types::TyParamName::new(formal),
                argument.projection_inner().unwrap_or(*argument),
            );
        }
        joined_arguments.push(joined);
    }
    Ok(Ty::obj_args_name(owner, &joined_arguments))
}

/// Join one parameter's arguments. `bounds` is the parameter's declared bound as each application
/// substitutes it; their join, the star bound, is computed only when the result needs it. `None`
/// means the readable arguments' element join is already in progress, so the argument is a star
/// the caller bounds.
fn join_arguments(
    oracle: &dyn TypeOracle,
    declaration_variance: TypeVariance,
    arguments: &[Ty],
    bounds: &[Ty],
    element_join: &mut dyn FnMut(&[Ty]) -> Option<Ty>,
) -> Result<Option<Ty>, ClassifierJoinFailure> {
    let star_bound = |element_join: &mut dyn FnMut(&[Ty]) -> Option<Ty>| {
        element_join(bounds).ok_or(ClassifierJoinFailure::Declined)
    };
    let Some((&first, rest)) = arguments.split_first() else {
        return Err(ClassifierJoinFailure::UnknownShape);
    };
    if rest.iter().all(|argument| *argument == first) {
        return Ok(Some(first));
    }
    if let Some(platform) = platform_flexible_join(arguments) {
        return Ok(Some(platform));
    }
    if arguments
        .iter()
        .any(|argument| argument.is_star_projection())
    {
        return Ok(Some(Ty::star_projection(star_bound(element_join)?)));
    }
    let projection = |argument: Ty| match argument.kind() {
        TyKind::InProjection(inner) => (true, false, inner),
        TyKind::OutProjection(inner) | TyKind::StarProjection(inner) => (false, true, inner),
        _ => (true, true, argument),
    };
    let projected = arguments
        .iter()
        .copied()
        .map(projection)
        .collect::<Vec<_>>();
    let inners = projected
        .iter()
        .map(|(_, _, inner)| *inner)
        .collect::<Vec<_>>();
    let readable = projected.iter().all(|(_, out, _)| *out);
    let writable = projected.iter().all(|(in_, _, _)| *in_);
    if declaration_variance != TypeVariance::In && readable {
        return Ok(element_join(&inners).map(|joined| {
            if declaration_variance == TypeVariance::Out {
                joined
            } else {
                Ty::out_projection(joined)
            }
        }));
    }
    if declaration_variance != TypeVariance::Out && writable {
        // An argument with a distinct subtype among the others adds nothing to the meet.
        let context = crate::assignable::TyCtx::new();
        let subtype =
            |sub: Ty, sup: Ty| crate::assignable::is_assignable(&context, oracle, sub, sup);
        let mut most_specific = Vec::<Ty>::new();
        for (index, candidate) in inners.iter().copied().enumerate() {
            let refined = inners
                .iter()
                .copied()
                .enumerate()
                .any(|(other_index, other)| {
                    other_index != index
                        && subtype(other, candidate)
                        && (!subtype(candidate, other) || other_index < index)
                });
            if !refined && !most_specific.contains(&candidate) {
                most_specific.push(candidate);
            }
        }
        let meet = match most_specific.as_slice() {
            [single] => *single,
            parts => {
                if parts.iter().copied().any(builtin_scalar) {
                    return Ok(Some(Ty::star_projection(star_bound(element_join)?)));
                }
                Ty::intersection(parts)
            }
        };
        return Ok(Some(if declaration_variance == TypeVariance::In {
            meet
        } else {
            Ty::in_projection(meet)
        }));
    }
    Ok(Some(Ty::out_projection(star_bound(element_join)?)))
}

/// The platform form when the arguments differ only by one top-level platform wrapper.
///
/// This is intentionally exact. A nested platform occurrence belongs to the enclosing
/// classifier's own argument join; treating it as an outer flexible difference makes the result
/// depend on the argument order and can discard a nested null-assertion obligation.
fn platform_flexible_join(arguments: &[Ty]) -> Option<Ty> {
    let lower = |argument: Ty| match argument.kind() {
        TyKind::PlatformNullable(inner) => inner,
        _ => argument,
    };
    let base = lower(*arguments.first()?);
    (arguments.iter().all(|argument| lower(*argument) == base)
        && arguments
            .iter()
            .any(|argument| argument.is_platform_nullable()))
    .then(|| Ty::platform_nullable(base))
}

/// Non-null built-in scalars, which share no denotable intersection with another argument.
fn builtin_scalar(ty: Ty) -> bool {
    ty.is_unsigned()
        || matches!(
            ty,
            Ty::Byte
                | Ty::Short
                | Ty::Int
                | Ty::Long
                | Ty::Float
                | Ty::Double
                | Ty::Char
                | Ty::Boolean
        )
}

#[cfg(test)]
mod tests {
    use super::{join_arguments, platform_flexible_join};
    use crate::types::{Ty, TypeVariance};

    struct NoSubtypes;

    impl crate::assignable::TypeOracle for NoSubtypes {
        fn direct_supertypes(&self, _: Ty) -> Vec<Ty> {
            Vec::new()
        }
    }

    fn join(variance: TypeVariance, arguments: &[Ty], bound: Ty) -> Ty {
        let bounds = vec![bound; arguments.len()];
        join_arguments(&NoSubtypes, variance, arguments, &bounds, &mut |types| {
            types.first().copied()
        })
        .expect("joined argument")
        .expect("no join in progress")
    }

    #[test]
    fn an_exact_platform_difference_keeps_the_platform_form_in_every_order() {
        let plain = Ty::obj("sample/Resp");
        let platform = Ty::platform_nullable(plain);

        assert_eq!(platform_flexible_join(&[platform, plain]), Some(platform));
        assert_eq!(platform_flexible_join(&[plain, platform]), Some(platform));
        assert_eq!(
            platform_flexible_join(&[plain, platform, plain]),
            Some(platform)
        );
    }

    #[test]
    fn nested_platform_and_genuine_nullable_differences_are_not_platform_pairs() {
        let plain_argument = Ty::obj("kotlin/String");
        let plain = Ty::obj_args("sample/Box", &[plain_argument]);
        let nested_platform = Ty::obj_args("sample/Box", &[Ty::platform_nullable(plain_argument)]);

        assert_eq!(platform_flexible_join(&[plain, nested_platform]), None);
        assert_eq!(platform_flexible_join(&[nested_platform, plain]), None);
        assert_eq!(platform_flexible_join(&[Ty::nullable(plain), plain]), None);
    }

    #[test]
    fn a_star_joins_at_the_star_of_the_declared_bound_in_every_order() {
        let declared_bound = Ty::obj("sample/Top");
        let narrower_star = Ty::star_projection(Ty::obj("sample/Leaf"));
        let other_star = Ty::star_projection(Ty::obj("sample/OtherLeaf"));
        let ordinary = Ty::obj("sample/Value");
        let expected = Ty::star_projection(declared_bound);

        for arguments in [
            [narrower_star, ordinary],
            [ordinary, narrower_star],
            [narrower_star, other_star],
            [other_star, narrower_star],
        ] {
            assert_eq!(
                join(TypeVariance::Invariant, &arguments, declared_bound),
                expected
            );
        }
    }

    #[test]
    fn unrelated_contravariant_arguments_meet_at_their_intersection_and_scalars_at_the_star() {
        let bound = Ty::nullable(Ty::obj("kotlin/Any"));
        assert_eq!(
            join(
                TypeVariance::In,
                &[Ty::obj("sample/A"), Ty::obj("sample/B")],
                bound
            ),
            Ty::intersection(&[Ty::obj("sample/A"), Ty::obj("sample/B")])
        );
        assert_eq!(
            join(TypeVariance::In, &[Ty::Int, Ty::obj("sample/B")], bound),
            Ty::star_projection(bound)
        );
    }

    #[test]
    fn a_readable_join_already_in_progress_is_left_to_the_caller_as_a_star() {
        let arguments = [Ty::obj("sample/A"), Ty::obj("sample/B")];
        let bounds = vec![Ty::obj("sample/Top"); arguments.len()];
        for variance in [TypeVariance::Invariant, TypeVariance::Out] {
            assert_eq!(
                join_arguments(&NoSubtypes, variance, &arguments, &bounds, &mut |_| None),
                Ok(None)
            );
        }
    }

    #[test]
    fn mixed_in_and_out_projections_join_at_the_out_projected_bound() {
        let bound = Ty::obj("sample/Top");
        assert_eq!(
            join(
                TypeVariance::Invariant,
                &[
                    Ty::in_projection(Ty::obj("sample/A")),
                    Ty::out_projection(Ty::obj("sample/B"))
                ],
                bound
            ),
            Ty::out_projection(bound)
        );
    }
}
