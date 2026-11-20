//! Joins that keep several unrelated most-specific common supertypes as an intersection.

use crate::assignable::TypeOracle;
use crate::symbol_source::SymbolSource;
use crate::types::{Ty, TyKind};

/// The intersection of `parts`, or the single part when only one remains.
pub(super) fn intersect_parts(parts: &[Ty]) -> Option<Ty> {
    match parts {
        [] => None,
        [single] => Some(*single),
        parts => Some(Ty::intersection(parts)),
    }
}

/// The join when an operand is itself an intersection, `None` when neither operand is one.
///
/// An intersection operand is a subtype of each of its components, so every supertype common to
/// one component and the other operand is common to both; the join is their intersection.
pub(super) fn join_intersection_operand(
    source: &dyn SymbolSource,
    oracle: &dyn TypeOracle,
    left: Ty,
    right: Ty,
    visiting: &mut crate::fx_hash::HashSet<(Ty, Ty)>,
) -> Option<Option<Ty>> {
    for (intersected, other, intersected_is_left) in [(left, right, true), (right, left, false)] {
        let TyKind::Intersection(components) = intersected.non_null().kind() else {
            continue;
        };
        let any = Ty::obj("kotlin/Any");
        let mut joined = components
            .iter()
            .filter_map(|&component| {
                let (l, r) = if intersected_is_left {
                    (component, other)
                } else {
                    (other, component)
                };
                super::applied_common_supertype_unguarded(source, oracle, l, r, visiting)
            })
            .map(Ty::non_null)
            .collect::<Vec<_>>();
        if joined.len() > 1 {
            joined.retain(|part| *part != any);
        }
        return Some(
            intersect_parts(&joined)
                .map(|joined| super::common_result_nullability(left, right, joined)),
        );
    }
    None
}

/// Whether `ty` names a classifier for the universal-supertype fallback: `Unit`, a class, or an
/// intersection of them.
pub(super) fn has_value_classifier(ty: Ty) -> bool {
    ty.non_null() == Ty::Unit
        || ty.kotlin_class_internal().is_some()
        || matches!(ty.non_null().kind(), TyKind::Intersection(_))
}

/// The join of every most-specific common classifier. Several unrelated ones (a shared superclass
/// and a shared interface) are all supertypes of both operands, and kotlinc keeps each of them as
/// an intersection.
pub(super) fn join_most_specific(
    source: &dyn SymbolSource,
    oracle: &dyn TypeOracle,
    most_specific: &[(crate::types::TypeName, Ty, Ty)],
    visiting: &mut crate::fx_hash::HashSet<(Ty, Ty)>,
) -> Option<Ty> {
    let joined = most_specific
        .iter()
        .map(|&(owner, left, right)| {
            super::join_applied_classifier(source, oracle, owner, left, right, visiting)
        })
        .collect::<Option<Vec<_>>>()?;
    intersect_parts(&joined)
}
