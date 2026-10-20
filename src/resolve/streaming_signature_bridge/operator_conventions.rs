//! Ordinary operator-convention selection for compact signature evaluation.
//!
//! This path shares the scoped receiver, context-parameter, and member-extension families used by
//! body checking. Delegated-property conventions have different context rules and must not be used
//! to decide whether a source compound assignment is in-place.

use super::*;
use crate::types::TyKind;

enum ReceiverOperatorRung {
    Selected {
        callable: crate::libraries::FunctionInfo,
        parameters: Vec<Ty>,
        result: Ty,
        argument_types: Vec<Ty>,
    },
    Ambiguous,
}

impl ProductionSignatureSemantics<'_> {
    pub(super) fn select_operator_convention(
        &self,
        scope: crate::fir::SignatureScope,
        origin: crate::fir::OriginId,
        receiver: crate::fir::ResolvedTy,
        spelling: &str,
        arguments: &[crate::fir::ResolvedSigCallArgument<'_>],
        local_operator: Option<crate::fir::ResolvedTy>,
        demand: &mut dyn FnMut(
            crate::fir::DeclarationId,
        )
            -> Result<crate::fir::ResolvedSignature, crate::fir::DiagnosticId>,
    ) -> Result<crate::fir::ResolvedTy, crate::fir::DiagnosticId> {
        if let Some(result) =
            self.select_receiver_operator_rung(scope, receiver, spelling, arguments, true, demand)?
        {
            return Ok(result);
        }
        if let Some(local_operator) = local_operator {
            if let Some(result) =
                self.select_local_operator(scope, local_operator, receiver, arguments)
            {
                return crate::fir::ResolvedTy::new(result).map_err(|_| Self::failure());
            }
        }

        let argument_names = arguments
            .iter()
            .map(|argument| argument.name.map(str::to_owned))
            .collect::<Vec<_>>();
        let argument_names = argument_names
            .iter()
            .any(Option::is_some)
            .then_some(argument_names.as_slice());
        let argument_kinds = arguments
            .iter()
            .map(Self::call_argument_kind)
            .collect::<Vec<_>>();
        for dispatch in self.signature_dispatch_receivers(scope) {
            let selected = self.signature_member_extension_call(
                scope,
                receiver.get(),
                dispatch,
                spelling,
                lookups::SignatureMemberExtensionArguments {
                    arguments: &argument_kinds,
                    names: argument_names,
                    ..Default::default()
                },
                super::super::CallResultConstraint::direct(None),
                super::super::MemberExtensionSelection::Operators,
            );
            let (result, declaration) = match selected {
                lookups::SignatureMemberExtensionCallSelection::Selected {
                    result,
                    declaration,
                } => (result, declaration),
                lookups::SignatureMemberExtensionCallSelection::None(_) => continue,
                lookups::SignatureMemberExtensionCallSelection::Ambiguous(_) => {
                    return Err(self.record_ambiguous_member(scope.owner, origin, spelling));
                }
            };
            if let Some(declaration) = declaration {
                if self
                    .headers
                    .stub(declaration)
                    .is_some_and(|stub| stub.signature_inference.is_some())
                {
                    return demand(declaration).map(|signature| signature.result);
                }
            }
            return crate::fir::ResolvedTy::new(result).map_err(|_| Self::failure());
        }
        self.select_receiver_operator_rung(scope, receiver, spelling, arguments, false, demand)?
            .ok_or_else(Self::failure)
    }

    fn select_receiver_operator_rung(
        &self,
        scope: crate::fir::SignatureScope,
        receiver: crate::fir::ResolvedTy,
        spelling: &str,
        arguments: &[crate::fir::ResolvedSigCallArgument<'_>],
        members: bool,
        demand: &mut dyn FnMut(
            crate::fir::DeclarationId,
        )
            -> Result<crate::fir::ResolvedSignature, crate::fir::DiagnosticId>,
    ) -> Result<Option<crate::fir::ResolvedTy>, crate::fir::DiagnosticId> {
        let selected = self.with_resolver(scope, |resolver| {
            let (mut functions, _) = resolver
                .receiver_callables(receiver.get(), spelling)
                .into_parts();
            functions.overloads.retain(|candidate| {
                candidate.flags.operator
                    && (candidate.kind == crate::libraries::FnKind::Member) == members
            });
            functions.overloads = self.implicit_context_candidates(scope, functions.overloads);
            let callables = crate::libraries::Callables::Functions(functions);
            let (argument_kinds, argument_types) =
                Self::mapped_call_arguments(callables.functions(), arguments, false)?;
            match resolver.select_receiver_function_with_params_tracking(
                receiver.get(),
                spelling,
                &argument_kinds,
                &[],
                &callables,
                None,
            ) {
                crate::symbol_resolver::CandidateSelection::Selected((
                    selected,
                    parameters,
                    result,
                )) => Some(ReceiverOperatorRung::Selected {
                    callable: selected,
                    parameters,
                    result,
                    argument_types,
                }),
                crate::symbol_resolver::CandidateSelection::None => None,
                crate::symbol_resolver::CandidateSelection::Ambiguous => {
                    Some(ReceiverOperatorRung::Ambiguous)
                }
            }
        });
        let (selected, parameters, result, argument_types) = match selected {
            Ok(ReceiverOperatorRung::Selected {
                callable,
                parameters,
                result,
                argument_types,
            }) => (callable, parameters, result, argument_types),
            Ok(ReceiverOperatorRung::Ambiguous) => return Err(Self::failure()),
            Err(_) => return Ok(None),
        };
        if members {
            let mut member = selected.member_with_return(result);
            member.params = parameters;
            if let Some(signature) =
                self.demanded_member_signature(member.stable_declaration, demand)?
            {
                return self
                    .apply_demanded_member(
                        receiver.get(),
                        &member,
                        &signature,
                        &argument_types,
                        &[],
                    )
                    .map(Some);
            }
        } else if let Some(source) = selected.source_key {
            if let Some(signature) =
                self.demanded_source_signature(None, selected.stable_declaration, demand)?
            {
                return self
                    .apply_demanded_source_callable(
                        source,
                        Some(receiver.get()),
                        &signature,
                        &argument_types,
                        None,
                        &[],
                        None,
                    )
                    .map(Some);
            }
        }
        crate::fir::ResolvedTy::new(result)
            .map(Some)
            .map_err(|_| Self::failure())
    }

    fn select_local_operator(
        &self,
        scope: crate::fir::SignatureScope,
        local_operator: crate::fir::ResolvedTy,
        receiver: crate::fir::ResolvedTy,
        arguments: &[crate::fir::ResolvedSigCallArgument<'_>],
    ) -> Option<Ty> {
        let TyKind::Fun(signature) = local_operator.get().non_null().kind() else {
            return None;
        };
        let context_count = signature.context_count.min(signature.params.len());
        let explicit = signature.params.get(context_count..)?;
        if !signature.has_receiver || explicit.len() != arguments.len() + 1 {
            return None;
        }
        let module = crate::module_symbols::ModuleSymbols::for_file(self.table, scope.source.raw());
        let source = crate::symbol_source::CompositeSource::new(vec![
            &module as &dyn crate::symbol_source::SymbolSource,
            &*self.table.libraries as &dyn crate::symbol_source::SymbolSource,
        ]);
        let oracle = crate::symbol_resolver::SourceOracle(&source);
        if super::super::context_argument_types(
            &self.implicit_receivers(scope),
            &signature.params[..context_count],
            &oracle,
        )
        .is_none()
        {
            return None;
        }
        let context = crate::assignable::TyCtx::new();
        let operands = std::iter::once(receiver.get())
            .chain(arguments.iter().map(|argument| argument.ty.get()));
        operands
            .zip(explicit.iter().copied())
            .all(|(actual, target)| {
                crate::assignable::is_assignable(&context, &oracle, actual, target)
            })
            .then_some(signature.ret)
    }
}

pub(super) fn selects_in_place_assignment(
    semantics: &ProductionSignatureSemantics<'_>,
    scope: crate::fir::SignatureScope,
    operator: crate::fir::SigBinaryOperator,
    origin: crate::fir::OriginId,
    receiver: crate::fir::ResolvedTy,
    argument: crate::fir::ResolvedTy,
    local_operator: Option<crate::fir::ResolvedTy>,
    demand: &mut dyn FnMut(
        crate::fir::DeclarationId,
    ) -> Result<crate::fir::ResolvedSignature, crate::fir::DiagnosticId>,
) -> bool {
    let spelling = match operator {
        crate::fir::SigBinaryOperator::Add => "plusAssign",
        crate::fir::SigBinaryOperator::Subtract => "minusAssign",
        crate::fir::SigBinaryOperator::Multiply => "timesAssign",
        crate::fir::SigBinaryOperator::Divide => "divAssign",
        crate::fir::SigBinaryOperator::Remainder => "remAssign",
        crate::fir::SigBinaryOperator::Equal
        | crate::fir::SigBinaryOperator::NotEqual
        | crate::fir::SigBinaryOperator::Less
        | crate::fir::SigBinaryOperator::LessOrEqual
        | crate::fir::SigBinaryOperator::Greater
        | crate::fir::SigBinaryOperator::GreaterOrEqual
        | crate::fir::SigBinaryOperator::BooleanAnd
        | crate::fir::SigBinaryOperator::BooleanOr
        | crate::fir::SigBinaryOperator::ReferentialEqual
        | crate::fir::SigBinaryOperator::ReferentialNotEqual => return false,
    };
    let arguments = [crate::fir::ResolvedSigCallArgument {
        ty: argument,
        origin,
        name: None,
        spread: false,
        integer_literal: None,
        lambda: false,
        contextual_call: false,
        wrapped_contextual_call: false,
    }];
    semantics
        .select_operator_convention(
            scope,
            origin,
            receiver,
            spelling,
            &arguments,
            local_operator,
            demand,
        )
        .is_ok_and(|result| result.get() == Ty::Unit)
}

pub(super) fn select_binary(
    semantics: &ProductionSignatureSemantics<'_>,
    scope: crate::fir::SignatureScope,
    operator: crate::fir::SigBinaryOperator,
    origin: crate::fir::OriginId,
    lhs: crate::fir::ResolvedTy,
    rhs: crate::fir::ResolvedTy,
    demand: &mut dyn FnMut(
        crate::fir::DeclarationId,
    ) -> Result<crate::fir::ResolvedSignature, crate::fir::DiagnosticId>,
) -> Result<crate::fir::ResolvedTy, crate::fir::DiagnosticId> {
    let convention = match operator {
        crate::fir::SigBinaryOperator::Add => Some(("plus", false)),
        crate::fir::SigBinaryOperator::Subtract => Some(("minus", false)),
        crate::fir::SigBinaryOperator::Multiply => Some(("times", false)),
        crate::fir::SigBinaryOperator::Divide => Some(("div", false)),
        crate::fir::SigBinaryOperator::Remainder => Some(("rem", false)),
        crate::fir::SigBinaryOperator::Less
        | crate::fir::SigBinaryOperator::LessOrEqual
        | crate::fir::SigBinaryOperator::Greater
        | crate::fir::SigBinaryOperator::GreaterOrEqual => Some(("compareTo", true)),
        crate::fir::SigBinaryOperator::Equal
        | crate::fir::SigBinaryOperator::NotEqual
        | crate::fir::SigBinaryOperator::BooleanAnd
        | crate::fir::SigBinaryOperator::BooleanOr
        | crate::fir::SigBinaryOperator::ReferentialEqual
        | crate::fir::SigBinaryOperator::ReferentialNotEqual => None,
    };
    if let Some((name, comparison)) = convention {
        let selected =
            semantics
                .with_resolver(scope, |resolver| {
                    match super::super::select_delegate_operator(
                    resolver,
                    lhs.get(),
                    name,
                    &[rhs.get()],
                ) {
                    crate::resolve::delegated_properties::DelegateConventionSelection::Selected(
                        selected,
                        result,
                    ) => Some((selected, result)),
                    crate::resolve::delegated_properties::DelegateConventionSelection::None(_)
                    | crate::resolve::delegated_properties::DelegateConventionSelection::Ambiguous(
                        _,
                    ) => None,
                }
                })
                .ok();
        if let Some((selected, result)) = selected {
            let selected_result = semantics.selected_convention_result(
                lhs.get(),
                &selected,
                result,
                &[rhs.get()],
                None,
                demand,
            )?;
            if comparison {
                if selected_result.get() != Ty::Int {
                    return Err(ProductionSignatureSemantics::failure());
                }
                return crate::fir::ResolvedTy::new(Ty::Boolean)
                    .map_err(|_| ProductionSignatureSemantics::failure());
            }
            return Ok(selected_result);
        }
    }
    let operator = match operator {
        crate::fir::SigBinaryOperator::Add => crate::ast::BinOp::Add,
        crate::fir::SigBinaryOperator::Subtract => crate::ast::BinOp::Sub,
        crate::fir::SigBinaryOperator::Multiply => crate::ast::BinOp::Mul,
        crate::fir::SigBinaryOperator::Divide => crate::ast::BinOp::Div,
        crate::fir::SigBinaryOperator::Remainder => crate::ast::BinOp::Rem,
        crate::fir::SigBinaryOperator::Equal => crate::ast::BinOp::Eq,
        crate::fir::SigBinaryOperator::NotEqual => crate::ast::BinOp::Ne,
        crate::fir::SigBinaryOperator::Less => crate::ast::BinOp::Lt,
        crate::fir::SigBinaryOperator::LessOrEqual => crate::ast::BinOp::Le,
        crate::fir::SigBinaryOperator::Greater => crate::ast::BinOp::Gt,
        crate::fir::SigBinaryOperator::GreaterOrEqual => crate::ast::BinOp::Ge,
        crate::fir::SigBinaryOperator::BooleanAnd => crate::ast::BinOp::And,
        crate::fir::SigBinaryOperator::BooleanOr => crate::ast::BinOp::Or,
        crate::fir::SigBinaryOperator::ReferentialEqual => crate::ast::BinOp::RefEq,
        crate::fir::SigBinaryOperator::ReferentialNotEqual => crate::ast::BinOp::RefNe,
    };
    semantics.checked_binary(scope, origin, operator, lhs.get(), rhs.get())
}
