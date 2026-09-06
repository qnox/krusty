//! Checked FIR for local delegated properties.

use super::*;
use crate::resolve::DelegateGetValueTarget;

impl BodyFirChecker<'_> {
    pub(super) fn local_delegate_statement(
        &mut self,
        statement: StmtId,
        mutable: bool,
        name: &str,
        explicit_type: Option<&crate::ast::TypeRef>,
        delegate: ExprId,
        origin: OriginId,
    ) -> Result<FirStatementId, BodyCheckFailure> {
        let span = self.file.stmt_spans.get(statement.0 as usize).copied();
        let delegate_ty = self.expression_type(delegate)?;
        let property_ty = match explicit_type {
            Some(ty) => self.info.resolved_type(ty).ok_or_else(|| {
                self.failure(Some(ty.span), BodyCheckFailureKind::UnresolvedTypeSyntax)
            })?,
            None => *self.info.local_decl_types.get(&statement).ok_or_else(|| {
                self.failure(
                    span,
                    BodyCheckFailureKind::UnsupportedStatement(StatementForm::LocalDelegate),
                )
            })?,
        };
        let property_ty = self.resolved_type(
            span.ok_or_else(|| self.failure(None, BodyCheckFailureKind::MissingSourceSpan))?,
            property_ty,
        )?;
        let get_value = self
            .info
            .delegate_getvalue(delegate)
            .ok_or_else(|| self.failure(span, BodyCheckFailureKind::MissingStableCallTarget))?;
        let get_value = self.delegate_call_target(delegate, delegate_ty, get_value)?;
        let set_value = if mutable {
            let target = self
                .info
                .delegate_setvalue(delegate)
                .ok_or_else(|| self.failure(span, BodyCheckFailureKind::MissingStableCallTarget))?;
            Some(self.delegate_call_target(delegate, delegate_ty, target)?)
        } else {
            None
        };

        let mut initializer = self.expression(delegate)?;
        let storage_ty = if let Some(provide) = self.info.delegate_provide(delegate) {
            let provide = self.delegate_call_target(delegate, delegate_ty, provide)?;
            let property = self.local_property_reference(origin, name, property_ty);
            let owner = self.synthetic_null(origin);
            let result = provide.result;
            initializer = self.body.add_expr(FirExpr {
                origin,
                ty: result,
                kind: FirExprKind::Call(FirCall {
                    target: provide.target,
                    dispatch_receiver: (!provide.extension).then_some(FirReceiver {
                        value: initializer,
                        conversion: None,
                    }),
                    extension_receiver: provide.extension.then_some(FirReceiver {
                        value: initializer,
                        conversion: None,
                    }),
                    parameter_types: provide.parameters,
                    arguments: Box::new([
                        FirCallArgument::Expression {
                            parameter: 0,
                            value: owner,
                            conversion: None,
                        },
                        FirCallArgument::Expression {
                            parameter: 1,
                            value: property,
                            conversion: None,
                        },
                    ]),
                    substitutions: Box::new([]),
                }),
            });
            result
        } else {
            delegate_ty
        };
        let storage = LocalBinding {
            value: self.allocate_local(),
            ty: storage_ty,
            lateinit: false,
        };
        self.body
            .set_debug_value_name(storage.value, format!("{name}$delegate"));
        self.delegate_scopes
            .last_mut()
            .expect("a local delegate belongs to a lexical scope")
            .insert(
                name.to_string(),
                LocalDelegateBinding {
                    storage: DelegateStorage::Local(storage),
                    property_ty,
                    get_value,
                    set_value,
                    name: name.into(),
                },
            );
        Ok(self.body.add_statement(FirStatement {
            origin,
            kind: FirStatementKind::Local {
                target: storage.value,
                ty: storage.ty,
                mutable: false,
                lateinit: false,
                initializer: Some(initializer),
                conversion: None,
            },
        }))
    }

    pub(super) fn delegated_read(
        &mut self,
        expression: ExprId,
        depth: u32,
        delegate: LocalDelegateBinding,
    ) -> Result<FirExprId, BodyCheckFailure> {
        let origin = self.expression_origin(expression)?;
        let receiver = self.delegate_storage_read(origin, depth, &delegate)?;
        let owner = self.synthetic_null(origin);
        let property = self.local_property_reference(origin, &delegate.name, delegate.property_ty);
        let call = self.delegate_call(
            delegate.get_value.target,
            delegate.get_value.extension,
            delegate.get_value.parameters,
            receiver,
            vec![owner, property],
        );
        self.add_expression_with_type(expression, delegate.property_ty, call)
    }

    pub(super) fn delegated_write(
        &mut self,
        statement: StmtId,
        depth: u32,
        delegate: LocalDelegateBinding,
        value: ExprId,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let span = self.file.stmt_spans.get(statement.0 as usize).copied();
        let origin = self.statement_origin(statement)?;
        let value = self.expression(value)?;
        self.delegated_write_value(origin, span, depth, delegate, value)
    }

    fn delegated_write_value(
        &mut self,
        origin: OriginId,
        span: Option<Span>,
        depth: u32,
        delegate: LocalDelegateBinding,
        value: FirExprId,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let target = delegate.set_value.clone().ok_or_else(|| {
            self.failure(
                span,
                BodyCheckFailureKind::UnsupportedStatement(StatementForm::Assign),
            )
        })?;
        let receiver = self.delegate_storage_read(origin, depth, &delegate)?;
        let owner = self.synthetic_null(origin);
        let property = self.local_property_reference(origin, &delegate.name, delegate.property_ty);
        Ok(self.delegate_call(
            target.target,
            target.extension,
            target.parameters,
            receiver,
            vec![owner, property, value],
        ))
    }

    pub(super) fn delegated_inc_dec_expression(
        &mut self,
        expression: ExprId,
        target: ExprId,
        decrement: bool,
        prefix: bool,
        depth: u32,
        delegate: LocalDelegateBinding,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let span = self.file.expr_span(expression);
        let concrete_span =
            span.ok_or_else(|| self.failure(None, BodyCheckFailureKind::MissingSourceSpan))?;
        let origin = self.expression_origin(expression)?;
        let resolution = self
            .info
            .resolved_inc_dec
            .get(&IncDecSite::Expression(expression))
            .copied()
            .ok_or_else(|| {
                self.failure(
                    span,
                    BodyCheckFailureKind::UnsupportedExpression(ExpressionForm::IncDec),
                )
            })?;
        let read_ty = self.resolved_type(concrete_span, resolution.receiver_ty)?;
        let updated_ty = self.resolved_type(concrete_span, resolution.updated_ty)?;
        let read = self.delegated_read(target, depth, delegate.clone())?;
        let mut statements = Vec::new();
        let (operand, result_source) = if prefix {
            (read, None)
        } else {
            let temporary = self.allocate_local();
            statements.push(self.body.add_statement(FirStatement {
                origin,
                kind: FirStatementKind::Local {
                    target: temporary,
                    ty: read_ty,
                    mutable: false,
                    lateinit: false,
                    initializer: Some(read),
                    conversion: None,
                },
            }));
            let stored = self.body.add_expr(FirExpr {
                origin,
                ty: read_ty,
                kind: FirExprKind::ValueRead(temporary),
            });
            (stored, Some(temporary))
        };
        let convention = if decrement { "dec" } else { "inc" };
        let updated_kind = if self.selected_operator(expression, convention) {
            if let Some(ResolvedCall::LocalFunction(selected)) = self
                .info
                .resolved_operator_call(expression, convention)
                .cloned()
            {
                self.local_operator_call_on_value(span, origin, &selected, operand, &[])?
            } else {
                FirExprKind::Call(self.source_member_operator_call_on_value(
                    expression,
                    convention,
                    operand,
                    &[],
                )?)
            }
        } else {
            FirExprKind::Unary {
                operation: if decrement {
                    FirUnaryOperation::Decrement
                } else {
                    FirUnaryOperation::Increment
                },
                operand,
            }
        };
        let updated = self.body.add_expr(FirExpr {
            origin,
            ty: updated_ty,
            kind: updated_kind,
        });
        let write_kind =
            self.delegated_write_value(origin, span, depth, delegate.clone(), updated)?;
        let write = self.body.add_expr(FirExpr {
            origin,
            ty: ResolvedTy::new(Ty::Unit).expect("Unit is publishable FIR"),
            kind: write_kind,
        });
        statements.push(self.body.add_statement(FirStatement {
            origin,
            kind: FirStatementKind::Expression(write),
        }));
        let result = match result_source {
            Some(temporary) => self.body.add_expr(FirExpr {
                origin,
                ty: read_ty,
                kind: FirExprKind::ValueRead(temporary),
            }),
            None => self.delegated_read(target, depth, delegate)?,
        };
        Ok(FirExprKind::Block {
            statements: statements.into_boxed_slice(),
            result: Some(result),
        })
    }

    pub(super) fn delegated_inc_dec_statement(
        &mut self,
        statement: StmtId,
        decrement: bool,
        depth: u32,
        delegate: LocalDelegateBinding,
        origin: OriginId,
    ) -> Result<FirExprId, BodyCheckFailure> {
        let span = self.file.stmt_spans.get(statement.0 as usize).copied();
        let concrete_span =
            span.ok_or_else(|| self.failure(None, BodyCheckFailureKind::MissingSourceSpan))?;
        let resolution = self
            .info
            .resolved_inc_dec
            .get(&IncDecSite::Statement(statement))
            .copied()
            .ok_or_else(|| {
                self.failure(
                    span,
                    BodyCheckFailureKind::UnsupportedStatement(StatementForm::IncDec),
                )
            })?;
        let read = self.delegate_storage_read(origin, depth, &delegate)?;
        let convention = if decrement { "dec" } else { "inc" };
        let updated_kind = if self
            .info
            .resolved_stmt_operator_call(statement, convention)
            .is_some()
        {
            self.zero_arg_statement_operator_call_on_value(statement, convention, read)?
        } else {
            FirExprKind::Unary {
                operation: if decrement {
                    FirUnaryOperation::Decrement
                } else {
                    FirUnaryOperation::Increment
                },
                operand: read,
            }
        };
        let updated = self.body.add_expr(FirExpr {
            origin,
            ty: self.resolved_type(concrete_span, resolution.updated_ty)?,
            kind: updated_kind,
        });
        let write_kind = self.delegated_write_value(origin, span, depth, delegate, updated)?;
        Ok(self.body.add_expr(FirExpr {
            origin,
            ty: ResolvedTy::new(Ty::Unit).expect("Unit is publishable FIR"),
            kind: write_kind,
        }))
    }

    fn delegate_storage_read(
        &mut self,
        origin: OriginId,
        depth: u32,
        delegate: &LocalDelegateBinding,
    ) -> Result<FirExprId, BodyCheckFailure> {
        let kind = match delegate.storage {
            DelegateStorage::ClassField(binding) => {
                self.class_storage_read_kind(binding, origin)?
            }
            DelegateStorage::Local(storage) if depth == u32::MAX => {
                FirExprKind::ValueRead(storage.value)
            }
            DelegateStorage::Local(storage) => {
                self.body.add_capture(FirCapture {
                    origin,
                    enclosing_depth: depth,
                    source: storage.value,
                    ty: storage.ty,
                    shared_cell: false,
                });
                FirExprKind::CapturedValueRead {
                    enclosing_depth: depth,
                    source: storage.value,
                }
            }
        };
        Ok(self.body.add_expr(FirExpr {
            origin,
            ty: delegate.storage.ty(),
            kind,
        }))
    }

    fn synthetic_null(&mut self, cause: OriginId) -> FirExprId {
        let origin = self
            .origins
            .synthetic(cause, SyntheticOriginKind::GeneratedAccessor);
        self.body.add_expr(FirExpr {
            origin,
            ty: ResolvedTy::new(Ty::Null).expect("Null is publishable FIR"),
            kind: FirExprKind::Constant(FirConstant::Null),
        })
    }

    fn local_property_reference(
        &mut self,
        cause: OriginId,
        name: &str,
        property_type: ResolvedTy,
    ) -> FirExprId {
        let origin = self
            .origins
            .synthetic(cause, SyntheticOriginKind::GeneratedAccessor);
        self.body.add_expr(FirExpr {
            origin,
            ty: ResolvedTy::new(Ty::obj("kotlin/reflect/KProperty"))
                .expect("KProperty is publishable FIR"),
            kind: FirExprKind::LocalPropertyReference {
                name: name.into(),
                property_type,
            },
        })
    }

    fn delegate_call(
        &self,
        target: FirCallTarget,
        extension: bool,
        parameter_types: Box<[ResolvedTy]>,
        receiver: FirExprId,
        arguments: Vec<FirExprId>,
    ) -> FirExprKind {
        FirExprKind::Call(FirCall {
            target,
            dispatch_receiver: (!extension).then_some(FirReceiver {
                value: receiver,
                conversion: None,
            }),
            extension_receiver: extension.then_some(FirReceiver {
                value: receiver,
                conversion: None,
            }),
            parameter_types,
            arguments: arguments
                .into_iter()
                .enumerate()
                .map(|(parameter, value)| FirCallArgument::Expression {
                    parameter: parameter as u32,
                    value,
                    conversion: None,
                })
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            substitutions: Box::new([]),
        })
    }

    fn delegate_call_target(
        &self,
        expression: ExprId,
        receiver: ResolvedTy,
        target: &DelegateGetValueTarget,
    ) -> Result<FirDelegateCall, BodyCheckFailure> {
        let span = self.file.expr_span(expression);
        selected_delegate_call(self.index, span, receiver, target)
    }
}

