//! How the runtime resumes a `Continuation` of a class of this file.
//!
//! A suspended computation resumes whatever continuation it was handed, and the runtime has to be
//! able to do so too (`kt_continuation_resume_with`). A class of the program that is a
//! continuation — a suspend function's frame class, or a source class implementing
//! `kotlin.coroutines.Continuation` — therefore publishes two thunks in its descriptor, with the
//! fixed signatures `KType.continuation_resume_with` and `KType.continuation_context` state. Each
//! dispatches VIRTUALLY through the member's slot, so a subclass's override is reached through its
//! base's thunk, exactly as the walk thunks of `super::objects` are.
//!
//! A `Result<T>` crosses as its raw value on this target (see `krusty_coroutines.c`), so the
//! operand of `resumeWith` is handed over as the reference it already is.

use super::representation::is_raw_result;
use super::*;

/// The two thunks a continuation class publishes; both `None` for every other type.
#[derive(Clone, Copy, Default)]
pub(super) struct ContinuationMembers {
    pub(super) resume_with: Option<FuncId>,
    pub(super) context: Option<FuncId>,
}

impl<'a> FileLowering<'a> {
    /// The slots of `class`'s `resumeWith` and `context` getter, when it is a continuation.
    fn continuation_slots(&self, class: ClassId) -> Result<Option<(u32, u32)>, Unsupported> {
        if let Some(members) = self.coroutines.frames.get(&class) {
            let resume_with = model::function_key(self.ir, class, members.resume_with);
            let context = model::function_key(self.ir, class, members.context);
            let resume_with = self.model.slot(class, &resume_with).ok_or_else(|| {
                format!(
                    "the generated `resumeWith` of `{}` has no virtual slot",
                    self.ir.classes[class as usize].fq_name()
                )
            })?;
            let context = self.model.slot(class, &context).ok_or_else(|| {
                format!(
                    "the generated `context` of `{}` has no virtual slot",
                    self.ir.classes[class as usize].fq_name()
                )
            })?;
            return Ok(Some((resume_with, context)));
        }

        let mut resume_with = None;
        let mut context = None;
        let mut at = Some(class);
        while let Some(id) = at {
            let fq_name = self.ir.classes[id as usize].fq_name;
            for edge in self
                .ir
                .function_overrides
                .get(&fq_name)
                .into_iter()
                .flatten()
            {
                let crate::fir::ResolvedFunctionOverrideTarget::External(declaration) =
                    edge.overridden
                else {
                    continue;
                };
                let Some(fact) = self.callables.callable(declaration) else {
                    continue;
                };
                let owner = super::super::super::intrinsics::DeclarationOwner::callable(
                    fact.physical_owner,
                    fact.declaration_owner,
                );
                let signature = super::super::super::intrinsics::FunctionSignature::new(
                    owner,
                    &fact.name,
                    &edge.declared_parameters,
                    edge.declared_result,
                )
                .with_receiver(fact.source_receiver);
                if !super::super::super::intrinsics::is_continuation_resume_with(signature) {
                    continue;
                }
                let implementation = match (edge.implementation_function, edge.implementation) {
                    (Some(function), _) => Some(function),
                    (None, crate::fir::ResolvedFunctionOverrideTarget::Module(callable)) => {
                        self.ir.checked_callable_functions.get(&callable).copied()
                    }
                    (None, crate::fir::ResolvedFunctionOverrideTarget::External(_)) => None,
                };
                let implementation = implementation.ok_or_else(|| {
                    format!(
                        "the `Continuation.resumeWith` implementation of `{}` has no common-IR function",
                        self.ir.classes[class as usize].fq_name()
                    )
                })?;
                let owner = self
                    .ir
                    .class_id_by_name(edge.implementation_owner)
                    .ok_or_else(|| {
                        format!(
                            "the `Continuation.resumeWith` implementation owner of `{}` is outside this file",
                            self.ir.classes[class as usize].fq_name()
                        )
                    })?;
                let key = model::function_key(self.ir, owner, implementation);
                resume_with = Some(self.model.slot(class, &key).ok_or_else(|| {
                    format!(
                        "the `Continuation.resumeWith` implementation of `{}` has no virtual slot",
                        self.ir.classes[class as usize].fq_name()
                    )
                })?);
            }
            for edge in self
                .ir
                .property_overrides
                .get(&fq_name)
                .into_iter()
                .flatten()
            {
                let crate::fir::ResolvedPropertyOverrideTarget::External(getter) = edge.overridden
                else {
                    continue;
                };
                let Some(fact) = self.callables.callable(getter) else {
                    continue;
                };
                let owner = super::super::super::intrinsics::DeclarationOwner::callable(
                    fact.physical_owner,
                    fact.declaration_owner,
                );
                let signature = super::super::super::intrinsics::PropertySignature::new(
                    owner,
                    &edge.name,
                    &[],
                    edge.declared_type,
                )
                .with_receiver(edge.declared_receiver);
                if !super::super::super::intrinsics::is_continuation_context_property(signature) {
                    continue;
                }
                let key = super::super::super::classes::SlotKey::Getter(edge.implementation);
                context = Some(self.model.slot(class, &key).ok_or_else(|| {
                    format!(
                        "the `Continuation.context` implementation of `{}` has no virtual slot",
                        self.ir.classes[class as usize].fq_name()
                    )
                })?);
            }
            at = self.model.layout(id).superclass;
        }
        match (resume_with, context) {
            (None, None) => Ok(None),
            (Some(resume_with), Some(context)) => Ok(Some((resume_with, context))),
            (Some(_), None) => Err(format!(
                "a `Continuation` implementation `{}` with no exact `context` override edge",
                self.ir.classes[class as usize].fq_name()
            )),
            (None, Some(_)) => Err(format!(
                "a `Continuation` implementation `{}` with no exact `resumeWith` override edge",
                self.ir.classes[class as usize].fq_name()
            )),
        }
    }

