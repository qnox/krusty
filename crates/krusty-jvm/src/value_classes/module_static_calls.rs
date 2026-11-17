//! JVM names of this file's calls to static declarations elsewhere in the module: file-facade
//! functions, `companion { }` block members, and their property accessors. Each takes the name the
//! declaring file's value-class pass gives it, from the declaration's recorded signature when the
//! call names one, and the call's own physical parameters erase over the carriers.

use super::member_names::{self, vc_mangle, vc_member_impl_name};
use super::representation::erase;
use super::{default_calls, private_accessor_calls, Under};
use krusty::fx_hash::HashMap;
use krusty::ir::{Callee, ExprId, IrExpr, IrFile};
use krusty::types::{Ty, TypeName};

pub(super) fn realize(
    ir: &mut IrFile,
    module_value_classes: &HashMap<TypeName, Ty>,
    callable_under: &Under,
    under: &Under,
) {
    let mut module_physical_parameters = Vec::new();
    for (index, e) in ir.exprs.iter_mut().enumerate() {
        let IrExpr::Call { callee, .. } = e else {
            continue;
        };
        let (name, params, ret, module_target, module_default_call, semantic_default, owner) =
            match &mut **callee {
                Callee::CrossFile {
                    facade,
                    name,
                    params,
                    ret,
                    module_target,
                    module_default_call,
                } => (
                    name,
                    params,
                    ret,
                    *module_target,
                    *module_default_call,
                    false,
                    Some(*facade),
                ),
                Callee::ModuleWithDefaults {
                    target,
                    default_provider,
                    name,
                    params,
                    ret,
                    dispatch_receiver_ty,
                    ..
                } => {
                    if let Some(receiver) = dispatch_receiver_ty {
                        *receiver = erase(receiver, under);
                    }
                    let provider = default_calls::module_provider(*target, *default_provider);
                    (name, params, ret, Some(provider), true, true, None)
                }
                _ => continue,
            };
        let record_physical = if let Some(callable) =
            module_target.and_then(|target| ir.referenced_module_callables.get(&target))
        {
            let (mangled, value_class_bounds) = member_names::module_call_jvm_name(
                member_names::ModuleCallSignature {
                    name,
                    parameters: &callable.parameters,
                    type_parameters: &callable.type_parameters,
                    result: &callable.result,
                    suspend: callable.flags.has(krusty::fir::DeclarationFlags::SUSPEND),
                },
                member_names::ModuleCallOwner {
                    // A `companion { }` block member is a static member of its value class.
                    value_class_member: callable
                        .owner
                        .or(match callable.placement {
                            krusty::ir::IrStaticPlacement::CompanionBlock { declaring_class } => {
                                Some(declaring_class)
                            }
                            krusty::ir::IrStaticPlacement::Package => None,
                        })
                        .is_some_and(|owner| module_value_classes.contains_key(&owner)),
                    file: callable.owner.is_none(),
                    module_default_call,
                    semantic_default,
                },
                callable_under,
            );
            *name = mangled;
            for (parameter, bound) in value_class_bounds {
                if let Some(slot) = params.get_mut(parameter) {
                    *slot = bound;
                }
            }
            ir.declaration_argument_boundaries
                .contains_key(&(index as ExprId))
        } else {
            if !private_accessor_calls::keeps_name(&ir.private_property_accessor_calls, index) {
                // A static of a value class (a `companion { }` block property's accessor) is
                // named like every member realized there.
                *name = if owner.is_some_and(|owner| module_value_classes.contains_key(&owner)) {
                    vc_member_impl_name(name, params, ret, callable_under, false)
                } else {
                    vc_mangle(name, params, ret, callable_under, true, false)
                };
            }
            false
        };
        for parameter in params.iter_mut() {
            *parameter = erase(parameter, under);
        }
        *ret = erase(ret, under);
        if record_physical {
            module_physical_parameters.push((index as ExprId, params.clone()));
        }
    }
    for (call, parameters) in module_physical_parameters {
        ir.physical_call_parameters
            .insert(call, parameters.into_boxed_slice());
    }
}
