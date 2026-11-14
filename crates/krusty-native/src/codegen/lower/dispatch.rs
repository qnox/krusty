//! Calls that reach a member through its class: dispatch through the receiver's vtable for a
//! virtual call, and the named class's own implementation for a `super` call.

use super::objects::{any_member, ktype, CheckedProperty, TYPE_OFFSET};
use super::*;
use krusty::types::TypeName;

/// The declaration a `super` call names: its class, its member, and how the frontend selected it.
pub(super) struct SuperTarget<'c> {
    pub(super) owner: TypeName,
    pub(super) name: &'c str,
    pub(super) kind: krusty::ir::IrSuperCallKind,
    pub(super) declaration: Option<krusty::fir::ResolvedFunctionOverrideTarget>,
    /// Checked call-site frame for a dependency super call, including class substitutions.
    pub(super) parameters: Option<Vec<krusty::types::Ty>>,
    pub(super) result: Option<krusty::types::Ty>,
}

/// One authenticated dependency operation that may be consumed by a published source-class slot.
pub(super) struct PublishedDependencyCall<'a> {
    pub(super) target: krusty::fir::ExternalCallableId,
    pub(super) key: krusty::backend::class_tables::module_slots::PlanKey,
    pub(super) slot_identity: krusty::fir::ExternalCallableId,
    pub(super) fact: krusty::backend::BackendCallableFact,
    pub(super) callee: Result<krusty::ir::Callee, &'static str>,
    pub(super) receiver: Option<u32>,
    pub(super) args: &'a [u32],
}

