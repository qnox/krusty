//! Dispatch for dependency members whose receiver may be implemented by a class in this file.
//!
//! Runtime entry points only understand runtime-owned objects. These operations test each known
//! source implementor once and enter its own slot before falling through to the runtime shape.

use super::*;

/// One interface-member call: the receiver, the arguments, the answer the site asked for, and the
/// member's realized parameter types. Those four travel together from the call site into the
/// dispatch.
pub(super) struct InterfaceMemberCall<'a> {
    pub(super) receiver: u32,
    pub(super) args: &'a [u32],
    pub(super) ret: Ty,
    pub(super) physical: &'a [Ty],
}

/// A class of this file that implements a dependency declaration, and the slot it does it in.
/// A class of another file of the module is an arm too: an arm names its class by descriptor.
pub(super) struct ImplementorArm {
    descriptor: DataId,
    slot: u32,
    params: Vec<ParameterBoundary>,
    ret: Ty,
}

/// One parameter of an implementor's arm, at both of its types. The call site hands the operand to
/// the DEPENDENCY declaration, whose parameter may be a type parameter the implementor fixes:
/// `Comparable<II>.compareTo` takes a `T`, carried as a reference, where `II.compareTo` takes the
/// `II` value. The operand crosses that declared boundary first and becomes the implementation's
/// type from there, as it would through the bridge a virtual call reaches.
pub(super) struct ParameterBoundary {
    pub(super) declared: Ty,
    pub(super) implementation: Ty,
}

impl<'a, 'b, 'c> BodyLowering<'a, 'b, 'c> {
    /// The slot that implements one exact dependency declaration in `class`, each parameter paired
    /// with `declared`, the parameter types the dependency declaration states.
    ///
    /// Common IR already joined an override to its provider-owned callable identity. Consuming that
    /// edge keeps same-name/same-arity overloads distinct and avoids repeating overload selection in
    /// the backend.
    pub(super) fn external_override_slot(
        &self,
        class: ClassId,
        overridden: krusty::fir::ExternalCallableId,
        declared: &[Ty],
    ) -> Option<ImplementorArm> {
        let ir = self.file.ir;
        // The class's own override, else the one it inherits from its nearest superclass in this
        // file that has one, as a getter's is found.
        let target = krusty::fir::ResolvedFunctionOverrideTarget::External(overridden);
        let mut at = Some(class);
        let edge = loop {
            let id = at?;
            if let Some(edge) = ir
                .function_overrides
                .get(&ir.classes[id as usize].fq_name)
                .into_iter()
                .flatten()
                .find(|edge| edge.overridden == target)
            {
                break edge;
            }
            at = self.file.model.layout(id).superclass;
        };
        let function = edge.implementation_function.or_else(|| {
            let krusty::fir::ResolvedFunctionOverrideTarget::Module(callable) = edge.implementation
            else {
                return None;
            };
            ir.checked_callable_functions.get(&callable).copied()
        })?;
        let owner = ir.class_id_by_name(edge.implementation_owner)?;
        let key = super::super::super::classes::function_key(ir, owner, function);
        let slot = self.file.model.slot(class, &key)?;
        let declaration = &ir.functions[function as usize];
        // An override takes the parameters of what it overrides, so a count that differs is a
        // declaration this pairing does not describe.
        if declaration.params.len() != declared.len() {
            return None;
        }
        let params = declared
            .iter()
            .zip(&declaration.params)
            .map(|(&declared, &implementation)| ParameterBoundary {
                declared,
                implementation,
            })
            .collect();
        Some(ImplementorArm {
            descriptor: self.file.classes[class as usize].descriptor,
            slot,
            params,
            ret: declaration.ret,
        })
    }

