//! Canonical `A & B` types and the denotable declaration they publish.
//!
//! An intersection keeps its components in the order they were given, as kotlinc's does: a
//! common supertype lists them in the order the first operand's supertypes were found, and
//! diagnostics spell them in that order. The type relation compares intersections as sets (see
//! `assignable`), so `A & B` and `B & A` are equal types with two spellings. Nullability stays on
//! the intersection when every component admits null, so `Left? & Right?` is not the non-null
//! type `Left & Right`.

use crate::types::TyKind;

use super::{Ty, TypeName, TypeVariance};

struct Piece {
    core: Ty,
    nullable: bool,
}

/// `A & B & …`, its distinct components in their first order. A single component is that component. A non-null `Nothing` component, or a
/// nullable bottom beside a non-null component, is divergent `Nothing`. Every component
/// nullable yields `T?`, including `Nothing?` when a nullable bottom is present.
pub(super) fn canonical(parts: &[Ty]) -> Ty {
    let mut pieces = Vec::new();
    for part in parts.iter().copied() {
        decompose(part, false, &mut pieces);
    }
    if pieces.is_empty() {
        return Ty::Nothing;
    }
    if pieces
        .iter()
        .any(|piece| piece.core == Ty::Nothing && !piece.nullable)
    {
        return Ty::Nothing;
    }
    if pieces
        .iter()
        .any(|piece| piece.core == Ty::Nothing && piece.nullable)
    {
        return if pieces.iter().all(|piece| piece.nullable) {
            Ty::nullable(Ty::Nothing)
        } else {
            Ty::Nothing
        };
    }
    let all_nullable = pieces.iter().all(|piece| piece.nullable);
    let mut cores = Vec::new();
    for piece in pieces {
        if !cores.iter().any(|core| same_structure(*core, piece.core)) {
            cores.push(piece.core);
        }
    }
    match cores.as_slice() {
        [] => Ty::Nothing,
        [one] if all_nullable => Ty::nullable(*one),
        [one] => *one,
        many => {
            let intersection = Ty::from(TyKind::Intersection(super::IntersectionParts::new(
                crate::types::TyList::new(many),
                None,
            )));
            if all_nullable {
                Ty::nullable(intersection)
            } else {
                intersection
            }
        }
    }
}

/// Diagnostic spelling of a nullable type. A nullable intersection is `A? & B?`, not `(A & B)?`.
pub(super) fn spell_nullable(
    inner: Ty,
    render: impl Fn(Ty) -> String,
    parenthesize_fun: bool,
) -> String {
    let spell_one = |ty: Ty| {
        let rendered = render(ty);
        if parenthesize_fun && ty.is_fun() {
            format!("({rendered})?")
        } else {
            format!("{rendered}?")
        }
    };
    match inner.kind() {
        TyKind::Intersection(parts) => parts
            .iter()
            .copied()
            .map(spell_one)
            .collect::<Vec<_>>()
            .join(" & "),
        _ => spell_one(inner),
    }
}

fn decompose(ty: Ty, inherited_nullable: bool, out: &mut Vec<Piece>) {
    match ty.kind() {
        TyKind::Error => {}
        TyKind::Nullable(inner) => decompose(inner, true, out),
        TyKind::PlatformNullable(inner) => decompose(inner, true, out),
        TyKind::MutabilityFlexible(inner) => decompose(inner, inherited_nullable, out),
        TyKind::Intersection(parts) => {
            for part in parts.iter().copied() {
                decompose(part, inherited_nullable, out);
            }
        }
        TyKind::Null => out.push(Piece {
            core: Ty::Nothing,
            nullable: true,
        }),
        _ => out.push(Piece {
            core: ty,
            nullable: inherited_nullable,
        }),
    }
}

