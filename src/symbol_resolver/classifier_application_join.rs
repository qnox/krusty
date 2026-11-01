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
//! - Writable (in) arguments meet: the argument that is a subtype of all the others, else the empty
//!   intersection `Nothing`. A built-in scalar shares no denotable intersection with another
//!   argument, so `Comparable<UInt>` and `Comparable<ULong>` join at `Comparable<*>`.
//! - Mixed in and out projections join at the star.
//!
//! The element join is the caller's: it is the same common-supertype operation one level down,
//! with that caller's recursion guard. When it declines, the argument falls back to an
//! out-projection of the parameter's star bound.

use crate::assignable::TypeOracle;
use crate::symbol_source::SymbolSource;
use crate::types::{Ty, TyKind, TypeName, TypeVariance};

use super::{ty_subst_keep_unbound, GSigBinds};

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
        )?;
        let formal = &classifier.type_params()[index];
        for (bindings, argument) in formals.iter_mut().zip(&arguments) {
            bindings.insert(
                formal.clone(),
                argument.projection_inner().unwrap_or(*argument),
            );
        }
        joined_arguments.push(joined);
    }
    Ok(Ty::obj_args_name(owner, &joined_arguments))
}

/// Join one parameter's arguments. `bounds` is the parameter's declared bound as each application
/// substitutes it; their join, the star bound, is computed only when the result needs it.
fn join_arguments(
    oracle: &dyn TypeOracle,
    declaration_variance: TypeVariance,
    arguments: &[Ty],
    bounds: &[Ty],
    element_join: &mut dyn FnMut(&[Ty]) -> Option<Ty>,
) -> Result<Ty, ClassifierJoinFailure> {
    let star_bound = |element_join: &mut dyn FnMut(&[Ty]) -> Option<Ty>| {
        element_join(bounds).ok_or(ClassifierJoinFailure::Declined)
    };
    let Some((&first, rest)) = arguments.split_first() else {
        return Err(ClassifierJoinFailure::UnknownShape);
    };
    if rest.iter().all(|argument| *argument == first) {
        return Ok(first);
    }
    if let Some(platform) = platform_flexible_join(arguments) {
        return Ok(platform);
    }
    if arguments
        .iter()
        .any(|argument| argument.is_star_projection())
    {
        return Ok(Ty::star_projection(star_bound(element_join)?));
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
        return Ok(match (declaration_variance, element_join(&inners)) {
            (TypeVariance::Out, Some(joined)) => joined,
            (_, Some(joined)) => Ty::out_projection(joined),
            (_, None) => Ty::out_projection(star_bound(element_join)?),
        });
    }
    if declaration_variance != TypeVariance::Out && writable {
        let context = crate::assignable::TyCtx::new();
        let meet = inners
            .iter()
            .copied()
            .find(|candidate| {
                inners.iter().all(|other| {
                    crate::assignable::is_assignable(&context, oracle, *candidate, *other)
                })
            })
            .unwrap_or(Ty::Nothing);
        if meet == Ty::Nothing && inners.iter().copied().any(builtin_scalar) {
            return Ok(Ty::star_projection(star_bound(element_join)?));
        }
        return Ok(if declaration_variance == TypeVariance::In {
            meet
        } else {
            Ty::in_projection(meet)
        });
    }
    Ok(Ty::out_projection(star_bound(element_join)?))
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
    fn unrelated_contravariant_arguments_meet_at_nothing_and_scalars_at_the_star() {
        let bound = Ty::nullable(Ty::obj("kotlin/Any"));
        assert_eq!(
            join(
                TypeVariance::In,
                &[Ty::obj("sample/A"), Ty::obj("sample/B")],
                bound
            ),
            Ty::Nothing
        );
        assert_eq!(
            join(TypeVariance::In, &[Ty::Int, Ty::obj("sample/B")], bound),
            Ty::star_projection(bound)
        );
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
