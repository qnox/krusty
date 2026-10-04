//! The synthetic `access$…` methods a private declaration needs when something outside it calls it.
//!
//! Each one forwards to an already-selected declaration; nothing here looks a target up, and the
//! bridge's own shape follows that declaration's rather than the call site's.

use super::*;

pub(super) fn emit_function_reference_access_bridge(
    ir: &IrFile,
    fid: u32,
    owner: &str,
    cw: &mut ClassWriter,
    owner_is_interface: bool,
    line: u32,
) {
    let function = &ir.functions[fid as usize];
    let parameters = jvm_function_params(ir, fid);
    let result = jvm_declared_ty(&function.ret);
    // A STATIC target — a value class's member accessor, realized over the carrier — is already
    // callable without an instance, so the bridge takes exactly its parameters and `invokestatic`s
    // it. An instance target keeps the owner ahead of them and a non-virtual call to the private
    // declaration.
    let mut bridge_parameters = Vec::with_capacity(parameters.len() + 1);
    if !function.is_static {
        bridge_parameters.push(Ty::obj(owner));
    }
    bridge_parameters.extend(parameters.iter().copied());
    let name = format!("access${}", function.name);
    let bridge_descriptor = method_descriptor(&bridge_parameters, result);
    // kotlinc visits a method's name and descriptor before its body.
    cw.reserve_method_name(&name);
    cw.reserve_descriptor(&bridge_descriptor);
    let mut code = CodeBuilder::new(bridge_parameters.iter().map(|ty| slot_words(*ty)).sum());
    let mut slot = 0u16;
    if !function.is_static {
        code.aload(0);
        slot = 1;
    }
    for &parameter in &parameters {
        load(parameter, slot, &mut code);
        slot += slot_words(parameter);
    }
    let descriptor = method_descriptor(&parameters, result);
    let method = if owner_is_interface {
        cw.interface_methodref(owner, &function.name, &descriptor)
    } else {
        cw.methodref(owner, &function.name, &descriptor)
    };
    let argument_words = parameters.iter().map(|ty| slot_words(*ty) as i32).sum();
    if line != 0 {
        code.mark_line(line);
    }
    if function.is_static {
        code.invokestatic(method, argument_words, slot_words(result) as i32);
    } else {
        code.invokespecial(method, argument_words, slot_words(result) as i32);
    }
    emit_return(result, &mut code);
    code.ensure_locals(slot.max(1));
    code.link();
    cw.add_method(
        0x1019, /* PUBLIC | STATIC | FINAL | SYNTHETIC */
        &name,
        &bridge_descriptor,
        &code,
    );
    set_bridge_locals(ir, fid, owner, &parameters, &name, &bridge_descriptor, cw);
}

