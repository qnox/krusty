//! Binding property headers once and checking their bodies against the selected declaration facts.

use super::*;

impl Checker<'_> {
    pub(super) fn check_property(&mut self, scope: &CheckerScope<'_>, p: &PropDecl, d: DeclId) {
        let check_ordinary_property_body = self
            .selected_body_declarations
            .as_ref()
            .is_none_or(|selected| selected.contains(&p.span));
        self.check_accessor_annotations(scope, p);
        let suppression_depth = self.check_top_level_property_annotation_applications(scope, p);
        // An extension property's own generic type parameters (`val <T> Array<T>.length: Int`)
        // scope over its receiver, declared type, and accessor bodies — bind them (erased) so
        // `T` resolves rather than reading as an unresolved reference.
        let decl_scope = scope.child(ScopeKind::Function { receiver: None });
        let scope = &decl_scope;
        let stable_property = self
            .active_declarations
            .and_then(|active| active.top_level_property_declaration(d));
        let deferred_local_header = self.has_deferred_local_header(stable_property);
        let finalized_local_header =
            stable_property
                .zip(self.resolved_index)
                .is_some_and(|(declaration, index)| {
                    index.declaration_header(declaration).is_some_and(|header| {
                        header.flags.has(crate::fir::DeclarationFlags::LOCAL_CLASS)
                    }) && !deferred_local_header
                });
        let visible_type_parameters = scope.visible_tparams();
        let type_parameters = if finalized_local_header {
            stable_property
                .and_then(|declaration| {
                    self.finalized_type_parameters(
                        &visible_type_parameters,
                        declaration,
                        &p.type_params,
                    )
                })
                .unwrap_or_else(|| {
                    if let Some(declaration) = stable_property {
                        self.failed_local_declaration_bindings.insert(declaration);
                    }
                    visible_type_parameters.clone()
                })
        } else if deferred_local_header {
            stable_property
                .and_then(|declaration| {
                    self.finalized_type_parameters(
                        &visible_type_parameters,
                        declaration,
                        &p.type_params,
                    )
                })
                .unwrap_or_else(|| {
                    if let Some(declaration) = stable_property {
                        self.failed_local_declaration_bindings.insert(declaration);
                    }
                    visible_type_parameters.clone()
                })
        } else {
            TParams::symbolic_from_decl_with(&p.type_params, &p.type_param_bounds, &|name| {
                self.select_classifier(scope, name).found()
            })
            .alpha_renamed_declaration(
                &p.type_params,
                self.compilation_id,
                self.file_index,
                p.span.lo,
            )
        };
        scope.declare_tparams(&p.type_params, &type_parameters, |name| {
            p.reified_type_params.contains(name)
        });
        self.check_declaration_type_parameter_annotations(scope, p.span.lo);
        let owned_type_parameters = self.publish_property_type_parameter_owner(scope, p);
        if finalized_local_header || deferred_local_header {
            if let Some(declaration) = stable_property {
                if !self.record_finalized_type_parameter_bounds(
                    scope,
                    declaration,
                    &p.type_params,
                    &p.type_param_bounds,
                ) {
                    self.failed_local_declaration_bindings.insert(declaration);
                }
            }
        } else if !deferred_local_header {
            for (_, bound) in &p.type_param_bounds {
                self.check_callable_type_parameter_bound(scope, bound);
            }
        }
        self.check_duplicate_param_names(&p.context_params, p.context_params.len(), p.span);
        let prev_extension_receiver = self.this_extension_receiver;
        let finalized_property_facts = finalized_local_header
            .then(|| {
                let declaration = stable_property?;
                let index = self.resolved_index?;
                let signature = index.signature(declaration)?;
                let property = index
                    .property_for_declaration(declaration)
                    .and_then(|property| index.property(property))?;
                (signature.parameters.len() == p.context_params.len()
                    && property.context_parameter_count as usize == p.context_params.len()
                    && property.extension_receiver.is_some() == p.receiver.is_some())
                .then(|| {
                    (
                        signature
                            .parameters
                            .iter()
                            .map(|parameter| parameter.get())
                            .collect::<Vec<_>>(),
                        signature.result.get(),
                        property.extension_receiver.map(crate::fir::ResolvedTy::get),
                    )
                })
            })
            .flatten();
        if finalized_local_header && finalized_property_facts.is_none() {
            if let Some(declaration) = stable_property {
                self.failed_local_declaration_bindings.insert(declaration);
            }
        }
        let property_spellings = self.finalized_declaration_spellings_for_binding(stable_property);
        // For an extension property (`val Recv.name: T get() = …`), `this` inside the
        // accessors is the receiver — carried by this rung, which ends before the delegate
        // and the initializer (both evaluated receiver-less at file initialization).
        let recv_ty = p.receiver.as_ref().map(|r| {
            if let Some(receiver) = finalized_property_facts
                .as_ref()
                .and_then(|(_, _, receiver)| *receiver)
            {
                self.record_or_bind_deferred_declaration_type(
                    scope,
                    stable_property.expect("a finalized property retains its declaration"),
                    r,
                    receiver,
                    property_spellings
                        .as_ref()
                        .and_then(|spellings| spellings.receiver()),
                )
            } else if finalized_local_header {
                Ty::Error
            } else if deferred_local_header {
                self.bind_active_deferred_declaration_type(
                    scope,
                    stable_property.expect("a deferred local property retains its declaration"),
                    r,
                    property_spellings
                        .as_ref()
                        .and_then(|spellings| spellings.receiver()),
                )
            } else if p.is_companion_extension {
                self.check_associated_companion_receiver_type(scope, r)
            } else {
                self.check_declaration_type(scope, r)
            }
        });
        let inference_key = DeclKey::declaration(self.file_index, d.0);
        let explicit_type_required = p.declared_ty().is_none()
            && p.init.is_none()
            && matches!(p.getter, Some(FunBody::Block(_)));
        self.report_property_inference_before_body(p.span, explicit_type_required);
        // Diagnostics from the accessor/initializer checks below explain an `Error` inference
        // on their own (`val LOG = logger<Foo>()` reports the unresolved reference, not a
        // second inference error — kotlinc parity).
        let body_diag_mark = self.diags.diags.len();
        let context_parameter_types = p
            .context_params
            .iter()
            .enumerate()
            .map(|(index, parameter)| {
                finalized_property_facts
                    .as_ref()
                    .and_then(|(parameters, _, _)| parameters.get(index))
                    .map(|&semantic| {
                        self.record_or_bind_deferred_parameter_type(
                            scope,
                            stable_property.expect("a finalized property retains its declaration"),
                            &parameter.ty,
                            false,
                            semantic,
                            property_spellings
                                .as_ref()
                                .and_then(|spellings| spellings.param(index)),
                        )
                    })
                    .unwrap_or_else(|| {
                        if finalized_local_header {
                            Ty::Error
                        } else if deferred_local_header {
                            self.bind_active_deferred_parameter_type(
                                scope,
                                stable_property
                                    .expect("a deferred local property retains its declaration"),
                                &parameter.ty,
                                false,
                                property_spellings
                                    .as_ref()
                                    .and_then(|spellings| spellings.param(index)),
                            )
                        } else {
                            self.check_declaration_type(scope, &parameter.ty)
                        }
                    })
            })
            .collect::<Vec<_>>();
        let context_receivers = p
            .context_params
            .iter()
            .zip(context_parameter_types.iter().copied())
            .map(|(parameter, ty)| lexical_context_receiver(self.file, parameter, ty))
            .collect::<Vec<_>>();
        // A companion-associated property opens its classifier's static scope; see functions.
        let companion_classifier = recv_ty
            .filter(|_| p.is_companion_extension)
            .and_then(Ty::obj_internal);
        let value_receiver = recv_ty.filter(|_| companion_classifier.is_none());
        let block = self.enter_block_body(companion_classifier, p.is_companion_block_member);
        let resolved_property_ty = {
            let context_scope = match companion_classifier {
                Some(classifier) => {
                    scope.declaration_companion_function_child(classifier, &context_receivers)
                }
                None => scope.declaration_function_child_with_context(
                    recv_ty,
                    p.receiver
                        .as_ref()
                        .map(|receiver| lexical_receiver_declaration(self.file, receiver)),
                    &context_receivers,
                ),
            };
            let scope = &context_scope;
            for (parameter, parameter_type) in p
                .context_params
                .iter()
                .zip(context_parameter_types.iter().copied())
            {
                if parameter.name == "_" {
                    continue;
                }
                self.declare_context_parameter(scope, &parameter.name, parameter_type);
            }
            if let Some(rt) = value_receiver {
                self.this_labels.push((
                    p.name.clone(),
                    rt,
                    false,
                    Some(ReceiverDeclarationRole::Extension(
                        p.receiver.as_ref().expect("receiver was resolved").span,
                    )),
                ));
                let label_index = self.this_labels.len() - 1;
                let receiver_span = p.receiver.as_ref().expect("receiver was resolved").span;
                self.extension_receiver_labels
                    .push((label_index, receiver_span));
                self.this_extension_receiver = Some(receiver_span);
            }
            let finalized_result = finalized_property_facts
                .as_ref()
                .map(|(_, result, _)| *result);
            let property_annotation = p.ty.as_ref().map(|reference| match finalized_result {
                Some(result) => self.record_or_bind_deferred_declaration_type(
                    scope,
                    stable_property.expect("a finalized property retains its declaration"),
                    reference,
                    result,
                    property_spellings
                        .as_ref()
                        .and_then(|spellings| spellings.ret()),
                ),
                None if finalized_local_header => Ty::Error,
                None if deferred_local_header => self.bind_active_deferred_declaration_type(
                    scope,
                    stable_property.expect("a deferred local property retains its declaration"),
                    reference,
                    property_spellings
                        .as_ref()
                        .and_then(|spellings| spellings.ret()),
                ),
                None => self.check_declaration_type(scope, reference),
            });
            let getter_annotation = p
                .getter_ty
                .as_ref()
                .map(|reference| match finalized_result {
                    Some(result) => self.record_or_bind_deferred_declaration_type(
                        scope,
                        stable_property.expect("a finalized property retains its declaration"),
                        reference,
                        result,
                        property_spellings
                            .as_ref()
                            .and_then(|spellings| spellings.getter_ret()),
                    ),
                    None if finalized_local_header => Ty::Error,
                    None if deferred_local_header => self.bind_active_deferred_declaration_type(
                        scope,
                        stable_property.expect("a deferred local property retains its declaration"),
                        reference,
                        property_spellings
                            .as_ref()
                            .and_then(|spellings| spellings.getter_ret()),
                    ),
                    None => self.check_declaration_type(scope, reference),
                });
            if let (Some(property), Some(getter)) = (property_annotation, getter_annotation) {
                if property != Ty::Error && getter != Ty::Error && property != getter {
                    self.diags.error(
                        p.getter_ty.as_ref().expect("getter annotation exists").span,
                        format!(
                            "getter return type '{}' must be equal to the property type '{}'",
                            getter.source_name(),
                            property.source_name()
                        ),
                    );
                }
            }
            let prop_ty = property_annotation
                .or(getter_annotation)
                .or_else(|| {
                    stable_property
                        .and_then(|declaration| self.resolved_index?.signature(declaration))
                        .map(|signature| signature.result.get())
                })
                .or_else(|| {
                    self.resolved_index.is_none().then(|| {
                        self.module
                            .legacy_symbols()?
                            .source_extension_property((self.file_index, d.0))
                            .map(|signature| signature.ty)
                    })?
                })
                .or_else(|| {
                    self.resolved_index.is_none().then(|| {
                        self.module
                            .legacy_symbols()?
                            .source_props
                            .get(&(self.file_index, d.0))
                            .map(|property| property.ty)
                    })?
                })
                .unwrap_or(Ty::Error);
            let storage_ty = self
                .stable_property_storage_type(stable_property)
                .or_else(|| {
                    self.resolved_index.is_none().then(|| {
                        self.module
                            .legacy_symbols()?
                            .source_props
                            .get(&(self.file_index, d.0))
                            .and_then(|property| property.storage_ty)
                    })?
                })
                .filter(|ty| !ty.mentions_pending())
                .unwrap_or(prop_ty);
            if prop_ty != Ty::Error {
                self.property_decl_types
                    .insert((p.span.lo, p.span.hi), prop_ty);
            }
            // A top-level computed property (`val g: T get() = …`) emits a static `getG()`, its getter
            // checked against the declared type. A stored property with no value receiver (top-level,
            // or a `companion { … }` block's, whose receiver is its classifier) binds `field`.
            let has_backing_field =
                value_receiver.is_none() && p.context_params.is_empty() && p.init.is_some();
            if let Some(g) = p
                .getter
                .as_ref()
                .filter(|body| self.should_check_selected_fun_body(body))
            {
                let field_ty = has_backing_field.then_some(storage_ty);
                self.with_ret_field(prop_ty, field_ty, scope, |c| match g {
                    FunBody::Expr(e) => {
                        c.check_expression_getter(scope, *e, prop_ty, p.ty.is_some());
                    }
                    FunBody::Block(b) => {
                        c.check_executable_body(scope, *b);
                    }
                    FunBody::None => {}
                });
            }
            // A setter body: an extension property's is checked with `this` = receiver; a top-level
            // backing-field property's binds `field` to the property type. Both bind the value param.
            if let Some((setter, body)) = p
                .setter
                .as_ref()
                .and_then(|setter| setter.body.as_ref().map(|body| (setter, body)))
                .filter(|(_, body)| self.should_check_selected_fun_body(body))
            {
                let field_ty = has_backing_field.then_some(storage_ty);
                self.with_ret_field(Ty::Unit, field_ty, scope, |c| {
                    let setter_scope = scope.child(ScopeKind::Function { receiver: None });
                    let scope = &setter_scope;
                    let pname = crate::ast::setter_param_or_value(setter.param.as_ref());
                    c.declare(scope, &pname, prop_ty, true);
                    match body {
                        FunBody::Expr(g) | FunBody::Block(g) => {
                            c.check_executable_body(scope, *g);
                        }
                        FunBody::None => {}
                    }
                });
            }
            if value_receiver.is_some() {
                self.extension_receiver_labels.pop();
                self.this_labels.pop();
            }
            prop_ty
        };
        let jvm_field_span = self
            .resolved_annotation_in_scope(scope, &p.annotations, "kotlin/jvm/JvmField")
            .map(|annotation| Span::new(annotation.at, annotation.span.hi));
        if let Some(span) = jvm_field_span.filter(|_| self.ty_is_value_class(resolved_property_ty))
        {
            self.diags.error(
                span,
                "jvmField cannot be applied to a property of a value class type.".to_string(),
            );
        }
        self.this_extension_receiver = prev_extension_receiver;
        // A delegated property's delegate expression (`by Del()`) must be type-checked so its
        // (and its sub-expressions') types are recorded for the lowering of `x$delegate`.
        if let Some(de) = p.delegate.filter(|_| check_ordinary_property_body) {
            let this_ref = recv_ty.unwrap_or(Ty::Null);
            let site = DelegateConventionSite::of(p, None, recv_ty);
            let (dt, _) = self.check_delegate_getvalue(
                scope,
                de,
                Ty::Null,
                this_ref,
                Some(resolved_property_ty),
                site.clone(),
            );
            if p.is_var {
                self.record_delegate_setvalue(scope, de, dt, this_ref, resolved_property_ty, site);
            }
            self.finish_executable_root(de);
        }
        if let Some(init) = p.init.filter(|_| check_ordinary_property_body) {
            let declared = if p.explicit_backing_field.is_some() {
                self.stable_property_storage_type(stable_property)
                    .or_else(|| {
                        self.resolved_index.is_none().then(|| {
                            self.module
                                .legacy_symbols()?
                                .source_props
                                .get(&(self.file_index, d.0))
                                .and_then(|property| property.storage_ty)
                        })?
                    })
                    .filter(|ty| !ty.mentions_pending() && !ty.mentions_error())
            } else {
                (!resolved_property_ty.mentions_error()).then_some(resolved_property_ty)
            };
            let it = match declared.filter(|_| p.declared_ty().is_some()) {
                Some(expected) => self.expr_declared(scope, init, expected),
                None => self.expr(scope, init),
            };
            if p.declared_ty().is_none() {
                self.report_unbound_call_result_type_parameter(scope, init);
            }
            if let Some(declared) = declared {
                if p.declared_ty().is_some() {
                    let actual =
                        self.recorded_expression_type_for_expected(scope, init, it, declared);
                    self.narrow_platform_value(declared, init, PlatformNarrowing::Declaration);
                    self.expect_assignable(
                        declared,
                        actual,
                        self.value_diagnostic_span(init, actual),
                        "property initializer",
                    );
                }
            }
            // A `const val`'s initializer is part of its stable declaration payload. Fold it only
            // after ordinary checking has bound every constant read and selected every operator;
            // the evaluator consumes those exact decisions and cannot mistake an overloaded
            // convention for a builtin operation. The payload is also attached to this root
            // expression so checked FIR emits a literal rather than a runtime `<clinit>` program.
            if p.is_const && !resolved_property_ty.mentions_error() {
                let context = constant_evaluation::CheckedConstantExpression {
                    file: self.file,
                    expression_types: &self.expr_types,
                    resolved_constants: &self.resolved_constants,
                    resolved_calls: &self.resolved_calls,
                    resolved_operator_calls: &self.resolved_operator_calls,
                };
                let folded = checked_constant_expression(context, init, resolved_property_ty);
                if let Some(folded) = folded {
                    self.resolved_constants.insert(init, folded);
                } else if checked_integer_constant(context, init)
                    == Some(IntegerConstant::DivisionByZero)
                {
                    // The expression adapts as an `Int` constant, but it has no magnitude to
                    // publish. kotlinc reports this instead of a type mismatch.
                    self.diags.error(
                        self.span(init),
                        "const 'val' initializer must be a constant value.".to_string(),
                    );
                }
            }
            self.finish_executable_root(init);
        }
        // The signature passes leave the type `Error` without a diagnostic; the checker reports the
        // inference failure only when neither the getter nor the initializer diagnosed itself — the
        // same coverage the signature passes had (top-level, non-extension, init or getter, no
        // delegate), now with the unresolved-reference cascade suppressed.
        if check_ordinary_property_body
            && resolved_property_ty == Ty::Error
            // A stable declaration with no published signature is already owned by Pass 1's
            // signature diagnostic. Pass 2 still checks its body for independent errors, but must
            // not invent a second generic inference error after consuming the compact failure.
            && !self.stable_signature_failed(stable_property)
            && p.receiver.is_none()
            && p.declared_ty().is_none()
            && (p.init.is_some() || p.getter.is_some())
            && p.delegate.is_none()
        {
            self.report_property_inference_failure(
                inference_key,
                p.span,
                &p.name,
                body_diag_mark,
                explicit_type_required,
            );
        }
        self.leave_block_body(block);
        self.retire_type_parameter_owners(&owned_type_parameters);
        self.active_lexical_policies.truncate(suppression_depth);
    }

    pub(super) fn report_property_inference_failure(
        &mut self,
        key: DeclKey,
        declaration_span: Span,
        name: &str,
        body_diag_mark: usize,
        explicit_type_required: bool,
    ) {
        if explicit_type_required {
            return;
        }
        match self
            .module
            .legacy_symbols()
            .and_then(|symbols| symbols.property_inference_failures.get(&key).cloned())
        {
            Some(PropertyInferenceFailure::Recursive { span }) => {
                self.diags.error(span, RECURSIVE_INFERENCE_MESSAGE);
            }
            Some(PropertyInferenceFailure::UninitializedReads(reads)) => {
                for (span, name) in reads {
                    self.diags
                        .error(span, format!("variable '{name}' must be initialized."));
                }
            }
            Some(PropertyInferenceFailure::Untypeable) | None
                if !self.diags.diags[body_diag_mark..]
                    .iter()
                    .any(|diagnostic| diagnostic.severity == crate::diag::Severity::Error) =>
            {
                self.diags
                    .error(declaration_span, cannot_infer_property_message(name));
            }
            Some(PropertyInferenceFailure::Untypeable) | None => {}
        }
    }

    pub(super) fn stable_signature_failed(
        &self,
        declaration: Option<crate::fir::DeclarationId>,
    ) -> bool {
        declaration.is_some_and(|declaration| {
            self.resolved_index
                .is_some_and(|index| index.signature(declaration).is_none())
        })
    }

    pub(super) fn report_property_inference_before_body(
        &mut self,
        declaration_span: Span,
        explicit_type_required: bool,
    ) {
        if explicit_type_required {
            self.diags
                .error(declaration_span, EXPLICIT_PROPERTY_TYPE_MESSAGE);
        }
    }

    /// A concrete result constraint available without resolving every read in a local property's
    /// initializer. This is intentionally a lower bound, not a type answer: callers feed it back as
    /// an expectation and accept it only if the ordinary semantic checker validates the complete
    /// expression. It breaks bounded self-reference such as `next?.index ?: current` without ever
    /// publishing `Pending` or asking lowering to infer.
    pub(super) fn local_property_inference_constraint(
        &self,
        scope: &CheckerScope<'_>,
        expression: ExprId,
    ) -> Option<Ty> {
        let usable = |ty: Ty| (ty != Ty::Error && !ty.mentions_pending()).then_some(ty);
        let constrained_join = |left: Ty, right: Ty| {
            if left == right || self.receiver_is_assignable(right, left) {
                Some(left)
            } else if self.receiver_is_assignable(left, right) {
                Some(right)
            } else {
                None
            }
        };
        match self.file.expr(expression) {
            Expr::IntLit(_) => Some(Ty::Int),
            Expr::LongLit(_) => Some(Ty::Long),
            Expr::UIntLit(_) => Some(Ty::UInt),
            Expr::ULongLit(_) => Some(Ty::ULong),
            Expr::DoubleLit(_) => Some(Ty::Double),
            Expr::FloatLit(_) => Some(Ty::Float),
            Expr::BoolLit(_) => Some(Ty::Boolean),
            Expr::CharLit(_) => Some(Ty::Char),
            Expr::StringLit(_) | Expr::Template(_) => Some(Ty::String),
            Expr::Name(name) => self.lookup(scope, name).and_then(|local| usable(local.ty)),
            Expr::NotNull { operand } => self
                .local_property_inference_constraint(scope, *operand)
                .map(Ty::non_null),
            Expr::Unary { op, operand } => match op {
                UnOp::Not => Some(Ty::Boolean),
                UnOp::Neg | UnOp::Plus => self.local_property_inference_constraint(scope, *operand),
            },
            Expr::Elvis { lhs, rhs } => {
                let left = self
                    .local_property_inference_constraint(scope, *lhs)
                    .map(Ty::non_null);
                let right = self.local_property_inference_constraint(scope, *rhs);
                match (left, right) {
                    (Some(left), Some(right)) => constrained_join(left, right),
                    (Some(known), None) | (None, Some(known)) => Some(known),
                    (None, None) => None,
                }
            }
            Expr::Binary { op, lhs, rhs, .. } => {
                let left = self.local_property_inference_constraint(scope, *lhs);
                let right = self.local_property_inference_constraint(scope, *rhs);
                if matches!(
                    op,
                    BinOp::Lt
                        | BinOp::Le
                        | BinOp::Gt
                        | BinOp::Ge
                        | BinOp::Eq
                        | BinOp::Ne
                        | BinOp::And
                        | BinOp::Or
                        | BinOp::RefEq
                        | BinOp::RefNe
                ) {
                    return Some(Ty::Boolean);
                }
                match (left, right) {
                    (Some(left), Some(right)) => {
                        binary_result(&self.fed_source(), *op, left, right).ok()
                    }
                    (Some(known), None) | (None, Some(known))
                        if known.is_numeric() || known == Ty::Char || known == Ty::String =>
                    {
                        binary_result(&self.fed_source(), *op, known, known).ok()
                    }
                    _ => None,
                }
            }
            Expr::If {
                then_branch,
                else_branch: Some(else_branch),
                ..
            } => {
                let then_ty = self.local_property_inference_constraint(scope, *then_branch);
                let else_ty = self.local_property_inference_constraint(scope, *else_branch);
                match (then_ty, else_ty) {
                    (Some(left), Some(right)) => constrained_join(left, right),
                    (Some(known), None) | (None, Some(known)) => Some(known),
                    (None, None) => None,
                }
            }
            Expr::Block {
                trailing: Some(trailing),
                ..
            } => self.local_property_inference_constraint(scope, *trailing),
            // A member read with an unresolved receiver is exactly the recursive edge this helper
            // is meant to leave open. Its sibling/branch supplies the constraint; the full checker
            // then resolves this read through the provisional SourceMember overlay.
            Expr::Member { .. }
            | Expr::SafeCall { .. }
            | Expr::SafeIndex { .. }
            | Expr::SafeIndexIncDec { .. }
            | Expr::Call { .. }
            | Expr::Index { .. }
            | Expr::CallableRef { .. }
            | Expr::ExtensionAccess { .. }
            | Expr::RangeTo { .. }
            | Expr::InRange { .. }
            | Expr::IncDec { .. }
            | Expr::Try { .. }
            | Expr::When { .. }
            | Expr::As { .. }
            | Expr::Is { .. }
            | Expr::Lambda { .. }
            | Expr::AnnotationArrayLiteral(_)
            | Expr::NullLit
            | Expr::Throw { .. }
            | Expr::Return { .. }
            | Expr::Break { .. }
            | Expr::Continue { .. }
            | Expr::UnsupportedAnnotationArgument(_)
            | Expr::If {
                else_branch: None, ..
            }
            | Expr::Block { trailing: None, .. } => None,
        }
    }
}
