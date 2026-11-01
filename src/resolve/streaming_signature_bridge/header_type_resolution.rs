//! Authoritative resolution of written compact declaration-header types.
//!
//! Header syntax can retain an expanded template for metadata publication, but source binding is
//! performed exactly once from the written tree. The selected alias already owns its stable,
//! declaration-site semantic expansion; resolving the parser-expanded tree again would be a second
//! lookup in a potentially different scope.

use super::super::TParams;
use super::*;

impl ProductionSignatureSemantics<'_> {
    pub(super) fn project_graph_type_spelling_sites(
        &self,
        graph: &crate::fir::SignatureGraph,
        syntax: crate::fir::HeaderTypeId,
        resolved: Ty,
    ) -> bool {
        let sites = graph.declaration_type_syntaxes(syntax);
        if sites.is_empty() {
            return true;
        }
        let sites = sites
            .iter()
            .map(|site| self.headers.syntax.written_type_id(*site))
            .collect::<Vec<_>>();
        self.selected_types.project_graph_type_to_declaration_sites(
            graph.written_type_id(syntax),
            resolved,
            &sites,
        )
    }

    fn expression_spelling(
        &self,
        graph: &crate::fir::SignatureGraph,
        expression: crate::fir::SigExprId,
    ) -> Option<super::super::type_spellings::ResolvedTypeSpelling> {
        if graph.written_type_application(expression).is_some() {
            return self.selected_types.application(expression);
        }
        match graph.expr(expression)? {
            crate::fir::SigExpr::Type { syntax, .. }
            | crate::fir::SigExpr::ContextualType { syntax, .. } => self
                .selected_types
                .graph_spelling(graph.written_type_id(syntax)),
            crate::fir::SigExpr::Nullable(inner) | crate::fir::SigExpr::NonNullable(inner) => {
                self.expression_spelling(graph, inner)
            }
            crate::fir::SigExpr::Known(_)
            | crate::fir::SigExpr::ClassifierType { .. }
            | crate::fir::SigExpr::DeclarationType(_)
            | crate::fir::SigExpr::Parameter { .. } => Some(Default::default()),
            _ => None,
        }
    }

    /// Project one already-evaluated graph occurrence independently to each exact stable header
    /// role that owns it. Only the semantic `Ty` is shared. Alias/classifier/package-boundary facts
    /// come from the graph occurrence selected during that evaluation; no header site is promoted
    /// to a canonical source for another site.
    fn project_declaration_expression_type(
        &self,
        graph: &crate::fir::SignatureGraph,
        expression: crate::fir::SigExprId,
        resolved: Ty,
    ) -> bool {
        let sites = graph.declaration_expression_syntaxes(expression);
        if sites.is_empty() {
            return true;
        }
        let sites = sites
            .iter()
            .map(|syntax| self.headers.syntax.written_type_id(*syntax))
            .collect::<Vec<_>>();
        if graph.written_type_application(expression).is_some() {
            return self
                .selected_types
                .project_application_to_declaration_sites(expression, resolved, &sites);
        }
        let Some(mut spelling) = self.expression_spelling(graph, expression) else {
            return false;
        };
        spelling.resolved = Some(resolved);
        for syntax in sites {
            self.selected_types.record_spelling(syntax, spelling);
        }
        true
    }

    fn apply_local_template_flags(mut ty: Ty, flags: crate::fir::LocalTypeTemplateFlags) -> Ty {
        if flags.in_projection {
            ty = Ty::in_projection(ty);
        }
        if flags.out_projection {
            ty = Ty::out_projection(ty);
        }
        if flags.definitely_non_null {
            ty = super::super::definitely_non_null_ty(ty);
        }
        if flags.nullable {
            ty = Ty::nullable(ty);
        }
        ty
    }

    fn resolve_local_template_node(
        &self,
        graph: &crate::fir::SignatureGraph,
        node_id: crate::fir::LocalTypeTemplateNodeId,
        alias_formals: &[String],
        source_file: crate::fir::SourceFileId,
    ) -> Option<(
        Ty,
        super::bound_local_alias_templates::LocalTypeSpellingProjection,
    )> {
        let node = graph.local_type_template_node(node_id)?.clone();
        let children = node
            .children
            .iter()
            .map(|child| {
                self.resolve_local_template_node(graph, *child, alias_formals, source_file)
            })
            .collect::<Option<Vec<_>>>()?;
        let child_types = children.iter().map(|(ty, _)| *ty).collect::<Vec<_>>();
        let resolved = match &node.target {
            crate::fir::LocalTypeTemplateTarget::Formal(ordinal) => {
                let formal = alias_formals.get(*ordinal as usize)?;
                Ty::ty_param(formal, Ty::Nothing)
            }
            crate::fir::LocalTypeTemplateTarget::Star => {
                Ty::star_projection(Ty::nullable(Ty::obj("kotlin/Any")))
            }
            crate::fir::LocalTypeTemplateTarget::Syntax {
                syntax,
                scope,
                child_formals,
            } => {
                let scope = graph.scope(*scope)?;
                let target = graph.transient_type_ref(*syntax)?;
                let source_formals = child_formals
                    .iter()
                    .map(|formal| graph.name(*formal).map(str::to_owned))
                    .collect::<Option<Vec<_>>>()?;
                let mut parameters = TParams::default();
                for (formal, child) in source_formals.iter().zip(child_types.iter().copied()) {
                    // Keep the template placeholder as the identity seen by type resolution.
                    // Binding it directly to `child` lets a nested parameter collapse to its
                    // `Nothing` bound before the declaration-owned substitution below can run.
                    parameters.insert_binding(formal, Ty::ty_param(formal, child), Vec::new());
                }
                let symbolic = self
                    .with_signature_type_scope(scope, |lexical| {
                        lexical.declare_tparams(&source_formals, &parameters, |_| false);
                        self.signature_type_ref_at_with_site(
                            scope,
                            lexical,
                            &target,
                            false,
                            Some(SelectedTypeSite {
                                syntax: None,
                                graph_syntax: None,
                                application: None,
                                local_template: Some(node_id),
                                abbreviation_origin: None,
                                project_children: false,
                            }),
                        )
                    })
                    .ok()
                    .flatten()?;
                let bindings = source_formals
                    .into_iter()
                    .map(crate::types::TyParamName::from)
                    .zip(child_types.iter().copied())
                    .collect::<crate::symbol_resolver::GSigBinds>();
                crate::types::ty_subst_alias_expansion(symbolic, &bindings)
            }
            crate::fir::LocalTypeTemplateTarget::Classifier { declaration, scope } => {
                let scope = graph.scope(*scope)?;
                let symbolic = <Self as crate::fir::SignatureSemantics>::classifier_type(
                    self,
                    *declaration,
                    scope,
                )
                .ok()?
                .get();
                let own_arity = self
                    .classifier_signature(*declaration)?
                    .type_parameters
                    .type_params
                    .len();
                if child_types.len() != own_arity || symbolic.type_args().len() < own_arity {
                    return None;
                }
                let arguments = child_types
                    .iter()
                    .copied()
                    .chain(symbolic.type_args()[own_arity..].iter().copied())
                    .collect::<Vec<_>>();
                Ty::obj_args_name(symbolic.obj_internal()?, &arguments)
            }
            crate::fir::LocalTypeTemplateTarget::Alias(alias) => {
                let bound = self.bind_local_alias_template(graph, *alias)?;
                let definition = graph.local_type_alias_template(*alias)?;
                let bindings = definition
                    .formals
                    .iter()
                    .map(|formal| graph.name(*formal).map(crate::types::TyParamName::from))
                    .collect::<Option<Vec<_>>>()?
                    .into_iter()
                    .zip(child_types.iter().copied())
                    .collect::<crate::symbol_resolver::GSigBinds>();
                crate::types::ty_subst_alias_expansion(bound.semantic, &bindings)
            }
        };
        let resolved = Self::apply_local_template_flags(resolved, node.flags);
        let source = graph.transient_type_ref(node.source)?;
        let mut binding = self
            .selected_types
            .local_template_node(node_id)
            .unwrap_or_default();
        binding.resolved = Some(resolved);
        self.selected_types
            .record_local_template_node(node_id, binding);
        let occurrence = super::bound_local_alias_templates::LocalTypeSpellingOccurrence {
            source,
            source_file,
            binding,
        };
        let child_spellings = children
            .into_iter()
            .map(|(_, spelling)| spelling)
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let spelling = match &node.target {
            crate::fir::LocalTypeTemplateTarget::Formal(ordinal) => {
                super::bound_local_alias_templates::LocalTypeSpellingProjection::Formal {
                    ordinal: *ordinal,
                    occurrence,
                }
            }
            crate::fir::LocalTypeTemplateTarget::Alias(alias) => {
                let bound = self.bind_local_alias_template(graph, *alias)?;
                let definition = graph.local_type_alias_template(*alias)?;
                let bindings = definition
                    .formals
                    .iter()
                    .map(|formal| graph.name(*formal).map(crate::types::TyParamName::from))
                    .collect::<Option<Vec<_>>>()?
                    .into_iter()
                    .zip(child_types.iter().copied())
                    .collect::<crate::symbol_resolver::GSigBinds>();
                let inner = bound.spelling.substitute(&child_spellings, &bindings)?;
                super::bound_local_alias_templates::LocalTypeSpellingProjection::Overlay {
                    occurrence,
                    inner: Box::new(inner),
                }
            }
            crate::fir::LocalTypeTemplateTarget::Star
            | crate::fir::LocalTypeTemplateTarget::Syntax { .. }
            | crate::fir::LocalTypeTemplateTarget::Classifier { .. } => {
                super::bound_local_alias_templates::LocalTypeSpellingProjection::Structural {
                    occurrence,
                    children: child_spellings,
                }
            }
        };
        Some((resolved, spelling))
    }

    fn bind_local_alias_template(
        &self,
        graph: &crate::fir::SignatureGraph,
        alias: crate::fir::LocalTypeAliasTemplateId,
    ) -> Option<super::bound_local_alias_templates::BoundLocalAliasTemplate> {
        match self.bound_local_alias_templates.begin(alias) {
            super::bound_local_alias_templates::BindRequest::Bind => {}
            super::bound_local_alias_templates::BindRequest::Complete(result) => {
                return result.ok();
            }
        }
        let result = (|| {
            let definition = graph.local_type_alias_template(alias)?;
            let source_file = graph.scope(definition.scope)?.source;
            let formals = definition
                .formals
                .iter()
                .map(|formal| graph.name(*formal).map(str::to_owned))
                .collect::<Option<Vec<_>>>()?;
            let (semantic, spelling) =
                self.resolve_local_template_node(graph, definition.root, &formals, source_file)?;
            Some(super::bound_local_alias_templates::BoundLocalAliasTemplate { semantic, spelling })
        })()
        .ok_or(());
        self.bound_local_alias_templates
            .finish(alias, result.clone());
        result.ok()
    }

    pub(super) fn bind_local_alias_templates(&self, graph: &crate::fir::SignatureGraph) {
        for alias in graph.local_type_alias_template_ids() {
            let _ = self.bind_local_alias_template(graph, alias);
        }
    }

    /// Resolve one graph-owned written application after selecting its declaration-site target.
    /// Package-owned argument subtrees are evaluated for their own diagnostics in source order,
    /// but their results never enter classifier application. Both explicit headers and the
    /// ordinary signature evaluator enter through this operation.
    pub(super) fn evaluate_written_type_application(
        &self,
        graph: &crate::fir::SignatureGraph,
        expression: crate::fir::SigExprId,
        evaluate_argument: &mut dyn FnMut(
            crate::fir::SigExprId,
        ) -> Result<
            crate::fir::ResolvedTy,
            crate::fir::DiagnosticId,
        >,
    ) -> Result<crate::fir::ResolvedTy, crate::fir::DiagnosticId> {
        let application = graph
            .written_type_application(expression)
            .ok_or_else(Self::failure)?;
        let origin = match graph.expr(expression) {
            Some(crate::fir::SigExpr::Type { origin, .. }) => origin,
            _ => return Err(Self::failure()),
        };
        let selected_site = SelectedTypeSite {
            syntax: None,
            graph_syntax: None,
            application: Some(expression),
            local_template: None,
            abbreviation_origin: None,
            project_children: false,
        };
        let resolved = if let Some(alias) = application.local_alias {
            let arguments = graph
                .operands(application.arguments)
                .iter()
                .map(|argument| {
                    let resolved = evaluate_argument(*argument)?;
                    if !self.project_declaration_expression_type(graph, *argument, resolved.get()) {
                        return Err(Self::failure());
                    }
                    Ok(resolved.get())
                })
                .collect::<Result<Vec<_>, _>>()?;
            let template = graph
                .local_type_alias_template(alias)
                .ok_or_else(Self::failure)?;
            let formals = template
                .formals
                .iter()
                .map(|formal| {
                    graph
                        .name(*formal)
                        .map(str::to_owned)
                        .ok_or_else(Self::failure)
                })
                .collect::<Result<Vec<_>, _>>()?;
            let bindings = formals
                .iter()
                .map(crate::types::TyParamName::from)
                .zip(arguments.iter().copied())
                .collect::<crate::symbol_resolver::GSigBinds>();
            let bound = self
                .bound_local_alias_templates
                .bound(alias)
                .ok_or_else(Self::failure)?
                .map_err(|()| Self::failure())?;
            for &syntax in graph.declaration_expression_syntaxes(expression) {
                self.selected_types.record_local_alias_projection(
                    self.headers.syntax.written_type_id(syntax),
                    super::bound_local_alias_templates::AppliedLocalTypeSpellingProjection {
                        root: bound.spelling.clone(),
                        formals: formals.clone().into_boxed_slice(),
                        arguments: arguments.clone().into_boxed_slice(),
                    },
                );
            }
            let resolved = crate::types::ty_subst_alias_expansion(bound.semantic, &bindings);
            Self::apply_local_template_flags(
                resolved,
                application
                    .use_flags
                    .expect("a local alias application retains its use-site flags"),
            )
        } else {
            let scope = graph
                .scope(application.declaration_scope)
                .ok_or_else(Self::failure)?;
            let target = graph
                .transient_type_ref(application.target)
                .ok_or_else(Self::failure)?;
            let formals = application
                .formals
                .iter()
                .map(|formal| {
                    graph
                        .name(*formal)
                        .map(str::to_owned)
                        .ok_or_else(Self::failure)
                })
                .collect::<Result<Vec<_>, _>>()?;
            let mut parameters = TParams::default();
            for formal in &formals {
                // The target is bound before package-owned source arguments are identified. A
                // bottom-bound symbolic parameter preserves applicability for every declaration
                // bound; the written, non-package arguments replace it below and the ordinary
                // finalized-header bound checker validates those actual semantic types.
                parameters.insert_binding(formal, Ty::ty_param(formal, Ty::Nothing), Vec::new());
            }
            let root_site = application.bind_root.then_some(SelectedTypeSite {
                syntax: None,
                graph_syntax: None,
                application: Some(expression),
                local_template: None,
                abbreviation_origin: Some(origin),
                project_children: false,
            });
            let symbolic = self
                .with_signature_type_scope(scope, |lexical| {
                    lexical.declare_tparams(&formals, &parameters, |_| false);
                    self.signature_type_ref_at_with_site(scope, lexical, &target, true, root_site)
                })
                .ok()
                .flatten()
                .ok_or_else(Self::failure)?;
            let written = graph
                .transient_written_type_ref(application.target)
                .ok_or_else(Self::failure)?;
            let kept_classifier_arguments = self
                .selected_types
                .application(expression)
                .and_then(|spelling| spelling.classifier_segment)
                .and_then(|segment| {
                    super::super::package_qualifier_type_arguments::without_package_arguments(
                        &written, segment,
                    )
                })
                .map(|reference| reference.targs.len())
                .unwrap_or(written.targs.len());
            let arguments = graph.operands(application.arguments);
            let argument_by_written_index = application
                .written_argument_indices
                .iter()
                .copied()
                .zip(arguments.iter().copied())
                .collect::<crate::fx_hash::HashMap<_, _>>();
            let written_argument_order = if written.is_function_type_syntax() {
                application.written_argument_indices.to_vec()
            } else {
                let mut order = super::super::package_qualifier_type_arguments::type_argument_indices_source_order(
                    &written,
                );
                order.extend(
                    application
                        .written_argument_indices
                        .iter()
                        .copied()
                        .filter(|index| *index >= written.targs.len() as u32),
                );
                order
            };
            let mut resolved_arguments = crate::fx_hash::HashMap::default();
            let mut argument_failure = None;
            for index in written_argument_order {
                let Some(argument) = argument_by_written_index.get(&index).copied() else {
                    continue;
                };
                match evaluate_argument(argument) {
                    Ok(resolved) => {
                        if !self.project_declaration_expression_type(
                            graph,
                            argument,
                            resolved.get(),
                        ) {
                            return Err(Self::failure());
                        }
                        resolved_arguments.insert(index, resolved);
                    }
                    Err(diagnostic) => {
                        argument_failure.get_or_insert(diagnostic);
                    }
                }
            }
            if let Some(diagnostic) = argument_failure {
                return Err(diagnostic);
            }
            let mut bindings = crate::symbol_resolver::GSigBinds::default();
            for (formal, index) in formals
                .iter()
                .zip(application.written_argument_indices.iter().copied())
            {
                if !written.is_function_type_syntax()
                    && index < written.targs.len() as u32
                    && index >= kept_classifier_arguments as u32
                {
                    continue;
                }
                let resolved = resolved_arguments
                    .get(&index)
                    .copied()
                    .ok_or_else(Self::failure)?
                    .get();
                bindings.insert(crate::types::TyParamName::new(formal), resolved);
            }
            if let Some(mut abbreviation) = self.selected_types.abbreviation(origin) {
                abbreviation.arguments = abbreviation
                    .arguments
                    .iter()
                    .map(|argument| crate::types::ty_subst_applied_arguments(*argument, &bindings))
                    .collect::<Vec<_>>()
                    .into_boxed_slice();
                self.selected_types.record(origin, abbreviation);
            }
            // These are artificial structural placeholders, not declaration formals. Their
            // `Nothing` seed only keeps the template symbolic while arguments are evaluated; the
            // already-resolved written arguments must be applied exactly, including nullability.
            crate::types::ty_subst_applied_arguments(symbolic, &bindings)
        };
        self.record_type_spelling_projection(Some(selected_site), resolved);
        if !self.project_declaration_expression_type(graph, expression, resolved) {
            return Err(Self::failure());
        }
        crate::fir::ResolvedTy::new(resolved).map_err(|_| Self::failure())
    }

    /// Resolve the structural compact type subset used by explicit body-local headers.
    pub(super) fn resolve_compact_graph_type(
        &self,
        graph: &crate::fir::SignatureGraph,
        expression: crate::fir::SigExprId,
    ) -> Option<Ty> {
        if graph.written_type_application(expression).is_some() {
            let resolved = self
                .evaluate_written_type_application(graph, expression, &mut |argument| {
                    let ty = self
                        .resolve_compact_graph_type(graph, argument)
                        .ok_or_else(Self::failure)?;
                    crate::fir::ResolvedTy::new(ty).map_err(|_| Self::failure())
                })
                .ok()
                .map(crate::fir::ResolvedTy::get)?;
            if !self.project_declaration_expression_type(graph, expression, resolved) {
                return None;
            }
            return Some(resolved);
        }
        let resolved = match graph.expr(expression)? {
            crate::fir::SigExpr::Known(ty) => ty.get(),
            crate::fir::SigExpr::ClassifierType { declaration, scope } => {
                let scope = graph.scope(scope)?;
                <Self as crate::fir::SignatureSemantics>::classifier_type(self, declaration, scope)
                    .ok()?
                    .get()
            }
            crate::fir::SigExpr::Type {
                syntax,
                scope,
                origin,
            } => {
                let scope = graph.scope(scope)?;
                <Self as crate::fir::SignatureSemantics>::resolve_type(
                    self, scope, origin, syntax, graph,
                )
                .ok()?
                .get()
            }
            crate::fir::SigExpr::Nullable(base) => {
                Ty::nullable(self.resolve_compact_graph_type(graph, base)?)
            }
            crate::fir::SigExpr::NonNullable(base) => {
                super::super::definitely_non_null_ty(self.resolve_compact_graph_type(graph, base)?)
            }
            _ => return None,
        };
        if resolved.mentions_error() || resolved.mentions_pending() {
            return None;
        }
        if !self.project_declaration_expression_type(graph, expression, resolved) {
            return None;
        }
        Some(resolved)
    }

    pub(super) fn resolve_compact_header_type(
        &self,
        scope: crate::fir::SignatureScope,
        syntax: crate::fir::HeaderTypeId,
    ) -> Option<Ty> {
        let written_syntax = self.headers.syntax.written_type_id(syntax);
        let written = self
            .headers
            .syntax
            .transient_written_type_ref(syntax, &self.headers.lookup_names)?;
        let resolved = self
            .with_signature_type_scope(scope, |lexical| {
                self.signature_type_ref_at_with_site(
                    scope,
                    lexical,
                    &written,
                    false,
                    Some(SelectedTypeSite {
                        syntax: Some(written_syntax),
                        graph_syntax: None,
                        application: None,
                        local_template: None,
                        abbreviation_origin: None,
                        project_children: true,
                    }),
                )
            })
            .ok()
            .flatten()?;
        (!resolved.mentions_error()).then_some(resolved)
    }

    pub(super) fn resolve_explicit_header_type(
        &self,
        scope: crate::fir::SignatureScope,
        syntax: crate::fir::HeaderTypeId,
    ) -> Result<Ty, crate::fir::DiagnosticId> {
        let written_syntax = self.headers.syntax.written_type_id(syntax);
        let written = self
            .headers
            .syntax
            .transient_written_type_ref(syntax, &self.headers.lookup_names)
            .ok_or_else(Self::failure)?;
        self.resolve_signature_type_reference_at(
            scope,
            scope,
            &written,
            true,
            Some(SelectedTypeSite {
                syntax: Some(written_syntax),
                graph_syntax: None,
                application: None,
                local_template: None,
                abbreviation_origin: None,
                project_children: true,
            }),
        )
    }

    pub(super) fn resolve_callable_header_bound(
        &self,
        scope: crate::fir::SignatureScope,
        syntax: crate::fir::HeaderTypeId,
        reference: &TypeRef,
    ) -> Result<Ty, crate::fir::DiagnosticId> {
        self.resolve_signature_type_reference_at(
            scope,
            scope,
            reference,
            false,
            Some(SelectedTypeSite {
                syntax: Some(self.headers.syntax.written_type_id(syntax)),
                graph_syntax: None,
                application: None,
                local_template: None,
                abbreviation_origin: None,
                project_children: true,
            }),
        )
    }

    pub(super) fn resolve_classifier_header_bound(
        &self,
        scope: crate::fir::SignatureScope,
        lexical: &super::super::CheckerScope<'_>,
        syntax: crate::fir::HeaderTypeId,
        reference: &TypeRef,
    ) -> Result<Ty, crate::fir::DiagnosticId> {
        let site = SelectedTypeSite {
            syntax: Some(self.headers.syntax.written_type_id(syntax)),
            graph_syntax: None,
            application: None,
            local_template: None,
            abbreviation_origin: None,
            project_children: true,
        };
        if let Some(resolved) =
            self.signature_type_ref_at_with_site(scope, lexical, reference, false, Some(site))
        {
            if !resolved.mentions_error() {
                return Ok(resolved);
            }
            if let Some(diagnostic) =
                self.recorded_type_diagnostic(scope.owner, scope.source, reference)
            {
                return Err(diagnostic);
            }
        }
        let failed = self.unresolved_signature_type_ref(scope, lexical, reference, false);
        Err(self.record_failed_type_reference(scope, scope, failed.span, &failed.name))
    }
}
