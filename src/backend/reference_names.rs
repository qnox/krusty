//! The Kotlin name a callable reference answers through `KCallable.name`.
//!
//! The name belongs to the referenced declaration, not to the function a backend emits for the
//! reference (an adapter, a lifted local function, a dependency's physical entry). Every backend
//! that realizes `KCallable.name` as a member of its reference objects reads it here, so each one
//! answers the same name for the same reference.

use crate::ir::{
    IrCallableReference, IrCallableReferenceTarget, IrClassifierCallable, IrFile,
    IrReferencedProperty,
};

use super::CheckedBackendCallables;

/// The Kotlin name of the declaration `reference` names, which `KCallable.name` answers.
///
/// `None` when the declaration's name cannot be read, or for a property's function-typed value. A
/// reference that cannot say its own name gets no `name` member rather than a wrong one.
pub fn callable_reference_name(
    ir: &IrFile,
    callables: &CheckedBackendCallables,
    reference: &IrCallableReference,
) -> Option<String> {
    match &reference.target {
        IrCallableReferenceTarget::Local { name, .. } => Some(name.to_string()),
        // Kotlin calls a constructor reference `<init>`, which is the name it answers too.
        IrCallableReferenceTarget::Constructor { .. } => Some("<init>".to_string()),
        // The declaration name is kept beside the call precisely so a reference's identity does
        // not depend on whatever the emitted function ended up being called.
        IrCallableReferenceTarget::Module(id) => ir
            .referenced_module_callables
            .get(id)
            .map(|callable| callable.name.to_string()),
        // A dependency declaration publishes its Kotlin name in the frozen selected-callable
        // facts; the physical spelling is an emit handle and not what `name` answers.
        IrCallableReferenceTarget::External { declaration, .. } => {
            let realization = callables.callable(*declaration)?;
            Some(
                realization
                    .reflection_name
                    .clone()
                    .unwrap_or_else(|| realization.name.clone()),
            )
        }
        IrCallableReferenceTarget::FunctionValueConversion { ordinal } => {
            Some(format!("suspendConversion{ordinal}"))
        }
        IrCallableReferenceTarget::FunctionInvoke => Some("invoke".to_string()),
        // A function value has no `name`; only the property reference itself does, a
        // `KProperty` value its target realizes as a function value.
        IrCallableReferenceTarget::Property(property) if property.reflective => {
            match property.referenced_property()? {
                IrReferencedProperty::Module(property) => ir
                    .checked_properties
                    .get(&property)
                    .map(|property| property.name.clone()),
                IrReferencedProperty::External(property) => callables
                    .property(property)
                    .map(|property| property.name.to_string()),
            }
        }
        IrCallableReferenceTarget::Property(_) => None,
        IrCallableReferenceTarget::Classifier { operation, .. } => Some(
            match operation {
                IrClassifierCallable::EnumValueOf => "valueOf",
                IrClassifierCallable::SamConstructor => "<init>",
            }
            .to_string(),
        ),
    }
}
