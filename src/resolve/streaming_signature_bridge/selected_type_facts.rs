//! Type spelling facts recorded by authoritative compact type resolution.
//!
//! A later inferred-result publication may need the selected alias for Kotlin metadata. It reads
//! the origin-keyed abbreviation. Declaration metadata reads classifier boundaries, aliases, and
//! resolved argument types by structural `HeaderTypeId`; neither consumer repeats lookup.

use crate::fir::{HeaderTypeId, OriginId, ResolvedTypeAbbreviation, SigExprId};
use crate::fx_hash::HashMap;
use std::{cell::RefCell, rc::Rc};

#[cfg(test)]
mod tests;

#[derive(Clone, Default)]
pub(super) struct SelectedTypeFacts {
    shared: Rc<RefCell<SelectedTypeFactsData>>,
}

#[derive(Default)]
struct SelectedTypeFactsData {
    abbreviations: HashMap<OriginId, ResolvedTypeAbbreviation>,
    graph_spellings: HashMap<HeaderTypeId, super::super::type_spellings::ResolvedTypeSpelling>,
    spellings: HashMap<HeaderTypeId, super::super::type_spellings::ResolvedTypeSpelling>,
    applications: HashMap<SigExprId, super::super::type_spellings::ResolvedTypeSpelling>,
    local_template_nodes: HashMap<
        crate::fir::LocalTypeTemplateNodeId,
        super::super::type_spellings::ResolvedTypeSpelling,
    >,
    local_alias_projections: HashMap<
        HeaderTypeId,
        super::bound_local_alias_templates::AppliedLocalTypeSpellingProjection,
    >,
}

impl SelectedTypeFacts {
    fn project_to_declaration_sites(
        &self,
        spelling: Option<super::super::type_spellings::ResolvedTypeSpelling>,
        resolved: crate::types::Ty,
        sites: &[HeaderTypeId],
    ) -> bool {
        if sites.is_empty() {
            return true;
        }
        let Some(mut spelling) = spelling else {
            return false;
        };
        spelling.resolved = Some(resolved);
        for &site in sites {
            self.record_spelling(site, spelling);
        }
        true
    }

    pub(super) fn project_application_to_declaration_sites(
        &self,
        application: SigExprId,
        resolved: crate::types::Ty,
        sites: &[HeaderTypeId],
    ) -> bool {
        self.project_to_declaration_sites(self.application(application), resolved, sites)
    }

    pub(super) fn project_graph_type_to_declaration_sites(
        &self,
        syntax: HeaderTypeId,
        resolved: crate::types::Ty,
        sites: &[HeaderTypeId],
    ) -> bool {
        self.project_to_declaration_sites(self.graph_spelling(syntax), resolved, sites)
    }

    pub(super) fn record(&self, origin: OriginId, abbreviation: ResolvedTypeAbbreviation) {
        self.shared
            .borrow_mut()
            .abbreviations
            .insert(origin, abbreviation);
    }

    pub(super) fn abbreviation(&self, origin: OriginId) -> Option<ResolvedTypeAbbreviation> {
        self.shared.borrow().abbreviations.get(&origin).cloned()
    }

    pub(super) fn record_spelling(
        &self,
        syntax: HeaderTypeId,
        spelling: super::super::type_spellings::ResolvedTypeSpelling,
    ) {
        self.shared.borrow_mut().spellings.insert(syntax, spelling);
    }

    pub(super) fn record_graph_spelling(
        &self,
        syntax: HeaderTypeId,
        spelling: super::super::type_spellings::ResolvedTypeSpelling,
    ) {
        self.shared
            .borrow_mut()
            .graph_spellings
            .insert(syntax, spelling);
    }

    pub(super) fn graph_spelling(
        &self,
        syntax: HeaderTypeId,
    ) -> Option<super::super::type_spellings::ResolvedTypeSpelling> {
        self.shared.borrow().graph_spellings.get(&syntax).copied()
    }

    pub(super) fn spelling(
        &self,
        syntax: HeaderTypeId,
    ) -> Option<super::super::type_spellings::ResolvedTypeSpelling> {
        self.shared.borrow().spellings.get(&syntax).copied()
    }

    /// Whether compact resolution selected every node needed to publish one declaration-owned
    /// written type. A graph edge records only potential ownership; an undemanded graph expression
    /// never publishes these facts and therefore leaves the occurrence to active Pass 2.
    pub(super) fn has_complete_declaration_spelling(
        &self,
        syntax: &crate::fir::HeaderSyntaxArena,
        root: HeaderTypeId,
    ) -> bool {
        let Some(nodes) = syntax.resolved_written_spelling_preorder(root, &mut |node| {
            self.spelling(node)
                .map(|spelling| spelling.classifier_segment)
        }) else {
            return false;
        };
        nodes.into_iter().all(|node| {
            self.spelling(node)
                .is_some_and(|spelling| spelling.resolved.is_some())
        })
    }

    pub(super) fn record_application(
        &self,
        expression: SigExprId,
        spelling: super::super::type_spellings::ResolvedTypeSpelling,
    ) {
        self.shared
            .borrow_mut()
            .applications
            .insert(expression, spelling);
    }

    pub(super) fn application(
        &self,
        expression: SigExprId,
    ) -> Option<super::super::type_spellings::ResolvedTypeSpelling> {
        self.shared.borrow().applications.get(&expression).copied()
    }

    pub(super) fn record_local_alias_projection(
        &self,
        syntax: HeaderTypeId,
        projection: super::bound_local_alias_templates::AppliedLocalTypeSpellingProjection,
    ) {
        self.shared
            .borrow_mut()
            .local_alias_projections
            .insert(syntax, projection);
    }

    pub(super) fn local_alias_projection(
        &self,
        syntax: HeaderTypeId,
    ) -> Option<super::bound_local_alias_templates::AppliedLocalTypeSpellingProjection> {
        self.shared
            .borrow()
            .local_alias_projections
            .get(&syntax)
            .cloned()
    }

    pub(super) fn record_local_template_node(
        &self,
        node: crate::fir::LocalTypeTemplateNodeId,
        spelling: super::super::type_spellings::ResolvedTypeSpelling,
    ) {
        self.shared
            .borrow_mut()
            .local_template_nodes
            .insert(node, spelling);
    }

    pub(super) fn local_template_node(
        &self,
        node: crate::fir::LocalTypeTemplateNodeId,
    ) -> Option<super::super::type_spellings::ResolvedTypeSpelling> {
        self.shared
            .borrow()
            .local_template_nodes
            .get(&node)
            .copied()
    }
}
