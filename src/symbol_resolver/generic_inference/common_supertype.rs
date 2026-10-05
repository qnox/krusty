//! Common supertype of inferred lower bounds.
//!
//! Kotlin does not erase `Inv<A>` and `Inv<B>` to a raw `Inv` or to `Any`. The classifier stays,
//! and each type argument is joined by its variance: a covariant argument joins directly, and an
//! invariant argument becomes an out-projection of that join (a captured `out` type). Unrelated
//! classifiers join at every most-specific shared supertype, which is an intersection when more
//! than one remains (`A : X, Y` and `B : X, Y` join at `X & Y`).

use std::collections::{HashSet, VecDeque};

use super::super::hierarchy_projection::{direct_supertypes, intersection_components};
use super::super::SourceOracle;
use crate::symbol_source::SymbolSource;
use crate::types::{Ty, TypeName, TypeVariance};

pub(super) fn common_super_type(source: &dyn SymbolSource, left: Ty, right: Ty) -> Ty {
    common_super_types(source, &[left, right])
}

pub(super) fn common_super_types(source: &dyn SymbolSource, bounds: &[Ty]) -> Ty {
    let nullable = bounds.iter().any(|bound| bound.is_nullable());
    let bounds = bounds
        .iter()
        .map(|bound| bound.non_null())
        .collect::<Vec<_>>();
    let result = common_at(source, &bounds);
    if nullable {
        Ty::nullable(result.non_null())
    } else {
        result
    }
}

