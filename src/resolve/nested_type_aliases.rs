//! Typealiases declared in a classifier body, selected by their already-resolved owner's identity.
//!
//! The owner may be a nested or hoisted local classifier whose runtime name bears no relation to
//! the source path, so a nested alias is found through the owner's declaration and never by
//! appending the alias spelling to the owner's runtime internal name. The three stores below own
//! disjoint declaration lifetimes: a body-local class's aliases are bound on its class rung and
//! absent from the module index, and a module is checked either over its resolved index or over
//! the legacy pass-one symbols. Each store keeps the qualified identity the alias's declaration
//! was published under, keyed by its owner classifier.

use super::classifier_binding::SelectedTypeAlias;
use super::*;

impl Checker<'_> {
    /// The typealias `name` declared in the body of the classifier `owner`.
    pub(super) fn nested_source_type_alias(
        &self,
        owner: TypeName,
        name: &str,
    ) -> Option<SelectedTypeAlias> {
        if let Some(alias) = self.body_local_type_aliases.get(&(owner, name.into())) {
            return Some(SelectedTypeAlias {
                identity: None,
                formals: alias.formals.clone(),
                expansion: alias.expansion,
                expansion_spelling: alias.expansion_spelling.clone(),
            });
        }
        let identity = self.nested_source_alias_identity(owner, name)?;
        let binding = self.source_alias_binding(identity)?;
        Some(SelectedTypeAlias {
            identity: Some(identity),
            formals: binding.formals,
            expansion: binding.expansion,
            expansion_spelling: binding.expansion_spelling,
        })
    }

    /// The declaration identity of the module typealias `alias` declared in `owner`'s body.
    pub(super) fn nested_source_alias_identity(
        &self,
        owner: TypeName,
        alias: &str,
    ) -> Option<TypeName> {
        if let Some(index) = self.resolved_index {
            return index
                .type_alias_in_classifier(owner, alias)
                .map(|header| header.identity);
        }
        self.module
            .legacy_symbols()?
            .nested_type_aliases
            .get(&(owner, alias.to_owned()))
            .copied()
    }
}
