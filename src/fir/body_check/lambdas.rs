//! Checked FIR construction for body-local anonymous callables.

use super::*;

impl BodyFirChecker<'_> {
    pub(super) fn lambda(
        &mut self,
        expression: ExprId,
        source_parameters: &[String],
        root: ExprId,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        crate::trace_compiler!(
            "fir",
            "checked lambda expression={expression:?} type={:?} parameters={source_parameters:?} lowering={:?}",
            self.info.semantic_ty(expression),
            self.info.expr_lowers.get(&expression),
        );
        let Ty::Fun(signature) = self.info.semantic_ty(expression).non_null() else {
            return Err(self.failure(
                self.file.expr_span(expression),
                BodyCheckFailureKind::UnsupportedExpression(ExpressionForm::Lambda),
            ));
        };
        let context_count = signature.context_count.min(signature.params.len());
        let named_context_count = self
            .file
            .anon_fun_context_count
            .get(&expression.0)
            .copied()
            .unwrap_or(0) as usize;
        if named_context_count > context_count || named_context_count > source_parameters.len() {
            return Err(self.failure(
                self.file.expr_span(expression),
                BodyCheckFailureKind::UnsupportedExpression(ExpressionForm::Lambda),
            ));
        }
        let receiver_count = usize::from(signature.has_receiver);
        let value_parameters = signature
            .params
            .get(context_count + receiver_count..)
            .ok_or_else(|| {
                self.failure(
                    self.file.expr_span(expression),
                    BodyCheckFailureKind::UnsupportedExpression(ExpressionForm::Lambda),
                )
            })?;
        let (context_parameter_names, source_value_parameters) =
            source_parameters.split_at(named_context_count);
        let parameter_names = match (source_value_parameters, value_parameters) {
            ([], [_]) => vec!["it"],
            ([], []) => Vec::new(),
            (names, parameters) if names.len() == parameters.len() => {
                names.iter().map(String::as_str).collect()
            }
            _ => {
                return Err(self.failure(
                    self.file.expr_span(expression),
                    BodyCheckFailureKind::UnsupportedExpression(ExpressionForm::Lambda),
                ));
            }
        };
        let span = self
            .file
            .expr_span(expression)
            .ok_or_else(|| self.failure(None, BodyCheckFailureKind::MissingSourceSpan))?;
        let callable = self.body.allocate_local_callable();
        let owner = self.body.owner();
        let target_origin = self.origins.source(self.source, span);
        let mut body = FirBody::new_local(owner, callable);
        if let Some(name) = self.body.debug_name() {
            body.set_debug_name(name.to_owned());
        }
        body.mark_source_lambda(self.lambda_binding_name.clone());
        if let Some(owner) = self.body.lexical_class_owner() {
            body.set_lexical_class_owner(Some(owner));
        }
        let result_type = self.resolved_type(span, signature.ret)?;
        body.set_result_type(result_type);
        body.set_implicit_return();
        let context_receivers = signature.params[..context_count]
            .iter()
            .map(|receiver| self.resolved_type(span, *receiver))
            .collect::<Result<Vec<_>, _>>()?;
        body.set_context_receiver_types(context_receivers);
        body.set_context_value_count(
            u32::try_from(named_context_count).expect("too many named context parameters"),
        );
        if signature.has_receiver {
            let receiver = self.resolved_type(span, signature.params[context_count])?;
            body.set_receiver_type(receiver);
        }
        let return_target = body.add_control_target(FirControlTarget {
            origin: target_origin,
            kind: FirControlTargetKind::Body(owner),
        });
        let mut outer_lambda_return_depths = self
            .outer_lambda_return_depths
            .iter()
            .map(|(source, depth)| {
                (
                    *source,
                    depth.checked_add(1).expect("too many nested lambdas"),
                )
            })
            .collect::<HashMap<_, _>>();
        if let Some(source) = self.lambda_return_source {
            outer_lambda_return_depths.insert(source, 1);
        }
        let mut outer_values = self
            .outer_values
            .iter()
            .map(|(name, (depth, value))| {
                (
                    name.clone(),
                    (
                        depth.checked_add(1).expect("too many nested lambdas"),
                        *value,
                    ),
                )
            })
            .collect::<HashMap<_, _>>();
        for scope in &self.scopes {
            outer_values.extend(
                scope
                    .iter()
                    .map(|(name, value)| (name.clone(), (0, *value))),
            );
        }
        crate::trace_compiler!(
            "fir",
            "checked lambda captures expression={expression:?} outer_values={:?}",
            outer_values.keys().collect::<Vec<_>>(),
        );
        let mut outer_delegates = self
            .outer_delegates
            .iter()
            .map(|(name, (depth, binding))| {
                (
                    name.clone(),
                    (
                        depth.checked_add(1).expect("too many nested lambdas"),
                        binding.clone(),
                    ),
                )
            })
            .collect::<HashMap<_, _>>();
        for scope in &self.delegate_scopes {
            outer_delegates.extend(
                scope
                    .iter()
                    .map(|(name, binding)| (name.clone(), (0, binding.clone()))),
            );
        }
        outer_values.extend(
            outer_delegates
                .iter()
                .filter_map(|(name, (depth, binding))| {
                    binding
                        .storage
                        .local()
                        .map(|storage| (name.clone(), (*depth, storage)))
                }),
        );
        let receiver_frame = self.receiver_frame();
        let class_values = self.nested_class_values()?;
        let class_capture_values = self.nested_class_capture_values()?;
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
            outer_values,
            outer_delegates,
            class_values,
            class_capture_values,
            class_delegates,
            class_receivers,
            enclosing_property: self.enclosing_property,
            session: self.session,
            return_target,
            lambda_return_source: Some(expression),
            outer_lambda_return_depths,
            function_return_depth: self
                .function_return_depth
                .checked_add(1)
                .expect("too many nested lambdas"),
            loops: if matches!(
                self.info.expr_lowers.get(&expression),
                Some(ExprLowering::Lambda(crate::resolve::LambdaInfo {
                    capture: crate::resolve::LambdaCapture::InlineSplice,
                    ..
                }))
            ) {
                self.loops
                    .iter()
                    .map(|(label, depth, target)| {
                        (
                            label.clone(),
                            depth
                                .checked_add(1)
                                .expect("too many nested inline lambdas"),
                            *target,
                        )
                    })
                    .collect()
            } else {
                Vec::new()
            },
            local_callable_scopes: vec![HashMap::new()],
            expression_substitutions: HashMap::new(),
            lambda_binding_name: None,
            outer_callables: self
                .outer_callables
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
                .collect(),
            streamed_outer_callables: self.streamed_outer_callables.clone(),
            nested_body_depth: self
                .nested_body_depth
                .checked_add(1)
                .expect("too many nested bodies"),
            expression_depth: 0,
            // Named context parameters are lexical values, not implicit-receiver tower rungs.
            owned_receiver_count: u32::try_from(
                context_count - named_context_count + receiver_count,
            )
            .expect("too many lambda receiver rungs"),
            outer_receiver_frames: std::iter::once(receiver_frame)
                .chain(self.outer_receiver_frames.iter().cloned())
                .collect(),
            constructor_prefix_capture_access: false,
        };
        for (name, ty) in context_parameter_names
            .iter()
            .map(String::as_str)
            .zip(signature.params[..named_context_count].iter().copied())
        {
            let ty = nested.resolved_type(span, ty)?;
            let value = nested.bind_local(name, ty);
            nested.body.add_parameter(FirValueParameter {
                origin: target_origin,
                value,
                ty,
            });
        }
        for (name, ty) in parameter_names
            .into_iter()
            .zip(value_parameters.iter().copied())
        {
            let ty = nested.resolved_type(span, ty)?;
            let value = nested.bind_local(name, ty);
            nested.body.add_parameter(FirValueParameter {
                origin: target_origin,
                value,
                ty,
            });
        }
        // The lambda's callable result is a real value boundary. Keep the source expression's
        // checked type on its child and publish the exact conversion to the contextually selected
        // function result on the FIR node. In particular, an effect-only block (`{ while (…) {} }`)
        // has source type `Unit`; when selected as `() -> Any?`, lowering must materialize the Unit
        // value before returning it instead of treating the effect-only root as an Object value.
        let result = nested.value_at_selected_boundary(root, result_type)?;
        let result_origin = nested.expression_origin(root)?;
        let statement = nested.body.add_statement(FirStatement {
            origin: result_origin,
            kind: FirStatementKind::Expression(result),
        });
        nested.body.push_root(statement);
        Ok(FirExprKind::Lambda {
            callable,
            body: Box::new(nested.body),
        })
    }
}