    /// The slot of `class`'s getter overriding the dependency property whose getter is
    /// `overridden`: the class registers the overridden declaration's identity at the slot of its
    /// own accessor, which answers the override's own type; the call site converts it to the one
    /// the declaration states.
    pub(super) fn external_property_getter_slot(
        &self,
        class: ClassId,
        overridden: krusty::fir::ExternalCallableId,
    ) -> Option<ImplementorArm> {
        let target = krusty::fir::ResolvedPropertyOverrideTarget::External(overridden);
        let ir = self.file.ir;
        let mut at = Some(class);
        let implementation = loop {
            let id = at?;
            if let Some(edge) = ir
                .property_overrides
                .get(&ir.classes[id as usize].fq_name)
                .into_iter()
                .flatten()
                .find(|edge| edge.overridden == target)
            {
                break edge.implementation_type;
            }
            at = self.file.model.layout(id).superclass;
        };
        let slot = self
            .file
            .model
            .slot(class, &crate::classes::SlotKey::Getter(target))?;
        Some(ImplementorArm {
            descriptor: self.file.classes[class as usize].descriptor,
            slot,
            params: Vec::new(),
            ret: implementation,
        })
    }

    /// The slot of `class`, a class another file of the module declares, that holds the member
    /// realizing the dependency property getter `overridden`, answering `declared`: an override
    /// or a delegation forwarder, at the slot the module plan numbers for every file alike. `None`
    /// where the plan has no table for the class, no slot for the member, or a slot whose carrier
    /// is not the declaration's.
    pub(super) fn planned_property_getter_slot(
        &mut self,
        class: krusty::types::TypeName,
        overridden: krusty::fir::ExternalCallableId,
        declared: Ty,
    ) -> Result<Option<ImplementorArm>, Unsupported> {
        let plan = self.file.slot_plan;
        let Some(Ok(table)) = plan.table(class) else {
            return Ok(None);
        };
        let Some(key) = self
            .file
            .module_facts
            .classes()
            .dependency_getter_implementation(class, overridden)
        else {
            return Ok(None);
        };
        let Some(&slot) = table.slots.get(&key) else {
            return Ok(None);
        };
        let Some(krusty::backend::class_tables::module_slots::PlannedSignature::Member {
            parameters: [],
            result,
        }) = plan.slot_signature(table, slot)
        else {
            return Ok(None);
        };
        if self.carrier(result) != self.carrier(declared) {
            return Ok(None);
        }
        let Some(descriptor) = self.file.type_descriptor(Ty::obj_name(class))? else {
            return Ok(None);
        };
        Ok(Some(ImplementorArm {
            descriptor,
            slot,
            params: Vec::new(),
            ret: result,
        }))
    }

    /// A call of a dependency interface member this file does not implement. The link joins the
    /// stub to the implementation a dependency object recorded under the same identity. An
    /// object the runtime makes records nothing, so one whose class implements the interface is
    /// tested for first and handed to the runtime function realizing the member on it.
    pub(super) fn dependency_interface_stub(
        &mut self,
        identity: Vec<u8>,
        signature: super::super::super::intrinsics::FunctionSignature<'_>,
        receiver: u32,
        args: &[u32],
        params: &[Ty],
        ret: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        let (number, declared_member_params) =
            self.interface_stub_number(identity, &signature, params)?;
        let runtime = self.runtime_implementations(signature)?;
        let Some(object) = self.receiver(receiver)? else {
            return Ok(None);
        };
        let arguments = self.arguments(args, params)?;
        if self.terminated {
            return Ok(None);
        }
        let applied_operands: Vec<(Value, Option<Ty>)> = arguments
            .iter()
            .zip(params)
            .map(|(&value, &param)| (value, Some(param)))
            .collect();
        // Runtime and linked implementations receive the same declared interface frame.
        let Some(crossed) =
            self.dependency_interface_arguments(&applied_operands, &declared_member_params)?
        else {
            return Ok(None);
        };
        let operands: Vec<(Value, Option<Ty>)> = crossed
            .iter()
            .zip(&declared_member_params)
            .map(|(&value, &param)| (value, Some(param)))
            .collect();
        let declared_ret = signature.ret;
        let produced = self.dispatch_runtime_implementations(
            &runtime,
            object,
            &operands,
            declared_ret,
            |body| {
                body.dispatch_through(
                    object,
                    model::Dispatch::Interface(number),
                    &declared_member_params,
                    declared_ret,
                    &crossed,
                )
            },
        )?;
        self.dependency_interface_result(produced, declared_ret, ret)
    }

