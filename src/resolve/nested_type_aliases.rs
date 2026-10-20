//! The classifier a qualifier segment selects inside an already-resolved owner.
//!
//! The owner may be a nested or hoisted local classifier whose runtime name bears no relation to
//! the source path, so a segment is looked up in the owner's classifier namespace by the owner's
//! identity, never by appending it to the owner's runtime internal name. The aliases body-local
//! classifiers declare never enter a module or dependency provider, so they are published as a
//! [`SymbolSource`] of their own: each under the stable identity its declaration has in its
//! owner's namespace, in the same record shape the providers return. A segment is then one
//! `symbols` query over that source federated over the providers, and selection reads the record
//! without knowing which declaration lifetime supplied it.

use super::classifier_binding::SelectedTypeAlias;
use super::*;
use crate::libraries::{AliasExpansion, ClassifierDeclaration, ResolvedSymbols};
use crate::symbol_source::{CompositeSource, SymbolNamespace};

/// The typealiases declared in the bodies of the body-local classifiers checked so far, each as
/// the declaration its owner's namespace holds.
#[derive(Default)]
pub(super) struct BodyLocalTypeAliases(HashMap<(TypeName, Box<str>), AliasExpansion>);

impl BodyLocalTypeAliases {
    /// Publish the alias `name` declared in the body of `owner`, under its identity in `owner`'s
    /// classifier namespace.
    pub(super) fn declare(&mut self, owner: TypeName, name: &str, alias: &LexicalTypeAlias) {
        self.0.insert(
            (owner, name.into()),
            AliasExpansion {
                identity: crate::types::type_name_child(owner, name),
                target: alias.target,
                formals: alias.formals.clone(),
                expansion: alias.expansion,
                expansion_spelling: alias.expansion_spelling.clone(),
            },
        );
    }
}

impl BodyLocalTypeAliases {
    /// The alias published under `identity`, which only this source assigns.
    pub(super) fn binding(&self, identity: TypeName) -> Option<AliasExpansion> {
        self.0
            .values()
            .find(|alias| alias.identity == identity)
            .cloned()
    }
}

impl SymbolSource for BodyLocalTypeAliases {
    fn symbols(&self, namespace: SymbolNamespace, name: &str) -> std::rc::Rc<ResolvedSymbols> {
        let alias = match namespace {
            SymbolNamespace::Classifier(owner) => self.0.get(&(owner, name.into())),
            SymbolNamespace::Package(_) => None,
        };
        std::rc::Rc::new(
            alias.map_or_else(ResolvedSymbols::default, |alias| ResolvedSymbols {
                classifier_name: Some(alias.target),
                classifier_declaration: Some(ClassifierDeclaration::TypeAlias(alias.clone())),
                ..ResolvedSymbols::default()
            }),
        )
    }
}

/// The classifier one segment names in its owner, with the typealias declaration that named it.
pub(super) struct NestedClassifier {
    pub(super) classifier: TypeName,
    pub(super) alias: Option<SelectedTypeAlias>,
}

impl Checker<'_> {
    /// The record `name` has in `namespace`: the body-local declarations federated over the module
    /// and dependency providers, as one query.
    fn namespace_symbols(
        &self,
        namespace: SymbolNamespace,
        name: &str,
    ) -> std::rc::Rc<ResolvedSymbols> {
        let providers = self.fed_source();
        CompositeSource::new(vec![&self.body_local_type_aliases, &providers])
            .symbols(namespace, name)
    }

    /// The typealias a qualified spelling `name` selects. Its prefix is resolved once, left to
    /// right: a root some classifier rung binds commits the prefix to a classifier and never
    /// backtracks to a same-named package, and only an unbound root continues as a package path.
    /// The last segment is then read from that qualifier's namespace in one query.
    pub(super) fn qualified_type_alias(
        &self,
        scope: &CheckerScope<'_>,
        name: &str,
    ) -> Option<AliasExpansion> {
        let (prefix, alias) = name.rsplit_once(['.', '/'])?;
        let namespace = match self.select_classifier(scope, prefix) {
            InheritedNestedClassifier::Found(owner) => SymbolNamespace::Classifier(owner),
            InheritedNestedClassifier::Ambiguous => return None,
            InheritedNestedClassifier::NotFound => {
                SymbolNamespace::Package(self.unbound_root_package(scope, prefix)?)
            }
        };
        match self
            .namespace_symbols(namespace, alias)
            .classifier_declaration
            .clone()?
        {
            ClassifierDeclaration::TypeAlias(alias) => Some(alias),
            ClassifierDeclaration::Ordinary(_) => None,
        }
    }

    /// The package `prefix` spells when no classifier rung binds its root segment.
    fn unbound_root_package(&self, scope: &CheckerScope<'_>, prefix: &str) -> Option<TypeName> {
        let mut segments = prefix
            .split(['.', '/'])
            .filter(|segment| !segment.is_empty());
        let root = segments.next()?;
        if self.select_classifier(scope, root) != InheritedNestedClassifier::NotFound {
            return None;
        }
        let source = self.fed_source();
        std::iter::once(root)
            .chain(segments)
            .try_fold(TypeName::ROOT, |package, segment| {
                source
                    .package_exists(package, segment)
                    .then(|| crate::types::type_name_child(package, segment))
            })
    }

    /// The classifier `name` selects in the body of the classifier `owner`: a nested classifier,
    /// or the target of a nested typealias.
    pub(super) fn nested_classifier(
        &self,
        owner: TypeName,
        name: &str,
    ) -> Option<NestedClassifier> {
        let record = self.namespace_symbols(SymbolNamespace::Classifier(owner), name);
        let classifier = record.classifier_name?;
        let alias = self
            .selected_provider_alias(classifier, record.classifier_declaration.as_ref())
            .ok()?;
        Some(NestedClassifier { classifier, alias })
    }
}