fn common_at(source: &dyn SymbolSource, bounds: &[Ty]) -> Ty {
    let Some((&first, rest)) = bounds.split_first() else {
        return Ty::obj_name(crate::types::wk::any());
    };
    if rest.iter().all(|bound| *bound == first) {
        return first;
    }
    let oracle = SourceOracle(source);
    let context = crate::assignable::TyCtx::new();
    let closures = bounds
        .iter()
        .map(|bound| closure(source, *bound))
        .collect::<Vec<_>>();
    let mut classifiers = Vec::new();
    for candidate in &closures[0] {
        let Ty::Obj(classifier, arguments) = candidate else {
            continue;
        };
        if !classifiers.contains(&(*classifier, arguments.len()))
            && closures.iter().skip(1).all(|types| {
                types.iter().any(|ty| {
                    matches!(ty, Ty::Obj(name, args)
                        if name == classifier && args.len() == arguments.len())
                })
            })
        {
            classifiers.push((*classifier, arguments.len()));
        }
    }

    let mut candidates = Vec::new();
    for (classifier, arity) in classifiers {
        let matching = closures
            .iter()
            .map(|types| {
                types
                    .iter()
                    .copied()
                    .filter(|ty| matches!(ty, Ty::Obj(name, args) if *name == classifier && args.len() == arity))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        collect_classifier_combinations(
            source,
            classifier,
            &matching,
            0,
            &mut Vec::new(),
            &mut candidates,
        );
    }
    let mut most_specific = Vec::new();
    for index in 0..candidates.len() {
        let candidate = candidates[index];
        let dominated = candidates.iter().any(|other| {
            other != &candidate
                && crate::assignable::is_assignable(&context, &oracle, *other, candidate)
                && !crate::assignable::is_assignable(&context, &oracle, candidate, *other)
        });
        if !dominated {
            most_specific.push(candidate);
        }
    }
    match most_specific.as_slice() {
        [] => Ty::obj("kotlin/Any"),
        [one] => *one,
        many => Ty::intersection(many),
    }
}

fn collect_classifier_combinations(
    source: &dyn SymbolSource,
    classifier: TypeName,
    matching: &[Vec<Ty>],
    index: usize,
    selected: &mut Vec<Ty>,
    candidates: &mut Vec<Ty>,
) {
    if index == matching.len() {
        let combined = combine_classifier(source, classifier, selected);
        if !candidates.contains(&combined) {
            candidates.push(combined);
        }
        return;
    }
    for ty in &matching[index] {
        selected.push(*ty);
        collect_classifier_combinations(
            source,
            classifier,
            matching,
            index + 1,
            selected,
            candidates,
        );
        selected.pop();
    }
}

fn combine_classifier(source: &dyn SymbolSource, classifier: TypeName, types: &[Ty]) -> Ty {
    let arguments = types
        .iter()
        .map(|ty| match ty {
            Ty::Obj(_, arguments) => arguments.as_ref(),
            _ => &[][..],
        })
        .collect::<Vec<_>>();
    let arity = arguments.first().map_or(0, |arguments| arguments.len());
    let variances = source
        .classifier(classifier)
        .map(|shape| shape.type_param_variances().clone())
        .unwrap_or_default();
    let mut combined_arguments = Vec::with_capacity(arity);
    for index in 0..arity {
        let projected = arguments
            .iter()
            .map(|arguments| arguments[index])
            .collect::<Vec<_>>();
        if projected.iter().all(|argument| *argument == projected[0]) {
            combined_arguments.push(projected[0]);
            continue;
        }
        let variance = variances
            .get(index)
            .copied()
            .unwrap_or(TypeVariance::Invariant);
        let readable = projected
            .iter()
            .map(|argument| argument.projection_read_ty().non_null())
            .collect::<Vec<_>>();
        let joined = common_at(source, &readable);
        combined_arguments.push(match variance {
            TypeVariance::Out => joined,
            TypeVariance::Invariant => Ty::out_projection(joined),
            TypeVariance::In => Ty::star_projection(Ty::nullable(Ty::obj("kotlin/Any"))),
        });
    }
    Ty::obj_args_name(classifier, &combined_arguments)
}

/// The classifier a reified operation records. An intersection is not a runtime class. When every
/// component is a resolved classifier, the operation uses their single common supertype (`Any`
/// when they share none, that shared classifier when they do). An unresolved component keeps the
/// checked intersection: a missing hierarchy is not evidence that the components meet only at `Any`.
pub(crate) fn reified_runtime_type(
    ty: Ty,
    direct_supertypes: &dyn Fn(TypeName) -> Option<Vec<TypeName>>,
) -> Ty {
    let nullable = ty.is_nullable();
    let inner = ty.non_null().projection_read_ty().non_null();
    let Ty::Intersection(parts) = inner else {
        return ty;
    };
    let Some(approximated) = approximate_reified_intersection(parts, direct_supertypes) else {
        return ty;
    };
    if nullable {
        Ty::nullable(approximated)
    } else {
        approximated
    }
}

fn approximate_reified_intersection(
    parts: &[Ty],
    direct_supertypes: &dyn Fn(TypeName) -> Option<Vec<TypeName>>,
) -> Option<Ty> {
    if parts.is_empty() {
        return None;
    }
    let mut names = Vec::with_capacity(parts.len());
    for part in parts {
        names.push(part.non_null().obj_internal()?);
    }
    Some(Ty::obj_name(single_reified_supertype(
        &names,
        direct_supertypes,
    )?))
}

fn single_reified_supertype(
    components: &[TypeName],
    direct_supertypes: &dyn Fn(TypeName) -> Option<Vec<TypeName>>,
) -> Option<TypeName> {
    let mut closures = Vec::with_capacity(components.len());
    for name in components {
        closures.push(supertype_closure(*name, direct_supertypes)?);
    }
    let mut common = closures.first().cloned().unwrap_or_default();
    for closure in closures.iter().skip(1) {
        common.retain(|name| closure.contains(name));
    }
    let any = crate::types::wk::any();
    loop {
        if common.is_empty() {
            return Some(any);
        }
        let most = common
            .iter()
            .copied()
            .filter(|name| {
                !common.iter().any(|other| {
                    other != name
                        && supertype_closure(*other, direct_supertypes)
                            .is_some_and(|closure| closure.contains(name))
                })
            })
            .collect::<Vec<_>>();
        if most.len() == 1 {
            return Some(most[0]);
        }
        if most.is_empty() || most.len() == common.len() {
            return Some(any);
        }
        for name in most {
            common.remove(&name);
        }
    }
}

fn supertype_closure(
    name: TypeName,
    direct_supertypes: &dyn Fn(TypeName) -> Option<Vec<TypeName>>,
) -> Option<HashSet<TypeName>> {
    let mut seen = HashSet::new();
    let mut pending = vec![name];
    let any = crate::types::wk::any();
    while let Some(current) = pending.pop() {
        if !seen.insert(current) {
            continue;
        }
        if current == any {
            continue;
        }
        pending.extend(direct_supertypes(current)?);
        pending.push(any);
    }
    Some(seen)
}

fn closure(source: &dyn SymbolSource, root: Ty) -> Vec<Ty> {
    let mut queue = VecDeque::from([root.non_null()]);
    let mut seen = HashSet::new();
    let mut types = Vec::new();
    let any = Ty::obj_name(crate::types::wk::any());
    while let Some(current) = queue.pop_front() {
        if let Some(parts) = intersection_components(current) {
            queue.extend(parts.iter().copied().map(Ty::non_null));
            continue;
        }
        if !seen.insert(current) {
            continue;
        }
        types.push(current);
        queue.extend(
            direct_supertypes(source, current)
                .into_iter()
                .map(Ty::non_null),
        );
        if current != any {
            queue.push_back(any);
        }
    }
    types
}

#[cfg(test)]
mod tests {
    use super::{common_super_type, common_super_types, reified_runtime_type};
    use crate::symbol_source::SymbolSource;
    use crate::types::{type_name, Ty};

    struct EmptySource;

    impl SymbolSource for EmptySource {}

    #[test]
    fn a_deep_invariant_join_is_not_operand_order_dependent() {
        let mut left = Ty::obj("demo/Left");
        let mut right = Ty::obj("demo/Right");
        for _ in 0..24 {
            left = Ty::obj_args("demo/Inv", &[left]);
            right = Ty::obj_args("demo/Inv", &[right]);
        }

        let left_first = common_super_type(&EmptySource, left, right);
        let right_first = common_super_type(&EmptySource, right, left);
        assert_eq!(left_first, right_first);
        assert_ne!(left_first, left);
        assert_ne!(left_first, right);
    }

    #[test]
    fn an_n_ary_join_does_not_discard_the_original_lower_bounds() {
        let left = Ty::obj("demo/Left");
        let middle = Ty::obj("demo/Middle");
        let right = Ty::obj("demo/Right");
        let forward = common_super_types(&EmptySource, &[left, middle, right]);
        let reverse = common_super_types(&EmptySource, &[right, middle, left]);
        assert_eq!(forward, reverse);
    }

    #[test]
    fn an_intersection_reifies_as_its_single_common_supertype() {
        let x = type_name("demo/X");
        let y = type_name("demo/Y");
        let z = type_name("demo/Z");
        let intersection = Ty::intersection(&[Ty::obj_name(x), Ty::obj_name(y)]);
        assert_eq!(
            reified_runtime_type(intersection, &|_| None),
            intersection,
            "a missing classifier hierarchy must not become Any"
        );
        assert_eq!(
            reified_runtime_type(intersection, &|_| Some(Vec::new())),
            Ty::obj_name(crate::types::wk::any())
        );
        assert_eq!(
            reified_runtime_type(intersection, &|name| {
                if name == x || name == y {
                    Some(vec![z])
                } else if name == z {
                    Some(Vec::new())
                } else {
                    None
                }
            }),
            Ty::obj_name(z)
        );
    }
}
