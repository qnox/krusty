//! Rebinding reparsed declaration annotations to finalized semantic types.

use crate::ast::TypeRef;
use crate::diag::Span;
use crate::symbol_source::SymbolSource as _;
use crate::types::Ty;

use super::Checker;

#[cfg(test)]
mod tests;

fn rewrite_spelling_deferred_bound_occurrences(
    spelling: &mut crate::spelling::Spelled,
    semantic_bounds: &crate::fx_hash::HashMap<String, Ty>,
) {
    for (semantic, argument) in &mut spelling.alias_args {
        *semantic = crate::types::ty_with_transitive_param_bounds(*semantic, semantic_bounds);
        rewrite_spelling_deferred_bound_occurrences(argument, semantic_bounds);
    }
    for argument in &mut spelling.args {
        rewrite_spelling_deferred_bound_occurrences(argument, semantic_bounds);
    }
}

impl Checker<'_> {
    pub(super) fn checked_declaration_spelling(
        &self,
        reference: &TypeRef,
    ) -> Option<&crate::spelling::Spelled> {
        self.resolved_declaration_spellings
            .get(&(reference.span.lo, reference.span.hi))
    }

    fn rewrite_deferred_bound_occurrences(
        &mut self,
        reference: &TypeRef,
        semantic_bounds: &crate::fx_hash::HashMap<String, Ty>,
    ) {
        let occurrence = (reference.span.lo, reference.span.hi);
        if let Some((semantic, _)) = self.resolved_type_bounds.get_mut(&occurrence) {
            *semantic = crate::types::ty_with_transitive_param_bounds(*semantic, semantic_bounds);
        }
        if let Some(semantic) = self.resolved_declaration_types.get_mut(&occurrence) {
            *semantic = crate::types::ty_with_transitive_param_bounds(*semantic, semantic_bounds);
        }
        if let Some(semantic) = self.resolved_type_tys.get_mut(&occurrence) {
            *semantic = crate::types::ty_with_transitive_param_bounds(*semantic, semantic_bounds);
        }
        if let Some(spelling) = self.resolved_declaration_spellings.get_mut(&occurrence) {
            rewrite_spelling_deferred_bound_occurrences(spelling, semantic_bounds);
        }
        if let Some(argument) = reference.arg.as_deref() {
            self.rewrite_deferred_bound_occurrences(argument, semantic_bounds);
        }
        for argument in &reference.targs {
            self.rewrite_deferred_bound_occurrences(argument, semantic_bounds);
        }
        for parameter in &reference.fun_params {
            self.rewrite_deferred_bound_occurrences(parameter, semantic_bounds);
        }
    }

    /// Bind a deferred local declaration's written bounds once, while all of its formal identities
    /// are already present in the lexical rung, then build the semantic type-parameter view from
    /// those checked results. The provisional rung contains identities only; it performs no lookup.
    pub(super) fn bind_deferred_type_parameters(
        &mut self,
        scope: &super::CheckerScope<'_>,
        source_names: &[String],
        source_bounds: &[(String, TypeRef)],
        declaration_start: u32,
        callable_bounds: bool,
    ) -> super::TParams {
        let enclosing = scope.visible_tparams();
        let binding_scope = scope.child(super::ScopeKind::Block);
        let scope = &binding_scope;
        let identities = source_names
            .iter()
            .enumerate()
            .map(|(ordinal, source)| {
                crate::types::declaration_type_parameter(
                    self.compilation_id,
                    self.file_index,
                    declaration_start,
                    ordinal,
                    source,
                )
            })
            .collect::<Vec<_>>();
        let mut provisional = enclosing.clone();
        for (source, &identity) in source_names.iter().zip(&identities) {
            provisional.insert_binding(
                source,
                Ty::ty_param(identity, Ty::nullable(Ty::obj("kotlin/Any"))),
                Vec::new(),
            );
        }
        scope.declare_tparams(source_names, &provisional, |_| false);
        let mut checked = vec![Vec::new(); source_names.len()];
        for (owner, reference) in source_bounds {
            let bound = if callable_bounds {
                self.check_callable_type_parameter_bound(scope, reference)
            } else {
                self.check_type_parameter_bound(scope, reference)
            };
            if let Some(ordinal) = source_names.iter().position(|source| source == owner) {
                checked[ordinal].push(bound);
            }
        }
        let implicit = Ty::nullable(Ty::obj("kotlin/Any"));
        let semantic_bounds = identities
            .iter()
            .enumerate()
            .map(|(ordinal, &identity)| {
                (
                    identity.to_string(),
                    checked[ordinal].first().copied().unwrap_or(implicit),
                )
            })
            .collect::<crate::fx_hash::HashMap<_, _>>();
        let corrected = checked
            .iter()
            .map(|bounds| {
                bounds
                    .iter()
                    .map(|&bound| {
                        crate::types::ty_with_transitive_param_bounds(bound, &semantic_bounds)
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let mut bound_ordinals = vec![0usize; source_names.len()];
        for (owner, reference) in source_bounds {
            let Some(parameter) = source_names.iter().position(|source| source == owner) else {
                continue;
            };
            let ordinal = bound_ordinals[parameter];
            let corrected_bound = corrected[parameter][ordinal];
            bound_ordinals[parameter] += 1;
            let occurrence = (reference.span.lo, reference.span.hi);
            self.resolved_type_bounds
                .get_mut(&occurrence)
                .expect("a checked deferred bound must have a bound occurrence")
                .0 = corrected_bound;
            *self
                .resolved_declaration_types
                .get_mut(&occurrence)
                .expect("a checked deferred bound must have a declaration occurrence") =
                corrected_bound;
            self.rewrite_deferred_bound_occurrences(reference, &semantic_bounds);
        }
        let mut finalized = enclosing;
        for ((source, &identity), bounds) in source_names.iter().zip(&identities).zip(corrected) {
            let mut bounds = bounds.into_iter();
            let primary = bounds.next().unwrap_or(implicit);
            finalized.insert_binding(source, Ty::ty_param(identity, primary), bounds.collect());
        }
        finalized
    }

    /// Recreate a declaration's lexical type-parameter rung exclusively from the stable header
    /// selected in Pass 1. A demanded local declaration must not walk its source bounds again just
    /// to construct the body scope: doing so would bind the same aliases/classifiers twice.
    pub(super) fn finalized_type_parameters(
        &self,
        enclosing: &super::TParams,
        declaration: crate::fir::DeclarationId,
        source_names: &[String],
    ) -> Option<super::TParams> {
        let index = self.resolved_index?;
        let source_count = u32::try_from(source_names.len()).expect("too many type parameters");
        let has_wrong_parameter_count = index
            .declaration_header(declaration)
            .is_some_and(|header| header.kind == crate::fir::DeclarationKind::Classifier)
            .then(|| index.classifier_own_type_parameter_count(declaration))
            .flatten()
            .map_or_else(
                || index.type_parameter(declaration, source_count).is_some(),
                |own| own != source_count,
            );
        if has_wrong_parameter_count {
            return None;
        }
        let mut parameters = enclosing.clone();
        for (ordinal, source) in source_names.iter().enumerate() {
            let parameter = index.type_parameter(
                declaration,
                u32::try_from(ordinal).expect("too many type parameters"),
            )?;
            if index.type_parameter_name(parameter) != Some(source.as_str()) {
                return None;
            }
            let header = index.type_parameter_header(parameter)?;
            let semantic = index.type_parameter_semantic_name(parameter)?;
            let mut bounds = header.bounds.iter().map(|bound| bound.ty.get());
            let primary = bounds
                .next()
                .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")));
            parameters.insert_binding(source, Ty::ty_param(semantic, primary), bounds.collect());
        }
        Some(parameters)
    }

    /// Whether an active local declaration's semantic header still belongs to Pass 2. Spelling
    /// sidecars have an independent lifecycle and cannot be used as a completion marker.
    pub(super) fn has_deferred_local_header(
        &self,
        declaration: Option<crate::fir::DeclarationId>,
    ) -> bool {
        declaration
            .zip(self.resolved_index)
            .is_some_and(|(declaration, index)| {
                index.declaration_header(declaration).is_some_and(|header| {
                    header.flags.has(crate::fir::DeclarationFlags::LOCAL_CLASS)
                }) && !index.declaration_semantic_header_complete(declaration)
            })
    }

    /// Bind the type-use annotations of an occurrence while its authoritative lexical scope is in
    /// hand. Only the semantic fact consumed later is retained; no diagnostic post-check repeats
    /// classifier selection from the annotation spelling.
    pub(super) fn retain_resolved_type_use_annotations(
        &mut self,
        scope: &super::CheckerScope<'_>,
        reference: &TypeRef,
    ) {
        let annotations = self
            .file
            .type_annotations
            .get(&reference.span.lo)
            .cloned()
            .unwrap_or_default();
        if annotations.iter().any(|annotation| {
            self.annotation_identity_in_scope(scope, annotation)
                == Some(crate::types::wk::unsafe_variance())
        }) {
            self.resolved_unsafe_variance_occurrences
                .insert((reference.span.lo, reference.span.hi));
        }
    }

    /// Restore the same fact from the checked spelling that Pass 1 finalized for a declaration.
    pub(super) fn retain_finalized_type_use_annotations(
        &mut self,
        reference: &TypeRef,
        spelled: &crate::spelling::Spelled,
    ) {
        if spelled.unsafe_variance
            || spelled.annotations.iter().any(|annotation| {
                matches!(
                    annotation,
                    crate::spelling::TypeUseAnnotation::Checked(annotation)
                        if annotation.annotation == crate::types::wk::unsafe_variance()
                )
            })
        {
            self.resolved_unsafe_variance_occurrences
                .insert((reference.span.lo, reference.span.hi));
        }
    }

    /// Publish the superclass occurrence from the classifier header Pass 1 already finalized.
    /// The detached parser copy is not resolved again: doing so would lose the declaration's
    /// lexical visibility policy, while editor navigation still needs the selected identity at
    /// the written source span.
    pub(super) fn record_finalized_superclass_type(&mut self, span: Span, semantic: Ty) {
        self.resolved_type_tys.insert((span.lo, span.hi), semantic);
    }

    /// Rebind a reparsed value-parameter annotation to the semantic type finalized in Pass 1.
    /// Signatures store a vararg's callable-facing array type, while the annotation occurrence
    /// denotes its element type; record that syntax-facing view and return the callable-facing type
    /// used for the lexical parameter binding.
    pub(super) fn record_finalized_parameter_type(
        &mut self,
        reference: &TypeRef,
        is_vararg: bool,
        semantic: Ty,
        spelled: &crate::spelling::Spelled,
    ) -> Ty {
        let declaration = if is_vararg {
            semantic.non_null().array_read_elem().unwrap_or(semantic)
        } else {
            semantic
        };
        self.resolved_type_tys
            .insert((reference.span.lo, reference.span.hi), declaration);
        self.resolved_declaration_types
            .insert((reference.span.lo, reference.span.hi), declaration);
        self.resolved_declaration_spellings
            .insert((reference.span.lo, reference.span.hi), spelled.clone());
        self.retain_finalized_alias_applications(reference, spelled);
        self.check_finalized_type_reference_usage(reference, declaration);
        self.check_finalized_type_bounds(reference, declaration, spelled);
        self.check_finalized_package_type_arguments(reference, declaration, spelled);
        semantic
    }

    /// Rebind an ordinary declaration annotation to the semantic type selected in Pass 1.
    /// `check_declaration_type` publishes both occurrence and declaration channels; the finalized
    /// path must preserve that same `TypeInfo` contract without repeating lookup.
    pub(super) fn record_finalized_declaration_type(
        &mut self,
        reference: &TypeRef,
        semantic: Ty,
        spelled: &crate::spelling::Spelled,
    ) -> Ty {
        self.resolved_type_tys
            .insert((reference.span.lo, reference.span.hi), semantic);
        self.resolved_declaration_types
            .insert((reference.span.lo, reference.span.hi), semantic);
        self.resolved_declaration_spellings
            .insert((reference.span.lo, reference.span.hi), spelled.clone());
        self.retain_finalized_alias_applications(reference, spelled);
        self.check_finalized_type_reference_usage(reference, semantic);
        self.check_finalized_type_bounds(reference, semantic, spelled);
        self.check_finalized_package_type_arguments(reference, semantic, spelled);
        semantic
    }

    fn active_deferred_spelling_owner(&self, declaration: crate::fir::DeclarationId) -> bool {
        let (Some(active), Some(index)) = (self.active_declarations, self.resolved_index) else {
            return false;
        };
        index.declaration_spellings_deferred(declaration)
            && active
                .stable_declarations()
                .any(|candidate| candidate == declaration)
    }

    /// Bind a declaration role whose semantic header and written occurrence were transferred
    /// together to active Pass 2. The role's checked annotation tree is the only source-spelling
    /// input; an absent or already-selected role is a broken ownership contract, not permission to
    /// enter the ordinary source path.
    pub(super) fn bind_active_deferred_declaration_type(
        &mut self,
        scope: &super::CheckerScope<'_>,
        declaration: crate::fir::DeclarationId,
        reference: &TypeRef,
        role: Option<crate::spelling::DeclaredSpellingRole<'_>>,
    ) -> Ty {
        if !self.active_deferred_spelling_owner(declaration) {
            self.failed_local_declaration_bindings.insert(declaration);
            return Ty::Error;
        }
        let checked = match role {
            Some(crate::spelling::DeclaredSpellingRole::Selected {
                semantic: Some(semantic),
                spelling,
            }) => self.record_finalized_declaration_type(reference, semantic, spelling),
            Some(crate::spelling::DeclaredSpellingRole::Active(source_facts)) => {
                self.check_declaration_type_with_source_facts(scope, reference, Some(source_facts))
            }
            _ => {
                self.failed_local_declaration_bindings.insert(declaration);
                return Ty::Error;
            }
        };
        if checked == Ty::Error || self.checked_declaration_spelling(reference).is_none() {
            self.failed_local_declaration_bindings.insert(declaration);
            return Ty::Error;
        }
        checked
    }

    pub(super) fn bind_active_deferred_parameter_type(
        &mut self,
        scope: &super::CheckerScope<'_>,
        declaration: crate::fir::DeclarationId,
        reference: &TypeRef,
        is_vararg: bool,
        role: Option<crate::spelling::DeclaredSpellingRole<'_>>,
    ) -> Ty {
        let checked =
            self.bind_active_deferred_declaration_type(scope, declaration, reference, role);
        super::semantic_value_parameter_ty(checked, is_vararg)
    }

    /// Record a Pass-1 occurrence sidecar, or perform the first binding of an occurrence whose
    /// mandatory semantic header explicitly transferred spelling ownership to active Pass 2.
    /// Missing metadata is never sufficient to enter the latter branch.
    pub(super) fn record_or_bind_deferred_declaration_type(
        &mut self,
        scope: &super::CheckerScope<'_>,
        declaration: crate::fir::DeclarationId,
        reference: &TypeRef,
        semantic: Ty,
        spelled: Option<crate::spelling::DeclaredSpellingRole<'_>>,
    ) -> Ty {
        if let Some(crate::spelling::DeclaredSpellingRole::Selected {
            semantic: selected,
            spelling,
        }) = spelled
        {
            if selected.is_some_and(|selected| selected != semantic) {
                self.failed_local_declaration_bindings.insert(declaration);
                return Ty::Error;
            }
            return self.record_finalized_declaration_type(reference, semantic, spelling);
        }
        if !self.active_deferred_spelling_owner(declaration) {
            self.failed_local_declaration_bindings.insert(declaration);
            return Ty::Error;
        }
        let Some(crate::spelling::DeclaredSpellingRole::Active(source_facts)) = spelled else {
            self.failed_local_declaration_bindings.insert(declaration);
            return Ty::Error;
        };
        let checked =
            self.check_declaration_type_with_source_facts(scope, reference, Some(source_facts));
        if checked != semantic || self.checked_declaration_spelling(reference).is_none() {
            self.failed_local_declaration_bindings.insert(declaration);
            return Ty::Error;
        }
        semantic
    }

    /// [`Self::record_or_bind_deferred_declaration_type`] for one value parameter. The stable
    /// signature owns the callable-facing vararg array; the written occurrence owns its element.
    pub(super) fn record_or_bind_deferred_parameter_type(
        &mut self,
        scope: &super::CheckerScope<'_>,
        declaration: crate::fir::DeclarationId,
        reference: &TypeRef,
        is_vararg: bool,
        semantic: Ty,
        spelled: Option<crate::spelling::DeclaredSpellingRole<'_>>,
    ) -> Ty {
        if let Some(crate::spelling::DeclaredSpellingRole::Selected {
            semantic: selected,
            spelling,
        }) = spelled
        {
            let expected = if is_vararg {
                semantic.non_null().array_read_elem().unwrap_or(semantic)
            } else {
                semantic
            };
            if selected.is_some_and(|selected| selected != expected) {
                self.failed_local_declaration_bindings.insert(declaration);
                return Ty::Error;
            }
            return self.record_finalized_parameter_type(reference, is_vararg, semantic, spelling);
        }
        if !self.active_deferred_spelling_owner(declaration) {
            self.failed_local_declaration_bindings.insert(declaration);
            return Ty::Error;
        }
        let expected = if is_vararg {
            semantic.non_null().array_read_elem().unwrap_or(semantic)
        } else {
            semantic
        };
        let Some(crate::spelling::DeclaredSpellingRole::Active(source_facts)) = spelled else {
            self.failed_local_declaration_bindings.insert(declaration);
            return Ty::Error;
        };
        let checked =
            self.check_declaration_type_with_source_facts(scope, reference, Some(source_facts));
        if checked != expected || self.checked_declaration_spelling(reference).is_none() {
            self.failed_local_declaration_bindings.insert(declaration);
            return Ty::Error;
        }
        semantic
    }

    /// Bind the single written type occurrence shared by a primary-constructor parameter and its
    /// `val`/`var` property. Compact signature collection deliberately assigns that occurrence to
    /// both stable declarations. Active Pass 2 must therefore select their paired spelling roles
    /// atomically: an active role is resolved exactly once, while every selected role is checked
    /// against that same result.
    pub(super) fn record_or_bind_deferred_constructor_property_type(
        &mut self,
        scope: &super::CheckerScope<'_>,
        constructor: crate::fir::DeclarationId,
        property: crate::fir::DeclarationId,
        reference: &TypeRef,
        is_vararg: bool,
        expected_semantic: Option<Ty>,
        constructor_role: Option<crate::spelling::DeclaredSpellingRole<'_>>,
        property_role: Option<crate::spelling::DeclaredSpellingRole<'_>>,
    ) -> Ty {
        let (Some(constructor_role), Some(property_role)) = (constructor_role, property_role)
        else {
            self.failed_local_declaration_bindings.insert(constructor);
            self.failed_local_declaration_bindings.insert(property);
            return Ty::Error;
        };
        if matches!(
            (constructor_role, property_role),
            (
                crate::spelling::DeclaredSpellingRole::Active(left),
                crate::spelling::DeclaredSpellingRole::Active(right),
            ) if left != right
        ) {
            self.failed_local_declaration_bindings.insert(constructor);
            self.failed_local_declaration_bindings.insert(property);
            return Ty::Error;
        }
        let (binding_owner, binding_role) = match (constructor_role, property_role) {
            (role @ crate::spelling::DeclaredSpellingRole::Active(_), _) => (constructor, role),
            (_, role @ crate::spelling::DeclaredSpellingRole::Active(_)) => (property, role),
            (role, _) => (constructor, role),
        };
        let expected_source_semantic = expected_semantic.map(|semantic| {
            if is_vararg {
                semantic.non_null().array_read_elem().unwrap_or(semantic)
            } else {
                semantic
            }
        });
        let source_semantic = match binding_role {
            crate::spelling::DeclaredSpellingRole::Active(_) => self
                .bind_active_deferred_declaration_type(
                    scope,
                    binding_owner,
                    reference,
                    Some(binding_role),
                ),
            crate::spelling::DeclaredSpellingRole::Selected { semantic, spelling } => {
                match semantic.or(expected_source_semantic) {
                    Some(semantic) => {
                        self.record_finalized_declaration_type(reference, semantic, spelling)
                    }
                    None => Ty::Error,
                }
            }
        };
        let checked = super::semantic_value_parameter_ty(source_semantic, is_vararg);
        let selected_matches =
            |role: crate::spelling::DeclaredSpellingRole<'_>,
             spelling: &crate::spelling::Spelled| {
                match role {
                    crate::spelling::DeclaredSpellingRole::Selected {
                        semantic,
                        spelling: selected,
                    } => {
                        semantic.is_none_or(|selected| selected == source_semantic)
                            && selected == spelling
                    }
                    crate::spelling::DeclaredSpellingRole::Active(_) => true,
                }
            };
        let valid = checked != Ty::Error
            && expected_semantic.is_none_or(|expected| expected == checked)
            && self
                .checked_declaration_spelling(reference)
                .is_some_and(|spelling| {
                    selected_matches(constructor_role, spelling)
                        && selected_matches(property_role, spelling)
                });
        if !valid {
            self.failed_local_declaration_bindings.insert(constructor);
            self.failed_local_declaration_bindings.insert(property);
            return Ty::Error;
        }
        checked
    }

    /// Publish source occurrences for type-parameter bounds that Pass 1 already bound. Source
    /// parameter names are used only to address their declaration-owned ordinal; every semantic
    /// type and spelling comes from the stable declaration header.
    pub(super) fn record_finalized_type_parameter_bounds(
        &mut self,
        _scope: &super::CheckerScope<'_>,
        declaration: crate::fir::DeclarationId,
        parameters: &[String],
        bounds: &[(String, TypeRef)],
    ) -> bool {
        let Some(index) = self.resolved_index else {
            return false;
        };
        let stable = (0..parameters.len())
            .map(|ordinal| {
                let ordinal = u32::try_from(ordinal).expect("too many type parameters");
                let parameter = index.type_parameter(declaration, ordinal)?;
                Some(
                    index
                        .type_parameter_header(parameter)?
                        .bounds
                        .iter()
                        .map(|bound| bound.ty.get())
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Option<Vec<_>>>();
        let Some(stable) = stable else {
            return parameters.is_empty() && bounds.is_empty();
        };
        let implicit = Ty::nullable(Ty::obj("kotlin/Any"));
        let Some(semantic_bounds) = parameters
            .iter()
            .enumerate()
            .map(|(ordinal, _)| {
                let parameter = index.type_parameter(
                    declaration,
                    u32::try_from(ordinal).expect("too many type parameters"),
                )?;
                Some((
                    index.type_parameter_semantic_name(parameter)?.to_string(),
                    stable[ordinal].first().copied().unwrap_or(implicit),
                ))
            })
            .collect::<Option<crate::fx_hash::HashMap<_, _>>>()
        else {
            return false;
        };
        let Some(spellings) = self.finalized_declaration_spellings_for_binding(Some(declaration))
        else {
            return false;
        };
        let mut ordinals = vec![0usize; parameters.len()];
        for (source, reference) in bounds {
            let Some(parameter) = parameters.iter().position(|name| name == source) else {
                return false;
            };
            let bound = ordinals[parameter];
            let Some(&semantic) = stable[parameter].get(bound) else {
                return false;
            };
            let semantic =
                crate::types::ty_with_transitive_param_bounds(semantic, &semantic_bounds);
            let Some(mut spelling) = spellings.bound(parameter, bound).cloned() else {
                return false;
            };
            rewrite_spelling_deferred_bound_occurrences(&mut spelling, &semantic_bounds);
            self.record_finalized_declaration_type(reference, semantic, &spelling);
            let interface =
                crate::fir::ResolvedTypeParameterBound::is_interface_type(semantic, |owner| {
                    self.fed_source()
                        .classifier(owner)
                        .is_some_and(|classifier| classifier.is_interface())
                });
            self.resolved_type_bounds.insert(
                (reference.span.lo, reference.span.hi),
                (semantic, interface),
            );
            ordinals[parameter] += 1;
        }
        stable
            .iter()
            .zip(ordinals)
            .all(|(stable, source)| stable.len() == source)
    }

    /// Published spelling facts for a finalized declaration. `None` denotes only the explicit
    /// active-Pass-2 ownership state, never an inference from an absent sidecar.
    pub(super) fn finalized_declaration_spellings_for_binding(
        &self,
        declaration: Option<crate::fir::DeclarationId>,
    ) -> Option<crate::spelling::DeclaredSpellingsForBinding> {
        let (declaration, index) = declaration.zip(self.resolved_index)?;
        if let Some(spellings) = index.declaration_spellings(declaration) {
            return Some(crate::spelling::DeclaredSpellingsForBinding::Complete(
                spellings.clone(),
            ));
        }
        if let Some(spellings) = index.deferred_declaration_spellings(declaration) {
            return Some(crate::spelling::DeclaredSpellingsForBinding::Deferred(
                spellings.clone(),
            ));
        }
        index
            .declaration_header(declaration)
            .is_some_and(|header| !header.flags.has(crate::fir::DeclarationFlags::LOCAL_CLASS))
            .then_some(crate::spelling::DeclaredSpellingsForBinding::Complete(
                crate::spelling::DeclaredSpellings::NONE.clone(),
            ))
    }
}
