//! Every type-parameter declaration record a file's common IR carries.
//!
//! A type-parameter occurrence names its declaration by semantic identity. The declaration's
//! complete written bounds travel separately: with the file's own generic functions and classes,
//! with the formals a body captures, and with each module declaration the file calls, copied from
//! the declaring source. A backend that needs those bounds for an occurrence finds them here.

use super::{IrFile, Ty};

impl IrFile {
    /// Each declaration record this file holds, as its semantic identity and complete written
    /// bounds. One identity may be listed by more than one record; each copy carries the same
    /// checked bounds.
    pub(crate) fn type_parameter_declarations(&self) -> Vec<(&str, &[(Ty, bool)])> {
        let declared = self
            .signatures
            .values()
            .chain(self.class_signatures.values())
            .flat_map(|signature| &signature.type_params)
            .chain(self.callable_bound_type_parameters.values().flatten())
            .chain(self.type_reflection.type_parameter_declarations())
            .map(|parameter| {
                (
                    parameter.semantic_name.as_str(),
                    parameter.bounds.as_slice(),
                )
            });
        let referenced = self
            .referenced_module_callables
            .values()
            .flat_map(|callable| {
                callable
                    .type_parameters
                    .iter()
                    .chain(&callable.owner_type_parameters)
            })
            .chain(
                self.referenced_module_properties
                    .values()
                    .flat_map(|property| {
                        property
                            .type_parameters
                            .iter()
                            .chain(&property.owner_type_parameters)
                    }),
            )
            .chain(
                self.module_constructions
                    .records
                    .values()
                    .flat_map(|constructor| &constructor.owner_type_parameters),
            )
            .map(|parameter| (parameter.semantic_name.as_str(), &*parameter.bounds));
        declared.chain(referenced).collect()
    }
}
