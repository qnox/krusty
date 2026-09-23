//! Provider-neutral value-class declaration facts and representation traversal.
//!
//! This module answers which classifier has which exact underlying type and walks nested chains.
//! It deliberately does not decide whether a target carrier is a reference, how it is boxed, or
//! which descriptor/storage ABI realizes it. A backend supplies that policy through
//! [`RepresentationPolicy`] and owns the resulting physical representation.

use crate::types::{Ty, TypeName};
use std::collections::{HashMap, HashSet};

/// Exact value-class classifier identity to its declared underlying semantic type.
pub(crate) type UnderlyingTypes = HashMap<TypeName, Ty>;

/// Target policy consulted only when a nullable value-class occurrence is reached.
///
/// Non-null value classes always project to their declared underlying type. For a nullable
/// occurrence, a backend decides whether its carrier can preserve all semantic values and the
/// additional outer `null`. The common traversal never equates "reference" with any particular
/// target representation.
pub(crate) trait RepresentationPolicy {
    fn project_nullable(
        &self,
        classifier: TypeName,
        underlying: Ty,
        declarations: &UnderlyingTypes,
    ) -> bool;
}

/// Recursively project a value-class type according to one target's nullable-carrier policy.
///
/// The returned [`Ty`] is still a semantic type. Selecting a machine carrier, box, storage layout,
/// or emitted signature remains the backend's responsibility.
pub(crate) fn project_underlying<P: RepresentationPolicy + ?Sized>(
    ty: Ty,
    declarations: &UnderlyingTypes,
    policy: &P,
) -> Ty {
    fn project<P: RepresentationPolicy + ?Sized>(
        ty: Ty,
        declarations: &UnderlyingTypes,
        policy: &P,
        seen: &mut HashSet<TypeName>,
    ) -> Ty {
        let Some(classifier) = ty.non_null().obj_internal() else {
            return ty;
        };
        let Some(&underlying) = declarations.get(&classifier) else {
            return ty;
        };
        if !seen.insert(classifier) {
            return ty;
        }
        if ty.is_nullable() && !policy.project_nullable(classifier, underlying, declarations) {
            return ty;
        }
        project(underlying, declarations, policy, seen)
    }

    project(ty, declarations, policy, &mut HashSet::new())
}

/// Whether a value class's underlying chain can itself denote `null`.
///
/// This is the provider-neutral distinguishability constraint for a nullable outer value class:
/// when true, an underlying `null` value and the outer value class's own `null` are semantically
/// distinct. Backends decide how their carrier preserves that distinction.
pub(crate) fn underlying_chain_accepts_null(ty: Ty, declarations: &UnderlyingTypes) -> bool {
    fn accepts(ty: Ty, declarations: &UnderlyingTypes, seen: &mut HashSet<TypeName>) -> bool {
        if ty.is_nullable() {
            return true;
        }
        let Some(classifier) = ty.obj_internal() else {
            return false;
        };
        let Some(&underlying) = declarations.get(&classifier) else {
            return false;
        };
        seen.insert(classifier) && accepts(underlying, declarations, seen)
    }

    accepts(ty, declarations, &mut HashSet::new())
}

pub(crate) fn nullable_value_requires_distinct_null(
    classifier: TypeName,
    declarations: &UnderlyingTypes,
) -> bool {
    declarations
        .get(&classifier)
        .copied()
        .is_some_and(|underlying| underlying_chain_accepts_null(underlying, declarations))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct AlwaysProject;

    impl RepresentationPolicy for AlwaysProject {
        fn project_nullable(
            &self,
            _classifier: TypeName,
            _underlying: Ty,
            _declarations: &UnderlyingTypes,
        ) -> bool {
            true
        }
    }

    #[test]
    fn nested_projection_keeps_exact_qualified_underlying_chain() {
        let declarations = [
            (crate::types::type_name("left/Id"), Ty::obj("right/Id")),
            (crate::types::type_name("right/Id"), Ty::String),
            (crate::types::type_name("other/Id"), Ty::Int),
        ]
        .into_iter()
        .collect();

        assert_eq!(
            project_underlying(Ty::obj("left/Id"), &declarations, &AlwaysProject),
            Ty::String,
        );
        assert_eq!(
            project_underlying(Ty::obj("other/Id"), &declarations, &AlwaysProject),
            Ty::Int,
        );
    }

    #[test]
    fn nullable_underlying_chain_requires_two_distinct_null_values() {
        let declarations = [
            (
                crate::types::type_name("sample/Outer"),
                Ty::obj("sample/Inner"),
            ),
            (
                crate::types::type_name("sample/Inner"),
                Ty::nullable(Ty::String),
            ),
            (crate::types::type_name("sample/Plain"), Ty::String),
        ]
        .into_iter()
        .collect();

        assert!(nullable_value_requires_distinct_null(
            crate::types::type_name("sample/Outer"),
            &declarations,
        ));
        assert!(!nullable_value_requires_distinct_null(
            crate::types::type_name("sample/Plain"),
            &declarations,
        ));
    }
}