/// kotlinc's whole-method locals of an `access$…` bridge: `$this` for an instance target, then the
/// target's own parameter names over the slots they arrive in.
fn set_bridge_locals(
    ir: &IrFile,
    fid: u32,
    owner: &str,
    parameters: &[Ty],
    name: &str,
    descriptor: &str,
    cw: &mut ClassWriter,
) {
    let extension_receivers = ir
        .function_parameter_identities(fid)
        .expect("an access bridge target carries exact parameter identities")
        .iter()
        .map(|identity| matches!(identity.role, crate::ir::IrParameterRole::ExtensionReceiver))
        .collect::<Vec<_>>();
    let names = crate::jvm::parameter_names::function_locals(ir, fid, parameters)
        .expect("an access bridge target carries exact parameter identities");
    // The accessor's own table spells an extension receiver `$this$<name>`. The bridge that
    // forwards to it spells that same slot `$receiver`, as kotlinc's `access$` local table does.
    let names = names
        .into_iter()
        .zip(extension_receivers)
        .map(|(name, extension)| {
            if extension {
                Some("$receiver".to_string())
            } else {
                name
            }
        });
    let mut locals = Vec::with_capacity(parameters.len() + 1);
    let mut slot = 0u16;
    if !ir.functions[fid as usize].is_static {
        locals.push(("$this".to_string(), format!("L{owner};"), 0));
        slot = 1;
    }
    for (name, &parameter) in names.into_iter().zip(parameters) {
        if let Some(name) = name {
            locals.push((name, local_variable_desc(parameter), slot));
        }
        slot += slot_words(parameter);
    }
    cw.set_method_debug(name, descriptor, None, &locals);
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ProtectedMemberAccessBridge {
    pub owner: crate::types::TypeName,
    name: String,
    target_parameters: Vec<Ty>,
    pub bridge_parameters: Vec<Ty>,
    result: Ty,
    parameter_names: Vec<Option<String>>,
}

impl ProtectedMemberAccessBridge {
    /// The accessor of a protected property accessor that a reference carrier calls: named after
    /// the accessor, over its own erased descriptor.
    pub(super) fn of_reference(
        bridge: &crate::jvm::property_references::ProtectedReferenceBridgeMethod,
    ) -> Self {
        let (parameters, result) = parse_physical_method_desc(&bridge.target_descriptor)
            .expect("a protected reference bridge retains a selected accessor descriptor");
        let target_parameters = parameters.iter().map(ir_ty_to_jvm).collect::<Vec<_>>();
        Self {
            owner: bridge.owner,
            name: bridge.target_name.clone(),
            parameter_names: bridge.target_parameter_names.clone(),
            bridge_parameters: target_parameters.clone(),
            target_parameters,
            result: ir_ty_to_jvm(&result),
        }
    }

    /// The accessor's name and descriptor, which identify it in its owner.
    pub(super) fn signature(&self) -> (String, String) {
        let mut parameters = Vec::with_capacity(self.bridge_parameters.len() + 1);
        parameters.push(Ty::obj_name(self.owner));
        parameters.extend(self.bridge_parameters.iter().copied());
        (
            format!("access${}", self.name),
            method_descriptor(&parameters, self.result),
        )
    }
}

pub(super) struct MemberAccessBridges {
    pub private: std::collections::HashSet<u32>,
    pub protected: std::collections::HashMap<crate::ir::ExprId, ProtectedMemberAccessBridge>,
}

fn protected_bridge_owner(
    ir: &IrFile,
    expression: crate::ir::ExprId,
    receiver: Option<crate::ir::ExprId>,
    physical_owner: &str,
    target_owner: crate::types::TypeName,
) -> Option<crate::types::TypeName> {
    let receiver_owner = receiver
        .and_then(|receiver| ir.logical_types.get(&receiver))
        .copied()
        .and_then(crate::types::Ty::kotlin_class_internal);
    receiver_owner
        .filter(|receiver| *receiver != target_owner && ir.class_id_by_name(*receiver).is_some())
        .or_else(|| {
            std::iter::successors(
                ir.expression_owners.get(&expression).copied(),
                |enclosing| enclosing.nested_owner(),
            )
            .find(|enclosing| {
                !enclosing.matches(physical_owner) && ir.class_id_by_name(*enclosing).is_some()
            })
        })
        .filter(|bridge_owner| {
            !bridge_owner.matches(physical_owner)
                && bridge_owner.namespace() != target_owner.namespace()
        })
}

/// The local-variable names of a protected property accessor's parameters: the setter's value is
/// named as its declaration names it.
fn protected_property_parameter_names(
    property: &crate::ir::IrModuleProperty,
    write: bool,
    accessor: &str,
) -> Vec<Option<String>> {
    let mut names = vec![None; property.context_parameters.len()];
    names.extend(property.extension_receiver.map(|_| None));
    if write {
        let setter = property.setter_parameter.as_ref().map(std::slice::from_ref);
        names.push(setter.and_then(|identity| {
            crate::jvm::parameter_names::resolved_local_variables(
                identity,
                &[property.ty],
                accessor,
            )
            .pop()
            .flatten()
        }));
    }
    names
}

/// Find private instance calls whose caller and declaration are different JVM classes, and
/// protected calls physically emitted outside the checked receiver subclass that grants access.
///
/// FIR/common IR retain Kotlin ownership and the selected member identity only. The Java-8 access
/// bridge is a physical realization, so this whole-file reachability walk belongs at the backend
/// boundary and runs once per emission pass, never once per method candidate.
pub(super) fn cross_owner_member_calls(
    ir: &IrFile,
    facade: &str,
    contexts: &[static_accessors::EmissionContext],
    private_interface_bodies_are_members: bool,
) -> MemberAccessBridges {
    let mut private = std::collections::HashSet::new();
    let mut protected = std::collections::HashMap::new();
    let mut scan = |owner: &str, roots: Vec<crate::ir::ExprId>, export_private: bool| {
        let mut seen = std::collections::HashSet::new();
        let mut stack = roots;
        while let Some(expression) = stack.pop() {
            if !seen.insert(expression) {
                continue;
            }
            // Local members carry a `FunId`; sibling-source members carry the exact stable module
            // declaration selected by FIR. Neither path reconstructs identity from owner/name.
            let local_target = match ir.expr(expression) {
                IrExpr::MethodCall {
                    class,
                    index,
                    receiver,
                    ..
                } => Some((
                    *class,
                    ir.classes[*class as usize].methods[*index as usize],
                    Some(*receiver),
                )),
                IrExpr::Call {
                    callee: Callee::Static { owner, .. },
                    ..
                } => ir.jvm_member_targets.get(&expression).map(|&function| {
                    let class = ir.class_id_by_name(*owner);
                    (
                        class.expect("a realized member's owner is in this file"),
                        function,
                        None,
                    )
                }),
                IrExpr::Call {
                    callee: Callee::Virtual { owner, .. },
                    dispatch_receiver: Some(receiver),
                    ..
                } => ir.jvm_member_targets.get(&expression).map(|&function| {
                    let class = ir.class_id_by_name(*owner);
                    (
                        class.expect("a realized member's owner is in this file"),
                        function,
                        Some(*receiver),
                    )
                }),
                IrExpr::PropertyRead {
                    owner,
                    receiver,
                    operation,
                    ..
                } => realized_property_accessor(ir, expression, *operation, *owner, *receiver),
                IrExpr::PropertyWrite {
                    owner,
                    receiver,
                    operation,
                    ..
                } => realized_property_accessor(ir, expression, *operation, *owner, *receiver),
                _ => None,
            };
            if let Some((class, target, receiver)) = local_target {
                let target_class = &ir.classes[class as usize];
                let visibility = ir.method_visibility(target);
                let crosses_owner = target_class.fq_name() != owner;
                // A non-private inline function's own body is copied into other classes, so a
                // private method of this same class still needs a public accessor.
                let export_same_owner =
                    export_private && !crosses_owner && !ir.lifted_functions.contains_key(&target);
                if (crosses_owner || export_same_owner)
                    && (private_interface_bodies_are_members || !target_class.is_interface)
                    && visibility.is_private()
                {
                    private.insert(target);
                }
                if visibility == crate::types::Visibility::Protected {
                    let receiver_owner = receiver
                        .and_then(|receiver| ir.logical_types.get(&receiver))
                        .copied()
                        .and_then(crate::types::Ty::kotlin_class_internal);
                    // Prefer the checked receiver classifier. Generic member selection may have
                    // already coerced that receiver to the declaring superclass; in that case the
                    // semantic containment edge identifies the enclosing source subclass. Walking
                    // `nested_owner` follows typed name-tree identity, never rendered JVM `$` text.
                    let bridge_owner = receiver_owner
                        .filter(|receiver| {
                            *receiver != target_class.fq_name_id()
                                && ir.class_id_by_name(*receiver).is_some()
                        })
                        .or_else(|| {
                            std::iter::successors(
                                ir.expression_owners.get(&expression).copied(),
                                |enclosing| enclosing.nested_owner(),
                            )
                            .find(|enclosing| {
                                !enclosing.matches(owner)
                                    && ir.class_id_by_name(*enclosing).is_some()
                            })
                        });
                    if let Some(bridge_owner) = bridge_owner.filter(|bridge_owner| {
                        !bridge_owner.matches(owner)
                            && bridge_owner.namespace() != target_class.fq_name_id().namespace()
                    }) {
                        let function = &ir.functions[target as usize];
                        let target_parameters = jvm_function_params(ir, target);
                        let bridge_parameters = ir
                            .module_member_accesses
                            .get(&expression)
                            .map(|access| match access {
                                crate::ir::IrModuleMemberAccess::Callable {
                                    selected_parameters,
                                    ..
                                } => selected_parameters
                                    .iter()
                                    .map(jvm_declared_ty)
                                    .collect::<Vec<_>>(),
                                crate::ir::IrModuleMemberAccess::Property {
                                    selected_parameters,
                                    ..
                                } => selected_parameters
                                    .iter()
                                    .map(jvm_declared_ty)
                                    .collect::<Vec<_>>(),
                            })
                            .unwrap_or_else(|| target_parameters.clone());
                        let parameter_names = crate::jvm::parameter_names::function_locals(
                            ir,
                            target,
                            &bridge_parameters,
                        )
                        .expect("an access bridge target carries exact parameter identities");
                        protected.insert(
                            expression,
                            ProtectedMemberAccessBridge {
                                owner: bridge_owner,
                                name: function.name.clone(),
                                target_parameters,
                                bridge_parameters,
                                result: jvm_declared_ty(&function.ret),
                                parameter_names,
                            },
                        );
                    }
                }
            }
            if let Some(crate::ir::IrModuleMemberAccess::Callable {
                target,
                selected_parameters,
            }) = ir.module_member_accesses.get(&expression)
            {
                crate::trace_compiler!(
                    "emit",
                    "module member access expression={expression} target={target:?} physical_owner={owner} local={}",
                    local_target.is_some()
                );
                if local_target.is_none() {
                    let Some(callable) = ir.referenced_module_callables.get(target) else {
                        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| {
                            stack.push(child)
                        });
                        continue;
                    };
                    let Some(target_owner) = callable.owner else {
                        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| {
                            stack.push(child)
                        });
                        continue;
                    };
                    crate::trace_compiler!(
                        "emit",
                        "module member declaration expression={expression} owner={} visibility={:?}",
                        target_owner,
                        callable.visibility
                    );
                    let (name, target_parameters, result, receiver) = match ir.expr(expression) {
                        IrExpr::Call {
                            callee:
                                Callee::Virtual {
                                    name,
                                    params: Some((parameters, result)),
                                    ..
                                },
                            dispatch_receiver: Some(receiver),
                            ..
                        } => (
                            name.clone(),
                            parameters.iter().map(jvm_declared_ty).collect(),
                            jvm_declared_ty(result),
                            Some(*receiver),
                        ),
                        _ => {
                            crate::ir::for_each_child(&ir.exprs, expression, &mut |child| {
                                stack.push(child)
                            });
                            continue;
                        }
                    };
                    if callable.visibility == crate::types::Visibility::Protected {
                        if let Some(bridge_owner) =
                            protected_bridge_owner(ir, expression, receiver, owner, target_owner)
                        {
                            crate::trace_compiler!(
                                "emit",
                                "protected member bridge expression={expression} owner={} target_owner={}",
                                bridge_owner,
                                target_owner
                            );
                            protected.insert(
                                expression,
                                ProtectedMemberAccessBridge {
                                    owner: bridge_owner,
                                    name,
                                    target_parameters,
                                    bridge_parameters: selected_parameters
                                        .iter()
                                        .map(jvm_declared_ty)
                                        .collect(),
                                    result,
                                    parameter_names: callable
                                        .parameter_identities
                                        .iter()
                                        .map(|identity| identity.source_name.clone())
                                        .collect(),
                                },
                            );
                        }
                    }
                }
            }
            if let Some(crate::ir::IrModuleMemberAccess::Property {
                target,
                write,
                selected_parameters,
            }) = ir.module_member_accesses.get(&expression)
            {
                let Some(property) = ir.referenced_module_properties.get(target) else {
                    crate::ir::for_each_child(&ir.exprs, expression, &mut |child| {
                        stack.push(child)
                    });
                    continue;
                };
                let Some(target_owner) = property.owner else {
                    crate::ir::for_each_child(&ir.exprs, expression, &mut |child| {
                        stack.push(child)
                    });
                    continue;
                };
                let visibility = if *write {
                    property.setter_visibility
                } else {
                    property.visibility
                };
                if visibility == crate::types::Visibility::Protected {
                    let (name, result, receiver) = match ir.expr(expression) {
                        IrExpr::Call {
                            callee: Callee::Virtual { name, .. },
                            dispatch_receiver: Some(receiver),
                            ..
                        } => (
                            name.clone(),
                            if *write { Ty::Unit } else { property.ty },
                            Some(*receiver),
                        ),
                        IrExpr::PropertyRead { receiver, .. } => (
                            crate::names::property_getter_name(&property.name),
                            property.ty,
                            *receiver,
                        ),
                        IrExpr::PropertyWrite { receiver, .. } => (
                            crate::names::property_setter_name(&property.name),
                            Ty::Unit,
                            *receiver,
                        ),
                        _ => {
                            crate::ir::for_each_child(&ir.exprs, expression, &mut |child| {
                                stack.push(child)
                            });
                            continue;
                        }
                    };
                    if let Some(bridge_owner) =
                        protected_bridge_owner(ir, expression, receiver, owner, target_owner)
                    {
                        let mut target_parameters = property.context_parameters.clone();
                        if let Some(extension) = property.extension_receiver {
                            target_parameters.push(extension);
                        }
                        if *write {
                            target_parameters.push(property.ty);
                        }
                        let parameter_names =
                            protected_property_parameter_names(property, *write, &name);
                        protected.insert(
                            expression,
                            ProtectedMemberAccessBridge {
                                owner: bridge_owner,
                                name,
                                target_parameters: target_parameters
                                    .iter()
                                    .map(jvm_declared_ty)
                                    .collect(),
                                bridge_parameters: selected_parameters
                                    .iter()
                                    .map(jvm_declared_ty)
                                    .collect(),
                                result: jvm_declared_ty(&result),
                                parameter_names,
                            },
                        );
                    }
                }
            }
            if !protected.contains_key(&expression) {
                if let Some(dependency) = ir.protected_dependency_calls.get(&expression).cloned() {
                    if let IrExpr::Call {
                        callee: Callee::Virtual { .. },
                        dispatch_receiver: Some(receiver),
                        ..
                    } = ir.expr(expression)
                    {
                        if let Some(bridge_owner) = protected_bridge_owner(
                            ir,
                            expression,
                            Some(*receiver),
                            owner,
                            dependency.owner,
                        ) {
                            crate::trace_compiler!(
                                "emit",
                                "protected dependency bridge expression={expression} owner={} target_owner={}",
                                bridge_owner,
                                dependency.owner
                            );
                            let parameters = dependency
                                .parameters
                                .iter()
                                .map(jvm_declared_ty)
                                .collect::<Vec<_>>();
                            let parameter_names = (0..parameters.len())
                                .map(|index| Some(format!("p{index}")))
                                .collect();
                            protected.insert(
                                expression,
                                ProtectedMemberAccessBridge {
                                    owner: bridge_owner,
                                    name: dependency.name,
                                    target_parameters: parameters.clone(),
                                    bridge_parameters: parameters,
                                    result: jvm_declared_ty(&dependency.result),
                                    parameter_names,
                                },
                            );
                        }
                    }
                }
            }
            crate::ir::for_each_child(&ir.exprs, expression, &mut |child| stack.push(child));
        }
    };

    for context in contexts {
        let owner = context.owner.internal_name(facade);
        for &root in &context.roots {
            scan(
                &owner,
                vec![root],
                static_accessors::non_private_inline_body(ir, root),
            );
        }
    }
    MemberAccessBridges { private, protected }
}

