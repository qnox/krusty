use super::*;
use crate::fir::{FirIteratorCall, FirIteratorReceiver};
use crate::resolve::ResolvedCall;

impl BodyFirChecker<'_> {
    pub(super) fn iterator_loop_header(
        &mut self,
        statement: StmtId,
        iterable_source: ExprId,
        variable: LocalValueId,
        variable_ty: ResolvedTy,
        iterable: FirExprId,
    ) -> Result<FirLoopHeader, BodyCheckFailure> {
        let span = self.file.stmt_spans.get(statement.0 as usize).copied();
        let protocol = self
            .info
            .iterator_protocol(iterable_source)
            .ok_or_else(|| {
                self.failure(
                    span,
                    BodyCheckFailureKind::UnsupportedStatement(StatementForm::ForEach),
                )
            })?;
        self.iterator_loop_header_from_protocol(
            statement,
            variable,
            variable_ty,
            iterable,
            protocol,
        )
    }

    pub(super) fn iterator_loop_header_from_protocol(
        &mut self,
        statement: StmtId,
        variable: LocalValueId,
        variable_ty: ResolvedTy,
        iterable: FirExprId,
        protocol: &crate::resolve::IteratorProtocolTarget,
    ) -> Result<FirLoopHeader, BodyCheckFailure> {
        let span = self.file.stmt_spans.get(statement.0 as usize).copied();
        let iterator_ty = ResolvedTy::new(protocol.iter_ty)
            .map_err(|error| self.failure(span, BodyCheckFailureKind::UnpublishableType(error)))?;
        let origin = self.statement_origin(statement)?;
        Ok(FirLoopHeader::Iterator {
            variable,
            variable_ty,
            iterable,
            iterator_ty,
            iterator: self.iterator_protocol_call(
                self.file.stmt_spans.get(statement.0 as usize).copied(),
                origin,
                &protocol.iterator,
            )?,
            has_next: self.iterator_protocol_call(
                self.file.stmt_spans.get(statement.0 as usize).copied(),
                origin,
                &protocol.has_next,
            )?,
            next: self.iterator_protocol_call(
                self.file.stmt_spans.get(statement.0 as usize).copied(),
                origin,
                &protocol.next,
            )?,
        })
    }

    pub(super) fn iterator_protocol_call(
        &mut self,
        span: Option<crate::diag::Span>,
        origin: OriginId,
        selected: &ResolvedCall,
    ) -> Result<FirIteratorCall, BodyCheckFailure> {
        let result = selected.ret();
        let (stable_declaration, external_declaration, receiver_ty, receiver, declared_result) =
            match selected {
                ResolvedCall::Member(selected)
                    if selected.member.params.is_empty() && selected.context_args.is_empty() =>
                {
                    (
                        selected.member.stable_declaration,
                        selected.member.external_identity,
                        selected.receiver,
                        FirIteratorReceiver::Dispatch,
                        selected.member.declared_ret,
                    )
                }
                ResolvedCall::Extension(selected)
                    if selected.params.is_empty() && selected.context_args.is_empty() =>
                {
                    (
                        selected.stable_declaration,
                        selected.callable.external_identity,
                        selected.receiver,
                        FirIteratorReceiver::Extension,
                        selected.callable.declared_ret,
                    )
                }
                ResolvedCall::MemberExtension {
                    stable_declaration,
                    external_identity,
                    dispatch_receiver,
                    extension_receiver,
                    params,
                    context_args,
                    ret,
                    inline,
                    inline_body_plan,
                    suspend,
                    declared_ret,
                    vararg_index,
                    ..
                } if params.is_empty() && context_args.is_empty() && vararg_index.is_none() => {
                    let dispatch_ty = dispatch_receiver.ty;
                    let dispatch_receiver = self
                        .materialize_implicit_receiver(origin, span, dispatch_receiver)?
                        .ok_or_else(|| {
                            self.failure(
                                span,
                                BodyCheckFailureKind::UnsupportedStatement(StatementForm::ForEach),
                            )
                        })?;
                    let target = if let Some(declaration) = stable_declaration {
                        self.index
                            .callable_for_declaration(*declaration)
                            .map(|callable| FirCallTarget::Module(callable.id))
                            .ok_or_else(|| {
                                self.failure(span, BodyCheckFailureKind::MissingStableCallTarget)
                            })?
                    } else {
                        FirCallTarget::External {
                            declaration: external_identity.ok_or_else(|| {
                                self.failure(span, BodyCheckFailureKind::MissingStableCallTarget)
                            })?,
                            receiver: Some(ResolvedTy::new(dispatch_ty).map_err(|error| {
                                self.failure(span, BodyCheckFailureKind::UnpublishableType(error))
                            })?),
                            declared_receiver: None,
                            parameters: vec![ResolvedTy::new(*extension_receiver).map_err(
                                |error| {
                                    self.failure(
                                        span,
                                        BodyCheckFailureKind::UnpublishableType(error),
                                    )
                                },
                            )?]
                            .into_boxed_slice(),
                            result: ResolvedTy::new(*ret).map_err(|error| {
                                self.failure(span, BodyCheckFailureKind::UnpublishableType(error))
                            })?,
                            declared_result: declared_ret
                                .map(ResolvedTy::new)
                                .transpose()
                                .map_err(|error| {
                                    self.failure(
                                        span,
                                        BodyCheckFailureKind::UnpublishableType(error),
                                    )
                                })?,
                            suspend: *suspend,
                            can_inline: inline.can_inline(),
                            inline_plan: super::calls::fir_inline_body_plan(
                                inline_body_plan.as_deref(),
                            ),
                            extension_receiver_parameter: Some(0),
                        }
                    };
                    return Ok(FirIteratorCall {
                        target,
                        receiver: FirIteratorReceiver::MemberExtension { dispatch_receiver },
                    });
                }
                ResolvedCall::TopLevel(_)
                | ResolvedCall::Companion(_)
                | ResolvedCall::LocalFunction(_)
                | ResolvedCall::Member(_)
                | ResolvedCall::Extension(_)
                | ResolvedCall::MemberExtension { .. } => {
                    return Err(self.failure(
                        span,
                        BodyCheckFailureKind::UnsupportedStatement(StatementForm::ForEach),
                    ));
                }
            };
        let resolved = |ty| {
            ResolvedTy::new(ty)
                .map_err(|error| self.failure(span, BodyCheckFailureKind::UnpublishableType(error)))
        };
        let target = if let Some(declaration) = stable_declaration {
            self.index
                .callable_for_declaration(declaration)
                .map(|callable| FirCallTarget::Module(callable.id))
                .ok_or_else(|| self.failure(span, BodyCheckFailureKind::MissingStableCallTarget))?
        } else {
            crate::trace_compiler!(
                "fir",
                "checked iterator protocol target selected={selected:?} external={external_declaration:?}"
            );
            FirCallTarget::External {
                declaration: external_declaration.ok_or_else(|| {
                    self.failure(span, BodyCheckFailureKind::MissingStableCallTarget)
                })?,
                receiver: Some(resolved(receiver_ty)?),
                declared_receiver: None,
                parameters: Box::new([]),
                result: resolved(result)?,
                declared_result: declared_result.map(resolved).transpose()?,
                suspend: false,
                can_inline: false,
                inline_plan: None,
                extension_receiver_parameter: None,
            }
        };
        Ok(FirIteratorCall { target, receiver })
    }
}