impl<'a, 'b, 'c> BodyLowering<'a, 'b, 'c> {
    /// A checked call to an overridable member of an exported source class must enter the class
    /// table. Its declaration may also have a direct linkable body, but calling that body would
    /// skip the most-derived override in the consumer module.
    pub(super) fn published_dependency_member_call(
        &mut self,
        call: PublishedDependencyCall<'_>,
    ) -> Option<Result<Option<Value>, Unsupported>> {
        use krusty::backend::class_tables::module_slots::{PlanKey, PlannedSignature};
        use krusty::types::SemanticCallableOwner;

        let PublishedDependencyCall {
            target,
            key,
            slot_identity,
            fact,
            callee,
            receiver,
            args,
        } = call;
        if matches!(
            &callee,
            Ok(krusty::ir::Callee::External { defaults, .. }) if !defaults.is_empty()
        ) {
            // The exact dependency default bridge owns this call and dispatches after evaluating
            // the declaring module's defaults.
            return None;
        }
        let Some(receiver) = receiver else {
            return None;
        };
        if !fact.is_member_function()
            || fact.owner_is_interface
            || fact.source_receiver.is_some()
            || fact.member_realization != krusty::libraries::MemberRealization::Dispatch
            || fact.compiler_intrinsic.is_some()
            || fact.direct_c_function.is_some()
        {
            return None;
        }
        let Some(SemanticCallableOwner::Classifier(owner)) = fact.declaration_owner else {
            return None;
        };
        let Some(classifier) = self.file.classifiers.classifier(owner) else {
            return None;
        };
        let Some(identity) = classifier.klib_identity.as_ref() else {
            return None;
        };
        if fact
            .klib_body_class_contract
            .as_ref()
            .is_some_and(|contract| contract.proves_final_class(identity, owner))
        {
            // A final class has no consumer-side override. Keep its existing direct realization.
            return None;
        }
        let Some(record) = self.file.laid_out.get(owner) else {
            // Runtime/Kotlin library classes also carry exact KLIB identities, but no Native
            // source-object layout. They retain their existing runtime or direct realization.
            return None;
        };
        if record.class_identity.as_ref() != Some(identity) {
            return Some(Err(declined!(
                "a published class layout of `{}` with a different identity",
                owner.render()
            )));
        }
        let Some(table) = self.file.slot_plan.table(owner) else {
            return Some(Err(declined!(
                "a published dispatch table for `{}`",
                owner.render()
            )));
        };
        let table = match table {
            Ok(table) => table,
            Err(reason) => {
                return Some(Err(declined!(
                    "a published dispatch table for `{}` ({reason})",
                    owner.render()
                )))
            }
        };
        let Some(&slot) = table.slots.get(&key) else {
            return Some(Err(declined!(
                "the selected overridable member slot of `{}`",
                owner.render()
            )));
        };
        let Some(PlannedSignature::Member {
            parameters: slot_parameters,
            result: slot_result,
        }) = self.file.slot_plan.slot_signature(table, slot)
        else {
            return Some(Err(declined!(
                "the selected member frame of `{}`",
                owner.render()
            )));
        };
        let slot_parameters = slot_parameters.to_vec();
        let callee = match callee {
            Ok(callee) => callee,
            Err(reason) => return Some(Err(Unsupported::from(reason))),
        };
        let selected = krusty::backend::instantiate_dependency_signature(&fact, &callee)
            .map_err(Unsupported::from);
        let selected = match selected {
            Ok(selected) => selected,
            Err(error) => return Some(Err(error)),
        };
        let declared_parameters = fact
            .declared_params
            .as_deref()
            .unwrap_or(fact.params.as_slice());
        let declared_result = fact.declared_ret.unwrap_or(fact.ret);
        let krusty::ir::Callee::External {
            params: checked_parameters,
            ret: checked_result,
            ..
        } = &callee
        else {
            return Some(Err(declined!("a checked external member call")));
        };
        if selected.parameters.as_ref() != checked_parameters.as_slice()
            || selected.result != *checked_result
            || declared_parameters != slot_parameters.as_slice()
            || declared_result != slot_result
        {
            return Some(Err(declined!(
                "a selected call frame that disagrees with the published slot for `{}`",
                owner.render()
            )));
        }
        let checked_result = *checked_result;
        let physical_result = fact.physical_ret;
        let mut physical_parameters = Vec::with_capacity(fact.physical_params.len() + 1);
        physical_parameters.push(Ty::obj_name(owner));
        physical_parameters.extend(fact.physical_params.iter().copied());
        let expected = match self
            .file
            .signature_of(&physical_parameters, physical_result)
        {
            Ok(expected) => expected,
            Err(error) => return Some(Err(error)),
        };
        let mut slot_abi_parameters = Vec::with_capacity(slot_parameters.len() + 1);
        slot_abi_parameters.push(any());
        slot_abi_parameters.extend(slot_parameters.iter().copied());
        let expected_slot = match self.file.signature_of(&slot_abi_parameters, slot_result) {
            Ok(expected) => expected,
            Err(error) => return Some(Err(error)),
        };
        // The base slot can be abstract and therefore need no linked body of its own. The
        // physical signature is authenticated by the provider's frozen frame and the exact
        // declaration slot binding; a consumer override supplies the runtime vtable entry.
        if expected.call_conv != expected_slot.call_conv
            || expected.params != expected_slot.params
            || expected.returns != expected_slot.returns
        {
            return Some(Err(declined!(
                "a selected physical frame that disagrees with slot {slot} of `{}`",
                owner.render()
            )));
        }
        // The key must describe the selected declaration, not merely an equal-name member.
        let key_matches = match key {
            PlanKey::DependencyCallable(selected) => selected == target && slot_identity == target,
            PlanKey::DependencyGetter(selected) => selected == target && selected == slot_identity,
            PlanKey::DependencySetter(selected) => selected == slot_identity,
            _ => false,
        };
        if !key_matches {
            return Some(Err(declined!(
                "a mismatched selected dependency slot identity"
            )));
        }
        Some((|| {
            let object = self.reference(receiver)?;
            let arguments = self.arguments(args, &slot_parameters)?;
            if self.terminated {
                return Ok(None);
            }
            let Some(value) = self.dispatch_through(
                object,
                model::Dispatch::Class(slot),
                &slot_parameters,
                slot_result,
                &arguments,
            )?
            else {
                return Ok(None);
            };
            if physical_result == Ty::Unit {
                if checked_result != Ty::Unit {
                    return Err(declined!(
                        "a Unit member slot with a non-Unit selected result"
                    ));
                }
                return Ok(None);
            }
            self.convert(value, Some(physical_result), checked_result)
        })())
    }

