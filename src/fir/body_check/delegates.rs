//! Checked FIR for local delegated properties.

use super::*;
use crate::fir::{FirValueParameter, FirValueParameterName, LocalDelegateAccessor};
use crate::resolve::DelegateGetValueTarget;

/// What a local delegated property's reference and accessors are built from.
#[derive(Clone, Copy)]
struct DelegatedLocal<'a> {
    name: &'a str,
    ty: ResolvedTy,
    mutable: bool,
    /// Its position among its lexical class's local delegated properties.
    ordinal: u32,
}

/// One generated accessor of a local delegated property: the operator it calls, whether it is the
/// setter, and where it is lifted to.
struct DelegateAccessorPlan<'a> {
    statement: StmtId,
    origin: OriginId,
    site: &'a crate::ast::LiftingSite,
    setter: bool,
    storage: LocalBinding,
    operator: &'a FirDelegateCall,
}

impl BodyFirChecker<'_> {
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
            let property =
                self.local_property_reference(origin, (name, property_ty), (mutable, ordinal));
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
        let storage_name = format!("{name}$delegate");
        self.body
            .set_debug_value_name(storage.value, storage_name.clone());
        let property = DelegatedLocal {
            name,
            ty: property_ty,
            mutable,
            ordinal,
        };
        let [getter_site, setter_site @ ..] = sites.as_slice() else {
            return Err(self.failure(span, BodyCheckFailureKind::MissingStableCallTarget));
        };
        let accessor = |operator, site, setter| DelegateAccessorPlan {
            statement,
            origin,
            site,
            setter,
            storage,
            operator,
        };
        let getter =
            self.local_delegate_accessor(accessor(&get_value, getter_site, false), property)?;
        let setter = match (&set_value, setter_site) {
            (Some(set_value), [site]) => {
                Some(self.local_delegate_accessor(accessor(set_value, site, true), property)?)
            }
            (None, []) => None,
            _ => return Err(self.failure(span, BodyCheckFailureKind::MissingStableCallTarget)),
        };
        self.delegate_scopes
            .last_mut()
            .expect("a local delegate belongs to a lexical scope")
            .insert(
                name.to_string(),
                LocalDelegateBinding {
                    storage: DelegateStorage::Local(storage),
                    storage_name: storage_name.into(),
                    property_ty,
                    getter,
                    setter,
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
        let call =
            self.delegate_accessor_call(origin, depth, &delegate, delegate.getter, vec![])?;
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
        let setter = delegate.setter.ok_or_else(|| {
            self.failure(
                span,
                BodyCheckFailureKind::UnsupportedStatement(StatementForm::Assign),
            )
        })?;
        self.delegate_accessor_call(origin, depth, &delegate, setter, vec![value])
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
            let kind = checker.delegate_accessor_call(
                origin,
                depth,
                &delegate,
                delegate.getter,
                vec![],
            )?;
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

    /// Call `accessor`, a generated accessor of `delegate` declared `depth` bodies out (`u32::MAX`
    /// for this one), with `arguments`. A member of a local class declared after the property
    /// reaches the accessor through the class's field holding the delegate, which it passes as the
    /// accessor's one capture.
    fn delegate_accessor_call(
        &mut self,
        origin: OriginId,
        depth: u32,
        delegate: &LocalDelegateBinding,
        accessor: LocalDelegateAccessor,
        arguments: Vec<FirExprId>,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let target = if depth == u32::MAX && matches!(delegate.storage, DelegateStorage::Local(_)) {
            FirLocalCallableRef {
                body_depth: 0,
                callable: accessor.callable,
                declaration: Some(accessor.declaration),
                external_capture_arguments: None,
            }
        } else {
            let (body_depth, callable) = self
                .outer_callables
                .get(&accessor.declaration)
                .copied()
                .ok_or_else(|| {
                self.failure(None, BodyCheckFailureKind::MissingStableCallTarget)
            })?;
            let external_capture_arguments = match delegate.storage {
                DelegateStorage::ClassField(binding)
                    if self
                        .streamed_outer_callables
                        .contains(&accessor.declaration) =>
                {
                    let kind = self.class_storage_read_kind(
                        ClassCaptureBinding {
                            shared_cell: false,
                            ..binding
                        },
                        origin,
                    )?;
                    let field = self.body.add_expr(FirExpr {
                        origin,
                        ty: binding.ty,
                        kind,
                    });
                    Some(Box::from([field]))
                }
                DelegateStorage::ClassField(_) | DelegateStorage::Local(_) => None,
            };
            FirLocalCallableRef {
                body_depth,
                callable,
                declaration: Some(accessor.declaration),
                external_capture_arguments,
            }
        };
        Ok(FirExprKind::LocalCall {
            target,
            extension_receiver: None,
            arguments: arguments
                .into_iter()
                .enumerate()
                .map(|(parameter, value)| FirCallArgument::Expression {
                    parameter: u32::try_from(parameter).expect("an accessor takes one value"),
                    value,
                    conversion: None,
                })
                .collect(),
        })
    }

    /// Declare the accessor kotlinc generates for a local delegated property as a local function
    /// of this body: `plan.operator` called on the delegate, which is its only capture, with no
    /// owner and the property's reference, and for a setter the value it receives.
    fn local_delegate_accessor(
        &mut self,
        plan: DelegateAccessorPlan<'_>,
        property: DelegatedLocal<'_>,
    ) -> Result<LocalDelegateAccessor, BodyCheckFailure> {
        let setter = plan.setter;
        let span = self.file.stmt_spans.get(plan.statement.0 as usize).copied();
        let declaration = delegate_accessor_declaration(
            self.file,
            self.index,
            self.body.owner(),
            plan.statement,
            setter,
        )
        .ok_or_else(|| self.failure(span, BodyCheckFailureKind::MissingStableCallTarget))?;
        let callable = self.body.allocate_local_callable();
        let mut body = FirBody::new_local(self.body.owner(), callable);
        if let Some(owner) = self.body.lexical_class_owner() {
            body.set_lexical_class_owner(Some(owner));
        }
        let unit = ResolvedTy::new(Ty::Unit).expect("Unit is publishable FIR");
        let result_ty = if setter { unit } else { property.ty };
        body.set_result_type(result_ty);
        body.set_implicit_return();
        body.set_delegate_accessor();
        body.set_lifting_site(crate::fir::FirLiftingSite::from_source(plan.site, true));
        body.add_control_target(FirControlTarget {
            origin: plan.origin,
            kind: FirControlTargetKind::Body(self.body.owner()),
        });
        // Build the accessor with this checker's own helpers, then put the enclosing body back.
        let enclosing = std::mem::replace(&mut self.body, body);
        let origin = plan.origin;
        self.body.add_capture(FirCapture {
            origin,
            enclosing_depth: 0,
            source: FirCaptureSource::Value(plan.storage.value),
            ty: plan.storage.ty,
            shared_cell: false,
        });
        let receiver = self.body.add_expr(FirExpr {
            origin,
            ty: plan.storage.ty,
            kind: FirExprKind::CapturedValueRead {
                enclosing_depth: 0,
                source: plan.storage.value,
            },
        });
        let owner = self.synthetic_null(origin);
        let reference = self.local_property_reference(
            origin,
            (property.name, property.ty),
            (property.mutable, property.ordinal),
        );
        let mut arguments = vec![owner, reference];
        if setter {
            let value = self.allocate_local();
            self.body.add_parameter(FirValueParameter {
                origin,
                value,
                ty: property.ty,
                name: FirValueParameterName::PropertySetterValue,
            });
            arguments.push(self.body.add_expr(FirExpr {
                origin,
                ty: property.ty,
                kind: FirExprKind::ValueRead(value),
            }));
        }
        let kind = self.delegate_call(
            plan.operator.target.clone(),
            plan.operator.extension,
            plan.operator.parameters.clone(),
            receiver,
            arguments,
        );
        let result = self.body.add_expr(FirExpr {
            origin,
            ty: result_ty,
            kind,
        });
        let root = self.body.add_statement(FirStatement {
            origin,
            kind: FirStatementKind::Expression(result),
        });
        self.body.push_root(root);
        let body = std::mem::replace(&mut self.body, enclosing);
        // Declared in the arena, outside any block: an accessor is reached only by its calls.
        self.body.add_statement(FirStatement {
            origin,
            kind: FirStatementKind::LocalFunction {
                declaration,
                callable,
                suspend: false,
                tailrec: false,
                body: Box::new(body),
            },
        });
        Ok(LocalDelegateAccessor {
            declaration,
            callable,
        })
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
    fn local_property_reference(
        &mut self,
        cause: OriginId,
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
                mutable,
                ordinal,
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
            stable_declaration,
            external_identity,
            external_default_provider,
            params,
            declared_params,
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
                return Ok(FirDelegateCall {
                    target: callable.id.into(),
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
                    suspend: false,
                    can_inline: false,
                    inline_plan: None,
                    extension_receiver_parameter: None,
                },
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
                return Ok(FirDelegateCall {
                    target: header.id.into(),
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
                    suspend: callable.suspend,
                    can_inline: callable.inline.can_inline(),
                    inline_plan: super::inline_body_plan::publish(
                        callable.inline_body_plan.as_deref(),
                        Some(0),
                    )
                    .map_err(|_| failure(BodyCheckFailureKind::UnsupportedCallShape))?,
                    extension_receiver_parameter: None,
                },
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
                    inline_plan: super::inline_body_plan::publish(
                        inline_body_plan.as_deref(),
                        None,
                    )
                    .map_err(|_| failure(BodyCheckFailureKind::UnsupportedCallShape))?,
                    extension_receiver_parameter: Some(
                        u32::try_from(extension_parameter)
                            .map_err(|_| failure(BodyCheckFailureKind::UnsupportedCallShape))?,
                    ),
                }
            };
            Ok(FirDelegateCall {
                target,
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
