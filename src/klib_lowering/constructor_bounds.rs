//! Constructor-bound proof over authenticated caller formals and explicit class evidence.

use super::klib_types::KlibTypeParameters;
use crate::assignable::{is_subtype, TyCtx, TypeOracle};
use crate::types::{Ty, TypeName, TypeVariance};

struct NoClassEvidence;

impl TypeOracle for NoClassEvidence {
    fn direct_supertypes(&self, _ty: Ty) -> Vec<Ty> {
        Vec::new()
    }
}

struct CallerBounds<'a> {
    classes: &'a dyn TypeOracle,
    parameters: &'a KlibTypeParameters,
}

impl TypeOracle for CallerBounds<'_> {
    fn direct_supertypes(&self, ty: Ty) -> Vec<Ty> {
        self.classes.direct_supertypes(ty)
    }
    fn same_class_name(&self, left: TypeName, right: TypeName) -> bool {
        self.classes.same_class_name(left, right)
    }
    fn type_param_variance(&self, owner: TypeName, index: usize) -> TypeVariance {
        self.classes.type_param_variance(owner, index)
    }
    fn type_param_upper_bounds(&self, owner: TypeName, index: usize) -> Vec<Ty> {
        self.classes.type_param_upper_bounds(owner, index)
    }
    fn platform_flexible_upper_bound(&self, lower: Ty) -> Ty {
        self.classes.platform_flexible_upper_bound(lower)
    }
    fn type_variable_bounds(&self, name: &str) -> Vec<Ty> {
        match self.parameters.declared_bounds(name) {
            // Even an empty vector is known declaration evidence (the implicit Any? bound).
            Some(bounds) => bounds.to_vec(),
            None => self.classes.type_variable_bounds(name),
        }
    }
}

pub(super) fn satisfies(
    classes: Option<&dyn TypeOracle>,
    parameters: &KlibTypeParameters,
    argument: Ty,
    bound: Ty,
) -> bool {
    let oracle = CallerBounds {
        classes: classes.unwrap_or(&NoClassEvidence),
        parameters,
    };
    is_subtype(&TyCtx::new(), &oracle, argument, bound)
}
