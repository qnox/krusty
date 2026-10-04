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