    /// A call of one exact method of a class this file declares. The method is the callee's
    /// function, and the slot is that function's own slot.
    pub(super) fn own_class_member(
        &mut self,
        function: krusty::ir::FunId,
        owner: TypeName,
        receiver: u32,
        args: &[u32],
    ) -> Result<Option<Value>, Unsupported> {
        let class = self.file.ir.class_id_by_name(owner).ok_or_else(|| {
            declined!(
                "a member of `{}` this file does not declare",
                owner.render().replace('/', ".")
            )
        })?;
        let key = super::super::super::classes::function_key(self.file.ir, class, function);
        let Some(slot) = self.file.model.slot(class, &key) else {
            return Err(declined!(
                "a member of `{}` with no slot",
                owner.render().replace('/', ".")
            ));
        };
        let (params, ret) = {
            let declaration = &self.file.ir.functions[function as usize];
            (declaration.params.clone(), declaration.ret)
        };
        let object = self.reference(receiver)?;
        if self.terminated {
            return Ok(None);
        }
        let arguments = self.arguments(args, &params)?;
        if self.terminated {
            return Ok(None);
        }
        self.dispatch(object, slot, &params, ret, &arguments)
    }

    /// A call through the receiver's vtable: `receiver.type.vtable[slot](receiver, args…)`.
    pub(super) fn dispatch(
        &mut self,
        receiver: Value,
        slot: u32,
        params: &[Ty],
        ret: Ty,
        arguments: &[Value],
    ) -> Result<Option<Value>, Unsupported> {
        self.dispatch_through(
            receiver,
            model::Dispatch::Class(slot),
            params,
            ret,
            arguments,
        )
    }

    /// A call through one of the receiver's tables: its vtable by class slot —
    /// `receiver.type.vtable[slot](receiver, args…)` — or, for a module interface member, the
    /// member's stub, which the final link writes from every implementation in the program (see
    /// `native::interface_calls`).
    pub(super) fn dispatch_through(
        &mut self,
        receiver: Value,
        through: model::Dispatch,
        params: &[Ty],
        ret: Ty,
        arguments: &[Value],
    ) -> Result<Option<Value>, Unsupported> {
        self.null_check(receiver)?;
        let mut all_params = vec![any()];
        all_params.extend_from_slice(params);
        let mut all_arguments = vec![receiver];
        all_arguments.extend_from_slice(arguments);
        let slot = match through {
            model::Dispatch::Class(slot) => slot,
            model::Dispatch::Interface(number) => {
                let stub = super::super::super::interface_calls::stub_symbol(
                    self.file.module_name,
                    number,
                );
                let stub = self.file.import(&stub, &all_params, ret)?;
                let func_ref = self.func_ref(stub);
                let call = self.emit_call(func_ref, &all_arguments)?;
                return Ok(self.builder.inst_results(call).first().copied());
            }
        };
        let flags = trusted();
        let ty = self
            .builder
            .ins()
            .load(types::I64, flags, receiver, TYPE_OFFSET);
        let table = self
            .builder
            .ins()
            .load(types::I64, flags, ty, ktype::VTABLE as i32);
        let function = self
            .builder
            .ins()
            .load(types::I64, flags, table, (slot * 8) as i32);
        let signature = self.file.signature_of(&all_params, ret)?;
        let signature = self.builder.import_signature(signature);
        let call = self
            .builder
            .ins()
            .call_indirect(signature, function, &all_arguments);
        // A dispatched call is as able to throw as a direct one, and MORE able to be forgotten:
        // it is the one call this backend emits that does not go through `emit_call`. A `try`
        // whose body invokes a lambda is the common shape, and the exception walked straight out
        // of the `try` until this check existed.
        self.check_pending()?;
        Ok(self.builder.inst_results(call).first().copied())
    }

