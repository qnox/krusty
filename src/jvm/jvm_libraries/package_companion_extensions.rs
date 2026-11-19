//! The package record's companion-extension facet.
//!
//! A package's written `companion fun/val C.name` is named only through `C`, and only where the
//! package's declaration is in scope. It is therefore published beside the package's callables,
//! never among them.

use crate::libraries::{Callables, FunctionInfo, FunctionSet, PropertyInfo, PropertySet};

use super::{JvmLibraries, SemanticCallableOwner, TypeName};

impl JvmLibraries {
    /// Split the `functions` and `properties` a record declares under one name into its callables
    /// and its companion extensions, the latter registered under `package`'s declaration owner. A
    /// namespace that is not a package (`package` is `None`) has no companion extensions.
    pub(super) fn split_companion_extensions(
        &self,
        package: Option<TypeName>,
        functions: Vec<FunctionInfo>,
        properties: Vec<PropertyInfo>,
    ) -> (Callables, Callables) {
        let in_package = package.is_some();
        let (companion_functions, functions): (Vec<_>, Vec<_>) = functions
            .into_iter()
            .partition(|function| in_package && function.associated_classifier.is_some());
        let (companion_properties, properties): (Vec<_>, Vec<_>) = properties
            .into_iter()
            .partition(|property| in_package && property.companion_associated);
        let companion_extensions = Callables::from_parts(
            FunctionSet {
                overloads: companion_functions,
            },
            PropertySet {
                overloads: companion_properties,
            },
        );
        (
            Callables::from_parts(
                FunctionSet {
                    overloads: functions,
                },
                PropertySet {
                    overloads: properties,
                },
            ),
            self.register_external_callables(
                companion_extensions,
                package.map(SemanticCallableOwner::Package),
            ),
        )
    }
}