/// Whether two types have one structure, with every intersection inside compared as the set of
/// its components: `A & B` and `B & A` are one type in two spellings.
pub fn same_structure(left: Ty, right: Ty) -> bool {
    if left == right {
        return true;
    }
    let all_same = |left: &[Ty], right: &[Ty]| {
        left.len() == right.len() && left.iter().zip(right).all(|(l, r)| same_structure(*l, *r))
    };
    match (left.kind(), right.kind()) {
        (TyKind::Obj(left_name, left_args), TyKind::Obj(right_name, right_args)) => {
            left_name == right_name && all_same(&left_args, &right_args)
        }
        (TyKind::Fun(left), TyKind::Fun(right)) => {
            left.suspend == right.suspend
                && left.has_receiver == right.has_receiver
                && left.context_count == right.context_count
                && all_same(&left.params, &right.params)
                && same_structure(left.ret, right.ret)
        }
        (TyKind::Nullable(left), TyKind::Nullable(right))
        | (TyKind::PlatformNullable(left), TyKind::PlatformNullable(right))
        | (TyKind::InProjection(left), TyKind::InProjection(right))
        | (TyKind::OutProjection(left), TyKind::OutProjection(right))
        | (TyKind::StarProjection(left), TyKind::StarProjection(right))
        | (TyKind::RawProjection(left), TyKind::RawProjection(right))
        | (TyKind::DefinitelyNotNull(left), TyKind::DefinitelyNotNull(right))
        | (TyKind::MutabilityFlexible(left), TyKind::MutabilityFlexible(right)) => {
            same_structure(left, right)
        }
        (TyKind::TyParam(left_name, left_bound), TyKind::TyParam(right_name, right_bound))
        | (TyKind::Var(left_name, left_bound), TyKind::Var(right_name, right_bound)) => {
            left_name == right_name && same_structure(left_bound, right_bound)
        }
        (TyKind::Intersection(left), TyKind::Intersection(right)) => {
            same_component_set(&left, &right, same_structure)
        }
        _ => false,
    }
}

/// Whether two intersections have the same components: every component of each has an equal one
/// in the other, so neither side may match two of its components to one of the other's.
pub fn same_component_set(left: &[Ty], right: &[Ty], same: impl Fn(Ty, Ty) -> bool) -> bool {
    let covered = |from: &[Ty], to: &[Ty]| from.iter().all(|&a| to.iter().any(|&b| same(a, b)));
    left.len() == right.len() && covered(left, right) && covered(right, left)
}

/// The denotable type a public declaration publishes for an intersection.
///
/// A nullable intersection is `Any?`. Unrelated classifiers are `Any`. A shared classifier
/// keeps that classifier and approximates each argument from its variance: covariant arguments
/// join, invariant arguments become an out-projection of that join, and differing
/// contravariant arguments become a star. Function types keep a shared shape; a parameter that
/// differs across components is a star.
pub fn declaration_approximation(
    ty: Ty,
    variance_at: &mut dyn FnMut(TypeName, usize) -> Option<TypeVariance>,
) -> Option<Ty> {
    match ty.kind() {
        TyKind::Nullable(inner) if matches!(inner.kind(), TyKind::Intersection(_)) => {
            Some(Ty::nullable(Ty::obj_name(super::wk::any())))
        }
        TyKind::Intersection(parts) => approximate_parts(parts.as_slice(), variance_at),
        _ => Some(ty),
    }
}

fn approximate_parts(
    parts: &[Ty],
    variance_at: &mut dyn FnMut(TypeName, usize) -> Option<TypeVariance>,
) -> Option<Ty> {
    if parts.iter().all(|part| part.is_fun()) {
        return approximate_functions(parts, variance_at);
    }
    if shared_classifier(parts).is_some() {
        return approximate_objects(parts, variance_at);
    }
    Some(Ty::obj_name(super::wk::any()))
}

fn shared_classifier(parts: &[Ty]) -> Option<(TypeName, usize)> {
    let TyKind::Obj(name, arguments) = (parts.first().copied()?).kind() else {
        return None;
    };
    parts
        .iter()
        .all(|part| matches!(part.kind(), TyKind::Obj(other, args) if other == name && args.len() == arguments.len()))
        .then_some((name, arguments.len()))
}

