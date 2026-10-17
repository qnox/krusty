//! Type spelling facts recorded by authoritative compact type resolution.
//!
//! A later inferred-result publication may need the selected alias for Kotlin metadata. It reads
//! the origin-keyed abbreviation. Declaration metadata reads classifier boundaries, aliases, and
//! resolved argument types by structural `HeaderTypeId`; neither consumer repeats lookup.

use crate::fir::{HeaderTypeId, OriginId, ResolvedTypeAbbreviation, SigExprId};
use crate::fx_hash::HashMap;
use std::{cell::RefCell, rc::Rc};

#[derive(Clone, Default)]
pub(super) struct SelectedTypeFacts {
    shared: Rc<RefCell<SelectedTypeFactsData>>,
}

#[derive(Default)]
struct SelectedTypeFactsData {
    abbreviations: HashMap<OriginId, ResolvedTypeAbbreviation>,
    spellings: HashMap<HeaderTypeId, super::super::type_spellings::ResolvedTypeSpelling>,
    applications: HashMap<SigExprId, super::super::type_spellings::ResolvedTypeSpelling>,
}

impl SelectedTypeFacts {
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

    pub(super) fn spelling(
        &self,
        syntax: HeaderTypeId,
    ) -> Option<super::super::type_spellings::ResolvedTypeSpelling> {
        self.shared.borrow().spellings.get(&syntax).copied()
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
}