    /// Call the link stub `number` with `operands`. The stub takes the declaration's own ABI,
    /// whatever types the call applies: an applied primitive or value class crosses into the
    /// declared (erased) parameter and the declared result back into the applied one, as through
    /// a bridge.
    fn dispatch_through_stub(
        &mut self,
        object: Value,
        number: u32,
        operands: &[(Value, Option<Ty>)],
        declared_params: &[Ty],
        declared_ret: Ty,
        ret: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        let Some(declared_arguments) =
            self.dependency_interface_arguments(operands, declared_params)?
        else {
            return Ok(None);
        };
        let produced = self.dispatch_through(
            object,
            model::Dispatch::Interface(number),
            declared_params,
            declared_ret,
            &declared_arguments,
        )?;
        self.dependency_interface_result(produced, declared_ret, ret)
    }

    /// Cross each applied operand into the interface declaration's frame before dispatch.
    fn dependency_interface_arguments(
        &mut self,
        operands: &[(Value, Option<Ty>)],
        declared_params: &[Ty],
    ) -> Result<Option<Vec<Value>>, Unsupported> {
        let mut declared_arguments = Vec::with_capacity(operands.len());
        for (&(value, applied), &declared) in operands.iter().zip(declared_params) {
            let Some(converted) = self.convert(value, applied, declared)? else {
                return Ok(None);
            };
            declared_arguments.push(converted);
        }
        Ok(Some(declared_arguments))
    }

    /// Cross the merged declared result back to the site's applied result, including Unit.
    fn dependency_interface_result(
        &mut self,
        produced: Option<Value>,
        declared_ret: Ty,
        ret: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        if self.terminated {
            return Ok(None);
        }
        match produced {
            Some(value) => self.convert(value, Some(declared_ret), ret),
            None => self.unit_where_wanted(ret),
        }
    }

    /// The number of the link stub for the dependency interface member `identity`, called with
    /// `params`, and the parameters the member declares, which the stub takes.
    fn interface_stub_number(
        &mut self,
        identity: Vec<u8>,
        signature: &super::super::super::intrinsics::FunctionSignature<'_>,
        params: &[Ty],
    ) -> Result<(u32, Vec<Ty>), Unsupported> {
        let mut declared_member_params = signature.params.to_vec();
        if let Some(receiver) = signature.receiver {
            if signature.context_count > declared_member_params.len() {
                return Err(declined!(
                    "the dependency interface member `{}` has malformed context parameters",
                    signature.name
                ));
            }
            declared_member_params.insert(signature.context_count, receiver);
        }
        if declared_member_params.len() != params.len() {
            return Err(declined!(
                "the dependency interface member `{}` is applied to {} parameters but declares {}",
                signature.name,
                params.len(),
                declared_member_params.len()
            ));
        }
        if let Some(reason) = self.file.unimplementable_interface_members.get(&identity) {
            return Err(declined!("{reason}"));
        }
        Ok((
            self.file.dependency_interface_number(identity),
            declared_member_params,
        ))
    }

