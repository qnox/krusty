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
    /// The first member rung visible on `receiver`, including headers published by classifiers in
    /// the active bounded body. The immutable symbol provider cannot contain those headers when
    /// their results are inferred on this Pass-2 lexical rung, so every member consumer must use
    /// this one union instead of independently querying the provider or the transient overlay.
    pub(super) fn body_local_member_overload_rung(
        &self,
        receiver: Ty,
        name: &str,
    ) -> (Ty, Vec<crate::libraries::FunctionInfo>, bool) {
        let candidates_at = |candidate_receiver: Ty| {
            let mut candidates = self
                .stable_receiver_callables(candidate_receiver, name)
                .functions()
                .iter()
                .filter(|candidate| candidate.kind == crate::libraries::FnKind::Member)
                .cloned()
                .collect::<Vec<_>>();
            let mut contains_body_local = false;
            if let Some(owner) =
                crate::symbol_resolver::member_scope_receiver(candidate_receiver).obj_internal()
            {
                if let Some(local) = self
                    .checked_local_methods
                    .get(&owner)
                    .and_then(|methods| methods.get(name))
                {
                    for candidate in local
                        .iter()
                        .filter(|candidate| candidate.kind == crate::libraries::FnKind::Member)
                    {
                        contains_body_local = true;
                        if !candidates.iter().any(|existing| {
                            existing.stable_declaration == candidate.stable_declaration
                        }) {
                            candidates.push(candidate.clone());
                        }
                    }
                }
            }
            (candidates, contains_body_local)
        };

        let (direct, contains_body_local) = candidates_at(receiver);
        if !direct.is_empty() {
            return (
                crate::symbol_resolver::member_scope_receiver(receiver),
                direct,
                contains_body_local,
            );
        }
        for supertype in self.body_local_supertypes(receiver) {
            let (inherited, contains_body_local) = candidates_at(supertype);
            if !inherited.is_empty() {
                return (supertype, inherited, contains_body_local);
            }
        }
        (receiver, Vec::new(), false)
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
        let candidate = crate::module_symbols::source_member_function(
            &function.name,
            &signature,
            receiver,
            owner_name,
            owner_is_interface,
        );
        self.checked_local_methods
            .entry(owner_name)
            .or_default()
            .entry(function.name.clone())
            .or_default()
            .push(candidate);
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
