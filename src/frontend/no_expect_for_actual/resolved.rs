//! What this check reads from resolution, and the one thing it adds.
//!
//! The diagnostic used to answer "what did this `actual` resolve to?" by walking `SymbolTable`'s
//! origin-specific tables — `funs`, then `ext_funs`; `source_props`, then `ext_props` — once per
//! declaration it reported. Each walk was keyed by the declaration's own source coordinate rather
//! than by a spelling, so it selected the right record, but it made a diagnostic pass depend on
//! how many tables a resolved declaration might be published into and on which order to try them.
//!
//! Resolution publishes that index itself now, at the boundary that owns the tables
//! ([`crate::resolve::declaration_index`]), and this module does not rebuild it: it holds the
//! published index and does lookups. An identity two records both claim is reported by the index
//! rather than settled by arrival order, and this check turns that into an internal error at the
//! declaration rather than rendering one of them.
//!
//! A source `typealias` is the one declaration with no resolved signature to carry an identity:
//! it publishes a type EXPANSION, keyed by the alias's own fully-qualified name. That name is the
//! alias's identity as a declared type — package-qualified, with no arity, receiver or overload
//! set for it to be ambiguous about — and the alias's DECLARATION identity still comes from the
//! compact header inventory, like every other declaration's. Joining those two is the one thing
//! this module adds, because both halves are the frontend's.

use crate::resolve::declaration_index::{ResolvedSourceDeclaration, ResolvedSourceDeclarations};
use crate::resolve::{ClassSig, SymbolTable};
use crate::types::{Ty, TypeName};
use std::collections::HashMap;

/// What one resolved source declaration contributes to a rendering: resolution's own record,
/// under the name this check has always called it by.
pub(super) type Resolved<'symbols> = ResolvedSourceDeclaration<'symbols>;

/// Every source declaration the check can be asked about, by stable declaration identity.
pub(super) struct ResolvedDeclarations<'symbols> {
    published: ResolvedSourceDeclarations<'symbols>,
    aliases: &'symbols HashMap<TypeName, (Vec<String>, Ty)>,
    /// A source `typealias`'s qualified identity as a declared TYPE, by its declaration identity.
    ///
    /// The expansion table is keyed by that qualified name, which is what a use site resolves; the
    /// alias's DECLARATION is identified like every other declaration's. Both are published from
    /// the compact header inventory once, so a reader asks by the declaration it holds rather than
    /// rebuilding a name from the syntax it came from.
    alias_identities: HashMap<crate::fir::DeclarationId, TypeName>,
}

impl<'symbols> ResolvedDeclarations<'symbols> {
    /// Take resolution's published index and join it with the alias identities the compact header
    /// inventory holds.
    pub(super) fn publish(
        symbols: &'symbols SymbolTable,
        headers: &crate::fir::StreamedHeaderModule,
    ) -> Self {
        // The alias's qualified identity, from the inventory's own record of what it declared:
        // the package of the file it is in and the name it published.
        let mut alias_identities = HashMap::new();
        for stub in &headers.stubs {
            if stub.kind != crate::fir::DeclarationKind::TypeAlias {
                continue;
            }
            let Some(spelling) = stub
                .lookup_name
                .and_then(|name| headers.lookup_names.get(name))
            else {
                continue;
            };
            let Some(package) = headers
                .sources
                .get(stub.source)
                .map(|source| source.package)
            else {
                continue;
            };
            alias_identities.insert(stub.id, crate::types::type_name_child(package, spelling));
        }
        Self {
            published: ResolvedSourceDeclarations::publish(symbols),
            aliases: &symbols.source_alias_expansions,
            alias_identities,
        }
    }

    pub(super) fn get(
        &self,
        declaration: crate::fir::DeclarationId,
    ) -> Option<&Resolved<'symbols>> {
        self.published.get(declaration)
    }

    /// More than one published record claimed this identity, so there is no single answer to
    /// render. Distinguished from an absence because the two are different broken contracts and a
    /// diagnostic that cannot be produced must say which one it met.
    pub(super) fn is_conflicted(&self, declaration: crate::fir::DeclarationId) -> bool {
        self.published.is_conflicted(declaration)
    }

    /// The classifier this identity declares, or `None` where the identity is not a classifier's.
    pub(super) fn classifier(
        &self,
        declaration: crate::fir::DeclarationId,
    ) -> Option<&'symbols ClassSig> {
        match self.published.get(declaration)? {
            Resolved::Classifier(class) => Some(class),
            Resolved::Function(_)
            | Resolved::Property(_)
            | Resolved::ExtensionProperty(_)
            | Resolved::MemberProperty(_)
            | Resolved::MemberExtensionProperty(_) => None,
        }
    }

    /// A source `typealias`'s own formal names and its resolved expansion, by the alias's
    /// DECLARATION identity.
    pub(super) fn alias_expansion(
        &self,
        declaration: crate::fir::DeclarationId,
    ) -> Option<&(Vec<String>, Ty)> {
        self.aliases.get(self.alias_identities.get(&declaration)?)
    }
}