    /// A read of a dependency interface's abstract property this file finds no other realization
    /// for (`Continuation.context` of a continuation another file or module made): its getter's
    /// stub, which the link joins to the getter each implementing class recorded under the
    /// property's identity, as a function member's stub is joined.
    pub(super) fn dependency_interface_getter(
        &mut self,
        operation: &IrCheckedOperation,
    ) -> Option<Result<Option<Value>, Unsupported>> {
        let access = self.file.callables.dependency_accessor(operation)?;
        let realization = access.callable;
        if access.writes()
            || !access.dispatches()
            || !access.substitutions().is_empty()
            || !realization.owner_is_interface
            || realization.kind != krusty::libraries::ExternalCallableKind::Member
            || realization.member_realization != krusty::libraries::MemberRealization::Dispatch
            || realization.compiler_operation().is_some()
        {
            return None;
        }
        let receiver = access.receiver?;
        let accessor = realization.dependency_callable.as_ref()?;
        let identity =
            krusty::backend::interface_identity::property_member_identity(accessor, false)?;
        let IrCheckedOperation::ExternalPropertyRead { result, .. } = operation else {
            return None;
        };
        let owner = super::super::super::intrinsics::DeclarationOwner::callable(
            realization.physical_owner,
            realization.declaration_owner,
        );
        let name = access.property.name.to_string();
        let declared_ret = realization.declared_ret.unwrap_or(realization.ret);
        let signature = super::super::super::intrinsics::FunctionSignature::new(
            owner,
            &name,
            &[],
            declared_ret,
        )
        .with_overridden(&realization.overridden_declarations);
        Some(self.dependency_interface_stub(identity, signature, receiver, &[], &[], *result))
    }

    /// Hand `object` to the runtime function of the first of `runtime`'s classes it is an instance
    /// of, with `operands` (each value and its physical type), answering `answer`; an object of
    /// none of them is `otherwise`'s.
    fn dispatch_runtime_implementations(
        &mut self,
        runtime: &[(&'static str, &'static str)],
        object: Value,
        operands: &[(Value, Option<Ty>)],
        answer: Ty,
        otherwise: impl FnOnce(&mut Self) -> Result<Option<Value>, Unsupported>,
    ) -> Result<Option<Value>, Unsupported> {
        if runtime.is_empty() {
            return otherwise(self);
        }
        self.null_check(object)?;
        let merge = self.builder.create_block();
        let carried = self.carrier(answer);
        if let Some(clif) = carried.clif() {
            self.builder.append_block_param(merge, clif);
        }
        for &(descriptor, function) in runtime {
            let descriptor = self.file.import_data(descriptor)?;
            let type_address = self.data_address(descriptor);
            let matches = self
                .runtime_call(
                    "kt_is_instance",
                    &[any(), any()],
                    Ty::Boolean,
                    &[object, type_address],
                )?
                .expect("`kt_is_instance` returns a Boolean");
            let made = self.builder.create_block();
            let rest = self.builder.create_block();
            self.builder.ins().brif(matches, made, &[], rest, &[]);

            self.continue_in(made);
            self.builder.seal_block(made);
            // A runtime member takes its receiver and every operand as a reference.
            let mut references = vec![object];
            for &(value, source) in operands {
                let Some(reference) = self.convert(value, source, any())? else {
                    return Ok(None);
                };
                references.push(reference);
            }
            let produced =
                self.runtime_call(function, &vec![any(); references.len()], any(), &references)?;
            let produced = match produced {
                Some(value) if !self.terminated => self.convert(value, Some(any()), answer)?,
                _ => None,
            };
            self.jump_to_merge(merge, carried, produced);
            self.continue_in(rest);
            self.builder.seal_block(rest);
        }
        let produced = otherwise(self)?;
        self.jump_to_merge(merge, carried, produced);

        self.builder.switch_to_block(merge);
        self.builder.seal_block(merge);
        Ok(carried.clif().map(|_| self.builder.block_params(merge)[0]))
    }

