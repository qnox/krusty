//! Type relations used while selecting a source constructor.
//!
//! Constructor argument matching consumes the same stable type-variable bounds as ordinary
//! assignability, while its classifier hierarchy combines current-module and dependency symbols.

use crate::types::{Ty, TypeName};

use super::SourceConstructorMatcher;

impl crate::assignable::TypeOracle for SourceConstructorMatcher<'_> {
    fn direct_supertypes(&self, ty: Ty) -> Vec<Ty> {
        let Some(internal) = ty.kotlin_class_internal() else {
            return Vec::new();
        };
        if let Some(class) = self.symbols.class_by_type_name(internal) {
            return self
                .symbols
                .applied_source_parents(class, class.type_parameter_bindings(ty))
                .into_iter()
                .map(|(_, applied)| applied)
                .collect();
        }
        crate::symbol_resolver::direct_supertypes(self.libraries, ty)
    }

    fn same_class_name(&self, a: TypeName, b: TypeName) -> bool {
        a == b
    }

    fn type_param_variance(&self, internal: TypeName, index: usize) -> crate::types::TypeVariance {
        self.symbols
            .class_by_type_name(internal)
            .and_then(|classifier| classifier.type_param_variances().get(index).copied())
            .or_else(|| {
                self.libraries
                    .classifier(internal)
                    .and_then(|classifier| classifier.type_param_variances.get(index).copied())
            })
            .unwrap_or_default()
    }

    fn type_param_upper_bounds(&self, internal: TypeName, index: usize) -> Vec<Ty> {
        self.symbols
            .class_by_type_name(internal)
            .and_then(|classifier| classifier.type_param_bounds().get(index).copied())
            .filter(|bound| *bound != Ty::Error)
            .map(|bound| vec![bound])
            .or_else(|| {
                self.libraries
                    .classifier(internal)
                    .and_then(|classifier| classifier.type_param_bounds.get(index).cloned())
                    .filter(|bounds| !bounds.is_empty())
            })
            .unwrap_or_else(|| vec![Ty::nullable(Ty::obj("kotlin/Any"))])
    }

    fn type_variable_bounds(&self, name: &str) -> Vec<Ty> {
        self.symbols.type_parameter_bounds(name)
    }
}
