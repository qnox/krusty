//! Exact dependency exports own calls and selected accessor operations before target primitives.
//! Checked instantiation proves operand roles; the stored export signature owns physical carriers.

use crate::backend::{instantiate_dependency_signature, DependencyCallShape, DependencyExportCall};
use crate::ir::IrCheckedOperation;

use super::*;

impl Body<'_> {
    pub(super) fn emitted_dependency_expression(
        &mut self,
        site: u32,
        node: &IrExpr,
    ) -> Option<Result<(), Unsupported>> {
        match node {
            IrExpr::Call {
                callee,
                dispatch_receiver,
                args,
            } if let Callee::External { target, .. } = &**callee
                && self.calls.external(*target).is_some() =>
            {
                Some(self.emitted_dependency_call(site, callee, *dispatch_receiver, args))
            }
            IrExpr::Checked(operation) => self.emitted_dependency_property(site, operation),
            _ => None,
        }
    }

    fn emitted_dependency_property(
        &mut self,
        site: u32,
        operation: &IrCheckedOperation,
    ) -> Option<Result<(), Unsupported>> {
        let access = self.callables.dependency_accessor(operation)?;
        self.calls.external(access.target)?;
        Some((|| {
            let callee = access.callee().map_err(str::to_owned)?;
            self.emitted_dependency_call(site, &callee, access.receiver, access.arguments)
        })())
    }

    fn emitted_dependency_call(
        &mut self,
        site: u32,
        callee: &Callee,
        dispatch_receiver: Option<u32>,
        arguments: &[u32],
    ) -> Result<(), Unsupported> {
        let Callee::External {
            target,
            params,
            ret,
            substitutions,
            defaults,
            extension_receiver_parameter,
            ..
        } = callee
        else {
            return Err("an emitted dependency call without a checked external identity".into());
        };
        let default_entry = match self.calls.external_default(*target) {
            _ if defaults.is_empty() => None,
            Some(entry) => Some(entry),
            None => {
                return Err(
                    "a dependency call with omitted arguments and no exported default bridge"
                        .into(),
                )
            }
        };
        if arguments.len() + defaults.len() != params.len() {
            return Err(
                "a dependency call whose operands disagree with its checked parameters".into(),
            );
        }
        let function = self
            .calls
            .external(*target)
            .ok_or_else(|| "an unregistered dependency callable identity".to_owned())?;
        let signature = self
            .calls
            .signature(*target)
            .ok_or_else(|| "an emitted dependency without its declared signature".to_owned())?
            .clone();
        let callable = self
            .callables
            .callable(*target)
            .ok_or_else(|| "a dependency call without frozen callable facts".to_owned())?;
        let instantiated = instantiate_dependency_signature(callable, *target, substitutions)
            .map_err(str::to_owned)?;
        if callable
            .generic_sig
            .as_ref()
            .is_some_and(|s| !s.formals.is_empty())
            && instantiated.result != *ret
        {
            return Err("a dependency generic result disagrees with its checked call".into());
        }
        let unread = self.calls.singleton_extension_unread_receiver(*target);
        let singleton = self.calls.singleton_member_receiver(*target);
        let selected = self.callables.singleton_dispatch(site);
        let exact_singleton =
            if super::super::callable_registry::authenticated_singleton_member_call(
                dispatch_receiver,
                selected.is_some(),
                callable.is_member_function(),
            ) {
                selected
                    .zip(singleton)
                    .is_some_and(|(selected, effect)| selected.classifier == effect.classifier())
            } else if callable.is_extension() {
                dispatch_receiver.is_some_and(|receiver| {
                    unread.is_some_and(|effect| {
                        crate::backend::authenticated_singleton_receiver(
                            Some(receiver),
                            selected.map(|s| s.classifier),
                            effect.semantic_type(),
                        )
                    })
                })
            } else {
                false
            };
        let call = DependencyCallShape {
            is_top_level_function: callable.is_top_level_function(),
            is_extension: callable.is_extension(),
            source_receiver: instantiated.receiver,
            context_count: callable.context_count,
            emitted_params: &instantiated.parameters,
            call_params: params,
            extension_receiver_parameter: *extension_receiver_parameter,
            has_dispatch_receiver: dispatch_receiver.is_some(),
            unread_receiver: unread.map(|effect| effect.semantic_type()),
            companion_receiver_position: callable.klib_body_companion_receiver_position(),
            member_receiver: self
                .calls
                .member_receiver(*target)
                .map(|r| r.semantic_type()),
            receiver_is_exact_singleton: exact_singleton,
        }
        .authenticate()
        .map_err(str::to_owned)?;
        let declared = signature.parameters();
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
        let omitted = match call {
            _ if defaults.is_empty() => Vec::new(),
            DependencyExportCall::TopLevel | DependencyExportCall::SerializedExtension => {
                defaults.iter().map(|&ordinal| ordinal as usize).collect()
            }
            DependencyExportCall::SourceExtension {
                receiver_position, ..
            } => defaults
                .iter()
                .map(|&ordinal| ordinal as usize)
                .map(|ordinal| ordinal + usize::from(ordinal >= receiver_position))
                .collect::<Vec<_>>(),
            _ => {
                return Err(
                    "a dependency call with omitted arguments through a receiver-erased export"
                        .into(),
                )
            }
        };
        if omitted.iter().any(|&position| position >= declared.len()) {
            return Err("a dependency call omitting an operand its export does not declare".into());
        }
        let abi = declared;
        match call {
            DependencyExportCall::TopLevel | DependencyExportCall::SerializedExtension => {
                self.dependency_arguments_omitting(arguments, abi, &omitted)?;
            }
            DependencyExportCall::SerializedClassifierAssociated { receiver } => {
                if !matches!(carrier(receiver)?, Some(ValType::Ref { .. })) {
                    return Err(
                        "a classifier-associated extension receiver without a reference carrier"
                            .into(),
                    );
                }
                self.dependency_arguments(arguments, abi)?;
            }
            DependencyExportCall::SingletonMember => {
                let effect = singleton.ok_or_else(|| {
                    "a receiver-erased dependency member without its singleton effect".to_owned()
                })?;
                if effect.evaluate_source() {
                    self.statement(dispatch_receiver.expect("validated singleton receiver"))?;
                }
                self.dependency_arguments(arguments, abi)?;
            }
            DependencyExportCall::Member | DependencyExportCall::MemberExtension => {
                self.value(
                    dispatch_receiver.expect("validated member receiver"),
                    abi[0],
                )?;
                self.dependency_arguments(arguments, &abi[1..])?;
            }
            DependencyExportCall::SourceExtension {
                receiver_position, ..
            } => {
                self.dependency_arguments(
                    &arguments[..receiver_position],
                    &abi[..receiver_position],
                )?;
                self.value(
                    dispatch_receiver.expect("validated extension receiver"),
                    abi[receiver_position],
                )?;
                let values = receiver_position + 1;
                let omitted = omitted
                    .iter()
                    .map(|&position| position - values)
                    .collect::<Vec<_>>();
                self.dependency_arguments_omitting(
                    &arguments[receiver_position..],
                    &abi[values..],
                    &omitted,
                )?;
            }
            DependencyExportCall::SourceSingletonExtension {
                receiver,
                receiver_position,
            }
            | DependencyExportCall::ClassifierAssociated {
                receiver,
                receiver_position,
            } => {
                if !matches!(carrier(receiver)?, Some(ValType::Ref { .. }))
                    || !matches!(carrier(abi[receiver_position])?, Some(ValType::Ref { .. }))
                {
                    return Err("an unread extension receiver without a reference carrier".into());
                }
                self.dependency_arguments(
                    &arguments[..receiver_position],
                    &abi[..receiver_position],
                )?;
                if matches!(call, DependencyExportCall::SourceSingletonExtension { .. })
                    && unread.is_none_or(|effect| effect.evaluate_source())
                {
                    self.statement(
                        dispatch_receiver.expect("validated singleton extension receiver"),
                    )?;
                }
                self.code.ref_null(HeapType::None);
                self.dependency_arguments(
                    &arguments[receiver_position..],
                    &abi[receiver_position + 1..],
                )?;
            }
        }
        match default_entry {
            Some(entry) => {
                for word in 0..abi.len().div_ceil(32) {
                    let mask = omitted
                        .iter()
                        .filter(|&&position| position / 32 == word)
                        .fold(0_u32, |mask, &position| mask | 1 << (position % 32));
                    self.code.i32_const(mask as i32);
                }
                self.code.call(entry);
            }
            None => {
                self.code.call(function);
            }
        }
        self.coerce_emitted_value(signature.result(), *ret)
    }

    /// The operands of `abi` in order: each supplied one evaluated, and a zero at each `omitted`
    /// position, whose default the declaration's default entry evaluates.
    fn dependency_arguments_omitting(
        &mut self,
        arguments: &[u32],
        abi: &[Ty],
        omitted: &[usize],
    ) -> Result<(), Unsupported> {
        if arguments.len() + omitted.len() != abi.len() {
            return Err(
                "a dependency export whose declared operand count disagrees with its call".into(),
            );
        }
        let mut supplied = arguments.iter();
        for (position, &parameter) in abi.iter().enumerate() {
            if omitted.contains(&position) {
                let carrier = carrier(parameter)?
                    .ok_or_else(|| "a defaulted call with a `Unit` parameter".to_string())?;
                super::default_arguments::push_zero(&mut self.code, carrier);
            } else {
                let argument = supplied.next().expect("counted above");
                self.value(*argument, parameter)?;
            }
        }
        Ok(())
    }

    fn dependency_arguments(&mut self, arguments: &[u32], abi: &[Ty]) -> Result<(), Unsupported> {
        if arguments.len() != abi.len() {
            return Err(
                "a dependency export whose declared operand count disagrees with its call".into(),
            );
        }
        for (&argument, &parameter) in arguments.iter().zip(abi) {
            self.value(argument, parameter)?;
        }
        Ok(())
    }
}