/// The `access$…` bridges of `class`'s private members that another class calls, after its declared
/// members as kotlinc's synthetic-accessor lowering appends them. A member realized as a static
/// value-class `-impl` gets a bridge over the same parameters.
pub(super) fn emit_private_member_access_bridges(
    ir: &IrFile,
    class: &IrClass,
    owner: &str,
    cw: &mut ClassWriter,
    run: &EmitRun,
) {
    let bridged = run.private_member_access_bridges.borrow();
    for &fid in class.methods.iter().filter(|fid| bridged.contains(fid)) {
        if ir.functions[fid as usize].is_static {
            emit_function_reference_access_bridge(ir, fid, owner, cw, false, class.decl_line);
        } else {
            emit_private_member_access_bridge(ir, fid, owner, cw, false, class.decl_line);
        }
    }
}

/// Emit protected-member accessor `bridge` into its owner `owner`'s class: load `$this` and each
/// parameter, adapt each to the member's own descriptor, and call the member on `$this`.
pub(super) fn emit_protected_member_access_bridge(
    bridge: &ProtectedMemberAccessBridge,
    owner: &str,
    cw: &mut ClassWriter,
    line: u32,
) {
    let target_descriptor = method_descriptor(&bridge.target_parameters, bridge.result);
    let mut bridge_parameters = Vec::with_capacity(bridge.bridge_parameters.len() + 1);
    bridge_parameters.push(Ty::obj_name(bridge.owner));
    bridge_parameters.extend(bridge.bridge_parameters.iter().copied());
    let bridge_descriptor = method_descriptor(&bridge_parameters, bridge.result);
    let bridge_name = format!("access${}", bridge.name);
    if cw.declares_method(&bridge_name, &bridge_descriptor) {
        return;
    }
    let mut code = CodeBuilder::new(
        bridge_parameters
            .iter()
            .map(|parameter| slot_words(*parameter))
            .sum(),
    );
    code.aload(0);
    let mut slot = 1;
    for (&bridge_parameter, &target_parameter) in bridge
        .bridge_parameters
        .iter()
        .zip(&bridge.target_parameters)
    {
        load(bridge_parameter, slot, &mut code);
        slot += slot_words(bridge_parameter);
        if bridge_parameter != target_parameter {
            if bridge_parameter.is_jvm_scalar() && target_parameter.is_reference() {
                box_prim_free(cw, &mut code, bridge_parameter);
            } else if bridge_parameter.is_reference() && target_parameter.is_jvm_scalar() {
                unbox_prim_from(cw, &mut code, bridge_parameter, target_parameter);
            } else if bridge_parameter.is_jvm_scalar() && target_parameter.is_jvm_scalar() {
                emit_num_conv(bridge_parameter, target_parameter, &mut code);
            } else if bridge_parameter.is_reference() && target_parameter.is_reference() {
                let class = cw.class_ref(&crate::jvm::names::instanceof_internal_name(
                    target_parameter,
                ));
                code.checkcast(class);
            }
        }
    }
    let target = cw.methodref(owner, &bridge.name, &target_descriptor);
    if line != 0 {
        code.mark_line(line);
    }
    let argument_words = bridge
        .target_parameters
        .iter()
        .map(|parameter| slot_words(*parameter) as i32)
        .sum();
    code.invokevirtual(target, argument_words, slot_words(bridge.result) as i32);
    emit_return(bridge.result, &mut code);
    code.ensure_locals(slot);
    code.link();
    cw.add_method(
        0x1019, /* PUBLIC | STATIC | FINAL | SYNTHETIC */
        &bridge_name,
        &bridge_descriptor,
        &code,
    );
    set_protected_bridge_locals(bridge, owner, &bridge_name, &bridge_descriptor, cw);
}

