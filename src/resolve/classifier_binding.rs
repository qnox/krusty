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

type SelectedAliasProvenance = Result<Option<SelectedTypeAlias>, InvalidAliasProvenance>;

#[derive(Clone, Copy)]
struct InvalidAliasProvenance;

fn selected_alias_from_binding(
    identity: TypeName,
    classifier: TypeName,
    binding: Option<crate::libraries::AliasExpansion>,
) -> SelectedAliasProvenance {
    // An ordinary classifier publishes its own identity in both record fields. Do not query
    // another provider for an equally qualified alias after that classifier has won.
    if identity == classifier {
        return Ok(None);
    }
    let binding = binding.ok_or(InvalidAliasProvenance)?;
    if binding.identity != identity || binding.target != classifier {
        return Err(InvalidAliasProvenance);
    }
    Ok(Some(SelectedTypeAlias {
        identity: Some(binding.identity),
        formals: binding.formals,
        expansion: binding.expansion,
    }))
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
        } else if let Some(alias) = self.selected_lexical_type_alias(scope, root_name) {
            let Ok((internal, alias)) = alias else {
                return Self::invalid_alias_selection(root_name);
            };
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
                        let provenance = declaration.map_or(Ok(None), |identity| {
                            self.selected_alias_from_identity(identity, classifier)
                        });
                        let Ok(alias) = provenance else {
                            return Self::invalid_alias_selection(root_name);
                        };
                        selected_alias = alias;
                        ResolvedQualifier::Classifier(classifier)
                    } else if let Some((classifier, alias)) =
                        self.selected_same_package_classifier(root_name)
                    {
                        let Ok(alias) = alias else {
                            return Self::invalid_alias_selection(root_name);
                        };
                        selected_alias = alias;
                        ResolvedQualifier::Classifier(classifier)
                    } else if let Some(classifier) =
                        self.alias_ahead_of_imported_classifier(scope, root_name, &source)
                    {
                        let Ok(alias) =
                            self.selected_scoped_type_alias(scope, root_name, classifier)
                        else {
                            return Self::invalid_alias_selection(root_name);
                        };
                        selected_alias = Some(alias);
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
                                let Ok(alias) =
                                    self.selected_imported_type_alias(root_name, internal)
                                else {
                                    return Self::invalid_alias_selection(root_name);
                                };
                                selected_alias = alias;
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
                    let Ok(alias) = self.selected_alias_from_identity(identity, internal) else {
                        let failed = segments
                            .last()
                            .map_or(root_name.as_str(), |(_, segment)| segment);
                        return Self::invalid_alias_selection(failed);
                    };
                    selected_alias = alias;
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

    fn invalid_alias_selection(
        name: &str,
    ) -> (
        InheritedNestedClassifier,
        Option<String>,
        Option<SelectedTypeAlias>,
    ) {
        (
            InheritedNestedClassifier::Ambiguous,
            Some(name.to_string()),
            None,
        )
    }

    pub(super) fn select_classifier(
        &self,
        scope: &CheckerScope<'_>,
        name: &str,
    ) -> InheritedNestedClassifier {
        self.select_classifier_binding(scope, name).0
    }

    fn selected_alias_from_identity(
        &self,
        identity: TypeName,
        classifier: TypeName,
    ) -> SelectedAliasProvenance {
        selected_alias_from_binding(identity, classifier, self.source_alias_binding(identity))
    }

    /// Classifier and alias provenance from the current-package rung. The package namespace and
    /// source lookup name identify the declaration before the target classifier is selected, so an
    /// alias keeps its own stable identity even when the provider normalizes the classifier facet to
    /// the alias target.
    fn selected_same_package_classifier(
        &self,
        name: &str,
    ) -> Option<(TypeName, SelectedAliasProvenance)> {
        let namespace = crate::symbol_source::SymbolNamespace::Package(self.source_package_name());
        let record = self.fed_source().symbols(namespace, name);
        let classifier = self
            .libraries
            .canonical_source_type_name(record.classifier_name?);
        let alias = record
            .classifier_declaration_name
            .map_or(Ok(None), |identity| {
                self.selected_alias_from_identity(identity, classifier)
            });
        Some((classifier, alias))
    }

    /// Alias provenance from the exact star/default import rung that selected `classifier`.
    fn selected_imported_type_alias(
        &self,
        name: &str,
        classifier: TypeName,
    ) -> SelectedAliasProvenance {
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
                                record.classifier_declaration_name,
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
                return Err(InvalidAliasProvenance);
            }
            let mut ordinary = false;
            let mut selected: Option<SelectedTypeAlias> = None;
            for (identity, _) in candidates {
                let Some(identity) = identity else {
                    // The selected provider did not publish alias provenance. Do not reinterpret
                    // that classifier through an equally named declaration from another source.
                    return Err(InvalidAliasProvenance);
                };
                let Some(alias) = self.selected_alias_from_identity(identity, classifier)? else {
                    // An ordinary classifier is the complete selected declaration, not missing
                    // alias provenance. Multiple provider/package records may normalize onto the
                    // same common classifier (for example java.lang.Object and kotlin.Any); the
                    // target-equality check above has already proved that they are one semantic
                    // identity. A same-target alias still conflicts with that ordinary identity.
                    if selected.is_some() {
                        return Err(InvalidAliasProvenance);
                    }
                    ordinary = true;
                    continue;
                };
                if ordinary {
                    return Err(InvalidAliasProvenance);
                }
                match &selected {
                    None => selected = Some(alias),
                    Some(previous)
                        if previous.formals == alias.formals
                            && previous.expansion == alias.expansion => {}
                    Some(_) => return Err(InvalidAliasProvenance),
                }
            }
            return if ordinary { Ok(None) } else { Ok(selected) };
        }
        Ok(None)
    }

    fn selected_lexical_type_alias(
        &self,
        scope: &CheckerScope<'_>,
        name: &str,
    ) -> Option<Result<(TypeName, SelectedTypeAlias), InvalidAliasProvenance>> {
        if let Some(alias) = scope.type_alias(name) {
            return Some(Ok((
                alias.target,
                SelectedTypeAlias {
                    identity: None,
                    formals: alias.formals,
                    expansion: alias.expansion,
                },
            )));
        }
        let identity = self.lexical_source_alias_identity(name)?;
        let Some(binding) = self.source_alias_binding(identity) else {
            return Some(Err(InvalidAliasProvenance));
        };
        Some(Ok((
            binding.target,
            SelectedTypeAlias {
                identity: Some(binding.identity),
                formals: binding.formals,
                expansion: binding.expansion,
            },
        )))
    }

    fn selected_scoped_type_alias(
        &self,
        scope: &CheckerScope<'_>,
        name: &str,
        classifier: TypeName,
    ) -> Result<SelectedTypeAlias, InvalidAliasProvenance> {
        let identity = self
            .scoped_source_alias_identity(scope, name)
            .ok_or(InvalidAliasProvenance)?;
        self.selected_alias_from_identity(identity, classifier)?
            .ok_or(InvalidAliasProvenance)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_alias_rejects_missing_or_mismatched_provider_contracts() {
        let alias = crate::types::type_name("fixture/Alias");
        let target = crate::types::type_name("fixture/Target");
        let other = crate::types::type_name("fixture/Other");
        assert!(selected_alias_from_binding(alias, target, None).is_err());
        assert!(selected_alias_from_binding(
            alias,
            target,
            Some(crate::libraries::AliasExpansion {
                identity: alias,
                target: other,
                formals: vec!["T".to_string()],
                expansion: Ty::obj_name(other),
                expansion_spelling: Default::default(),
            }),
        )
        .is_err());
    }
}
