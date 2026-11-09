//! Realize the checked callable-reference identity without inspecting its invocation adapter.

use crate::callable_references::{Declaration, ReferenceKind};
use krusty::ir::{IrCallableReference, IrCallableReferenceTarget as Target, IrReferencedProperty};

use super::*;

/// The classifier-owned callables a reference may name, by the operation that tells them apart.
const ENUM_VALUE_OF: u8 = 0;
const SAM_CONSTRUCTOR: u8 = 1;
const ENUM_ENTRIES: u8 = 2;

impl Body<'_> {
    pub(super) fn callable_reference_kind(
        &mut self,
        reference: &IrCallableReference,
    ) -> Result<i32, Unsupported> {
        let declaration = match &reference.target {
            Target::Module(callable) => Declaration::Source {
                module: self.module_name.to_owned(),
                callable: *callable,
            },
            Target::External { declaration, .. } => {
                let identity = self
                    .callables
                    .callable(*declaration)
                    .and_then(|fact| fact.dependency_callable.as_ref())
                    .ok_or(
                        "a callable reference without its module-qualified dependency identity",
                    )?;
                Declaration::Dependency(identity.clone())
            }
            Target::Local { function, .. } => Declaration::Local {
                file: self.reference_file,
                function: *function,
            },
            Target::Constructor { classifier, .. } => Declaration::Constructor {
                classifier: *classifier,
                parameters: reference.declaration_parameters.clone(),
                result: reference.declaration_result,
            },
            Target::FunctionValueConversion { ordinal } => Declaration::Conversion {
                ordinal: *ordinal,
                parameters: reference.declaration_parameters.clone(),
                result: reference.declaration_result,
                suspend: reference.declaration_suspend,
            },
            // A property read as a function value is the property reference, equal to every other
            // reference to the same property. Its adapter only performs the read.
            Target::Property(property) => match property.referenced_property() {
                Some(IrReferencedProperty::Module(property)) => Declaration::Property {
                    module: self.module_name.to_owned(),
                    property,
                },
                Some(IrReferencedProperty::External(property)) => {
                    let identity = self
                        .callables
                        .property(property)
                        .and_then(|fact| self.callables.callable(fact.getter))
                        .and_then(|fact| fact.dependency_callable.as_ref())
                        .ok_or(
                            "a property reference without its module-qualified dependency identity",
                        )?;
                    Declaration::Dependency(identity.clone())
                }
                None => match &property.target {
                    krusty::fir::FirPropertyReferenceTarget::Classifier {
                        owner,
                        property: krusty::fir::FirClassifierProperty::EnumEntries,
                        ..
                    } => Declaration::Classifier {
                        classifier: *owner,
                        operation: ENUM_ENTRIES,
                    },
                    _ => {
                        return Err(
                            "a property read as a function value without its property declaration"
                                .into(),
                        );
                    }
                },
            },
            Target::FunctionInvoke => Declaration::Invoke {
                arity: reference.declaration_parameters.len(),
                suspend: reference.declaration_suspend,
            },
            Target::Classifier {
                classifier,
                operation,
            } => Declaration::Classifier {
                classifier: *classifier,
                operation: match operation {
                    krusty::ir::IrClassifierCallable::EnumValueOf => ENUM_VALUE_OF,
                    krusty::ir::IrClassifierCallable::SamConstructor => SAM_CONSTRUCTOR,
                },
            },
        };
        let TyKind::Fun(function) = reference.function_type.non_null().kind() else {
            return Err("a callable reference without its checked function type".into());
        };
        let suspend_conversion = function.suspend && !reference.declaration_suspend;
        let mut flags = u8::from(suspend_conversion) << 1;
        if let Some(adaptation) = reference.adaptation.as_deref() {
            let vararg_conversion = adaptation.arguments.iter().any(|argument| {
                matches!(
                    argument,
                    krusty::fir::FirAdaptedReferenceArgument::Vararg {
                        whole_array: false,
                        ..
                    }
                )
            });
            let unit_conversion = adaptation.result_type.get() == Ty::Unit
                && reference.declaration_result != Ty::Unit;
            flags |= u8::from(vararg_conversion)
                | (u8::from(adaptation.suspend_conversion) << 1)
                | (u8::from(unit_conversion) << 2);
        }
        self.runtime.callable_references.kind(ReferenceKind {
            declaration,
            bound: reference.bound_receiver.is_some(),
            reflection_owner: reference.reflection_owner,
            arity: function.params.len(),
            adapted: reference.adaptation.is_some() || suspend_conversion,
            adaptation_flags: flags,
        })
    }
}