fn approximate_objects(
    parts: &[Ty],
    variance_at: &mut dyn FnMut(TypeName, usize) -> Option<TypeVariance>,
) -> Option<Ty> {
    let Some((name, length)) = shared_classifier(parts) else {
        return Some(Ty::obj_name(super::wk::any()));
    };
    if length == 0 {
        return Some(Ty::obj_name(name));
    }
    let mut arguments = Vec::with_capacity(length);
    for index in 0..length {
        let at_index = parts
            .iter()
            .filter_map(|part| match part.kind() {
                TyKind::Obj(_, args) => args.get(index).copied(),
                _ => None,
            })
            .collect::<Vec<_>>();
        arguments.push(approximate_argument(name, index, &at_index, variance_at)?);
    }
    Some(Ty::obj_args_name(name, &arguments))
}

fn approximate_argument(
    classifier: TypeName,
    index: usize,
    arguments: &[Ty],
    variance_at: &mut dyn FnMut(TypeName, usize) -> Option<TypeVariance>,
) -> Option<Ty> {
    if arguments.is_empty() {
        return Some(Ty::obj_name(super::wk::any()));
    }
    if arguments
        .iter()
        .all(|argument| same_structure(*argument, arguments[0]))
    {
        return Some(arguments[0]);
    }
    match variance_at(classifier, index)? {
        TypeVariance::Out => covariant_join(arguments, variance_at),
        TypeVariance::Invariant => covariant_join(arguments, variance_at).map(Ty::out_projection),
        TypeVariance::In => Some(Ty::star_projection(Ty::nullable(Ty::obj_name(
            super::wk::any(),
        )))),
    }
}

fn approximate_functions(
    parts: &[Ty],
    variance_at: &mut dyn FnMut(TypeName, usize) -> Option<TypeVariance>,
) -> Option<Ty> {
    let signatures = parts
        .iter()
        .filter_map(|part| match part.kind() {
            TyKind::Fun(signature) => Some(signature),
            _ => None,
        })
        .collect::<Vec<_>>();
    let Some(first) = signatures.first().copied() else {
        return Some(Ty::obj_name(super::wk::any()));
    };
    if signatures.iter().any(|signature| {
        signature.params.len() != first.params.len()
            || signature.suspend != first.suspend
            || signature.has_receiver != first.has_receiver
            || signature.context_count != first.context_count
    }) {
        return Some(Ty::obj_name(super::wk::any()));
    }
    let mut params = Vec::with_capacity(first.params.len());
    for index in 0..first.params.len() {
        let at_index = signatures
            .iter()
            .map(|signature| signature.params[index])
            .collect::<Vec<_>>();
        if at_index
            .iter()
            .all(|parameter| same_structure(*parameter, at_index[0]))
        {
            params.push(at_index[0]);
        } else {
            params.push(Ty::star_projection(Ty::nullable(Ty::obj_name(
                super::wk::any(),
            ))));
        }
    }
    let returns = signatures
        .iter()
        .map(|signature| signature.ret)
        .collect::<Vec<_>>();
    Some(Ty::fun_with_shape(
        params,
        covariant_join(&returns, variance_at)?,
        first.context_count,
        first.has_receiver,
        first.suspend,
    ))
}

fn covariant_join(
    types: &[Ty],
    variance_at: &mut dyn FnMut(TypeName, usize) -> Option<TypeVariance>,
) -> Option<Ty> {
    let mut unique = Vec::new();
    for ty in types.iter().copied() {
        if !unique.iter().any(|seen| same_structure(*seen, ty)) {
            unique.push(ty);
        }
    }
    match unique.as_slice() {
        [] => Some(Ty::obj_name(super::wk::any())),
        [one] => Some(*one),
        many if shared_classifier(many).is_some() => approximate_objects(many, variance_at),
        many if many.iter().all(|ty| ty.is_fun()) => approximate_functions(many, variance_at),
        many if many.iter().all(|ty| ty.is_nullable()) => {
            let cores = many.iter().map(|ty| ty.non_null()).collect::<Vec<_>>();
            covariant_join(&cores, variance_at).map(Ty::nullable)
        }
        _ => Some(Ty::obj_name(super::wk::any())),
    }
}

#[cfg(test)]
mod tests {
    use super::{declaration_approximation, Ty, TypeVariance};
    use crate::types::TyKind;

    fn out_variance(_classifier: crate::types::TypeName, _index: usize) -> Option<TypeVariance> {
        Some(TypeVariance::Out)
    }

