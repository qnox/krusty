//! The declaring-class application of each selected member's dispatch receiver.
//!
//! A member declared on a generic class reads the class's type parameters. When the call's
//! receiver names a subclass (`Derived : Base<String>`), the receiver's own type arguments do
//! not bind them; its applied supertype does. The checker publishes that application on the
//! selected member once the selection is final, so lowering binds the parameters from a checked
//! fact rather than walking a hierarchy.

use super::{Checker, ResolvedCall};
use crate::symbol_resolver::ResolvedMember;
use crate::types::Ty;

impl Checker<'_> {
    /// Record on every selected member call whose receiver is a proper subclass of a generic
    /// declaring class the receiver applied as that class.
    pub(super) fn publish_member_declaring_receivers(&mut self) {
        let mut calls = std::mem::take(&mut self.resolved_calls);
        let mut operators = std::mem::take(&mut self.resolved_operator_calls);
        let mut statements = std::mem::take(&mut self.resolved_stmt_operator_calls);
        {
            let source = self.fed_source();
            let selected = calls
                .values_mut()
                .chain(operators.values_mut())
                .chain(statements.values_mut());
            for call in selected {
                if let ResolvedCall::Member(selected) = call {
                    selected.declaring_receiver = declaring_receiver(&source, selected);
                }
            }
        }
        self.resolved_calls = calls;
        self.resolved_operator_calls = operators;
        self.resolved_stmt_operator_calls = statements;
    }
}

/// `selected.receiver` applied as the member's declaring class when that class is generic and
/// the receiver names one of its proper subclasses.
fn declaring_receiver(
    source: &dyn crate::symbol_source::SymbolSource,
    selected: &ResolvedMember,
) -> Option<Ty> {
    let owner = selected.member.owner?;
    let receiver = selected.receiver.non_null();
    if receiver.kotlin_class_internal()? == owner
        || source.classifier(owner)?.type_params.is_empty()
    {
        return None;
    }
    crate::symbol_resolver::applied_hierarchy(source, receiver)
        .into_iter()
        .find_map(|(classifier, applied, _)| (classifier == owner).then_some(applied))
}
