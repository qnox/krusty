//! Rebinding reparsed declaration annotations to finalized semantic types.

use crate::ast::TypeRef;
use crate::diag::Span;
use crate::types::Ty;

use super::Checker;

#[cfg(test)]
mod tests;

impl Checker<'_> {
    fn rewrite_deferred_bound_occurrences(
        &mut self,
        reference: &TypeRef,
        semantic_bounds: &crate::fx_hash::HashMap<String, Ty>,
    ) {
        fn rewrite_spelling(
            spelling: &mut crate::spelling::Spelled,
            semantic_bounds: &crate::fx_hash::HashMap<String, Ty>,
        ) {
            for (semantic, argument) in &mut spelling.alias_args {
                *semantic = crate::types::ty_with_param_bounds(*semantic, semantic_bounds);
                rewrite_spelling(argument, semantic_bounds);
            }
            for argument in &mut spelling.args {
                rewrite_spelling(argument, semantic_bounds);
            }
        }

        let occurrence = (reference.span.lo, reference.span.hi);
        if let Some((semantic, _)) = self.resolved_type_bounds.get_mut(&occurrence) {
            *semantic = crate::types::ty_with_param_bounds(*semantic, semantic_bounds);
        }
        if let Some(semantic) = self.resolved_declaration_types.get_mut(&occurrence) {
            *semantic = crate::types::ty_with_param_bounds(*semantic, semantic_bounds);
        }
        if let Some(semantic) = self.resolved_type_tys.get_mut(&occurrence) {
            *semantic = crate::types::ty_with_param_bounds(*semantic, semantic_bounds);
        }
        if let Some(spelling) = self.resolved_declaration_spellings.get_mut(&occurrence) {
            rewrite_spelling(spelling, semantic_bounds);
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
        let mut semantic_bounds = identities
            .iter()
            .enumerate()
            .map(|(ordinal, &identity)| {
                (
                    identity.to_string(),
                    checked[ordinal].first().copied().unwrap_or(implicit),
                )
            })
            .collect::<crate::fx_hash::HashMap<_, _>>();
        for _ in 0..source_names.len() {
            semantic_bounds = semantic_bounds
                .iter()
                .map(|(identity, bound)| {
                    (
                        identity.clone(),
                        crate::types::ty_with_param_bounds(*bound, &semantic_bounds),
                    )
                })
                .collect();
        }
        let corrected = checked
            .iter()
            .map(|bounds| {
                bounds
                    .iter()
                    .map(|&bound| crate::types::ty_with_param_bounds(bound, &semantic_bounds))
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
        if index
            .type_parameter(
                declaration,
                u32::try_from(source_names.len()).expect("too many type parameters"),
            )
            .is_some()
        {
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

    /// Whether an active local declaration's compact header deliberately left both semantic
    /// binding and spelling projection to Pass 2. Demanded/inline/default-owned locals publish an
    /// explicit spelling record even when it is empty, so absence is unambiguous here.
    pub(super) fn has_deferred_local_header(
        &self,
        declaration: Option<crate::fir::DeclarationId>,
    ) -> bool {
        declaration
            .zip(self.resolved_index)
            .is_some_and(|(declaration, index)| {
                index.declaration_header(declaration).is_some_and(|header| {
                    header.flags.has(crate::fir::DeclarationFlags::LOCAL_CLASS)
                }) && index.declaration_spellings(declaration).is_none()
            })
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
        self.check_finalized_type_reference_usage(reference, semantic);
        self.check_finalized_type_bounds(reference, semantic, spelled);
        self.check_finalized_package_type_arguments(reference, semantic, spelled);
        semantic
    }

    /// Publish source occurrences for type-parameter bounds that Pass 1 already bound. Source
    /// parameter names are used only to address their declaration-owned ordinal; every semantic
    /// type and spelling comes from the stable declaration header.
    pub(super) fn record_finalized_type_parameter_bounds(
        &mut self,
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
        let spellings = self
            .finalized_declaration_spellings(Some(declaration))
            .clone();
        let mut ordinals = vec![0usize; parameters.len()];
        for (source, reference) in bounds {
            let Some(parameter) = parameters.iter().position(|name| name == source) else {
                return false;
            };
            let bound = ordinals[parameter];
            let Some(&semantic) = stable[parameter].get(bound) else {
                return false;
            };
            self.record_finalized_declaration_type(
                reference,
                semantic,
                spellings.bound(parameter, bound),
            );
            ordinals[parameter] += 1;
        }
        stable
            .iter()
            .zip(ordinals)
            .all(|(stable, source)| stable.len() == source)
    }
}
