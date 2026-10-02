//! Canonical `A & B` types and the denotable declaration they publish.
//!
//! Component order is structural identity: classifier identity, type arguments, and function
//! shape. Diagnostic spelling is not an identity. Nullability stays on the intersection when
//! every component admits null, so `Left? & Right?` is not the non-null type `Left & Right`.

use std::cell::Cell;
use std::cmp::Ordering;

use super::{intern_tys, Ty, TypeName, TypeVariance};

struct Piece {
    core: Ty,
    nullable: bool,
}

/// `A & B & …`. A single component is that component. A non-null `Nothing` component, or a
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
        if !cores
            .iter()
            .any(|core| structural_cmp(*core, piece.core) == Ordering::Equal)
        {
            cores.push(piece.core);
        }
    }
    cores.sort_by(|left, right| component_order(*left, *right));
    match cores.as_slice() {
        [] => Ty::Nothing,
        [one] if all_nullable => Ty::nullable(*one),
        [one] => *one,
        many => {
            let intersection = Ty::Intersection(intern_tys(many));
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
        if parenthesize_fun && matches!(ty, Ty::Fun(_)) {
            format!("({rendered})?")
        } else {
            format!("{rendered}?")
        }
    };
    match inner {
        Ty::Intersection(parts) => parts
            .iter()
            .copied()
            .map(spell_one)
            .collect::<Vec<_>>()
            .join(" & "),
        other => spell_one(other),
    }
}

fn decompose(ty: Ty, inherited_nullable: bool, out: &mut Vec<Piece>) {
    match ty {
        Ty::Error => {}
        Ty::Nullable(inner) => decompose(*inner, true, out),
        Ty::PlatformNullable(inner) => decompose(*inner, true, out),
        Ty::Intersection(parts) => {
            for part in parts.iter().copied() {
                decompose(part, inherited_nullable, out);
            }
        }
        Ty::Null => out.push(Piece {
            core: Ty::Nothing,
            nullable: true,
        }),
        other => out.push(Piece {
            core: other,
            nullable: inherited_nullable,
        }),
    }
}

/// Order components by their name path, then by structure. Intern ids follow whichever name was
/// seen first in the process, so `Right` can sort before `Left` even though the path `Left`
/// precedes `Right`. Permutations still collapse to one intersection.
fn component_order(left: Ty, right: Ty) -> Ordering {
    match (left, right) {
        (Ty::Obj(left_name, left_args), Ty::Obj(right_name, right_args)) => {
            type_path_cmp(left_name, right_name)
                .then_with(|| slice_cmp(left_args, right_args, component_order))
        }
        _ => structural_cmp(left, right),
    }
}

fn type_path_cmp(left: TypeName, right: TypeName) -> Ordering {
    fn segments(mut name: TypeName) -> Vec<&'static str> {
        let mut segments = Vec::new();
        while name != TypeName::ROOT {
            segments.push(name.segment_ref());
            name = name.parent().unwrap_or(TypeName::ROOT);
        }
        segments.reverse();
        segments
    }
    segments(left).cmp(&segments(right))
}

fn structural_cmp(left: Ty, right: Ty) -> Ordering {
    structural_rank(left)
        .cmp(&structural_rank(right))
        .then_with(|| match (left, right) {
            (Ty::Obj(left_name, left_args), Ty::Obj(right_name, right_args)) => {
                type_name_cmp(left_name, right_name)
                    .then_with(|| slice_cmp(left_args, right_args, structural_cmp))
            }
            (Ty::Fun(left), Ty::Fun(right)) => left
                .suspend
                .cmp(&right.suspend)
                .then(left.has_receiver.cmp(&right.has_receiver))
                .then(left.context_count.cmp(&right.context_count))
                .then_with(|| slice_cmp(&left.params, &right.params, structural_cmp))
                .then_with(|| structural_cmp(left.ret, right.ret)),
            (Ty::Nullable(left), Ty::Nullable(right))
            | (Ty::PlatformNullable(left), Ty::PlatformNullable(right))
            | (Ty::InProjection(left), Ty::InProjection(right))
            | (Ty::OutProjection(left), Ty::OutProjection(right))
            | (Ty::StarProjection(left), Ty::StarProjection(right))
            | (Ty::DefinitelyNotNull(left), Ty::DefinitelyNotNull(right)) => {
                structural_cmp(*left, *right)
            }
            (Ty::TyParam(left_name, left_bound), Ty::TyParam(right_name, right_bound)) => left_name
                .cmp(right_name)
                .then_with(|| structural_cmp(*left_bound, *right_bound)),
            (Ty::Intersection(left), Ty::Intersection(right)) => {
                slice_cmp(left, right, structural_cmp)
            }
            _ => Ordering::Equal,
        })
}

