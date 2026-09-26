use super::*;
use crate::fir::{FirIteratorCall, FirIteratorContextArgument, FirIteratorReceiver};
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
        let iterable_ty = self
            .body
            .expr(iterable)
            .ok_or_else(|| {
                self.failure(
                    span,
                    BodyCheckFailureKind::UnsupportedStatement(StatementForm::ForEach),
                )
            })?
            .ty;
        Ok(FirLoopHeader::Iterator {
            variable,
            variable_ty,
            iterable,
            iterator_ty,
            iterator: Box::new(self.iterator_protocol_call(
                self.file.stmt_spans.get(statement.0 as usize).copied(),
                origin,
                &protocol.iterator,
                iterable_ty,
            )?),
            has_next: Box::new(self.iterator_protocol_call(
                self.file.stmt_spans.get(statement.0 as usize).copied(),
                origin,
                &protocol.has_next,
                iterator_ty,
            )?),
            next: Box::new(self.iterator_protocol_call(
                self.file.stmt_spans.get(statement.0 as usize).copied(),
                origin,
                &protocol.next,
                iterator_ty,
            )?),
        })
    }

    pub(super) fn iterator_protocol_call(
        &mut self,
        span: Option<crate::diag::Span>,
        origin: OriginId,
        selected: &ResolvedCall,
        receiver_ty: ResolvedTy,
    ) -> Result<FirIteratorCall, BodyCheckFailure> {
        let selected_target = self.selected_call_target(span, Some(selected))?;
        if !selected_target.value_parameters.is_empty() || selected_target.vararg_index.is_some() {
            return Err(self.failure(
                span,
                BodyCheckFailureKind::UnsupportedStatement(StatementForm::ForEach),
            ));
        }
        let context_arguments = selected_target
            .context_arguments
            .iter()
            .zip(&selected_target.context_parameters)
            .map(|(argument, parameter_type)| {
                let receiver = self.materialize_context_argument_at(
                    span,
                    origin,
                    argument.as_ref().ok_or_else(|| {
                        self.failure(
                            span,
                            BodyCheckFailureKind::UnsupportedStatement(StatementForm::ForEach),
                        )
                    })?,
                )?;
                Ok(FirIteratorContextArgument {
                    parameter_type: *parameter_type,
                    receiver,
                })
            })
            .collect::<Result<Vec<_>, BodyCheckFailure>>()?
            .into_boxed_slice();
        let receiver = match selected {
            ResolvedCall::Member(_) => FirIteratorReceiver::Dispatch,
            ResolvedCall::Extension(_) => FirIteratorReceiver::Extension,
            ResolvedCall::MemberExtension {
                dispatch_receiver, ..
            } => {
                let dispatch_receiver = self
                    .materialize_implicit_receiver(origin, span, dispatch_receiver)?
                    .ok_or_else(|| {
                        self.failure(
                            span,
                            BodyCheckFailureKind::UnsupportedStatement(StatementForm::ForEach),
                        )
                    })?;
                FirIteratorReceiver::MemberExtension { dispatch_receiver }
            }
            ResolvedCall::TopLevel(_)
            | ResolvedCall::Companion(_)
            | ResolvedCall::LocalFunction(_) => {
                return Err(self.failure(
                    span,
                    BodyCheckFailureKind::UnsupportedStatement(StatementForm::ForEach),
                ));
            }
        };
        let declared_receiver = match (&selected_target.target, &receiver) {
            (
                FirCallTarget::Module(target),
                FirIteratorReceiver::Extension | FirIteratorReceiver::MemberExtension { .. },
            ) => Some(
                self.index
                    .callable(*target)
                    .and_then(|callable| callable.shape.extension_receiver)
                    .ok_or_else(|| {
                        self.failure(span, BodyCheckFailureKind::UnsupportedCallShape)
                    })?,
            ),
            _ => None,
        };
        let receiver_conversion = declared_receiver
            .map(|declared| {
                ResolvedTy::new(crate::types::stored_value_ty(declared.get())).map_err(|error| {
                    self.failure(span, BodyCheckFailureKind::UnpublishableType(error))
                })
            })
            .transpose()?
            .and_then(|declared| self.selected_type_conversion(receiver_ty, declared, origin));
        Ok(FirIteratorCall {
            target: selected_target.target,
            receiver,
            context_arguments,
            receiver_conversion,
        })
    }
}
