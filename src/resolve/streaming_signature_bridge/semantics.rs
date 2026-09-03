//! Resolver-backed evaluation of compact signature expressions.

use super::*;

/// Remove solver-local projection captures from an inferred declaration result. A capture is
/// readable through its upper bound at the result root; inside a generic argument it is exposed as
/// a star projection so the published signature does not claim an invariant type that callers
/// could never name.
fn denotable_signature_result(
    ty: Ty,
    denotable_parameters: &std::collections::HashSet<String>,
) -> Ty {
    fn result(
        ty: Ty,
        denotable: &std::collections::HashSet<String>,
        visiting: &mut std::collections::HashSet<&'static str>,
    ) -> Ty {
        match ty {
            Ty::TyParam(name, bound) if !denotable.contains(name) => {
                if !visiting.insert(name) {
                    return Ty::nullable(Ty::obj("kotlin/Any"));
                }
                let approximated = result(bound.projection_read_ty(), denotable, visiting);
                visiting.remove(name);
                approximated
            }
            Ty::TyParam(..) => ty,
            Ty::Obj(owner, arguments) if !arguments.is_empty() => Ty::obj_args_name(
                owner,
                &arguments
                    .iter()
                    .map(|argument| match *argument {
                        Ty::TyParam(name, bound) if !denotable.contains(name) => {
                            Ty::star_projection(result(
                                bound.projection_read_ty(),
                                denotable,
                                visiting,
                            ))
                        }
                        Ty::InProjection(inner) => {
                            Ty::in_projection(result(*inner, denotable, visiting))
                        }
                        Ty::OutProjection(inner) => {
                            Ty::out_projection(result(*inner, denotable, visiting))
                        }
                        Ty::StarProjection(inner) => {
                            Ty::star_projection(result(*inner, denotable, visiting))
                        }
                        argument => result(argument, denotable, visiting),
                    })
                    .collect::<Vec<_>>(),
            ),
            Ty::Fun(signature) => Ty::fun_with_shape(
                signature
                    .params
                    .iter()
                    .map(|parameter| result(*parameter, denotable, visiting))
                    .collect(),
                result(signature.ret, denotable, visiting),
                signature.context_count,
                signature.has_receiver,
                signature.suspend,
            ),
            Ty::Nullable(inner) => Ty::nullable(result(*inner, denotable, visiting)),
            Ty::PlatformNullable(inner) => {
                Ty::platform_nullable(result(*inner, denotable, visiting))
            }
            Ty::InProjection(inner) => Ty::in_projection(result(*inner, denotable, visiting)),
            Ty::OutProjection(inner) => Ty::out_projection(result(*inner, denotable, visiting)),
            Ty::StarProjection(inner) => Ty::star_projection(result(*inner, denotable, visiting)),
            _ => ty,
        }
    }

    result(
        ty,
        denotable_parameters,
        &mut std::collections::HashSet::new(),
    )
}

/// Select the language-defined SAM construction for a classifier call. Qualified and unqualified
/// classifier spellings share this path; the qualifier affects only how the classifier was found,
/// never whether its abstract method is applicable.
fn selected_sam_constructor_result(
    semantics: &ProductionSignatureSemantics<'_>,
    scope: crate::fir::SignatureScope,
    internal: crate::types::TypeName,
    actual: Ty,
    type_arguments: &[Ty],
) -> Option<Ty> {
    let module =
        crate::module_symbols::ModuleSymbols::for_file(semantics.table, scope.source.raw());
    let source = crate::symbol_source::CompositeSource::new(vec![
        &module as &dyn crate::symbol_source::SymbolSource,
        &*semantics.table.libraries as &dyn crate::symbol_source::SymbolSource,
    ]);
    let classifier = crate::symbol_source::SymbolSource::classifier(&source, internal)?;
    if !type_arguments.is_empty() && type_arguments.len() != classifier.type_params.len() {
        return None;
    }
    let target = if type_arguments.is_empty() {
        Ty::obj_name(internal)
    } else {
        Ty::obj_args_name(internal, type_arguments)
    };
    let signature = crate::symbol_resolver::semantic_sam_signature(&source, target)?;
    super::super::select_sam_constructor(&source, signature, actual).map(|selected| selected.result)
}