    /// A call dispatched on the receiver's own type, named by the STATIC type it is made through:
    /// `Callee::Virtual`. The class or interface that declares the member fixes the slot number —
    /// `module_interfaces` gives an interface member one number that means the same thing in
    /// every implementation — so the slot is looked up on the DECLARING classifier and the vtable
    /// it indexes is the receiver's. That is what makes `class C(a: I) : I by a` work: the
    /// forwarder synthesized on `C` knows only `I`.
    pub(super) fn virtual_call(
        &mut self,
        owner: TypeName,
        name: &str,
        params: Option<&(Vec<Ty>, Ty)>,
        target: Option<krusty::ir::IrVirtualTarget>,
        receiver: u32,
        args: &[u32],
    ) -> Result<Option<Value>, Unsupported> {
        let class = self.file.class_of(owner, "a virtual call to a member of")?;
        let ir = self.file.ir;
        let unknown = || {
            format!(
                "a virtual call to an unknown member (`{}.{name}`)",
                owner.render().replace('/', ".")
            )
        };
        let methods = &ir.classes[class as usize].methods;
        // The member the checker selected, by its declaration: the owner's own or one it
        // inherits, whose key every table below its declaring class carries. Only a call that
        // names no module function or property (one a plugin synthesized, or a dependency member
        // the class overrides) is matched among the owner's methods by its signature.
        let fid = match target {
            Some(krusty::ir::IrVirtualTarget::Function(
                krusty::fir::ResolvedFunctionOverrideTarget::Module(callable),
            )) => ir
                .checked_callable_functions
                .get(&callable)
                .copied()
                .ok_or_else(unknown)?,
            Some(
                krusty::ir::IrVirtualTarget::PropertyGetter(
                    krusty::fir::ResolvedPropertyOverrideTarget::Module(property),
                )
                | krusty::ir::IrVirtualTarget::PropertySetter(
                    krusty::fir::ResolvedPropertyOverrideTarget::Module(property),
                ),
            ) => {
                let setter = matches!(target, Some(krusty::ir::IrVirtualTarget::PropertySetter(_)));
                let accessor = match ir.local_property_layouts.get(&property) {
                    Some(krusty::ir::IrLocalPropertyLayout::Member {
                        getter,
                        setter: setter_function,
                        ..
                    }) => {
                        if setter {
                            *setter_function
                        } else {
                            *getter
                        }
                    }
                    Some(krusty::ir::IrLocalPropertyLayout::MemberExtension {
                        getter,
                        setter: setter_function,
                        ..
                    }) => {
                        if setter {
                            *setter_function
                        } else {
                            Some(*getter)
                        }
                    }
                    _ => None,
                };
                accessor.ok_or_else(unknown)?
            }
            _ => {
                let declared = params.map(|(params, _)| params.as_slice());
                methods
                    .iter()
                    .copied()
                    .find(|&fid| {
                        let function = &ir.functions[fid as usize];
                        function.name == name
                            && declared.is_none_or(|params| function.params == params)
                    })
                    .ok_or_else(unknown)?
            }
        };
        let function = &ir.functions[fid as usize];
        if function.dispatch_receiver.is_none() {
            return Err(declined!("a virtual call to the static member `{name}`"));
        }
        let declaring = function
            .dispatch_receiver
            .and_then(|receiver| ir.class_id_by_name(receiver))
            .ok_or_else(unknown)?;
        let key = model::function_key(ir, declaring, fid);
        let Some(slot) = self.file.dispatch_of(class, &key) else {
            return Err(declined!("a virtual call with no dispatch slot (`{name}`)"));
        };
        // The ABI is the DECLARATION's, not the call site's: every override fills this slot with a
        // body compiled to the declaration's carriers, so an argument whose checked type is narrower
        // still crosses as what the slot expects.
        let carried = super::super::super::captures::carried_parameters(ir, fid);
        let ret = function.ret;
        let Some(object) = self.receiver(receiver)? else {
            return Ok(None);
        };
        let arguments = self.arguments(args, &carried)?;
        if self.terminated {
            return Ok(None);
        }
        self.dispatch_through(object, slot, &carried, ret, &arguments)
    }