    /// Each class the runtime makes that implements the interface declaring `signature`, as the
    /// descriptor its instances carry and the runtime function realizing the member on one. A
    /// class implementing it with no such function declines: a call through the interface could
    /// reach an instance nothing implements the member of.
    fn runtime_implementations(
        &self,
        signature: super::super::super::intrinsics::FunctionSignature<'_>,
    ) -> Result<Vec<(&'static str, &'static str)>, Unsupported> {
        let Some(interface) = signature.owner.semantic_classifier() else {
            return Ok(Vec::new());
        };
        let mut implementations = Vec::new();
        for (class, descriptor) in super::classifier_shapes::runtime_member_classes() {
            if !self.file.is_subclassifier(class, interface) {
                continue;
            }
            let function =
                super::super::super::intrinsics::runtime_implementation(class, signature)
                    .ok_or_else(|| {
                        declined!(
                            "the member `{}.{}` of the runtime's `{}`",
                            interface.render().replace('/', "."),
                            signature.name,
                            class.render().replace('/', ".")
                        )
                    })?;
            implementations.push((descriptor, function));
        }
        Ok(implementations)
    }

    /// Whether `internal`'s member stays with the runtime, rather than a direct implementor call.
    ///
    /// A closed range is worn by objects the runtime builds, and nothing in this file is one of
    /// them. A collection is worn by both, so its member is dispatched below: each class of this
    /// file, then the runtime function that realizes the member on a runtime collection. A special
    /// bridge whose answer is `Boolean` or `Int` (`contains`, `remove`, `indexOf`) is dispatched
    /// the same way, and an argument that is not the override's element answers the bridge's
    /// rejection instead of being cast. A bridge with any other answer, such as `Map.get`, stays
    /// with the runtime.
    fn runtime_owns_member(
        &self,
        internal: krusty::types::TypeName,
        signature: super::super::super::intrinsics::FunctionSignature<'_>,
    ) -> bool {
        if super::classifier_shapes::is_closed_range(internal)
            || super::classifier_shapes::is_closed_floating_range(internal)
        {
            return true;
        }
        let ty = Ty::obj_name(internal);
        let bridge = self
            .file
            .mapped_collection(ty)
            .is_some_and(|collection| collection.has_special_bridge(signature.name));
        bridge && !matches!(signature.ret, Ty::Boolean | Ty::Int)
    }

    /// A dependency member called on a receiver whose static type is a class of this file that
    /// overrides it, such as the forwarder a `by` delegation generates for an inherited
    /// `Comparable.compareTo`. The class's slot holds the member for it and every subclass, so the
    /// call dispatches there, as a call through the class's own declaration would.
    pub(super) fn source_class_member(
        &mut self,
        target: krusty::fir::ExternalCallableId,
        signature: super::super::super::intrinsics::FunctionSignature<'_>,
        call: InterfaceMemberCall<'_>,
    ) -> Option<Result<Option<Value>, Unsupported>> {
        let internal = self
            .checked_type(call.receiver)?
            .non_null()
            .obj_internal()?;
        let class = self
            .file
            .ir
            .class_id_by_name(internal)
            .filter(|&class| !self.file.ir.classes[class as usize].is_interface)?;
        let arm = self.external_override_slot(class, target, signature.params)?;
        Some(self.dependency_interface_dispatch(&[arm], &[], call, false, None, None))
    }

