//! Dispatch for dependency members whose receiver may be implemented by a class in this file.
//!
//! Runtime entry points only understand runtime-owned objects. These operations test each known
//! source implementor once and enter its own slot before falling through to the runtime shape.

use super::*;

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
        let runtime = self.runtime_implementations(signature)?;
        let number = self.file.dependency_interface_number(identity);
        let Some(object) = self.receiver(receiver)? else {
            return Ok(None);
        };
        let arguments = self.arguments(args, params)?;
        if self.terminated {
            return Ok(None);
        }
        let operands: Vec<(Value, Option<Ty>)> = arguments
            .iter()
            .zip(params)
            .map(|(&value, &param)| (value, Some(param)))
            .collect();
        self.dispatch_runtime_implementations(&runtime, object, &operands, ret, |body| {
            body.dispatch_through(
                object,
                model::Dispatch::Interface(number),
                params,
                ret,
                &arguments,
            )
        })
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
    /// A collection or a closed range is also worn by objects the runtime builds. No static type
    /// tells one of those from a class of this file, which is why a member asked of such a type
    /// declines once the handlers that can tell them apart have passed. A special bridge may skip
    /// the override entirely when the argument cannot be what the declaration accepts, and this
    /// dispatch has no bridge to put in front of an implementor's arm.
    fn runtime_owns_member(&self, internal: krusty::types::TypeName, name: &str) -> bool {
        if super::classifier_shapes::is_closed_range(internal)
            || super::classifier_shapes::is_closed_floating_range(internal)
        {
            return true;
        }
        let ty = Ty::obj_name(internal);
        self.file.implements_collection_of(ty)
            || self.file.mapped_collection(ty).is_some_and(|collection| {
                super::super::super::intrinsics::has_special_bridge(collection, name)
            })
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
        receiver: u32,
        args: &[u32],
        ret: Ty,
    ) -> Option<Result<Option<Value>, Unsupported>> {
        if self.runtime_owns_member(internal, signature.name) {
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
        Some(self.dependency_interface_dispatch(&implementors, &runtime, receiver, args, ret))
    }

    /// Dispatch among the classes of this file implementing a dependency interface member, then
    /// among the classes the runtime makes that implement it.
    fn dependency_interface_dispatch(
        &mut self,
        implementors: &[ImplementorArm],
        runtime: &[(&'static str, &'static str)],
        receiver: u32,
        args: &[u32],
        ret: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        let object = self.reference(receiver)?;
        if self.terminated {
            return Ok(None);
        }
        let mut operands = Vec::with_capacity(args.len());
        for argument in args {
            let Some(value) = self.expression(*argument)? else {
                return Ok(None);
            };
            if self.terminated {
                return Ok(None);
            }
            operands.push((value, self.physical_type(*argument)));
        }
        let runtime_operands = operands.clone();
        self.dispatch_by_implementor_with(implementors, object, &operands, ret, |body, object| {
            body.dispatch_runtime_implementations(runtime, object, &runtime_operands, ret, |body| {
                // Unreachable when every implementor was tested. The merge fills the carrier.
                body.runtime_call("kt_abstract_method_called", &[], Ty::Unit, &[])?;
                Ok(None)
            })
        })
    }

    pub(super) fn dispatch_by_implementor_with(
        &mut self,
        implementors: &[ImplementorArm],
        object: Value,
        arguments: &[(Value, Option<Ty>)],
        answer: Ty,
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
