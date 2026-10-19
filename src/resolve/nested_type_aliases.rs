//! Typealiases declared in a classifier body, selected by their already-resolved owner's identity.
//!
//! The owner may be a nested or hoisted local classifier whose runtime name bears no relation to
//! the source path, so a nested alias is found through the owner's declaration and never by
//! appending the alias spelling to the owner's runtime internal name. The three stores below own
//! disjoint declaration lifetimes: a body-local class's aliases are bound on its class rung and
//! absent from the module index, and a module is checked either over its resolved index or over
//! the legacy pass-one symbols.

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
        let signature = self.module.legacy_symbols()?.class_by_type_name(owner)?;
        let declaration = if signature.source_file == self.file_index {
            match self.active_declarations {
                Some(active) => active.class(self.file, signature.stable_declaration?)?.1,
                None => match self.file.decl(signature.source_decl?) {
                    Decl::Class(declaration) => declaration,
                    Decl::Fun(_) | Decl::Property(_) => return None,
                },
            }
        } else {
            let source = self
                .source_files
                .and_then(|files| files.get(signature.source_file as usize))?;
            match source.decl(signature.source_decl?) {
                Decl::Class(declaration) => declaration,
                Decl::Fun(_) | Decl::Property(_) => return None,
            }
        };
        if !declaration
            .type_aliases
            .iter()
            .any(|candidate| candidate.name == alias)
        {
            return None;
        }
        let package = if signature.source_file == self.file_index {
            self.file.package.as_deref()
        } else {
            self.source_files
                .and_then(|files| files.get(signature.source_file as usize))?
                .package
                .as_deref()
        }
        .unwrap_or("")
        .replace('.', "/");
        let declaration_owner = declaration.name.replace('.', "/");
        let identity = type_name(&if package.is_empty() {
            format!("{declaration_owner}/{alias}")
        } else {
            format!("{package}/{declaration_owner}/{alias}")
        });
        self.source_alias_declared(identity).then_some(identity)
    }
}
