//! Structural site ownership for compact signature type resolution.

use super::*;

/// One structural compact type occurrence being resolved. Only the root owns a graph origin for
/// inferred-result abbreviation; every descendant carries an explicit declaration-header site.
#[derive(Clone, Copy)]
pub(super) struct SelectedTypeSite {
    pub(super) syntax: Option<crate::fir::HeaderTypeId>,
    /// Graph-local structural applications need the selected classifier boundary while the graph
    /// is alive. This id is never reinterpreted as a declaration-header syntax id.
    pub(super) application: Option<crate::fir::SigExprId>,
    pub(super) abbreviation_origin: Option<crate::fir::OriginId>,
    /// Whether recursive resolution owns the written descendants. A structural graph application
    /// resolves its child expressions separately, so its target-template placeholders must not
    /// overwrite those already-published child facts.
    pub(super) project_children: bool,
}

impl ProductionSignatureSemantics<'_> {
    /// One compact expression may supply the same written source occurrence to several stable
    /// declarations (notably a primary-constructor `val` parameter and its property). Resolution
    /// still happens once. Mirror its already-selected structural facts to every declaration site
    /// retained by the graph/header bridge; this performs no lookup and never reconstructs a
    /// boundary from spelling.
    pub(super) fn publish_shared_type_spelling_sites(&self, sites: &[crate::fir::HeaderTypeId]) {
        let Some((&source, targets)) = sites.split_first() else {
            return;
        };
        for &target in targets {
            let mut pending = vec![(source, target)];
            while let Some((source, target)) = pending.pop() {
                let Some((source, source_function, source_children)) =
                    self.headers.syntax.written_type_node(source)
                else {
                    break;
                };
                let Some((target, target_function, target_children)) =
                    self.headers.syntax.written_type_node(target)
                else {
                    break;
                };
                if source_function != target_function
                    || source_children.len() != target_children.len()
                {
                    break;
                }
                if let Some(spelling) = self.selected_types.spelling(source) {
                    self.selected_types.record_spelling(target, spelling);
                }
                pending.extend(source_children.into_iter().zip(target_children));
            }
        }
    }

    /// Resolve one written occurrence, then publish the selected binding and semantic result at
    /// its stable declaration site. Recursive lookup receives structural child sites from this
    /// module; lookup itself never reconstructs a publication site from spelling.
    pub(super) fn signature_type_ref_at_with_site(
        &self,
        scope: crate::fir::SignatureScope,
        lexical: &super::super::CheckerScope<'_>,
        reference: &TypeRef,
        include_scope_owner_body: bool,
        selected_site: Option<SelectedTypeSite>,
    ) -> Option<Ty> {
        let resolved = self.signature_type_ref_at_impl(
            scope,
            lexical,
            reference,
            include_scope_owner_body,
            selected_site,
        )?;
        if let Some(syntax) = selected_site.and_then(|site| site.syntax) {
            let mut spelling = self.selected_types.spelling(syntax).unwrap_or_default();
            spelling.resolved = Some(resolved);
            self.selected_types.record_spelling(syntax, spelling);
        }
        if let Some(application) = selected_site.and_then(|site| site.application) {
            let mut spelling = self
                .selected_types
                .application(application)
                .unwrap_or_default();
            spelling.resolved = Some(resolved);
            self.selected_types
                .record_application(application, spelling);
        }
        Some(resolved)
    }

    pub(super) fn record_type_spelling_binding(
        &self,
        site: Option<SelectedTypeSite>,
        binding: super::super::type_spellings::ResolvedTypeSpelling,
    ) {
        if let Some(syntax) = site.and_then(|site| site.syntax) {
            self.selected_types.record_spelling(syntax, binding);
        }
        if let Some(application) = site.and_then(|site| site.application) {
            self.selected_types.record_application(application, binding);
        }
    }

    pub(super) fn record_type_spelling_projection(
        &self,
        site: Option<SelectedTypeSite>,
        resolved: Ty,
    ) {
        if let Some(syntax) = site.and_then(|site| site.syntax) {
            let mut spelling = self.selected_types.spelling(syntax).unwrap_or_default();
            spelling.resolved = Some(resolved);
            self.selected_types.record_spelling(syntax, spelling);
        }
        if let Some(application) = site.and_then(|site| site.application) {
            let mut spelling = self
                .selected_types
                .application(application)
                .unwrap_or_default();
            spelling.resolved = Some(resolved);
            self.selected_types
                .record_application(application, spelling);
        }
    }

    pub(super) fn signature_type_ref(
        &self,
        scope: crate::fir::SignatureScope,
        lexical: &super::super::CheckerScope<'_>,
        reference: &TypeRef,
    ) -> Option<Ty> {
        self.signature_type_ref_at(scope, lexical, reference, true)
    }

    pub(super) fn unresolved_signature_type_ref<'a>(
        &self,
        scope: crate::fir::SignatureScope,
        lexical: &super::super::CheckerScope<'_>,
        reference: &'a TypeRef,
        include_scope_owner_body: bool,
    ) -> &'a TypeRef {
        let nested = reference
            .fun_params
            .iter()
            .chain(reference.arg.iter().map(|argument| &**argument))
            .chain(reference.targs.iter());
        for component in nested {
            if self
                .signature_type_ref_at(scope, lexical, component, include_scope_owner_body)
                .is_none()
            {
                return self.unresolved_signature_type_ref(
                    scope,
                    lexical,
                    component,
                    include_scope_owner_body,
                );
            }
        }
        reference
    }

    pub(super) fn signature_type_ref_at(
        &self,
        scope: crate::fir::SignatureScope,
        lexical: &super::super::CheckerScope<'_>,
        reference: &TypeRef,
        include_scope_owner_body: bool,
    ) -> Option<Ty> {
        self.signature_type_ref_at_with_site(
            scope,
            lexical,
            reference,
            include_scope_owner_body,
            None,
        )
    }

    pub(super) fn selected_type_child(
        &self,
        site: Option<SelectedTypeSite>,
        reference: &TypeRef,
        child: &TypeRef,
    ) -> Option<SelectedTypeSite> {
        let site = site?;
        if !site.project_children {
            return None;
        }
        let ty = self.headers.syntax.ty(site.syntax?)?;
        let syntax = match ty.kind {
            crate::fir::HeaderTypeKind::Classifier {
                detail,
                abbreviated_argument,
            } => {
                if let Some(index) = reference
                    .targs
                    .iter()
                    .position(|candidate| std::ptr::eq(candidate, child))
                {
                    let detail = self.headers.syntax.classifier_type(detail)?;
                    *self
                        .headers
                        .syntax
                        .type_operands(detail.arguments)
                        .get(index)?
                } else if reference
                    .arg
                    .as_deref()
                    .is_some_and(|candidate| std::ptr::eq(candidate, child))
                {
                    abbreviated_argument?
                } else {
                    return None;
                }
            }
            crate::fir::HeaderTypeKind::Function {
                parameters, result, ..
            } => {
                if let Some(index) = reference
                    .fun_params
                    .iter()
                    .position(|candidate| std::ptr::eq(candidate, child))
                {
                    *self.headers.syntax.type_operands(parameters).get(index)?
                } else if reference
                    .arg
                    .as_deref()
                    .is_some_and(|candidate| std::ptr::eq(candidate, child))
                {
                    result?
                } else {
                    return None;
                }
            }
        };
        Some(SelectedTypeSite {
            syntax: Some(syntax),
            application: None,
            abbreviation_origin: None,
            project_children: true,
        })
    }
}
