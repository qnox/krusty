//! Checked FIR for body-local function declarations and calls.

use super::*;
use crate::ast::{FunBody, FunDecl};
use crate::resolve::ResolvedLocalFunctionCall;

impl BodyFirChecker<'_> {
    pub(super) fn local_function_statement(
        &mut self,
        statement: StmtId,
        function: &FunDecl,
        origin: OriginId,
    ) -> Result<FirStatementId, BodyCheckFailure> {
        let callable = self.local_callable(statement).ok_or_else(|| {
            self.failure(
                self.file.stmt_spans.get(statement.0 as usize).copied(),
                BodyCheckFailureKind::MissingStableCallTarget,
            )
        })?;
        let declaration =
            body_local_callable_declaration(self.file, self.index, self.body.owner(), statement)
                .ok_or_else(|| {
                    self.failure(
                        self.file.stmt_spans.get(statement.0 as usize).copied(),
                        BodyCheckFailureKind::MissingStableCallTarget,
                    )
                })?;
        let Some(StmtLowering::LocalFunction(info)) = self.info.stmt_lowers.get(&statement) else {
            return Err(self.failure(
                self.file.stmt_spans.get(statement.0 as usize).copied(),
                BodyCheckFailureKind::UnsupportedStatement(StatementForm::LocalFunction),
            ));
        };
        if function.params.len() != info.sig.params.len() {
            return Err(self.failure(
                Some(function.span),
                BodyCheckFailureKind::UnsupportedCallShape,
            ));
        }
        let (root, implicit_return) = match function.body {
            FunBody::Expr(root) => (root, true),
            FunBody::Block(root) => (root, false),
            FunBody::None => {
                return Err(self.failure(
                    Some(function.span),
                    BodyCheckFailureKind::UnsupportedStatement(StatementForm::LocalFunction),
                ));
            }
        };

        let mut body = FirBody::new_local(self.body.owner(), callable);
        if let Some(owner) = self.body.lexical_class_owner() {
            body.set_lexical_class_owner(Some(owner));
        }
        body.set_result_type(self.resolved_type(function.span, info.sig.ret)?);
        if implicit_return {
            body.set_implicit_return();
        }
        body.set_debug_name(function.name.clone());
        let context_count = info.sig.context_count.min(info.sig.params.len());
        body.set_context_receiver_types(
            info.sig.params[..context_count]
                .iter()
                .map(|ty| self.resolved_type(function.span, *ty))
                .collect::<Result<Vec<_>, _>>()?,
        );
        let context_value_count = function.context_value_count().min(context_count);
        body.set_context_value_count(context_value_count as u32);
        if let Some(receiver) = info.receiver {
            let receiver_span = function
                .receiver
                .as_ref()
                .map_or(function.span, |receiver| receiver.span);
            body.set_receiver_type(self.resolved_type(receiver_span, receiver)?);
        }
        let return_target = body.add_control_target(FirControlTarget {
            origin,
            kind: FirControlTargetKind::Body(self.body.owner()),
        });
        let mut visible = info
            .captures
            .iter()
            .filter_map(|capture| {
                self.local_binding(&capture.name)
                    .map(|binding| (capture.name.clone(), (0, binding)))
                    .or_else(|| {
                        self.outer_values
                            .get(&capture.name)
                            .copied()
                            .map(|(depth, binding)| {
                                (
                                    capture.name.clone(),
                                    (
                                        depth.checked_add(1).expect("too many nested bodies"),
                                        binding,
                                    ),
                                )
                            })
                    })
            })
            .collect::<HashMap<_, _>>();
        let visible_delegates =
            info.captures
                .iter()
                .filter_map(|capture| {
                    self.local_delegate(&capture.name)
                        .map(|binding| (capture.name.clone(), (0, binding)))
                        .or_else(|| {
                            self.outer_delegates.get(&capture.name).cloned().map(
                                |(depth, binding)| {
                                    (
                                        capture.name.clone(),
                                        (
                                            depth.checked_add(1).expect("too many nested bodies"),
                                            binding,
                                        ),
                                    )
                                },
                            )
                        })
                })
                .collect::<HashMap<_, _>>();
        visible.extend(
            visible_delegates
                .iter()
                .filter_map(|(name, (depth, binding))| {
                    binding
                        .storage
                        .local()
                        .map(|storage| (name.clone(), (*depth, storage)))
                }),
        );
        let outer_callables = self.nested_outer_callables();
        let receiver_frame = self.receiver_frame();
        let class_values = self.nested_class_values()?;
        let class_delegates = self.nested_class_delegates()?;
        let class_receivers = self.nested_class_receivers()?;
        let mut nested = BodyFirChecker {
            file: self.file,
            info: self.info,
            source: self.source,
            index: self.index,
            origins: self.origins,
            body,
            scopes: vec![HashMap::new()],
            delegate_scopes: vec![HashMap::new()],
            outer_values: visible,
            outer_delegates: visible_delegates,
            class_values,
            class_delegates,
            class_receivers,
            enclosing_property: self.enclosing_property,
            session: self.session,
            return_target,
            lambda_return_source: None,
            outer_lambda_return_depths: HashMap::new(),
            function_return_depth: 0,
            loops: Vec::new(),
            local_callable_scopes: vec![HashMap::new()],
            expression_substitutions: HashMap::new(),
            outer_callables,
            streamed_outer_callables: self.streamed_outer_callables.clone(),
            nested_body_depth: self
                .nested_body_depth
                .checked_add(1)
                .expect("too many nested bodies"),
            owned_receiver_count: u32::try_from(
                context_count + usize::from(info.receiver.is_some()),
            )
            .expect("too many local-function receiver rungs"),
            outer_receiver_frames: std::iter::once(receiver_frame)
                .chain(self.outer_receiver_frames.iter().cloned())
                .collect(),
            constructor_prefix_capture_access: false,
        };
        for (ordinal, (parameter, ty)) in function
            .params
            .iter()
            .zip(info.sig.params.iter().copied())
            .enumerate()
        {
            if let Some(default) = parameter.default {
                let value = nested.expression(default)?;
                let default_origin = nested.expression_origin(default)?;
                nested.body.add_default_value(FirDefaultValue {
                    origin: default_origin,
                    parameter: u32::try_from(ordinal).map_err(|_| {
                        nested.failure(
                            Some(function.span),
                            BodyCheckFailureKind::UnsupportedCallShape,
                        )
                    })?,
                    value,
                });
            }
            if ordinal >= context_value_count && ordinal < context_count {
                continue;
            }
            let ty = nested.resolved_type(parameter.ty.span, ty)?;
            let value = if parameter.name == "_" {
                nested.allocate_local()
            } else {
                nested.bind_local(&parameter.name, ty)
            };
            nested.body.add_parameter(FirValueParameter {
                origin: nested.origins.source(nested.source, parameter.ty.span),
                value,
                ty,
            });
        }
        for capture in &info.captures {
            if let Some((depth, binding)) = nested.outer_values.get(&capture.name).copied() {
                nested.body.add_capture(FirCapture {
                    origin,
                    enclosing_depth: depth,
                    source: binding.value,
                    ty: binding.ty,
                    shared_cell: capture.shared_cell,
                });
            } else if nested.class_values.contains_key(&capture.name)
                || nested.class_delegates.contains_key(&capture.name)
            {
                // The legacy capture inventory is name-based and includes values already stored in
                // an enclosing local classifier. Checked FIR captures that classifier receiver when
                // the body materializes the field read; inventing a LocalValueId here would create
                // a second, impossible capture ABI.
            } else {
                return Err(nested.failure(Some(function.span), BodyCheckFailureKind::UnknownLocal));
            }
        }
        let result = nested.expression(root)?;
        let result_origin = nested.expression_origin(root)?;
        let root_statement = nested.body.add_statement(FirStatement {
            origin: result_origin,
            kind: FirStatementKind::Expression(result),
        });
        nested.body.push_root(root_statement);
        Ok(self.body.add_statement(FirStatement {
            origin,
            kind: FirStatementKind::LocalFunction {
                declaration,
                callable,
                suspend: function.is_suspend(),
                body: Box::new(nested.body),
            },
        }))
    }

    pub(super) fn local_function_call(
        &mut self,
        expression: ExprId,
        arguments: &[ExprId],
        selected: ResolvedLocalFunctionCall,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        if selected.provided_arg_count != arguments.len() {
            return Err(self.failure(
                self.file.expr_span(expression),
                BodyCheckFailureKind::UnsupportedCallShape,
            ));
        }
        let cause = self.expression_origin(expression)?;
        let target = self
            .local_callable_ref(selected.stmt_id, cause)?
            .ok_or_else(|| {
                self.failure(
                    self.file.expr_span(expression),
                    BodyCheckFailureKind::MissingStableCallTarget,
                )
            })?;
        Ok(FirExprKind::LocalCall {
            target,
            extension_receiver: selected
                .receiver
                .map(|receiver| {
                    if let Some(implicit) = self
                        .info
                        .implicit_receiver_selections
                        .get(&receiver)
                        .cloned()
                    {
                        let cause = self.expression_origin(expression)?;
                        return self
                            .materialize_implicit_receiver(
                                cause,
                                self.file.expr_span(receiver),
                                &implicit,
                            )?
                            .ok_or_else(|| {
                                self.failure(
                                    self.file.expr_span(receiver),
                                    BodyCheckFailureKind::UnsupportedCallShape,
                                )
                            });
                    }
                    Ok(FirReceiver {
                        value: self.expression(receiver)?,
                        conversion: None,
                    })
                })
                .transpose()?,
            arguments: self.call_arguments_with_context(
                expression,
                arguments,
                &selected.sig.params,
                selected.context_args.iter().map(Some),
                selected.sig.vararg_index,
            )?,
        })
    }

    /// Publish an operator selected from the lexical local-function scope. The receiver may be a
    /// synthesized read produced by an increment/assignment operation rather than an AST call
    /// operand, so checked local-call FIR is assembled from the selected signature and stable
    /// body-local callable identity directly.
    pub(super) fn local_operator_call_on_value(
        &mut self,
        span: Option<Span>,
        cause: OriginId,
        selected: &ResolvedLocalFunctionCall,
        receiver: FirExprId,
        operands: &[ExprId],
    ) -> Result<FirExprKind, BodyCheckFailure> {
        if selected.provided_arg_count != operands.len()
            || selected.sig.source_receiver.is_none()
            || selected.sig.vararg_index.is_some()
        {
            return Err(self.failure(span, BodyCheckFailureKind::UnsupportedCallShape));
        }
        let target = self
            .local_callable_ref(selected.stmt_id, cause)?
            .ok_or_else(|| self.failure(span, BodyCheckFailureKind::MissingStableCallTarget))?;
        let context_count = selected.sig.context_count.min(selected.sig.params.len());
        if selected.context_args.len() != context_count
            || selected.sig.params.len() != context_count + operands.len()
        {
            return Err(self.failure(span, BodyCheckFailureKind::UnsupportedCallShape));
        }
        let mut arguments = selected
            .context_args
            .iter()
            .zip(&selected.sig.params[..context_count])
            .enumerate()
            .map(|(parameter, (argument, expected))| {
                let receiver = self.materialize_context_argument_at(span, cause, argument)?;
                let actual = self.body.expr(receiver.value).ok_or_else(|| {
                    self.failure(span, BodyCheckFailureKind::UnsupportedCallShape)
                })?;
                let expected = ResolvedTy::new(*expected).map_err(|error| {
                    self.failure(span, BodyCheckFailureKind::UnpublishableType(error))
                })?;
                Ok(FirCallArgument::Expression {
                    parameter: u32::try_from(parameter).map_err(|_| {
                        self.failure(span, BodyCheckFailureKind::UnsupportedCallShape)
                    })?,
                    value: receiver.value,
                    conversion: receiver
                        .conversion
                        .or_else(|| self.selected_type_conversion(actual.ty, expected, cause)),
                })
            })
            .collect::<Result<Vec<_>, BodyCheckFailure>>()?;
        for (offset, operand) in operands.iter().copied().enumerate() {
            let parameter = context_count + offset;
            let expected = ResolvedTy::new(selected.sig.params[parameter]).map_err(|error| {
                self.failure(span, BodyCheckFailureKind::UnpublishableType(error))
            })?;
            arguments.push(FirCallArgument::Expression {
                parameter: u32::try_from(parameter)
                    .map_err(|_| self.failure(span, BodyCheckFailureKind::UnsupportedCallShape))?,
                value: self.expression(operand)?,
                conversion: self.selected_value_conversion(operand, expected, cause)?,
            });
        }
        let expected_receiver = ResolvedTy::new(
            selected
                .sig
                .source_receiver
                .expect("validated local extension receiver"),
        )
        .map_err(|error| self.failure(span, BodyCheckFailureKind::UnpublishableType(error)))?;
        let actual_receiver = self
            .body
            .expr(receiver)
            .ok_or_else(|| self.failure(span, BodyCheckFailureKind::UnsupportedCallShape))?
            .ty;
        Ok(FirExprKind::LocalCall {
            target,
            extension_receiver: Some(FirReceiver {
                value: receiver,
                conversion: self.selected_type_conversion(
                    actual_receiver,
                    expected_receiver,
                    cause,
                ),
            }),
            arguments: arguments.into_boxed_slice(),
        })
    }

    fn local_callable(&self, statement: StmtId) -> Option<LocalCallableId> {
        self.local_callable_scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(&statement).copied())
    }

    pub(super) fn local_callable_ref(
        &mut self,
        statement: StmtId,
        origin: OriginId,
    ) -> Result<Option<FirLocalCallableRef>, BodyCheckFailure> {
        let Some(declaration) =
            body_local_callable_declaration(self.file, self.index, self.body.owner(), statement)
        else {
            return Ok(None);
        };
        if let Some(callable) = self.local_callable(statement) {
            return Ok(Some(FirLocalCallableRef {
                body_depth: 0,
                callable,
                declaration: Some(declaration),
                external_capture_arguments: None,
            }));
        }
        let Some((body_depth, callable)) = self.outer_callables.get(&declaration).copied() else {
            return Ok(None);
        };
        let external_capture_arguments = if self.streamed_outer_callables.contains(&declaration) {
            let requirements = self
                .session
                .local_callables
                .get(&declaration)
                .cloned()
                .ok_or_else(|| self.failure(None, BodyCheckFailureKind::MissingStableCallTarget))?;
            let value_captures = match self.info.stmt_lowers.get(&statement) {
                Some(StmtLowering::LocalFunction(function)) => function.captures.clone(),
                _ => return Err(self.failure(None, BodyCheckFailureKind::MissingStableCallTarget)),
            };
            if value_captures.len() != requirements.captures.len() {
                return Err(self.failure(None, BodyCheckFailureKind::UnsupportedCallShape));
            }
            let mut arguments = Vec::with_capacity(
                requirements.captures.len() + requirements.implicit_receiver_captures.len(),
            );
            for (capture, required) in value_captures.iter().zip(&requirements.captures) {
                let binding = self
                    .class_values
                    .get(&capture.name)
                    .copied()
                    .or_else(|| {
                        self.class_delegates
                            .get(&capture.name)
                            .and_then(|delegate| match delegate.storage {
                                DelegateStorage::ClassField(binding) => Some(binding),
                                DelegateStorage::Local(_) => None,
                            })
                    })
                    .ok_or_else(|| {
                        self.failure(None, BodyCheckFailureKind::MissingStableCallTarget)
                    })?;
                if binding.ty != required.ty || binding.shared_cell != required.shared_cell {
                    return Err(self.failure(None, BodyCheckFailureKind::UnsupportedCallShape));
                }
                // A shared capture parameter carries the cell holder, not its Kotlin element. The
                // enclosing class field already is that holder, so suppress the ordinary semantic
                // dereference while publishing this physical closure operand.
                let kind = self.class_storage_read_kind(
                    ClassCaptureBinding {
                        shared_cell: false,
                        ..binding
                    },
                    origin,
                )?;
                arguments.push(self.body.add_expr(FirExpr {
                    origin,
                    ty: required.ty,
                    kind,
                }));
            }
            for required in &requirements.implicit_receiver_captures {
                let depth = required
                    .depth
                    .checked_add(self.owned_receiver_count)
                    .ok_or_else(|| {
                        self.failure(None, BodyCheckFailureKind::UnsupportedCallShape)
                    })?;
                let binding = self
                    .class_receiver_binding_at(depth)
                    .filter(|binding| binding.ty == required.ty)
                    .ok_or_else(|| {
                        self.failure(None, BodyCheckFailureKind::MissingStableCallTarget)
                    })?;
                let kind = self.class_storage_read_kind(binding, origin)?;
                arguments.push(self.body.add_expr(FirExpr {
                    origin,
                    ty: required.ty,
                    kind,
                }));
            }
            Some(arguments.into_boxed_slice())
        } else {
            None
        };
        Ok(Some(FirLocalCallableRef {
            body_depth,
            callable,
            declaration: Some(declaration),
            external_capture_arguments,
        }))
    }

    pub(super) fn nested_outer_callables(
        &self,
    ) -> HashMap<BodyLocalCallableDeclarationId, (u32, LocalCallableId)> {
        self.outer_callables
            .iter()
            .map(|(statement, (depth, callable))| {
                (
                    *statement,
                    (
                        depth.checked_add(1).expect("too many nested bodies"),
                        *callable,
                    ),
                )
            })
            .chain(self.local_callable_scopes.iter().flat_map(|scope| {
                scope.iter().filter_map(|(statement, callable)| {
                    body_local_callable_declaration(
                        self.file,
                        self.index,
                        self.body.owner(),
                        *statement,
                    )
                    .map(|declaration| (declaration, (1, *callable)))
                })
            }))
            .collect()
    }
}