    /// `super.p` and `super.p = v` — the named class's own realization of a PROPERTY.
    ///
    /// Returns `None` when the class declares no such property, so the caller keeps its decline.
    ///
    /// Nothing here may dispatch. `super.p` is written inside the override of `p`, and reaching the
    /// slot would reach that override — which is the accessor doing the asking. So a source-written
    /// accessor of the named class is called directly, and a default one is the field that class
    /// contributes: a distinct field from the override's, because an overriding `var` declares
    /// storage of its own and `super.b` is the reason a program can tell.
    fn direct_property(
        &mut self,
        class: ClassId,
        target: krusty::fir::PropertyId,
        setter: bool,
        receiver: u32,
        args: &[u32],
    ) -> Result<Option<Value>, Unsupported> {
        let CheckedProperty::Member(property_class, index) = self.checked_property(&target)? else {
            return Err("a `super` access to a top-level property".into());
        };
        if property_class != class {
            return Err("a `super` property target owned by another class".into());
        }
        let name = self.file.ir.classes[class as usize].properties[index]
            .name
            .clone();
        match (setter, args) {
            (false, []) => self.direct_property_read(class, index, receiver),
            (true, [value]) => self
                .direct_property_write(class, index, receiver, *value)
                .map(|()| None),
            _ => Err(declined!(
                "a `super` access to `{name}` with {} operands",
                args.len()
            )),
        }
    }

    fn direct_property_read(
        &mut self,
        class: ClassId,
        index: usize,
        receiver: u32,
    ) -> Result<Option<Value>, Unsupported> {
        let property = self.file.ir.classes[class as usize].properties[index].clone();
        let Some(object) = self.receiver(receiver)? else {
            return Ok(None);
        };
        if let Some(getter) = property.getter {
            let Some(id) = self.file.functions[getter as usize] else {
                return Err(declined!(
                    "a `super` read of the abstract `{}`",
                    property.name
                ));
            };
            let func_ref = self.func_ref(id);
            let call = self.emit_call(func_ref, &[object])?;
            return Ok(self.builder.inst_results(call).first().copied());
        }
        let Some(field) = property.backing_field else {
            return Err(declined!(
                "a `super` read of `{}`, which has neither storage nor a getter",
                property.name
            ));
        };
        let ty = self.file.ir.classes[class as usize].fields[field as usize].ty;
        self.load_field(object, class, field, ty).map(Some)
    }

    fn direct_property_write(
        &mut self,
        class: ClassId,
        index: usize,
        receiver: u32,
        value: u32,
    ) -> Result<(), Unsupported> {
        let property = self.file.ir.classes[class as usize].properties[index].clone();
        // The setter takes the property's own type; a direct store takes the FIELD's, which is the
        // same rule `written_property_ty` states for an ordinary write.
        let target = match (property.setter, property.backing_field) {
            (Some(_), _) => property.ty,
            (None, Some(field)) => self.file.ir.classes[class as usize].fields[field as usize].ty,
            (None, None) => {
                return Err(declined!(
                    "a `super` write of `{}`, which has neither storage nor a setter",
                    property.name
                ));
            }
        };
        let Some(object) = self.receiver(receiver)? else {
            return Ok(());
        };
        let value = self.coerce(value, target)?;
        if self.terminated {
            return Ok(());
        }
        let Some(value) = value else {
            return Err(declined!("a `Unit` value assigned to `{}`", property.name));
        };
        if let Some(setter) = property.setter {
            let Some(id) = self.file.functions[setter as usize] else {
                return Err(declined!(
                    "a `super` write of the abstract `{}`",
                    property.name
                ));
            };
            let func_ref = self.func_ref(id);
            self.emit_call(func_ref, &[object, value])?;
            return Ok(());
        }
        let field = property.backing_field.expect("checked above");
        let offset = self.file.model.layout(class).fields[field as usize].offset as i32;
        self.builder.ins().store(trusted(), value, object, offset);
        Ok(())
    }

