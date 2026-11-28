//! JVM emission of a virtual member call: the owner and form its realization selected, and the
//! operands, bridges and result narrowing the call site needs.

use krusty::ir::{Callee, IrExpr};
use krusty::jvm::classfile::CodeBuilder;
use krusty::jvm::names::method_descriptor;
use krusty::types::Ty;

use super::super::{
    access_bridges, declared_jvm_interface, jvm_declared_ty, jvm_static_members, jvm_tys,
    parse_descriptor_params, slot_words, ty_from_descriptor_ret,
};

impl super::super::Emitter<'_> {
    /// Emit call `e` to the [`Callee::Virtual`] `callee` on `dispatch_receiver`.
    pub(super) fn emit_virtual_call(
        &mut self,
        e: u32,
        callee: &Callee,
        dispatch_receiver: Option<u32>,
        args: &[u32],
        code: &mut CodeBuilder,
    ) {
        let Callee::Virtual {
            owner,
            name,
            descriptor,
            params,
            interface,
            module_target,
            target,
        } = callee
        else {
            unreachable!("a virtual call emits a virtual callee");
        };
        let recv = dispatch_receiver.expect("virtual call needs a receiver");
        let interface = *interface || declared_jvm_interface(self.ir, *owner);
        // A sibling-file user method carries its signature as `Ty`s (`params`): build the
        // descriptor and emit a plain virtual/interface call. The classpath-operator
        // special-casing below only applies to the `descriptor` form (a classpath receiver).
        if let Some((param_tys, ret_ty)) = params {
            let static_owner = match (module_target, target) {
                (Some(target), _) => {
                    jvm_static_members::module_singleton_static_owner(self.ir, *target)
                }
                (None, Some(target)) => {
                    module_property_accessor(target).and_then(|(property, side)| {
                        jvm_static_members::module_singleton_static_accessor(
                            self.ir, property, side,
                        )
                    })
                }
                (None, None) => None,
            };
            if let Some(owner) = static_owner {
                let call = jvm_static_members::ModuleStaticCall {
                    owner,
                    name,
                    parameters: param_tys,
                    result: ret_ty,
                    receiver: recv,
                    args,
                };
                self.emit_module_singleton_static_call(e, call, code);
                return;
            }
            let owner_identity = *owner;
            // A member an array inherits names the erased JVM array type. That decision
            // reads the receiver's type; the dispatch classifier selects every other owner.
            let Some((owner, interface)) =
                self.source_virtual_call_owner(e, owner_identity, interface, recv)
            else {
                return;
            };
            let mut name = name.clone();
            let ptys = jvm_tys(param_tys);
            let ret = self.physical_call_result(e, jvm_declared_ty(ret_ty));
            let mut descriptor = method_descriptor(&ptys, ret);
            let mut ops = vec![recv];
            ops.extend(args.iter().copied());
            // The receiver is already of the class the call names; only the arguments
            // are materialized at the parameter types.
            let bridge = self
                .run
                .protected_member_access_bridges
                .borrow()
                .get(&e)
                .cloned();
            let ordinary_virtual = access_bridges::select_member_invocation(
                self.ir,
                self.run,
                self.static_owner,
                e,
                owner_identity,
                bridge.is_some(),
                self.export_private_calls,
            ) == access_bridges::MemberInvocation::Virtual;
            let result_narrow = ordinary_virtual
                .then(|| self.ir.jvm_overridden_call_realizations.get(&e))
                .flatten()
                .and_then(|realization| {
                    let narrow = (realization.descriptor != descriptor && ret.is_reference())
                        .then(|| krusty::jvm::names::instanceof_internal_name(ret));
                    name = realization.physical_name.clone();
                    descriptor = realization.descriptor.clone();
                    narrow
                });
            // A realized overridden signature takes its own parameters: an `Int`
            // argument of `remove(element: Int)` is boxed for `remove(Object)`.
            let realized_parameters = ordinary_virtual
                .then(|| self.ir.jvm_overridden_call_realizations.get(&e))
                .flatten()
                .and_then(|realization| parse_descriptor_params(&realization.descriptor));
            let realized = realized_parameters.is_some();
            let ptys = realized_parameters.unwrap_or(ptys);
            let call_parameters = bridge.as_ref().map_or(ptys.as_slice(), |bridge| {
                bridge.bridge_parameters.as_slice()
            });
            // A receiver read from a generic declaration's erased result narrows once,
            // to the class the call names, as kotlinc's does: an upcast to the
            // member's declaring class between them would leave the slot unnarrowed.
            let erased_receiver = bridge
                .is_none()
                .then(|| self.erased_dispatch_receiver(recv))
                .flatten()
                .filter(|_| !owner.starts_with('['));
            if let Some(erased) = erased_receiver {
                ops[0] = erased;
            }
            let mut physical = vec![bridge.as_ref().map_or_else(
                || match erased_receiver {
                    Some(_) => Ty::obj(&owner),
                    None => self.value_ty(recv),
                },
                |bridge| Ty::obj_name(bridge.owner),
            )];
            physical.extend(call_parameters.iter().copied());
            let operand_result = if bridge.is_some() {
                self.emit_source_access_bridge_operands(&ops, &physical, code)
            } else if realized {
                // The realized parameters are a physical signature of their own: an
                // `Int` argument is boxed for `remove(Object)`, not only re-cast.
                self.emit_call_descriptor_operands(e, 1, &ops, &physical, code)
            } else {
                self.emit_source_call_operands(e, 1, &ops, &physical, code)
            };
            if let Err(mismatch) = operand_result {
                self.bail_descriptor_arity(&mismatch, ret, code);
                return;
            }
            let aw: i32 = ptys.iter().map(|t| slot_words(*t) as i32).sum();
            self.mark_call_start(e, code);
            let call = access_bridges::SelectedMemberCall {
                expression: e,
                owner_identity,
                owner: &owner,
                name: &name,
                descriptor: &descriptor,
                parameters: &ptys,
                result: ret,
                interface_owner: interface,
                argument_words: aw,
                protected: bridge.as_ref(),
                export_private_calls: self.export_private_calls,
            };
            // The private member is not a function of this file, so it has no bridge
            // id here. The declaring class still published `access$<name>(Owner)`.
            if access_bridges::private_module_callable(self.ir, *module_target) {
                access_bridges::emit_private_member_extension_call(self.cw, code, &call);
                return;
            }
            access_bridges::emit_selected_member_call(
                self.ir,
                self.run,
                self.static_owner,
                self.cw,
                code,
                &call,
            );
            if let Some(internal) = result_narrow {
                let class = self.cw.class_ref(&internal);
                code.checkcast(class);
            }
            return;
        }
        let owner_identity = *owner;
        let (owner, mut name, mut descriptor) = (
            owner_identity.render(),
            name.clone(),
            self.physical_call_descriptor(e, descriptor),
        );
        let declared_descriptor = descriptor.clone();
        let protected_bridge = self
            .run
            .protected_member_access_bridges
            .borrow()
            .get(&e)
            .cloned();
        // An overridden declaration's realization can erase the result the selected
        // declaration names (`get(Ljava/lang/Object;)Ljava/lang/Object;` for an
        // override returning `Int?`); the call then narrows it back, as kotlinc does.
        let mut realized_result = false;
        if protected_bridge.is_none() {
            if let Some(realization) = self.ir.jvm_overridden_call_realizations.get(&e) {
                name = realization.physical_name.clone();
                realized_result = realization.descriptor != descriptor;
                descriptor = realization.descriptor.clone();
            }
        }
        if self.emit_primitive_inc_dec_virtual(&owner, &name, &descriptor, recv, args, code) {
            return;
        }
        // A `@JvmStatic` member of an `object`/companion (`Dispatchers.IO`): an ordinary
        // member call in the language — resolved and lowered with a receiver — that kotlinc
        // emits as a static taking none. Drop the receiver and `invokestatic`. The receiver is
        // still EVALUATED when it can have an effect; kotlinc emits the same `…; pop;
        // invokestatic` and elides a bare singleton/local read entirely.
        if self.bodies.method_is_static(&owner, &name, &descriptor)
            && parse_descriptor_params(&descriptor).is_some_and(|params| params.len() == args.len())
        {
            // The receiver is dropped, not skipped: it is still an expression the source
            // program evaluates. Elide it only when it can run no code, or when it merely
            // reads a static of the very class this `invokestatic` initializes anyway (the
            // `Obj.INSTANCE` singleton read) — which is exactly what kotlinc elides.
            let initializes_owner = matches!(
                self.ir.expr(recv),
                IrExpr::ExternalStaticField { owner: field_owner, .. }
                    if field_owner.matches(&owner)
            );
            if !krusty::ir::expr_runs_no_code(self.ir, recv) && !initializes_owner {
                self.emit_value(recv, code);
                code.pop();
            }
            let physical_params = parse_descriptor_params(&descriptor)
                .expect("static method descriptor must be valid");
            let ret = ty_from_descriptor_ret(&descriptor);
            if let Err(mismatch) =
                self.emit_call_descriptor_operands(e, 0, args, &physical_params, code)
            {
                self.bail_descriptor_arity(&mismatch, ret, code);
                return;
            }
            let aw: i32 = physical_params.iter().map(|t| slot_words(*t) as i32).sum();
            let m = self.cw.methodref(&owner, &name, &descriptor);
            self.mark_call_start(e, code);
            code.invokestatic(m, aw, slot_words(ret) as i32);
            return;
        }
        if parse_descriptor_params(&descriptor).is_some_and(|params| params.len() == args.len() + 1)
        {
            let mut physical_args = Vec::with_capacity(args.len() + 1);
            physical_args.push(recv);
            physical_args.extend(args.iter().copied());
            let physical_params = parse_descriptor_params(&descriptor)
                .expect("static extension descriptor must be valid");
            let ret = ty_from_descriptor_ret(&descriptor);
            if let Err(mismatch) =
                self.emit_call_descriptor_operands(e, 1, &physical_args, &physical_params, code)
            {
                self.bail_descriptor_arity(&mismatch, ret, code);
                return;
            }
            let aw: i32 = physical_params.iter().map(|t| slot_words(*t) as i32).sum();
            let m = self.cw.methodref(&owner, &name, &descriptor);
            self.mark_call_start(e, code);
            code.invokestatic(m, aw, slot_words(ret) as i32);
            return;
        }
        let physical_params =
            parse_descriptor_params(&descriptor).expect("virtual call descriptor must be valid");
        let call_parameters = protected_bridge
            .as_ref()
            .map_or(physical_params.as_slice(), |bridge| {
                bridge.bridge_parameters.as_slice()
            });
        krusty::trace_compiler!(
            "emit",
            "virtual {owner}.{name}{descriptor} receiver={recv} {:?} receiver_ty={:?} args={args:?}",
            self.ir.expr(recv),
            self.value_ty(recv),
        );
        let ret = ty_from_descriptor_ret(&descriptor);
        let operand_result = if let Some(bridge) = protected_bridge.as_ref() {
            let mut operands = Vec::with_capacity(args.len() + 1);
            operands.push(recv);
            operands.extend(args.iter().copied());
            let mut physical = Vec::with_capacity(call_parameters.len() + 1);
            physical.push(Ty::obj_name(bridge.owner));
            physical.extend(call_parameters.iter().copied());
            self.emit_source_access_bridge_operands(&operands, &physical, code)
        } else {
            self.emit_descriptor_virtual_operands(
                e,
                crate::ir_emit::call_operands::VirtualCallTarget {
                    owner: &owner,
                    name: &name,
                    descriptor: &descriptor,
                },
                recv,
                args,
                call_parameters,
                code,
            )
        };
        if let Err(mismatch) = operand_result {
            self.bail_descriptor_arity(&mismatch, ret, code);
            return;
        }
        let aw: i32 = call_parameters.iter().map(|t| slot_words(*t) as i32).sum();
        if let Some(bridge) = protected_bridge {
            let mut bridge_params = Vec::with_capacity(bridge.bridge_parameters.len() + 1);
            bridge_params.push(Ty::obj_name(bridge.owner));
            bridge_params.extend(bridge.bridge_parameters.iter().copied());
            let bridge_descriptor = method_descriptor(&bridge_params, ret);
            let bridge_name = format!("access${name}");
            let method =
                self.cw
                    .methodref(&bridge.owner.render(), &bridge_name, &bridge_descriptor);
            self.mark_call_start(e, code);
            code.invokestatic(method, aw + 1, slot_words(ret) as i32);
        } else if interface {
            let m = self.cw.interface_methodref(&owner, &name, &descriptor);
            self.mark_call_start(e, code);
            code.invokeinterface(m, aw, slot_words(ret) as i32);
        } else {
            let m = self.cw.methodref(&owner, &name, &descriptor);
            self.mark_call_start(e, code);
            code.invokevirtual(m, aw, slot_words(ret) as i32);
        }
        let widened_ret = ty_from_descriptor_ret(&descriptor);
        let declared_ret = ty_from_descriptor_ret(&declared_descriptor);
        if declared_ret != widened_ret
            && declared_ret.is_reference()
            && (realized_result
                || !crate::ir_emit::declaration_types::jvm_is_erased_top(widened_ret))
        {
            let internal = krusty::jvm::names::instanceof_internal_name(declared_ret);
            let class = self.cw.class_ref(&internal);
            code.checkcast(class);
        }
    }
}

/// The sibling-file property and accessor side a virtual accessor call selects.
fn module_property_accessor(
    target: &krusty::ir::IrVirtualTarget,
) -> Option<(krusty::fir::PropertyId, super::super::PropertyAccessorSide)> {
    use krusty::fir::ResolvedPropertyOverrideTarget::Module;
    use krusty::ir::IrVirtualTarget::{PropertyGetter, PropertySetter};
    match target {
        PropertyGetter(Module(property)) => {
            Some((*property, super::super::PropertyAccessorSide::Getter))
        }
        PropertySetter(Module(property)) => {
            Some((*property, super::super::PropertyAccessorSide::Setter))
        }
        _ => None,
    }
}