    /// Emit `class`'s two continuation thunks, or nothing when it is no continuation.
    pub(super) fn define_continuation_members(
        &mut self,
        class: ClassId,
        base: &str,
    ) -> Result<ContinuationMembers, Unsupported> {
        let Some((resume_slot, context_slot)) = self.continuation_slots(class)? else {
            return Ok(ContinuationMembers::default());
        };
        let (resume_params, _) = self.slot_signature(class, resume_slot)?;
        let [result_param] = resume_params.as_slice() else {
            return Err(format!(
                "a `resumeWith` of `{}` that takes no one operand",
                self.ir.classes[class as usize].fq_name()
            ));
        };
        let result_param = *result_param;
        let (_, context_ret) = self.slot_signature(class, context_slot)?;

        let name = format!("{base}_continuation_resume_with");
        let resume_with = self.declare_local_function(&name, &[any(), any()], Ty::Unit)?;
        let signature = self.signature_of(&[any(), any()], Ty::Unit)?;
        self.emit_function(
            resume_with,
            signature,
            Ty::Unit,
            &name,
            &mut |body, params| {
                let carried = body.file.values.project(result_param);
                let Some(result) = body.convert_carriers(params[1], Some(any()), carried)? else {
                    return Err(format!("a `Unit` result in `{name}`"));
                };
                body.dispatch(params[0], resume_slot, &[result_param], Ty::Unit, &[result])?;
                body.builder.ins().return_(&[]);
                body.terminate();
                Ok(())
            },
        )?;

        let name = format!("{base}_continuation_context");
        let context = self.declare_local_function(&name, &[any()], any())?;
        let signature = self.signature_of(&[any()], any())?;
        self.emit_function(context, signature, any(), &name, &mut |body, params| {
            let Some(produced) = body.dispatch(params[0], context_slot, &[], context_ret, &[])?
            else {
                return Err(format!("a `Unit` answer from `{name}`"));
            };
            let Some(value) = body.convert(produced, Some(context_ret), any())? else {
                return Err(format!("a `Unit` answer from `{name}`"));
            };
            body.builder.ins().return_(&[value]);
            body.terminate();
            Ok(())
        })?;
        Ok(ContinuationMembers {
            resume_with: Some(resume_with),
            context: Some(context),
        })
    }

    /// The parameters and result of the entry in `slot` of `class`'s table.
    fn slot_signature(&self, class: ClassId, slot: u32) -> Result<(Vec<Ty>, Ty), Unsupported> {
        let entry = self
            .model
            .layout(class)
            .vtable
            .get(slot as usize)
            .cloned()
            .ok_or_else(|| format!("a continuation member in slot {slot}, which no table has"))?;
        let fid = match entry {
            Slot::Function(fid) => fid,
            Slot::Bridge { declared, .. } => declared,
            // `override val context = …`: a getter synthesized over the backing field.
            Slot::FieldGetter { class, field } => {
                let field = &self.ir.classes[class as usize].fields[field as usize];
                return Ok((Vec::new(), field.ty));
            }
            other => {
                return Err(format!(
                    "a continuation member in the vtable entry {other:?}"
                ))
            }
        };
        let function = &self.ir.functions[fid as usize];
        Ok((function.params.clone(), function.ret))
    }
}