pub(super) fn property_delegate_plan(
    file: &File,
    info: &TypeInfo,
    index: &ResolvedModuleIndex,
    delegate: ExprId,
    mutable: bool,
) -> Result<FirPropertyDelegatePlan, BodyCheckFailure> {
    let span = file.expr_span(delegate);
    let delegate_type =
        ResolvedTy::new(info.semantic_ty(delegate)).map_err(|error| BodyCheckFailure {
            span,
            kind: BodyCheckFailureKind::UnpublishableType(error),
        })?;
    let provide_delegate = info
        .delegate_provide(delegate)
        .map(|target| selected_delegate_call(index, span, delegate_type, target))
        .transpose()?;
    let storage_type = provide_delegate
        .as_ref()
        .map_or(delegate_type, |call| call.result);
    let get_value = info
        .delegate_getvalue(delegate)
        .ok_or(BodyCheckFailure {
            span,
            kind: BodyCheckFailureKind::MissingStableCallTarget,
        })
        .and_then(|target| selected_delegate_call(index, span, storage_type, target))?;
    let set_value = mutable
        .then(|| {
            info.delegate_setvalue(delegate)
                .ok_or(BodyCheckFailure {
                    span,
                    kind: BodyCheckFailureKind::MissingStableCallTarget,
                })
                .and_then(|target| selected_delegate_call(index, span, storage_type, target))
        })
        .transpose()?;
    Ok(FirPropertyDelegatePlan {
        storage_type,
        provide_delegate,
        get_value,
        set_value,
    })
}

