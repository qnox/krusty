//! Bind-once classifier selection, including the exact typealias declaration (when any) that won
//! the root scope-tower rung. Consumers apply the returned alias template directly; they never
//! rediscover alias-ness from the source spelling after selection.

use super::*;

/// The typealias binding selected on the classifier tower together with its expanded classifier.
/// A declared alias carries its qualified identity. `identity` is absent only for a statement-local
/// alias, whose lexical scope slot is itself the stable binding for the lifetime of this check.
#[derive(Clone)]
pub(super) struct SelectedTypeAlias {
    pub(super) identity: Option<TypeName>,
    pub(super) formals: Vec<String>,
    pub(super) expansion: Ty,
}

impl Checker<'_> {
    /// Select the classifier root from the scope tower, then commit every remaining segment through
    /// the shared qualifier loop. The third result is the alias binding from the same winning root
    /// rung. There is no import/module/classpath retry after this returns.
    pub(super) fn select_classifier_binding(
        &self,
        scope: &CheckerScope<'_>,
        name: &str,
    ) -> (
        InheritedNestedClassifier,
        Option<String>,
        Option<SelectedTypeAlias>,
    ) {
        let segments = name
            .split(['.', '/'])
            .filter(|segment| !segment.is_empty())
            .map(|segment| (None, segment.to_string()))
            .collect::<Vec<_>>();
        let Some((_, root_name)) = segments.first() else {
            return (
                InheritedNestedClassifier::NotFound,
                Some(name.to_string()),
                None,
            );
        };
        let source = self.fed_source();
        let mut selected_alias = None;
        let scoped = scope.symbols(root_name, &source);
        let root = if let Some(internal) = scoped.classifier_name {
            ResolvedQualifier::Classifier(internal)
        } else if let Some((internal, alias)) = self.selected_lexical_type_alias(scope, root_name) {
            selected_alias = Some(alias);
            ResolvedQualifier::Classifier(internal)
        } else if let Some(internal) = self.classifier_header_lexical_type_name(root_name) {
            ResolvedQualifier::Classifier(internal)
        } else if let Some(internal) = self.enclosing_nested_type_name(root_name) {
            ResolvedQualifier::Classifier(internal)
        } else {
            match self.inherited_nested_type_name(root_name) {
                InheritedNestedClassifier::Found(internal) => {
                    ResolvedQualifier::Classifier(internal)
                }
                InheritedNestedClassifier::Ambiguous => {
                    return (
                        InheritedNestedClassifier::Ambiguous,
                        Some(root_name.clone()),
                        None,
                    );
                }
                InheritedNestedClassifier::NotFound => {
                    if let Some((classifier, declaration)) =
                        self.explicit_import_classifier_binding(root_name)
                    {
                        selected_alias = declaration.and_then(|identity| {
                            self.selected_alias_from_identity(identity, classifier)
                        });
                        ResolvedQualifier::Classifier(classifier)
                    } else if let Some((classifier, alias)) =
                        self.selected_same_package_classifier(root_name)
                    {
                        selected_alias = alias;
                        ResolvedQualifier::Classifier(classifier)
                    } else if let Some(classifier) =
                        self.alias_ahead_of_imported_classifier(scope, root_name, &source)
                    {
                        selected_alias =
                            self.selected_scoped_type_alias(scope, root_name, classifier);
                        ResolvedQualifier::Classifier(classifier)
                    } else {
                        let imported = classifier_from_imports(
                            root_name,
                            &self.imports,
                            &self.import_levels,
                            &source,
                        );
                        crate::trace_compiler!(
                            "resolve",
                            "classifier root={root_name} imported={:?}",
                            imported.found().map(TypeName::render)
                        );
                        match imported {
                            InheritedNestedClassifier::Found(internal) => {
                                selected_alias =
                                    self.selected_imported_type_alias(root_name, internal);
                                ResolvedQualifier::Classifier(internal)
                            }
                            InheritedNestedClassifier::Ambiguous => {
                                return (
                                    InheritedNestedClassifier::Ambiguous,
                                    Some(root_name.clone()),
                                    None,
                                );
                            }
                            InheritedNestedClassifier::NotFound => {
                                match self
                                    .classifier_header_owner
                                    .map_or(InheritedNestedClassifier::NotFound, |owner| {
                                        self.inherited_nested_type_for_owner(root_name, owner)
                                    }) {
                                    InheritedNestedClassifier::Found(internal) => {
                                        ResolvedQualifier::Classifier(internal)
                                    }
                                    InheritedNestedClassifier::Ambiguous => {
                                        return (
                                            InheritedNestedClassifier::Ambiguous,
                                            Some(root_name.clone()),
                                            None,
                                        );
                                    }
                                    InheritedNestedClassifier::NotFound
                                        if segments.len() > 1
                                            && source.package_exists(TypeName::ROOT, root_name) =>
                                    {
                                        ResolvedQualifier::Package(crate::types::type_name_child(
                                            TypeName::ROOT,
                                            root_name,
                                        ))
                                    }
                                    InheritedNestedClassifier::NotFound => {
                                        return (
                                            InheritedNestedClassifier::NotFound,
                                            Some(root_name.clone()),
                                            None,
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
        };
        match walk_qualifier_namespace_facets_with_declaration_identity(
            &source,
            root.classifier(),
            None,
            root_name,
            &segments[1..],
        ) {
            Ok((ResolvedQualifier::Classifier(internal), declaration_identity)) => {
                let internal = self.libraries.canonical_source_type_name(internal);
                if let Some(identity) = declaration_identity {
                    selected_alias = self.selected_alias_from_identity(identity, internal);
                }
                (
                    InheritedNestedClassifier::Found(internal),
                    None,
                    selected_alias,
                )
            }
            Ok((ResolvedQualifier::Value | ResolvedQualifier::Package(_), _)) => (
                InheritedNestedClassifier::NotFound,
                segments.last().map(|(_, segment)| segment.clone()),
                None,
            ),
            Err(QualifierError::UnresolvedSegment { name, .. })
            | Err(QualifierError::AmbiguousRoot { name, .. }) => {
                (InheritedNestedClassifier::NotFound, Some(name), None)
            }
            Err(QualifierError::NotANameChain { .. }) => (
                InheritedNestedClassifier::NotFound,
                Some(root_name.clone()),
                None,
            ),
        }
    }

    pub(super) fn select_classifier(
        &self,
        scope: &CheckerScope<'_>,
        name: &str,
    ) -> InheritedNestedClassifier {
        self.select_classifier_binding(scope, name).0
    }

    fn alias_target_classifier(&self, expansion: Ty) -> Option<TypeName> {
        match expansion.non_null() {
            Ty::Unit => Some(type_name("kotlin/Unit")),
            expansion => expansion
                .obj_internal()
                .map(|classifier| self.libraries.canonical_source_type_name(classifier)),
        }
    }

    fn selected_alias_from_identity(
        &self,
        identity: TypeName,
        classifier: TypeName,
    ) -> Option<SelectedTypeAlias> {
        let (formals, expansion) = self.source_alias_expansion(identity)?;
        (self.alias_target_classifier(expansion) == Some(classifier)).then_some(SelectedTypeAlias {
            identity: Some(identity),
            formals,
            expansion,
        })
    }

    /// Classifier and alias provenance from the current-package rung. The package namespace and
    /// source lookup name identify the declaration before the target classifier is selected, so an
    /// alias keeps its own stable identity even when the provider normalizes the classifier facet to
    /// the alias target.
    fn selected_same_package_classifier(
        &self,
        name: &str,
    ) -> Option<(TypeName, Option<SelectedTypeAlias>)> {
        let namespace = crate::symbol_source::SymbolNamespace::Package(self.source_package_name());
        let record = self.fed_source().symbols(namespace, name);
        let classifier = self
            .libraries
            .canonical_source_type_name(record.classifier_name?);
        let identity = namespace
            .existing_classifier(name)
            .expect("selected same-package classifier declaration must be interned");
        let alias = self.selected_alias_from_identity(identity, classifier);
        Some((classifier, alias))
    }

    /// Alias provenance from the exact star/default import rung that selected `classifier`.
    fn selected_imported_type_alias(
        &self,
        name: &str,
        classifier: TypeName,
    ) -> Option<SelectedTypeAlias> {
        if let CheckerModuleSymbols::Legacy(source) = &self.module {
            if let Some(alias) = source
                .pass_one_symbols()
                .class_names
                .alias_expansion(name)
                .filter(|alias| alias.target == classifier)
            {
                return Some(SelectedTypeAlias {
                    identity: Some(alias.identity),
                    formals: alias.formals.clone(),
                    expansion: alias.expansion,
                });
            }
        }

        let source = self.fed_source();
        for level in &self.import_levels {
            let candidates = level
                .packages
                .iter()
                .filter_map(|&package| {
                    let record = source.symbols(
                        crate::symbol_source::SymbolNamespace::Package(package),
                        name,
                    );
                    record
                        .classifier_name
                        .filter(|_| !level.builtins_only || record.builtin_classifier)
                        .map(|target| {
                            (
                                crate::types::type_name_child(package, name),
                                self.libraries.canonical_source_type_name(target),
                            )
                        })
                })
                .collect::<Vec<_>>();
            if candidates.is_empty() {
                continue;
            }
            if candidates
                .iter()
                .any(|(_, candidate)| *candidate != classifier)
            {
                return None;
            }
            let mut selected: Option<SelectedTypeAlias> = None;
            for (identity, _) in candidates {
                let Some(alias) = self.selected_alias_from_identity(identity, classifier) else {
                    // A real classifier occupies the selected rung. A same-target alias from a
                    // lower package must not change that declaration into an alias application.
                    return None;
                };
                match &selected {
                    None => selected = Some(alias),
                    Some(previous)
                        if previous.formals == alias.formals
                            && previous.expansion == alias.expansion => {}
                    Some(_) => return None,
                }
            }
            return selected;
        }
        None
    }

    fn selected_lexical_type_alias(
        &self,
        scope: &CheckerScope<'_>,
        name: &str,
    ) -> Option<(TypeName, SelectedTypeAlias)> {
        let alias = if let Some(alias) = scope.type_alias(name) {
            SelectedTypeAlias {
                identity: None,
                formals: alias.formals,
                expansion: alias.expansion,
            }
        } else {
            let identity = self.lexical_source_alias_identity(name)?;
            let (formals, expansion) = self.source_alias_expansion(identity)?;
            SelectedTypeAlias {
                identity: Some(identity),
                formals,
                expansion,
            }
        };
        Some((self.alias_target_classifier(alias.expansion)?, alias))
    }

    fn selected_scoped_type_alias(
        &self,
        scope: &CheckerScope<'_>,
        name: &str,
        classifier: TypeName,
    ) -> Option<SelectedTypeAlias> {
        let identity = self.scoped_source_alias_identity(scope, name)?;
        self.selected_alias_from_identity(identity, classifier)
    }
}