    /// Dispatch one dependency interface member by the exact override edge each implementor
    /// published. An implementor without that edge in this file is reached through the member's
    /// link stub; with no stub, the call is left to the paths that already know the member. This
    /// does not recover an implementation by its name. A member the runtime also answers is left
    /// there too.
    ///
    /// `stub` is the member's KLIB identity and the parameters a call applies to it. An object
    /// another file or module made, which none of these classes is, reaches the member through its
    /// link stub, as a call this file does not implement does.
    pub(super) fn dependency_interface_member(
        &mut self,
        internal: krusty::types::TypeName,
        target: krusty::fir::ExternalCallableId,
        signature: super::super::super::intrinsics::FunctionSignature<'_>,
        call: InterfaceMemberCall<'_>,
        stub: Option<(Vec<u8>, &[Ty])>,
    ) -> Option<Result<Option<Value>, Unsupported>> {
        if self.runtime_owns_member(internal, signature) {
            return None;
        }
        let declared = self.file.implementors_of(internal);
        // With no class of its own behind the type (a dependency body realized apart from the
        // classes that inherit it), the interface stub still reaches every implementation record.
        if declared.is_empty() && stub.is_none() {
            return None;
        }
        let implementors: Vec<ImplementorArm> = declared
            .iter()
            .filter_map(|&class| self.external_override_slot(class, target, signature.params))
            .collect();
        // A class whose implementation it inherits from a superclass another unit laid out has
        // no arm of its own; the interface stub its table holds by the member's identity is the
        // only path that reaches it.
        if stub.is_none() && implementors.len() != declared.len() {
            return None;
        }
        let stub = match stub
            .map(|(identity, params)| self.interface_stub_number(identity, &signature, params))
            .transpose()
        {
            Ok(stub) => stub,
            Err(declined) => return Some(Err(declined)),
        };
        let runtime = match self.runtime_implementations(signature) {
            Ok(runtime) => runtime,
            Err(declined) => return Some(Err(declined)),
        };
        let bridge = self
            .file
            .mapped_collection(Ty::obj_name(internal))
            .is_some_and(|collection| collection.has_special_bridge(signature.name));
        // A runtime collection is not one of the classes above. The member's own runtime function
        // is the remaining arm; an abstract-method trap would be a call the program did make.
        let collection = self.checked_type(call.receiver).and_then(|ty| {
            super::lists::collection_runtime_member(self.file, ty, signature, call.physical)
                .filter(|(_, carried, _)| carried.len() == call.args.len() + 1)
        });
        Some(self.dependency_interface_dispatch(
            &implementors,
            &runtime,
            call,
            bridge,
            collection,
            stub.map(|(number, params)| (number, params, signature.ret)),
        ))
    }

