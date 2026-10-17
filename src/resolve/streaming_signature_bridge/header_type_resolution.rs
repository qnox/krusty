//! Authoritative resolution of written compact declaration-header types.
//!
//! Header syntax can retain an expanded template for metadata publication, but source binding is
//! performed exactly once from the written tree. The selected alias already owns its stable,
//! declaration-site semantic expansion; resolving the parser-expanded tree again would be a second
//! lookup in a potentially different scope.

use super::super::TParams;
use super::*;

impl ProductionSignatureSemantics<'_> {
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
        node: crate::fir::LocalTypeTemplateNodeId,
        alias_formals: &[String],
    ) -> Option<Ty> {
        let node = graph.local_type_template_node(node)?;
        let children = node
            .children
            .iter()
            .map(|child| self.resolve_local_template_node(graph, *child, alias_formals))
            .collect::<Option<Vec<_>>>()?;
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
                for (formal, child) in source_formals.iter().zip(children.iter().copied()) {
                    // Keep the template placeholder as the identity seen by type resolution.
                    // Binding it directly to `child` lets a nested parameter collapse to its
                    // `Nothing` bound before the declaration-owned substitution below can run.
                    parameters.insert_binding(formal, Ty::ty_param(formal, child), Vec::new());
                }
                let symbolic = self
                    .with_signature_type_scope(scope, |lexical| {
                        lexical.declare_tparams(&source_formals, &parameters, |_| false);
                        self.signature_type_ref_at_with_site(scope, lexical, &target, false, None)
                    })
                    .ok()
                    .flatten()?;
                let bindings = source_formals
                    .into_iter()
                    .zip(children)
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
                if children.len() != own_arity || symbolic.type_args().len() < own_arity {
                    return None;
                }
                let arguments = children
                    .into_iter()
                    .chain(symbolic.type_args()[own_arity..].iter().copied())
                    .collect::<Vec<_>>();
                Ty::obj_args_name(symbolic.obj_internal()?, &arguments)
            }
            crate::fir::LocalTypeTemplateTarget::Alias(alias) => {
                let symbolic = self.bind_local_alias_template(graph, *alias)?;
                let definition = graph.local_type_alias_template(*alias)?;
                let bindings = definition
                    .formals
                    .iter()
                    .map(|formal| graph.name(*formal).map(str::to_owned))
                    .collect::<Option<Vec<_>>>()?
                    .into_iter()
                    .zip(children)
                    .collect::<crate::symbol_resolver::GSigBinds>();
                crate::types::ty_subst_alias_expansion(symbolic, &bindings)
            }
        };
        Some(Self::apply_local_template_flags(resolved, node.flags))
    }

    fn bind_local_alias_template(
        &self,
        graph: &crate::fir::SignatureGraph,
        alias: crate::fir::LocalTypeAliasTemplateId,
    ) -> Option<Ty> {
        match self.bound_local_alias_templates.begin(alias) {
            super::bound_local_alias_templates::BindRequest::Bind => {}
            super::bound_local_alias_templates::BindRequest::Complete(result) => {
                return result.ok();
            }
        }
        let result = (|| {
            let definition = graph.local_type_alias_template(alias)?;
            let formals = definition
                .formals
                .iter()
                .map(|formal| graph.name(*formal).map(str::to_owned))
                .collect::<Option<Vec<_>>>()?;
            self.resolve_local_template_node(graph, definition.root, &formals)
        })()
        .ok_or(());
        self.bound_local_alias_templates.finish(alias, result);
        result.ok()
    }

    pub(super) fn bind_local_alias_templates(&self, graph: &crate::fir::SignatureGraph) {
        for alias in graph.local_type_alias_template_ids() {
            let _ = self.bind_local_alias_template(graph, alias);
        }
    }

    /// Resolve one graph-owned written application after selecting its declaration-site target.
    /// Argument evaluation stays lazy so package-owned argument subtrees are never treated as
    /// classifier arguments. Both explicit headers and the ordinary signature evaluator enter
    /// through this operation.
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
        let declaration_syntax = graph.declaration_expression_syntax(expression);
        let selected_site = SelectedTypeSite {
            syntax: declaration_syntax.map(|syntax| self.headers.syntax.written_type_id(syntax)),
            application: Some(expression),
            abbreviation_origin: None,
            project_children: false,
        };
        let resolved = if let Some(alias) = application.local_alias {
            let arguments = graph
                .operands(application.arguments)
                .iter()
                .map(|argument| evaluate_argument(*argument).map(crate::fir::ResolvedTy::get))
                .collect::<Result<Vec<_>, _>>()?;
            let template = graph
                .local_type_alias_template(alias)
                .ok_or_else(Self::failure)?;
            let bindings = template
                .formals
                .iter()
                .map(|formal| {
                    graph
                        .name(*formal)
                        .map(str::to_owned)
                        .ok_or_else(Self::failure)
                })
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .zip(arguments)
                .collect::<crate::symbol_resolver::GSigBinds>();
            let symbolic = self
                .bound_local_alias_templates
                .bound(alias)
                .ok_or_else(Self::failure)?
                .map_err(|()| Self::failure())?;
            let resolved = crate::types::ty_subst_alias_expansion(symbolic, &bindings);
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
                syntax: selected_site.syntax,
                application: Some(expression),
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
            let mut bindings = crate::symbol_resolver::GSigBinds::default();
            for ((formal, argument), index) in formals
                .iter()
                .zip(arguments)
                .zip(application.written_argument_indices.iter().copied())
            {
                if !written.is_function_type_syntax()
                    && index < written.targs.len() as u32
                    && index >= kept_classifier_arguments as u32
                {
                    continue;
                }
                let resolved = evaluate_argument(*argument)?.get();
                bindings.insert(formal.clone(), resolved);
            }
            if let Some(mut abbreviation) = self.selected_types.abbreviation(origin) {
                abbreviation.arguments = abbreviation
                    .arguments
                    .iter()
                    .map(|argument| crate::types::ty_subst_alias_expansion(*argument, &bindings))
                    .collect::<Vec<_>>()
                    .into_boxed_slice();
                self.selected_types.record(origin, abbreviation);
            }
            crate::types::ty_subst_alias_expansion(symbolic, &bindings)
        };
        self.record_type_spelling_projection(Some(selected_site), resolved);
        crate::fir::ResolvedTy::new(resolved).map_err(|_| Self::failure())
    }

    /// Resolve the structural compact type subset used by explicit body-local headers.
    pub(super) fn resolve_compact_graph_type(
        &self,
        graph: &crate::fir::SignatureGraph,
        expression: crate::fir::SigExprId,
    ) -> Option<Ty> {
        if graph.written_type_application(expression).is_some() {
            return self
                .evaluate_written_type_application(graph, expression, &mut |argument| {
                    let ty = self
                        .resolve_compact_graph_type(graph, argument)
                        .ok_or_else(Self::failure)?;
                    crate::fir::ResolvedTy::new(ty).map_err(|_| Self::failure())
                })
                .ok()
                .map(crate::fir::ResolvedTy::get);
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
        if let Some(syntax) = graph.declaration_expression_syntax(expression) {
            let syntax = self.headers.syntax.written_type_id(syntax);
            let mut spelling = self.selected_types.spelling(syntax).unwrap_or_default();
            spelling.resolved = Some(resolved);
            self.selected_types.record_spelling(syntax, spelling);
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
                        application: None,
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
                application: None,
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
                application: None,
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
            application: None,
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
