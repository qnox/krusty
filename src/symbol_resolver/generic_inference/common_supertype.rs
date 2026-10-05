//! Common supertype of two inferred lower bounds.
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

const DEPTH_LIMIT: u32 = 16;

pub(super) fn common_super_type(source: &dyn SymbolSource, left: Ty, right: Ty) -> Ty {
    let nullable = left.is_nullable() || right.is_nullable();
    let result = common_at(source, left.non_null(), right.non_null(), 0);
    if nullable {
        Ty::nullable(result.non_null())
    } else {
        result
    }
}

fn common_at(source: &dyn SymbolSource, left: Ty, right: Ty, depth: u32) -> Ty {
    if depth > DEPTH_LIMIT || left == right {
        return left;
    }
    let oracle = SourceOracle(source);
    let context = crate::assignable::TyCtx::new();
    let left_to_right = crate::assignable::is_assignable(&context, &oracle, left, right);
    let right_to_left = crate::assignable::is_assignable(&context, &oracle, right, left);
    if left_to_right && !right_to_left {
        return right;
    }
    if right_to_left && !left_to_right {
        return left;
    }
    if let (Ty::Obj(left_name, left_args), Ty::Obj(right_name, right_args)) = (left, right) {
        if left_name == right_name && left_args.len() == right_args.len() {
            return combine_classifier(source, left_name, left_args, right_args, depth);
        }
    }
    let mut candidates = Vec::new();
    for left_type in closure(source, left) {
        for right_type in closure(source, right) {
            let (Ty::Obj(left_name, left_args), Ty::Obj(right_name, right_args)) =
                (left_type, right_type)
            else {
                continue;
            };
            if left_name != right_name || left_args.len() != right_args.len() {
                continue;
            }
            let combined = if left_type == right_type {
                left_type
            } else {
                combine_classifier(source, left_name, left_args, right_args, depth)
            };
            if !candidates.contains(&combined) {
                candidates.push(combined);
            }
        }
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

fn combine_classifier(
    source: &dyn SymbolSource,
    classifier: TypeName,
    left_args: &[Ty],
    right_args: &[Ty],
    depth: u32,
) -> Ty {
    let variances = source
        .classifier(classifier)
        .map(|shape| shape.type_param_variances().clone())
        .unwrap_or_default();
    let mut arguments = Vec::with_capacity(left_args.len());
    for (index, (&left, &right)) in left_args.iter().zip(right_args).enumerate() {
        if left == right {
            arguments.push(left);
            continue;
        }
        let variance = variances
            .get(index)
            .copied()
            .unwrap_or(TypeVariance::Invariant);
        let joined = common_at(
            source,
            left.projection_read_ty().non_null(),
            right.projection_read_ty().non_null(),
            depth + 1,
        );
        arguments.push(match variance {
            TypeVariance::Out => joined,
            TypeVariance::Invariant => Ty::out_projection(joined),
            TypeVariance::In => Ty::star_projection(Ty::nullable(Ty::obj("kotlin/Any"))),
        });
    }
    Ty::obj_args_name(classifier, &arguments)
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
    use super::reified_runtime_type;
    use crate::types::{type_name, Ty};

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