    /// A non-virtual call to the named class's own implementation: `super.f()`.
    pub(super) fn direct_call(
        &mut self,
        target: SuperTarget<'_>,
        receiver: u32,
        args: &[u32],
    ) -> Result<Option<Value>, Unsupported> {
        let SuperTarget {
            owner,
            name,
            kind,
            declaration,
            parameters,
            result,
        } = target;
        if super::super::super::intrinsics::is_any(owner) {
            let symbol = match (name, args.len()) {
                ("toString", 0) => "kt_any_to_string",
                ("hashCode", 0) => "kt_any_hash_code",
                ("equals", 1) => "kt_any_equals",
                _ => return Err(declined!("a `super` call to `Any.{name}`")),
            };
            let (params, ret) = any_member(symbol).expect("a kotlin.Any member");
            let mut arguments = vec![self.reference(receiver)?];
            for argument in args {
                arguments.push(self.reference(*argument)?);
            }
            if self.terminated {
                return Ok(None);
            }
            return self.runtime_call(symbol, &params, ret, &arguments);
        }
        if self.file.ir.class_id_by_name(owner).is_none() {
            return self.laid_out_direct_call(
                owner,
                kind,
                declaration,
                parameters.as_deref(),
                result,
                name,
                receiver,
                args,
            );
        }
        let class = self.file.class_of(owner, "a `super` call to a method of")?;
        match kind {
            krusty::ir::IrSuperCallKind::PropertyGetter(property) => {
                return self.direct_property(class, property, false, receiver, args);
            }
            krusty::ir::IrSuperCallKind::PropertySetter(property) => {
                return self.direct_property(class, property, true, receiver, args);
            }
            krusty::ir::IrSuperCallKind::Function => {}
        }
        let ir = self.file.ir;
        let Some(krusty::fir::ResolvedFunctionOverrideTarget::Module(source)) = declaration else {
            return Err(declined!(
                "a `super` call without a source declaration (`{name}`)"
            ));
        };
        let Some(&fid) = ir.checked_callable_functions.get(&source) else {
            return Err(declined!("a `super` call to an unknown method (`{name}`)"));
        };
        let Some(id) = self.file.functions[fid as usize] else {
            return Err(declined!("a `super` call to the abstract method `{name}`"));
        };
        let params = super::super::super::captures::carried_parameters(ir, fid);
        let Some(object) = self.receiver(receiver)? else {
            return Ok(None);
        };
        let mut arguments = vec![object];
        arguments.extend(self.arguments(args, &params)?);
        if self.terminated {
            return Ok(None);
        }
        let func_ref = self.func_ref(id);
        let call = self.emit_call(func_ref, &arguments)?;
        Ok(self.builder.inst_results(call).first().copied())
    }

