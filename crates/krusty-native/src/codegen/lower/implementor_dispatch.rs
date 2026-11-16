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
pub(super) struct ImplementorArm {
    class: ClassId,
    slot: u32,
    params: Vec<ParameterBoundary>,
    ret: Ty,
}

/// One parameter of an implementor's arm, at both of its types. The call site hands the operand to
/// the DEPENDENCY declaration, whose parameter may be a type parameter the implementor fixes:
/// `Comparable<II>.compareTo` takes a `T`, carried as a reference, where `II.compareTo` takes the
/// `II` value. The operand crosses that declared boundary first and becomes the implementation's
/// type from there, as it would through the bridge a virtual call reaches.
struct ParameterBoundary {
    declared: Ty,
    implementation: Ty,
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
        let class_name = ir.classes[class as usize].fq_name;
        let edge = ir
            .function_overrides
            .get(&class_name)?
            .iter()
            .find(|edge| {
                edge.overridden == krusty::fir::ResolvedFunctionOverrideTarget::External(overridden)
            })?;
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
            class,
            slot,
            params,
            ret: declaration.ret,
        })
    }

    /// The slot of `class`'s getter overriding the dependency property whose getter is
    /// `overridden`, answering `declared`: the class registers the overridden declaration's
    /// identity at the slot of its own accessor. `None` where the override's representation is not the declaration's, which a
    /// direct dispatch could not reconcile.
    pub(super) fn external_property_getter_slot(
        &self,
        class: ClassId,
        overridden: krusty::fir::ExternalCallableId,
        declared: Ty,
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
        if self.carrier(implementation) != self.carrier(declared) {
            return None;
        }
        let slot = self
            .file
            .model
            .slot(class, &crate::classes::SlotKey::Getter(target))?;
        Some(ImplementorArm {
            class,
            slot,
            params: Vec::new(),
            ret: implementation,
        })
    }

    /// A call of a dependency interface member this file does not implement. The link joins the
    /// stub to the implementation a dependency object recorded under the same identity. An
    /// object the runtime makes records nothing, so one whose class implements the interface is
    /// tested for first and handed to the runtime function realizing the member on it.
    pub(super) fn dependency_interface_stub(
        &mut self,
        identity: &krusty::libraries::KlibDependencyCallable,
        signature: super::super::super::intrinsics::FunctionSignature<'_>,
        receiver: u32,
        args: &[u32],
        params: &[Ty],
        ret: Ty,
    ) -> Result<Option<Value>, Unsupported> {
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
        let runtime = self.runtime_implementations(signature)?;
        let number = self.file.dependency_interface_number(identity);
        let Some(object) = self.receiver(receiver)? else {
            return Ok(None);
        };
        let arguments = self.arguments(args, params)?;
        if self.terminated {
            return Ok(None);
        }
        // The interface slot has the declaration's ABI. A generic member applied at a primitive
        // or value class (`ListIterator<Int>.previous`) crosses that boundary as a call through
        // the erased slot does: each operand enters at its declared type and the answer leaves
        // it at the type the site asked for.
        let mut crossed = Vec::with_capacity(arguments.len());
        for ((&value, &applied), &declared) in
            arguments.iter().zip(params).zip(&declared_member_params)
        {
            let Some(value) = self.convert(value, Some(applied), declared)? else {
                return Ok(None);
            };
            crossed.push(value);
        }
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
        if self.terminated {
            return Ok(None);
        }
        match produced {
            Some(value) => self.convert(value, Some(declared_ret), ret),
            None => self.unit_where_wanted(ret),
        }
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
        let bridge = self.file.mapped_collection(ty).is_some_and(|collection| {
            super::super::super::intrinsics::has_special_bridge(collection, signature.name)
        });
        bridge && !matches!(signature.ret, Ty::Boolean | Ty::Int)
    }

    /// Dispatch one dependency interface member by the exact override edge each implementor
    /// published. An implementor without that edge leaves the call to the paths that already
    /// know the member; this does not recover an implementation by its name. A member the runtime
    /// also answers is left there too.
    pub(super) fn dependency_interface_member(
        &mut self,
        internal: krusty::types::TypeName,
        target: krusty::fir::ExternalCallableId,
        signature: super::super::super::intrinsics::FunctionSignature<'_>,
        call: InterfaceMemberCall<'_>,
    ) -> Option<Result<Option<Value>, Unsupported>> {
        if self.runtime_owns_member(internal, signature) {
            return None;
        }
        let declared = self.file.implementors_of(internal);
        let implementors: Vec<ImplementorArm> = declared
            .iter()
            .filter_map(|&class| self.external_override_slot(class, target, signature.params))
            .collect();
        if implementors.is_empty() || implementors.len() != declared.len() {
            return None;
        }
        let runtime = match self.runtime_implementations(signature) {
            Ok(runtime) => runtime,
            Err(declined) => return Some(Err(declined)),
        };
        let bridge = self
            .file
            .mapped_collection(Ty::obj_name(internal))
            .is_some_and(|collection| {
                super::super::super::intrinsics::has_special_bridge(collection, signature.name)
            });
        // A runtime collection is not one of the classes above. The member's own runtime function
        // is the remaining arm; an abstract-method trap would be a call the program did make.
        let collection = self.checked_type(call.receiver).and_then(|ty| {
            super::lists::collection_runtime_member(self.file, ty, signature, call.physical)
                .filter(|(_, carried, _)| carried.len() == call.args.len() + 1)
        });
        Some(self.dependency_interface_dispatch(&implementors, &runtime, call, bridge, collection))
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
                        // No source class and no runtime function realizes this member.
                        body.runtime_call("kt_abstract_method_called", &[], Ty::Unit, &[])?;
                        Ok(None)
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
            let descriptor = self.file.classes[arm.class as usize].descriptor;
            let type_address = self.data_address(descriptor);
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
        let Some(descriptor) = self.file.type_descriptor(boundary.implementation)? else {
            return Ok(false);
        };
        let type_address = self.data_address(descriptor);
        let matches = self
            .runtime_call(
                "kt_is_instance",
                &[any(), any()],
                Ty::Boolean,
                &[*value, type_address],
            )?
            .expect("`kt_is_instance` returns a Boolean");
        let accepted = self.builder.create_block();
        let rejected = self.builder.create_block();
        self.builder
            .ins()
            .brif(matches, accepted, &[], rejected, &[]);
        self.continue_in(rejected);
        self.builder.seal_block(rejected);
        let rejected_value = self.bridge_rejection(answer);
        self.jump_to_merge(merge, carried, rejected_value);
        self.continue_in(accepted);
        self.builder.seal_block(accepted);
        Ok(false)
    }

    /// `contains`/`remove` answer `false` for a foreign argument; an index answers `-1`.
    fn bridge_rejection(&mut self, answer: Ty) -> Option<Value> {
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