fn structural_rank(ty: Ty) -> u8 {
    match ty {
        Ty::Unit => 0,
        Ty::Obj(_, _) => 1,
        Ty::Null => 2,
        Ty::Nothing => 3,
        Ty::Error => 4,
        Ty::Pending => 5,
        Ty::Fun(_) => 6,
        Ty::Nullable(_) => 7,
        Ty::PlatformNullable(_) => 8,
        Ty::InProjection(_) => 9,
        Ty::OutProjection(_) => 10,
        Ty::StarProjection(_) => 11,
        Ty::TyParam(_, _) => 12,
        Ty::DefinitelyNotNull(_) => 13,
        Ty::Intersection(_) => 14,
    }
}

fn type_name_cmp(left: TypeName, right: TypeName) -> Ordering {
    left.name_id().0.cmp(&right.name_id().0)
}

fn slice_cmp(left: &[Ty], right: &[Ty], cmp: fn(Ty, Ty) -> Ordering) -> Ordering {
    left.len().cmp(&right.len()).then_with(|| {
        left.iter()
            .zip(right)
            .find_map(|(left, right)| match cmp(*left, *right) {
                Ordering::Equal => None,
                order => Some(order),
            })
            .unwrap_or(Ordering::Equal)
    })
}

/// The denotable type a public declaration publishes for an intersection.
///
/// A nullable intersection is `Any?`. Unrelated classifiers are `Any`. A shared classifier
/// keeps that classifier and approximates each argument from its variance: covariant arguments
/// join, invariant arguments become an out-projection of that join, and differing
/// contravariant arguments become a star. Function types keep a shared shape; a parameter that
/// differs across components is a star.
pub(crate) fn declaration_approximation(
    ty: Ty,
    variance_at: &mut dyn FnMut(TypeName, usize) -> TypeVariance,
) -> Ty {
    match ty {
        Ty::Nullable(inner) if matches!(*inner, Ty::Intersection(_)) => {
            Ty::nullable(Ty::obj_name(super::wk::any()))
        }
        Ty::Intersection(parts) => approximate_parts(parts, variance_at),
        other => other,
    }
}

fn approximate_parts(
    parts: &[Ty],
    variance_at: &mut dyn FnMut(TypeName, usize) -> TypeVariance,
) -> Ty {
    if parts.iter().all(|part| matches!(part, Ty::Fun(_))) {
        return approximate_functions(parts, variance_at);
    }
    if shared_classifier(parts).is_some() {
        return approximate_objects(parts, variance_at);
    }
    Ty::obj_name(super::wk::any())
}

fn shared_classifier(parts: &[Ty]) -> Option<(TypeName, usize)> {
    let Ty::Obj(name, arguments) = parts.first().copied()? else {
        return None;
    };
    parts
        .iter()
        .all(|part| matches!(part, Ty::Obj(other, args) if *other == name && args.len() == arguments.len()))
        .then_some((name, arguments.len()))
}

fn approximate_objects(
    parts: &[Ty],
    variance_at: &mut dyn FnMut(TypeName, usize) -> TypeVariance,
) -> Ty {
    let Some((name, length)) = shared_classifier(parts) else {
        return Ty::obj_name(super::wk::any());
    };
    if length == 0 {
        return Ty::obj_name(name);
    }
    let mut arguments = Vec::with_capacity(length);
    for index in 0..length {
        let at_index = parts
            .iter()
            .filter_map(|part| match part {
                Ty::Obj(_, args) => args.get(index).copied(),
                _ => None,
            })
            .collect::<Vec<_>>();
        arguments.push(approximate_argument(name, index, &at_index, variance_at));
    }
    Ty::obj_args_name(name, &arguments)
}

