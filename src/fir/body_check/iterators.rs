use super::*;
use crate::fir::{FirIteratorCall, FirIteratorContextArgument, FirIteratorReceiver};
use crate::resolve::ResolvedCall;

impl BodyFirChecker<'_> {
    /// A `for` loop's own variable. A destructuring loop's (`for ((a, b) in xs)`) is a compiler-
    /// generated container that only the prepended destructuring reads: it carries that role and
    /// no source name.
    pub(super) fn loop_variable(&mut self, statement: StmtId, name: &str) -> LocalValueId {
        let variable = self.allocate_local();
        if self.file.destructuring.loops.contains_key(&statement) {
            self.body.mark_destructuring_loop_container(variable);
        } else {
            self.body.set_debug_value_name(variable, name);
        }
        variable
    }

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
        let iterator_enhanced =
            has_enhanced_result(&protocol.iterator) || protocol.enhancement.iterator.head();
        let mut iterator =
            self.iterator_protocol_call(span, origin, &protocol.iterator, iterable_ty)?;
        let has_next =
            self.iterator_protocol_call(span, origin, &protocol.has_next, iterator_ty)?;
        let mut next = self.iterator_protocol_call(span, origin, &protocol.next, iterator_ty)?;
        if iterator_enhanced {
            iterator.result_check = self.protocol_result_check(&protocol.iterator);
        }
        // The checker substituted the iterable's marks through `iterator()` and `next()`: a Java
        // `Iterator<E>` enhanced from `MutableIterator<E>` yields a marked `E`.
        let element_enhanced =
            has_enhanced_result(&protocol.next) || protocol.enhancement.element.head();
        // kotlinc's `acceptsNullValues`: a type parameter whose bound admits `null` accepts it.
        let element = variable_ty.get();
        let element_rejects_null = !element.admits_null()
            && !element.upper_bound_admits_null()
            && (element.is_reference() || element.is_jvm_scalar());
        if element_enhanced && element_rejects_null {
            next.result_check = self.protocol_result_check(&protocol.next);
        }
        Ok(FirLoopHeader::Iterator {
            variable,
            variable_ty,
            iterable,
            iterator_ty,
            iterator: Box::new(iterator),
            has_next: Box::new(has_next),
            next: Box::new(next),
        })
    }

    /// The not-null check a stored protocol result gets, named like any checked call result.
    fn protocol_result_check(
        &mut self,
        selected: &ResolvedCall,
    ) -> Option<crate::fir::FirPlatformNarrowingId> {
        let ResolvedCall::Member(member) = selected else {
            return None;
        };
        let message = format!("{}(...)", member.member.name).into_boxed_str();
        Some(
            self.body
                .add_platform_narrowing(crate::fir::FirPlatformNarrowing {
                    message: Some(message),
                }),
        )
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
            result_check: None,
        })
    }
}

/// Whether a selected protocol call's declaration result is enhanced to not-null.
fn has_enhanced_result(selected: &ResolvedCall) -> bool {
    matches!(
        selected,
        ResolvedCall::Member(member)
            if member.member.call_sig.result_enhancement
                == crate::libraries::ResultEnhancement::NotNull
    )
}