impl ProductionSignatureSemantics<'_> {
    /// Capture storage visible from a compact inferred member signature. Pass-1 capture discovery
    /// has already selected the lexical value and its semantic type; evaluating the dependency
    /// must consume that fact directly instead of pretending the generated field is a Kotlin
    /// property and reopening member lookup.
    fn enclosing_capture_type(
        &self,
        scope: crate::fir::SignatureScope,
        spelling: &str,
    ) -> Option<Ty> {
        let mut current = Some(scope.owner);
        while let Some(declaration) = current {
            let anchor = self.headers.declarations.anchor(declaration)?;
            if anchor.kind == crate::fir::DeclarationKind::Classifier {
                if let Some(capture) = self
                    .classifier_signature(declaration)
                    .and_then(|classifier| classifier.declared_props.get(spelling))
                    .filter(|property| !property.source_visible)
                {
                    return Some(capture.ty);
                }
            }
            current = anchor.owner;
        }
        None
    }

    fn record_scoped_argument_constraints(
        &self,
        scope: crate::fir::SignatureScope,
        parameters: &[Ty],
        arguments: &[Ty],
    ) {
        let constraint_frame = self.active_scoped_constraint_frame(scope.owner);
        crate::trace_compiler!(
            "signature",
            "scoped argument owner lookup scope={:?} active={constraint_frame:?} parameters={parameters:?} arguments={arguments:?}",
            scope.owner,
        );
        let Some((constraint_owner, constraint_index)) = constraint_frame else {
            return;
        };
        let active_formals = self
            .scoped_constraint_inputs
            .borrow()
            .get(&constraint_owner)
            .and_then(|stack| stack.get(constraint_index))
            .map(|inputs| {
                let mut formals = std::collections::HashSet::new();
                for input in inputs {
                    super::postponed_calls::collect_type_parameters(*input, &mut formals);
                }
                formals
            })
            .unwrap_or_default();
        if active_formals.is_empty() {
            return;
        }
        let mut all = self.scoped_constraints.borrow_mut();
        let Some(active) = all
            .get_mut(&constraint_owner)
            .and_then(|stack| stack.get_mut(constraint_index))
        else {
            return;
        };
        crate::trace_compiler!(
            "signature",
            "scoped argument constraints owner={:?} parameters={parameters:?} arguments={arguments:?} before={active:?}",
            constraint_owner,
        );
        for (&parameter, &argument) in parameters.iter().zip(arguments) {
            let mut inferred = crate::symbol_resolver::GSigBinds::new();
            crate::symbol_resolver::unify_inferred_ty(parameter, argument, &mut inferred);
            crate::symbol_resolver::unify_inferred_ty(argument, parameter, &mut inferred);
            inferred.retain(|formal, _| active_formals.contains(formal.as_str()));
            Self::merge_scoped_constraints(active, inferred);
        }
        crate::trace_compiler!(
            "signature",
            "scoped argument constraints owner={:?} after={active:?}",
            constraint_owner,
        );
    }

    fn record_scoped_member_constraints(
        &self,
        scope: crate::fir::SignatureScope,
        receiver: Ty,
        member: &crate::libraries::LibraryMember,
        parameters: &[Ty],
        arguments: &[Ty],
    ) {
        let specialized = parameters
            .iter()
            .map(|parameter| self.apply_dispatch_receiver(receiver, member, *parameter))
            .collect::<Vec<_>>();
        self.record_scoped_argument_constraints(scope, &specialized, arguments);
    }

    /// Declare a callable/property's own type parameters when it has no transitional symbol-table
    /// record. Enum-entry members are the first such family: their complete syntax is packed in the
    /// header arena, but they live on the entry subclass rather than in the parent enum's legacy
    /// method table. Stable alpha-renamed identities must match the later index publication path.
    fn declare_header_only_type_parameters(
        &self,
        lexical: &super::super::CheckerScope<'_>,
        declaration: crate::fir::DeclarationId,
    ) -> Result<(), crate::fir::DiagnosticId> {
        let header = self
            .headers
            .syntax
            .declaration(declaration)
            .ok_or_else(Self::failure)?;
        let anchor = self
            .headers
            .declarations
            .anchor(declaration)
            .ok_or_else(Self::failure)?;
        let (parameters, bounds, declaration_start) = match header.kind {
            crate::fir::HeaderDeclarationKind::Callable {
                type_parameters,
                bounds,
                signature_start,
                ..
            } => (type_parameters, Some(bounds), signature_start),
            crate::fir::HeaderDeclarationKind::Property {
                type_parameters,
                bounds,
                ..
            } => (type_parameters, Some(bounds), anchor.range.lo),
            crate::fir::HeaderDeclarationKind::Classifier {
                type_parameters,
                bounds,
                ..
            } => (type_parameters, Some(bounds), anchor.range.lo),
            crate::fir::HeaderDeclarationKind::TypeAlias {
                type_parameters, ..
            } => (type_parameters, None, anchor.range.lo),
            crate::fir::HeaderDeclarationKind::Constructor { .. } => return Ok(()),
        };
        let packed = self.headers.syntax.type_parameters(parameters);
        if packed.is_empty() {
            return Ok(());
        }
        let source_names = packed
            .iter()
            .map(|parameter| {
                self.headers
                    .lookup_names
                    .get(parameter.name)
                    .map(str::to_owned)
            })
            .collect::<Option<Vec<_>>>()
            .ok_or_else(Self::failure)?;
        let declared_bounds = bounds
            .map(|bounds| self.headers.syntax.bounds(bounds))
            .unwrap_or_default()
            .iter()
            .map(|bound| {
                Some((
                    self.headers.lookup_names.get(bound.parameter)?.to_owned(),
                    self.headers
                        .syntax
                        .transient_type_ref(bound.ty, &self.headers.lookup_names)?,
                ))
            })
            .collect::<Option<Vec<_>>>()
            .ok_or_else(Self::failure)?;
        let enclosing = lexical.visible_tparams();
        let semantic = super::super::TParams::symbolic_from_decl_enclosing(
            &source_names,
            &declared_bounds,
            &|name| self.table.class_names.get(name),
            &|name| enclosing.contains(name).then(|| enclosing.bound(name)),
        )
        .alpha_renamed_declaration(
            &source_names,
            self.table.compilation_id,
            anchor.source.raw(),
            declaration_start,
        );
        lexical.declare_tparams(&source_names, &semantic, |source_name| {
            packed.iter().any(|parameter| {
                parameter.flags.is_reified()
                    && self.headers.lookup_names.get(parameter.name) == Some(source_name)
            })
        });
        Ok(())
    }

    /// Reconstruct the semantic generic signature of a compact callable that has no transitional
    /// symbol-table member. The declaration's alpha-renamed type-parameter identities come from the
    /// same lexical header scope used to resolve its written parameter/result types.
    fn header_only_callable_generic_signature(
        &self,
        declaration: crate::fir::DeclarationId,
        signature: &crate::fir::ResolvedSignature,
    ) -> Result<crate::libraries::GenericSig, crate::fir::DiagnosticId> {
        let anchor = self
            .headers
            .declarations
            .anchor(declaration)
            .ok_or_else(Self::failure)?;
        let scope = crate::fir::SignatureScope {
            owner: declaration,
            source: anchor.source,
        };
        self.with_signature_type_scope(scope, |lexical| {
            let visible = lexical.visible_tparams();
            let mut formals = Vec::new();
            let mut formal_bounds = Vec::new();
            for parameter in self.header_type_parameters(declaration) {
                let source_name = self
                    .headers
                    .lookup_names
                    .get(parameter.name)
                    .ok_or_else(Self::failure)?;
                let semantic = visible.bound(source_name);
                let formal = semantic
                    .ty_param_name()
                    .ok_or_else(Self::failure)?
                    .to_owned();
                let mut bounds = vec![semantic
                    .ty_param_bound()
                    .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")))];
                bounds.extend(visible.extra_bounds_of(source_name));
                formals.push(formal);
                formal_bounds.push(bounds);
            }
            Ok(crate::libraries::GenericSig {
                formals,
                formal_bounds,
                receiver: None,
                params: signature
                    .parameters
                    .iter()
                    .map(|parameter| parameter.get())
                    .collect(),
                ret: signature.result.get(),
                return_policy: crate::libraries::GenericReturnPolicy::Exact,
            })
        })?
    }

    pub(super) fn with_signature_type_scope<T>(
        &self,
        scope: crate::fir::SignatureScope,
        resolve: impl FnOnce(&super::super::CheckerScope<'_>) -> T,
    ) -> Result<T, crate::fir::DiagnosticId> {
        let root = super::super::CheckerScope::root();
        let lexical = root.child(super::super::scope::ScopeKind::Function { receiver: None });
        let anchor = self
            .headers
            .declarations
            .anchor(scope.owner)
            .ok_or_else(Self::failure)?;
        let mut owner = (anchor.kind == crate::fir::DeclarationKind::Classifier)
            .then_some(scope.owner)
            .or(anchor.owner);
        let mut classifier_owners = Vec::new();
        while let Some(declaration) = owner {
            let Some(owner_anchor) = self.headers.declarations.anchor(declaration) else {
                break;
            };
            if owner_anchor.kind == crate::fir::DeclarationKind::Classifier {
                classifier_owners.push(declaration);
            }
            owner = owner_anchor.owner;
        }
        // Declare outermost first so a nearer classifier's source parameter shadows an enclosing
        // parameter with the same spelling while both retain distinct semantic identities.
        for declaration in classifier_owners.into_iter().rev() {
            if let Some(class) = self.classifier_signature(declaration) {
                // Synthetic anonymous/local classifiers may materialize captured formals that do
                // not occur in their compact source header. The semantic classifier signature is
                // authoritative for both own and captured parameters.
                let parameters = class
                    .type_parameters
                    .type_params()
                    .iter()
                    .zip(class.type_parameters.type_param_bounds())
                    .chain(
                        class
                            .captured_type_parameters
                            .type_params()
                            .iter()
                            .zip(class.captured_type_parameters.type_param_bounds()),
                    )
                    .collect::<Vec<_>>();
                let mut semantic = super::super::TParams::default();
                let exact_source_names = self
                    .header_type_parameters(declaration)
                    .iter()
                    .chain(self.header_classifier_captures(declaration))
                    .filter_map(|parameter| self.headers.lookup_names.get(parameter.name))
                    .collect::<Vec<_>>();
                let source_names = parameters
                    .iter()
                    .enumerate()
                    .map(|(ordinal, (name, bound))| {
                        let semantic_source = crate::types::type_parameter_source_name(name);
                        let source = exact_source_names
                            .iter()
                            .copied()
                            .find(|candidate| *candidate == semantic_source)
                            .or_else(|| exact_source_names.get(ordinal).copied())
                            .unwrap_or(semantic_source);
                        semantic.insert_binding(source, Ty::ty_param(name, **bound), Vec::new());
                        source.to_owned()
                    })
                    .collect::<Vec<_>>();
                lexical.declare_tparams(&source_names, &semantic, |_| false);
            }
        }
        if let Some(generic) = self
            .callable_signature(scope.owner)
            .and_then(|signature| signature.generic_sig.as_ref())
        {
            self.declare_semantic_type_parameters(
                &lexical,
                scope.owner,
                &generic.formals,
                &generic.formal_bounds,
            );
        } else if let Some(property) = self
            .table
            .source_props
            .values()
            .find(|property| property.stable_declaration == Some(scope.owner))
        {
            let bounds = property
                .formal_bounds
                .iter()
                .copied()
                .map(|bound| vec![bound])
                .collect::<Vec<_>>();
            self.declare_semantic_type_parameters(
                &lexical,
                scope.owner,
                &property.formals,
                &bounds,
            );
        } else if let Some(property) = self
            .table
            .ext_props
            .values()
            .flatten()
            .find(|property| property.stable_declaration == Some(scope.owner))
        {
            let bounds = property
                .formal_bounds
                .iter()
                .copied()
                .map(|bound| vec![bound])
                .collect::<Vec<_>>();
            self.declare_semantic_type_parameters(
                &lexical,
                scope.owner,
                &property.formals,
                &bounds,
            );
        } else if let Some(property) = self
            .table
            .classes
            .values()
            .flat_map(|class| class.member_ext_props.values().flatten())
            .find(|property| property.stable_declaration() == Some(scope.owner))
        {
            let bounds = property
                .type_param_bounds()
                .iter()
                .copied()
                .map(|bound| vec![bound])
                .collect::<Vec<_>>();
            self.declare_semantic_type_parameters(
                &lexical,
                scope.owner,
                property.type_params(),
                &bounds,
            );
        } else {
            self.declare_header_only_type_parameters(&lexical, scope.owner)?;
        }
        Ok(resolve(&lexical))
    }

    /// Resolve a compact declared header type after signature solving but before the compact header
    /// arena is destroyed. This is the same Pass-1 operation as [`Self::resolve_type`]; it exists so
    /// classifier supertypes are published from authoritative compact syntax instead of accepting
    /// provisional `Ty::Error` arguments from the transitional collector.
    pub(super) fn resolve_compact_header_type(
        &self,
        scope: crate::fir::SignatureScope,
        syntax: crate::fir::HeaderTypeId,
    ) -> Option<Ty> {
        let reference = self
            .headers
            .syntax
            .transient_type_ref(syntax, &self.headers.lookup_names)?;
        self.with_signature_type_scope(scope, |lexical| {
            self.classifier_header_type_ref(scope, lexical, &reference)
        })
        .ok()
        .flatten()
        .filter(|ty| !ty.mentions_error())
    }

    /// Resolve one non-local explicit declaration type directly from the compact header arena.
    /// Unlike `resolve_compact_header_type`, this enters the declaration's complete lexical class
    /// scope and preserves a precise lookup/alias diagnostic. It is the authoritative Pass-1 path
    /// for callable parameters/results and property types; the transitional symbol table is not a
    /// fallback merely because it happened to manufacture a publishable target-classifier shape.
    pub(super) fn resolve_explicit_header_type(
        &self,
        scope: crate::fir::SignatureScope,
        syntax: crate::fir::HeaderTypeId,
    ) -> Result<Ty, crate::fir::DiagnosticId> {
        let reference = self
            .headers
            .syntax
            .transient_type_ref(syntax, &self.headers.lookup_names)
            .ok_or_else(Self::failure)?;
        self.resolve_signature_type_reference(scope, &reference)
    }

    fn resolve_signature_type_reference(
        &self,
        scope: crate::fir::SignatureScope,
        reference: &TypeRef,
    ) -> Result<Ty, crate::fir::DiagnosticId> {
        let resolved = self.with_signature_type_scope(scope, |lexical| {
            self.signature_type_ref(scope, lexical, reference)
                .map(Ok)
                .unwrap_or_else(|| {
                    let failed = self.unresolved_signature_type_ref(scope, lexical, reference);
                    let spelling = self
                        .qualified_classifier_binding(scope, &failed.name)
                        .1
                        .unwrap_or_else(|| failed.name.clone());
                    Err(self.record_unresolved_reference_at(
                        scope.owner,
                        scope.source,
                        failed.span,
                        &spelling,
                    ))
                })
        })?;
        let ty = resolved?;
        if !ty.mentions_error() {
            return Ok(ty);
        }
        if let Some(diagnostic) =
            self.recorded_type_diagnostic(scope.owner, scope.source, reference.span)
        {
            return Err(diagnostic);
        }
        let failed = self.with_signature_type_scope(scope, |lexical| {
            self.unresolved_signature_type_ref(scope, lexical, reference)
                .clone()
        })?;
        let spelling = self
            .qualified_classifier_binding(scope, &failed.name)
            .1
            .unwrap_or_else(|| failed.name.clone());
        Err(self.record_unresolved_reference_at(scope.owner, scope.source, failed.span, &spelling))
    }

    /// Resolve the restricted compact type-expression subset used by explicit body-local headers.
    /// These nodes differ from inferred signature expressions: all semantic choices are type lookup
    /// and lexical alias expansion already fixed while the bounded Pass-1 AST was live.
    pub(super) fn resolve_compact_graph_type(
        &self,
        graph: &crate::fir::SignatureGraph,
        expression: crate::fir::SigExprId,
    ) -> Option<Ty> {
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
        (!resolved.mentions_error() && !resolved.mentions_pending()).then_some(resolved)
    }

    pub(super) fn merge_scoped_constraints(
        target: &mut crate::symbol_resolver::GSigBinds,
        source: crate::symbol_resolver::GSigBinds,
    ) {
        for (formal, actual) in source {
            target
                .entry(formal)
                .and_modify(|known| {
                    *known = crate::symbol_resolver::merge_inferred_ty(Some(*known), actual)
                })
                .or_insert(actual);
        }
    }

    /// Select a `companion fun C.name` declaration through the classifier coordinate written at
    /// the call site. A companion block does not create a singleton value, so this rung must be
    /// evaluated before qualified-receiver folding tries to turn `C` into an object/companion.
    fn select_associated_classifier_call(
        &self,
        scope: crate::fir::SignatureScope,
        classifier: crate::types::TypeName,
        spelling: &str,
        arguments: &[crate::fir::ResolvedSigCallArgument<'_>],
        type_arguments: &[crate::fir::ResolvedTy],
        trailing_lambda: bool,
        demand: &mut dyn FnMut(
            crate::fir::DeclarationId,
        )
            -> Result<crate::fir::ResolvedSignature, crate::fir::DiagnosticId>,
    ) -> Result<Option<crate::fir::ResolvedTy>, crate::fir::DiagnosticId> {
        let receiver = Ty::obj_name(classifier);
        let resolved_type_arguments = type_arguments
            .iter()
            .map(|argument| argument.get())
            .collect::<Vec<_>>();
        let selected = self.with_resolver(scope, |resolver| {
            let (mut functions, _) = resolver.receiver_callables(receiver, spelling).into_parts();
            functions
                .overloads
                .retain(|candidate| candidate.companion_extension);
            functions.overloads =
                self.implicit_context_candidates(scope, std::mem::take(&mut functions.overloads));
            if functions.overloads.is_empty() {
                return None;
            }
            let callables = crate::libraries::Callables::from_parts(
                functions,
                crate::libraries::PropertySet::default(),
            );
            let (argument_kinds, argument_types) =
                Self::mapped_call_arguments(callables.functions(), arguments, trailing_lambda)?;
            let projected = self.project_postponed_callables(scope, callables, &argument_kinds);
            let crate::symbol_resolver::CandidateSelection::Selected((selected, _, result)) =
                resolver.select_receiver_function_with_params_tracking(
                    receiver,
                    spelling,
                    &argument_kinds,
                    &resolved_type_arguments,
                    projected.callables(),
                )
            else {
                return None;
            };
            Some((
                result,
                selected.source_key,
                selected.stable_declaration,
                argument_types,
                projected.selected_bindings(&selected),
            ))
        });
        let Ok((result, source, declaration, argument_types, postponed_bindings)) = selected else {
            return Ok(None);
        };
        self.commit_postponed_bindings(scope, postponed_bindings);
        if let Some(source) = source {
            if let Some(signature) = self.demanded_source_signature(None, declaration, demand)? {
                return self
                    .apply_demanded_source_callable(
                        source,
                        Some(receiver),
                        &signature,
                        &argument_types,
                        None,
                        &resolved_type_arguments,
                        None,
                    )
                    .map(Some);
            }
        }
        crate::fir::ResolvedTy::new(result)
            .map(Some)
            .map_err(|_| Self::failure())
    }

    /// Select the ordinary callable family denoted by `Classifier.name(...)`. The shared resolver
    /// owns whether the declaration is a Java static, Kotlin companion instance/static, or an
    /// implicit enum callable; compact signature solving only consumes the selected semantic member
    /// and, for source declarations, demands its stable inferred result.
    fn classifier_call_argument_expectations(
        &self,
        scope: crate::fir::SignatureScope,
        classifier: crate::types::TypeName,
        spelling: &str,
        arguments: &[crate::fir::SigCallArgumentProbe<'_>],
        type_arguments: &[crate::fir::ResolvedTy],
        trailing_lambda: bool,
    ) -> Result<Box<[Option<crate::fir::ResolvedTy>]>, crate::fir::DiagnosticId> {
        let resolved_type_arguments = type_arguments
            .iter()
            .map(|argument| argument.get())
            .collect::<Vec<_>>();
        let (parameters, call_sig) = self.with_resolver(scope, |resolver| {
            let (receiver, candidates) =
                resolver.classifier_call_candidates(classifier, spelling)?;
            let (kinds, _) = Self::probe_call_arguments(&candidates, arguments, trailing_lambda)?;
            crate::trace_compiler!(
                "signature",
                "classifier call expectation candidates classifier={classifier:?} spelling={spelling} kinds={kinds:?} candidates={:?}",
                candidates
                    .iter()
                    .map(|candidate| (
                        candidate.callable.owner,
                        candidate.callable.descriptor.as_str(),
                        candidate.semantic_params(),
                        candidate.call_sig.required,
                    ))
                    .collect::<Vec<_>>(),
            );
            let callables = crate::libraries::Callables::from_parts(
                crate::libraries::FunctionSet {
                    overloads: candidates.clone(),
                },
                crate::libraries::PropertySet::default(),
            );
            let selected = match resolver.select_receiver_function_with_params_tracking(
                receiver,
                spelling,
                &kinds,
                &resolved_type_arguments,
                &callables,
            ) {
                crate::symbol_resolver::CandidateSelection::Selected((selected, _, _)) => {
                    selected
                }
                crate::symbol_resolver::CandidateSelection::None
                | crate::symbol_resolver::CandidateSelection::Ambiguous => {
                    Self::uniquely_mapped_candidate(&candidates, arguments, trailing_lambda)?
                }
            };
            let parameters = crate::symbol_resolver::specialized_function_params(
                &selected,
                &kinds,
                &resolved_type_arguments,
            );
            let parameters = parameters
                .get(selected.context_count.min(parameters.len())..)
                .unwrap_or_default()
                .iter()
                .copied();
            Some((
                Self::functional_parameter_shapes(resolver, &selected, parameters),
                selected.call_sig,
            ))
        })?;
        Self::postponed_call_expectations(arguments, &parameters, &call_sig, trailing_lambda)
            .ok_or_else(Self::failure)
    }

    fn select_classifier_call(
        &self,
        scope: crate::fir::SignatureScope,
        classifier: crate::types::TypeName,
        spelling: &str,
        arguments: &[crate::fir::ResolvedSigCallArgument<'_>],
        type_arguments: &[crate::fir::ResolvedTy],
        trailing_lambda: bool,
        demand: &mut dyn FnMut(
            crate::fir::DeclarationId,
        )
            -> Result<crate::fir::ResolvedSignature, crate::fir::DiagnosticId>,
    ) -> Result<Option<crate::fir::ResolvedTy>, crate::fir::DiagnosticId> {
        let resolved_type_arguments = type_arguments
            .iter()
            .map(|argument| argument.get())
            .collect::<Vec<_>>();
        let selected = self.with_resolver(scope, |resolver| {
            let (receiver, candidates) =
                resolver.classifier_call_candidates(classifier, spelling)?;
            let (arguments, argument_types) =
                Self::mapped_call_arguments(&candidates, arguments, trailing_lambda)?;
            let member = resolver.select_classifier_callable_from_candidates(
                receiver,
                spelling,
                &arguments,
                &resolved_type_arguments,
                candidates,
            )?;
            Some((member, argument_types))
        });
        let Ok((member, argument_types)) = selected else {
            return Ok(None);
        };
        let receiver = member.receiver;
        if let Some(declaration) = member.member.stable_declaration {
            if self
                .headers
                .stubs
                .iter()
                .any(|stub| stub.id == declaration && stub.signature_inference.is_some())
            {
                let signature = demand(declaration)?;
                return self
                    .apply_demanded_member(
                        receiver,
                        &member.member,
                        &signature,
                        &argument_types,
                        &resolved_type_arguments,
                    )
                    .map(Some);
            }
        }
        if let Some(signature) =
            self.demanded_member_signature(member.member.stable_declaration, demand)?
        {
            return self
                .apply_demanded_member(
                    receiver,
                    &member.member,
                    &signature,
                    &argument_types,
                    &resolved_type_arguments,
                )
                .map(Some);
        }
        crate::fir::ResolvedTy::new(member.ret)
            .map(Some)
            .map_err(|_| Self::failure())
    }

    /// Property counterpart of [`Self::select_associated_classifier_call`]. The ordinary extension
    /// property selector supplies scope, visibility, receiver applicability, and specialization;
    /// this adapter only requires the selected declaration to carry the associated-call fact.
    fn select_associated_classifier_property(
        &self,
        scope: crate::fir::SignatureScope,
        classifier: crate::types::TypeName,
        spelling: &str,
        demand: &mut dyn FnMut(
            crate::fir::DeclarationId,
        )
            -> Result<crate::fir::ResolvedSignature, crate::fir::DiagnosticId>,
    ) -> Result<Option<crate::fir::ResolvedTy>, crate::fir::DiagnosticId> {
        let receiver = Ty::obj_name(classifier);
        let property = self
            .with_resolver(scope, |resolver| {
                resolver
                    .select_extension_property(receiver, spelling)
                    .ok()
                    .flatten()
                    .filter(crate::libraries::PropertyInfo::is_companion_extension)
            })
            .ok();
        let Some(property) = property else {
            let associated = self.with_resolver(scope, |resolver| {
                resolver.accessible_classifier_associated_property(classifier, spelling)
            });
            return match associated {
                Ok(property) => crate::fir::ResolvedTy::new(property.ty)
                    .map(Some)
                    .map_err(|_| Self::failure()),
                Err(_) => Ok(None),
            };
        };
        if let Some(signature) =
            self.demanded_source_signature(None, property.stable_declaration, demand)?
        {
            return Ok(Some(signature.result));
        }
        crate::fir::ResolvedTy::new(property.ty)
            .map(Some)
            .map_err(|_| Self::failure())
    }
}

impl ProductionSignatureSemantics<'_> {
    pub(super) fn active_scoped_constraint_frame(
        &self,
        declaration: crate::fir::DeclarationId,
    ) -> Option<(crate::fir::DeclarationId, usize)> {
        let constraints = self.scoped_constraints.borrow();
        let inputs = self.scoped_constraint_inputs.borrow();
        let mut current = Some(declaration);
        while let Some(declaration) = current {
            if let (Some(constraint_stack), Some(input_stack)) =
                (constraints.get(&declaration), inputs.get(&declaration))
            {
                if let Some(index) =
                    input_stack
                        .iter()
                        .enumerate()
                        .rev()
                        .find_map(|(index, inputs)| {
                            (constraint_stack.get(index).is_some()
                                && inputs.iter().any(|input| input.mentions_ty_param()))
                            .then_some(index)
                        })
                {
                    return Some((declaration, index));
                }
            }
            current = self.declaration_semantic_parent(declaration);
        }
        None
    }

    fn push_scoped_receiver(&self, declaration: crate::fir::DeclarationId, receiver: Ty) {
        self.scoped_receivers
            .borrow_mut()
            .entry(declaration)
            .or_default()
            .push(receiver);
    }

    fn pop_scoped_receiver(&self, declaration: crate::fir::DeclarationId) {
        let mut receivers = self.scoped_receivers.borrow_mut();
        let remove = receivers.get_mut(&declaration).is_some_and(|stack| {
            stack.pop();
            stack.is_empty()
        });
        if remove {
            receivers.remove(&declaration);
        }
    }

    fn push_scoped_constraint_frame(
        &self,
        declaration: crate::fir::DeclarationId,
        inputs: Vec<Ty>,
    ) {
        self.scoped_constraint_inputs
            .borrow_mut()
            .entry(declaration)
            .or_default()
            .push(inputs);
        self.scoped_constraints
            .borrow_mut()
            .entry(declaration)
            .or_default()
            .push(crate::symbol_resolver::GSigBinds::new());
    }

    fn pop_scoped_constraint_frame(&self, declaration: crate::fir::DeclarationId) {
        let mut semantic_ancestors = Vec::new();
        let mut ancestor = self.declaration_semantic_parent(declaration);
        while let Some(current) = ancestor {
            semantic_ancestors.push(current);
            ancestor = self.declaration_semantic_parent(current);
        }
        let mut inputs = self.scoped_constraint_inputs.borrow_mut();
        let remove_inputs = inputs.get_mut(&declaration).is_some_and(|stack| {
            stack.pop();
            stack.is_empty()
        });
        if remove_inputs {
            inputs.remove(&declaration);
        }
        drop(inputs);

        let completed = {
            let mut all = self.scoped_constraints.borrow_mut();
            let (completed, remove) = all
                .get_mut(&declaration)
                .map(|stack| (stack.pop(), stack.is_empty()))
                .unwrap_or((None, false));
            if remove {
                all.remove(&declaration);
            }
            completed
        };
        let Some(completed) = completed else {
            return;
        };
        let mut all = self.scoped_constraints.borrow_mut();
        let active_parent = all
            .get(&declaration)
            .and_then(|stack| stack.last())
            .is_some()
            .then_some(declaration)
            .or_else(|| {
                semantic_ancestors
                    .into_iter()
                    .find(|ancestor| all.get(ancestor).and_then(|stack| stack.last()).is_some())
            });
        if let Some(parent) = active_parent
            .and_then(|parent| all.get_mut(&parent))
            .and_then(|stack| stack.last_mut())
        {
            Self::merge_scoped_constraints(parent, completed.clone());
            drop(all);
            let mut finished = self.completed_scoped_constraints.borrow_mut();
            let target = finished.entry(declaration).or_default();
            Self::merge_scoped_constraints(target, completed);
            return;
        }
        drop(all);
        let mut finished = self.completed_scoped_constraints.borrow_mut();
        let target = finished.entry(declaration).or_default();
        Self::merge_scoped_constraints(target, completed);
    }

    /// Contextual shapes contributed by one already-resolved classifier constructor. Both bare
    /// classifier calls and receiver-bound inner-class calls use this operation; only the step that
    /// resolves `internal` differs between those scope-tower rungs.
    fn constructor_call_argument_expectations(
        &self,
        scope: crate::fir::SignatureScope,
        internal: crate::types::TypeName,
        arguments: &[crate::fir::SigCallArgumentProbe<'_>],
        type_arguments: &[Ty],
        trailing_lambda: bool,
    ) -> Result<Box<[Option<crate::fir::ResolvedTy>]>, crate::fir::DiagnosticId> {
        let (parameters, slots) = self.with_resolver(scope, |resolver| {
            let source_probes = arguments
                .iter()
                .map(Self::probe_argument_kind)
                .collect::<Vec<_>>();
            let source_indices = (0..arguments.len()).collect::<Vec<_>>();
            let names = arguments
                .iter()
                .map(|argument| match argument {
                    crate::fir::SigCallArgumentProbe::Typed(argument) => {
                        argument.name.map(str::to_owned)
                    }
                    crate::fir::SigCallArgumentProbe::PostponedLambda { name, .. }
                    | crate::fir::SigCallArgumentProbe::PostponedCallableReference {
                        name, ..
                    } => name.map(str::to_owned),
                })
                .collect::<Vec<_>>();
            let selected = ((!trailing_lambda && names.iter().all(Option::is_none))
                .then(|| {
                    resolver.select_constructor_declaration_with_type_arguments(
                        internal,
                        &source_probes,
                        type_arguments,
                    )
                })
                .flatten())
            .or_else(|| {
                // A callable reference is postponed until its expected reflective
                // property/function shape is known. If applicability cannot run before that
                // materialization, a single declaration-owned source argument map supplies the
                // expectation without selecting between distinct constructor shapes.
                let classifier = resolver.classifier(internal)?;
                let mut candidates = classifier.constructors.iter().filter(|candidate| {
                    crate::libraries::map_call_args(
                        &source_indices,
                        Some(&names),
                        &candidate.call_sig.param_names,
                        candidate.params.len(),
                        candidate.call_sig.required,
                        &candidate.call_sig.param_defaults,
                        candidate.call_sig.vararg_index,
                        trailing_lambda,
                    )
                    .is_ok()
                });
                let mut selected = candidates.next()?.clone();
                if candidates.next().is_some() {
                    return None;
                }
                selected.owner.get_or_insert(internal);
                Some(selected)
            })?;
            let slots = crate::libraries::map_call_args(
                &source_indices,
                Some(&names),
                &selected.call_sig.param_names,
                selected.params.len(),
                selected.call_sig.required,
                &selected.call_sig.param_defaults,
                selected.call_sig.vararg_index,
                trailing_lambda,
            )
            .ok()?;
            let mapped_probes = slots
                .iter()
                .map(|source| {
                    source
                        .and_then(|source| source_probes.get(source).cloned())
                        .unwrap_or(crate::symbol_resolver::CallArgKind::OmittedDefault)
                })
                .collect::<Vec<_>>();
            let parameters = resolver
                .specialized_constructor_parameter_types(&selected, &mapped_probes, type_arguments)
                .into_iter()
                .map(|parameter| {
                    resolver
                        .functional_expectation(parameter)
                        .unwrap_or(parameter)
                })
                .collect::<Vec<_>>();
            Some((parameters, slots))
        })?;
        Ok(Self::postponed_expectations(arguments, &slots, &parameters))
    }
}

impl crate::fir::SignatureSemantics for ProductionSignatureSemantics<'_> {
    fn enter_scoped_receiver(
        &self,
        declaration: crate::fir::DeclarationId,
        receiver: crate::fir::ResolvedTy,
    ) {
        crate::trace_compiler!(
            "signature",
            "enter contextual receiver declaration={declaration:?} receiver={:?}",
            receiver.get(),
        );
        self.push_scoped_receiver(declaration, receiver.get());
        self.push_scoped_constraint_frame(declaration, vec![receiver.get()]);
    }

    fn exit_scoped_receiver(&self, declaration: crate::fir::DeclarationId) {
        self.pop_scoped_receiver(declaration);
        self.pop_scoped_constraint_frame(declaration);
    }

    fn enter_contextual_function(
        &self,
        declaration: crate::fir::DeclarationId,
        inputs: &[crate::fir::ResolvedTy],
        context_receivers: &[crate::fir::ResolvedTy],
        receiver: Option<crate::fir::ResolvedTy>,
    ) {
        crate::trace_compiler!(
            "signature",
            "enter contextual function declaration={declaration:?} inputs={:?} contexts={:?} receiver={:?}",
            inputs.iter().map(|input| input.get()).collect::<Vec<_>>(),
            context_receivers
                .iter()
                .map(|input| input.get())
                .collect::<Vec<_>>(),
            receiver.map(crate::fir::ResolvedTy::get),
        );
        for context in context_receivers {
            self.push_scoped_receiver(declaration, context.get());
        }
        if let Some(receiver) = receiver {
            self.push_scoped_receiver(declaration, receiver.get());
        }
        self.push_scoped_constraint_frame(
            declaration,
            inputs.iter().map(|input| input.get()).collect(),
        );
    }

    fn exit_contextual_function(
        &self,
        declaration: crate::fir::DeclarationId,
        receiver_count: usize,
    ) {
        for _ in 0..receiver_count {
            self.pop_scoped_receiver(declaration);
        }
        self.pop_scoped_constraint_frame(declaration);
    }

    fn declaration_parameters(
        &self,
        declaration: crate::fir::DeclarationId,
    ) -> Result<Box<[crate::fir::ResolvedTy]>, crate::fir::DiagnosticId> {
        self.parameters
            .get(&declaration)
            .cloned()
            .ok_or_else(Self::failure)
    }

    fn approximate_declaration_result(
        &self,
        declaration: crate::fir::DeclarationId,
        result: crate::fir::ResolvedTy,
    ) -> Result<crate::fir::ResolvedTy, crate::fir::DiagnosticId> {
        let anchor = self
            .headers
            .declarations
            .anchor(declaration)
            .ok_or_else(Self::failure)?;
        let scope = crate::fir::SignatureScope {
            owner: declaration,
            source: anchor.source,
        };
        let denotable = self.with_signature_type_scope(scope, |lexical| {
            lexical
                .lexical_tparam_identities()
                .into_iter()
                .collect::<std::collections::HashSet<_>>()
        })?;
        crate::fir::ResolvedTy::new(denotable_signature_result(result.get(), &denotable))
            .map_err(|_| Self::failure())
    }

    fn classifier_type(
        &self,
        declaration: crate::fir::DeclarationId,
        scope: crate::fir::SignatureScope,
    ) -> Result<crate::fir::ResolvedTy, crate::fir::DiagnosticId> {
        // Build the active lexical type-parameter bindings from stable declaration ownership.
        // Anonymous/local classifiers copy visible source parameters into their generated header;
        // those copied slots must bind to the enclosing declaration identities rather than becoming
        // fresh parameters of the generated classifier.
        let mut owners = Vec::new();
        let mut current = Some(scope.owner);
        while let Some(owner) = current {
            owners.push(owner);
            current = self
                .headers
                .declarations
                .anchor(owner)
                .and_then(|anchor| anchor.owner);
        }
        owners.reverse();
        let mut lexical = std::collections::HashMap::<&str, Ty>::new();
        for owner in owners {
            let semantic = self
                .callable_signature(owner)
                .and_then(|signature| signature.generic_sig.as_ref())
                .map(|generic| {
                    generic
                        .formals
                        .iter()
                        .zip(&generic.formal_bounds)
                        .map(|(name, bounds)| {
                            Ty::ty_param(
                                name,
                                bounds
                                    .first()
                                    .copied()
                                    .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any"))),
                            )
                        })
                        .collect::<Vec<_>>()
                })
                .or_else(|| {
                    self.classifier_signature(owner).map(|classifier| {
                        classifier
                            .type_parameters
                            .type_params
                            .iter()
                            .zip(&classifier.type_parameters.type_param_bounds)
                            .map(|(name, bound)| Ty::ty_param(name, *bound))
                            .collect::<Vec<_>>()
                    })
                })
                .unwrap_or_default();
            for (parameter, ty) in self.header_type_parameters(owner).iter().zip(semantic) {
                if let Some(source) = self.headers.lookup_names.get(parameter.name) {
                    lexical.insert(source, ty);
                }
            }
        }
        let resolved = self
            .classifier_types
            .get(&declaration)
            .copied()
            .map(|classifier| {
                let Some(signature) = self.table.class_by_type_name(classifier) else {
                    return Ty::obj_name(classifier);
                };
                let own_sources = self
                    .header_type_parameters(declaration)
                    .iter()
                    .filter_map(|parameter| self.headers.lookup_names.get(parameter.name));
                let own = signature
                    .type_parameters
                    .type_params
                    .iter()
                    .zip(&signature.type_parameters.type_param_bounds)
                    .zip(own_sources)
                    .map(|((parameter, bound), source)| {
                        lexical
                            .get(source)
                            .copied()
                            .unwrap_or_else(|| Ty::ty_param(parameter, *bound))
                    });
                let captured_sources = self
                    .header_classifier_captures(declaration)
                    .iter()
                    .filter_map(|parameter| self.headers.lookup_names.get(parameter.name))
                    .collect::<Vec<_>>();
                let captured = signature
                    .captured_type_parameters
                    .type_params
                    .iter()
                    .zip(&signature.captured_type_parameters.type_param_bounds)
                    .enumerate()
                    .map(|(ordinal, (parameter, bound))| {
                        let source = captured_sources
                            .get(ordinal)
                            .copied()
                            .unwrap_or_else(|| crate::types::type_parameter_source_name(parameter));
                        lexical
                            .get(source)
                            .copied()
                            .unwrap_or_else(|| Ty::ty_param(parameter, *bound))
                    });
                let arguments = own.chain(captured).collect::<Vec<_>>();
                Ty::obj_args_name(classifier, &arguments)
            })
            .and_then(|ty| crate::fir::ResolvedTy::new(ty).ok())
            .ok_or_else(Self::failure);
        crate::trace_compiler!(
            "signature",
            "classifier_type declaration={declaration:?} -> {resolved:?} methods={:?}",
            self.classifier_signature(declaration)
                .map(|classifier| classifier.methods.keys().collect::<Vec<_>>()),
        );
        resolved
    }

    fn resolve_type(
        &self,
        scope: crate::fir::SignatureScope,
        _origin: crate::fir::OriginId,
        syntax: crate::fir::HeaderTypeId,
        graph: &crate::fir::SignatureGraph,
    ) -> Result<crate::fir::ResolvedTy, crate::fir::DiagnosticId> {
        let reference = graph.transient_type_ref(syntax).ok_or_else(Self::failure)?;
        // Signature-native type resolution: a type reference in a compact header is a type
        // parameter of the owning declaration chain, a scoped alias/classifier, or a leaf shape.
        // None of those needs a body checker, and Pass 1 has no body to check.
        let ty = self.resolve_signature_type_reference(scope, &reference)?;
        crate::fir::ResolvedTy::new(ty).map_err(|_| Self::failure())
    }

    fn resolve_contextual_type(
        &self,
        scope: crate::fir::SignatureScope,
        origin: crate::fir::OriginId,
        syntax: crate::fir::HeaderTypeId,
        expected: crate::fir::ResolvedTy,
        graph: &crate::fir::SignatureGraph,
    ) -> Result<crate::fir::ResolvedTy, crate::fir::DiagnosticId> {
        if let Ok(resolved) = self.resolve_type(scope, origin, syntax, graph) {
            return Ok(resolved);
        }
        let reference = graph.transient_type_ref(syntax).ok_or_else(Self::failure)?;
        if reference.name.contains(['.', '/', '$'])
            || !reference.targs.is_empty()
            || reference.arg.is_some()
            || !reference.fun_params.is_empty()
        {
            return Err(Self::failure());
        }
        let module = crate::module_symbols::ModuleSymbols::for_file(self.table, scope.source.raw());
        let source = crate::symbol_source::CompositeSource::new(vec![
            &module as &dyn crate::symbol_source::SymbolSource,
            &*self.table.libraries as &dyn crate::symbol_source::SymbolSource,
        ]);
        let crate::symbol_resolver::InheritedNestedClassifier::Found(classifier) =
            super::super::context_sensitive_resolution::expected_nested_classifier(
                &source,
                expected.get(),
                &reference.name,
            )
        else {
            return Err(Self::failure());
        };
        let contextual = crate::symbol_resolver::apply_subtype_arguments_from_supertype(
            &source,
            Ty::obj_name(classifier),
            expected.get(),
        );
        let contextual = if reference.nullable() {
            Ty::nullable(contextual)
        } else {
            contextual
        };
        crate::fir::ResolvedTy::new(contextual).map_err(|_| Self::failure())
    }

    fn select_value(
        &self,
        scope: crate::fir::SignatureScope,
        spelling: &str,
        origin: crate::fir::OriginId,
        _expected: Option<crate::fir::ResolvedTy>,
        demand: &mut dyn FnMut(
            crate::fir::DeclarationId,
        )
            -> Result<crate::fir::ResolvedSignature, crate::fir::DiagnosticId>,
    ) -> Result<crate::fir::ResolvedTy, crate::fir::DiagnosticId> {
        crate::trace_compiler!(
            "signature",
            "select_value spelling={spelling} receivers={:?}",
            self.implicit_receivers(scope),
        );
        if spelling.contains('.') {
            if let Some((qualifier, name)) = spelling.rsplit_once('.') {
                if qualifier
                    .strip_prefix("this@")
                    .is_some_and(|label| self.enclosing_enum_entry_receiver(scope, label).is_some())
                {
                    if let Some(declaration) = self.enclosing_enum_entry_property(scope, name) {
                        return demand(declaration).map(|signature| signature.result);
                    }
                }
                if qualifier == "super"
                    || qualifier.starts_with("super<")
                    || qualifier.starts_with("super@")
                {
                    return self
                        .selected_super_member_property_result(scope, qualifier, name, demand);
                }
                if let Some(classifier) =
                    self.qualified_classifier_or_source_alias(scope, qualifier)
                {
                    if let Some(result) =
                        self.select_associated_classifier_property(scope, classifier, name, demand)?
                    {
                        return Ok(result);
                    }
                }
            }
            if let Ok(value) = self.qualified_receiver_ty(scope, spelling, origin, demand) {
                return Ok(value);
            }
            if let Some((qualifier, name)) = spelling.rsplit_once('.') {
                let package = crate::types::type_name(&qualifier.replace('.', "/"));
                let file = self
                    .headers
                    .scopes
                    .file(scope.source)
                    .ok_or_else(Self::failure)?;
                let access_package = self
                    .headers
                    .scopes
                    .path(file.package)
                    .iter()
                    .map(|segment| self.headers.lookup_names.get(*segment))
                    .collect::<Option<Vec<_>>>()
                    .map(|segments| crate::types::type_name(&segments.join("/")))
                    .ok_or_else(Self::failure)?;
                let module =
                    crate::module_symbols::ModuleSymbols::for_file(self.table, scope.source.raw());
                let property = crate::symbol_resolver::SymbolResolver::new_scoped_with_module(
                    self.table.libraries.as_ref(),
                    &module,
                    std::slice::from_ref(&package),
                )
                .with_access_context(access_package, scope.source.raw(), Vec::new())
                .resolve_symbol(crate::symbol_resolver::SymRecv::TopLevel, name, &[], &[])
                .and_then(crate::symbol_resolver::Symbol::value)
                .filter(|property| property.kind == crate::libraries::PropKind::TopLevel);
                if let Some(property) = property {
                    if let Some(signature) = self.demanded_source_signature(
                        Some(scope),
                        property.stable_declaration,
                        demand,
                    )? {
                        return Ok(signature.result);
                    }
                    return crate::fir::ResolvedTy::new(property.ty).map_err(|_| Self::failure());
                }
            }
            return Err(Self::failure());
        }
        if spelling == "this" {
            return self
                .implicit_receivers(scope)
                .into_iter()
                .next()
                .and_then(|receiver| crate::fir::ResolvedTy::new(receiver).ok())
                .ok_or_else(Self::failure);
        }
        // `super.p` / `super<C>.p` in an inferred initializer. The receiver of a super access is the
        // SUPERTYPE, not the current class: without this the whole module's signatures decline with
        // no diagnostic. An explicit qualifier names the supertype directly; a bare `super` takes the
        // class supertype, which is the only one whose members a super access can read.
        if spelling == "super" || spelling.starts_with("super<") || spelling.starts_with("super@") {
            let receivers = self.implicit_receivers(scope);
            let label = spelling.rsplit_once('@').map(|(_, label)| label);
            let current = match label {
                Some(label) => receivers.into_iter().find(|receiver| {
                    receiver
                        .obj_internal()
                        .is_some_and(|classifier| classifier.nested_segment_ref() == label)
                }),
                None => receivers.into_iter().next(),
            }
            .ok_or_else(Self::failure)?;
            let base_spelling = spelling.split('@').next().unwrap_or(spelling);
            if let Some(qualifier) = base_spelling
                .strip_prefix("super<")
                .and_then(|rest| rest.strip_suffix('>'))
            {
                let internal = self
                    .qualified_classifier(scope, qualifier)
                    .ok_or_else(Self::failure)?;
                return crate::fir::ResolvedTy::new(Ty::obj_name(internal))
                    .map_err(|_| Self::failure());
            }
            let module =
                crate::module_symbols::ModuleSymbols::for_file(self.table, scope.source.raw());
            let source = crate::symbol_source::CompositeSource::new(vec![
                &module as &dyn crate::symbol_source::SymbolSource,
                &*self.table.libraries as &dyn crate::symbol_source::SymbolSource,
            ]);
            let source = &source as &dyn crate::symbol_source::SymbolSource;
            let supertypes = crate::symbol_resolver::direct_supertypes(source, current);
            let selected = self
                .enum_entry_direct_super(scope, current)
                .or_else(|| {
                    supertypes
                        .iter()
                        .copied()
                        .find(|supertype| {
                            supertype.kotlin_class_internal().is_some_and(|internal| {
                                source
                                    .classifier(internal)
                                    .is_some_and(|declaration| !declaration.is_interface())
                            })
                        })
                        .or_else(|| supertypes.first().copied())
                })
                .ok_or_else(Self::failure)?;
            return crate::fir::ResolvedTy::new(selected).map_err(|_| Self::failure());
        }
        if let Some(label) = spelling.strip_prefix("this@") {
            if let Some(receiver) = self.enclosing_enum_entry_receiver(scope, label) {
                return crate::fir::ResolvedTy::new(receiver).map_err(|_| Self::failure());
            }
            let labels_current_extension = self.headers.stubs.iter().any(|stub| {
                stub.id == scope.owner
                    && stub
                        .lookup_name
                        .and_then(|name| self.headers.lookup_names.get(name))
                        == Some(label)
            });
            if labels_current_extension {
                if let Some(receiver) = self.declaration_extension_receiver(scope.owner) {
                    return crate::fir::ResolvedTy::new(receiver).map_err(|_| Self::failure());
                }
            }
            return self
                .implicit_receivers(scope)
                .into_iter()
                .find(|receiver| {
                    receiver
                        .obj_internal()
                        .is_some_and(|classifier| classifier.nested_segment_ref() == label)
                })
                .and_then(|receiver| crate::fir::ResolvedTy::new(receiver).ok())
                .ok_or_else(Self::failure);
        }
        // Primary-constructor parameters occupy the lexical initializer rung in front of dispatch
        // properties. In `class C(vararg xs: Int) { val xs = xs }`, selecting the property first
        // creates a false self-cycle; the right-hand `xs` is the normalized `IntArray` parameter.
        if let Some(parameter) =
            self.demanded_enclosing_constructor_parameter(scope, spelling, demand)?
        {
            return crate::fir::ResolvedTy::new(parameter).map_err(|_| Self::failure());
        }
        if let Some(capture) = self.enclosing_capture_type(scope, spelling) {
            return crate::fir::ResolvedTy::new(capture).map_err(|_| Self::failure());
        }
        if let Some(declaration) = self.enclosing_enum_entry_property(scope, spelling) {
            return demand(declaration).map(|signature| signature.result);
        }
        for receiver in self
            .implicit_receivers(scope)
            .into_iter()
            .chain(self.enclosing_lexical_singleton_receivers(scope))
        {
            if let Some(result) =
                self.selected_member_property_type(scope, receiver, spelling, demand)?
            {
                return Ok(result);
            }
            // Compile-time constants are declaration facts on the receiver classifier, not
            // property accessor candidates. This is the same provider-normalized constant channel
            // used for qualified reads; it also applies to a bare read inside an extension on a
            // companion receiver (`fun Int.Companion.max() = MAX_VALUE`).
            if let Some(internal) = receiver.non_null().obj_internal() {
                if let Some(constant) = self
                    .table
                    .libraries
                    .classifier(internal)
                    .and_then(|declaration| declaration.constants.get(spelling).cloned())
                {
                    return crate::fir::ResolvedTy::new(constant.ty).map_err(|_| Self::failure());
                }
            }
            if let Ok((result, member, extension)) = self.with_resolver(scope, |resolver| {
                let crate::symbol_resolver::Symbol::Member(facets) = resolver.resolve_symbol(
                    crate::symbol_resolver::SymRecv::Value(receiver),
                    spelling,
                    &[],
                    &[],
                )?
                else {
                    return None;
                };
                if let Some(property) = facets.extension_property {
                    return Some((property.ty, None, Some(property)));
                }
                facets.read.map(|property| {
                    let member = property.member;
                    (property.ret, Some(member), None)
                })
            }) {
                if let Some(member) = member.as_ref() {
                    if let Some(signature) =
                        self.demanded_member_signature(member.stable_declaration, demand)?
                    {
                        return self.apply_demanded_member(receiver, member, &signature, &[], &[]);
                    }
                }
                if let Some(extension) = extension {
                    if let Some(signature) =
                        self.demanded_source_signature(None, extension.stable_declaration, demand)?
                    {
                        return Ok(signature.result);
                    }
                }
                return crate::fir::ResolvedTy::new(result).map_err(|_| Self::failure());
            }
            for dispatch_receiver in self.signature_dispatch_receivers(scope) {
                let selected = self.member_extension_property_for(
                    scope,
                    receiver,
                    dispatch_receiver,
                    spelling,
                );
                let (result, declaration) = match selected {
                    Ok(Some(selected)) => selected,
                    Ok(None) => continue,
                    Err(()) => {
                        return Err(self.record_ambiguous_member(scope.owner, origin, spelling));
                    }
                };
                crate::trace_compiler!(
                    "signature",
                    "member extension property selected name={spelling} extension={receiver:?} dispatch={dispatch_receiver:?} result={result:?} declaration={declaration:?}",
                );
                if let Some(declaration) = declaration {
                    if self
                        .headers
                        .stubs
                        .iter()
                        .any(|stub| stub.id == declaration && stub.signature_inference.is_some())
                    {
                        return demand(declaration).map(|signature| signature.result);
                    }
                }
                return crate::fir::ResolvedTy::new(result).map_err(|_| Self::failure());
            }
        }
        for classifier in self.lexical_class_names(scope) {
            if self.classifier_has_enum_entry(classifier, spelling) {
                return crate::fir::ResolvedTy::new(Ty::obj_name(classifier))
                    .map_err(|_| Self::failure());
            }
            if let Some(result) =
                self.selected_implicit_classifier_property(scope, classifier, spelling)
            {
                return crate::fir::ResolvedTy::new(result).map_err(|_| Self::failure());
            }
        }
        // A provider may expose receiver-less properties through an inherited classifier namespace
        // (for example a foreign superclass's associated declaration). Walk only namespaces whose
        // provider explicitly permits that inheritance and consume the normalized Kotlin property;
        // storage/realization never enters signature solving.
        for owner in self.lexical_classifier_callable_owners(scope) {
            if let Ok(property) = self.with_resolver(scope, |resolver| {
                resolver.accessible_classifier_associated_property(owner, spelling)
            }) {
                return crate::fir::ResolvedTy::new(property.ty).map_err(|_| Self::failure());
            }
        }
        // Enum entries are values exported by an explicit member import or a classifier star
        // import. They have no callable/property facet, so the ordinary imported-value resolver
        // cannot select them; retain the already-normalized classifier import rung and select the
        // declaration fact directly. Lexical values and receiver members above remain nearer.
        let imports = self.function_import_scope(scope.source)?;
        if let Some((namespace, declared_name)) = imports.explicit_target(spelling) {
            if let crate::symbol_source::SymbolNamespace::Classifier(owner) = namespace {
                if self.classifier_has_enum_entry(owner, &declared_name) {
                    return crate::fir::ResolvedTy::new(Ty::obj_name(owner))
                        .map_err(|_| Self::failure());
                }
                // A companion-declared property may be realized by an associated platform
                // accessor rather than an instance accessor (`@JvmField` on JVM). Explicit import
                // still denotes the Kotlin property declaration; consume the provider-normalized
                // property exactly as the ordinary Pass-2 checker does.
                if let Some(property) = self
                    .table
                    .libraries
                    .classifier_associated_property(owner, &declared_name)
                {
                    return crate::fir::ResolvedTy::new(property.ty).map_err(|_| Self::failure());
                }
            }
        }
        let mut imported_entry_owners = Vec::new();
        for owner in imports.levels()[1].iter().copied() {
            if self.classifier_has_enum_entry(owner, spelling)
                && !imported_entry_owners.contains(&owner)
            {
                imported_entry_owners.push(owner);
            }
        }
        match imported_entry_owners.as_slice() {
            [owner] => {
                return crate::fir::ResolvedTy::new(Ty::obj_name(*owner))
                    .map_err(|_| Self::failure());
            }
            [_, _, ..] => return Err(Self::failure()),
            [] => {}
        }
        let property = match self.with_resolver(scope, |resolver| {
            let properties = resolver
                .resolve_symbol(
                    crate::symbol_resolver::SymRecv::TopLevel,
                    spelling,
                    &[],
                    &[],
                )?
                .values();
            let module =
                crate::module_symbols::ModuleSymbols::for_file(self.table, scope.source.raw());
            let source = crate::symbol_source::CompositeSource::new(vec![
                &module as &dyn crate::symbol_source::SymbolSource,
                &*self.table.libraries as &dyn crate::symbol_source::SymbolSource,
            ]);
            let oracle = crate::symbol_resolver::SourceOracle(&source);
            let receivers = self.implicit_receivers(scope);
            let mut applicable = properties
                .into_iter()
                .filter(|property| {
                    property.kind == crate::libraries::PropKind::TopLevel
                        && property.context_count <= property.getter.params.len()
                        && super::super::context_argument_types(
                            &receivers,
                            &property.getter.params[..property.context_count],
                            &oracle,
                        )
                        .is_some()
                })
                .collect::<Vec<_>>();
            let best_context = applicable
                .iter()
                .map(|property| property.context_count)
                .max()?;
            applicable.retain(|property| property.context_count == best_context);
            let [property] = applicable.as_slice() else {
                return None;
            };
            Some(property.clone())
        }) {
            Ok(property) => property,
            Err(_) => {
                // A PRIMARY CONSTRUCTOR PARAMETER read from a member property initializer
                // (`class A(y: Int) { var x = y }`). It is not a member and not a top-level value, so no
                // ordinary lookup finds it; the enclosing classifier's constructor shape is where it lives.
                if let Some(parameter) =
                    self.demanded_enclosing_constructor_parameter(scope, spelling, demand)?
                {
                    return crate::fir::ResolvedTy::new(parameter).map_err(|_| Self::failure());
                }
                // Lexically nested classifiers occupy a nearer scope-tower level than imports.
                // `Resolver::classifier_in_scope` receives the file/import scope but not the
                // transient source-containment chain used by this compact signature graph, so add
                // that one semantic tier explicitly. This is what makes a named companion object
                // (`companion object B`) usable as the value receiver in `B.p`.
                let classifier =
                    if let Some(classifier) = self.lexically_nested_classifier(scope, spelling) {
                        classifier
                    } else {
                        self.with_resolver(scope, |resolver| {
                            let crate::symbol_resolver::CandidateSelection::Selected(classifier) =
                                resolver.classifier_in_scope(spelling)
                            else {
                                return None;
                            };
                            Some(classifier)
                        })?
                    };
                let Some(value) = self.classifier_value_type(classifier) else {
                    crate::trace_compiler!(
                        "signature",
                        "select_value declined {spelling}: classifier is neither a singleton nor has a companion",
                    );
                    return Err(Self::failure());
                };
                return crate::fir::ResolvedTy::new(value).map_err(|_| Self::failure());
            }
        };
        if let Some(signature) = self.demanded_source_signature_at(
            Some(scope),
            property.stable_declaration,
            Some(origin),
            demand,
        )? {
            return Ok(signature.result);
        }
        crate::fir::ResolvedTy::new(property.ty).map_err(|_| Self::failure())
    }

    fn select_call(
        &self,
        scope: crate::fir::SignatureScope,
        spelling: &str,
        origin: crate::fir::OriginId,
        arguments: &[crate::fir::ResolvedSigCallArgument<'_>],
        type_arguments: &[crate::fir::ResolvedTy],
        trailing_lambda: bool,
        expected: Option<crate::fir::ResolvedTy>,
        demand: &mut dyn FnMut(
            crate::fir::DeclarationId,
        )
            -> Result<crate::fir::ResolvedSignature, crate::fir::DiagnosticId>,
    ) -> Result<crate::fir::ResolvedTy, crate::fir::DiagnosticId> {
        crate::trace_compiler!(
            "signature",
            "select_call spelling={spelling} expected={:?}",
            expected.map(crate::fir::ResolvedTy::get),
        );
        let entry_candidates = self.enclosing_enum_entry_callables(scope, spelling);
        if !entry_candidates.is_empty() {
            let argument_types = arguments
                .iter()
                .map(|argument| argument.ty.get())
                .collect::<Vec<_>>();
            let resolved_type_arguments = type_arguments
                .iter()
                .map(|argument| argument.get())
                .collect::<Vec<_>>();
            let module =
                crate::module_symbols::ModuleSymbols::for_file(self.table, scope.source.raw());
            let source = crate::symbol_source::CompositeSource::new(vec![
                &module as &dyn crate::symbol_source::SymbolSource,
                &*self.table.libraries as &dyn crate::symbol_source::SymbolSource,
            ]);
            let mut applicable = Vec::new();
            for declaration in entry_candidates {
                let signature = demand(declaration)?;
                let generic =
                    self.header_only_callable_generic_signature(declaration, &signature)?;
                if let Some((_, result)) = crate::symbol_resolver::specialize_typed_call_signature(
                    &source,
                    &generic,
                    &argument_types,
                    &resolved_type_arguments,
                    expected.map(crate::fir::ResolvedTy::get),
                ) {
                    applicable
                        .push(crate::fir::ResolvedTy::new(result).map_err(|_| Self::failure())?);
                }
            }
            return match applicable.as_slice() {
                [result] => Ok(*result),
                [] | [_, _, ..] => Err(Self::failure()),
            };
        }
        if let Some(parameter) =
            self.demanded_enclosing_constructor_parameter(scope, spelling, demand)?
        {
            let callee = crate::fir::ResolvedTy::new(parameter).map_err(|_| Self::failure())?;
            return self.select_invoke(scope, origin, callee, arguments, demand);
        }
        if let Some(capture) = self.enclosing_capture_type(scope, spelling) {
            let callee = crate::fir::ResolvedTy::new(capture).map_err(|_| Self::failure())?;
            return self.select_invoke(scope, origin, callee, arguments, demand);
        }
        let (argument_kinds, argument_types) = Self::mapped_call_arguments(&[], arguments, false)
            .unwrap_or_else(|| {
                let kinds = arguments.iter().map(Self::call_argument_kind).collect();
                let types = arguments.iter().map(|argument| argument.ty.get()).collect();
                (kinds, types)
            });
        let resolved_type_arguments = type_arguments
            .iter()
            .map(|argument| argument.get())
            .collect::<Vec<_>>();
        if let Some((qualifier, name)) = spelling.rsplit_once('.') {
            if qualifier == "super"
                || qualifier.starts_with("super<")
                || qualifier.starts_with("super@")
            {
                return self.selected_super_member_call_result(
                    scope,
                    qualifier,
                    name,
                    arguments,
                    &resolved_type_arguments,
                    trailing_lambda,
                    demand,
                );
            }
            // A dotted callee is either PACKAGE-qualified (`kotlin.collections.listOf`) or a member
            // call on a qualified RECEIVER (`E.valueOf`, `E.OK.toString`, `C.Companion.of`). The
            // package form is tried first because it needs no receiver at all; otherwise the
            // qualifier is folded into a receiver type and the last segment is an ordinary member
            // call, exactly as the checker would resolve it.
            let package_call = self.select_qualified_package_call(
                scope,
                spelling,
                arguments,
                type_arguments,
                trailing_lambda,
                demand,
            );
            if package_call.is_ok() {
                return package_call;
            }
            // A dotted callee also spells a NESTED CLASSIFIER's constructor: `Container.Nested(x)`
            // names the type `Container$Nested`, not a member `Nested` on a value `Container`. The
            // qualifier there is a namespace, so folding it into a receiver cannot resolve it — a
            // plain class is not a value. Try the whole spelling as a classifier first.
            if let Some(internal) = self.qualified_classifier(scope, spelling) {
                if let [argument] = argument_types.as_slice() {
                    if let Some(result) = selected_sam_constructor_result(
                        self,
                        scope,
                        internal,
                        *argument,
                        &resolved_type_arguments,
                    ) {
                        return crate::fir::ResolvedTy::new(result).map_err(|_| Self::failure());
                    }
                }
                if let Some((declaration, selected_argument_types)) = self
                    .with_resolver(scope, |resolver| {
                        let (arguments, types) = Self::mapped_constructor_arguments(
                            resolver,
                            internal,
                            arguments,
                            trailing_lambda,
                        )
                        .unwrap_or_else(|| (argument_kinds.clone(), argument_types.clone()));
                        resolver
                            .select_constructor_declaration_with_type_arguments(
                                internal,
                                &arguments,
                                &resolved_type_arguments,
                            )
                            .map(|declaration| (declaration, types))
                    })
                    .ok()
                {
                    return self.constructor_result(
                        scope,
                        &declaration,
                        &selected_argument_types,
                        None,
                        &resolved_type_arguments,
                    );
                }
                // Constructor applicability is one rung, not a terminal interpretation of the
                // classifier spelling. A class with a companion also denotes that singleton in
                // value position, so an inapplicable constructor falls through to the companion's
                // ordinary invoke convention. `select_invoke` owns member/extension selection and
                // inferred source-signature demand for local and dependency declarations alike.
                if let Some(value) = self.classifier_value_type(internal) {
                    let callee = crate::fir::ResolvedTy::new(value).map_err(|_| Self::failure())?;
                    if let Ok(result) = self.select_invoke(scope, origin, callee, arguments, demand)
                    {
                        return Ok(result);
                    }
                }
            }
            if let Some(classifier) = self.qualified_classifier_or_source_alias(scope, qualifier) {
                if let Some(result) = self.select_associated_classifier_call(
                    scope,
                    classifier,
                    name,
                    arguments,
                    type_arguments,
                    trailing_lambda,
                    demand,
                )? {
                    return Ok(result);
                }
                if let Some(result) = self.select_classifier_call(
                    scope,
                    classifier,
                    name,
                    arguments,
                    type_arguments,
                    trailing_lambda,
                    demand,
                )? {
                    return Ok(result);
                }
            }
            let receiver = self.qualified_receiver_ty(scope, qualifier, origin, demand)?;
            return self
                .select_member_call(
                    scope,
                    name,
                    origin,
                    receiver,
                    arguments,
                    type_arguments,
                    trailing_lambda,
                    None,
                    demand,
                )
                .and_then(|selection| selection.ty.ok_or_else(Self::failure));
        }
        let explicit_context_arguments = self
            .headers
            .scopes
            .file(scope.source)
            .is_some_and(|file| file.explicit_context_arguments);
        for receiver in self
            .implicit_receivers(scope)
            .into_iter()
            .chain(self.enclosing_lexical_singleton_receivers(scope))
        {
            if let Ok((
                result,
                member,
                source,
                declaration,
                selected_argument_types,
                postponed_bindings,
            )) = self.with_resolver(scope, |resolver| {
                let (mut functions, properties) =
                    resolver.receiver_callables(receiver, spelling).into_parts();
                functions.overloads = self
                    .implicit_context_candidates(scope, std::mem::take(&mut functions.overloads));
                let callables = crate::libraries::Callables::from_parts(functions, properties);
                let (selected_arguments, selected_argument_types) =
                    Self::mapped_call_arguments(callables.functions(), arguments, trailing_lambda)?;
                let projected =
                    self.project_postponed_callables(scope, callables, &selected_arguments);
                let selection = resolver.select_receiver_function_with_params_tracking(
                    receiver,
                    spelling,
                    &selected_arguments,
                    &resolved_type_arguments,
                    projected.callables(),
                );
                crate::trace_compiler!(
                    "signature",
                    "implicit receiver call receiver={receiver:?} spelling={spelling} selection={}",
                    match &selection {
                        crate::symbol_resolver::CandidateSelection::Selected(_) => "selected",
                        crate::symbol_resolver::CandidateSelection::Ambiguous => "ambiguous",
                        crate::symbol_resolver::CandidateSelection::None => "none",
                    },
                );
                let crate::symbol_resolver::CandidateSelection::Selected((
                    selected,
                    parameters,
                    result,
                )) = selection
                else {
                    return None;
                };
                let postponed_bindings = projected.selected_bindings(&selected);
                if selected.kind == crate::libraries::FnKind::Member {
                    let mut member = selected.member_with_return(result);
                    member.params = parameters;
                    return Some((
                        result,
                        Some(member),
                        None,
                        None,
                        selected_argument_types,
                        postponed_bindings,
                    ));
                }
                Some((
                    result,
                    None,
                    selected.source_key,
                    selected.stable_declaration,
                    selected_argument_types,
                    postponed_bindings,
                ))
            }) {
                self.commit_postponed_bindings(scope, postponed_bindings);
                if let Some(member) = member.as_ref() {
                    if let Some(declaration) = member.stable_declaration {
                        if self.headers.stubs.iter().any(|stub| {
                            stub.id == declaration && stub.signature_inference.is_some()
                        }) {
                            let signature = demand(declaration)?;
                            let parameters = signature
                                .parameters
                                .iter()
                                .map(|parameter| parameter.get())
                                .collect::<Vec<_>>();
                            self.record_scoped_member_constraints(
                                scope,
                                receiver,
                                member,
                                &parameters,
                                &selected_argument_types,
                            );
                            return self.apply_demanded_member(
                                receiver,
                                member,
                                &signature,
                                &selected_argument_types,
                                &resolved_type_arguments,
                            );
                        }
                    }
                    if let Some(signature) =
                        self.demanded_member_signature(member.stable_declaration, demand)?
                    {
                        let parameters = signature
                            .parameters
                            .iter()
                            .map(|parameter| parameter.get())
                            .collect::<Vec<_>>();
                        self.record_scoped_member_constraints(
                            scope,
                            receiver,
                            member,
                            &parameters,
                            &selected_argument_types,
                        );
                        return self.apply_demanded_member(
                            receiver,
                            member,
                            &signature,
                            &selected_argument_types,
                            &resolved_type_arguments,
                        );
                    }
                    let parameters = member
                        .generic_sig
                        .as_ref()
                        .map(|signature| signature.params.as_slice())
                        .unwrap_or(&member.params);
                    self.record_scoped_member_constraints(
                        scope,
                        receiver,
                        member,
                        parameters,
                        &selected_argument_types,
                    );
                }
                if let Some(source) = source {
                    if let Some(signature) =
                        self.demanded_source_signature(None, declaration, demand)?
                    {
                        return self.apply_demanded_source_callable(
                            source,
                            Some(receiver),
                            &signature,
                            &selected_argument_types,
                            None,
                            &resolved_type_arguments,
                            expected.map(crate::fir::ResolvedTy::get),
                        );
                    }
                }
                crate::trace_compiler!(
                    "signature",
                    "implicit receiver call selected receiver={receiver:?} spelling={spelling} result={result:?}",
                );
                return crate::fir::ResolvedTy::new(result).map_err(|_| Self::failure());
            }
            // A property whose value is callable participates in call syntax after ordinary member
            // functions at this same receiver rung. The property declaration and the `invoke`
            // convention are both selected by the shared resolver; SigExpr only composes them.
            if let Some(callee) =
                self.selected_member_property_type(scope, receiver, spelling, demand)?
            {
                if let Ok(result) = self.select_invoke(scope, origin, callee, arguments, demand) {
                    return Ok(result);
                }
                if let Ok(result) = self.select_member_call(
                    scope,
                    "invoke",
                    origin,
                    callee,
                    arguments,
                    &[],
                    false,
                    None,
                    demand,
                ) {
                    return result.ty.ok_or_else(Self::failure);
                }
            }
            let argument_names = arguments
                .iter()
                .map(|argument| argument.name.map(str::to_owned))
                .collect::<Vec<_>>();
            let argument_names = argument_names
                .iter()
                .any(Option::is_some)
                .then_some(argument_names.as_slice());
            let spread = arguments
                .iter()
                .map(|argument| argument.spread)
                .collect::<Vec<_>>();
            for dispatch_receiver in self.signature_dispatch_receivers(scope) {
                let selected = self.signature_member_extension_call(
                    scope,
                    receiver,
                    dispatch_receiver,
                    spelling,
                    super::lookups::SignatureMemberExtensionArguments {
                        types: &argument_types,
                        names: argument_names,
                        spread: &spread,
                        explicit_type_arguments: &resolved_type_arguments,
                        trailing_lambda,
                    },
                    expected.map(crate::fir::ResolvedTy::get),
                    super::super::MemberExtensionSelection::All,
                );
                let Some((result, declaration)) = selected else {
                    crate::trace_compiler!(
                        "signature",
                        "member extension miss name={spelling} extension={receiver:?} dispatch={dispatch_receiver:?}",
                    );
                    continue;
                };
                crate::trace_compiler!(
                    "signature",
                    "member extension selected name={spelling} extension={receiver:?} dispatch={dispatch_receiver:?} result={result:?} declaration={declaration:?}",
                );
                if let Some(declaration) = declaration {
                    if self
                        .headers
                        .stubs
                        .iter()
                        .any(|stub| stub.id == declaration && stub.signature_inference.is_some())
                    {
                        return demand(declaration).map(|signature| signature.result);
                    }
                }
                return crate::fir::ResolvedTy::new(result).map_err(|_| Self::failure());
            }
            if let Some(result) = self.bound_inner_constructor_result(
                scope,
                receiver,
                spelling,
                arguments,
                &resolved_type_arguments,
            )? {
                return Ok(result);
            }
        }
        for classifier in self.lexical_class_names(scope) {
            // The enclosing classifier itself is in lexical type scope. Inside its companion,
            // `Owner(args)` is therefore the owner's ordinary constructor call; treating `Owner`
            // only as a companion value would incorrectly search `Owner.Companion.invoke`.
            if classifier.nested_segment_ref() == spelling {
                if let Some((declaration, selected_argument_types)) = self
                    .with_resolver(scope, |resolver| {
                        let (arguments, types) = Self::mapped_constructor_arguments(
                            resolver,
                            classifier,
                            arguments,
                            trailing_lambda,
                        )
                        .unwrap_or_else(|| (argument_kinds.clone(), argument_types.clone()));
                        resolver
                            .select_constructor_declaration_with_type_arguments(
                                classifier,
                                &arguments,
                                &resolved_type_arguments,
                            )
                            .map(|declaration| (declaration, types))
                    })
                    .ok()
                {
                    return self.constructor_result(
                        scope,
                        &declaration,
                        &selected_argument_types,
                        None,
                        &resolved_type_arguments,
                    );
                }
            }
        }
        for callable_owner in self.lexical_classifier_callable_owners(scope) {
            if let Some(result) = self.select_classifier_call(
                scope,
                callable_owner,
                spelling,
                arguments,
                type_arguments,
                trailing_lambda,
                demand,
            )? {
                return Ok(result);
            }
        }
        if let Some((owner, declared_name)) =
            self.explicit_imported_classifier_callable(scope.source, spelling)
        {
            if let Some(result) = self.select_classifier_call(
                scope,
                owner,
                &declared_name,
                arguments,
                type_arguments,
                trailing_lambda,
                demand,
            )? {
                return Ok(result);
            }
        }
        let source_alias =
            self.applied_source_alias_expansion(scope, spelling, &resolved_type_arguments);
        let nested_classifier = self.lexically_nested_classifier(scope, spelling);
        let selected = self.with_resolver(scope, |resolver| {
            let candidates = resolver.top_level_candidates(spelling);
            crate::trace_compiler!(
                "signature",
                "top-level candidates spelling={spelling} arguments={argument_types:?} candidates={:?}",
                candidates
                    .iter()
                    .map(|candidate| (
                        candidate.source_key,
                        candidate.context_count,
                        candidate.semantic_params().into_owned(),
                        candidate.callable.ret,
                    ))
                    .collect::<Vec<_>>(),
            );
            if let Some(explicit) = Self::explicit_context_call(
                explicit_context_arguments,
                &candidates,
                arguments,
                trailing_lambda,
            ) {
                    let (selected, callable) =
                    if self.table.declaration_suppresses_visibility(scope.owner) {
                        resolver.select_top_level_function_candidates_with_expected_ignoring_visibility(
                            spelling,
                            explicit.candidates,
                            &explicit.arguments,
                            &resolved_type_arguments,
                            expected.map(crate::fir::ResolvedTy::get),
                        )?
                    } else {
                        resolver.select_top_level_function_candidates_with_expected(
                            spelling,
                            explicit.candidates,
                            &explicit.arguments,
                            &resolved_type_arguments,
                            expected.map(crate::fir::ResolvedTy::get),
                        )?
                    };
                return Some((
                    SelectedTopLevelCall::Callable {
                        callable,
                        source: selected.source_key,
                        declaration: selected.stable_declaration,
                    },
                    explicit.argument_types,
                ));
            }
            let candidates = self.implicit_context_candidates(scope, candidates);
            let mapped = Self::mapped_call_arguments(&candidates, arguments, trailing_lambda);
            let (selected_arguments, selected_argument_types) = mapped
                .clone()
                .unwrap_or_else(|| (argument_kinds.clone(), argument_types.clone()));
            if self.table.declaration_suppresses_visibility(scope.owner) {
                let all_top_level = candidates
                    .iter()
                    .filter(|candidate| candidate.kind == crate::libraries::FnKind::TopLevel)
                    .cloned()
                    .collect::<Vec<_>>();
                if let Some((selected, callable)) = resolver
                    .select_top_level_function_candidates_with_expected_ignoring_visibility(
                        spelling,
                        all_top_level,
                        &selected_arguments,
                        &resolved_type_arguments,
                        expected.map(crate::fir::ResolvedTy::get),
                    )
                {
                    return Some((
                        SelectedTopLevelCall::Callable {
                            callable,
                            source: selected.source_key,
                            declaration: selected.stable_declaration,
                        },
                        selected_argument_types,
                    ));
                }
            }
            let selected_top_level = resolver.select_top_level_function_candidates_with_expected(
                spelling,
                candidates.clone(),
                &selected_arguments,
                &resolved_type_arguments,
                expected.map(crate::fir::ResolvedTy::get),
            );
            crate::trace_compiler!(
                "signature",
                "projected top-level selection spelling={spelling} arguments={selected_arguments:?} type_arguments={resolved_type_arguments:?} selected={}",
                selected_top_level.is_some(),
            );
            if let Some((selected, callable)) = selected_top_level {
                return Some((
                    SelectedTopLevelCall::Callable {
                        source: selected.source_key,
                        declaration: selected.stable_declaration,
                        callable,
                    },
                    selected_argument_types.clone(),
                ));
            }
            let symbol = resolver.select_symbol(
                crate::symbol_resolver::SymRecv::TopLevel,
                spelling,
                &selected_arguments,
                &resolved_type_arguments,
            );
            crate::trace_compiler!(
                "signature",
                "call selection {spelling} arguments={selected_arguments:?} type_arguments={resolved_type_arguments:?} selected={}",
                symbol.is_some(),
            );
            // `pick_top_level` admits only PUBLIC declarations, so a call to a `private`/`internal`
            // top-level function in the same file never gets a `top_level_call` facet and the whole
            // module's signatures decline. The checker reaches those through its own
            // `source_callable_visible` rung; mirror that here, and only when the visible family is
            // unambiguous.
            let same_file_callable = || {
                let visible = candidates
                    .iter()
                    .filter(|candidate| {
                        candidate.kind == crate::libraries::FnKind::TopLevel
                            && match candidate.visibility {
                                crate::types::Visibility::Public => false,
                                crate::types::Visibility::Internal => {
                                    candidate.source_key.is_some()
                                }
                                crate::types::Visibility::Private => candidate
                                    .source_key
                                    .is_some_and(|(file, _)| file == scope.source.raw()),
                                crate::types::Visibility::PackagePrivate
                                | crate::types::Visibility::Protected => false,
                            }
                    })
                    .collect::<Vec<_>>();
                match visible.as_slice() {
                    [selected] => Some((*selected).clone()),
                    [] | [_, _, ..] => None,
                }
            };
            if let Some(symbol) = symbol {
                if let crate::symbol_resolver::Symbol::Member(facets) = &symbol {
                    if let [property] = facets.values.as_slice() {
                        return Some((
                            SelectedTopLevelCall::Value(property.clone()),
                            argument_types.clone(),
                        ));
                    }
                }
                if let crate::symbol_resolver::Symbol::Constructor(constructor) = &symbol {
                    let declaration = match constructor {
                        crate::symbol_resolver::SelectedConstructorCall::Direct(declaration) => {
                            declaration.as_ref().clone()
                        }
                        crate::symbol_resolver::SelectedConstructorCall::Platform(realization) => {
                            realization.declaration.clone()
                        }
                    };
                    return Some((
                        SelectedTopLevelCall::Constructor(declaration),
                        argument_types.clone(),
                    ));
                }
            }
            if let Some(selected) = same_file_callable() {
                return Some((
                    SelectedTopLevelCall::Callable {
                        callable: selected.callable.clone(),
                        source: selected.source_key,
                        declaration: selected.stable_declaration,
                    },
                    selected_argument_types,
                ));
            }
            // A lexically nested classifier is a nearer type-scope rung than file imports. Use
            // the same ordering as value/type lookup instead of letting an imported same-named
            // classifier shadow `class Outer { class Nested; fun f() = Nested() }`.
            let internal = match nested_classifier {
                Some(internal) => internal,
                None => match resolver.classifier_in_scope(spelling) {
                    crate::symbol_resolver::CandidateSelection::Selected(internal) => internal,
                    crate::symbol_resolver::CandidateSelection::Ambiguous
                    | crate::symbol_resolver::CandidateSelection::None => return None,
                },
            };
            crate::trace_compiler!(
                "signature",
                "constructor candidate spelling={spelling} classifier={internal} source={:?}",
                self.table.class_by_type_name(internal).map(|class| (
                    class.ctor_params.as_slice(),
                    class.ctor_param_names.as_slice(),
                    class.ctor_defaults.as_slice(),
                    class.has_primary_ctor,
                )),
            );
            // `I { … }` constructs a fun interface through its abstract method, not a class ctor.
            if arguments.len() == 1
                && resolver
                    .classifier(internal)
                    .is_some_and(|declaration| declaration.sam_eligible)
            {
                return Some((
                    SelectedTopLevelCall::SamConstructor(internal),
                    argument_types.clone(),
                ));
            }
            let (constructor_arguments, constructor_argument_types) =
                Self::mapped_constructor_arguments(
                    resolver,
                    internal,
                    arguments,
                    trailing_lambda,
                )
                .unwrap_or_else(|| (argument_kinds.clone(), argument_types.clone()));
            if let Some(declaration) = resolver.select_constructor_declaration_with_type_arguments(
                internal,
                &constructor_arguments,
                &resolved_type_arguments,
            ) {
                return Some((
                    SelectedTopLevelCall::Constructor(declaration),
                    constructor_argument_types,
                ));
            }
            self.classifier_value_type(internal).map(|value| {
                (
                    SelectedTopLevelCall::ClassifierValue(value),
                    argument_types.clone(),
                )
            })
        })
        .map_err(|diagnostic| {
            self.record_top_level_context_call_failure(
                scope,
                origin,
                spelling,
                arguments,
                trailing_lambda,
            )
                .unwrap_or(diagnostic)
        })?;
        let (selected, selected_argument_types) = selected;
        match selected {
            SelectedTopLevelCall::Callable {
                callable,
                source,
                declaration,
            } => {
                crate::trace_compiler!(
                    "signature",
                    "selected callable name={} source={source:?} result={:?}",
                    callable.name,
                    callable.ret,
                );
                if let Some(source) = source {
                    if let Some(signature) =
                        self.demanded_source_signature(None, declaration, demand)?
                    {
                        let context_count = self
                            .table
                            .funs
                            .values()
                            .flatten()
                            .find(|candidate| {
                                candidate.source_file == Some(source.0)
                                    && candidate.source_decl == Some(DeclId(source.1))
                            })
                            .map(|candidate| candidate.context_count)
                            .unwrap_or_default()
                            .min(signature.parameters.len());
                        let parameters = signature.parameters[context_count..]
                            .iter()
                            .map(|parameter| parameter.get())
                            .collect::<Vec<_>>();
                        self.record_scoped_argument_constraints(
                            scope,
                            &parameters,
                            &selected_argument_types,
                        );
                        return self.apply_demanded_source_callable(
                            source,
                            None,
                            &signature,
                            &selected_argument_types,
                            (!trailing_lambda
                                && arguments.iter().all(|argument| argument.name.is_none()))
                            .then_some(argument_kinds.as_slice()),
                            &resolved_type_arguments,
                            expected.map(crate::fir::ResolvedTy::get),
                        );
                    }
                }
                crate::fir::ResolvedTy::new(callable.ret).map_err(|_| Self::failure())
            }
            SelectedTopLevelCall::Value(property) => {
                let callee = match self.demanded_source_signature(
                    Some(scope),
                    property.stable_declaration,
                    demand,
                )? {
                    Some(signature) => signature.result,
                    None => {
                        crate::fir::ResolvedTy::new(property.ty).map_err(|_| Self::failure())?
                    }
                };
                self.select_invoke(scope, origin, callee, &arguments, demand)
            }
            SelectedTopLevelCall::ClassifierValue(callee) => {
                let callee = crate::fir::ResolvedTy::new(callee).map_err(|_| Self::failure())?;
                self.select_invoke(scope, origin, callee, arguments, demand)
            }
            SelectedTopLevelCall::SamConstructor(internal) => {
                let actual = selected_argument_types
                    .first()
                    .copied()
                    .ok_or_else(Self::failure)?;
                let result = selected_sam_constructor_result(
                    self,
                    scope,
                    internal,
                    actual,
                    &resolved_type_arguments,
                )
                .ok_or_else(Self::failure)?;
                if let Some((formals, expansion)) = source_alias.as_ref() {
                    return self
                        .apply_source_alias_constructor_result(
                            scope,
                            formals,
                            *expansion,
                            result,
                            Some(actual),
                            expected.map(crate::fir::ResolvedTy::get),
                        )
                        .ok_or_else(Self::failure);
                }
                crate::fir::ResolvedTy::new(result).map_err(|_| Self::failure())
            }
            SelectedTopLevelCall::Constructor(member) => {
                let result = self.constructor_result(
                    scope,
                    &member,
                    &selected_argument_types,
                    None,
                    &resolved_type_arguments,
                )?;
                if let Some((formals, expansion)) = source_alias.as_ref() {
                    return self
                        .apply_source_alias_constructor_result(
                            scope,
                            formals,
                            *expansion,
                            result.get(),
                            None,
                            expected.map(crate::fir::ResolvedTy::get),
                        )
                        .ok_or_else(Self::failure);
                }
                Ok(result)
            }
        }
    }

    fn call_argument_expectations(
        &self,
        scope: crate::fir::SignatureScope,
        spelling: &str,
        origin: crate::fir::OriginId,
        arguments: &[crate::fir::SigCallArgumentProbe<'_>],
        type_arguments: &[crate::fir::ResolvedTy],
        trailing_lambda: bool,
        _expected: Option<crate::fir::ResolvedTy>,
        demand: &mut dyn FnMut(
            crate::fir::DeclarationId,
        )
            -> Result<crate::fir::ResolvedSignature, crate::fir::DiagnosticId>,
    ) -> Result<Box<[Option<crate::fir::ResolvedTy>]>, crate::fir::DiagnosticId> {
        let resolved_type_arguments = type_arguments
            .iter()
            .map(|argument| argument.get())
            .collect::<Vec<_>>();
        // This phase exists only to supply contextual types to postponed arguments. Once every
        // argument is already typed, candidate existence/applicability is owned by `select_call`
        // after materialization. Repeating a partial lookup here both wastes work and can reject a
        // valid later tower rung (callable properties, member extensions, header-only declarations)
        // which final selection already handles through the shared resolver.
        if arguments
            .iter()
            .all(|argument| matches!(argument, crate::fir::SigCallArgumentProbe::Typed(_)))
        {
            return Ok(vec![None; arguments.len()].into_boxed_slice());
        }
        if spelling.contains('.') {
            if let Ok(expectations) = self.qualified_package_call_argument_expectations(
                scope,
                spelling,
                arguments,
                type_arguments,
                trailing_lambda,
            ) {
                return Ok(expectations);
            }
        }
        // A SAM constructor's abstract method supplies its otherwise absent callable shape.
        if arguments.len() == 1 {
            let module =
                crate::module_symbols::ModuleSymbols::for_file(self.table, scope.source.raw());
            let source = crate::symbol_source::CompositeSource::new(vec![
                &module as &dyn crate::symbol_source::SymbolSource,
                &*self.table.libraries as &dyn crate::symbol_source::SymbolSource,
            ]);
            let lexical_classifier = self
                .qualified_classifier(scope, spelling)
                .or_else(|| self.lexically_nested_classifier(scope, spelling));
            let sam_interface = self
                .with_resolver(scope, |resolver| {
                    lexical_classifier.or_else(|| match resolver.classifier_in_scope(spelling) {
                        crate::symbol_resolver::CandidateSelection::Selected(internal) => {
                            Some(internal)
                        }
                        crate::symbol_resolver::CandidateSelection::Ambiguous
                        | crate::symbol_resolver::CandidateSelection::None => None,
                    })
                })
                .ok()
                .filter(|internal| {
                    crate::symbol_resolver::semantic_sam_signature(&source, Ty::obj_name(*internal))
                        .is_some()
                });
            if let Some(internal) = sam_interface {
                let target = self
                    .applied_source_alias_expansion(scope, spelling, &resolved_type_arguments)
                    .map_or_else(|| Ty::obj_name(internal), |(_, expansion)| expansion);
                let sam = crate::symbol_resolver::semantic_sam_signature(&source, target)
                    .expect("the SAM signature was just selected");
                let shape = Ty::fun_with_shape(
                    sam.params.clone(),
                    sam.ret,
                    sam.context_count,
                    sam.has_receiver,
                    sam.suspend,
                );
                return Ok(vec![crate::fir::ResolvedTy::new(shape).ok()].into_boxed_slice());
            }
        }
        let parameters_and_slots: Result<
            (Vec<Ty>, Vec<Option<usize>>, crate::libraries::CallSig),
            _,
        > = self.with_resolver(scope, |resolver| {
            let candidates = resolver.top_level_candidates(spelling);
            let (kinds, slots) =
                Self::probe_call_arguments(&candidates, arguments, trailing_lambda)?;
            let visibility_override = self
                .table
                .declaration_suppresses_visibility(scope.owner)
                .then(|| {
                    resolver.select_top_level_function_candidates_ignoring_visibility(
                        spelling,
                        candidates
                            .iter()
                            .filter(|candidate| {
                                candidate.kind == crate::libraries::FnKind::TopLevel
                            })
                            .cloned()
                            .collect(),
                        &kinds,
                        &resolved_type_arguments,
                    )
                })
                .flatten()
                .and_then(|(selected, _)| {
                    let parameters = crate::symbol_resolver::specialized_function_params(
                        &selected,
                        &kinds,
                        &resolved_type_arguments,
                    );
                    let value_parameters = parameters.get(selected.context_count..)?.to_vec();
                    Some((selected, Some(value_parameters)))
                });
            let (selected, specialized_parameters) = visibility_override
                .or_else(|| {
                    resolver
                        .select_symbol(
                            crate::symbol_resolver::SymRecv::TopLevel,
                            spelling,
                            &kinds,
                            &resolved_type_arguments,
                        )
                        .and_then(|symbol| match symbol {
                            crate::symbol_resolver::Symbol::Member(facets) => {
                                let callable = facets.top_level_call?;
                                facets
                                    .overloads
                                    .iter()
                                    .find(|candidate| {
                                        candidate.callable.owner == callable.owner
                                            && candidate.callable.name == callable.name
                                            && candidate.callable.descriptor == callable.descriptor
                                    })
                                    .cloned()
                                    .and_then(|selected| {
                                        let parameters =
                                            crate::symbol_resolver::specialized_function_params(
                                                &selected,
                                                &kinds,
                                                &resolved_type_arguments,
                                            );
                                        let value_parameters =
                                            parameters.get(selected.context_count..)?.to_vec();
                                        Some((selected, Some(value_parameters)))
                                    })
                            }
                            crate::symbol_resolver::Symbol::Constructor(_)
                            | crate::symbol_resolver::Symbol::Instance(_)
                            | crate::symbol_resolver::Symbol::Companion(_) => None,
                        })
                })
                .or_else(|| {
                    let mut visible = candidates.iter().filter(|candidate| {
                        candidate.kind == crate::libraries::FnKind::TopLevel
                            && match candidate.visibility {
                                crate::types::Visibility::Public => true,
                                crate::types::Visibility::Internal => {
                                    candidate.source_key.is_some()
                                }
                                crate::types::Visibility::Private => candidate
                                    .source_key
                                    .is_some_and(|(file, _)| file == scope.source.raw()),
                                crate::types::Visibility::PackagePrivate
                                | crate::types::Visibility::Protected => false,
                            }
                    });
                    let selected = visible.next()?.clone();
                    visible.next().is_none().then(|| {
                        let parameters = crate::symbol_resolver::specialized_function_params(
                            &selected,
                            &kinds,
                            &resolved_type_arguments,
                        );
                        let value_parameters = parameters
                            .get(selected.context_count..)
                            .unwrap_or_default()
                            .to_vec();
                        (selected, Some(value_parameters))
                    })
                })?;
            Some({
                // `select_symbol` has already run the ordinary top-level generic inference
                // engine. Its callable parameters are the selected declaration's value
                // parameters after substitution; use them to contextualize postponed
                // arguments instead of rebuilding inference in the signature graph. The
                // unique-candidate branch is only a provisional shape used when postponed
                // arguments prevent applicability from being decided yet.
                let mut specialized_parameters = specialized_parameters
                    .unwrap_or_else(|| selected.semantic_params().into_owned());
                if let Some(generic) = selected.generic_sig.as_ref() {
                    let mut bindings = crate::symbol_resolver::seeded_gsig_binds(
                        generic,
                        &resolved_type_arguments,
                    );
                    let inferred = crate::symbol_resolver::infer_generic_call_bindings(
                        generic,
                        kinds.iter().enumerate().filter_map(|(parameter, argument)| {
                            let actual = argument.ty();
                            (actual != Ty::Error && !argument.is_omitted_default()).then_some((
                                parameter,
                                argument.type_for(
                                    generic.params.get(parameter).copied().unwrap_or(Ty::Error),
                                ),
                                argument.is_spread(),
                            ))
                        }),
                        selected.call_sig.vararg_index,
                    );
                    for (parameter, actual) in inferred {
                        bindings.entry(parameter).or_insert(actual);
                    }
                    specialized_parameters = selected
                        .semantic_params()
                        .iter()
                        .map(|parameter| {
                            crate::symbol_resolver::ty_subst_keep_unbound(*parameter, &bindings)
                        })
                        .collect();
                }
                crate::trace_compiler!(
                    "signature",
                    "call expectation specialization {spelling} type_arguments={resolved_type_arguments:?} arguments={kinds:?} parameters={specialized_parameters:?}",
                );
                let parameters =
                    Self::functional_parameter_shapes(resolver, &selected, specialized_parameters);
                crate::trace_compiler!(
                    "signature",
                    "call expectation {spelling} type_arguments={resolved_type_arguments:?} generic={:?} parameters={parameters:?}",
                    selected.generic_sig,
                );
                (parameters, slots, selected.call_sig.clone())
            })
        });
        let (parameters, slots, call_sig) = match parameters_and_slots {
            Ok(selected) => selected,
            // A bare call can also name a MEMBER of an implicit receiver
            // (`fun call() = higherOrder(::method)` inside the class declaring `higherOrder`).
            // Only top-level callables were consulted above, so such a call could never shape its
            // postponed lambda and the whole module's signatures declined.
            Err(_) => {
                // If ordinary callable selection found no function, the spelling may denote a
                // callable-valued top-level property. Select that property through the same
                // namespace lookup as `select_call`, demand its inferred declaration type when
                // necessary, and let the shared invoke-shape operation contextualize postponed
                // lambdas/references.
                if let Ok(property) = self.with_resolver(scope, |resolver| {
                    let crate::symbol_resolver::Symbol::Member(facets) = resolver.resolve_symbol(
                        crate::symbol_resolver::SymRecv::TopLevel,
                        spelling,
                        &[],
                        &[],
                    )?
                    else {
                        return None;
                    };
                    match facets.values.as_slice() {
                        [property] => Some(property.clone()),
                        [] | [_, _, ..] => None,
                    }
                }) {
                    let callee = match self.demanded_source_signature(
                        Some(scope),
                        property.stable_declaration,
                        demand,
                    )? {
                        Some(signature) => signature.result,
                        None => {
                            crate::fir::ResolvedTy::new(property.ty).map_err(|_| Self::failure())?
                        }
                    };
                    if let Ok(expectations) =
                        self.invoke_argument_expectations(scope, callee, arguments)
                    {
                        return Ok(expectations);
                    }
                }
                if let Some((owner, declared_name)) =
                    self.explicit_imported_classifier_callable(scope.source, spelling)
                {
                    if let Ok(expectations) = self.classifier_call_argument_expectations(
                        scope,
                        owner,
                        &declared_name,
                        arguments,
                        type_arguments,
                        trailing_lambda,
                    ) {
                        return Ok(expectations);
                    }
                }
                // A classifier constructor is an ordinary callable candidate, but it does not live
                // in the top-level function family above. Select its semantic declaration through
                // the normal constructor overload engine, including explicit classifier type
                // arguments, then use the shared argument mapper to place a trailing/named lambda.
                // This supplies `Delegate<A> { value -> ... }` with `(A) -> R` before the lambda is
                // materialized; no compact-graph overload engine is introduced here.
                let lexical_classifier = self
                    .qualified_classifier(scope, spelling)
                    .or_else(|| self.lexically_nested_classifier(scope, spelling));
                let constructor = lexical_classifier.or_else(|| {
                    self.with_resolver(scope, |resolver| {
                        match resolver.classifier_in_scope(spelling) {
                            crate::symbol_resolver::CandidateSelection::Selected(internal) => {
                                Some(internal)
                            }
                            crate::symbol_resolver::CandidateSelection::Ambiguous
                            | crate::symbol_resolver::CandidateSelection::None => None,
                        }
                    })
                    .ok()
                });
                if let Some(internal) = constructor {
                    if let Ok(expectations) = self.constructor_call_argument_expectations(
                        scope,
                        internal,
                        arguments,
                        &resolved_type_arguments,
                        trailing_lambda,
                    ) {
                        return Ok(expectations);
                    }
                    // `Outer.Nested(args)` first denotes the nested classifier's constructor
                    // family. If that family is inapplicable, the same classifier can still
                    // denote its companion singleton in value position, whose ordinary
                    // `operator invoke` supplies the next callable rung. Shape postponed
                    // arguments from that invoke family before folding `Outer` into a receiver;
                    // the latter would inspect the wrong classifier value.
                    if let Some(value) = self.classifier_value_type(internal) {
                        let callee =
                            crate::fir::ResolvedTy::new(value).map_err(|_| Self::failure())?;
                        if let Ok(expectations) =
                            self.invoke_argument_expectations(scope, callee, arguments)
                        {
                            return Ok(expectations);
                        }
                    }
                }
                if let Some((qualifier, member)) = spelling.rsplit_once('.') {
                    let classifier = self.qualified_classifier_or_source_alias(scope, qualifier);
                    crate::trace_compiler!(
                        "signature",
                        "call expectation qualified classifier qualifier={qualifier} member={member} classifier={classifier:?}",
                    );
                    if let Some(classifier) = classifier {
                        let expectations = self.classifier_call_argument_expectations(
                            scope,
                            classifier,
                            member,
                            arguments,
                            type_arguments,
                            trailing_lambda,
                        );
                        crate::trace_compiler!(
                            "signature",
                            "classifier call expectation qualifier={qualifier} member={member} selected={}",
                            expectations.is_ok(),
                        );
                        if let Ok(expectations) = expectations {
                            return Ok(expectations);
                        }
                    }
                    let receiver = self.qualified_receiver_ty(scope, qualifier, origin, demand)?;
                    return self.member_call_argument_expectations(
                        scope,
                        member,
                        origin,
                        receiver,
                        arguments,
                        type_arguments,
                        trailing_lambda,
                        None,
                        demand,
                    );
                }
                if let Ok(expectations) = self.bare_member_call_expectations(
                    scope,
                    spelling,
                    arguments,
                    &resolved_type_arguments,
                    trailing_lambda,
                    demand,
                ) {
                    return Ok(expectations);
                } else {
                    // The final call-selection tower next considers callables contributed by the
                    // lexically enclosing classifier. In particular, an enum's implicit
                    // `values`/`valueOf` functions are visible inside its companion even though
                    // neither is a companion member. Ask the same classifier candidate operation
                    // for postponed-argument shapes here; otherwise expectation materialization
                    // rejects the call before final selection can reach that rung.
                    for classifier in self.lexical_classifier_callable_owners(scope) {
                        if let Ok(expectations) = self.classifier_call_argument_expectations(
                            scope,
                            classifier,
                            spelling,
                            arguments,
                            type_arguments,
                            trailing_lambda,
                        ) {
                            return Ok(expectations);
                        }
                    }
                    // A bare inherited inner-class construction (`class C : A { fun f() =
                    // B(arg) }`) is bound to the same implicit receiver rung as `this.B(arg)`.
                    // Ordinary member functions at that rung were tried above; only then consult
                    // the classifier facet and reuse the constructor expectation operation.
                    for receiver in self
                        .implicit_receivers(scope)
                        .into_iter()
                        .chain(self.enclosing_lexical_singleton_receivers(scope))
                    {
                        let Some(internal) =
                            self.bound_inner_classifier(scope, receiver, spelling)?
                        else {
                            continue;
                        };
                        if let Ok(expectations) = self.constructor_call_argument_expectations(
                            scope,
                            internal,
                            arguments,
                            &resolved_type_arguments,
                            trailing_lambda,
                        ) {
                            return Ok(expectations);
                        }
                    }
                    return Err(Self::failure());
                }
            }
        };
        if call_sig.param_names.is_empty()
            && call_sig.param_defaults.is_empty()
            && call_sig.vararg_index.is_none()
        {
            return Ok(Self::postponed_expectations(arguments, &slots, &parameters));
        }
        Self::postponed_call_expectations(arguments, &parameters, &call_sig, trailing_lambda)
            .ok_or_else(Self::failure)
    }

    fn select_callable_reference(
        &self,
        scope: crate::fir::SignatureScope,
        spelling: &str,
        origin: crate::fir::OriginId,
        expected: Option<crate::fir::ResolvedTy>,
        demand: &mut dyn FnMut(
            crate::fir::DeclarationId,
        )
            -> Result<crate::fir::ResolvedSignature, crate::fir::DiagnosticId>,
    ) -> Result<crate::fir::ResolvedTy, crate::fir::DiagnosticId> {
        // Receiver-less `::x` binds at the first applicable implicit receiver rung.
        for receiver in self
            .implicit_receivers(scope)
            .into_iter()
            .chain(self.enclosing_lexical_singleton_receivers(scope))
        {
            let receiver = crate::fir::ResolvedTy::new(receiver).map_err(|_| Self::failure())?;
            if let Ok(selected) = self.select_bound_callable_reference(
                scope, spelling, origin, receiver, false, expected, demand,
            ) {
                return Ok(selected);
            }
        }
        if let Some(expected_ty) = expected {
            let expected_function = match expected_ty.get().non_null() {
                Ty::Fun(_) => Some(expected_ty.get().non_null()),
                nominal => crate::symbol_resolver::classifier_callable_signature(
                    &*self.table.libraries,
                    nominal,
                ),
            };
            if let Some(selected) =
                self.classifier_constructor_reference(scope, spelling, expected_ty)
            {
                return Ok(selected);
            }
            let Some(Ty::Fun(expected)) = expected_function else {
                // A non-functional expectation cannot shape a reference; infer its natural type.
                return self.select_callable_reference(scope, spelling, origin, None, demand);
            };
            if let Ok(property) = self.with_resolver(scope, |resolver| {
                let crate::symbol_resolver::Symbol::Member(facets) = resolver.resolve_symbol(
                    crate::symbol_resolver::SymRecv::TopLevel,
                    spelling,
                    &[],
                    &[],
                )?
                else {
                    return None;
                };
                let properties = facets
                    .values
                    .into_iter()
                    .filter(|property| {
                        property.kind == crate::libraries::PropKind::TopLevel
                            && property.context_count == 0
                    })
                    .collect::<Vec<_>>();
                match properties.as_slice() {
                    [property] => Some(property.clone()),
                    [] | [_, _, ..] => None,
                }
            }) {
                let result = match self.demanded_source_signature(
                    Some(scope),
                    property.stable_declaration,
                    demand,
                )? {
                    Some(signature) => signature.result.get(),
                    None => property.ty,
                };
                if let Some(contextual) =
                    self.contextual_callable_reference_type(scope, &[], result, Some(expected_ty))?
                {
                    return Ok(contextual);
                }
            }
            let candidates = self.with_resolver(scope, |resolver| {
                Some(resolver.top_level_candidates(spelling))
            })?;
            let mut candidates_with_finalized_signatures = Vec::with_capacity(candidates.len());
            for mut candidate in candidates {
                let signature = match self.demanded_source_signature(
                    Some(scope),
                    candidate.stable_declaration,
                    demand,
                )? {
                    Some(signature) => Some(signature),
                    None => self.demanded_member_signature(candidate.stable_declaration, demand)?,
                };
                if let Some(signature) = signature {
                    let parameters = signature
                        .parameters
                        .iter()
                        .map(|parameter| parameter.get())
                        .collect::<Vec<_>>();
                    let result = signature.result.get();
                    if let Some(generic) = candidate.generic_sig.as_mut() {
                        generic.params = parameters;
                        generic.ret = result;
                    } else {
                        candidate.callable.params = parameters;
                        candidate.callable.ret = result;
                    }
                }
                candidates_with_finalized_signatures.push(candidate);
            }
            let module =
                crate::module_symbols::ModuleSymbols::for_file(self.table, scope.source.raw());
            let source = crate::symbol_source::CompositeSource::new(vec![
                &module as &dyn crate::symbol_source::SymbolSource,
                &*self.table.libraries as &dyn crate::symbol_source::SymbolSource,
            ]);
            let oracle = crate::symbol_resolver::SourceOracle(&source);
            let context = crate::assignable::TyCtx::new();
            let selected = candidates_with_finalized_signatures
                .into_iter()
                .filter(|candidate| {
                    candidate.kind == crate::libraries::FnKind::TopLevel
                        || candidate.callable.singleton_dispatch.is_some()
                })
                .filter(|candidate| !candidate.callable.suspend || expected.suspend)
                .filter_map(|mut candidate| {
                    let specialized = super::super::callable_reference_selection::specialize_candidate(
                        &source,
                        &mut candidate,
                        None,
                        Some(&expected.params),
                        Some(expected.ret),
                        |actual, bound| {
                            crate::assignable::is_subtype(&context, &oracle, actual, bound)
                        },
                    );
                    if specialized
                        != super::super::callable_reference_selection::CallableRefSpecialization::Specialized
                    {
                        return None;
                    }
                    super::super::callable_reference_selection::parameter_plan(
                        &candidate.callable.params,
                        &candidate.call_sig,
                        &expected.params,
                        |actual, target| {
                            crate::assignable::is_subtype(&context, &oracle, actual, target)
                        },
                    )?;
                    if !super::super::callable_reference_selection::is_compatible(
                        &expected.params,
                        candidate.callable.ret,
                        candidate.callable.suspend,
                        expected,
                        true,
                        |actual, target| {
                            crate::assignable::is_subtype(&context, &oracle, actual, target)
                        },
                    ) {
                        return None;
                    }
                    let mut parameters = expected.params.clone();
                    for (parameter, declared) in
                        parameters.iter_mut().zip(&candidate.callable.params)
                    {
                        if matches!(parameter.non_null(), Ty::TyParam(..)) {
                            *parameter = *declared;
                        }
                    }
                    let result = if matches!(expected.ret.non_null(), Ty::TyParam(..)) {
                        candidate.callable.ret
                    } else {
                        expected.ret
                    };
                    Some(Ty::fun_with_shape(
                        parameters,
                        result,
                        expected.context_count,
                        expected.has_receiver,
                        expected.suspend,
                    ))
                })
                .collect::<Vec<_>>();
            let selected = selected.into_iter().fold(Vec::new(), |mut unique, ty| {
                if !unique.contains(&ty) {
                    unique.push(ty);
                }
                unique
            });
            let [selected] = selected.as_slice() else {
                return Err(Self::failure());
            };
            return crate::fir::ResolvedTy::new(*selected).map_err(|_| Self::failure());
        }
        // A receiver-less property reference (`::x`) is selected from the same import-scope
        // property candidates as an ordinary value read. Its reflective result type depends on the
        // property's semantic type, so demand an inferred source property before constructing
        // `KProperty0<T>`/`KMutableProperty0<T>`.
        if let Ok(property) = self.with_resolver(scope, |resolver| {
            let crate::symbol_resolver::Symbol::Member(facets) = resolver.resolve_symbol(
                crate::symbol_resolver::SymRecv::TopLevel,
                spelling,
                &[],
                &[],
            )?
            else {
                return None;
            };
            let properties = facets
                .values
                .into_iter()
                .filter(|property| {
                    property.kind == crate::libraries::PropKind::TopLevel
                        && property.context_count == 0
                })
                .collect::<Vec<_>>();
            match properties.as_slice() {
                [property] => Some(property.clone()),
                [] | [_, _, ..] => None,
            }
        }) {
            let result = match self.demanded_source_signature(
                Some(scope),
                property.stable_declaration,
                demand,
            )? {
                Some(signature) => signature.result.get(),
                None => property.ty,
            };
            let ty = self
                .table
                .libraries
                .property_reference_type(0, property.setter.is_some(), &[result])
                .ok_or_else(Self::failure)?;
            return crate::fir::ResolvedTy::new(ty).map_err(|_| Self::failure());
        }
        let selected = self.with_resolver(scope, |resolver| {
            let crate::symbol_resolver::Symbol::Member(facets) = resolver.resolve_symbol(
                crate::symbol_resolver::SymRecv::TopLevel,
                spelling,
                &[],
                &[],
            )?
            else {
                return None;
            };
            let candidates = facets
                .overloads
                .into_iter()
                .filter(|candidate| candidate.kind == crate::libraries::FnKind::TopLevel)
                .collect::<Vec<_>>();
            match candidates.as_slice() {
                [selected] => Some(selected.clone()),
                [] | [_, _, ..] => None,
            }
        });
        let selected = match selected {
            Ok(selected) => selected,
            Err(failure) => {
                // `::A` names a CLASS, not a function: the reference is to its constructor, whose
                // type is `(constructor parameters) -> A`. Constructor parameters always carry
                // declared types, so this needs no demand-driven inference.
                let classifier = self.with_resolver(scope, |resolver| {
                    match resolver.classifier_in_scope(spelling) {
                        crate::symbol_resolver::CandidateSelection::Selected(classifier) => {
                            Some(classifier)
                        }
                        crate::symbol_resolver::CandidateSelection::Ambiguous
                        | crate::symbol_resolver::CandidateSelection::None => None,
                    }
                });
                let Ok(classifier) = classifier else {
                    return Err(failure);
                };
                let Some(declaration) = self.table.classes.get(&classifier) else {
                    return Err(failure);
                };
                let module =
                    crate::module_symbols::ModuleSymbols::for_file(self.table, scope.source.raw());
                let source = crate::symbol_source::CompositeSource::new(vec![
                    &module as &dyn crate::symbol_source::SymbolSource,
                    &*self.table.libraries as &dyn crate::symbol_source::SymbolSource,
                ]);
                if let Some(sam) = crate::symbol_resolver::semantic_sam_signature(
                    &source,
                    Ty::obj_name(classifier),
                ) {
                    let operand = Ty::fun_with_shape(
                        sam.params,
                        sam.ret,
                        sam.context_count,
                        sam.has_receiver,
                        sam.suspend,
                    );
                    return crate::fir::ResolvedTy::new(Ty::obj_args(
                        "kotlin/reflect/KFunction1",
                        &[operand, Ty::obj_name(classifier)],
                    ))
                    .map_err(|_| Self::failure());
                }
                // With no expected type, a callable reference has a REFLECTIVE type
                // (`KFunction1<P, A>`), not a plain function type. Publishing the plain shape makes
                // every use site cast a lambda to `kotlin.reflect.KFunction` and fail at run time.
                let mut arguments = declaration.ctor_params.clone();
                arguments.push(Ty::obj_name(classifier));
                let reflective =
                    format!("kotlin/reflect/KFunction{}", declaration.ctor_params.len());
                return crate::fir::ResolvedTy::new(Ty::obj_args(&reflective, &arguments))
                    .map_err(|_| Self::failure());
            }
        };
        let (parameters, result) =
            match self.demanded_source_signature(None, selected.stable_declaration, demand)? {
                Some(signature) => (
                    signature
                        .parameters
                        .iter()
                        .map(|parameter| parameter.get())
                        .collect::<Vec<_>>(),
                    signature.result.get(),
                ),
                None => {
                    if selected
                        .generic_sig
                        .as_ref()
                        .is_some_and(|signature| !signature.formals.is_empty())
                    {
                        return Err(Self::failure());
                    }
                    let mut parameters = selected.semantic_params().into_owned();
                    if let Some(receiver) = selected.semantic_receiver() {
                        parameters.insert(selected.context_count.min(parameters.len()), receiver);
                    }
                    (parameters, selected.callable.ret)
                }
            };
        crate::fir::ResolvedTy::new(Ty::fun_with_shape(
            parameters,
            result,
            selected.context_count,
            selected.is_extension(),
            selected.callable.suspend,
        ))
        .map_err(|_| Self::failure())
    }

    fn select_bound_callable_reference(
        &self,
        scope: crate::fir::SignatureScope,
        spelling: &str,
        _origin: crate::fir::OriginId,
        receiver: crate::fir::ResolvedTy,
        unbound: bool,
        expected: Option<crate::fir::ResolvedTy>,
        demand: &mut dyn FnMut(
            crate::fir::DeclarationId,
        )
            -> Result<crate::fir::ResolvedSignature, crate::fir::DiagnosticId>,
    ) -> Result<crate::fir::ResolvedTy, crate::fir::DiagnosticId> {
        // `Outer::Nested` and `Outer::Inner` select a nested classifier's constructor through the
        // same semantic nested-classifier lookup as ordinary resolution. An unbound inner
        // constructor carries the outer instance as its leading callable parameter; a reference on
        // an outer value has already bound that receiver.
        let nested = self.with_resolver(scope, |resolver| {
            match resolver.nested_classifier(receiver.get(), spelling) {
                crate::symbol_resolver::CandidateSelection::Selected(classifier) => {
                    Some(classifier)
                }
                crate::symbol_resolver::CandidateSelection::None
                | crate::symbol_resolver::CandidateSelection::Ambiguous => None,
            }
        });
        crate::trace_compiler!(
            "callable_ref",
            "signature nested constructor probe receiver={:?} spelling={spelling} selected={nested:?} declaration={:?}",
            receiver.get(),
            nested
                .as_ref()
                .ok()
                .and_then(|nested| self.table.class_by_type_name(*nested))
                .map(|classifier| (
                    classifier.is_interface(),
                    classifier.is_object(),
                    classifier.is_abstract(),
                    classifier.is_annotation(),
                    classifier.type_params.clone(),
                    classifier.captured_type_parameters.type_params.clone(),
                )),
        );
        if let Ok(nested) = nested {
            if let Some(classifier) = self.table.class_by_type_name(nested).filter(|classifier| {
                !classifier.is_interface()
                    && !classifier.is_object()
                    && !classifier.is_abstract()
                    && !classifier.is_annotation()
            }) {
                let module =
                    crate::module_symbols::ModuleSymbols::for_file(self.table, scope.source.raw());
                let source = crate::symbol_source::CompositeSource::new(vec![
                    &module as &dyn crate::symbol_source::SymbolSource,
                    &*self.table.libraries as &dyn crate::symbol_source::SymbolSource,
                ]);
                let declaration = crate::symbol_source::SymbolSource::classifier(&source, nested)
                    .ok_or_else(Self::failure)?;
                let expected_function =
                    expected.and_then(|expected| match expected.get().non_null() {
                        Ty::Fun(function) => Some(function),
                        _ => None,
                    });
                let constructor_arguments = expected_function.map_or_else(
                    || classifier.ctor_params.clone(),
                    |function| {
                        if unbound && classifier.inner_of_name().is_some() {
                            function.params.get(1..).unwrap_or_default().to_vec()
                        } else {
                            function.params.clone()
                        }
                    },
                );
                let constructor_argument_kinds = constructor_arguments
                    .iter()
                    .copied()
                    .map(crate::symbol_resolver::CallArgKind::Typed)
                    .collect::<Vec<_>>();
                let selected = crate::symbol_resolver::select_constructor_declaration_from_type(
                    &*self.table.libraries,
                    &source,
                    &declaration,
                    &constructor_argument_kinds,
                )
                .or_else(|| {
                    // An enclosing SAM/call candidate may expose its own still-open type
                    // parameters as this reference's expected inputs. They are inference slots,
                    // so ordinary call applicability cannot treat them as concrete argument types.
                    // Use the shared callable-reference mapper and accept only one applicable
                    // constructor declaration; ambiguity remains a frontend failure.
                    let oracle = crate::symbol_resolver::SourceOracle(&source);
                    let context = crate::assignable::TyCtx::new();
                    let applicable = declaration
                        .constructors
                        .iter()
                        .filter(|constructor| {
                            super::super::callable_reference_selection::parameter_plan(
                                &constructor.params,
                                &constructor.call_sig,
                                &constructor_arguments,
                                |actual, target| {
                                    crate::assignable::is_subtype(&context, &oracle, actual, target)
                                },
                            )
                            .is_some()
                        })
                        .collect::<Vec<_>>();
                    match applicable.as_slice() {
                        [constructor] => Some((*constructor).clone()),
                        [] | [_, _, ..] => None,
                    }
                })
                .ok_or_else(Self::failure)?;
                let mut arguments =
                    crate::symbol_resolver::infer_constructor_type_args_for_formals(
                        nested,
                        &declaration,
                        &classifier.type_params,
                        &constructor_arguments,
                        expected_function.map(|function| function.ret),
                    )
                    .unwrap_or_else(|| {
                        declaration
                            .type_params()
                            .iter()
                            .enumerate()
                            .map(|(index, parameter)| {
                                declaration
                                    .type_param_bounds()
                                    .get(index)
                                    .and_then(|bounds| bounds.first())
                                    .copied()
                                    .unwrap_or_else(|| {
                                        Ty::ty_param(parameter, Ty::nullable(Ty::obj("kotlin/Any")))
                                    })
                            })
                            .collect()
                    });
                let own = classifier.type_params.len();
                let captured = &classifier.captured_type_parameters;
                arguments.extend((arguments.len()..own + captured.type_params.len()).map(
                    |index| {
                        let captured_index = index.saturating_sub(own);
                        Ty::ty_param(
                            &captured.type_params[captured_index],
                            captured
                                .type_param_bounds
                                .get(captured_index)
                                .copied()
                                .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any"))),
                        )
                    },
                ));
                let mut outer_bindings = None;
                if let Some(outer) = classifier.inner_of_name().and_then(|outer| {
                    self.table
                        .class_by_type_name(outer)
                        .map(|declaration| declaration.type_parameter_bindings(receiver.get()))
                }) {
                    for (index, captured) in classifier
                        .captured_type_parameters
                        .type_params
                        .iter()
                        .enumerate()
                    {
                        if let Some(argument) = outer.get(captured).copied() {
                            if let Some(slot) = arguments.get_mut(own + index) {
                                *slot = argument;
                            }
                        }
                    }
                    outer_bindings = Some(outer);
                }
                crate::trace_compiler!(
                    "signature",
                    "nested constructor reference classifier={nested:?} receiver={:?} own={} captured={:?} outer_bindings={outer_bindings:?} arguments={arguments:?}",
                    receiver.get(),
                    own,
                    classifier.captured_type_parameters.type_params,
                );
                let bindings = declaration
                    .type_params()
                    .iter()
                    .cloned()
                    .zip(arguments.iter().copied())
                    .collect::<crate::symbol_resolver::GSigBinds>();
                let declared_parameters = selected
                    .generic_sig
                    .as_ref()
                    .map_or(selected.params.as_slice(), |signature| {
                        signature.params.as_slice()
                    });
                let mut parameters = declared_parameters
                    .iter()
                    .map(|parameter| {
                        crate::symbol_resolver::ty_subst_keep_unbound(*parameter, &bindings)
                    })
                    .collect::<Vec<_>>();
                if unbound && classifier.inner_of_name().is_some() {
                    parameters.insert(0, receiver.get());
                }
                let result = Ty::obj_args_name(nested, &arguments);
                let function = self
                    .contextual_callable_reference_type(scope, &parameters, result, expected)?
                    .map(crate::fir::ResolvedTy::get)
                    .unwrap_or_else(|| Ty::fun(parameters, result));
                let ty = if unbound {
                    self.table
                        .libraries
                        .function_reference_type(function)
                        .unwrap_or(function)
                } else {
                    function
                };
                return crate::fir::ResolvedTy::new(ty).map_err(|_| Self::failure());
            }
        }
        if unbound {
            if let Some(property) = receiver.get().non_null().obj_internal().and_then(|owner| {
                self.table
                    .libraries
                    .classifier_associated_property(owner, spelling)
            }) {
                if expected.is_some_and(|expected| matches!(expected.get().non_null(), Ty::Fun(_)))
                {
                    if let Some(contextual) =
                        self.contextual_callable_reference_type(scope, &[], property.ty, expected)?
                    {
                        return Ok(contextual);
                    }
                }
                let natural = self
                    .table
                    .libraries
                    .property_reference_type(0, property.setter.is_some(), &[property.ty])
                    .ok_or_else(Self::failure)?;
                return crate::fir::ResolvedTy::new(natural).map_err(|_| Self::failure());
            }
        }
        let unbound = unbound
            && receiver
                .get()
                .non_null()
                .obj_internal()
                .is_none_or(|classifier| !self.classifier_is_singleton(classifier));
        let (property, mut candidates) = self.with_resolver(scope, |resolver| {
            let property = resolver
                .resolve_symbol(
                    crate::symbol_resolver::SymRecv::Value(receiver.get()),
                    spelling,
                    &[],
                    &[],
                )
                .and_then(|symbol| match symbol {
                    crate::symbol_resolver::Symbol::Member(facets) => facets
                        .property_ref
                        .clone()
                        .or_else(|| facets.extension_property_ref()),
                    crate::symbol_resolver::Symbol::Instance(_)
                    | crate::symbol_resolver::Symbol::Companion(_)
                    | crate::symbol_resolver::Symbol::Constructor(_) => None,
                });
            // A callable reference selects from the overload FAMILY under its contextual function
            // shape. Asking `resolve_symbol(..., [])` for that family prematurely selects a
            // zero-argument call and drops source members whose return is still demand-driven.
            let candidates = resolver
                .receiver_callables(receiver.get(), spelling)
                .functions()
                .iter()
                .filter(|candidate| {
                    matches!(
                        candidate.kind,
                        crate::libraries::FnKind::Member | crate::libraries::FnKind::Extension
                    )
                })
                .cloned()
                .collect::<Vec<_>>();
            (property.is_some() || !candidates.is_empty()).then_some((property, candidates))
        })?;
        if let Some(property) = property {
            let result = match property.stable_declaration {
                Some(declaration)
                    if self.headers.stubs.iter().any(|stub| {
                        stub.id == declaration && stub.signature_inference.is_some()
                    }) =>
                {
                    demand(declaration)?.result.get()
                }
                _ => property.prop_ty,
            };
            let natural_parameters = if unbound {
                vec![receiver.get()]
            } else {
                Vec::new()
            };
            let arguments = if unbound {
                vec![receiver.get(), result]
            } else {
                vec![result]
            };
            let natural = self
                .table
                .libraries
                .property_reference_type(
                    usize::from(unbound),
                    property.setter.is_some(),
                    &arguments,
                )
                .ok_or_else(Self::failure)?;
            if expected.is_some_and(|expected| matches!(expected.get().non_null(), Ty::Fun(_))) {
                if let Some(contextual) = self.contextual_callable_reference_type(
                    scope,
                    &natural_parameters,
                    result,
                    expected,
                )? {
                    return Ok(contextual);
                }
            }
            if let Some(expected) = expected {
                let module =
                    crate::module_symbols::ModuleSymbols::for_file(self.table, scope.source.raw());
                let source = crate::symbol_source::CompositeSource::new(vec![
                    &module as &dyn crate::symbol_source::SymbolSource,
                    &*self.table.libraries as &dyn crate::symbol_source::SymbolSource,
                ]);
                let oracle = crate::symbol_resolver::SourceOracle(&source);
                if !crate::assignable::is_subtype(
                    &crate::assignable::TyCtx::new(),
                    &oracle,
                    natural,
                    expected.get(),
                ) {
                    return Err(Self::failure());
                }
                return Ok(expected);
            }
            return crate::fir::ResolvedTy::new(natural).map_err(|_| Self::failure());
        }
        if let Some(expected) = expected {
            let Ty::Fun(expected_function) = expected.get().non_null() else {
                return Err(Self::failure());
            };
            // Member candidates occupy the earlier scope-tower rung. Select them with the same
            // adapted-reference algorithm as checked bodies before touching extensions: an
            // extension currently being inferred must not create a false cycle when an applicable
            // declared member already wins (`this::contains` inside `Foo.contains(vararg ...)`).
            let mut members = candidates
                .iter()
                .filter(|candidate| candidate.kind == crate::libraries::FnKind::Member)
                .cloned()
                .collect::<Vec<_>>();
            for candidate in &mut members {
                if let Some(signature) =
                    self.demanded_member_signature(candidate.stable_declaration, demand)?
                {
                    candidate.callable.ret = signature.result.get();
                }
            }
            let module =
                crate::module_symbols::ModuleSymbols::for_file(self.table, scope.source.raw());
            let source = crate::symbol_source::CompositeSource::new(vec![
                &module as &dyn crate::symbol_source::SymbolSource,
                &*self.table.libraries as &dyn crate::symbol_source::SymbolSource,
            ]);
            let oracle = crate::symbol_resolver::SourceOracle(&source);
            let context = crate::assignable::TyCtx::new();
            let selected =
                super::super::callable_reference_selection::select_adapted_instance_candidate(
                    &source,
                    &members,
                    unbound.then(|| receiver.get()),
                    expected_function,
                    |actual, target| {
                        crate::assignable::is_subtype(&context, &oracle, actual, target)
                    },
                    |left_params: &[Ty], left_ret, right_params: &[Ty], right_ret| {
                        left_params.len() == right_params.len()
                            && left_params.iter().zip(right_params).all(|(left, right)| {
                                crate::assignable::is_subtype(&context, &oracle, *left, *right)
                            })
                            && crate::assignable::is_subtype(&context, &oracle, left_ret, right_ret)
                    },
                );
            if let Some((selected, argument_mapping, _type_arguments)) = selected {
                let (function, _) =
                    super::super::callable_reference_selection::realize_adapted_instance_shape(
                        expected_function,
                        unbound.then(|| receiver.get()),
                        &selected.semantic_params(),
                        selected.callable.ret,
                        &argument_mapping,
                    );
                return crate::fir::ResolvedTy::new(function).map_err(|_| Self::failure());
            }

            // Only after the member rung has no applicable candidate may extension candidates be
            // forced. Their inferred returns can legitimately depend on other declarations.
            candidates.retain(|candidate| candidate.kind == crate::libraries::FnKind::Extension);
            for candidate in &mut candidates {
                if let Some(signature) =
                    self.demanded_source_signature(None, candidate.stable_declaration, demand)?
                {
                    candidate.callable.ret = signature.result.get();
                }
            }
        }
        // A natural callable-reference type still obeys the scope tower. Declared members form an
        // earlier rung than extensions, so the presence of both must not be reported as an
        // ambiguity merely because no expected function shape was available to filter them.
        if candidates
            .iter()
            .any(|candidate| candidate.kind == crate::libraries::FnKind::Member)
        {
            candidates.retain(|candidate| candidate.kind == crate::libraries::FnKind::Member);
        }
        let selected = match candidates.as_slice() {
            [selected] => selected.clone(),
            [] | [_, _, ..] => return Err(Self::failure()),
        };
        let demanded = match self.demanded_member_signature(selected.stable_declaration, demand)? {
            Some(signature) => Some(signature),
            None => self.demanded_source_signature(None, selected.stable_declaration, demand)?,
        };
        let (parameters, result) = demanded.map_or_else(
            || {
                (
                    selected.semantic_params().into_owned(),
                    selected.callable.ret,
                )
            },
            |signature| {
                (
                    signature
                        .parameters
                        .iter()
                        .map(|parameter| parameter.get())
                        .collect(),
                    signature.result.get(),
                )
            },
        );
        let mut bindings = crate::symbol_resolver::GSigBinds::new();
        if selected.kind == crate::libraries::FnKind::Member {
            let owner = selected.callable.owner_type();
            if let Some((_, applied, _)) = self
                .table
                .applied_hierarchy(receiver.get())
                .into_iter()
                .find(|(candidate, _, _)| *candidate == owner)
            {
                if let Some(classifier) = self.table.class_by_type_name(owner) {
                    bindings.extend(classifier.type_parameter_bindings(applied));
                }
            }
        }
        if let Some(declared) = selected.semantic_receiver() {
            crate::symbol_resolver::unify_inferred_ty(declared, receiver.get(), &mut bindings);
        }
        let mut parameters = parameters
            .into_iter()
            .map(|parameter| crate::symbol_resolver::ty_subst_keep_unbound(parameter, &bindings))
            .collect::<Vec<_>>();
        if unbound {
            parameters.insert(0, receiver.get());
        }
        let result = crate::symbol_resolver::ty_subst_keep_unbound(result, &bindings);
        let function = Ty::fun_with_shape(
            parameters,
            result,
            selected.context_count,
            unbound && selected.is_extension(),
            selected.callable.suspend,
        );
        let published = if expected.is_none() {
            self.table
                .libraries
                .function_reference_type(function)
                .unwrap_or(function)
        } else {
            function
        };
        crate::fir::ResolvedTy::new(published).map_err(|_| Self::failure())
    }

    fn select_lateinit_initialized(
        &self,
        scope: crate::fir::SignatureScope,
        spelling: &str,
        _origin: crate::fir::OriginId,
        receiver: Option<crate::fir::ResolvedTy>,
        _unbound: bool,
        _demand: &mut dyn FnMut(
            crate::fir::DeclarationId,
        )
            -> Result<crate::fir::ResolvedSignature, crate::fir::DiagnosticId>,
    ) -> Result<crate::fir::ResolvedTy, crate::fir::DiagnosticId> {
        let is_lateinit = |declaration: Option<crate::fir::DeclarationId>| {
            declaration.is_some_and(|declaration| {
                self.headers.stubs.iter().any(|stub| {
                    stub.id == declaration && stub.flags.has(crate::fir::DeclarationFlags::LATEINIT)
                })
            })
        };
        let property_on_receiver = |receiver: Ty| {
            self.with_resolver(scope, |resolver| {
                let crate::symbol_resolver::Symbol::Member(facets) = resolver.resolve_symbol(
                    crate::symbol_resolver::SymRecv::Value(receiver),
                    spelling,
                    &[],
                    &[],
                )?
                else {
                    return None;
                };
                let property = facets
                    .property_ref
                    .clone()
                    .or_else(|| facets.extension_property_ref())?;
                is_lateinit(property.stable_declaration).then_some(property.stable_declaration)
            })
            .ok()
            .flatten()
        };

        let selected = match receiver {
            Some(receiver) => property_on_receiver(receiver.get()),
            None => self
                .implicit_receivers(scope)
                .into_iter()
                .find_map(property_on_receiver)
                .or_else(|| {
                    self.with_resolver(scope, |resolver| {
                        let crate::symbol_resolver::Symbol::Member(facets) = resolver
                            .resolve_symbol(
                                crate::symbol_resolver::SymRecv::TopLevel,
                                spelling,
                                &[],
                                &[],
                            )?
                        else {
                            return None;
                        };
                        let properties = facets
                            .values
                            .iter()
                            .filter(|property| {
                                property.kind == crate::libraries::PropKind::TopLevel
                                    && property.context_count == 0
                                    && is_lateinit(property.stable_declaration)
                            })
                            .collect::<Vec<_>>();
                        match properties.as_slice() {
                            [property] => property.stable_declaration,
                            [] | [_, _, ..] => None,
                        }
                    })
                    .ok()
                }),
        };
        selected.ok_or_else(Self::failure)?;
        crate::fir::ResolvedTy::new(Ty::Boolean).map_err(|_| Self::failure())
    }

    fn class_literal_type(
        &self,
        receiver: crate::fir::ResolvedTy,
        unbound: bool,
    ) -> Result<crate::fir::ResolvedTy, crate::fir::DiagnosticId> {
        let receiver = receiver.get();
        if !unbound && !receiver.is_reference() && receiver.jvm_boxed_ref().is_none() {
            return Err(Self::failure());
        }
        let base = self
            .table
            .libraries
            .class_literal_type()
            .ok_or_else(Self::failure)?;
        let result = if unbound {
            super::super::parameterized_class_literal_type(base, receiver)
        } else {
            base
        };
        crate::fir::ResolvedTy::new(result).map_err(|_| Self::failure())
    }

    fn class_literal_receiver_is_value(
        &self,
        scope: crate::fir::SignatureScope,
        root: &str,
    ) -> Result<bool, crate::fir::DiagnosticId> {
        crate::trace_compiler!(
            "signature",
            "class_literal_receiver_is_value root={root} receivers={:?}",
            self.implicit_receivers(scope),
        );
        if root == "this" {
            return Ok(!self.implicit_receivers(scope).is_empty());
        }
        for receiver in self.implicit_receivers(scope) {
            if self
                .with_resolver(scope, |resolver| {
                    resolver
                        .resolve_symbol(
                            crate::symbol_resolver::SymRecv::Value(receiver),
                            root,
                            &[],
                            &[],
                        )
                        .and_then(crate::symbol_resolver::Symbol::value)
                })
                .is_ok()
            {
                return Ok(true);
            }
        }
        if let Ok(classifier) =
            self.with_resolver(scope, |resolver| match resolver.classifier_in_scope(root) {
                crate::symbol_resolver::CandidateSelection::Selected(classifier) => {
                    Some(classifier)
                }
                crate::symbol_resolver::CandidateSelection::Ambiguous
                | crate::symbol_resolver::CandidateSelection::None => None,
            })
        {
            if self.classifier_is_singleton(classifier) {
                return Ok(true);
            }
        }
        Ok(self
            .with_resolver(scope, |resolver| {
                resolver
                    .resolve_symbol(crate::symbol_resolver::SymRecv::TopLevel, root, &[], &[])
                    .and_then(crate::symbol_resolver::Symbol::value)
            })
            .is_ok())
    }

    fn callable_reference_receiver_is_value(
        &self,
        scope: crate::fir::SignatureScope,
        root: &str,
        target: &str,
    ) -> Result<bool, crate::fir::DiagnosticId> {
        if root == "this"
            || self.enclosing_constructor_parameter(scope, root).is_some()
            || self.enclosing_capture_type(scope, root).is_some()
            || self.enclosing_enum_entry_property(scope, root).is_some()
        {
            return Ok(true);
        }

        // The first segment obeys the ordinary value scope tower. Query only declaration-valued
        // receiver facets here: `resolve_symbol(Value, root)` also exposes a same-named singleton
        // classifier value, which is precisely the classifier/value ambiguity this operation must
        // postpone until it has inspected the referenced declaration.
        for receiver in self.implicit_receivers(scope) {
            let selected = self.with_resolver(scope, |resolver| {
                resolver
                    .select_member_property(receiver, root)
                    .map(|_| ())
                    .or_else(|| {
                        resolver
                            .select_extension_property(receiver, root)
                            .ok()
                            .flatten()
                            .map(|_| ())
                    })
            });
            if selected.is_ok() {
                return Ok(true);
            }
        }
        let top_level_value = self.with_resolver(scope, |resolver| {
            resolver
                .resolve_symbol(crate::symbol_resolver::SymRecv::TopLevel, root, &[], &[])
                .map(crate::symbol_resolver::Symbol::values)
                .filter(|properties| {
                    properties
                        .iter()
                        .any(|property| property.kind == crate::libraries::PropKind::TopLevel)
                })
                .map(|_| ())
        });
        crate::trace_compiler!(
            "callable_ref",
            "qualified callable-reference root={root} target={target} top_level_value={}",
            top_level_value.is_ok(),
        );
        if top_level_value.is_ok() {
            return Ok(true);
        }

        let classifier = self.lexically_nested_classifier(scope, root).or_else(|| {
            self.with_resolver(scope, |resolver| match resolver.classifier_in_scope(root) {
                crate::symbol_resolver::CandidateSelection::Selected(classifier) => {
                    Some(classifier)
                }
                crate::symbol_resolver::CandidateSelection::Ambiguous
                | crate::symbol_resolver::CandidateSelection::None => None,
            })
            .ok()
        });
        let Some(classifier) = classifier else {
            crate::trace_compiler!(
                "callable_ref",
                "qualified callable-reference root={root} target={target} classifier=none",
            );
            return Ok(false);
        };
        let nested = self.with_resolver(scope, |resolver| {
            match resolver.nested_classifier(Ty::obj_name(classifier), target) {
                crate::symbol_resolver::CandidateSelection::Selected(nested) => Some(nested),
                crate::symbol_resolver::CandidateSelection::Ambiguous
                | crate::symbol_resolver::CandidateSelection::None => None,
            }
        });
        if nested.is_ok() {
            crate::trace_compiler!(
                "callable_ref",
                "qualified callable-reference root={root} target={target} classifier={classifier:?} nested=true",
            );
            return Ok(false);
        }
        crate::trace_compiler!(
            "callable_ref",
            "qualified callable-reference root={root} target={target} classifier={classifier:?} nested=false singleton={}",
            self.classifier_is_singleton(classifier),
        );
        Ok(self.classifier_is_singleton(classifier))
    }

    fn select_member(
        &self,
        scope: crate::fir::SignatureScope,
        spelling: &str,
        origin: crate::fir::OriginId,
        receiver: crate::fir::ResolvedTy,
        _expected: Option<crate::fir::ResolvedTy>,
        demand: &mut dyn FnMut(
            crate::fir::DeclarationId,
        )
            -> Result<crate::fir::ResolvedSignature, crate::fir::DiagnosticId>,
    ) -> Result<crate::fir::ResolvedTy, crate::fir::DiagnosticId> {
        crate::trace_compiler!(
            "signature",
            "select_member spelling={spelling} receiver={:?}",
            receiver.get(),
        );
        // An ENUM ENTRY read against its own enum type. Entries are not value members, so the
        // ordinary receiver-member lookup below never finds them; their type is the enum itself.
        if let Some(internal) = receiver.get().non_null().obj_internal() {
            if self.classifier_has_enum_entry(internal, spelling) {
                return Ok(receiver);
            }
            // An enum that ALSO declares a companion: `select_value` folds the bare enum name to
            // that companion instance, but `MyEnum.O` still names the ENTRY — an entry outranks a
            // companion member. Re-read the entry against the companion's owner.
            if let Some(owner) = internal.nested_owner().filter(|owner| {
                self.table
                    .classes
                    .get(owner)
                    .and_then(|declaration| declaration.companion_internal)
                    == Some(internal)
            }) {
                if self.classifier_has_enum_entry(owner, spelling) {
                    return crate::fir::ResolvedTy::new(Ty::obj_name(owner))
                        .map_err(|_| Self::failure());
                }
            }
        }
        if let Ok(classifier) = self.with_resolver(scope, |resolver| {
            match resolver.nested_classifier(receiver.get(), spelling) {
                crate::symbol_resolver::CandidateSelection::Selected(classifier) => {
                    Some(classifier)
                }
                crate::symbol_resolver::CandidateSelection::Ambiguous
                | crate::symbol_resolver::CandidateSelection::None => None,
            }
        }) {
            if self.classifier_is_singleton(classifier) {
                return crate::fir::ResolvedTy::new(Ty::obj_name(classifier))
                    .map_err(|_| Self::failure());
            }
        }
        if let Some(result) =
            self.selected_member_property_type(scope, receiver.get(), spelling, demand)?
        {
            return Ok(result);
        }
        // A compactly inferred extension property is intentionally registered with an unpublished
        // result until its SigExpr is forced. The general symbol facet exposes only readable final
        // values, so select the declaration through the same extension-property overload selector
        // and turn its stable source identity into a graph dependency here.
        if let Ok(property) = self.with_resolver(scope, |resolver| {
            resolver
                .select_extension_property(receiver.get(), spelling)
                .ok()
                .flatten()
        }) {
            if let Some(signature) =
                self.demanded_source_signature(None, property.stable_declaration, demand)?
            {
                return Ok(signature.result);
            }
            if let Ok(result) = crate::fir::ResolvedTy::new(property.ty) {
                return Ok(result);
            }
        }
        // A library COMPANION CONSTANT (`Int.MAX_VALUE`, `Double.NaN`, `Char.MIN_VALUE`). These are
        // compile-time constants carried on the companion classifier's `constants` map, not member
        // properties, so no member lookup can reach them. The checker consults the same channel via
        // `library_companion_const`; here the receiver is already the companion type.
        if let Some(internal) = receiver.get().non_null().obj_internal() {
            if let Some(constant) = self
                .table
                .libraries
                .classifier(internal)
                .and_then(|declaration| declaration.constants.get(spelling).cloned())
            {
                return crate::fir::ResolvedTy::new(constant.ty).map_err(|_| Self::failure());
            }
        }
        let selected = self.with_resolver(scope, |resolver| {
            let crate::symbol_resolver::Symbol::Member(facets) = resolver.resolve_symbol(
                crate::symbol_resolver::SymRecv::Value(receiver.get()),
                spelling,
                &[],
                &[],
            )?
            else {
                return None;
            };
            if let Some(property) = facets.extension_property {
                return Some((property.ty, None, Some(property)));
            }
            facets.read.map(|property| {
                let member = property.member;
                (property.ret, Some(member), None)
            })
        });
        if let Ok((result, member, extension)) = selected {
            if let Some(member) = member.as_ref() {
                if let Some(signature) =
                    self.demanded_member_signature(member.stable_declaration, demand)?
                {
                    return self.apply_demanded_member(
                        receiver.get(),
                        member,
                        &signature,
                        &[],
                        &[],
                    );
                }
            }
            if let Some(extension) = extension {
                if let Some(signature) =
                    self.demanded_source_signature(None, extension.stable_declaration, demand)?
                {
                    return Ok(signature.result);
                }
            }
            return crate::fir::ResolvedTy::new(result).map_err(|_| Self::failure());
        }
        for dispatch_receiver in self.signature_dispatch_receivers(scope) {
            let selected = self.member_extension_property_for(
                scope,
                receiver.get(),
                dispatch_receiver,
                spelling,
            );
            let (result, declaration) = match selected {
                Ok(Some(selected)) => selected,
                Ok(None) => continue,
                Err(()) => {
                    return Err(self.record_ambiguous_member(scope.owner, origin, spelling));
                }
            };
            crate::trace_compiler!(
                "signature",
                "qualified member extension property selected name={spelling} extension={:?} dispatch={dispatch_receiver:?} result={result:?} declaration={declaration:?}",
                receiver.get(),
            );
            if let Some(declaration) = declaration {
                if self
                    .headers
                    .stubs
                    .iter()
                    .any(|stub| stub.id == declaration && stub.signature_inference.is_some())
                {
                    return demand(declaration).map(|signature| signature.result);
                }
            }
            return crate::fir::ResolvedTy::new(result).map_err(|_| Self::failure());
        }
        Err(self.record_unresolved_reference(scope.owner, origin, spelling))
    }

    fn select_member_call(
        &self,
        scope: crate::fir::SignatureScope,
        spelling: &str,
        origin: crate::fir::OriginId,
        receiver: crate::fir::ResolvedTy,
        arguments: &[crate::fir::ResolvedSigCallArgument<'_>],
        type_arguments: &[crate::fir::ResolvedTy],
        trailing_lambda: bool,
        expected: Option<crate::fir::ResolvedTy>,
        demand: &mut dyn FnMut(
            crate::fir::DeclarationId,
        )
            -> Result<crate::fir::ResolvedSignature, crate::fir::DiagnosticId>,
    ) -> Result<crate::fir::ResolvedMemberCall, crate::fir::DiagnosticId> {
        crate::trace_compiler!("signature", "select_member_call spelling={spelling}");
        let ordinary_argument_types = arguments
            .iter()
            .map(|argument| argument.ty.get())
            .collect::<Vec<_>>();
        let type_arguments = type_arguments
            .iter()
            .map(|argument| argument.get())
            .collect::<Vec<_>>();
        if spelling == "invoke" && type_arguments.is_empty() {
            if let Ok(result) = self.select_invoke(scope, origin, receiver, arguments, demand) {
                return Ok(crate::fir::ResolvedMemberCall {
                    ty: Some(result),
                    declaration: None,
                });
            }
        }
        if spelling == "get" {
            if let [index] = ordinary_argument_types.as_slice() {
                if *index == Ty::Int {
                    if let Some(element) = receiver.get().array_read_elem() {
                        return crate::fir::ResolvedTy::new(element)
                            .map(|ty| crate::fir::ResolvedMemberCall {
                                ty: Some(ty),
                                declaration: None,
                            })
                            .map_err(|_| Self::failure());
                    }
                }
            }
        }
        let selected = self.with_resolver(scope, |resolver| {
            let (mut functions, properties) = resolver
                .receiver_callables(receiver.get(), spelling)
                .into_parts();
            functions.overloads =
                self.implicit_context_candidates(scope, std::mem::take(&mut functions.overloads));
            let callables = crate::libraries::Callables::from_parts(functions, properties);
            let (argument_kinds, argument_types) =
                Self::mapped_call_arguments(callables.functions(), arguments, trailing_lambda)?;
            let projected =
                self.project_postponed_callables(scope, callables, &argument_kinds);
            crate::trace_compiler!(
                "signature",
                "member call selection receiver={:?} spelling={spelling} candidates={} arguments={argument_kinds:?}",
                receiver.get(),
                projected.callables().functions().len(),
            );
            let selection = resolver.select_receiver_function_with_params_tracking(
                receiver.get(),
                spelling,
                &argument_kinds,
                &type_arguments,
                projected.callables(),
            );
            crate::trace_compiler!(
                "signature",
                "member call selection result receiver={:?} spelling={spelling} selected={}",
                receiver.get(),
                match &selection {
                    crate::symbol_resolver::CandidateSelection::Selected(_) => "yes",
                    crate::symbol_resolver::CandidateSelection::Ambiguous => "ambiguous",
                    crate::symbol_resolver::CandidateSelection::None => "none",
                },
            );
            let crate::symbol_resolver::CandidateSelection::Selected((
                selected,
                parameters,
                result,
            )) = selection
            else {
                return None;
            };
            crate::trace_compiler!(
                "signature",
                "selected member callable name={} kind={:?} source={:?} parameters={parameters:?} result={result:?}",
                selected.callable.name,
                selected.kind,
                selected.source_key,
            );
            let postponed_bindings = projected.selected_bindings(&selected);
            if selected.kind == crate::libraries::FnKind::Member {
                let mut member = selected.member_with_return(result);
                member.params = parameters;
                return Some((
                    result,
                    Some(member),
                    None,
                    None,
                    argument_types,
                    postponed_bindings,
                    None,
                ));
            }
            Some((
                result,
                None,
                selected.source_key,
                selected.stable_declaration,
                argument_types,
                postponed_bindings,
                Some(parameters),
            ))
        });
        let (result, member, source, source_declaration, argument_types) = match selected {
            Ok((
                result,
                member,
                source,
                source_declaration,
                argument_types,
                postponed_bindings,
                extension_parameters,
            )) => {
                self.commit_postponed_bindings(scope, postponed_bindings);
                if let Some(parameters) = extension_parameters {
                    // An extension's selected value parameters can still contain active builder
                    // variables supplied by its receiver. Feed the typed arguments back into that
                    // scoped constraint set (`Continuation<R>.resume(outerR)` constrains the
                    // lambda's `R`) even when the extension is dependency-backed and therefore has
                    // no source declaration to demand below.
                    self.record_scoped_argument_constraints(scope, &parameters, &argument_types);
                }
                (result, member, source, source_declaration, argument_types)
            }
            Err(_) => {
                // A member PROPERTY whose value is callable participates in call syntax after
                // ordinary member functions, exactly as at a top-level callee: `outer.Inner().fn()`
                // reads the property `fn` and applies the `invoke` convention to its value.
                if let Some(callee) =
                    self.selected_member_property_type(scope, receiver.get(), spelling, demand)?
                {
                    if let Ok(result) = self.select_invoke(scope, origin, callee, arguments, demand)
                    {
                        return Ok(crate::fir::ResolvedMemberCall {
                            ty: Some(result),
                            declaration: None,
                        });
                    }
                    // A nominal property value may declare `operator fun invoke` without being a
                    // function type or a `fun interface`. Run that convention through this same
                    // member selector so candidate collection, generic inference, argument
                    // mapping, and source-signature demand stay shared with ordinary calls.
                    if spelling != "invoke" {
                        return self.select_member_call(
                            scope,
                            "invoke",
                            origin,
                            callee,
                            arguments,
                            &[],
                            trailing_lambda,
                            expected,
                            demand,
                        );
                    }
                }
                let argument_names = arguments
                    .iter()
                    .map(|argument| argument.name.map(str::to_owned))
                    .collect::<Vec<_>>();
                let argument_names = argument_names
                    .iter()
                    .any(Option::is_some)
                    .then_some(argument_names.as_slice());
                let spread = arguments
                    .iter()
                    .map(|argument| argument.spread)
                    .collect::<Vec<_>>();
                for dispatch_receiver in self.signature_dispatch_receivers(scope) {
                    let selected = self.signature_member_extension_call(
                        scope,
                        receiver.get(),
                        dispatch_receiver,
                        spelling,
                        super::lookups::SignatureMemberExtensionArguments {
                            types: &ordinary_argument_types,
                            names: argument_names,
                            spread: &spread,
                            explicit_type_arguments: &type_arguments,
                            trailing_lambda,
                        },
                        expected.map(crate::fir::ResolvedTy::get),
                        super::super::MemberExtensionSelection::All,
                    );
                    let Some((result, declaration)) = selected else {
                        continue;
                    };
                    if let Some(declaration) = declaration {
                        if self.headers.stubs.iter().any(|stub| {
                            stub.id == declaration && stub.signature_inference.is_some()
                        }) {
                            return demand(declaration).map(|signature| {
                                crate::fir::ResolvedMemberCall {
                                    ty: Some(signature.result),
                                    declaration: Some(declaration),
                                }
                            });
                        }
                    }
                    return crate::fir::ResolvedTy::new(result)
                        .map(|ty| crate::fir::ResolvedMemberCall {
                            ty: Some(ty),
                            declaration,
                        })
                        .map_err(|_| Self::failure());
                }
                if let Some(result) = self.bound_inner_constructor_result(
                    scope,
                    receiver.get(),
                    spelling,
                    arguments,
                    &type_arguments,
                )? {
                    return Ok(crate::fir::ResolvedMemberCall {
                        ty: Some(result),
                        declaration: None,
                    });
                }
                if ordinary_argument_types.is_empty() {
                    if matches!(spelling, "inc" | "dec") {
                        if let Some(result) = super::super::builtin_inc_dec_result(receiver.get()) {
                            return crate::fir::ResolvedTy::new(result)
                                .map(|ty| crate::fir::ResolvedMemberCall {
                                    ty: Some(ty),
                                    declaration: None,
                                })
                                .map_err(|_| Self::failure());
                        }
                    }
                    let operator = match spelling {
                        "unaryMinus" => Some(crate::ast::UnOp::Neg),
                        "not" => Some(crate::ast::UnOp::Not),
                        "unaryPlus" => Some(crate::ast::UnOp::Plus),
                        _ => None,
                    };
                    if let Some(result) = operator.and_then(|operator| {
                        super::super::builtin_unary_result(
                            self.table.libraries.as_ref(),
                            receiver.get().range_operand_bound(),
                            operator,
                        )
                    }) {
                        return crate::fir::ResolvedTy::new(result)
                            .map(|ty| crate::fir::ResolvedMemberCall {
                                ty: Some(ty),
                                declaration: None,
                            })
                            .map_err(|_| Self::failure());
                    }
                }
                let [argument] = ordinary_argument_types.as_slice() else {
                    return Err(self.record_member_call_selection_failure(
                        scope,
                        origin,
                        receiver.get(),
                        spelling,
                    ));
                };
                let Some(operator) = crate::ast::BinOp::from_arith_operator_name(spelling) else {
                    return Err(self.record_member_call_selection_failure(
                        scope,
                        origin,
                        receiver.get(),
                        spelling,
                    ));
                };
                return self
                    .checked_binary(scope, origin, operator, receiver.get(), *argument)
                    .map_or_else(
                        |_| {
                            Err(self.record_member_call_selection_failure(
                                scope,
                                origin,
                                receiver.get(),
                                spelling,
                            ))
                        },
                        |ty| {
                            Ok(crate::fir::ResolvedMemberCall {
                                ty: Some(ty),
                                declaration: None,
                            })
                        },
                    );
            }
        };
        if let Some(member) = member.as_ref() {
            if let Some(declaration) = member.stable_declaration {
                if self
                    .headers
                    .stubs
                    .iter()
                    .any(|stub| stub.id == declaration && stub.signature_inference.is_some())
                {
                    let signature = demand(declaration)?;
                    let parameters = signature
                        .parameters
                        .iter()
                        .map(|parameter| parameter.get())
                        .collect::<Vec<_>>();
                    self.record_scoped_member_constraints(
                        scope,
                        receiver.get(),
                        member,
                        &parameters,
                        &argument_types,
                    );
                    return self
                        .apply_demanded_member(
                            receiver.get(),
                            member,
                            &signature,
                            &argument_types,
                            &type_arguments,
                        )
                        .map(|ty| crate::fir::ResolvedMemberCall {
                            ty: Some(ty),
                            declaration: Some(declaration),
                        });
                }
            }
            if let Some(signature) =
                self.demanded_member_signature(member.stable_declaration, demand)?
            {
                let parameters = signature
                    .parameters
                    .iter()
                    .map(|parameter| parameter.get())
                    .collect::<Vec<_>>();
                self.record_scoped_member_constraints(
                    scope,
                    receiver.get(),
                    member,
                    &parameters,
                    &argument_types,
                );
                return self
                    .apply_demanded_member(
                        receiver.get(),
                        member,
                        &signature,
                        &argument_types,
                        &type_arguments,
                    )
                    .map(|ty| crate::fir::ResolvedMemberCall {
                        ty: Some(ty),
                        declaration: member.stable_declaration,
                    });
            }
            let parameters = member
                .generic_sig
                .as_ref()
                .map(|signature| signature.params.as_slice())
                .unwrap_or(&member.params);
            self.record_scoped_member_constraints(
                scope,
                receiver.get(),
                member,
                parameters,
                &argument_types,
            );
        }
        if let Some(source) = source {
            if let Some(signature) =
                self.demanded_source_signature(None, source_declaration, demand)?
            {
                return self
                    .apply_demanded_source_callable(
                        source,
                        Some(receiver.get()),
                        &signature,
                        &argument_types,
                        None,
                        &type_arguments,
                        expected.map(crate::fir::ResolvedTy::get),
                    )
                    .map(|ty| crate::fir::ResolvedMemberCall {
                        ty: Some(ty),
                        declaration: source_declaration,
                    });
            }
        }
        let declaration = member.and_then(|member| member.stable_declaration);
        let ty = crate::fir::ResolvedTy::new(result).ok();
        if ty.is_none() && declaration.is_none() {
            return Err(Self::failure());
        }
        Ok(crate::fir::ResolvedMemberCall { ty, declaration })
    }

    fn member_call_argument_expectations(
        &self,
        scope: crate::fir::SignatureScope,
        spelling: &str,
        _origin: crate::fir::OriginId,
        receiver: crate::fir::ResolvedTy,
        arguments: &[crate::fir::SigCallArgumentProbe<'_>],
        type_arguments: &[crate::fir::ResolvedTy],
        trailing_lambda: bool,
        _expected: Option<crate::fir::ResolvedTy>,
        _demand: &mut dyn FnMut(
            crate::fir::DeclarationId,
        )
            -> Result<crate::fir::ResolvedSignature, crate::fir::DiagnosticId>,
    ) -> Result<Box<[Option<crate::fir::ResolvedTy>]>, crate::fir::DiagnosticId> {
        if spelling == "invoke" && type_arguments.is_empty() {
            if let Ok(expectations) = self.invoke_argument_expectations(scope, receiver, arguments)
            {
                return Ok(expectations);
            }
        }
        if arguments
            .iter()
            .all(|argument| matches!(argument, crate::fir::SigCallArgumentProbe::Typed(_)))
        {
            return Ok(vec![None; arguments.len()].into_boxed_slice());
        }
        let type_arguments = type_arguments
            .iter()
            .map(|argument| argument.get())
            .collect::<Vec<_>>();
        let (parameters, slots): (Vec<Ty>, Vec<Option<usize>>) =
            self.with_resolver(scope, |resolver| {
                let (mut functions, properties) = resolver
                    .receiver_callables(receiver.get(), spelling)
                    .into_parts();
                functions.overloads = self
                    .implicit_context_candidates(scope, std::mem::take(&mut functions.overloads));
                let callables = crate::libraries::Callables::from_parts(functions, properties);
                let (kinds, slots) =
                    Self::probe_call_arguments(callables.functions(), arguments, trailing_lambda)?;
                let projected = self.project_postponed_callables(scope, callables, &kinds);
                resolver
                    .select_receiver_function_with_params(
                        receiver.get(),
                        spelling,
                        &kinds,
                        &type_arguments,
                        projected.callables(),
                    )
                    .map(|(_, parameters)| {
                        let parameters = parameters
                            .into_iter()
                            .map(|parameter| {
                                resolver
                                    .functional_expectation(parameter)
                                    .unwrap_or(parameter)
                            })
                            .collect();
                        (parameters, slots)
                    })
            })?;
        crate::trace_compiler!(
            "signature",
            "member call expectations receiver={:?} spelling={spelling} parameters={parameters:?} slots={slots:?}",
            receiver.get(),
        );
        Ok(Self::postponed_expectations(arguments, &slots, &parameters))
    }

    fn select_binary(
        &self,
        scope: crate::fir::SignatureScope,
        operator: crate::fir::SigBinaryOperator,
        origin: crate::fir::OriginId,
        lhs: crate::fir::ResolvedTy,
        rhs: crate::fir::ResolvedTy,
        demand: &mut dyn FnMut(
            crate::fir::DeclarationId,
        )
            -> Result<crate::fir::ResolvedSignature, crate::fir::DiagnosticId>,
    ) -> Result<crate::fir::ResolvedTy, crate::fir::DiagnosticId> {
        let convention = match operator {
            crate::fir::SigBinaryOperator::Add => Some(("plus", false)),
            crate::fir::SigBinaryOperator::Subtract => Some(("minus", false)),
            crate::fir::SigBinaryOperator::Multiply => Some(("times", false)),
            crate::fir::SigBinaryOperator::Divide => Some(("div", false)),
            crate::fir::SigBinaryOperator::Remainder => Some(("rem", false)),
            crate::fir::SigBinaryOperator::Less
            | crate::fir::SigBinaryOperator::LessOrEqual
            | crate::fir::SigBinaryOperator::Greater
            | crate::fir::SigBinaryOperator::GreaterOrEqual => Some(("compareTo", true)),
            crate::fir::SigBinaryOperator::Equal
            | crate::fir::SigBinaryOperator::NotEqual
            | crate::fir::SigBinaryOperator::BooleanAnd
            | crate::fir::SigBinaryOperator::BooleanOr
            | crate::fir::SigBinaryOperator::ReferentialEqual
            | crate::fir::SigBinaryOperator::ReferentialNotEqual => None,
        };
        if let Some((name, comparison)) = convention {
            let selected = self
                .with_resolver(
                    scope,
                    |resolver| match super::super::select_delegate_operator(
                        resolver,
                        lhs.get(),
                        name,
                        &[rhs.get()],
                    ) {
                        crate::symbol_resolver::CandidateSelection::Selected((
                            selected,
                            result,
                        )) => Some((selected, result)),
                        crate::symbol_resolver::CandidateSelection::None
                        | crate::symbol_resolver::CandidateSelection::Ambiguous => None,
                    },
                )
                .ok();
            if let Some((selected, result)) = selected {
                let selected_result = self.selected_convention_result(
                    lhs.get(),
                    &selected,
                    result,
                    &[rhs.get()],
                    demand,
                )?;
                if comparison {
                    if selected_result.get() != Ty::Int {
                        return Err(Self::failure());
                    }
                    return crate::fir::ResolvedTy::new(Ty::Boolean).map_err(|_| Self::failure());
                }
                return Ok(selected_result);
            }
        }
        let operator = match operator {
            crate::fir::SigBinaryOperator::Add => crate::ast::BinOp::Add,
            crate::fir::SigBinaryOperator::Subtract => crate::ast::BinOp::Sub,
            crate::fir::SigBinaryOperator::Multiply => crate::ast::BinOp::Mul,
            crate::fir::SigBinaryOperator::Divide => crate::ast::BinOp::Div,
            crate::fir::SigBinaryOperator::Remainder => crate::ast::BinOp::Rem,
            crate::fir::SigBinaryOperator::Equal => crate::ast::BinOp::Eq,
            crate::fir::SigBinaryOperator::NotEqual => crate::ast::BinOp::Ne,
            crate::fir::SigBinaryOperator::Less => crate::ast::BinOp::Lt,
            crate::fir::SigBinaryOperator::LessOrEqual => crate::ast::BinOp::Le,
            crate::fir::SigBinaryOperator::Greater => crate::ast::BinOp::Gt,
            crate::fir::SigBinaryOperator::GreaterOrEqual => crate::ast::BinOp::Ge,
            crate::fir::SigBinaryOperator::BooleanAnd => crate::ast::BinOp::And,
            crate::fir::SigBinaryOperator::BooleanOr => crate::ast::BinOp::Or,
            crate::fir::SigBinaryOperator::ReferentialEqual => crate::ast::BinOp::RefEq,
            crate::fir::SigBinaryOperator::ReferentialNotEqual => crate::ast::BinOp::RefNe,
        };
        self.checked_binary(scope, origin, operator, lhs.get(), rhs.get())
    }

    fn select_invoke(
        &self,
        scope: crate::fir::SignatureScope,
        _origin: crate::fir::OriginId,
        callee: crate::fir::ResolvedTy,
        arguments: &[crate::fir::ResolvedSigCallArgument<'_>],
        demand: &mut dyn FnMut(
            crate::fir::DeclarationId,
        )
            -> Result<crate::fir::ResolvedSignature, crate::fir::DiagnosticId>,
    ) -> Result<crate::fir::ResolvedTy, crate::fir::DiagnosticId> {
        crate::trace_compiler!(
            "signature",
            "select invoke callee={:?} arguments={:?}",
            callee.get(),
            arguments
                .iter()
                .map(|argument| argument.ty.get())
                .collect::<Vec<_>>(),
        );
        let callable = match callee.get().non_null() {
            Ty::Fun(_) => Some(callee.get().non_null()),
            nominal => crate::symbol_resolver::classifier_callable_signature(
                &*self.table.libraries,
                nominal,
            ),
        };
        if let Some(Ty::Fun(signature)) = callable {
            let implicit_prefix = signature
                .context_count
                .saturating_add(usize::from(signature.has_receiver))
                .min(signature.params.len());
            let accepts_explicit_shape = signature.params.len() == arguments.len();
            let accepts_implicit_shape =
                signature.params.len() - implicit_prefix == arguments.len();
            if (!accepts_explicit_shape && !accepts_implicit_shape)
                || arguments
                    .iter()
                    .any(|argument| argument.name.is_some() || argument.spread)
            {
                return Err(Self::failure());
            }
            return crate::fir::ResolvedTy::new(signature.ret).map_err(|_| Self::failure());
        }

        let selected = self.with_resolver(scope, |resolver| {
            let (mut functions, properties) = resolver
                .receiver_callables(callee.get(), "invoke")
                .into_parts();
            functions
                .overloads
                .retain(|candidate| candidate.flags.operator);
            functions.overloads = self.implicit_context_candidates(scope, functions.overloads);
            let callables = crate::libraries::Callables::from_parts(functions, properties);
            let (argument_kinds, argument_types) =
                Self::mapped_call_arguments(callables.functions(), arguments, false)?;
            let crate::symbol_resolver::CandidateSelection::Selected((
                selected,
                parameters,
                result,
            )) = resolver.select_receiver_function_with_params_tracking(
                callee.get(),
                "invoke",
                &argument_kinds,
                &[],
                &callables,
            )
            else {
                return None;
            };
            Some((selected, parameters, result, argument_types))
        });
        if let Ok((selected, parameters, result, argument_types)) = selected {
            if selected.kind == crate::libraries::FnKind::Member {
                let mut member = selected.member_with_return(result);
                member.params = parameters;
                if let Some(signature) =
                    self.demanded_member_signature(member.stable_declaration, demand)?
                {
                    return self.apply_demanded_member(
                        callee.get(),
                        &member,
                        &signature,
                        &argument_types,
                        &[],
                    );
                }
            }
            if let Some(source) = selected.source_key {
                if let Some(signature) =
                    self.demanded_source_signature(None, selected.stable_declaration, demand)?
                {
                    return self.apply_demanded_source_callable(
                        source,
                        Some(callee.get()),
                        &signature,
                        &argument_types,
                        None,
                        &[],
                        None,
                    );
                }
            }
            return crate::fir::ResolvedTy::new(result).map_err(|_| Self::failure());
        }

        let argument_types = arguments
            .iter()
            .map(|argument| argument.ty.get())
            .collect::<Vec<_>>();
        let argument_names = arguments
            .iter()
            .map(|argument| argument.name.map(str::to_owned))
            .collect::<Vec<_>>();
        let argument_names = argument_names
            .iter()
            .any(Option::is_some)
            .then_some(argument_names.as_slice());
        let spread = arguments
            .iter()
            .map(|argument| argument.spread)
            .collect::<Vec<_>>();
        for dispatch in self.signature_dispatch_receivers(scope) {
            let Some((result, declaration)) = self.signature_member_extension_call(
                scope,
                callee.get(),
                dispatch,
                "invoke",
                super::lookups::SignatureMemberExtensionArguments {
                    types: &argument_types,
                    names: argument_names,
                    spread: &spread,
                    ..Default::default()
                },
                None,
                super::super::MemberExtensionSelection::Operators,
            ) else {
                continue;
            };
            if let Some(declaration) = declaration {
                if self
                    .headers
                    .stubs
                    .iter()
                    .any(|stub| stub.id == declaration && stub.signature_inference.is_some())
                {
                    return demand(declaration).map(|signature| signature.result);
                }
            }
            return crate::fir::ResolvedTy::new(result).map_err(|_| Self::failure());
        }
        Err(Self::failure())
    }

    fn invoke_argument_expectations(
        &self,
        scope: crate::fir::SignatureScope,
        callee: crate::fir::ResolvedTy,
        arguments: &[crate::fir::SigCallArgumentProbe<'_>],
    ) -> Result<Box<[Option<crate::fir::ResolvedTy>]>, crate::fir::DiagnosticId> {
        if arguments
            .iter()
            .all(|argument| matches!(argument, crate::fir::SigCallArgumentProbe::Typed(_)))
        {
            return Ok(vec![None; arguments.len()].into_boxed_slice());
        }
        let callable = match callee.get().non_null() {
            Ty::Fun(_) => Some(callee.get().non_null()),
            nominal => crate::symbol_resolver::classifier_callable_signature(
                &*self.table.libraries,
                nominal,
            ),
        };
        let signature = match callable {
            Some(Ty::Fun(signature)) => signature,
            _ => {
                let (parameters, slots) = self.with_resolver(scope, |resolver| {
                    let (mut functions, properties) = resolver
                        .receiver_callables(callee.get(), "invoke")
                        .into_parts();
                    functions
                        .overloads
                        .retain(|candidate| candidate.flags.operator);
                    let callables = crate::libraries::Callables::from_parts(functions, properties);
                    let (kinds, slots) =
                        Self::probe_call_arguments(callables.functions(), arguments, false)?;
                    resolver
                        .select_receiver_function_with_params(
                            callee.get(),
                            "invoke",
                            &kinds,
                            &[],
                            &callables,
                        )
                        .map(|(_, parameters)| (parameters, slots))
                })?;
                return Ok(Self::postponed_expectations(arguments, &slots, &parameters));
            }
        };
        if arguments.iter().any(|argument| match argument {
            crate::fir::SigCallArgumentProbe::Typed(argument) => {
                argument.name.is_some() || argument.spread
            }
            crate::fir::SigCallArgumentProbe::PostponedLambda { name, spread, .. }
            | crate::fir::SigCallArgumentProbe::PostponedCallableReference {
                name, spread, ..
            } => name.is_some() || *spread,
        }) {
            return Err(Self::failure());
        }
        let implicit_prefix = signature
            .context_count
            .saturating_add(usize::from(signature.has_receiver))
            .min(signature.params.len());
        let parameters = if signature.params.len() == arguments.len() {
            &signature.params[..]
        } else if signature.params.len() - implicit_prefix == arguments.len() {
            &signature.params[implicit_prefix..]
        } else {
            return Err(Self::failure());
        };
        parameters
            .iter()
            .copied()
            .map(|parameter| {
                let expectation = match parameter.non_null() {
                    Ty::Fun(_) => Some(parameter.non_null()),
                    nominal => crate::symbol_resolver::classifier_callable_signature(
                        &*self.table.libraries,
                        nominal,
                    ),
                };
                expectation
                    .map(crate::fir::ResolvedTy::new)
                    .transpose()
                    .map_err(|_| Self::failure())
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Vec::into_boxed_slice)
    }

    fn make_function_type(
        &self,
        parameters: &[crate::fir::ResolvedTy],
        result: crate::fir::ResolvedTy,
        context_count: u32,
        has_receiver: bool,
        suspend: bool,
    ) -> Result<crate::fir::ResolvedTy, crate::fir::DiagnosticId> {
        let parameters = parameters
            .iter()
            .map(|parameter| parameter.get())
            .collect::<Vec<_>>();
        let ty = Ty::fun_with_shape(
            parameters,
            result.get(),
            context_count as usize,
            has_receiver,
            suspend,
        );
        crate::fir::ResolvedTy::new(ty).map_err(|_| Self::failure())
    }

    fn contextual_function_result(
        &self,
        declaration: crate::fir::DeclarationId,
        actual: crate::fir::ResolvedTy,
        expected: crate::fir::ResolvedTy,
    ) -> Result<crate::fir::ResolvedTy, crate::fir::DiagnosticId> {
        self.checked_contextual_function_result(declaration, actual, expected)
    }

    fn make_contextual_function_type(
        &self,
        declaration: crate::fir::DeclarationId,
        parameters: &[crate::fir::ResolvedTy],
        result: crate::fir::ResolvedTy,
        context_count: u32,
        has_receiver: bool,
        suspend: bool,
    ) -> Result<crate::fir::ResolvedTy, crate::fir::DiagnosticId> {
        let bindings = self
            .completed_scoped_constraints
            .borrow_mut()
            .remove(&declaration)
            .unwrap_or_default();
        crate::trace_compiler!(
            "signature",
            "make contextual function declaration={declaration:?} parameters={:?} result={:?} bindings={bindings:?}",
            parameters.iter().map(|parameter| parameter.get()).collect::<Vec<_>>(),
            result.get(),
        );
        let parameters = parameters
            .iter()
            .map(|parameter| {
                crate::fir::ResolvedTy::new(crate::symbol_resolver::ty_subst_keep_unbound(
                    parameter.get(),
                    &bindings,
                ))
                .map_err(|_| Self::failure())
            })
            .collect::<Result<Vec<_>, _>>()?;
        let result = crate::fir::ResolvedTy::new(crate::symbol_resolver::ty_subst_keep_unbound(
            result.get(),
            &bindings,
        ))
        .map_err(|_| Self::failure())?;
        self.make_function_type(&parameters, result, context_count, has_receiver, suspend)
    }

    fn select_delegate(
        &self,
        declaration: crate::fir::DeclarationId,
        scope: crate::fir::SignatureScope,
        _origin: crate::fir::OriginId,
        delegate: crate::fir::ResolvedTy,
        local: bool,
        demand: &mut dyn FnMut(
            crate::fir::DeclarationId,
        )
            -> Result<crate::fir::ResolvedSignature, crate::fir::DiagnosticId>,
    ) -> Result<crate::fir::ResolvedTy, crate::fir::DiagnosticId> {
        let this_ref = if local {
            Ty::Null
        } else {
            self.delegate_this_ref(declaration)
        };
        let arguments = [this_ref, Ty::obj("kotlin/reflect/KProperty")];
        let provided = self.with_resolver(scope, |resolver| {
            Some(super::super::select_delegate_operator(
                resolver,
                delegate.get(),
                "provideDelegate",
                &arguments,
            ))
        })?;
        crate::trace_compiler!(
            "signature",
            "delegate provide receiver={:?} selection={:?}",
            delegate.get(),
            match &provided {
                crate::symbol_resolver::CandidateSelection::Selected((selected, result)) => Some((
                    selected.semantic_receiver(),
                    selected.semantic_params(),
                    *result,
                )),
                crate::symbol_resolver::CandidateSelection::None
                | crate::symbol_resolver::CandidateSelection::Ambiguous => None,
            },
        );
        let stored = match provided {
            crate::symbol_resolver::CandidateSelection::None => {
                let selected = self
                    .implicit_receivers(scope)
                    .into_iter()
                    .find_map(|dispatch| {
                        self.signature_member_extension_call(
                            scope,
                            delegate.get(),
                            dispatch,
                            "provideDelegate",
                            super::lookups::SignatureMemberExtensionArguments {
                                types: &arguments,
                                ..Default::default()
                            },
                            None,
                            super::super::MemberExtensionSelection::Operators,
                        )
                    });
                match selected {
                    Some((_result, Some(declaration)))
                        if self.headers.stubs.iter().any(|stub| {
                            stub.id == declaration && stub.signature_inference.is_some()
                        }) =>
                    {
                        demand(declaration)?.result
                    }
                    Some((result, _)) => {
                        crate::fir::ResolvedTy::new(result).map_err(|_| Self::failure())?
                    }
                    None => delegate,
                }
            }
            crate::symbol_resolver::CandidateSelection::Ambiguous => return Err(Self::failure()),
            crate::symbol_resolver::CandidateSelection::Selected((selected, result)) => self
                .selected_convention_result(
                    delegate.get(),
                    &selected,
                    result,
                    &arguments,
                    demand,
                )?,
        };
        let selected = self.with_resolver(scope, |resolver| {
            Some(super::super::select_delegate_operator(
                resolver,
                stored.get(),
                "getValue",
                &arguments,
            ))
        })?;
        crate::trace_compiler!(
            "signature",
            "delegate get receiver={:?} selection={:?}",
            stored.get(),
            match &selected {
                crate::symbol_resolver::CandidateSelection::Selected((selected, result)) => Some((
                    selected.semantic_receiver(),
                    selected.semantic_params(),
                    *result,
                )),
                crate::symbol_resolver::CandidateSelection::None
                | crate::symbol_resolver::CandidateSelection::Ambiguous => None,
            },
        );
        match selected {
            crate::symbol_resolver::CandidateSelection::Selected((selected, result)) => {
                self.selected_convention_result(stored.get(), &selected, result, &arguments, demand)
            }
            crate::symbol_resolver::CandidateSelection::None => {
                for dispatch in self.implicit_receivers(scope) {
                    let Some((result, declaration)) = self.signature_member_extension_call(
                        scope,
                        stored.get(),
                        dispatch,
                        "getValue",
                        super::lookups::SignatureMemberExtensionArguments {
                            types: &arguments,
                            ..Default::default()
                        },
                        None,
                        super::super::MemberExtensionSelection::Operators,
                    ) else {
                        continue;
                    };
                    if let Some(declaration) = declaration {
                        if self.headers.stubs.iter().any(|stub| {
                            stub.id == declaration && stub.signature_inference.is_some()
                        }) {
                            return demand(declaration).map(|signature| signature.result);
                        }
                    }
                    return crate::fir::ResolvedTy::new(result).map_err(|_| Self::failure());
                }
                Err(Self::failure())
            }
            crate::symbol_resolver::CandidateSelection::Ambiguous => Err(Self::failure()),
        }
    }

    fn least_upper_bound(
        &self,
        scope: crate::fir::SignatureScope,
        origin: crate::fir::OriginId,
        operands: &[crate::fir::ResolvedTy],
    ) -> Result<crate::fir::ResolvedTy, crate::fir::DiagnosticId> {
        let Some(first) = operands.first().copied() else {
            return Err(Self::failure());
        };
        let _ = origin;
        // Least upper bound is type algebra; only its LOOKUP CONTEXT is caller-specific. Signature
        // evaluation supplies the module view of the file whose signature is being solved, so no
        // checker is constructed here. A branch whose type is still undetermined has no common
        // supertype with anything and declines rather than naming a placeholder.
        let module = crate::module_symbols::ModuleSymbols::for_file(self.table, scope.source.raw());
        let source = crate::symbol_source::CompositeSource::new(vec![
            &module as &dyn crate::symbol_source::SymbolSource,
            &*self.table.libraries as &dyn crate::symbol_source::SymbolSource,
        ]);
        let oracle = crate::symbol_resolver::SourceOracle(&source);
        let joined = operands
            .iter()
            .skip(1)
            .try_fold(first.get(), |left, right| {
                super::super::semantic_common_supertype(&source, &oracle, left, right.get())
            })
            .ok_or_else(Self::failure)?;
        if joined.mentions_error() || joined.mentions_pending() {
            return Err(Self::failure());
        }
        crate::fir::ResolvedTy::new(joined).map_err(|_| Self::failure())
    }

    fn make_nullable(
        &self,
        base: crate::fir::ResolvedTy,
    ) -> Result<crate::fir::ResolvedTy, crate::fir::DiagnosticId> {
        crate::fir::ResolvedTy::new(Ty::nullable(base.get())).map_err(|_| Self::failure())
    }

    fn make_non_nullable(
        &self,
        base: crate::fir::ResolvedTy,
    ) -> Result<crate::fir::ResolvedTy, crate::fir::DiagnosticId> {
        let ty = super::super::definitely_non_null_ty(base.get());
        crate::fir::ResolvedTy::new(ty).map_err(|_| Self::failure())
    }

    fn substitute(
        &self,
        base: crate::fir::ResolvedTy,
        substitutions: &[(crate::fir::TypeParameterId, crate::fir::ResolvedTy)],
    ) -> Result<crate::fir::ResolvedTy, crate::fir::DiagnosticId> {
        if substitutions.is_empty() {
            Ok(base)
        } else {
            Err(Self::failure())
        }
    }

    fn recursive_inference_diagnostic(
        &self,
        declaration: crate::fir::DeclarationId,
    ) -> crate::fir::DiagnosticId {
        self.record_recursive_inference(declaration)
    }

    fn missing_signature_diagnostic(
        &self,
        _declaration: crate::fir::DeclarationId,
    ) -> crate::fir::DiagnosticId {
        Self::failure()
    }
}