fn approximate_argument(
    classifier: TypeName,
    index: usize,
    arguments: &[Ty],
    variance_at: &mut dyn FnMut(TypeName, usize) -> TypeVariance,
) -> Ty {
    if arguments.is_empty() {
        return Ty::obj_name(super::wk::any());
    }
    if arguments
        .iter()
        .all(|argument| structural_cmp(*argument, arguments[0]) == Ordering::Equal)
    {
        return arguments[0];
    }
    match variance_at(classifier, index) {
        TypeVariance::Out => covariant_join(arguments, variance_at),
        TypeVariance::Invariant => Ty::out_projection(covariant_join(arguments, variance_at)),
        TypeVariance::In => Ty::star_projection(Ty::nullable(Ty::obj_name(super::wk::any()))),
    }
}

fn approximate_functions(
    parts: &[Ty],
    variance_at: &mut dyn FnMut(TypeName, usize) -> TypeVariance,
) -> Ty {
    let signatures = parts
        .iter()
        .filter_map(|part| match part {
            Ty::Fun(signature) => Some(*signature),
            _ => None,
        })
        .collect::<Vec<_>>();
    let Some(first) = signatures.first().copied() else {
        return Ty::obj_name(super::wk::any());
    };
    if signatures.iter().any(|signature| {
        signature.params.len() != first.params.len()
            || signature.suspend != first.suspend
            || signature.has_receiver != first.has_receiver
            || signature.context_count != first.context_count
    }) {
        return Ty::obj_name(super::wk::any());
    }
    let mut params = Vec::with_capacity(first.params.len());
    for index in 0..first.params.len() {
        let at_index = signatures
            .iter()
            .map(|signature| signature.params[index])
            .collect::<Vec<_>>();
        if at_index
            .iter()
            .all(|parameter| structural_cmp(*parameter, at_index[0]) == Ordering::Equal)
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
    Ty::fun_with_shape(
        params,
        covariant_join(&returns, variance_at),
        first.context_count,
        first.has_receiver,
        first.suspend,
    )
}

fn covariant_join(
    types: &[Ty],
    variance_at: &mut dyn FnMut(TypeName, usize) -> TypeVariance,
) -> Ty {
    let mut unique = Vec::new();
    for ty in types.iter().copied() {
        if !unique
            .iter()
            .any(|seen| structural_cmp(*seen, ty) == Ordering::Equal)
        {
            unique.push(ty);
        }
    }
    match unique.as_slice() {
        [] => Ty::obj_name(super::wk::any()),
        [one] => *one,
        many if shared_classifier(many).is_some() => approximate_objects(many, variance_at),
        many if many.iter().all(|ty| matches!(ty, Ty::Fun(_))) => {
            approximate_functions(many, variance_at)
        }
        many if many.iter().all(|ty| ty.is_nullable()) => {
            let cores = many.iter().map(|ty| ty.non_null()).collect::<Vec<_>>();
            Ty::nullable(covariant_join(&cores, variance_at))
        }
        _ => Ty::obj_name(super::wk::any()),
    }
}

type VarianceLookup = fn(TypeName, usize) -> TypeVariance;

thread_local! {
    static VARIANCE: Cell<VarianceLookup> = Cell::new(invariant_variance);
}

fn invariant_variance(_classifier: TypeName, _index: usize) -> TypeVariance {
    TypeVariance::Invariant
}

pub(crate) fn variance_of(classifier: TypeName, index: usize) -> TypeVariance {
    VARIANCE.with(|cell| cell.get()(classifier, index))
}

pub(crate) struct VarianceScope {
    previous: VarianceLookup,
}

impl Drop for VarianceScope {
    fn drop(&mut self) {
        VARIANCE.with(|cell| cell.set(self.previous));
    }
}

/// Install the classifier-variance lookup used while a backend encodes an intersection.
pub(crate) fn enter_variance(lookup: VarianceLookup) -> VarianceScope {
    VARIANCE.with(|cell| VarianceScope {
        previous: cell.replace(lookup),
    })
}

#[cfg(test)]
mod tests {
    use super::{declaration_approximation, Ty, TypeVariance};

    fn out_variance(_classifier: crate::types::TypeName, _index: usize) -> TypeVariance {
        TypeVariance::Out
    }

    fn invariant_variance(_classifier: crate::types::TypeName, _index: usize) -> TypeVariance {
        TypeVariance::Invariant
    }

    fn in_variance(_classifier: crate::types::TypeName, _index: usize) -> TypeVariance {
        TypeVariance::In
    }

    #[test]
    fn permutations_and_duplicates_are_one_intersection() {
        let forward = Ty::intersection(&[Ty::Int, Ty::String, Ty::Int]);
        let reverse = Ty::intersection(&[Ty::String, Ty::Int]);
        assert_eq!(forward, reverse);
        match forward {
            Ty::Intersection(parts) => assert_eq!(parts.len(), 2),
            other => panic!("expected an intersection, got {other:?}"),
        }
    }

    #[test]
    fn same_classifier_with_distinct_arguments_stays_distinct() {
        let list_int = Ty::obj_args("kotlin/collections/List", &[Ty::Int]);
        let list_string = Ty::obj_args("kotlin/collections/List", &[Ty::String]);
        let intersection = Ty::intersection(&[list_string, list_int, list_string]);
        let swapped = Ty::intersection(&[list_int, list_string]);
        assert_eq!(intersection, swapped);
        match intersection {
            Ty::Intersection(parts) => {
                assert_eq!(parts.len(), 2);
                assert!(parts.contains(&list_int));
                assert!(parts.contains(&list_string));
            }
            other => panic!("expected both list instantiations, got {other:?}"),
        }
    }

    #[test]
    fn distinct_function_shapes_stay_distinct() {
        let forward = Ty::fun(vec![Ty::Int], Ty::String);
        let reverse = Ty::fun(vec![Ty::String], Ty::Int);
        let intersection = Ty::intersection(&[forward, reverse]);
        assert_eq!(intersection, Ty::intersection(&[reverse, forward]));
        match intersection {
            Ty::Intersection(parts) => assert_eq!(parts.len(), 2),
            other => panic!("expected both function shapes, got {other:?}"),
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
            Ty::nullable(Ty::obj("kotlin/Any"))
        );
    }

    #[test]
    fn unrelated_classifiers_publish_any() {
        let intersection = Ty::intersection(&[Ty::obj("demo/Left"), Ty::obj("demo/Right")]);
        assert_eq!(
            declaration_approximation(intersection, &mut invariant_variance),
            Ty::obj("kotlin/Any")
        );
    }

    #[test]
    fn a_shared_classifier_approximates_arguments_by_variance() {
        let list_int = Ty::obj_args("kotlin/collections/List", &[Ty::Int]);
        let list_string = Ty::obj_args("kotlin/collections/List", &[Ty::String]);
        let lists = Ty::intersection(&[list_int, list_string]);
        assert_eq!(
            declaration_approximation(lists, &mut out_variance),
            Ty::obj_args("kotlin/collections/List", &[Ty::obj("kotlin/Any")])
        );

        let inv_int = Ty::obj_args("demo/Inv", &[Ty::Int]);
        let inv_string = Ty::obj_args("demo/Inv", &[Ty::String]);
        let invariant = Ty::intersection(&[inv_int, inv_string]);
        assert_eq!(
            declaration_approximation(invariant, &mut invariant_variance),
            Ty::obj_args("demo/Inv", &[Ty::out_projection(Ty::obj("kotlin/Any"))])
        );

        let inn_int = Ty::obj_args("demo/Inn", &[Ty::Int]);
        let inn_string = Ty::obj_args("demo/Inn", &[Ty::String]);
        let contravariant = Ty::intersection(&[inn_int, inn_string]);
        match declaration_approximation(contravariant, &mut in_variance) {
            Ty::Obj(name, arguments) => {
                assert!(name.matches("demo/Inn"));
                assert!(matches!(arguments, [Ty::StarProjection(_)]));
            }
            other => panic!("expected a star argument, got {other:?}"),
        }
    }

    #[test]
    fn differing_function_parameters_publish_a_star() {
        let left = Ty::fun(vec![Ty::Int], Ty::String);
        let right = Ty::fun(vec![Ty::String], Ty::String);
        match declaration_approximation(Ty::intersection(&[left, right]), &mut out_variance) {
            Ty::Fun(signature) => {
                assert!(matches!(
                    signature.params.as_slice(),
                    [Ty::StarProjection(_)]
                ));
                assert_eq!(signature.ret, Ty::String);
            }
            other => panic!("expected a function approximation, got {other:?}"),
        }
    }
}