fn set_protected_bridge_locals(
    bridge: &ProtectedMemberAccessBridge,
    owner: &str,
    name: &str,
    descriptor: &str,
    cw: &mut ClassWriter,
) {
    let mut locals = vec![("$this".to_string(), format!("L{owner};"), 0)];
    let mut slot = 1u16;
    for (source_name, &parameter) in bridge.parameter_names.iter().zip(&bridge.bridge_parameters) {
        if let Some(source_name) = source_name {
            locals.push((source_name.clone(), local_variable_desc(parameter), slot));
        }
        slot += slot_words(parameter);
    }
    cw.set_method_debug(name, descriptor, None, &locals);
}

/// Redirect a selected property accessor through its protected bridge when this exact operation's
/// physical owner is outside the checked receiver subclass.
pub(super) fn protected_property_access(
    run: &EmitRun,
    expression: crate::ir::ExprId,
    access: crate::jvm::inline::PropertyAccess,
) -> crate::jvm::inline::PropertyAccess {
    use crate::jvm::inline::PropertyAccess;
    let Some(bridge) = run
        .protected_member_access_bridges
        .borrow()
        .get(&expression)
        .cloned()
    else {
        return access;
    };
    if !matches!(
        access,
        PropertyAccess::Accessor {
            is_static: false,
            ..
        }
    ) {
        return access;
    }
    let mut parameters = vec![Ty::obj_name(bridge.owner)];
    parameters.extend(bridge.bridge_parameters.iter().copied());
    PropertyAccess::AccessBridge {
        owner: bridge.owner,
        name: format!("access${}", bridge.name),
        descriptor: method_descriptor(&parameters, bridge.result),
        takes_receiver: true,
        inline_uninitialized_guard: None,
    }
}

