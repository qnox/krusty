//! Checked FIR for local delegated properties.

use super::*;
use crate::fir::LocalDelegatedPropertyId;
use crate::resolve::DelegateGetValueTarget;

impl BodyCheckSession {
    fn allocate_local_delegated_property(
        &mut self,
        owner: BodyOwnerId,
    ) -> LocalDelegatedPropertyId {
        self.local_delegated_properties.allocate(owner)
    }
}

impl BodyFirChecker<'_> {
    pub(super) fn local_delegate(&self, name: &str) -> Option<LocalDelegateBinding> {
        self.delegate_scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).cloned())
    }

    pub(super) fn delegated_binding(&self, name: &str) -> Option<(u32, LocalDelegateBinding)> {
        self.local_delegate(name)
            .map(|binding| (u32::MAX, binding))
            .or_else(|| self.outer_delegates.get(name).cloned())
            .or_else(|| {
                self.class_delegates
                    .get(name)
                    .cloned()
                    .map(|binding| (u32::MAX, binding))
            })
    }

    pub(super) fn local_delegate_statement(
        &mut self,
        statement: StmtId,
        origin: OriginId,
    ) -> Result<FirStatementId, BodyCheckFailure> {
        let span = self.file.stmt_spans.get(statement.0 as usize).copied();
        // The statement's own syntax, read from the file the body belongs to rather than passed
        // down field by field: every field below is this one declaration's.
        let Stmt::LocalDelegate {
            is_var: mutable,
            name,
            ty: explicit_type,
            delegate,
            ..
        } = self.file.stmt(statement)
        else {
            return Err(self.failure(
                span,
                BodyCheckFailureKind::UnsupportedStatement(StatementForm::LocalDelegate),
            ));
        };
        let (mutable, delegate) = (*mutable, *delegate);
        let provenance = self.file.local_delegates.get(&statement).ok_or_else(|| {
            self.failure(
                span,
                BodyCheckFailureKind::UnsupportedStatement(StatementForm::LocalDelegate),
            )
        })?;
        let (ordinal, sites) = (provenance.ordinal, provenance.accessors.clone());
        let (name, explicit_type) = (name.as_str(), explicit_type.as_ref());
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
        let get_value_dispatch = self.local_delegate_dispatch_parameter(&get_value)?;
        let set_value = if mutable {
            let target = self
                .info
                .delegate_setvalue(delegate)
                .ok_or_else(|| self.failure(span, BodyCheckFailureKind::MissingStableCallTarget))?;
            Some(self.delegate_call_target(delegate, delegate_ty, target)?)
        } else {
            None
        };
        let set_value_dispatch = set_value
            .as_ref()
            .map(|call| self.local_delegate_dispatch_parameter(call))
            .transpose()?
            .flatten();

        let declaration = self.allocate_local_delegated_property();
        let mut initializer = self.expression(delegate)?;
        let storage_ty = if let Some(provide) = self.info.delegate_provide(delegate) {
            let provide = self.delegate_call_target(delegate, delegate_ty, provide)?;
            let reference = self.local_property_reference(
                origin,
                declaration,
                (name, property_ty),
                (mutable, ordinal),
            );
            let owner = self.synthetic_null(origin);
            let result = provide.result;
            let kind = self.delegate_convention_call(
                origin,
                span,
                &provide,
                initializer,
                vec![owner, reference],
            )?;
            initializer = self.body.add_expr(FirExpr {
                origin,
                ty: result,
                kind,
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
        let storage_name = format!("{name}$delegate");
        self.body
            .set_debug_value_name(storage.value, storage_name.clone());
        let reference = self.local_property_reference(
            origin,
            declaration,
            (name, property_ty),
            (mutable, ordinal),
        );
        let expected_sites = 1 + usize::from(mutable);
        if sites.len() != expected_sites {
            return Err(self.failure(span, BodyCheckFailureKind::MissingStableCallTarget));
        }
        let accessor_sites = sites
            .iter()
            .map(|site| crate::fir::FirLiftingSite::from_source(site, true))
            .collect::<Vec<_>>();
        for site in &accessor_sites {
            self.body.add_bodiless_lifting_site(site.clone());
        }
        self.body
            .add_local_delegate_plan(crate::fir::FirLocalDelegatePlan {
                declaration,
                storage_name: storage_name.clone().into(),
                storage_type: storage_ty,
                property_type: property_ty,
                reference,
                get_value: get_value.clone(),
                get_value_dispatch,
                set_value: set_value.clone(),
                set_value_dispatch,
                accessor_sites: accessor_sites.into_boxed_slice(),
                line: self
                    .file
                    .stmt_lines
                    .get(statement.0 as usize)
                    .copied()
                    .unwrap_or(0),
            });
        self.delegate_scopes
            .last_mut()
            .expect("a local delegate belongs to a lexical scope")
            .insert(
                name.to_string(),
                LocalDelegateBinding {
                    storage: DelegateStorage::Local(storage),
                    storage_name: storage_name.into(),
                    property_ty,
                    get_value,
                    set_value,
                    name: name.into(),
                    declaration,
                },
            );
        Ok(self.body.add_statement(FirStatement {
            origin,
            kind: FirStatementKind::Local {
                target: storage.value,
                ty: storage.ty,
                mutable: false,
                lateinit: false,
                deferred: false,
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
        let dispatch_receiver = self.delegate_dispatch_receiver(
            origin,
            self.file.expr_span(expression),
            &delegate.get_value,
        )?;
        let call = FirExprKind::LocalDelegateAccess {
            plan: delegate.declaration,
            delegate: receiver,
            dispatch_receiver,
            value: None,
        };
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
        let setter = delegate.set_value.as_ref().ok_or_else(|| {
            self.failure(
                span,
                BodyCheckFailureKind::UnsupportedStatement(StatementForm::Assign),
            )
        })?;
        let receiver = self.delegate_storage_read(origin, depth, &delegate)?;
        let dispatch_receiver = self.delegate_dispatch_receiver(origin, span, setter)?;
        Ok(FirExprKind::LocalDelegateAccess {
            plan: delegate.declaration,
            delegate: receiver,
            dispatch_receiver,
            value: Some(value),
        })
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
                    deferred: false,
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

    /// `a++` or `++a` as a statement on a local delegated property, as kotlinc lowers it: the
    /// expression form with its value discarded. A postfix update holds the value it read in a
    /// temporary; a prefix one reads the property again after writing it.
    pub(super) fn delegated_inc_dec_statement(
        &mut self,
        statement: StmtId,
        (decrement, prefix): (bool, bool),
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
        let property_ty = delegate.property_ty;
        let read = |checker: &mut Self| -> Result<FirExprId, BodyCheckFailure> {
            let receiver = checker.delegate_storage_read(origin, depth, &delegate)?;
            let dispatch_receiver =
                checker.delegate_dispatch_receiver(origin, span, &delegate.get_value)?;
            let kind = FirExprKind::LocalDelegateAccess {
                plan: delegate.declaration,
                delegate: receiver,
                dispatch_receiver,
                value: None,
            };
            Ok(checker.body.add_expr(FirExpr {
                origin,
                ty: property_ty,
                kind,
            }))
        };
        let mut statements = Vec::new();
        let operand = if prefix {
            read(self)?
        } else {
            let temporary = self.allocate_local();
            let initializer = read(self)?;
            statements.push(self.body.add_statement(FirStatement {
                origin,
                kind: FirStatementKind::Local {
                    target: temporary,
                    ty: property_ty,
                    mutable: false,
                    lateinit: false,
                    deferred: false,
                    initializer: Some(initializer),
                    conversion: None,
                },
            }));
            self.body.add_expr(FirExpr {
                origin,
                ty: property_ty,
                kind: FirExprKind::ValueRead(temporary),
            })
        };
        let convention = if decrement { "dec" } else { "inc" };
        let updated_kind = if self
            .info
            .resolved_stmt_operator_call(statement, convention)
            .is_some()
        {
            self.zero_arg_statement_operator_call_on_value(statement, convention, operand)?
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
            ty: self.resolved_type(concrete_span, resolution.updated_ty)?,
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
        let (ty, result) = if prefix {
            (property_ty, Some(read(self)?))
        } else {
            (
                ResolvedTy::new(Ty::Unit).expect("Unit is publishable FIR"),
                None,
            )
        };
        Ok(self.body.add_expr(FirExpr {
            origin,
            ty,
            kind: FirExprKind::Block {
                statements: statements.into_boxed_slice(),
                result,
            },
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
                    source: FirCaptureSource::Value(storage.value),
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

    /// The `KProperty` value a LOCAL delegated property's conventions receive.
    ///
    /// Its type is the classifier resolution answered with when it selected those conventions — the
    /// same recorded fact a member or top-level delegated property publishes on its plan, so the
    /// local path does not spell the name a second time.
    fn allocate_local_delegated_property(&mut self) -> LocalDelegatedPropertyId {
        self.session
            .allocate_local_delegated_property(self.body.owner())
    }

    fn local_property_reference(
        &mut self,
        cause: OriginId,
        declaration: LocalDelegatedPropertyId,
        (name, property_type): (&str, ResolvedTy),
        (mutable, ordinal): (bool, u32),
    ) -> FirExprId {
        let origin = self
            .origins
            .synthetic(cause, SyntheticOriginKind::GeneratedAccessor);
        let reference_type = self
            .info
            .delegate_property_reference_type()
            .expect("a selected delegate convention resolved its KProperty classifier");
        self.body.add_expr(FirExpr {
            origin,
            ty: ResolvedTy::new(reference_type).expect("KProperty is publishable FIR"),
            kind: FirExprKind::LocalPropertyReference {
                name: name.into(),
                property_type,
                declaration,
                mutable,
                ordinal,
            },
        })
    }

    fn delegate_convention_call(
        &mut self,
        origin: OriginId,
        span: Option<Span>,
        convention: &FirDelegateCall,
        delegate: FirExprId,
        arguments: Vec<FirExprId>,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let delegate_receiver = FirReceiver {
            value: delegate,
            conversion: None,
        };
        let dispatch_receiver = match &convention.dispatch_receiver {
            Some(selected) => {
                Some(self.member_extension_dispatch_receiver(origin, span, selected)?)
            }
            None if !convention.extension => Some(delegate_receiver),
            None => None,
        };
        Ok(FirExprKind::Call(FirCall {
            target: convention.target.clone(),
            dispatch_receiver,
            extension_receiver: convention.extension.then_some(delegate_receiver),
            parameter_types: convention.parameters.clone(),
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
        }))
    }

    fn delegate_dispatch_receiver(
        &mut self,
        origin: OriginId,
        span: Option<Span>,
        convention: &FirDelegateCall,
    ) -> Result<Option<FirReceiver>, BodyCheckFailure> {
        match convention.dispatch_receiver.as_ref() {
            Some(FirDelegateDispatchReceiver::Singleton { .. }) | None => Ok(None),
            Some(selected) => self
                .member_extension_dispatch_receiver(origin, span, selected)
                .map(Some),
        }
    }

    fn member_extension_dispatch_receiver(
        &mut self,
        origin: OriginId,
        span: Option<Span>,
        selected: &FirDelegateDispatchReceiver,
    ) -> Result<FirReceiver, BodyCheckFailure> {
        let selection = match selected {
            FirDelegateDispatchReceiver::Scoped { ty, current, depth } => {
                crate::resolve::ImplicitReceiverSelection {
                    ty: ty.get(),
                    current: *current,
                    receiver_depth: *depth as usize,
                    classifier: None,
                    context_binding: None,
                    singleton: None,
                }
            }
            FirDelegateDispatchReceiver::ContextBinding {
                ty,
                name,
                shadow_depth,
            } => crate::resolve::ImplicitReceiverSelection {
                ty: ty.get(),
                current: false,
                receiver_depth: 0,
                classifier: None,
                context_binding: Some((name.to_string(), *shadow_depth as usize)),
                singleton: None,
            },
            FirDelegateDispatchReceiver::Singleton { ty, classifier } => {
                crate::resolve::ImplicitReceiverSelection {
                    ty: ty.get(),
                    current: false,
                    receiver_depth: 0,
                    classifier: None,
                    context_binding: None,
                    singleton: Some(crate::resolve::SingletonValue {
                        classifier: *classifier,
                    }),
                }
            }
        };
        self.materialize_implicit_receiver(origin, span, &selection)?
            .ok_or_else(|| self.failure(span, BodyCheckFailureKind::UnsupportedCallShape))
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

    fn local_delegate_dispatch_parameter(
        &self,
        call: &FirDelegateCall,
    ) -> Result<Option<FirLocalDelegateDispatchParameter>, BodyCheckFailure> {
        let Some(dispatch) = call.dispatch_receiver.as_ref() else {
            return Ok(None);
        };
        Ok(match dispatch {
            FirDelegateDispatchReceiver::Scoped { depth, .. } => {
                let receiver =
                    self.receiver_capture_at_depth(*depth as usize)
                        .ok_or_else(|| {
                            self.failure(None, BodyCheckFailureKind::UnsupportedCallShape)
                        })?;
                Some(FirLocalDelegateDispatchParameter::ImplicitReceiver(
                    receiver,
                ))
            }
            FirDelegateDispatchReceiver::ContextBinding { name, .. } => Some(
                FirLocalDelegateDispatchParameter::ContextValue(name.clone()),
            ),
            FirDelegateDispatchReceiver::Singleton { .. } => None,
        })
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
        // The classifier RESOLUTION answered with when it selected these conventions, not a name
        // this phase spells for itself.
        property_reference_type: ResolvedTy::new(info.delegate_property_reference_type().ok_or(
            BodyCheckFailure {
                span,
                kind: BodyCheckFailureKind::MissingStableCallTarget,
            },
        )?)
        .map_err(|error| BodyCheckFailure {
            span,
            kind: BodyCheckFailureKind::UnpublishableType(error),
        })?,
        provide_delegate,
        get_value,
        set_value,
    })
}

/// The DECLARED value parameters of a selected convention, as the declaration spells them.
///
/// `applied` is how many the call actually supplies, and the two must agree: a delegate operator's
/// own value parameters are the whole contract here, because a context-prefixed operator is not a
/// convention at all and is excluded during selection. A disagreement means the selected shape is
/// not one this boundary can map, and it is refused rather than trimmed to fit.
fn declared_parameters(
    declared: &[Ty],
    applied: usize,
    resolved: impl Fn(Ty) -> Result<ResolvedTy, BodyCheckFailure>,
) -> Result<Box<[ResolvedTy]>, BodyCheckFailure> {
    if declared.len() != applied {
        return Err(BodyCheckFailure {
            span: None,
            kind: BodyCheckFailureKind::UnsupportedCallShape,
        });
    }
    Ok(declared
        .iter()
        .copied()
        .map(resolved)
        .collect::<Result<Vec<_>, _>>()?
        .into_boxed_slice())
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
            applied_receiver,
            declared_receiver,
            stable_declaration,
            external_identity,
            external_default_provider,
            params,
            declared_params,
            declared_ret,
            ret,
            ..
        } => {
            let parameters = params
                .iter()
                .copied()
                .map(resolved)
                .collect::<Result<Vec<_>, _>>()?
                .into_boxed_slice();
            let declared_parameters =
                declared_parameters(declared_params, parameters.len(), resolved)?;
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
                let substitutions = delegate_substitutions(
                    index,
                    span,
                    std::iter::once((*declared_receiver, *applied_receiver))
                        .chain(declared_params.iter().copied().zip(params.iter().copied()))
                        .chain(std::iter::once((*declared_ret, *ret))),
                )?;
                return Ok(FirDelegateCall {
                    target: callable.id.into(),
                    substitutions,
                    parameters,
                    declared_parameters,
                    result: resolved(*ret)?,
                    extension: false,
                    dispatch_receiver: None,
                    receiver,
                    declared_receiver: None,
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
                    overridden_results: Box::new([]),
                    suspend: false,
                    can_inline: false,
                    inline_plan: None,
                    extension_receiver_parameter: None,
                },
                substitutions: Box::new([]),
                parameters,
                declared_parameters,
                result: resolved(*ret)?,
                extension: false,
                dispatch_receiver: None,
                receiver,
                declared_receiver: None,
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
            // The provider's declaration fact where it recorded one; otherwise the declaration is
            // not generic and its ordinary parameter list IS its spelling. Either way the extension
            // receiver leads the list and is not one of the operator's value parameters.
            let declared = callable
                .declared_params
                .as_deref()
                .unwrap_or(&callable.params);
            let declared_start = declared
                .len()
                .checked_sub(parameters.len())
                .ok_or_else(|| failure(BodyCheckFailureKind::UnsupportedCallShape))?;
            let declared = declared
                .get(declared_start..)
                .ok_or_else(|| failure(BodyCheckFailureKind::UnsupportedCallShape))?;
            let declared_parameters = declared_parameters(declared, parameters.len(), resolved)?;
            if let Some(declaration) = stable_declaration {
                let result = resolved(callable.ret)?;
                let header = index
                    .callable_for_declaration(*declaration)
                    .ok_or_else(|| failure(BodyCheckFailureKind::MissingStableCallTarget))?;
                let substitutions = callable
                    .generic_sig
                    .as_deref()
                    .map(|generic| {
                        delegate_substitutions(
                            index,
                            span,
                            generic
                                .receiver
                                .into_iter()
                                .zip(Some(receiver.get()))
                                .chain(
                                    generic
                                        .params
                                        .iter()
                                        .copied()
                                        .zip(callable.params.iter().copied().skip(1)),
                                )
                                .chain(std::iter::once((generic.ret, callable.ret))),
                        )
                    })
                    .transpose()?
                    .unwrap_or_else(|| Box::new([]));
                return Ok(FirDelegateCall {
                    target: header.id.into(),
                    substitutions,
                    parameters,
                    declared_parameters,
                    result,
                    extension: true,
                    dispatch_receiver: None,
                    receiver,
                    declared_receiver: callable.source_receiver.map(resolved).transpose()?,
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
                    overridden_results: callable
                        .overridden_results
                        .iter()
                        .copied()
                        .map(resolved)
                        .collect::<Result<Vec<_>, _>>()?
                        .into_boxed_slice(),
                    suspend: callable.suspend,
                    can_inline: callable.inline.can_inline(),
                    inline_plan: super::inline_body_plan::publish(
                        callable.inline_body_plan.as_deref(),
                        Some(0),
                    )
                    .map_err(|_| failure(BodyCheckFailureKind::UnsupportedCallShape))?,
                    extension_receiver_parameter: None,
                },
                substitutions: Box::new([]),
                parameters,
                declared_parameters,
                result: resolved(callable.ret)?,
                extension: true,
                dispatch_receiver: None,
                receiver,
                declared_receiver: callable.source_receiver.map(resolved).transpose()?,
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
            declared_params,
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
            let declared_parameters =
                declared_parameters(declared_params, call_parameters.len(), resolved)?;
            let (target, substitutions) = if let Some(declaration) = stable_declaration {
                let callable = index
                    .callable_for_declaration(*declaration)
                    .ok_or_else(|| failure(BodyCheckFailureKind::MissingStableCallTarget))?;
                crate::trace_compiler!(
                    "fir",
                    "delegate member-extension declaration={declaration:?} callable={:?} result={ret:?}",
                    callable.id,
                );
                let signature = index
                    .signature(callable.declaration)
                    .ok_or_else(|| failure(BodyCheckFailureKind::MissingStableCallTarget))?;
                let substitutions = delegate_substitutions(
                    index,
                    span,
                    callable
                        .shape
                        .extension_receiver
                        .into_iter()
                        .map(|receiver| (receiver.get(), *extension_receiver))
                        .chain(
                            signature
                                .parameters
                                .iter()
                                .map(|parameter| parameter.get())
                                .zip(params.iter().copied()),
                        )
                        .chain(std::iter::once((signature.result.get(), *ret))),
                )?;
                (callable.id.into(), substitutions)
            } else {
                let declaration = external_identity
                    .ok_or_else(|| failure(BodyCheckFailureKind::MissingStableCallTarget))?;
                let mut parameters = params.clone();
                let extension_parameter = (*context_count).min(parameters.len());
                parameters.insert(extension_parameter, *extension_receiver);
                (
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
                        overridden_results: Box::new([]),
                        suspend: *suspend,
                        can_inline: inline.can_inline(),
                        inline_plan: super::inline_body_plan::publish(
                            inline_body_plan.as_deref(),
                            None,
                        )
                        .map_err(|_| failure(BodyCheckFailureKind::UnsupportedCallShape))?,
                        extension_receiver_parameter: Some(
                            u32::try_from(extension_parameter)
                                .map_err(|_| failure(BodyCheckFailureKind::UnsupportedCallShape))?,
                        ),
                    },
                    Vec::<FirTypeSubstitution>::new().into_boxed_slice(),
                )
            };
            Ok(FirDelegateCall {
                target,
                substitutions,
                parameters: call_parameters,
                declared_parameters,
                result: resolved(*ret)?,
                extension: true,
                dispatch_receiver: Some(delegate_dispatch_receiver(dispatch_receiver, &resolved)?),
                receiver,
                declared_receiver: Some(resolved(*extension_receiver)?),
            })
        }
    }
}

/// Serialize the generic bindings convention selection already fixed. Every pair is the
/// declaration shape followed by its selected shape; this is not another applicability pass.
fn delegate_substitutions(
    index: &ResolvedModuleIndex,
    span: Option<crate::diag::Span>,
    shapes: impl IntoIterator<Item = (Ty, Ty)>,
) -> Result<Box<[FirTypeSubstitution]>, BodyCheckFailure> {
    let mut bindings = crate::symbol_resolver::GSigBinds::new();
    for (declared, selected) in shapes {
        crate::symbol_resolver::unify_inferred_ty(declared, selected, &mut bindings);
    }
    let failure = |kind| BodyCheckFailure { span, kind };
    let mut substitutions = bindings
        .into_iter()
        .map(|(name, value)| {
            let parameter = index
                .type_parameter_by_semantic_name(&name)
                .ok_or_else(|| failure(BodyCheckFailureKind::MissingStableCallTarget))?;
            Ok(FirTypeSubstitution {
                parameter: parameter.into(),
                value: ResolvedTy::new(value)
                    .map_err(|error| failure(BodyCheckFailureKind::UnpublishableType(error)))?,
                additional_bounds: Box::new([]),
            })
        })
        .collect::<Result<Vec<_>, BodyCheckFailure>>()?;
    substitutions.sort_unstable_by_key(|substitution| match substitution.parameter {
        FirTypeParameterRef::Module(parameter) => parameter.raw(),
        FirTypeParameterRef::External { ordinal, .. } => ordinal,
    });
    Ok(substitutions.into_boxed_slice())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_extension_declaration_shorter_than_its_applied_shape_is_rejected() {
        let mut callable = crate::libraries::LibraryCallable::library(
            crate::types::type_name("fixture/Operators"),
            "read",
            vec![Ty::Int, Ty::Null, Ty::obj("fixture/PropertyToken")],
            Ty::Int,
            Ty::Int,
            "(ILjava/lang/Object;Ljava/lang/Object;)I",
        );
        callable.source_receiver = Some(Ty::Int);
        callable.declared_params = Some(vec![Ty::Int].into_boxed_slice());
        let target = DelegateGetValueTarget::Extension {
            callable: Box::new(callable),
            stable_declaration: None,
        };

        let failure = selected_delegate_call(
            &ResolvedModuleIndex::default(),
            None,
            ResolvedTy::new(Ty::Int).expect("fixture receiver is publishable"),
            &target,
        )
        .expect_err("a truncated declaration shape must fail closed");

        assert!(matches!(
            failure.kind,
            BodyCheckFailureKind::UnsupportedCallShape
        ));
    }
}