    fn invariant_variance(
        _classifier: crate::types::TypeName,
        _index: usize,
    ) -> Option<TypeVariance> {
        Some(TypeVariance::Invariant)
    }

    fn in_variance(_classifier: crate::types::TypeName, _index: usize) -> Option<TypeVariance> {
        Some(TypeVariance::In)
    }

    fn missing_variance(
        _classifier: crate::types::TypeName,
        _index: usize,
    ) -> Option<TypeVariance> {
        None
    }

    #[test]
    fn duplicates_collapse_and_the_first_order_stays() {
        let forward = Ty::intersection(&[Ty::Int, Ty::String, Ty::Int]);
        assert_eq!(forward, Ty::intersection(&[Ty::Int, Ty::String]));
        assert_ne!(forward, Ty::intersection(&[Ty::String, Ty::Int]));
        match forward.kind() {
            TyKind::Intersection(parts) => assert_eq!(parts.as_slice(), &[Ty::Int, Ty::String]),
            _ => {
                let other = forward;
                panic!("expected an intersection, got {other:?}")
            }
        }
    }

    #[test]
    fn components_that_differ_only_in_a_nested_order_are_one_component() {
        let a = Ty::obj("demo/A");
        let b = Ty::obj("demo/B");
        let first = Ty::obj_args("demo/I", &[Ty::intersection(&[a, b])]);
        let second = Ty::obj_args("demo/I", &[Ty::intersection(&[b, a])]);
        assert_eq!(Ty::intersection(&[first, second]), first);
    }

    #[test]
    fn a_component_set_needs_a_match_for_every_component_on_both_sides() {
        let same = |left: Ty, right: Ty| left == right;
        let (i, j, k) = (Ty::obj("demo/I"), Ty::obj("demo/J"), Ty::obj("demo/K"));
        assert!(super::same_component_set(&[i, j], &[j, i], same));
        assert!(!super::same_component_set(&[i, i], &[i, j], same));
        assert!(!super::same_component_set(&[i, j], &[i, i], same));
        assert!(!super::same_component_set(&[i, j], &[i, k], same));
    }

    #[test]
    fn same_classifier_with_distinct_arguments_stays_distinct() {
        let list_int = Ty::obj_args("kotlin/collections/List", &[Ty::Int]);
        let list_string = Ty::obj_args("kotlin/collections/List", &[Ty::String]);
        let intersection = Ty::intersection(&[list_string, list_int, list_string]);
        assert_eq!(intersection, Ty::intersection(&[list_string, list_int]));
        match intersection.kind() {
            TyKind::Intersection(parts) => {
                assert_eq!(parts.as_slice(), &[list_string, list_int]);
            }
            _ => {
                let other = intersection;
                panic!("expected both list instantiations, got {other:?}")
            }
        }
    }

    #[test]
    fn distinct_function_shapes_stay_distinct() {
        let forward = Ty::fun(vec![Ty::Int], Ty::String);
        let reverse = Ty::fun(vec![Ty::String], Ty::Int);
        let intersection = Ty::intersection(&[forward, reverse]);
        match intersection.kind() {
            TyKind::Intersection(parts) => assert_eq!(parts.as_slice(), &[forward, reverse]),
            _ => {
                let other = intersection;
                panic!("expected both function shapes, got {other:?}")
            }
        }
    }

    #[test]
    fn nullable_components_keep_a_nullable_intersection() {
        let left = Ty::nullable(Ty::obj("demo/Left"));
        let right = Ty::nullable(Ty::obj("demo/Right"));
        let intersection = Ty::intersection(&[left, right]);
        assert!(intersection.admits_null());
        assert_eq!(
            intersection,
            Ty::nullable(Ty::intersection(&[
                Ty::obj("demo/Left"),
                Ty::obj("demo/Right")
            ]))
        );
    }

    #[test]
    fn a_non_null_component_drops_intersection_nullability() {
        let intersection =
            Ty::intersection(&[Ty::nullable(Ty::obj("demo/Left")), Ty::obj("demo/Right")]);
        assert!(!intersection.admits_null());
        assert_eq!(
            intersection,
            Ty::intersection(&[Ty::obj("demo/Left"), Ty::obj("demo/Right")])
        );
    }