/// The getter or setter recorded for this property operation.
///
/// The record is the declaration's function, attached when the operation is realized or when a
/// companion initializer moves onto another class. A missing record stays missing: the accessor
/// is not chosen by scanning the owner's properties for the source spelling.
fn realized_property_accessor(
    ir: &IrFile,
    expression: crate::ir::ExprId,
    operation: Option<u32>,
    owner: crate::types::TypeName,
    receiver: Option<crate::ir::ExprId>,
) -> Option<(crate::ir::ClassId, u32, Option<crate::ir::ExprId>)> {
    let function = ir
        .jvm_member_targets
        .get(&operation.unwrap_or(expression))
        .copied()?;
    let class = ir.class_id_by_name(owner)?;
    Some((class, function, receiver))
}

/// How another class reaches a private accessor through `access$<name>`: a static value-class
/// `-impl` bridge takes the same carrier, and an instance accessor's takes the owner.
pub(super) fn private_member_accessor_access(
    ir: &IrFile,
    getter: u32,
    owner: TypeName,
) -> crate::jvm::inline::PropertyAccess {
    use crate::jvm::inline::PropertyAccess;
    let function = &ir.functions[getter as usize];
    let parameters = jvm_function_params(ir, getter);
    let result = jvm_declared_ty(&function.ret);
    let name = format!("access${}", function.name);
    if function.is_static {
        return PropertyAccess::Accessor {
            owner,
            name,
            descriptor: method_descriptor(&parameters, result),
            is_static: true,
            is_interface: false,
            static_receiver: (function.dispatch_receiver == Some(owner))
                .then(|| parameters.first().copied())
                .flatten(),
        };
    }
    let mut bridge_parameters = vec![Ty::obj_name(owner)];
    bridge_parameters.extend(parameters);
    PropertyAccess::AccessBridge {
        owner,
        name,
        descriptor: method_descriptor(&bridge_parameters, result),
        takes_receiver: true,
        inline_uninitialized_guard: None,
    }
}

