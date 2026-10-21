//! Calls into a previously emitted dependency use its declaration-owned ABI, never the
//! specialized types of the current call. Checked instantiation validates operand roles first.

use super::*;

impl BodyLowering<'_, '_, '_> {
    /// A selected property's exact accessor export owns the access before target storage helpers.
    /// No accessor spelling, overload reconstruction, or second evaluation of operands is needed.
    pub(in super::super) fn emitted_dependency_property(
        &mut self,
        site: u32,
        operation: &IrCheckedOperation,
    ) -> Option<Result<Option<Value>, Unsupported>> {
        let (property, dispatch, receiver, arguments, parameters, result, write) = match operation {
            IrCheckedOperation::ExternalPropertyRead {
                target,
                dispatch,
                receiver,
                arguments,
                parameters,
                result,
                ..
            } => (
                *target, *dispatch, *receiver, arguments, parameters, *result, false,
            ),
            IrCheckedOperation::ExternalPropertyWrite {
                target,
                dispatch,
                receiver,
                arguments,
                parameters,
                result,
                ..
            } => (
                *target, *dispatch, *receiver, arguments, parameters, *result, true,
            ),
            _ => return None,
        };
        let property = self.file.callables.property(property)?;
        let target = if write {
            property.setter?
        } else {
            property.getter
        };
        let realization = self.file.callables.callable(target)?;
        let identity = realization.dependency_callable.as_ref()?;
        let export = self.file.dependency_symbols.get(identity)?;
        Some((|| {
            if dispatch != crate::ir::IrPropertyDispatch::Ordinary {
                return Err("a dependency accessor export called through super".into());
            }
            if realization
                .generic_sig
                .as_ref()
                .is_some_and(|signature| !signature.formals.is_empty())
            {
                return Err("a dependency property without checked generic substitutions".into());
            }
            // Source extensions may retain a separate receiver. Serialized extension accesses
            // already flatten it at the declaration's context coordinate; member extensions
            // retain class dispatch separately in both forms.
            let extension_receiver_parameter =
                if realization.is_member_extension()
                    || (realization.is_extension() && receiver.is_none())
                {
                    Some(u32::try_from(realization.context_count).map_err(|_| {
                        Unsupported::from("an invalid accessor receiver coordinate")
                    })?)
                } else {
                    None
                };
            let callee = Callee::External {
                target,
                default_provider: None,
                params: parameters.clone(),
                ret: result,
                substitutions: Vec::new(),
                defaults: Vec::new(),
                extension_receiver_parameter,
            };
            self.emitted_dependency_call(site, export, realization, &callee, receiver, arguments)
        })())
    }

    pub(super) fn emitted_dependency_call(
        &mut self,
        site: u32,
        export: &super::super::super::DependencyExport,
        realization: &crate::backend::BackendCallableFact,
        callee: &Callee,
        dispatch_receiver: Option<u32>,
        args: &[u32],
    ) -> Result<Option<Value>, Unsupported> {
        let Callee::External {
            target,
            params,
            ret,
            substitutions,
            extension_receiver_parameter,
            ..
        } = callee
        else {
            return Err("an emitted dependency call without a checked external identity".into());
        };
        if args.len() != params.len() {
            return Err(
                "a dependency call whose operands disagree with its checked parameters".into(),
            );
        }
        let instantiated =
            crate::backend::instantiate_dependency_signature(realization, *target, substitutions)
                .map_err(Unsupported::from)?;
        if realization
            .generic_sig
            .as_ref()
            .is_some_and(|signature| !signature.formals.is_empty())
            && instantiated.result != *ret
        {
            return Err("a dependency generic result disagrees with its checked call".into());
        }
        let unread_receiver = export
            .singleton_extension_receiver
            .map(|receiver| receiver.semantic_type());
        let selected_singleton = self.file.callables.singleton_dispatch(site);
        let receiver_is_exact_singleton = dispatch_receiver.is_some_and(|receiver| {
            if realization.is_member_function() {
                selected_singleton
                    .zip(export.singleton_member_receiver)
                    .is_some_and(|(selected, effect)| selected.classifier == effect.classifier())
            } else {
                unread_receiver.is_some_and(|expected| {
                    crate::backend::authenticated_singleton_receiver(
                        Some(receiver),
                        selected_singleton.map(|dispatch| dispatch.classifier),
                        expected,
                    )
                })
            }
        });
        let call = dependency_export_call(
            realization.is_top_level_function(),
            realization.is_extension(),
            instantiated.receiver,
            realization.context_count,
            &instantiated.parameters,
            params,
            *extension_receiver_parameter,
            dispatch_receiver.is_some(),
            unread_receiver,
            realization.klib_body_companion_receiver_position(),
            export
                .member_receiver
                .map(|receiver| receiver.semantic_type()),
            receiver_is_exact_singleton && export.singleton_member_receiver.is_some(),
        )?;
        let abi = export.signature.parameters();
        let inserts_receiver = matches!(
            call,
            DependencyExportCall::Member
                | DependencyExportCall::MemberExtension
                | DependencyExportCall::SourceExtension { .. }
                | DependencyExportCall::SourceSingletonExtension { .. }
                | DependencyExportCall::ClassifierAssociated { .. }
        );
        if abi.len() != args.len() + usize::from(inserts_receiver) {
            return Err(
                "a dependency export whose declared operand count disagrees with its call".into(),
            );
        }
        let arguments = match call {
            DependencyExportCall::TopLevel | DependencyExportCall::SerializedExtension => {
                self.arguments(args, abi)?
            }
            DependencyExportCall::SingletonMember => {
                let receiver = dispatch_receiver.expect("validated singleton member receiver");
                let effect = export.singleton_member_receiver.ok_or_else(|| {
                    declined!("a receiver-erased dependency member without its singleton effect")
                })?;
                if effect.evaluate_source() {
                    self.statement(receiver)?;
                    if self.terminated {
                        return Ok(None);
                    }
                }
                self.arguments(args, abi)?
            }
            DependencyExportCall::Member | DependencyExportCall::MemberExtension => {
                let receiver = dispatch_receiver.expect("validated dependency member receiver");
                let receiver = self
                    .coerce(receiver, abi[0])?
                    .ok_or_else(|| declined!("a `Unit` dependency member receiver"))?;
                if self.terminated {
                    return Ok(None);
                }
                let mut arguments = vec![receiver];
                arguments.extend(self.arguments(args, &abi[1..])?);
                arguments
            }
            DependencyExportCall::SerializedClassifierAssociated { receiver } => {
                if self.carrier(receiver) != Carrier::Ref {
                    return Err(declined!(
                        "a classifier-associated extension receiver without a reference carrier"
                    ));
                }
                self.arguments(args, abi)?
            }
            DependencyExportCall::SourceExtension {
                receiver_position, ..
            } => {
                let receiver = dispatch_receiver.expect("validated source extension receiver");
                let mut arguments =
                    self.arguments(&args[..receiver_position], &abi[..receiver_position])?;
                if self.terminated {
                    return Ok(None);
                }
                let receiver = self
                    .coerce(receiver, abi[receiver_position])?
                    .ok_or_else(|| declined!("a `Unit` dependency receiver"))?;
                if self.terminated {
                    return Ok(None);
                }
                arguments.push(receiver);
                arguments.extend(
                    self.arguments(&args[receiver_position..], &abi[receiver_position + 1..])?,
                );
                arguments
            }
            DependencyExportCall::SourceSingletonExtension {
                receiver,
                receiver_position,
            } => {
                if self.carrier(receiver) != Carrier::Ref
                    || self.carrier(abi[receiver_position]) != Carrier::Ref
                {
                    return Err(declined!(
                        "an unread singleton extension receiver without a reference carrier"
                    ));
                }
                let mut arguments =
                    self.arguments(&args[..receiver_position], &abi[..receiver_position])?;
                if self.terminated {
                    return Ok(None);
                }
                if export
                    .singleton_extension_receiver
                    .is_none_or(|effect| effect.evaluate_source())
                {
                    let source = dispatch_receiver.expect("validated singleton extension receiver");
                    // Unread is not inert: initialization effects require their separate proof.
                    let _ = self.coerce(source, receiver)?;
                    if self.terminated {
                        return Ok(None);
                    }
                }
                arguments.push(self.builder.ins().iconst(types::I64, 0));
                arguments.extend(
                    self.arguments(&args[receiver_position..], &abi[receiver_position + 1..])?,
                );
                arguments
            }
            DependencyExportCall::ClassifierAssociated {
                receiver,
                receiver_position,
            } => {
                if self.carrier(receiver) != Carrier::Ref
                    || self.carrier(abi[receiver_position]) != Carrier::Ref
                {
                    return Err(declined!(
                        "a classifier-associated extension receiver without a reference carrier"
                    ));
                }
                let mut arguments =
                    self.arguments(&args[..receiver_position], &abi[..receiver_position])?;
                if self.terminated {
                    return Ok(None);
                }
                arguments.push(self.builder.ins().iconst(types::I64, 0));
                arguments.extend(
                    self.arguments(&args[receiver_position..], &abi[receiver_position + 1..])?,
                );
                arguments
            }
        };
        if self.terminated {
            return Ok(None);
        }
        let id = self
            .file
            .import(&export.symbol, abi, export.signature.result())?;
        let func_ref = self.func_ref(id);
        let call = self.emit_call(func_ref, &arguments)?;
        match self.builder.inst_results(call).first().copied() {
            Some(value) => self.convert(value, Some(export.signature.result()), *ret),
            None => Ok(None),
        }
    }
}
