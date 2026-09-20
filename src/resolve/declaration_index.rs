//! Every source declaration of this module, by the stable identity it already carries.
//!
//! A reader that needs "what did this declaration resolve to?" used to walk the origin-specific
//! tables itself — `funs` then `ext_funs`, `source_props` then `ext_props`, and four more per
//! classifier. Each walk selected the right record, because the key was the declaration's own
//! identity and not a spelling, but it made every reader depend on how many tables a resolved
//! declaration may be published into and on which order to try them; adding a table meant
//! changing each of them, and forgetting one made a declaration answer differently depending on
//! who asked.
//!
//! Resolution publishes the index instead — once, here, at the boundary that owns the tables.
//! Each table is walked one time and each record entered under the identity it carries; every
//! reader does lookups.
//!
//! Two records claiming ONE identity is a broken contract, not a last-writer-wins. An ordinary
//! `insert` silently dropped one of them and answered with whichever arrived second, so a
//! collision is recorded and reported to the caller rather than settled by arrival order.

use super::{
    ClassSig, DeclaredPropertySig, ExtPropSig, MemberExtPropSig, Signature, SourcePropertySig,
    SymbolTable,
};
use crate::fir::DeclarationId;
use std::collections::{HashMap, HashSet};

/// What one resolved source declaration is.
pub enum ResolvedSourceDeclaration<'symbols> {
    Function(&'symbols Signature),
    Property(&'symbols SourcePropertySig),
    ExtensionProperty(&'symbols ExtPropSig),
    Classifier(&'symbols ClassSig),
    /// A member property. A classifier keeps its ordinary members and its member EXTENSION
    /// properties in separate tables with separate record types, and the source wrote one
    /// declaration either way.
    MemberProperty(&'symbols DeclaredPropertySig),
    MemberExtensionProperty(&'symbols MemberExtPropSig),
}

/// The published index.
pub struct ResolvedSourceDeclarations<'symbols> {
    by_identity: HashMap<DeclarationId, ResolvedSourceDeclaration<'symbols>>,
    /// Identities more than one published record claimed. A reader is told so rather than handed
    /// one of them.
    conflicted: HashSet<DeclarationId>,
}

impl<'symbols> ResolvedSourceDeclarations<'symbols> {
    /// Walk each published table once and index it by the identity its records already carry.
    pub(crate) fn publish(symbols: &'symbols SymbolTable) -> Self {
        let mut index = Self {
            by_identity: HashMap::new(),
            conflicted: HashSet::new(),
        };
        for signature in symbols.funs.values().flatten().chain(
            symbols
                .ext_funs
                .values()
                .flat_map(|receivers| receivers.values())
                .flatten(),
        ) {
            index.enter(
                signature.stable_declaration,
                ResolvedSourceDeclaration::Function(signature),
            );
        }
        for property in symbols.source_props.values() {
            index.enter(
                property.stable_declaration,
                ResolvedSourceDeclaration::Property(property),
            );
        }
        for property in symbols.ext_props.values().flatten() {
            index.enter(
                property.stable_declaration,
                ResolvedSourceDeclaration::ExtensionProperty(property),
            );
        }
        for class in symbols.classes.values() {
            index.enter(
                class.stable_declaration,
                ResolvedSourceDeclaration::Classifier(class),
            );
            // A classifier's members are declarations too, and they live in four tables of their
            // own. Publishing them here is what lets a member be asked for by identity instead of
            // entering a name-indexed table and filtering what comes back.
            for signature in class.methods.values().flatten() {
                index.enter(
                    signature.stable_declaration,
                    ResolvedSourceDeclaration::Function(signature),
                );
            }
            for function in class.member_ext_funs.values().flatten() {
                let signature = function.signature();
                index.enter(
                    signature.stable_declaration,
                    ResolvedSourceDeclaration::Function(signature),
                );
            }
            for property in class.declared_props.values() {
                index.enter(
                    property.stable_declaration,
                    ResolvedSourceDeclaration::MemberProperty(property),
                );
            }
            for property in class.member_ext_props.values().flatten() {
                index.enter(
                    property.stable_declaration(),
                    ResolvedSourceDeclaration::MemberExtensionProperty(property),
                );
            }
        }
        index
    }

    /// A record with no stable identity belongs to no source declaration — a generated member, or
    /// one published before identities were interned — and is simply not in the index. A SECOND
    /// record under an identity already claimed is the contract failure.
    fn enter(
        &mut self,
        declaration: Option<DeclarationId>,
        resolved: ResolvedSourceDeclaration<'symbols>,
    ) {
        let Some(declaration) = declaration else {
            return;
        };
        if self.by_identity.insert(declaration, resolved).is_some() {
            self.conflicted.insert(declaration);
        }
    }

    /// What this identity resolved to, or `None` when nothing published it — or when more than one
    /// record did, which [`Self::is_conflicted`] tells apart from an absence.
    pub(crate) fn get(
        &self,
        declaration: DeclarationId,
    ) -> Option<&ResolvedSourceDeclaration<'symbols>> {
        if self.conflicted.contains(&declaration) {
            return None;
        }
        self.by_identity.get(&declaration)
    }

    /// More than one published record claimed this identity.
    pub(crate) fn is_conflicted(&self, declaration: DeclarationId) -> bool {
        self.conflicted.contains(&declaration)
    }
}

impl SymbolTable {
    pub(crate) fn class_by_stable_declaration(
        &self,
        declaration: DeclarationId,
    ) -> Option<&ClassSig> {
        self.classes
            .values()
            .find(|class| class.stable_declaration == Some(declaration))
    }
}
