//! Immutable checked facts of the reached KLIB body graph.
//!
//! Resolution collects these records before backend entry. Consumers read the same semantic
//! snapshot; it contains no provider reference or lookup operation.

use crate::assignable::TypeOracle;
use crate::fx_hash::HashMap;
use crate::libraries::LibraryType;
use crate::types::{Ty, TyKind, TypeName, TypeVariance};
use std::sync::Arc;

/// Declaration facts of one selected dependency graph, separate from executable body roots.
#[derive(Debug, Default)]
pub(crate) struct CheckedKlibDependencyFacts {
    pub(crate) builtin_implementations:
        HashMap<super::ExternalCallableId, Box<[super::BuiltinImplementation]>>,
    pub(crate) member_edges:
        HashMap<crate::libraries::KlibDependencyCallable, super::CheckedMemberCallEdge>,
    pub(crate) type_hierarchy: Option<CheckedKlibTypeFacts>,
}

/// Provider-normalized class facts reachable from the serialized types a checker selected.
#[derive(Clone, Default)]
pub(crate) struct CheckedKlibTypeFacts {
    pub(crate) classifiers: HashMap<TypeName, Arc<LibraryType>>,
    pub(crate) actualized_expects: HashMap<TypeName, TypeName>,
    pub(crate) flexible_upper_bounds: HashMap<TypeName, TypeName>,
    pub(crate) type_variable_bounds: HashMap<String, Box<[Ty]>>,
}

impl CheckedKlibTypeFacts {
    fn classifier(&self, identity: TypeName) -> Option<&Arc<LibraryType>> {
        let identity = self
            .actualized_expects
            .get(&identity)
            .copied()
            .unwrap_or(identity);
        self.classifiers.get(&identity)
    }
}

impl std::fmt::Debug for CheckedKlibTypeFacts {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CheckedKlibTypeFacts")
            .field("classifiers", &self.classifiers.len())
            .field("actualized_expects", &self.actualized_expects.len())
            .field("flexible_upper_bounds", &self.flexible_upper_bounds.len())
            .field("type_variable_bounds", &self.type_variable_bounds.len())
            .finish()
    }
}

impl TypeOracle for CheckedKlibTypeFacts {
    fn direct_supertypes(&self, ty: Ty) -> Vec<Ty> {
        if let TyKind::Fun(_) = ty.non_null().kind() {
            return vec![
                crate::libraries::function_classifiers::supertype_classifier(ty.non_null()),
            ];
        }
        let Some(identity) = ty.kotlin_class_internal() else {
            return Vec::new();
        };
        let Some(classifier) = self.classifier(identity) else {
            return Vec::new();
        };
        crate::assignable::classifier_supertypes(classifier, ty, &|name| {
            self.classifier(name).map(|classifier| {
                (
                    classifier.type_params().clone(),
                    classifier.type_param_bounds().clone(),
                )
            })
        })
    }

    fn same_class_name(&self, left: TypeName, right: TypeName) -> bool {
        left == right
            || self.actualized_expects.get(&left).copied().unwrap_or(left)
                == self
                    .actualized_expects
                    .get(&right)
                    .copied()
                    .unwrap_or(right)
    }

    fn type_param_variance(&self, owner: TypeName, index: usize) -> TypeVariance {
        self.classifier(owner)
            .and_then(|classifier| classifier.type_param_variances().get(index).copied())
            .unwrap_or_default()
    }

    fn type_param_upper_bounds(&self, owner: TypeName, index: usize) -> Vec<Ty> {
        self.classifier(owner)
            .and_then(|classifier| classifier.type_param_bounds().get(index).cloned())
            .filter(|bounds| !bounds.is_empty())
            .unwrap_or_else(|| vec![Ty::nullable(Ty::obj("kotlin/Any"))])
    }

    fn platform_flexible_upper_bound(&self, lower: Ty) -> Ty {
        let TyKind::Obj(owner, arguments) = lower.kind() else {
            return lower;
        };
        let Some(upper) = self.flexible_upper_bounds.get(&owner).copied() else {
            return lower;
        };
        Ty::obj_args_name(upper, arguments.as_slice())
    }

    fn type_variable_bounds(&self, name: &str) -> Vec<Ty> {
        self.type_variable_bounds
            .get(name)
            .map_or_else(Vec::new, |bounds| bounds.to_vec())
    }
}