/// A private declaration referenced from another file of this module. Its only legal cross-file
/// use is the body of a non-private `inline` function, which the caller expands into its own class.
pub(super) fn private_module_callable(ir: &IrFile, target: Option<crate::fir::CallableId>) -> bool {
    target.is_some_and(|target| {
        ir.referenced_module_callables
            .get(&target)
            .is_some_and(|callable| callable.visibility.is_private())
    })
}

/// An already-selected member call: a protected or private access bridge, a same-owner private
/// accessor, or the ordinary interface or class invocation.
pub(super) struct SelectedMemberCall<'a> {
    pub(super) expression: crate::ir::ExprId,
    pub(super) owner_identity: TypeName,
    pub(super) owner: &'a str,
    pub(super) name: &'a str,
    pub(super) descriptor: &'a str,
    pub(super) parameters: &'a [Ty],
    pub(super) result: Ty,
    pub(super) interface_owner: bool,
    pub(super) argument_words: i32,
    pub(super) protected: Option<&'a ProtectedMemberAccessBridge>,
    /// The caller is a non-private `inline` function, so a same-class private member goes through
    /// its accessor: the copied body must not name the private method.
    pub(super) export_private_calls: bool,
}

/// The physical invocation shape the selected declaration needs.
///
/// Only [`MemberInvocation::Virtual`] dispatches on the call-site spelling, so only it may carry a
/// call-site retarget (the special-builtin rename): every other shape names something derived from
/// the DECLARED member — an `access$` bridge or the private accessor itself — and bridge bodies
/// always forward to the declared name.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum MemberInvocation {
    ProtectedBridge,
    PrivateExtensionBridge,
    DirectPrivateAccessor,
    Virtual,
}