    #[test]
    fn nullable_bottom_beside_a_non_null_component_is_nothing() {
        let intersection = Ty::intersection(&[Ty::nullable(Ty::Nothing), Ty::obj("demo/Right")]);
        assert_eq!(intersection, Ty::Nothing);
        assert_eq!(
            Ty::intersection(&[Ty::Nothing, Ty::obj("demo/Right")]),
            Ty::Nothing
        );
    }

    #[test]
    fn nullable_bottom_beside_nullable_components_is_nullable_nothing() {
        let intersection = Ty::intersection(&[
            Ty::nullable(Ty::Nothing),
            Ty::nullable(Ty::obj("demo/Left")),
        ]);
        assert_eq!(intersection, Ty::nullable(Ty::Nothing));
    }

    #[test]
    fn a_nullable_intersection_publishes_nullable_any() {
        let intersection = Ty::intersection(&[
            Ty::nullable(Ty::obj("demo/Left")),
            Ty::nullable(Ty::obj("demo/Right")),
        ]);
        assert_eq!(
            declaration_approximation(intersection, &mut out_variance),
            Some(Ty::nullable(Ty::obj("kotlin/Any")))
        );
    }

    #[test]
    fn unrelated_classifiers_publish_any() {
        let intersection = Ty::intersection(&[Ty::obj("demo/Left"), Ty::obj("demo/Right")]);
        assert_eq!(
            declaration_approximation(intersection, &mut invariant_variance),
            Some(Ty::obj("kotlin/Any"))
        );
    }

    #[test]
    fn a_shared_classifier_approximates_arguments_by_variance() {
        let list_int = Ty::obj_args("kotlin/collections/List", &[Ty::Int]);
        let list_string = Ty::obj_args("kotlin/collections/List", &[Ty::String]);
        let lists = Ty::intersection(&[list_int, list_string]);
        assert_eq!(
            declaration_approximation(lists, &mut out_variance),
            Some(Ty::obj_args(
                "kotlin/collections/List",
                &[Ty::obj("kotlin/Any")]
            ))
        );

        let inv_int = Ty::obj_args("demo/Inv", &[Ty::Int]);
        let inv_string = Ty::obj_args("demo/Inv", &[Ty::String]);
        let invariant = Ty::intersection(&[inv_int, inv_string]);
        assert_eq!(
            declaration_approximation(invariant, &mut invariant_variance),
            Some(Ty::obj_args(
                "demo/Inv",
                &[Ty::out_projection(Ty::obj("kotlin/Any"))]
            ))
        );

        let inn_int = Ty::obj_args("demo/Inn", &[Ty::Int]);
        let inn_string = Ty::obj_args("demo/Inn", &[Ty::String]);
        let contravariant = Ty::intersection(&[inn_int, inn_string]);
        match declaration_approximation(contravariant, &mut in_variance).map(Ty::kind) {
            Some(TyKind::Obj(name, arguments)) => {
                assert!(name.matches("demo/Inn"));
                assert!(
                    matches!(arguments.as_slice(), [argument] if argument.is_star_projection())
                );
            }
            other => panic!("expected a star argument, got {other:?}"),
        }
    }

    #[test]
    fn a_shared_generic_classifier_requires_recorded_variance() {
        let box_int = Ty::obj_args("demo/Box", &[Ty::Int]);
        let box_string = Ty::obj_args("demo/Box", &[Ty::String]);
        assert_eq!(
            declaration_approximation(
                Ty::intersection(&[box_int, box_string]),
                &mut missing_variance,
            ),
            None
        );
    }

    #[test]
    fn differing_function_parameters_publish_a_star() {
        let left = Ty::fun(vec![Ty::Int], Ty::String);
        let right = Ty::fun(vec![Ty::String], Ty::String);
        match declaration_approximation(Ty::intersection(&[left, right]), &mut out_variance)
            .map(Ty::kind)
        {
            Some(TyKind::Fun(signature)) => {
                assert!(matches!(
                    signature.params.as_slice(),
                    [parameter] if parameter.is_star_projection()
                ));
                assert_eq!(signature.ret, Ty::String);
            }
            other => panic!("expected a function approximation, got {other:?}"),
        }
    }
}
