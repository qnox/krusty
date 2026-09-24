use std::collections::{HashMap, HashSet};

use crate::types::{Ty, TypeName};

use super::{ExprId, IrFile};

/// Target-neutral semantic facts captured before a backend erases value-class identities.
#[derive(Default)]
pub(super) struct ValueClassConstructorFacts {
    /// Classes whose primary constructor has a value-class-typed parameter.
    primary_owners: HashSet<TypeName>,
    /// Primary-constructor parameter types as declared, for Kotlin metadata.
    primary_declared_params: HashMap<TypeName, Vec<Ty>>,
    /// Exact selected constructions whose declared parameter list mentions a value class.
    parameter_constructions: HashSet<ExprId>,
}

impl ValueClassConstructorFacts {
    pub(super) fn remap_classifier_identities(
        &mut self,
        names: &HashMap<TypeName, TypeName>,
        remap_ty: impl Fn(Ty) -> Ty,
    ) {
        self.primary_owners = std::mem::take(&mut self.primary_owners)
            .into_iter()
            .map(|owner| names.get(&owner).copied().unwrap_or(owner))
            .collect();
        self.primary_declared_params = std::mem::take(&mut self.primary_declared_params)
            .into_iter()
            .map(|(owner, parameters)| {
                (
                    names.get(&owner).copied().unwrap_or(owner),
                    parameters
                        .into_iter()
                        .map(|value| remap_ty(value))
                        .collect(),
                )
            })
            .collect();
    }
}

impl IrFile {
    pub fn mark_value_param_ctor(&mut self, internal: &str) {
        self.mark_value_param_ctor_name(crate::types::type_name(internal));
    }

    pub fn mark_value_param_ctor_name(&mut self, internal: TypeName) {
        self.value_class_constructor_facts
            .primary_owners
            .insert(internal);
    }

    pub fn has_value_param_ctor(&self, internal: &str) -> bool {
        self.value_class_constructor_facts
            .primary_owners
            .contains(&crate::types::type_name(internal))
    }

    pub(crate) fn mark_value_class_parameter_construction(&mut self, call: ExprId) {
        self.value_class_constructor_facts
            .parameter_constructions
            .insert(call);
    }

    pub(crate) fn has_value_class_parameter_construction(&self, call: ExprId) -> bool {
        self.value_class_constructor_facts
            .parameter_constructions
            .contains(&call)
    }

    pub fn record_vc_ctor_declared_params(&mut self, internal: TypeName, declared: Vec<Ty>) {
        self.value_class_constructor_facts
            .primary_declared_params
            .insert(internal, declared);
    }

    pub fn vc_ctor_declared_params(&self, internal: TypeName) -> Option<&[Ty]> {
        self.value_class_constructor_facts
            .primary_declared_params
            .get(&internal)
            .map(Vec::as_slice)
    }

    pub(super) fn vc_ctor_declared_param_types(&self) -> impl Iterator<Item = Ty> + '_ {
        self.value_class_constructor_facts
            .primary_declared_params
            .values()
            .flatten()
            .copied()
    }
}
