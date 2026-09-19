//! The resolved declaration facts this check renders, published once at the signature boundary.
//!
//! The diagnostic used to answer "what did this `actual` resolve to?" by walking `SymbolTable`'s
//! origin-specific tables — `funs`, then `ext_funs`; `source_props`, then `ext_props` — once per
//! declaration it reported. Each walk was keyed by the declaration's own source coordinate rather
//! than by a spelling, so it selected the right record, but it made a diagnostic pass depend on
//! how many tables a resolved declaration might be published into and on which order to try them.
//!
//! This is the boundary contract instead. Every source declaration that carries a stable identity
//! is published here ONCE, keyed by that identity, and the check does lookups only. A declaration
//! reachable through two tables cannot answer differently depending on which is consulted first,
//! and adding a third table is a change here rather than in every reader.
//!
//! A source `typealias` is the one declaration with no resolved signature to carry an identity:
//! it publishes a type EXPANSION, keyed by the alias's own fully-qualified name. That name is the
//! alias's identity as a declared type — package-qualified, with no arity, receiver or overload
//! set for it to be ambiguous about — and the alias's DECLARATION identity still comes from the
//! compact header inventory, like every other declaration's.

use crate::resolve::{ClassSig, ExtPropSig, Signature, SourcePropertySig, SymbolTable};
use crate::types::{Ty, TypeName};
use std::collections::HashMap;

/// What one resolved source declaration contributes to a rendering.
pub(super) enum Resolved<'symbols> {
    Function(&'symbols Signature),
    Property(&'symbols SourcePropertySig),
    ExtensionProperty(&'symbols ExtPropSig),
    Classifier(&'symbols ClassSig),
}

/// Every source declaration the check can be asked about, by stable declaration identity.
pub(super) struct ResolvedDeclarations<'symbols> {
    by_identity: HashMap<crate::fir::DeclarationId, Resolved<'symbols>>,
    aliases: &'symbols HashMap<TypeName, (Vec<String>, Ty)>,
}

impl<'symbols> ResolvedDeclarations<'symbols> {
    /// Walk each published table once and index it by the identity its records already carry.
    pub(super) fn publish(symbols: &'symbols SymbolTable) -> Self {
        let mut by_identity = HashMap::new();
        for signature in symbols.funs.values().flatten().chain(
            symbols
                .ext_funs
                .values()
                .flat_map(|receivers| receivers.values())
                .flatten(),
        ) {
            if let Some(declaration) = signature.stable_declaration {
                by_identity.insert(declaration, Resolved::Function(signature));
            }
        }
        for property in symbols.source_props.values() {
            if let Some(declaration) = property.stable_declaration {
                by_identity.insert(declaration, Resolved::Property(property));
            }
        }
        for property in symbols.ext_props.values().flatten() {
            if let Some(declaration) = property.stable_declaration {
                by_identity.insert(declaration, Resolved::ExtensionProperty(property));
            }
        }
        for class in symbols.classes.values() {
            if let Some(declaration) = class.stable_declaration {
                by_identity.insert(declaration, Resolved::Classifier(class));
            }
        }
        Self {
            by_identity,
            aliases: &symbols.source_alias_expansions,
        }
    }

    pub(super) fn get(
        &self,
        declaration: crate::fir::DeclarationId,
    ) -> Option<&Resolved<'symbols>> {
        self.by_identity.get(&declaration)
    }

    /// The classifier this identity declares, or `None` where the identity is not a classifier's.
    pub(super) fn classifier(
        &self,
        declaration: crate::fir::DeclarationId,
    ) -> Option<&'symbols ClassSig> {
        match self.by_identity.get(&declaration)? {
            Resolved::Classifier(class) => Some(class),
            Resolved::Function(_) | Resolved::Property(_) | Resolved::ExtensionProperty(_) => None,
        }
    }

    /// A source `typealias`'s own formal names and its resolved expansion.
    pub(super) fn alias_expansion(&self, qualified: &str) -> Option<&(Vec<String>, Ty)> {
        self.aliases.get(&crate::types::type_name(qualified))
    }
}
