//! Distinct Kotlin member slots of one class that the JVM realizes as one method.
//!
//! Override resolution publishes, per class and function name, the member slots that stay distinct
//! in Kotlin: the class's own declarations with what each overrides, and one fake override per
//! group of inherited declarations nothing overrides (`IrFile::function_slot_families`). Each
//! declaration of a slot occupies its own JVM signature in the class: its method, or the bridge
//! the overriding member realizes for it. When two slots share a signature, kotlinc reports an
//! ACCIDENTAL_OVERRIDE on each of the class's own declarations whose method takes that signature,
//! or CONFLICTING_INHERITED_JVM_DECLARATIONS on the class when none does: the signature is then an
//! inherited method or a bridge.

use super::{qualified_type_name, rendered_callable_name, source_type_name};
use krusty::diag::{DiagSink, Span};
use krusty::fir::{
    ResolvedFunctionOverrideTarget, ResolvedParameterIdentity, ResolvedSlotFunction,
};
use krusty::ir::IrFile;
use krusty::jvm::bridge_erasure::bridge_erasure;
use krusty::jvm::names::method_descriptor;
use krusty::types::Ty;

/// What reading a slot member's JVM signature needs beyond the file's IR.
pub(super) struct SlotSignatureFacts<'a> {
    pub(super) override_results: &'a crate::override_results::OverrideResults,
    pub(super) callables: &'a krusty::backend::CheckedBackendCallables,
}

/// Report every JVM signature two member slots of a class share. Returns whether any was reported.
pub(super) fn report(ir: &IrFile, facts: &SlotSignatureFacts<'_>, diags: &mut DiagSink) -> bool {
    let mut reports = Vec::<(u32, String)>::new();
    for (class_id, class) in ir.classes.iter().enumerate() {
        let Some(families) = ir.function_slot_families.get(&class.fq_name_id()) else {
            continue;
        };
        let owner = class.fq_name.render().replace(['/', '$'], ".");
        let owner = owner.rsplit('.').next().unwrap_or(&owner).to_owned();
        for family in families {
            let slots = family
                .slots
                .iter()
                .map(|slot| {
                    slot.iter()
                        .map(|function| signature(ir, facts, &family.name, function))
                        .collect::<Option<Vec<_>>>()
                })
                .collect::<Option<Vec<_>>>();
            // A member whose JVM method the backend cannot name has no signature to compare.
            let Some(slots) = slots else {
                continue;
            };
            let mut seen = Vec::<(String, String)>::new();
            for (index, signatures) in slots.iter().enumerate() {
                for signature in signatures {
                    if seen.contains(signature) {
                        continue;
                    }
                    let sharing = slots
                        .iter()
                        .enumerate()
                        .filter(|(_, other)| other.contains(signature))
                        .map(|(other, _)| other)
                        .collect::<Vec<_>>();
                    if sharing.len() < 2 || sharing[0] != index {
                        continue;
                    }
                    seen.push(signature.clone());
                    let Some(listed) = sharing
                        .iter()
                        .map(|&slot| {
                            let functions = &family.slots[slot];
                            let realized = slots[slot]
                                .iter()
                                .position(|other| other == signature)
                                .expect("a sharing slot holds the signature");
                            // A declared slot takes the signature with its own method or with a
                            // bridge for the overridden declaration; an inherited slot is one fake
                            // override, which kotlinc renders as the nearest declaration.
                            let view = if functions[0].declared_by_classifier && realized != 0
                                || !functions[0].declared_by_classifier
                                    && realized == 0
                                    && applied_descriptor(&functions[0]) != signature.1
                            {
                                MemberView::Bridge(&functions[realized])
                            } else {
                                MemberView::Member(&functions[0])
                            };
                            render(&family.name, view, &owner)
                        })
                        .collect::<Option<Vec<_>>>()
                    else {
                        continue;
                    };
                    let listed = sorted(listed);
                    // A declaration is reported where its own method takes the signature; a
                    // bridge it realizes for an overridden declaration is not an override of
                    // its own.
                    let declared = sharing
                        .iter()
                        .filter(|&&slot| {
                            family.slots[slot][0].declared_by_classifier
                                && slots[slot][0] == *signature
                        })
                        .map(|&slot| &family.slots[slot][0])
                        .collect::<Vec<_>>();
                    let (name, descriptor) = signature;
                    if declared.is_empty() {
                        let offset = ir
                            .class_diagnostic_span(class_id as u32)
                            .map_or(0, |span| span.lo);
                        reports.push((
                            offset,
                            message(
                                "inherited platform declarations clash",
                                name,
                                descriptor,
                                &listed,
                            ),
                        ));
                    }
                    for function in declared {
                        let Some(offset) = declaration_offset(ir, function) else {
                            continue;
                        };
                        reports.push((
                            offset,
                            message("accidental override", name, descriptor, &listed),
                        ));
                    }
                }
            }
        }
    }
    reports.sort_by_key(|(offset, _)| *offset);
    let found = !reports.is_empty();
    for (offset, message) in reports {
        diags.error(Span::new(offset, offset), message);
    }
    found
}

fn declaration_offset(ir: &IrFile, function: &ResolvedSlotFunction) -> Option<u32> {
    let ResolvedFunctionOverrideTarget::Module(callable) = function.target else {
        return None;
    };
    let function = *ir.checked_callable_functions.get(&callable)?;
    ir.function_position(function).signature_offset
}

