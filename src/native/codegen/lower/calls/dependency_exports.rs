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
        let omitted = omitted_operands(call, defaults, *extension_receiver_parameter)?;
        // The operands the call supplies, in the export's order; an omitted one is filled by the
        // declaration's default entry.
        let supplied = declared
            .iter()
            .enumerate()
            .filter(|(ordinal, _)| !omitted.contains(ordinal))
            .map(|(_, ty)| *ty)
            .collect::<Vec<_>>();
        let abi = supplied.as_slice();
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
        let (symbol, arguments, signature) = if omitted.is_empty() {
            (&export.symbol, arguments, declared.to_vec())
        } else {
            let symbol = export.default_entry.as_ref().ok_or_else(|| {
                declined!(
                    "a top-level dependency call with omitted arguments and no exported default bridge"
                )
            })?;
            let mut signature = declared.to_vec();
            signature.extend(super::super::defaults::module_default_masks(declared.len()));
            let mut masks = vec![0u32; declared.len().div_ceil(32)];
            let mut supplied = arguments.into_iter();
            let mut operands = Vec::with_capacity(signature.len());
            for (ordinal, ty) in declared.iter().enumerate() {
                if omitted.contains(&ordinal) {
                    masks[ordinal / 32] |= 1 << (ordinal % 32);
                    operands.push(self.zero_of(*ty));
                } else {
                    operands.push(supplied.next().expect("counted above"));
                }
            }
            for mask in masks {
                operands.push(
                    self.builder
                        .ins()
                        .iconst(types::I32, i64::from(mask as i32)),
                );
            }
            (symbol, operands, signature)
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

/// The export operands a call leaves out, from the value-parameter ordinals it omits. Only a
/// function whose receivers precede its value parameters takes its defaults through the entry.
fn omitted_operands(
    call: DependencyExportCall,
    defaults: &[u32],
    extension_receiver_parameter: Option<u32>,
) -> Result<Vec<usize>, Unsupported> {
    if defaults.is_empty() {
        return Ok(Vec::new());
    }
    let inserted = match call {
        DependencyExportCall::TopLevel | DependencyExportCall::SerializedExtension => None,
        DependencyExportCall::SourceExtension {
            receiver_position, ..
        } => Some(receiver_position),
        _ => return Err(declined!("a dependency member call with omitted arguments")),
    };
    defaults
        .iter()
        .map(|&ordinal| {
            // The ordinal counts value parameters only; the checked parameter list holds a member
            // extension's receiver at its own slot.
            let parameter = match extension_receiver_parameter {
                Some(receiver) if ordinal >= receiver => ordinal + 1,
                _ => ordinal,
            } as usize;
            match inserted {
                Some(position) if parameter < position => Err(declined!(
                    "a dependency call omitting an operand before its receiver"
                )),
                Some(_) => Ok(parameter + 1),
                None => Ok(parameter),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn omitted_value_parameters_follow_the_receivers_of_the_export() {
        // `fun f(a, b = …)`: the value ordinals are the export's.
        assert_eq!(
            omitted_operands(DependencyExportCall::TopLevel, &[1], None)
                .map_err(|decline| decline.construct().to_owned()),
            Ok(vec![1])
        );
        // `fun C.f(a, b = …)` called as `c.f(a)`: the receiver the call inserts precedes them.
        let extension = DependencyExportCall::SourceExtension {
            receiver: Ty::String,
            receiver_position: 0,
        };
        assert_eq!(
            omitted_operands(extension, &[0, 1], None)
                .map_err(|decline| decline.construct().to_owned()),
            Ok(vec![1, 2])
        );
        // A member extension's checked parameters hold its receiver at its own slot.
        assert_eq!(
            omitted_operands(DependencyExportCall::SerializedExtension, &[0], Some(0))
                .map_err(|decline| decline.construct().to_owned()),
            Ok(vec![1])
        );
        assert_eq!(
            omitted_operands(DependencyExportCall::Member, &[0], None)
                .map_err(|decline| decline.construct().to_owned()),
            Err("a dependency member call with omitted arguments".to_owned())
        );
    }
}
