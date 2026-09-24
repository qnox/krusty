//! Dependency-order checking for body-local class methods.
//!
//! A local or anonymous classifier is checked on its Pass-2 lexical rung. Its methods are emitted
//! in source order, but an earlier body may call a later method whose result is inferred from that
//! later body. This bounded scheduler checks the selected declaration on demand, in the same
//! checker and scope chain, and marks it consumed so the ordinary stream never checks it twice.

use super::*;

#[derive(Clone)]
pub(super) struct LocalMethodDependency {
    source: crate::libraries::SourceMember,
    owner: DeclId,
    method: usize,
    properties: Vec<ScopedProperty>,
    this_labels: Vec<(String, Ty, bool)>,
    extension_receiver_labels: Vec<(usize, Span)>,
    lexical_class_context: Vec<TypeName>,
    exact_anonymous_class_roots: std::collections::HashSet<TypeName>,
    static_companion_this: Option<crate::symbol_resolver::ClassifierCompanionInstance>,
    static_singleton_this: Option<SingletonValue>,
    this_extension_receiver: Option<Span>,
}

pub(super) enum LocalMethodDemand {
    NotScheduled,
    Complete(Option<Ty>),
    Recursive,
}

impl Checker<'_> {
    /// Select the one inherited declaration whose invariant input shape this body-local method
    /// overrides. This runs only over typed provider candidates; the method's still-pending result
    /// is deliberately irrelevant to default availability, while the completed override plan later
    /// validates covariance and freezes the same stable declaration edge.
    fn inherited_body_local_default_candidate(
        &self,
        owner: TypeName,
        implementation: &crate::libraries::FunctionInfo,
    ) -> Option<(
        crate::libraries::FunctionInfo,
        crate::fir::ResolvedFunctionOverrideTarget,
    )> {
        let implementation_formals = implementation
            .generic_sig
            .as_ref()
            .map(|signature| signature.formals.as_slice())
            .unwrap_or_default();
        let implementation_parameters = implementation.semantic_params();
        let source = self.fed_source();
        let matches_override = |candidate: &crate::libraries::FunctionInfo| {
            if candidate.kind != implementation.kind
                || candidate.visibility == Visibility::Private
                || candidate.flags.is_final
            {
                return false;
            }
            let candidate_formals = candidate
                .generic_sig
                .as_ref()
                .map(|signature| signature.formals.as_slice())
                .unwrap_or_default();
            crate::symbol_resolver::override_input_shapes_match(
                &source,
                crate::symbol_resolver::OverrideInputShape {
                    params: &candidate.semantic_params(),
                    receiver: candidate.callable.source_receiver,
                    formals: candidate_formals,
                    context_count: candidate.context_count,
                    suspend: candidate.flags.suspend,
                },
                crate::symbol_resolver::OverrideInputShape {
                    params: &implementation_parameters,
                    receiver: implementation.callable.source_receiver,
                    formals: implementation_formals,
                    context_count: implementation.context_count,
                    suspend: implementation.flags.suspend,
                },
            )
        };
        let identity = |candidate: &crate::libraries::FunctionInfo| {
            if let Some(provider) = candidate.callable.external_default_provider {
                return Some(crate::fir::ResolvedFunctionOverrideTarget::External(
                    provider,
                ));
            }
            if let Some(declaration) = candidate.stable_declaration {
                if let Some(provider) = self.body_local_default_providers.get(&declaration) {
                    return Some(*provider);
                }
                let callable = self
                    .resolved_index
                    .and_then(|index| index.callable_for_declaration(declaration))?;
                return Some(
                    self.resolved_index?
                        .callable_default_provider(callable.id)
                        .unwrap_or(crate::fir::ResolvedFunctionOverrideTarget::Module(
                            callable.id,
                        )),
                );
            }
            candidate
                .callable
                .external_identity
                .map(crate::fir::ResolvedFunctionOverrideTarget::External)
        };

        let direct_supertypes = |receiver| {
            let active = self.body_local_supertypes(receiver);
            if active.is_empty() {
                crate::symbol_resolver::direct_supertypes(&source, receiver)
            } else {
                active
            }
        };
        let mut rung = direct_supertypes(Ty::obj_name(owner));
        let mut seen = std::collections::HashSet::new();
        while !rung.is_empty() {
            let mut next = Vec::new();
            let mut candidates = Vec::new();
            for supertype in rung {
                if !seen.insert(supertype) {
                    continue;
                }
                let declared = self
                    .body_local_declared_member_candidates_at(
                        supertype,
                        &implementation.callable.name,
                    )
                    .0;
                candidates.extend(
                    declared
                        .into_iter()
                        .filter(|candidate| matches_override(candidate))
                        .filter(|candidate| {
                            candidate
                                .call_sig
                                .param_defaults
                                .iter()
                                .any(|default| *default)
                        }),
                );
                next.extend(direct_supertypes(supertype));
            }
            if let Some(selected) = candidates.first().cloned() {
                let selected_identity = identity(&selected);
                if selected_identity.is_some()
                    && candidates
                        .iter()
                        .all(|candidate| identity(candidate) == selected_identity)
                {
                    return Some((selected, selected_identity?));
                }
                return None;
            }
            rung = next;
        }
        None
    }

    /// Normalize inherited default availability on one declaration candidate at the active
    /// body-local hierarchy boundary. Revisited postponed expressions may republish the same
    /// declaration before or after its surrounding classifier bodies, but selection must observe
    /// the semantic override edge derived from the current stable owner/hierarchy identities, not
    /// whichever transient publication happened last.
    fn apply_inherited_body_local_defaults(
        &self,
        owner: TypeName,
        candidate: &mut crate::libraries::FunctionInfo,
    ) -> Option<crate::fir::ResolvedFunctionOverrideTarget> {
        let is_override = candidate.stable_declaration.is_some_and(|declaration| {
            self.resolved_index
                .and_then(|index| index.declaration_header(declaration))
                .is_some_and(|header| header.flags.has(crate::fir::DeclarationFlags::OVERRIDE))
        });
        if !is_override
            || candidate
                .call_sig
                .param_defaults
                .iter()
                .any(|default| *default)
        {
            return None;
        }
        let Some((inherited, provider)) =
            self.inherited_body_local_default_candidate(owner, candidate)
        else {
            return None;
        };
        candidate.call_sig.param_defaults = inherited.call_sig.param_defaults;
        candidate.call_sig.required = inherited.call_sig.required;
        candidate.default_values = inherited.default_values;
        candidate.callable.external_default_provider = inherited
            .callable
            .external_default_provider
            .or(inherited.callable.external_identity);
        candidate.callable.default_realization = inherited.callable.default_realization;
        Some(provider)
    }

    /// The first member rung visible on `receiver`, including headers published by classifiers in
    /// the active bounded body. The immutable symbol provider cannot contain those headers when
    /// their results are inferred on this Pass-2 lexical rung, so every member consumer must use
    /// this one union instead of independently querying the provider or the transient overlay.
    pub(super) fn body_local_member_overload_rung(
        &self,
        receiver: Ty,
        name: &str,
    ) -> (Ty, Vec<crate::libraries::FunctionInfo>, bool) {
        let body_local_receiver = crate::symbol_resolver::member_scope_receiver(receiver)
            .obj_internal()
            .is_some_and(|owner| {
                self.resolved_body_local_supertypes.contains_key(&owner)
                    || self.checked_local_methods.contains_key(&owner)
                    || self.resolved_index.is_some_and(|index| {
                        index
                            .classifier_declaration(owner)
                            .and_then(|declaration| index.declaration_header(declaration))
                            .is_some_and(|header| {
                                header.flags.has(crate::fir::DeclarationFlags::LOCAL_CLASS)
                            })
                    })
            });
        if !body_local_receiver {
            return (
                crate::symbol_resolver::member_scope_receiver(receiver),
                self.stable_receiver_callables(receiver, name)
                    .functions()
                    .iter()
                    .filter(|candidate| candidate.kind == crate::libraries::FnKind::Member)
                    .cloned()
                    .collect(),
                false,
            );
        }
        let (mut direct, contains_body_local) =
            self.body_local_declared_member_candidates_at(receiver, name);
        if !direct.is_empty() {
            if let Some(owner) =
                crate::symbol_resolver::member_scope_receiver(receiver).obj_internal()
            {
                for candidate in &mut direct {
                    let _ = self.apply_inherited_body_local_defaults(owner, candidate);
                }
            }
            return (
                crate::symbol_resolver::member_scope_receiver(receiver),
                direct,
                contains_body_local,
            );
        }
        for supertype in self.body_local_supertypes(receiver) {
            let (inherited, contains_body_local) =
                self.body_local_declared_member_candidates_at(supertype, name);
            if !inherited.is_empty() {
                return (supertype, inherited, contains_body_local);
            }
        }
        (receiver, Vec::new(), false)
    }

    /// Candidate union at one exact receiver rung. The provider and active checked-local overlay
    /// contribute the same normalized shape; callers decide whether to stop at this rung or combine
    /// several direct supertypes for override matching.
    fn body_local_classifier_bindings(
        &self,
        owner: TypeName,
        receiver: Ty,
    ) -> Option<crate::symbol_resolver::GSigBinds> {
        let declaration = self.resolved_index?.classifier_declaration(owner)?;
        let declaration_arguments = self
            .checked_local_classifier_type_arguments
            .get(&declaration)?;
        let receiver = crate::symbol_resolver::member_scope_receiver(receiver);
        let applied_arguments = receiver.type_args();
        if declaration_arguments.len() != applied_arguments.len() {
            return None;
        }
        declaration_arguments
            .iter()
            .zip(applied_arguments)
            .map(|(formal, &argument)| Some((formal.ty_param_name()?.to_owned(), argument)))
            .collect()
    }

    fn body_local_declared_member_candidates_at(
        &self,
        receiver: Ty,
        name: &str,
    ) -> (Vec<crate::libraries::FunctionInfo>, bool) {
        let mut candidates =
            crate::symbol_resolver::declared_member_callables(&self.fed_source(), receiver, name)
                .functions()
                .iter()
                .filter(|candidate| candidate.kind == crate::libraries::FnKind::Member)
                .cloned()
                .collect::<Vec<_>>();
        let mut contains_body_local = false;
        if let Some(owner) = crate::symbol_resolver::member_scope_receiver(receiver).obj_internal()
        {
            let classifier_bindings = self.body_local_classifier_bindings(owner, receiver);
            if let Some(local) = self
                .checked_local_methods
                .get(&owner)
                .and_then(|methods| methods.get(name))
            {
                for candidate in local
                    .iter()
                    .filter(|candidate| candidate.kind == crate::libraries::FnKind::Member)
                {
                    let Some(classifier_bindings) = classifier_bindings.as_ref() else {
                        // The active classifier layout and the applied receiver are one checked
                        // contract. A partial or mismatched layout cannot publish an unspecialized
                        // member candidate as if it were semantically complete.
                        continue;
                    };
                    contains_body_local = true;
                    let mut candidate = candidate.clone();
                    crate::symbol_resolver::specialize_member_function(
                        &self.fed_source(),
                        receiver,
                        &mut candidate,
                        classifier_bindings,
                    );
                    if let Some(existing) = candidates.iter().position(|existing| {
                        existing.stable_declaration == candidate.stable_declaration
                    }) {
                        candidates[existing] = candidate;
                    } else {
                        candidates.push(candidate);
                    }
                }
            }
        }
        (candidates, contains_body_local)
    }

    /// Register one selected body-local member before the class's source-order method walk begins.
    /// Stable declaration identity coordinates the dependency; parser coordinates remain confined
    /// to the active unit and are used only to fetch the already-reparsed declaration body.
    pub(super) fn register_local_method_dependency(
        &mut self,
        scope: &CheckerScope<'_>,
        owner: DeclId,
        method: usize,
        function: &FunDecl,
        properties: &[ScopedProperty],
        source: crate::libraries::SourceMember,
    ) {
        if self.signature_defaults_only
            || self.capture_scope.is_some()
            || self.active_declarations.is_none()
        {
            return;
        }
        let Some(declaration) = self.active_source_member_declaration(source) else {
            return;
        };
        if self.has_finalized_signature(Some(declaration)) {
            self.publish_finalized_body_local_method_candidate(owner, function, declaration);
            return;
        }
        self.publish_body_local_method_candidate(scope, owner, function, source, declaration);
        self.local_method_dependencies.insert(
            declaration,
            LocalMethodDependency {
                source,
                owner,
                method,
                properties: properties.to_vec(),
                this_labels: self.this_labels.clone(),
                extension_receiver_labels: self.extension_receiver_labels.clone(),
                lexical_class_context: self.lexical_class_context.clone(),
                exact_anonymous_class_roots: self.exact_anonymous_class_roots.clone(),
                static_companion_this: self.static_companion_this.clone(),
                static_singleton_this: self.static_singleton_this.clone(),
                this_extension_receiver: self.this_extension_receiver,
            },
        );
    }

    /// Expose an already-finalized local method on the active local-class provider boundary. The
    /// immutable module index owns its signature, but a deferred local classifier has no immutable
    /// classifier projection yet; publishing the exact declaration candidate keeps both sources in
    /// the same stable-identity union used by inferred local methods.
    fn publish_finalized_body_local_method_candidate(
        &mut self,
        owner: DeclId,
        function: &FunDecl,
        declaration: crate::fir::DeclarationId,
    ) {
        let (owner_name, owner_is_interface) = match self.file.decl(owner) {
            Decl::Class(class) => match self.active_classifier_internal(owner, class) {
                Some(owner_name) => (owner_name, class.is_interface()),
                None => return,
            },
            Decl::Fun(_) | Decl::Property(_) => return,
        };
        let Some(mut candidate) =
            self.module
                .finalized_function(declaration, owner_name, owner_is_interface)
        else {
            return;
        };
        self.body_local_default_providers.remove(&declaration);
        if let Some(provider) = self.apply_inherited_body_local_defaults(owner_name, &mut candidate)
        {
            self.body_local_default_providers
                .insert(declaration, provider);
        }
        let candidates = self
            .checked_local_methods
            .entry(owner_name)
            .or_default()
            .entry(function.name.clone())
            .or_default();
        if let Some(existing) = candidates
            .iter_mut()
            .find(|existing| existing.stable_declaration == Some(declaration))
        {
            *existing = candidate;
        } else {
            candidates.push(candidate);
        }
    }

    /// Publish one active body-local method header before source-order body checking starts. The
    /// return of an expression-bodied declaration may remain `Pending` in this transient candidate;
    /// selection of that exact stable declaration immediately forces its registered body dependency,
    /// and no pending type is allowed into checked FIR.
    fn publish_body_local_method_candidate(
        &mut self,
        class_scope: &CheckerScope<'_>,
        owner: DeclId,
        function: &FunDecl,
        source: crate::libraries::SourceMember,
        declaration: crate::fir::DeclarationId,
    ) {
        let (owner_name, owner_is_interface) = match self.file.decl(owner) {
            Decl::Class(class) => match self.active_classifier_internal(owner, class) {
                Some(owner_name) => (owner_name, class.is_interface()),
                None => return,
            },
            Decl::Fun(_) | Decl::Property(_) => return,
        };

        let method_scope = class_scope.child(ScopeKind::Function { receiver: None });
        let method_scope = &method_scope;
        let method_tparams = method_scope
            .visible_tparams()
            .symbolic_extended_with(
                &function.type_params,
                &function.type_param_bounds,
                &|name| self.select_classifier(method_scope, name).found(),
            )
            .alpha_renamed_declaration(
                &function.type_params,
                self.compilation_id,
                self.file_index,
                function.signature_span.lo,
            );
        method_scope.declare_tparams(&function.type_params, &method_tparams, |name| {
            function.reified_type_params.contains(name)
        });

        let receiver = function
            .receiver
            .as_ref()
            .map(|receiver| self.type_ref_ty_silent(method_scope, receiver));
        let params = function
            .params
            .iter()
            .map(|parameter| {
                semantic_value_parameter_ty(
                    self.type_ref_ty_silent(method_scope, &parameter.ty),
                    parameter.is_vararg,
                )
            })
            .collect::<Vec<_>>();
        let result = function.ret.as_ref().map_or_else(
            || match function.body {
                FunBody::Expr(_) => Ty::Pending,
                FunBody::Block(_) | FunBody::None => Ty::Unit,
            },
            |result| self.type_ref_ty_silent(method_scope, result),
        );
        let formal_bounds = function
            .type_params
            .iter()
            .map(|parameter| {
                function
                    .type_param_bounds
                    .iter()
                    .filter(|(owner, _)| owner == parameter)
                    .map(|(_, bound)| self.type_ref_ty_silent(method_scope, bound))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let generic_sig = (!function.type_params.is_empty()).then(|| {
            source_generic_signature_from_tparams(
                function,
                &method_tparams,
                receiver,
                params.clone(),
                result,
                inferred_return_type_parameter(self.file, function),
                formal_bounds,
            )
        });
        let lambda_param_types = function
            .params
            .iter()
            .map(
                |parameter| match self.type_ref_ty_silent(method_scope, &parameter.ty) {
                    Ty::Fun(shape) => shape.params.clone(),
                    _ => Vec::new(),
                },
            )
            .collect::<Vec<_>>();
        let defaults = function
            .params
            .iter()
            .map(|parameter| parameter.default.is_some())
            .collect::<Vec<_>>();
        let signature = Signature {
            params: params.clone(),
            ret: result,
            generic_sig,
            projected_return_hazard: has_projected_generic_return_hazard(self.file, function),
            flags: SigFlags::default()
                .with_vararg(function.params.iter().any(|parameter| parameter.is_vararg))
                .with_is_inline(function.is_inline())
                .with_is_operator(function.is_operator())
                .with_is_infix(function.is_infix())
                .with_is_override(function.is_override())
                .with_is_final(function.is_final())
                .with_is_suspend(function.is_suspend())
                .with_has_reified_type_params(!function.reified_type_params.is_empty())
                .with_is_abstract(function.is_abstract()),
            annotations: function
                .annotations
                .iter()
                .filter_map(|annotation| {
                    self.annotation_identity_in_scope(method_scope, annotation)
                })
                .collect(),
            equality_bound: function
                .params
                .iter()
                .find_map(|parameter| self.equality_bound_parameter_ty(method_scope, parameter)),
            vararg_index: function
                .params
                .iter()
                .position(|parameter| parameter.is_vararg),
            required: crate::libraries::required_arity(params.len(), &defaults),
            param_defaults: defaults,
            exact_params: function
                .params
                .iter()
                .map(|parameter| {
                    self.file
                        .type_annotations
                        .get(&parameter.ty.span.lo)
                        .into_iter()
                        .flatten()
                        .any(|annotation| {
                            self.annotation_identity_in_scope(method_scope, annotation)
                                == Some(type_name("kotlin/internal/Exact"))
                        })
                })
                .collect(),
            no_infer_params: function
                .params
                .iter()
                .map(|parameter| {
                    self.file
                        .type_annotations
                        .get(&parameter.ty.span.lo)
                        .into_iter()
                        .flatten()
                        .any(|annotation| {
                            self.annotation_identity_in_scope(method_scope, annotation)
                                == Some(type_name("kotlin/internal/NoInfer"))
                        })
                })
                .collect(),
            implicit_integer_coercion: function
                .params
                .iter()
                .map(|parameter| {
                    self.parameter_has_implicit_integer_coercion(
                        method_scope,
                        &parameter.annotations,
                    )
                })
                .collect(),
            param_default_values: vec![None; params.len()],
            param_names: function
                .params
                .iter()
                .map(|parameter| parameter.name.clone())
                .collect(),
            lambda_param_types,
            lambda_recv: function
                .params
                .iter()
                .map(|parameter| parameter.ty.fun_has_receiver())
                .collect(),
            visibility: function.visibility,
            context_count: function.context_count,
            source_decl: None,
            stable_declaration: Some(declaration),
            source_file: Some(self.file_index),
            source_member: Some(source),
            source_receiver: receiver,
            package: String::new(),
            contract: None,
            plugin_expression: None,
        };
        let mut candidate = crate::module_symbols::source_member_function(
            &function.name,
            &signature,
            receiver,
            owner_name,
            owner_is_interface,
        );
        self.body_local_default_providers.remove(&declaration);
        if let Some(provider) = self.apply_inherited_body_local_defaults(owner_name, &mut candidate)
        {
            self.body_local_default_providers
                .insert(declaration, provider);
        }
        let candidates = self
            .checked_local_methods
            .entry(owner_name)
            .or_default()
            .entry(function.name.clone())
            .or_default();
        if let Some(existing) = candidates
            .iter_mut()
            .find(|existing| existing.stable_declaration == candidate.stable_declaration)
        {
            *existing = candidate;
        } else {
            candidates.push(candidate);
        }
    }

    pub(super) fn begin_registered_local_method(
        &mut self,
        source: crate::libraries::SourceMember,
    ) -> Option<crate::fir::DeclarationId> {
        let declaration = self.active_source_member_declaration(source)?;
        self.local_method_dependencies
            .contains_key(&declaration)
            .then(|| {
                self.checking_local_method_dependencies.insert(declaration);
                declaration
            })
    }

    pub(super) fn finish_registered_local_method(
        &mut self,
        declaration: crate::fir::DeclarationId,
    ) {
        self.checking_local_method_dependencies.remove(&declaration);
        self.checked_local_method_dependencies.insert(declaration);
    }

    pub(super) fn registered_local_method_is_complete(
        &self,
        source: crate::libraries::SourceMember,
    ) -> bool {
        self.active_source_member_declaration(source)
            .is_some_and(|declaration| {
                self.checked_local_method_dependencies
                    .contains(&declaration)
            })
    }

    /// Force a later body-local member while its declaring class scope is still on the stack.
    /// The complete checker output is retained, so this is dependency scheduling within Pass 2,
    /// not a signature-only precheck and not an additional body pass.
    pub(super) fn force_local_method_dependency(
        &mut self,
        scope: &CheckerScope<'_>,
        declaration: Option<crate::fir::DeclarationId>,
    ) -> LocalMethodDemand {
        let Some(declaration) = declaration else {
            return LocalMethodDemand::NotScheduled;
        };
        let Some(dependency) = self.local_method_dependencies.get(&declaration).cloned() else {
            return LocalMethodDemand::NotScheduled;
        };
        if self
            .checked_local_method_dependencies
            .contains(&declaration)
        {
            return LocalMethodDemand::Complete(
                self.checked_source_member_result(Some(dependency.source), Some(declaration)),
            );
        }
        if !self.checking_local_method_dependencies.insert(declaration) {
            return LocalMethodDemand::Recursive;
        }

        let (function, owner) = match self.file.decl(dependency.owner) {
            Decl::Class(class) => match class.methods.get(dependency.method) {
                Some(function) => (
                    function.clone(),
                    self.active_classifier_internal(dependency.owner, class),
                ),
                None => {
                    self.checking_local_method_dependencies.remove(&declaration);
                    return LocalMethodDemand::Complete(None);
                }
            },
            Decl::Fun(_) | Decl::Property(_) => {
                self.checking_local_method_dependencies.remove(&declaration);
                return LocalMethodDemand::Complete(None);
            }
        };
        let Some(owner) = owner else {
            self.checking_local_method_dependencies.remove(&declaration);
            return LocalMethodDemand::Complete(None);
        };
        let Some(class_scope) = scope.ancestors().find(|candidate| {
            matches!(candidate.kind(), ScopeKind::Class { ty, .. } if ty.obj_internal() == Some(owner))
        }) else {
            self.checking_local_method_dependencies.remove(&declaration);
            return LocalMethodDemand::Complete(None);
        };

        let saved_body = self.take_body_state();
        let saved_this_labels =
            std::mem::replace(&mut self.this_labels, dependency.this_labels.clone());
        let saved_extension_receiver_labels = std::mem::replace(
            &mut self.extension_receiver_labels,
            dependency.extension_receiver_labels.clone(),
        );
        self.lexical_class_context = dependency.lexical_class_context.clone();
        self.exact_anonymous_class_roots = dependency.exact_anonymous_class_roots.clone();
        self.static_companion_this = dependency.static_companion_this.clone();
        self.static_singleton_this = dependency.static_singleton_this.clone();
        self.this_extension_receiver = dependency.this_extension_receiver;

        self.check_method(
            class_scope,
            &function,
            &dependency.properties,
            Some(dependency.source),
            Some(declaration),
        );

        self.this_labels = saved_this_labels;
        self.extension_receiver_labels = saved_extension_receiver_labels;
        self.restore_body_state(saved_body);
        self.checking_local_method_dependencies.remove(&declaration);
        self.checked_local_method_dependencies.insert(declaration);
        LocalMethodDemand::Complete(
            self.checked_source_member_result(Some(dependency.source), Some(declaration)),
        )
    }
}