    /// Dispatch among the classes of this file implementing a dependency interface member, then
    /// among the classes the runtime makes that implement it.
    ///
    /// `bridge` is a collection special bridge: an argument that is not an instance of the
    /// override's element type answers the rejection (`false`, or `-1` for an index) instead of
    /// being cast into that element type.
    fn dependency_interface_dispatch(
        &mut self,
        implementors: &[ImplementorArm],
        runtime: &[(&'static str, &'static str)],
        call: InterfaceMemberCall<'_>,
        bridge: bool,
        collection: Option<(&'static str, Vec<Ty>, Ty)>,
        stub: Option<(u32, Vec<Ty>, Ty)>,
    ) -> Result<Option<Value>, Unsupported> {
        let object = self.reference(call.receiver)?;
        if self.terminated {
            return Ok(None);
        }
        let mut operands = Vec::with_capacity(call.args.len());
        for argument in call.args {
            let Some(value) = self.expression(*argument)? else {
                return Ok(None);
            };
            if self.terminated {
                return Ok(None);
            }
            operands.push((value, self.physical_type(*argument)));
        }
        let runtime_operands = operands.clone();
        self.dispatch_by_implementor_with(
            implementors,
            object,
            &operands,
            call.ret,
            bridge,
            |body, object| {
                if let Some((symbol, carried, runtime_answer)) = collection {
                    return body.collection_runtime_call(
                        symbol,
                        &carried,
                        runtime_answer,
                        object,
                        &runtime_operands,
                        call.ret,
                    );
                }
                body.dispatch_runtime_implementations(
                    runtime,
                    object,
                    &runtime_operands,
                    call.ret,
                    |body| {
                        let Some((number, params, declared_ret)) = stub else {
                            // No source class and no runtime function realizes this member.
                            body.runtime_call("kt_abstract_method_called", &[], Ty::Unit, &[])?;
                            return Ok(None);
                        };
                        body.dispatch_through_stub(
                            object,
                            number,
                            &runtime_operands,
                            &params,
                            declared_ret,
                            call.ret,
                        )
                    },
                )
            },
        )
    }

    /// Call the runtime function that realizes a collection member, converting each operand to the
    /// carrier that function takes and the result to the type the call site asked for.
    fn collection_runtime_call(
        &mut self,
        symbol: &'static str,
        carried: &[Ty],
        runtime_answer: Ty,
        object: Value,
        operands: &[(Value, Option<Ty>)],
        ret: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        let mut values = Vec::with_capacity(carried.len());
        values.push(object);
        for ((value, source), target) in operands.iter().zip(carried.iter().skip(1)) {
            let Some(converted) = self.convert(*value, *source, *target)? else {
                return Ok(None);
            };
            values.push(converted);
        }
        let Some(produced) = self.runtime_call(symbol, carried, runtime_answer, &values)? else {
            return Ok(None);
        };
        self.convert(produced, Some(runtime_answer), ret)
    }

    pub(super) fn dispatch_by_implementor_with(
        &mut self,
        implementors: &[ImplementorArm],
        object: Value,
        arguments: &[(Value, Option<Ty>)],
        answer: Ty,
        bridge: bool,
        runtime: impl FnOnce(&mut Self, Value) -> Result<Option<Value>, Unsupported>,
    ) -> Result<Option<Value>, Unsupported> {
        let merge = self.builder.create_block();
        let carried = self.carrier(answer);
        if let Some(clif) = carried.clif() {
            self.builder.append_block_param(merge, clif);
        }
        for arm in implementors {
            let type_address = self.data_address(arm.descriptor);
            let matches = self
                .runtime_call(
                    "kt_is_instance",
                    &[any(), any()],
                    Ty::Boolean,
                    &[object, type_address],
                )?
                .expect("`kt_is_instance` returns a Boolean");
            let mine = self.builder.create_block();
            let rest = self.builder.create_block();
            self.builder.ins().brif(matches, mine, &[], rest, &[]);

            self.continue_in(mine);
            self.builder.seal_block(mine);
            if bridge
                && self.bridge_argument_misses(arguments, &arm.params, answer, merge, carried)?
            {
                self.continue_in(rest);
                self.builder.seal_block(rest);
                continue;
            }
            let mut operands = Vec::with_capacity(arguments.len());
            for (&(value, source), boundary) in arguments.iter().zip(&arm.params) {
                let Some(crossing) = self.convert(value, source, boundary.declared)? else {
                    return Ok(None);
                };
                let Some(converted) =
                    self.convert(crossing, Some(boundary.declared), boundary.implementation)?
                else {
                    return Ok(None);
                };
                operands.push(converted);
            }
            let params: Vec<Ty> = arm.params.iter().map(|p| p.implementation).collect();
            let produced = self.dispatch(object, arm.slot, &params, arm.ret, &operands)?;
            let produced = match produced {
                Some(value) => self.convert(value, Some(arm.ret), answer)?,
                None => self.unit_where_wanted(answer)?,
            };
            self.jump_to_merge(merge, carried, produced);
            self.continue_in(rest);
            self.builder.seal_block(rest);
        }
        let produced = runtime(self, object)?;
        self.jump_to_merge(merge, carried, produced);

        self.builder.switch_to_block(merge);
        self.builder.seal_block(merge);
        Ok(carried.clif().map(|_| self.builder.block_params(merge)[0]))
    }

    /// An argument of a collection special bridge that is not an instance of the override's
    /// element. The rejection is already on its way to `merge`; the caller continues with the
    /// next implementor.
    fn bridge_argument_misses(
        &mut self,
        arguments: &[(Value, Option<Ty>)],
        params: &[ParameterBoundary],
        answer: Ty,
        merge: Block,
        carried: Carrier,
    ) -> Result<bool, Unsupported> {
        let (Some((value, source)), Some(boundary)) = (arguments.first(), params.first()) else {
            return Ok(false);
        };
        if boundary.implementation.non_null() == boundary.declared.non_null()
            || !boundary.implementation.is_reference()
        {
            return Ok(false);
        }
        if source.is_none_or(|ty| self.carrier(ty) != Carrier::Ref) {
            let rejected = self.bridge_rejection(answer);
            self.jump_to_merge(merge, carried, rejected);
            return Ok(true);
        }
        let Some((accepted, rejected)) = self.bridge_argument_guard(*value, boundary)? else {
            return Ok(false);
        };
        self.continue_in(rejected);
        self.builder.seal_block(rejected);
        let rejected_value = self.bridge_rejection(answer);
        self.jump_to_merge(merge, carried, rejected_value);
        self.continue_in(accepted);
        self.builder.seal_block(accepted);
        Ok(false)
    }

    /// The branch of a collection special bridge on one erased reference argument: it goes to the
    /// first block when the argument is an instance of the override's parameter type (a `null` the
    /// type admits too), and to the second, which the caller fills with the bridge's rejection,
    /// when it is not. A primitive parameter is asked through its box, so a `null` and a value of
    /// another type are rejected rather than unboxed. `None` where the override's parameter is the
    /// declared one, or has no type descriptor to test, so there is nothing to bridge.
    pub(super) fn bridge_argument_guard(
        &mut self,
        value: Value,
        boundary: &ParameterBoundary,
    ) -> Result<Option<(Block, Block)>, Unsupported> {
        if boundary.implementation.non_null() == boundary.declared.non_null() {
            return Ok(None);
        }
        let Some(descriptor) = self.file.type_descriptor(boundary.implementation)? else {
            return Ok(None);
        };
        let type_address = self.data_address(descriptor);
        let mut matches = self
            .runtime_call(
                "kt_is_instance",
                &[any(), any()],
                Ty::Boolean,
                &[value, type_address],
            )?
            .expect("`kt_is_instance` returns a Boolean");
        if boundary.implementation.admits_null() {
            let absent = self.builder.ins().icmp_imm_u(IntCC::Equal, value, 0);
            matches = self.builder.ins().bor(matches, absent);
        }
        let accepted = self.builder.create_block();
        let rejected = self.builder.create_block();
        self.builder
            .ins()
            .brif(matches, accepted, &[], rejected, &[]);
        Ok(Some((accepted, rejected)))
    }

    /// `contains`/`remove` answer `false` for a foreign argument; an index answers `-1`.
    pub(super) fn bridge_rejection(&mut self, answer: Ty) -> Option<Value> {
        let clif = self.carrier(answer).clif()?;
        let bits = if answer == Ty::Int { -1 } else { 0 };
        Some(self.builder.ins().iconst(clif, bits))
    }

    fn unit_where_wanted(&mut self, answer: Ty) -> Result<Option<Value>, Unsupported> {
        if self.terminated || self.carrier(answer) != Carrier::Ref {
            return Ok(None);
        }
        self.runtime_call("kt_unit", &[], any(), &[])
    }

    fn jump_to_merge(&mut self, merge: Block, carried: Carrier, produced: Option<Value>) {
        if self.terminated {
            return;
        }
        match (carried.clif(), produced) {
            (Some(_), Some(value)) => {
                self.builder.ins().jump(merge, &[BlockArg::Value(value)]);
            }
            (Some(clif), None) => {
                let filler = match clif {
                    types::F32 => self.builder.ins().f32const(0.0),
                    types::F64 => self.builder.ins().f64const(0.0),
                    integer => self.builder.ins().iconst(integer, 0),
                };
                self.builder.ins().jump(merge, &[BlockArg::Value(filler)]);
            }
            (None, _) => {
                self.builder.ins().jump(merge, &[]);
            }
        }
    }
}
