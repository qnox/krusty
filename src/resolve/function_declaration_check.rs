//! Checking top-level and body-local function declarations against their owned stable headers.

use super::*;

impl Checker<'_> {
    pub(super) fn check_fun(
        &mut self,
        scope: &CheckerScope<'_>,
        f: &FunDecl,
        source_decl: Option<DeclId>,
    ) {
        let public_api_inline = self.enter_public_api_inline(scope, f);
        let enclosing_return_frame = self.lambda_returns.enter_function(Some(f.name.clone()));
        let stable_declaration = source_decl.and_then(|source| {
            self.active_declarations
                .and_then(|active| active.file_declaration(self.file, source))
        });
        let deferred_local_header = self.has_deferred_local_header(stable_declaration);
        let finalized_local_header =
            stable_declaration
                .zip(self.resolved_index)
                .is_some_and(|(declaration, index)| {
                    index.declaration_header(declaration).is_some_and(|header| {
                        header.flags.has(crate::fir::DeclarationFlags::LOCAL_CLASS)
                    }) && !deferred_local_header
                });
        let suppression_depth =
            self.check_function_annotation_applications(scope, f, "top level function");
        self.check_infix_declaration(f, false);
        let owned_type_parameters = self.publish_function_type_parameter_owner(scope, f);
        if finalized_local_header {
            if let Some(declaration) = stable_declaration {
                if !self.record_finalized_type_parameter_bounds(
                    declaration,
                    &f.type_params,
                    &f.type_param_bounds,
                ) {
                    self.failed_local_declaration_bindings.insert(declaration);
                }
            }
        } else if !deferred_local_header {
            for (_, bound) in &f.type_param_bounds {
                self.check_callable_type_parameter_bound(scope, bound);
            }
        }
        self.check_duplicate_param_names(&f.params, f.context_count, f.span);
        // Duplicate type-parameter names (`fun <T, T> f()`) are illegal (conflicting declaration).
        {
            let mut seen = crate::fx_hash::HashSet::default();
            for tp in &f.type_params {
                if !seen.insert(tp.as_str()) {
                    self.diags.error(
                        f.span,
                        format!("conflicting declaration: type parameter '{tp}' is declared more than once"),
                    );
                }
            }
        }
        // At most one `vararg` parameter is allowed (kotlinc: multiple vararg parameters are prohibited).
        let vararg_count = f
            .params
            .iter()
            .filter(|parameter| parameter.is_vararg)
            .count();
        if vararg_count > 1 {
            for parameter in f.params.iter().filter(|parameter| parameter.is_vararg) {
                self.diags.error(
                    parameter
                        .vararg_span
                        .expect("a parsed vararg parameter must retain its modifier span"),
                    "multiple vararg parameters are prohibited.".to_string(),
                );
            }
        }
        // A `reified` type parameter is only allowed on an `inline` function (kotlinc rejects it
        // otherwise — reification needs the body inlined at the call site).
        if !f.reified_type_params.is_empty() && !f.is_inline() {
            self.diags.error(
                f.span,
                "'reified' type parameter is only allowed on an 'inline' function".to_string(),
            );
        }
        let check_ordinary_body = self
            .selected_body_declarations
            .as_ref()
            .is_none_or(|selected| selected.contains(&f.span));
        self.reset_body_mutations(
            (!self.signature_defaults_only && check_ordinary_body)
                .then(|| fun_body_expr(&f.body))
                .flatten(),
        );
        // Inline functions are expanded at each call site by the lowerer (like kotlinc's inliner);
        // a body that is ALSO emitted standalone (a facade static for cross-file calls — see
        // `inline_fn_facade_emittable`) is lowered identically to a plain function, so checking it
        // here covers both. A lambda *parameter* of an inline function may be invoked on a mutable
        // capture (it ends up inlined into the caller), so permit mutation while checking the body.
        let prev_allow = self.allow_lambda_mutation;
        if f.is_inline() {
            self.allow_lambda_mutation = true;
        }
        let (finalized_result, finalized_parameters) = (!deferred_local_header)
            .then(|| {
                stable_declaration
                    .and_then(|declaration| self.resolved_index?.signature(declaration))
            })
            .flatten()
            .map(|signature| {
                (
                    Some(signature.result.get()),
                    Some(
                        signature
                            .parameters
                            .iter()
                            .map(|parameter| parameter.get())
                            .collect::<Vec<_>>(),
                    ),
                )
            })
            .unwrap_or((None, None));
        let finalized_extension_receiver = (!deferred_local_header)
            .then(|| {
                stable_declaration.and_then(|declaration| {
                    self.resolved_index?.callable_for_declaration(declaration)
                })
            })
            .flatten()
            .and_then(|callable| callable.shape.extension_receiver)
            .map(crate::fir::ResolvedTy::get);
        let finalized_callable_shape_matches = !finalized_local_header
            || (finalized_parameters
                .as_ref()
                .is_some_and(|parameters| parameters.len() == f.params.len())
                && stable_declaration
                    .and_then(|declaration| {
                        self.resolved_index?.callable_for_declaration(declaration)
                    })
                    .is_some_and(|callable| {
                        callable.shape.context_parameter_count as usize == f.context_count
                            && callable.shape.extension_receiver.is_some() == f.receiver.is_some()
                    }));
        if !finalized_callable_shape_matches {
            if let Some(declaration) = stable_declaration {
                self.failed_local_declaration_bindings.insert(declaration);
            }
        }
        let finalized_result = finalized_callable_shape_matches
            .then_some(finalized_result)
            .flatten();
        let finalized_parameters = finalized_callable_shape_matches
            .then_some(finalized_parameters)
            .flatten();
        let finalized_extension_receiver = finalized_callable_shape_matches
            .then_some(finalized_extension_receiver)
            .flatten();
        let spellings = self.finalized_declaration_spellings(stable_declaration);
        let parameter_types = match finalized_parameters
            .as_deref()
            .filter(|parameters| parameters.len() == f.params.len())
        {
            Some(parameters) => f
                .params
                .iter()
                .zip(parameters.iter().copied())
                .enumerate()
                .map(|(index, (parameter, semantic))| {
                    self.record_finalized_parameter_type(
                        &parameter.ty,
                        parameter.is_vararg,
                        semantic,
                        spellings.param(index),
                    )
                })
                .collect::<Vec<_>>(),
            None if finalized_local_header => vec![Ty::Error; f.params.len()],
            None => f
                .params
                .iter()
                .map(|parameter| {
                    semantic_value_parameter_ty(
                        self.check_declaration_type(scope, &parameter.ty),
                        parameter.is_vararg,
                    )
                })
                .collect::<Vec<_>>(),
        };
        // Extension function: look up in ext_funs table; set this_ty to the receiver type.
        let prev_extension_receiver = self.this_extension_receiver;
        // `this` inside the body and inside every parameter default is the extension receiver.
        let extension_receiver = f.receiver.as_ref().map(|r| {
            if let Some(receiver) = finalized_extension_receiver {
                self.record_finalized_declaration_type(r, receiver, &spellings.receiver)
            } else if finalized_local_header {
                Ty::Error
            } else if f.is_companion_extension() {
                self.check_associated_companion_receiver_type(scope, r)
            } else {
                self.check_declaration_type(scope, r)
            }
        });
        let context_receivers = f
            .params
            .iter()
            .zip(&parameter_types)
            .take(f.context_count)
            .map(|(parameter, &ty)| lexical_context_receiver(self.file, parameter, ty))
            .collect::<Vec<_>>();
        // A companion-associated function opens its classifier's static scope, with no `this`.
        let companion_classifier = extension_receiver
            .filter(|_| f.is_companion_extension())
            .and_then(Ty::obj_internal);
        let block = self.enter_block_body(companion_classifier, f.is_companion_block_member());
        if let Some(recv_ref) = &f.receiver {
            let recv_ty = extension_receiver.expect("receiver was resolved");
            if companion_classifier.is_none() {
                self.this_labels.push((
                    f.name.clone(),
                    recv_ty,
                    false,
                    Some(ReceiverDeclarationRole::Extension(recv_ref.span)),
                ));
                let label_index = self.this_labels.len() - 1;
                self.extension_receiver_labels
                    .push((label_index, recv_ref.span));
                self.this_extension_receiver = Some(recv_ref.span);
            }
            // Pick THIS declaration's overload out of the receiver+name overload set by matching its
            // parameter list (an extension may be overloaded by arity — `fun R.f()` and `fun R.f(x)`).
            let want = parameter_types.clone();
            self.ret_ty = if let Some(ret) = &f.ret {
                if let Some(result) = finalized_result {
                    self.record_finalized_declaration_type(ret, result, &spellings.ret)
                } else if finalized_local_header {
                    Ty::Error
                } else {
                    self.check_declaration_type(scope, ret)
                }
            } else {
                finalized_result
                    .or_else(|| {
                        source_decl
                            .and_then(|declaration| {
                                self.module.legacy_symbols()?.source_extension_signature(
                                    &f.name,
                                    self.file_index,
                                    declaration,
                                )
                            })
                            .map(|(_, signature)| {
                                signature
                                    .generic_sig
                                    .as_ref()
                                    .map_or(signature.ret, |generic| generic.ret)
                            })
                    })
                    .or_else(|| {
                        self.module
                            .legacy_symbols()?
                            .ext_fun_overloads_in_import_scope(
                                recv_ty,
                                &f.name,
                                &self.function_import_scope,
                            )
                            .iter()
                            .find(|signature| signature.params == want)
                            .map(|signature| {
                                signature
                                    .generic_sig
                                    .as_ref()
                                    .map_or(signature.ret, |generic| generic.ret)
                            })
                    })
                    .unwrap_or(Ty::Unit)
            };
        } else {
            // Use this declaration's own collected return type; companion methods fall back to the
            // declared return type because they are not stored in `funs`.
            let want = parameter_types
                .iter()
                .copied()
                .map(erased_type_key)
                .collect::<Vec<_>>();
            let explicit_result = f.ret.as_ref().map(|ret| {
                if let Some(result) = finalized_result {
                    self.record_finalized_declaration_type(ret, result, &spellings.ret)
                } else if finalized_local_header {
                    Ty::Error
                } else {
                    self.check_declaration_type(scope, ret)
                }
            });
            let own_ret = explicit_result
                .or(finalized_result)
                .or_else(|| {
                    source_decl
                        .and_then(|declaration| {
                            self.module.legacy_symbols()?.source_function_signature(
                                &f.name,
                                self.file_index,
                                declaration,
                            )
                        })
                        .map(|signature| {
                            signature
                                .generic_sig
                                .as_ref()
                                .map_or(signature.ret, |generic| generic.ret)
                        })
                })
                .or_else(|| {
                    self.module
                        .legacy_symbols()?
                        .fun_ret_by_erased_params(&f.name, &want)
                });
            self.ret_ty = own_ret.unwrap_or(Ty::Unit);
        }
        // For expression-body functions with no explicit return type, infer the return type from the
        // body expression and write it back to the canonical signature table. Later call resolution and
        // lowering then consume the same `Signature::ret` field as annotated functions.
        // An expression-body function without an annotation gets its return from the body in the
        // declaration's current semantic scope. Collection may have a provisional result, but that
        // result cannot replace inference: for `fun <T> content(x: T) = Content(x)` it carries an
        // unscoped placeholder and would make the checker compare `Content<T#decl>` with `Content<T>`.
        let infer_ret = !self.signature_defaults_only
            && check_ordinary_body
            && f.ret.is_none()
            && matches!(&f.body, FunBody::Expr(_))
            // Pass 1 owns every non-local signature decision. Pass 2 still checks the expression
            // body against that result, but must not infer and republish a second signature from
            // the reparsed body. Ordinary local declarations have no finalized entry and continue
            // to infer on their lexical rung below.
            && !self.has_finalized_signature(stable_declaration);
        if !self.signature_defaults_only && !infer_ret {
            self.check_operator_declaration(
                f,
                self.ret_ty,
                operator_declarations::OperatorDeclarationSite::NonMember,
            );
        }
        // Default arguments are evaluated in the caller's context. The extension receiver and
        // preceding value parameters are available, while later parameters are not.
        {
            let defaults_scope = match companion_classifier {
                Some(classifier) => {
                    scope.declaration_companion_function_child(classifier, &context_receivers)
                }
                None => scope.declaration_function_child_with_context(
                    extension_receiver,
                    f.receiver
                        .as_ref()
                        .map(|receiver| lexical_receiver_declaration(self.file, receiver)),
                    &context_receivers,
                ),
            };
            let scope = &defaults_scope;
            self.check_parameter_defaults(
                scope,
                f.params
                    .iter()
                    .map(|parameter| (parameter.name.as_str(), parameter.default)),
                &parameter_types,
                ParameterDefaultFlow::Function,
            );
        }
        {
            let params_scope = match companion_classifier {
                Some(classifier) => {
                    scope.declaration_companion_function_child(classifier, &context_receivers)
                }
                None => scope.declaration_function_child_with_context(
                    extension_receiver,
                    f.receiver
                        .as_ref()
                        .map(|receiver| lexical_receiver_declaration(self.file, receiver)),
                    &context_receivers,
                ),
            };
            let scope = &params_scope;
            for (index, (p, &ty)) in f.params.iter().zip(&parameter_types).enumerate() {
                if p.name != "_" {
                    self.declare_function_parameter(scope, p, ty, None, index < f.context_count);
                }
            }
            if self.signature_defaults_only {
                // A default owned by a local declaration must be entered through the enclosing
                // body's lexical statement path so preceding locals, classifiers, receivers, and
                // type parameters exist exactly as written. The selected set contains the nested
                // declaration rather than this enclosing function. Walk this bounded Pass-1 unit
                // for that dependency; no ordinary FIR or AST survives the pass.
                if !self.selected_signature_default_function(f) {
                    self.check_fun_body(scope, f);
                }
            } else if infer_ret {
                if let FunBody::Expr(e) = &f.body {
                    let inferred = self.expr(scope, *e);
                    self.finish_executable_root(*e);
                    if inferred != Ty::Error {
                        self.ret_ty = inferred;
                        if let Some(declaration) = source_decl {
                            self.record_checked_function_result(
                                declaration,
                                &f.name,
                                f.receiver.is_some(),
                                inferred,
                            );
                        }
                    }
                }
            } else if check_ordinary_body {
                self.check_fun_body(scope, f);
            }
        }
        if infer_ret {
            self.check_operator_declaration(
                f,
                self.ret_ty,
                operator_declarations::OperatorDeclarationSite::NonMember,
            );
        }
        self.report_inferred_nothing_return(f, self.ret_ty, false);
        if f.receiver.is_some() && companion_classifier.is_none() {
            self.extension_receiver_labels.pop();
            self.this_labels.pop();
        }
        self.this_extension_receiver = prev_extension_receiver;
        self.allow_lambda_mutation = prev_allow;
        self.leave_public_api_inline(public_api_inline);
        self.leave_block_body(block);
        self.lambda_returns.leave_function(enclosing_return_frame);
        self.retire_type_parameter_owners(&owned_type_parameters);
        self.active_lexical_policies.truncate(suppression_depth);
    }

    /// Check an instance method: the class properties are visible (implicit `this`), then the
    /// method's own parameters shadow them.
    /// Check one top-level function declaration, in a rung of its own.
    ///
    /// The rung is what retires the declaration's type parameters: declaring them into the
    /// CALLER's scope would leave them visible to every later declaration in the file.
    pub(super) fn check_top_level_fun(&mut self, scope: &CheckerScope<'_>, f: &FunDecl, d: DeclId) {
        let decl_scope = scope.child(ScopeKind::Function { receiver: None });
        let scope = &decl_scope;
        let stable_declaration = self
            .active_declarations
            .and_then(|active| active.file_declaration(self.file, d));
        let finalized_local_header =
            stable_declaration
                .zip(self.resolved_index)
                .is_some_and(|(declaration, index)| {
                    index.declaration_header(declaration).is_some_and(|header| {
                        header.flags.has(crate::fir::DeclarationFlags::LOCAL_CLASS)
                    }) && !self.has_deferred_local_header(Some(declaration))
                });
        let visible_type_parameters = scope.visible_tparams();
        let type_parameters = if finalized_local_header {
            stable_declaration
                .and_then(|declaration| {
                    self.finalized_type_parameters(
                        &visible_type_parameters,
                        declaration,
                        &f.type_params,
                    )
                })
                .unwrap_or_else(|| {
                    if let Some(declaration) = stable_declaration {
                        self.failed_local_declaration_bindings.insert(declaration);
                    }
                    visible_type_parameters.clone()
                })
        } else if self.has_deferred_local_header(stable_declaration) {
            self.bind_deferred_type_parameters(
                scope,
                &f.type_params,
                &f.type_param_bounds,
                f.signature_span.lo,
                true,
            )
        } else {
            TParams::symbolic_from_decl_with(&f.type_params, &f.type_param_bounds, &|name| {
                self.select_classifier(scope, name).found()
            })
            .alpha_renamed_declaration(
                &f.type_params,
                self.compilation_id,
                self.file_index,
                f.signature_span.lo,
            )
        };
        self.resolved_declaration_type_parameters.insert(
            f.signature_span.lo,
            f.type_params
                .iter()
                .map(|name| {
                    type_parameters
                        .bound(name)
                        .ty_param_name()
                        .unwrap_or(name)
                        .to_string()
                })
                .collect(),
        );
        crate::trace_compiler!(
            "resolve",
            "top-level function {} type parameters={:?}",
            f.name,
            f.type_params
                .iter()
                .map(|name| (name, type_parameters.bound(name)))
                .collect::<Vec<_>>()
        );
        scope.declare_tparams(&f.type_params, &type_parameters, |name| {
            f.reified_type_params.contains(name)
        });
        self.check_fun(scope, f, Some(d));
    }

    /// Check one top-level property declaration in `scope`.
    ///
    /// Mirrors [`Self::check_class`]: the file walk delegates rather than inlining, so every
    /// declaration kind is reachable from any scope.
    pub(super) fn should_check_selected_fun_body(&self, body: &FunBody) -> bool {
        self.selected_body_declarations
            .as_ref()
            .is_none_or(|selected| {
                fun_body_expr(body)
                    .and_then(|root| self.file.expr_span(root))
                    .is_some_and(|span| selected.contains(&span))
            })
    }

    fn selected_signature_default_function(&self, function: &FunDecl) -> bool {
        self.signature_defaults_only
            && self
                .selected_stable_body_declarations
                .as_ref()
                .is_some_and(|selected| {
                    let Some(active) = self.active_declarations else {
                        return false;
                    };
                    selected.iter().any(|declaration| {
                        active
                            .function(self.file, *declaration)
                            .is_some_and(|candidate| std::ptr::eq(candidate, function))
                    })
                })
    }

    pub(super) fn selected_signature_default_source_member(
        &self,
        member: crate::libraries::SourceMember,
    ) -> bool {
        self.signature_defaults_only
            && self
                .active_declarations
                .zip(self.resolved_index)
                .and_then(|(active, index)| {
                    active.source_member_declaration(self.file, index, member)
                })
                .is_some_and(|declaration| {
                    self.selected_stable_body_declarations
                        .as_ref()
                        .is_some_and(|selected| selected.contains(&declaration))
                })
    }

    pub(super) fn selected_signature_default_constructor(
        &self,
        class: DeclId,
        secondary: Option<usize>,
    ) -> bool {
        self.signature_defaults_only
            && self
                .selected_stable_body_declarations
                .as_ref()
                .is_some_and(|selected| {
                    let Some(active) = self.active_declarations else {
                        return false;
                    };
                    let Some(classifier) = active.classifier_declaration(class) else {
                        return false;
                    };
                    selected.iter().any(|declaration| {
                        let Some(anchor) = self
                            .resolved_index
                            .and_then(|index| index.declaration_anchor(*declaration))
                        else {
                            return false;
                        };
                        anchor.kind == crate::fir::DeclarationKind::Constructor
                            && anchor.owner.is_some_and(|owner| {
                                owner == classifier
                                    || active.same_parser_declaration(owner, classifier)
                            })
                            && anchor.sibling
                                == secondary.map_or(0, |index| {
                                    u32::try_from(index + 1)
                                        .expect("too many secondary constructors")
                                })
                    })
                })
    }
}