fn selected_delegate_call(
    index: &ResolvedModuleIndex,
    span: Option<crate::diag::Span>,
    receiver: ResolvedTy,
    target: &DelegateGetValueTarget,
) -> Result<FirDelegateCall, BodyCheckFailure> {
    crate::trace_compiler!(
        "fir",
        "publish delegate convention receiver={:?} target={target:?}",
        receiver.get(),
    );
    let failure = |kind| BodyCheckFailure { span, kind };
    let resolved = |ty| {
        ResolvedTy::new(ty).map_err(|error| failure(BodyCheckFailureKind::UnpublishableType(error)))
    };
    match target {
        DelegateGetValueTarget::Member {
            stable_declaration,
            external_identity,
            external_default_provider,
            params,
            ret,
            ..
        } => {
            let parameters = params
                .iter()
                .copied()
                .map(resolved)
                .collect::<Result<Vec<_>, _>>()?
                .into_boxed_slice();
            if let Some(declaration) = stable_declaration {
                crate::trace_compiler!(
                    "fir",
                    "delegate member declaration={declaration:?} name={:?} callable={:?}",
                    index.declaration_name(*declaration),
                    index
                        .callable_for_declaration(*declaration)
                        .map(|callable| callable.id),
                );
                let callable = index
                    .callable_for_declaration(*declaration)
                    .ok_or_else(|| failure(BodyCheckFailureKind::MissingStableCallTarget))?;
                return Ok(FirDelegateCall {
                    target: callable.id.into(),
                    parameters,
                    result: resolved(*ret)?,
                    extension: false,
                    dispatch_receiver: None,
                });
            }
            let declaration = external_identity
                .ok_or_else(|| failure(BodyCheckFailureKind::MissingStableCallTarget))?;
            Ok(FirDelegateCall {
                target: FirCallTarget::External {
                    declaration,
                    default_provider: *external_default_provider,
                    receiver: Some(receiver),
                    declared_receiver: None,
                    parameters: parameters.clone(),
                    result: resolved(*ret)?,
                    declared_result: None,
                    suspend: false,
                    can_inline: false,
                    inline_plan: None,
                    extension_receiver_parameter: None,
                },
                parameters,
                result: resolved(*ret)?,
                extension: false,
                dispatch_receiver: None,
            })
        }
        DelegateGetValueTarget::Extension {
            callable,
            stable_declaration,
        } => {
            let parameters = callable
                .params
                .get(1..)
                .ok_or_else(|| failure(BodyCheckFailureKind::UnsupportedCallShape))?
                .iter()
                .copied()
                .map(resolved)
                .collect::<Result<Vec<_>, _>>()?
                .into_boxed_slice();
            if let Some(declaration) = stable_declaration {
                let result = resolved(callable.ret)?;
                let header = index
                    .callable_for_declaration(*declaration)
                    .ok_or_else(|| failure(BodyCheckFailureKind::MissingStableCallTarget))?;
                return Ok(FirDelegateCall {
                    target: header.id.into(),
                    parameters,
                    result,
                    extension: true,
                    dispatch_receiver: None,
                });
            }
            let declaration = callable
                .external_identity
                .ok_or_else(|| failure(BodyCheckFailureKind::MissingStableCallTarget))?;
            Ok(FirDelegateCall {
                target: FirCallTarget::External {
                    declaration,
                    default_provider: callable.external_default_provider,
                    receiver: Some(receiver),
                    declared_receiver: callable.source_receiver.map(resolved).transpose()?,
                    parameters: parameters.clone(),
                    result: resolved(callable.ret)?,
                    declared_result: callable.declared_ret.map(resolved).transpose()?,
                    suspend: callable.suspend,
                    can_inline: callable.inline.can_inline(),
                    inline_plan: super::calls::fir_inline_body_plan(
                        callable.inline_body_plan.as_deref(),
                        Some(0),
                    ),
                    extension_receiver_parameter: None,
                },
                parameters,
                result: resolved(callable.ret)?,
                extension: true,
                dispatch_receiver: None,
            })
        }
        DelegateGetValueTarget::MemberExtension {
            stable_declaration,
            external_identity,
            external_default_provider,
            extension_receiver,
            dispatch_receiver,
            context_count,
            params,
            ret,
            inline,
            inline_body_plan,
            suspend,
            declared_ret,
            ..
        } => {
            let call_parameters = params
                .iter()
                .copied()
                .map(resolved)
                .collect::<Result<Vec<_>, _>>()?
                .into_boxed_slice();
            let target = if let Some(declaration) = stable_declaration {
                let callable = index
                    .callable_for_declaration(*declaration)
                    .ok_or_else(|| failure(BodyCheckFailureKind::MissingStableCallTarget))?;
                crate::trace_compiler!(
                    "fir",
                    "delegate member-extension declaration={declaration:?} callable={:?} result={ret:?}",
                    callable.id,
                );
                callable.id.into()
            } else {
                let declaration = external_identity
                    .ok_or_else(|| failure(BodyCheckFailureKind::MissingStableCallTarget))?;
                let mut parameters = params.clone();
                let extension_parameter = (*context_count).min(parameters.len());
                parameters.insert(extension_parameter, *extension_receiver);
                FirCallTarget::External {
                    declaration,
                    default_provider: *external_default_provider,
                    receiver: Some(resolved(dispatch_receiver.ty)?),
                    declared_receiver: None,
                    parameters: parameters
                        .into_iter()
                        .map(resolved)
                        .collect::<Result<Vec<_>, _>>()?
                        .into_boxed_slice(),
                    result: resolved(*ret)?,
                    declared_result: declared_ret.map(resolved).transpose()?,
                    suspend: *suspend,
                    can_inline: inline.can_inline(),
                    inline_plan: super::calls::fir_inline_body_plan(
                        inline_body_plan.as_deref(),
                        None,
                    ),
                    extension_receiver_parameter: Some(
                        u32::try_from(extension_parameter)
                            .map_err(|_| failure(BodyCheckFailureKind::UnsupportedCallShape))?,
                    ),
                }
            };
            Ok(FirDelegateCall {
                target,
                parameters: call_parameters,
                result: resolved(*ret)?,
                extension: true,
                dispatch_receiver: Some(delegate_dispatch_receiver(dispatch_receiver, &resolved)?),
            })
        }
    }
}

fn delegate_dispatch_receiver(
    selected: &crate::resolve::ImplicitReceiverSelection,
    resolved: &impl Fn(Ty) -> Result<ResolvedTy, BodyCheckFailure>,
) -> Result<FirDelegateDispatchReceiver, BodyCheckFailure> {
    let ty = resolved(selected.ty)?;
    if let Some((name, shadow_depth)) = &selected.context_binding {
        return Ok(FirDelegateDispatchReceiver::ContextBinding {
            ty,
            name: name.clone().into_boxed_str(),
            shadow_depth: u32::try_from(*shadow_depth).map_err(|_| BodyCheckFailure {
                span: None,
                kind: BodyCheckFailureKind::UnsupportedCallShape,
            })?,
        });
    }
    if let Some(singleton) = &selected.singleton {
        return Ok(FirDelegateDispatchReceiver::Singleton {
            ty,
            classifier: singleton.classifier,
        });
    }
    Ok(FirDelegateDispatchReceiver::Scoped {
        ty,
        current: selected.current,
        depth: u32::try_from(selected.receiver_depth).map_err(|_| BodyCheckFailure {
            span: None,
            kind: BodyCheckFailureKind::UnsupportedCallShape,
        })?,
    })
}