/// Select the [`MemberInvocation`] for an already-resolved member call. No lookup or overload
/// selection happens here: `protected` reports the caller's recorded bridge, and the private
/// shapes read the selected declaration's visibility.
pub(super) fn select_member_invocation(
    ir: &IrFile,
    run: &EmitRun,
    source_owner: Option<StaticOwner>,
    expression: crate::ir::ExprId,
    owner_identity: TypeName,
    protected: bool,
    export_private_calls: bool,
) -> MemberInvocation {
    let member_target = ir.jvm_member_targets.get(&expression).copied();
    let private_extension_bridge = member_target.is_some_and(|function| {
        (source_owner != Some(StaticOwner::Class(owner_identity)) || export_private_calls)
            && run
                .private_member_access_bridges
                .borrow()
                .contains(&function)
    });
    let same_owner_private = !private_extension_bridge
        && source_owner == Some(StaticOwner::Class(owner_identity))
        && member_target.is_some_and(|function| ir.method_visibility(function).is_private());
    if protected {
        MemberInvocation::ProtectedBridge
    } else if private_extension_bridge {
        MemberInvocation::PrivateExtensionBridge
    } else if same_owner_private {
        MemberInvocation::DirectPrivateAccessor
    } else {
        MemberInvocation::Virtual
    }
}

/// Emit [`SelectedMemberCall`]. The exact selected declaration determines whether the physical
/// invocation goes through its access bridge; descriptors and owners are built here too.
pub(super) fn emit_selected_member_call(
    ir: &IrFile,
    run: &EmitRun,
    source_owner: Option<StaticOwner>,
    cw: &mut ClassWriter,
    code: &mut CodeBuilder,
    call: &SelectedMemberCall<'_>,
) {
    match select_member_invocation(
        ir,
        run,
        source_owner,
        call.expression,
        call.owner_identity,
        call.protected.is_some(),
        call.export_private_calls,
    ) {
        MemberInvocation::ProtectedBridge => emit_protected_member_invocation(
            cw,
            code,
            call.protected
                .expect("a protected-bridge invocation carries its bridge"),
            call,
        ),
        MemberInvocation::PrivateExtensionBridge => {
            emit_private_member_extension_call(cw, code, call)
        }
        MemberInvocation::DirectPrivateAccessor => {
            emit_direct_private_accessor_call(cw, code, call)
        }
        MemberInvocation::Virtual if call.interface_owner => {
            let method = cw.interface_methodref(call.owner, call.name, call.descriptor);
            code.invokeinterface(
                method,
                call.argument_words,
                physical_call_result_words(call.result),
            );
        }
        MemberInvocation::Virtual => {
            let method = cw.methodref(call.owner, call.name, call.descriptor);
            code.invokevirtual(
                method,
                call.argument_words,
                physical_call_result_words(call.result),
            );
        }
    }
}

fn emit_protected_member_invocation(
    cw: &mut ClassWriter,
    code: &mut CodeBuilder,
    bridge: &ProtectedMemberAccessBridge,
    call: &SelectedMemberCall<'_>,
) {
    let mut bridge_parameters = Vec::with_capacity(bridge.bridge_parameters.len() + 1);
    bridge_parameters.push(Ty::obj_name(bridge.owner));
    bridge_parameters.extend(bridge.bridge_parameters.iter().copied());
    let bridge_descriptor = method_descriptor(&bridge_parameters, call.result);
    let bridge_name = format!("access${}", call.name);
    let method = cw.methodref(&bridge.owner.render(), &bridge_name, &bridge_descriptor);
    code.invokestatic(
        method,
        call.argument_words + 1,
        physical_call_result_words(call.result),
    );
}