    /// `super.f()` to a method of a superclass an earlier file laid out, through the symbol its
    /// record names.
    fn laid_out_direct_call(
        &mut self,
        owner: TypeName,
        kind: krusty::ir::IrSuperCallKind,
        declaration: Option<krusty::fir::ResolvedFunctionOverrideTarget>,
        selected_parameters: Option<&[Ty]>,
        selected_result: Option<Ty>,
        name: &str,
        receiver: u32,
        args: &[u32],
    ) -> Result<Option<Value>, Unsupported> {
        let outside = || {
            declined!(
                "a `super` call to a method of `{}`, which is not declared in this file",
                owner.render()
            )
        };
        if kind != krusty::ir::IrSuperCallKind::Function {
            return Err(outside());
        }
        let record = self.file.laid_out.get(owner).ok_or_else(|| outside())?;
        let (function, params, _) = match declaration {
            Some(krusty::fir::ResolvedFunctionOverrideTarget::Module(callable)) => {
                record.methods.get(&callable).cloned().ok_or_else(|| {
                    declined!(
                        "a `super` call to `{name}`, which the file declaring it does not export"
                    )
                })?
            }
            Some(krusty::fir::ResolvedFunctionOverrideTarget::External(target)) => {
                let fact = self.file.callables.callable(target).ok_or_else(outside)?;
                if !fact.is_member_function()
                    || fact.declaration_owner
                        != Some(krusty::types::SemanticCallableOwner::Classifier(owner))
                {
                    return Err(outside());
                }
                let identity = self
                    .file
                    .callables
                    .declaration_identity(target)
                    .ok_or_else(outside)?;
                let function = record
                    .published_methods
                    .get(identity)
                    .cloned()
                    .ok_or_else(|| {
                        declined!(
                            "a `super` call to `{name}`, which the file declaring it does not export"
                        )
                    })?;
                let selected_parameters = selected_parameters.ok_or_else(outside)?;
                let selected_result = selected_result.ok_or_else(outside)?;
                let checked = krusty::ir::Callee::External {
                    target,
                    default_provider: None,
                    params: selected_parameters.to_vec(),
                    ret: selected_result,
                    substitutions: Vec::new(),
                    defaults: Vec::new(),
                    extension_receiver_parameter: None,
                };
                // The class application on this selected call is checked FIR data. Reuse the
                // shared dependency signature validator so a Base<Int> call cannot be compared
                // or emitted as if the producer declared Base<Int> rather than Base<T>.
                let instantiated =
                    krusty::backend::instantiate_dependency_signature(fact, &checked)
                        .map_err(Unsupported::from)?;
                if instantiated.parameters.as_ref() != selected_parameters
                    || instantiated.result != selected_result
                {
                    return Err(declined!(
                        "a `super` call to `{name}` whose checked class application disagrees with its declaration"
                    ));
                }
                let parameters = fact
                    .declared_params
                    .as_deref()
                    .unwrap_or(fact.params.as_slice());
                let parameters = parameters.to_vec();
                let physical_parameters = fact.physical_params.clone();
                let physical_result = fact.physical_ret;
                if parameters.len() != selected_parameters.len()
                    || physical_parameters.len() != parameters.len()
                {
                    return Err(declined!(
                        "a `super` call to `{name}` whose declared and physical frames disagree"
                    ));
                }
                let mut expected_parameters = Vec::with_capacity(physical_parameters.len() + 1);
                expected_parameters.push(Ty::obj_name(owner));
                expected_parameters.extend(physical_parameters.iter().copied());
                let expected = self
                    .file
                    .signature_of(&expected_parameters, physical_result)?;
                let ObjectFunction::Linked { signature, .. } = &function else {
                    return Err(outside());
                };
                if signature.call_conv != expected.call_conv
                    || signature.params != expected.params
                    || signature.returns != expected.returns
                {
                    return Err(declined!(
                        "a `super` call to `{name}` whose selected semantic frame differs from the published Native frame"
                    ));
                }
                let id = self.file.import_object_function(&function)?;
                let object = self
                    .coerce(receiver, Ty::obj_name(owner))?
                    .ok_or_else(|| declined!("a `Unit` receiver for a `super` call to `{name}`"))?;
                let mut arguments = vec![object];
                arguments.extend(self.arguments(args, &parameters)?);
                if self.terminated {
                    return Ok(None);
                }
                let func_ref = self.func_ref(id);
                let call = self.emit_call(func_ref, &arguments)?;
                if physical_result == Ty::Unit {
                    if selected_result != Ty::Unit {
                        return Err(declined!(
                            "a `super` call to `{name}` whose selected result disagrees with its physical result"
                        ));
                    }
                    return Ok(None);
                }
                let Some(value) = self.builder.inst_results(call).first().copied() else {
                    return Err(declined!(
                        "a `super` call to `{name}` whose linked method has no result"
                    ));
                };
                return self.convert(value, Some(physical_result), selected_result);
            }
            None => return Err(outside()),
        };
        let id = self.file.import_object_function(&function)?;
        let Some(object) = self.receiver(receiver)? else {
            return Ok(None);
        };
        let mut arguments = vec![object];
        arguments.extend(self.arguments(args, &params)?);
        if self.terminated {
            return Ok(None);
        }
        let func_ref = self.func_ref(id);
        let call = self.emit_call(func_ref, &arguments)?;
        Ok(self.builder.inst_results(call).first().copied())
    }
}
