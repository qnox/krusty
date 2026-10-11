//! Translation of checker-selected `super` calls into checked FIR.

use super::super::*;

impl BodyFirChecker<'_> {
    /// The function a `super` call selected: its current-module callable, or its dependency
    /// callable. A function selection with neither has no stable target to call. An accessor has
    /// no callable of its own; its [`crate::fir::FirSuperCallKind`] names its property instead.
    fn super_declaration(
        &self,
        span: Option<Span>,
        target: &crate::resolve::ResolvedSuperCall,
        kind: crate::fir::FirSuperCallKind,
    ) -> Result<Option<crate::fir::ResolvedFunctionOverrideTarget>, BodyCheckFailure> {
        if kind != crate::fir::FirSuperCallKind::Function {
            return Ok(None);
        }
        match (target.stable_declaration, target.external_declaration) {
            (Some(declaration), _) => self
                .index
                .callable_for_declaration(declaration)
                .map(|callable| crate::fir::ResolvedFunctionOverrideTarget::Module(callable.id)),
            (None, Some(external)) => Some(crate::fir::ResolvedFunctionOverrideTarget::External(
                external,
            )),
            (None, None) => None,
        }
        .map(Some)
        .ok_or_else(|| self.failure(span, BodyCheckFailureKind::MissingStableCallTarget))
    }

    /// Check a `super`-qualified call against the supertype declaration the checker already selected.
    ///
    /// `super` is not a receiver expression: the recorded [`ImplicitReceiverSelection`] names which
    /// enclosing instance supplies `this` (a labeled `super@Outer` targets an outer one), and the
    /// callable is fixed to one supertype declaration, so dispatch must stay non-virtual.
    pub(in crate::fir::body_check) fn selected_super_call(
        &mut self,
        expression: ExprId,
        arguments: &[ExprId],
        target: &crate::resolve::ResolvedSuperCall,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let span = self.file.expr_span(expression);
        let cause = self.expression_origin(expression)?;
        self.selected_super_call_at(
            span,
            cause,
            Some(expression),
            arguments,
            target,
            crate::fir::FirSuperCallKind::Function,
        )
    }

    /// As [`Self::selected_super_call`], but for a site that is a STATEMENT rather than an
    /// expression (`super.p = v`, whose selected setter is an ordinary super call whose single
    /// argument is the assigned value).
    pub(in crate::fir::body_check) fn selected_super_call_at(
        &mut self,
        span: Option<Span>,
        cause: OriginId,
        expression: Option<ExprId>,
        arguments: &[ExprId],
        target: &crate::resolve::ResolvedSuperCall,
        kind: crate::fir::FirSuperCallKind,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let dispatch_receiver = self
            .materialize_implicit_receiver(cause, span, &target.receiver)?
            .ok_or_else(|| self.failure(span, BodyCheckFailureKind::UnsupportedCallShape))?;
        let parameters = target
            .params
            .iter()
            .copied()
            .map(|parameter| {
                ResolvedTy::new(parameter).map_err(|error| {
                    self.failure(span, BodyCheckFailureKind::UnpublishableType(error))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let checked = match expression {
            Some(expression) => self
                .call_arguments(expression, arguments, &target.params)?
                .into_vec(),
            None => {
                if arguments.len() != target.params.len() {
                    return Err(self.failure(span, BodyCheckFailureKind::UnsupportedCallShape));
                }
                arguments
                    .iter()
                    .enumerate()
                    .map(|(parameter, argument)| {
                        let value = self.expression(*argument)?;
                        Ok(FirCallArgument::Expression {
                            parameter: u32::try_from(parameter).map_err(|_| {
                                self.failure(span, BodyCheckFailureKind::UnsupportedCallShape)
                            })?,
                            value,
                            conversion: self.selected_value_conversion(
                                *argument,
                                value,
                                parameters[parameter],
                                cause,
                            )?,
                        })
                    })
                    .collect::<Result<Vec<_>, BodyCheckFailure>>()?
            }
        };
        let result = ResolvedTy::new(target.ret)
            .map_err(|error| self.failure(span, BodyCheckFailureKind::UnpublishableType(error)))?;
        let dispatch_owner = target
            .receiver
            .ty
            .non_null()
            .obj_internal()
            .ok_or_else(|| self.failure(span, BodyCheckFailureKind::UnsupportedCallShape))?;
        // The declaration's physical parameters derive the descriptor only for a source
        // declaration, which publishes none, and must then be parallel to the call's parameters.
        // A provider-described declaration keeps its own ABI shape (a dependency suspend member
        // ends with the classfile's CPS continuation), which the target consumes as published.
        if target.descriptor.is_empty() && target.physical_params.len() != parameters.len() {
            return Err(self.failure(span, BodyCheckFailureKind::UnsupportedCallShape));
        }
        let declaration_parameters = target
            .physical_params
            .iter()
            .copied()
            .map(|parameter| {
                ResolvedTy::new(parameter).map_err(|error| {
                    self.failure(span, BodyCheckFailureKind::UnpublishableType(error))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(FirExprKind::Call(FirCall {
            target: FirCallTarget::Super {
                owner: target.owner,
                dispatch_owner,
                enclosing_dispatch: !target.receiver.current,
                kind,
                name: target.name.clone(),
                parameters: declaration_parameters.into_boxed_slice(),
                result,
                interface: target.interface,
                realization: target.realization,
                descriptor: target.descriptor.clone(),
                physical_result: ResolvedTy::new(target.physical_ret).map_err(|error| {
                    self.failure(span, BodyCheckFailureKind::UnpublishableType(error))
                })?,
                declaration: self.super_declaration(span, target, kind)?,
                source_member: target.source_member,
                suspend: target.suspend,
            },
            dispatch_receiver: Some(dispatch_receiver),
            extension_receiver: None,
            parameter_types: parameters.into_boxed_slice(),
            arguments: checked.into_boxed_slice(),
            substitutions: Box::new([]),
        }))
    }
}