impl BodyLowering<'_, '_, '_> {
    /// The runtime function realizing a read of the coroutine library property `target`.
    pub(super) fn coroutine_property(
        &self,
        target: crate::fir::ExternalPropertyId,
    ) -> Option<&'static str> {
        let property = self.file.callables.property(target)?;
        let getter = self.file.callables.callable(property.getter)?;
        let owner = super::super::super::intrinsics::DeclarationOwner::callable(
            getter.physical_owner,
            getter.declaration_owner,
        );
        let signature = super::super::super::intrinsics::PropertySignature::new(
            owner,
            &property.name,
            &getter.params,
            getter.ret,
        )
        .with_receiver(getter.source_receiver);
        super::super::super::intrinsics::coroutine_property(signature)
    }

    /// Read it: the receiver, when there is one, is the runtime function's one operand. A
    /// `Boolean` answer comes back as one; every other answer is a reference.
    pub(super) fn coroutine_property_read(
        &mut self,
        symbol: &str,
        receiver: Option<u32>,
        result: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        let mut arguments = Vec::new();
        if let Some(receiver) = receiver {
            arguments.push(self.raw_reference(receiver)?);
            if self.terminated {
                return Ok(None);
            }
        }
        let params = vec![any(); arguments.len()];
        if result == Ty::Boolean {
            return self.runtime_call(symbol, &params, Ty::Boolean, &arguments);
        }
        let Some(produced) = self.runtime_call(symbol, &params, any(), &arguments)? else {
            return Ok(None);
        };
        self.from_raw_reference(produced, result)
    }
}

impl BodyLowering<'_, '_, '_> {
    /// A call [`intrinsics::coroutine_call`](super::super::super::intrinsics::coroutine_call)
    /// realizes, or `None` when `signature` is no such call.
    pub(super) fn coroutine_call(
        &mut self,
        signature: super::super::super::intrinsics::FunctionSignature<'_>,
        receiver: Option<u32>,
        args: &[u32],
        ret: Ty,
    ) -> Option<Result<Option<Value>, Unsupported>> {
        let call = super::super::super::intrinsics::coroutine_call(signature)?;
        Some(self.realize_coroutine_call(call, signature, receiver, args, ret))
    }

    /// Each operand crosses as a reference. One the DECLARATION types `Result` crosses as its raw
    /// value: the receiver of `Result`'s own members and extensions, and `resumeWith`'s operand. A
    /// `Result` that is a type argument (`resume(value: T)` with `T = Result<…>`) is boxed, as the
    /// library sees a `T`.
    fn realize_coroutine_call(
        &mut self,
        call: super::super::super::intrinsics::CoroutineCall,
        signature: super::super::super::intrinsics::FunctionSignature<'_>,
        receiver: Option<u32>,
        args: &[u32],
        ret: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        let receiver = receiver.filter(|_| !call.companion_receiver);
        let raw_receiver = signature.receiver.map_or(call.result_member, is_raw_result);
        let operands = receiver
            .map(|receiver| (receiver, raw_receiver))
            .into_iter()
            .chain(args.iter().enumerate().map(|(index, &argument)| {
                let raw = signature
                    .params
                    .get(index)
                    .is_some_and(|&ty| is_raw_result(ty));
                (argument, raw)
            }));
        let mut arguments = Vec::with_capacity(args.len() + 1);
        for (operand, raw) in operands {
            arguments.push(if raw {
                self.raw_reference(operand)?
            } else {
                self.reference(operand)?
            });
            if self.terminated {
                return Ok(None);
            }
        }
        let params = vec![any(); arguments.len()];
        if !call.answers {
            self.runtime_call(call.symbol, &params, Ty::Unit, &arguments)?;
            return Ok(None);
        }
        let Some(produced) = self.runtime_call(call.symbol, &params, any(), &arguments)? else {
            return Ok(None);
        };
        if is_raw_result(signature.ret) {
            return self.from_raw_reference(produced, ret);
        }
        self.convert(produced, Some(any()), ret)
    }

    /// `operand` as a reference, a `Result` among them as its raw value rather than a box. Any
    /// other value class crosses boxed, as the generic `T` it is to the library.
    fn raw_reference(&mut self, operand: u32) -> Result<Value, Unsupported> {
        let ty = self.type_of(operand);
        if let Some(ty) = ty.filter(|&ty| is_raw_result(ty)) {
            let Some(value) = self.expression(operand)? else {
                return Ok(self.builder.ins().iconst(types::I64, 0));
            };
            let carried = self.file.values.project(ty);
            return Ok(self
                .convert_carriers(value, Some(carried), any())?
                .unwrap_or(value));
        }
        self.reference(operand)
    }

    /// A raw reference the runtime answered, as `ty`: a `Result` takes it as its value.
    fn from_raw_reference(&mut self, value: Value, ty: Ty) -> Result<Option<Value>, Unsupported> {
        if is_raw_result(ty) {
            let carried = self.file.values.project(ty);
            return self.convert_carriers(value, Some(any()), carried);
        }
        self.convert(value, Some(any()), ty)
    }
}