/// `invokespecial` of a private accessor from its declaring class.
fn emit_direct_private_accessor_call(
    cw: &mut ClassWriter,
    code: &mut CodeBuilder,
    call: &SelectedMemberCall<'_>,
) {
    let method = if call.interface_owner {
        cw.interface_methodref(call.owner, call.name, call.descriptor)
    } else {
        cw.methodref(call.owner, call.name, call.descriptor)
    };
    code.invokespecial(
        method,
        call.argument_words,
        physical_call_result_words(call.result),
    );
}

/// `invokestatic access$<name>(Owner, …)` for a private member reached from another class.
///
/// An interface owner is an `InterfaceMethodref`; a class owner is a `Methodref`. The bridge method
/// itself, including its `invokespecial` of the accessor, is emitted with the other access bridges.
pub(super) fn emit_private_member_extension_call(
    cw: &mut ClassWriter,
    code: &mut CodeBuilder,
    call: &SelectedMemberCall<'_>,
) {
    let mut bridge_parameters = Vec::with_capacity(call.parameters.len() + 1);
    bridge_parameters.push(Ty::obj(call.owner));
    bridge_parameters.extend(call.parameters.iter().copied());
    let descriptor = method_descriptor(&bridge_parameters, call.result);
    let name = format!("access${}", call.name);
    let method = if call.interface_owner {
        cw.interface_methodref(call.owner, &name, &descriptor)
    } else {
        cw.methodref(call.owner, &name, &descriptor)
    };
    code.invokestatic(
        method,
        call.argument_words + 1,
        physical_call_result_words(call.result),
    );
}

/// Emit the Java-8 realization of a Kotlin private member used by a lexically related class.
/// The bridge takes the semantic dispatch receiver as its leading static operand, then forwards to
/// the already-selected declaration. No lookup or overload selection occurs here.
pub(super) fn emit_private_member_access_bridge(
    ir: &IrFile,
    fid: u32,
    owner: &str,
    cw: &mut ClassWriter,
    owner_is_interface: bool,
    line: u32,
) {
    let function = &ir.functions[fid as usize];
    debug_assert!(!function.is_static);
    let parameters = jvm_function_params(ir, fid);
    let result = jvm_declared_ty(&function.ret);
    let target_descriptor = method_descriptor(&parameters, result);
    let mut bridge_parameters = Vec::with_capacity(parameters.len() + 1);
    bridge_parameters.push(Ty::obj(owner));
    bridge_parameters.extend(parameters.iter().copied());
    let bridge_descriptor = method_descriptor(&bridge_parameters, result);
    let bridge_name = format!("access${}", function.name);
    // kotlinc visits a method's name and descriptor before its body.
    cw.reserve_method_name(&bridge_name);
    cw.reserve_descriptor(&bridge_descriptor);
    let mut code = CodeBuilder::new(
        bridge_parameters
            .iter()
            .map(|parameter| slot_words(*parameter))
            .sum(),
    );
    // kotlinc's bridge to a suspend member is itself a suspend call, whose line starts with its
    // receiver; an ordinary bridge's line marks the call.
    let suspend = ir.suspend_funs.contains(&fid);
    if suspend && line != 0 {
        code.mark_line(line);
    }
    code.aload(0);
    let mut slot = 1;
    for parameter in &parameters {
        load(*parameter, slot, &mut code);
        slot += slot_words(*parameter);
    }
    let target = if owner_is_interface {
        cw.interface_methodref(owner, &function.name, &target_descriptor)
    } else {
        cw.methodref(owner, &function.name, &target_descriptor)
    };
    let argument_words = parameters
        .iter()
        .map(|parameter| slot_words(*parameter) as i32)
        .sum();
    if !suspend && line != 0 {
        code.mark_line(line);
    }
    code.invokespecial(target, argument_words, slot_words(result) as i32);
    emit_return(result, &mut code);
    code.ensure_locals(slot);
    code.link();
    cw.add_method(
        if owner_is_interface {
            0x1009 // PUBLIC | STATIC | SYNTHETIC (FINAL is illegal on an interface method)
        } else {
            0x1019 // PUBLIC | STATIC | FINAL | SYNTHETIC
        },
        &bridge_name,
        &bridge_descriptor,
        &code,
    );
    set_bridge_locals(
        ir,
        fid,
        owner,
        &parameters,
        &bridge_name,
        &bridge_descriptor,
        cw,
    );
}