/// The JVM method name and descriptor a slot member occupies in the class.
fn signature(
    ir: &IrFile,
    facts: &SlotSignatureFacts<'_>,
    name: &str,
    function: &ResolvedSlotFunction,
) -> Option<(String, String)> {
    match function.target {
        ResolvedFunctionOverrideTarget::External(target) => {
            let callable = facts.callables.callable(target)?;
            // A declaration with no classfile method of its own (a big-arity `FunctionN.invoke`)
            // is realized through another member, which this check does not name.
            if callable.descriptor.is_empty() {
                return None;
            }
            Some((
                krusty::jvm::names::mapped_builtin_virtual_name(
                    callable.physical_owner,
                    callable.physical_name(),
                    &callable.descriptor,
                )
                .to_owned(),
                callable.descriptor.clone(),
            ))
        }
        ResolvedFunctionOverrideTarget::Module(callable) => {
            if let Some(&realized) = ir.checked_callable_functions.get(&callable) {
                return Some((
                    ir.functions[realized as usize].name.clone(),
                    crate::ir_emit::function_descriptor(ir, facts.override_results, realized),
                ));
            }
            // A declaration of another file: its declared shape erases as the bridge that
            // realizes it here would.
            let mut parameters = function
                .declared_parameters
                .iter()
                .map(|parameter| bridge_erasure(parameter.get()))
                .collect::<Vec<_>>();
            let mut result = bridge_erasure(function.declared_result.get());
            if function.suspend {
                parameters.push(Ty::obj("kotlin/coroutines/Continuation"));
                result = Ty::nullable(Ty::obj("kotlin/Any"));
            }
            Some((name.to_owned(), method_descriptor(&parameters, result)))
        }
    }
}

/// How a slot is listed: as the class's member with the types the class sees, or as the bridge
/// the class realizes for an overridden declaration, whose type parameters take their bounds.
enum MemberView<'a> {
    Member(&'a ResolvedSlotFunction),
    Bridge(&'a ResolvedSlotFunction),
}

/// The descriptor the class's view of an inherited declaration erases to. When it differs from the
/// declaration's own, kotlinc lists the declaration as the bridge-like method it is in the class.
fn applied_descriptor(function: &ResolvedSlotFunction) -> String {
    let parameters = function
        .applied_parameters
        .iter()
        .map(|ty| bridge_erasure(ty.get()))
        .collect::<Vec<_>>();
    method_descriptor(&parameters, bridge_erasure(function.applied_result.get()))
}

/// A bridge's type: a type parameter's own bound in its place.
fn bridged(ty: Ty) -> Ty {
    match ty.ty_param_bound() {
        Some(bound) => bridged(bound),
        None => ty,
    }
}

fn render(name: &str, view: MemberView<'_>, owner: &str) -> Option<(String, String)> {
    let (function, types, result) = match view {
        MemberView::Member(function) => (
            function,
            function
                .applied_parameters
                .iter()
                .map(|ty| ty.get())
                .collect::<Vec<_>>(),
            function.applied_result.get(),
        ),
        MemberView::Bridge(function) => (
            function,
            function
                .declared_parameters
                .iter()
                .map(|ty| bridged(ty.get()))
                .collect(),
            bridged(function.declared_result.get()),
        ),
    };
    let mut rendered = String::from("fun ");
    let mut qualified = String::from("fun ");
    let mut parameters = Vec::new();
    for (ty, identity) in types.into_iter().zip(function.parameter_identities.iter()) {
        match identity {
            ResolvedParameterIdentity::ExtensionReceiver => {
                rendered.push_str(&source_type_name(ty).ok()?);
                rendered.push('.');
                qualified.push_str(&qualified_type_name(ty).ok()?);
                qualified.push('.');
            }
            ResolvedParameterIdentity::Source(parameter) => {
                parameters.push((rendered_callable_name(parameter), ty));
            }
            ResolvedParameterIdentity::Unnamed { ordinal } => {
                parameters.push((
                    crate::parameter_names::unnamed_java_parameter(*ordinal as usize),
                    ty,
                ));
            }
            _ => return None,
        }
    }
    let write = |target: &mut String, type_name: &dyn Fn(Ty) -> Option<String>| {
        target.push_str(&rendered_callable_name(name));
        target.push('(');
        for (index, (parameter, ty)) in parameters.iter().enumerate() {
            if index > 0 {
                target.push_str(", ");
            }
            target.push_str(parameter);
            target.push_str(": ");
            target.push_str(&type_name(*ty)?);
        }
        target.push_str("): ");
        target.push_str(&type_name(result)?);
        target.push_str(" defined in ");
        target.push_str(owner);
        Some(())
    };
    write(&mut rendered, &|ty| source_type_name(ty).ok())?;
    write(&mut qualified, &|ty| qualified_type_name(ty).ok())?;
    Some((qualified, rendered))
}

/// kotlinc lists the declarations by their fully qualified rendering.
fn sorted(mut listed: Vec<(String, String)>) -> Vec<String> {
    listed.sort();
    listed.into_iter().map(|(_, rendered)| rendered).collect()
}

fn message(kind: &str, name: &str, descriptor: &str, declarations: &[String]) -> String {
    let mut message = format!(
        "{kind}: The following declarations have the same JVM signature ({name}{descriptor}):"
    );
    for declaration in declarations {
        message.push_str("\n    ");
        message.push_str(declaration);
    }
    message
}
