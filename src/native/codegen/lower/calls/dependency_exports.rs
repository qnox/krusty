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
        let access = self.file.callables.dependency_accessor(operation)?;
        let realization = access.callable;
        let identity = realization.dependency_callable.as_ref()?;
        let export = self.file.dependency_symbols.get(identity)?;
        Some((|| {
            let callee = access.callee().map_err(Unsupported::from)?;
            self.emitted_dependency_call(
                site,
                export,
                realization,
                &callee,
                access.receiver,
                access.arguments,
            )
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
            defaults,
            ..
        } = callee
        else {
            return Err("an emitted dependency call without a checked external identity".into());
        };
        if args.len() + defaults.len() != params.len() {
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
        let call = DependencyCallShape {
            is_top_level_function: realization.is_top_level_function(),
            is_extension: realization.is_extension(),
            source_receiver: instantiated.receiver,
            context_count: realization.context_count,
            emitted_params: &instantiated.parameters,
            call_params: params,
            extension_receiver_parameter: *extension_receiver_parameter,
            has_dispatch_receiver: dispatch_receiver.is_some(),
            unread_receiver,
            companion_receiver_position: realization.klib_body_companion_receiver_position(),
            member_receiver: export
                .member_receiver
                .map(|receiver| receiver.semantic_type()),
            receiver_is_exact_singleton: receiver_is_exact_singleton
                && export.singleton_member_receiver.is_some(),
        }
        .authenticate()
        .map_err(Unsupported::from)?;
        let declared = export.signature.parameters();
        let inserts_receiver = matches!(
            call,
            DependencyExportCall::Member
                | DependencyExportCall::MemberExtension
                | DependencyExportCall::SourceExtension { .. }
                | DependencyExportCall::SourceSingletonExtension { .. }
                | DependencyExportCall::ClassifierAssociated { .. }
        );
        if declared.len() != params.len() + usize::from(inserts_receiver) {
            return Err(
                "a dependency export whose declared operand count disagrees with its call".into(),
            );
        }
        // The operands the call supplies, at their declared positions. An omitted parameter is
        // a value parameter, so it follows an inserted receiver.
        let omitted = call
            .omitted_operands(defaults, declared.len())
            .map_err(Unsupported::from)?;
        let supplied = declared
            .iter()
            .enumerate()
            .filter(|(position, _)| !omitted.contains(position))
            .map(|(_, ty)| *ty)
            .collect::<Vec<_>>();
        let abi = supplied.as_slice();
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
        let (symbol, signature, arguments) = match &export.default_entry {
            Some(entry) if !omitted.is_empty() => {
                // A zero in each omitted slot and its bit in the mask; the entry evaluates the
                // marked defaults in the declaring module, in declaration order.
                let mut supplied = arguments.into_iter();
                let mut masks = vec![0u32; declared.len().div_ceil(32)];
                let mut operands = Vec::with_capacity(declared.len() + masks.len());
                for (position, ty) in declared.iter().enumerate() {
                    if omitted.contains(&position) {
                        masks[position / 32] |= 1 << (position % 32);
                        operands.push(self.zero_of(*ty));
                    } else {
                        operands.push(supplied.next().expect("one value per supplied operand"));
                    }
                }
                for mask in masks {
                    operands.push(
                        self.builder
                            .ins()
                            .iconst(types::I32, i64::from(mask as i32)),
                    );
                }
                let mut signature = declared.to_vec();
                signature.extend(super::super::defaults::module_default_masks(declared.len()));
                (entry.as_str(), signature, operands)
            }
            _ => (export.symbol.as_str(), declared.to_vec(), arguments),
        };
        let id = self
            .file
            .import(symbol, &signature, export.signature.result())?;
        let func_ref = self.func_ref(id);
        let call = self.emit_call(func_ref, &arguments)?;
        match self.builder.inst_results(call).first().copied() {
            Some(value) => self.convert(value, Some(export.signature.result()), *ret),
            None => Ok(None),
        }
    }
}
