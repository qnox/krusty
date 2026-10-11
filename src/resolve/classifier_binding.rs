//! Bind-once classifier selection, including the exact typealias declaration (when any) that won
//! the root scope-tower rung. Consumers apply the returned alias template directly; they never
//! rediscover alias-ness from the source spelling after selection.

use super::*;
use crate::symbol_resolver::{CandidateSelectionWithTies, ClassifierMiss, ScopedClassifier};

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

fn selected_alias_from_expansion(
    identity: TypeName,
    classifier: TypeName,
    binding: Option<crate::libraries::AliasExpansion>,
) -> Result<SelectedTypeAlias, InvalidAliasProvenance> {
    let binding = binding.ok_or(InvalidAliasProvenance)?;
    if binding.identity != identity || binding.target != classifier {
        return Err(InvalidAliasProvenance);
    }
    Ok(SelectedTypeAlias {
        identity: Some(binding.identity),
        formals: binding.formals,
        expansion: binding.expansion,
    })
}

fn selected_alias_from_declaration(
    classifier: TypeName,
    declaration: Option<&crate::libraries::ClassifierDeclaration>,
) -> SelectedAliasProvenance {
    match declaration.ok_or(InvalidAliasProvenance)? {
        crate::libraries::ClassifierDeclaration::Ordinary(_) => Ok(None),
        crate::libraries::ClassifierDeclaration::TypeAlias(binding) => {
            selected_alias_from_expansion(binding.identity, classifier, Some(binding.clone()))
                .map(Some)
        }
    }
}

/// The alias template an explicit import's classifier facet carries, validated against the
/// classifier it names.
fn explicit_import_alias(candidate: &ScopedClassifier) -> SelectedAliasProvenance {
    candidate
        .alias
        .clone()
        .map(|alias| {
            selected_alias_from_expansion(alias.identity, candidate.classifier, Some(alias))
        })
        .transpose()
}

/// Star/default-import candidates whose classifier facets name no typealias declaration.
fn unaliased_candidates(candidates: Vec<TypeName>) -> Vec<ScopedClassifier> {
    candidates
        .into_iter()
        .map(|classifier| ScopedClassifier {
            classifier,
            alias: None,
        })
        .collect()
}

fn same_selected_alias(
    left: Option<&SelectedTypeAlias>,
    right: Option<&SelectedTypeAlias>,
) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => {
            left.identity == right.identity
                && left.formals == right.formals
                && left.expansion == right.expansion
        }
        (None, Some(_)) | (Some(_), None) => false,
    }
}

/// How a qualified classifier spelling meets a scope-tower rung whose root lacks the written
/// suffix. A qualifier in an expression commits to the first rung that binds its root; a type
/// reference considers only complete paths, so `A.B` in a type reaches an imported `A` that has
/// `B` past a nearer lexical `A` that does not (kotlinc 2.4.20).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum QualifiedRoot {
    Committed,
    CompletePath,
}

type ClassifierBinding = (
    InheritedNestedClassifier,
    Option<ClassifierMiss>,
    Option<SelectedTypeAlias>,
);

enum CompleteImportedSelection {
    None(Option<ClassifierBinding>),
    Selected(ClassifierBinding),
    Ambiguous(Vec<TypeName>),
}

impl Checker<'_> {
    /// Select the classifier root from the scope tower, then commit every remaining segment through
    /// the shared qualifier loop. The third result is the alias binding from the same winning root
    /// rung. There is no import/module/classpath retry after this returns.
    pub(super) fn select_classifier_binding(
        &self,
        scope: &CheckerScope<'_>,
        name: &str,
    ) -> ClassifierBinding {
        self.select_classifier_binding_with(scope, name, QualifiedRoot::Committed)
    }

    /// Bind a type reference's classifier: each scope-tower rung offers its root, and a rung whose
    /// root does not contain the written suffix yields to the next one, as kotlinc's type resolution
    /// selects among complete paths only.
    pub(super) fn select_type_classifier_binding(
        &self,
        scope: &CheckerScope<'_>,
        name: &str,
    ) -> ClassifierBinding {
        self.select_classifier_binding_with(scope, name, QualifiedRoot::CompletePath)
    }

    fn select_classifier_binding_with(
        &self,
        scope: &CheckerScope<'_>,
        name: &str,
        mode: QualifiedRoot,
    ) -> ClassifierBinding {
        let segments = name
            .split(['.', '/'])
            .filter(|segment| !segment.is_empty())
            .map(|segment| (None, segment.to_string()))
            .collect::<Vec<_>>();
        let Some((_, root_name)) = segments.first() else {
            return (
                InheritedNestedClassifier::NotFound,
                Some(ClassifierMiss::Unresolved(name.to_string())),
                None,
            );
        };
        let source = self.fed_source();
        // The miss of the nearest rung that bound the root: a lower rung may still supply a
        // complete path, but its own misses never replace this one.
        let nearest_miss: std::cell::RefCell<Option<ClassifierBinding>> = Default::default();
        let offer = |root: TypeName, alias: Option<SelectedTypeAlias>| {
            let binding = self.commit_classifier_suffix(
                &source,
                ResolvedQualifier::Classifier(root),
                root_name,
                &segments,
                alias,
            );
            if mode == QualifiedRoot::CompletePath
                && binding.0 == InheritedNestedClassifier::NotFound
            {
                nearest_miss.borrow_mut().get_or_insert(binding);
                None
            } else {
                Some(binding)
            }
        };
        let scoped = scope.symbols(root_name, &source);
        if let Some(internal) = scoped.classifier_name {
            if let Some(binding) = offer(internal, None) {
                return binding;
            }
        }
        if let Some(alias) = self.selected_lexical_type_alias(scope, root_name) {
            let Ok((internal, alias)) = alias else {
                return Self::invalid_alias_selection(root_name);
            };
            if let Some(binding) = offer(internal, Some(alias)) {
                return binding;
            }
        }
        if let Some(internal) = self.classifier_header_lexical_type_name(root_name) {
            if let Some(binding) = offer(internal, None) {
                return binding;
            }
        }
        if let Some(internal) = self.enclosing_nested_type_name(root_name) {
            if let Some(binding) = offer(internal, None) {
                return binding;
            }
        }
        match self.inherited_nested_type_name(root_name) {
            InheritedNestedClassifier::Found(internal) => {
                if let Some(binding) = offer(internal, None) {
                    return binding;
                }
            }
            InheritedNestedClassifier::Ambiguous => {
                return nearest_miss.take().unwrap_or((
                    InheritedNestedClassifier::Ambiguous,
                    Some(ClassifierMiss::Unresolved(root_name.clone())),
                    None,
                ));
            }
            InheritedNestedClassifier::NotFound => {}
        }
        // Explicit imports rank above the current package. Every explicit import under the root is
        // one rung; conflicting imports are reported on the import list and stay candidates here.
        let explicit = crate::symbol_resolver::explicit_classifier_candidates(
            &source,
            &self.function_import_scope,
            root_name,
        );
        match explicit.as_slice() {
            [] => {}
            [candidate] => {
                let Ok(alias) = explicit_import_alias(candidate) else {
                    return Self::invalid_alias_selection(root_name);
                };
                if let Some(binding) = offer(candidate.classifier, alias) {
                    return binding;
                }
            }
            // An expression qualifier commits its root before any suffix is examined, and a
            // conflicting import binds no root: the reference names its unbound root (kotlinc
            // 2.4.20 reports `Same()` as an unresolved reference).
            [_, _, ..] if mode == QualifiedRoot::Committed => {
                return (
                    InheritedNestedClassifier::NotFound,
                    Some(ClassifierMiss::Unresolved(root_name.clone())),
                    None,
                );
            }
            // A type reference selects among the complete explicit paths: one completion binds,
            // several are an ambiguity between those candidates.
            [_, _, ..] => {
                let mut completed: Vec<(ClassifierBinding, ScopedClassifier)> = Vec::new();
                for candidate in &explicit {
                    let Ok(alias) = explicit_import_alias(candidate) else {
                        return Self::invalid_alias_selection(root_name);
                    };
                    let Some(binding) = offer(candidate.classifier, alias) else {
                        continue;
                    };
                    let InheritedNestedClassifier::Found(classifier) = binding.0 else {
                        return binding;
                    };
                    let alias = (classifier == candidate.classifier)
                        .then(|| candidate.alias.clone())
                        .flatten();
                    completed.push((binding, ScopedClassifier { classifier, alias }));
                }
                match completed.len() {
                    0 => {}
                    1 => return completed.remove(0).0,
                    _ => {
                        return (
                            InheritedNestedClassifier::Ambiguous,
                            Some(ClassifierMiss::Ambiguous(
                                completed
                                    .into_iter()
                                    .map(|(_, candidate)| candidate)
                                    .collect(),
                            )),
                            None,
                        );
                    }
                }
            }
        }
        if let Some((classifier, alias)) = self.selected_same_package_classifier(root_name) {
            let Ok(alias) = alias else {
                return Self::invalid_alias_selection(root_name);
            };
            if let Some(binding) = offer(classifier, alias) {
                return binding;
            }
        }
        if let Some(classifier) = self.alias_ahead_of_imported_classifier(scope, root_name, &source)
        {
            let Ok(alias) = self.selected_scoped_type_alias(scope, root_name, classifier) else {
                return Self::invalid_alias_selection(root_name);
            };
            if let Some(binding) = offer(classifier, Some(alias)) {
                return binding;
            }
        }
        if mode == QualifiedRoot::CompletePath {
            match self.complete_imported_classifier_binding(root_name, &segments, &source) {
                CompleteImportedSelection::Selected(binding) => return binding,
                CompleteImportedSelection::Ambiguous(candidates) => {
                    return (
                        InheritedNestedClassifier::Ambiguous,
                        Some(ClassifierMiss::Ambiguous(unaliased_candidates(candidates))),
                        None,
                    );
                }
                CompleteImportedSelection::None(miss) => {
                    if let Some(miss) = miss {
                        nearest_miss.borrow_mut().get_or_insert(miss);
                    }
                }
            }
        } else {
            match imported_classifier_selection(
                root_name,
                &self.imports,
                &self.import_levels,
                &source,
            ) {
                CandidateSelectionWithTies::Selected(internal) => {
                    crate::trace_compiler!(
                        "resolve",
                        "classifier root={root_name} imported={}",
                        internal.render()
                    );
                    let internal = self.libraries.canonical_source_type_name(internal);
                    let Ok(alias) = self.selected_imported_type_alias(root_name, internal) else {
                        return Self::invalid_alias_selection(root_name);
                    };
                    if let Some(binding) = offer(internal, alias) {
                        return binding;
                    }
                }
                CandidateSelectionWithTies::Ambiguous(candidates) => {
                    return nearest_miss.take().unwrap_or((
                        InheritedNestedClassifier::Ambiguous,
                        Some(ClassifierMiss::Ambiguous(unaliased_candidates(candidates))),
                        None,
                    ));
                }
                CandidateSelectionWithTies::None => {}
            }
        }
        match self
            .classifier_header_owner
            .map_or(InheritedNestedClassifier::NotFound, |owner| {
                self.inherited_nested_type_for_owner(root_name, owner)
            }) {
            InheritedNestedClassifier::Found(internal) => {
                if let Some(binding) = offer(internal, None) {
                    return binding;
                }
            }
            InheritedNestedClassifier::Ambiguous => {
                return nearest_miss.take().unwrap_or((
                    InheritedNestedClassifier::Ambiguous,
                    Some(ClassifierMiss::Unresolved(root_name.clone())),
                    None,
                ));
            }
            InheritedNestedClassifier::NotFound => {}
        }
        if let Some(miss) = nearest_miss.take() {
            return miss;
        }
        if segments.len() > 1 && source.package_exists(TypeName::ROOT, root_name) {
            return self.commit_classifier_suffix(
                &source,
                ResolvedQualifier::Package(crate::types::type_name_child(
                    TypeName::ROOT,
                    root_name,
                )),
                root_name,
                &segments,
                None,
            );
        }
        (
            InheritedNestedClassifier::NotFound,
            Some(ClassifierMiss::Unresolved(root_name.clone())),
            None,
        )
    }

    /// Walk the written suffix from one selected root through the shared qualifier loop.
    fn commit_classifier_suffix(
        &self,
        source: &CachedCompositeSource<'_>,
        root: ResolvedQualifier,
        root_name: &str,
        segments: &[(Option<ExprId>, String)],
        mut selected_alias: Option<SelectedTypeAlias>,
    ) -> ClassifierBinding {
        match walk_qualifier_namespace_facets_with_declaration_identity(
            source,
            root.classifier(),
            None,
            root_name,
            &segments[1..],
        ) {
            Ok((ResolvedQualifier::Classifier(internal), declaration_identity)) => {
                let internal = self.libraries.canonical_source_type_name(internal);
                if let Some(declaration) = declaration_identity {
                    let Ok(alias) = selected_alias_from_declaration(internal, Some(&declaration))
                    else {
                        let failed = segments
                            .last()
                            .map_or(root_name, |(_, segment)| segment.as_str());
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
                segments
                    .last()
                    .map(|(_, segment)| ClassifierMiss::Unresolved(segment.clone())),
                None,
            ),
            Err(QualifierError::UnresolvedSegment { name, .. })
            | Err(QualifierError::AmbiguousRoot { name, .. }) => (
                InheritedNestedClassifier::NotFound,
                Some(ClassifierMiss::Unresolved(name)),
                None,
            ),
            Err(QualifierError::NotANameChain { .. }) => (
                InheritedNestedClassifier::NotFound,
                Some(ClassifierMiss::Unresolved(root_name.to_string())),
                None,
            ),
        }
    }

    /// Select star/default-imported type paths only after advancing every root through the written
    /// suffix. Root ambiguity is not path ambiguity: `p.A` and `q.A` may share one import rung while
    /// only one of them declares the requested `B` in `A.B`.
    fn complete_imported_classifier_binding(
        &self,
        root_name: &str,
        segments: &[(Option<ExprId>, String)],
        source: &CachedCompositeSource<'_>,
    ) -> CompleteImportedSelection {
        let mut nearest_miss = None;
        for level in &self.import_levels {
            let candidates = crate::symbol_resolver::classifier_candidates_at_import_level(
                source, root_name, level,
            );
            if candidates.is_empty() {
                continue;
            }
            let roots = level
                .packages
                .iter()
                .filter_map(|&owner| {
                    let namespace = if source.classifier(owner).is_some() {
                        crate::symbol_source::SymbolNamespace::Classifier(owner)
                    } else {
                        crate::symbol_source::SymbolNamespace::Package(owner)
                    };
                    let record = source.symbols(namespace, root_name);
                    let root = record
                        .classifier_name
                        .filter(|root| candidates.contains(root))?;
                    Some((
                        self.libraries.canonical_source_type_name(root),
                        record.classifier_declaration.clone(),
                    ))
                })
                .collect::<Vec<_>>();
            debug_assert!(!roots.is_empty());

            let mut completed: Vec<(TypeName, Option<SelectedTypeAlias>)> = Vec::new();
            for (root, declaration) in roots {
                let alias = selected_alias_from_declaration(root, declaration.as_ref());
                let binding = self.commit_classifier_suffix(
                    source,
                    ResolvedQualifier::Classifier(root),
                    root_name,
                    segments,
                    alias.as_ref().ok().cloned().flatten(),
                );
                let InheritedNestedClassifier::Found(classifier) = binding.0 else {
                    nearest_miss.get_or_insert(binding);
                    continue;
                };
                let Ok(_) = alias else {
                    return CompleteImportedSelection::Selected(Self::invalid_alias_selection(
                        root_name,
                    ));
                };
                let selected_alias = binding.2;
                if let Some((_, previous_alias)) = completed
                    .iter()
                    .find(|(previous, _)| *previous == classifier)
                {
                    if !same_selected_alias(previous_alias.as_ref(), selected_alias.as_ref()) {
                        return CompleteImportedSelection::Selected(Self::invalid_alias_selection(
                            root_name,
                        ));
                    }
                    continue;
                }
                completed.push((classifier, selected_alias));
            }

            match completed.len() {
                0 => {}
                1 => {
                    let (classifier, alias) = completed.pop().expect("one completed type path");
                    return CompleteImportedSelection::Selected((
                        InheritedNestedClassifier::Found(classifier),
                        None,
                        alias,
                    ));
                }
                _ => {
                    return CompleteImportedSelection::Ambiguous(
                        completed
                            .into_iter()
                            .map(|(classifier, _)| classifier)
                            .collect(),
                    );
                }
            }
        }
        CompleteImportedSelection::None(nearest_miss)
    }

    fn enclosing_nested_type_name(&self, name: &str) -> Option<TypeName> {
        // Probe the current class and its structural lexical owners in nearest-first order. Each
        // owner's companion object contributes its static classifier scope right after the owner.
        let source = self.fed_source();
        self.lexical_source_class_names()
            .into_iter()
            .find_map(|outer| self.static_rung_nested_type_name(&source, outer, name))
    }

    /// A nested classifier of one lexical class rung: the owner's own nested classifiers, then
    /// those of its companion object (kotlinc's `staticScope`, then `companionStaticScope`).
    fn static_rung_nested_type_name(
        &self,
        source: &CachedCompositeSource<'_>,
        owner: TypeName,
        name: &str,
    ) -> Option<TypeName> {
        let own = type_name_nested_child(owner, name);
        if source.classifier(own).is_some() {
            return Some(own);
        }
        let companion = source.classifier(owner)?.companion_object.as_ref()?.1;
        let nested = type_name_nested_child(companion, name);
        source.classifier(nested).is_some().then_some(nested)
    }

    /// Classifier declarations visible in a classifier header before package/import lookup. The
    /// current classifier is recursively visible, as are its own nested declarations and siblings
    /// owned by enclosing lexical classifiers. Supertype-list resolution does not install this
    /// context; primary-constructor parameter declarations do.
    fn classifier_header_lexical_type_name(&self, name: &str) -> Option<TypeName> {
        let owner = self.classifier_header_owner?;
        if owner.nested_segment_ref() == name {
            return Some(owner);
        }
        let source = self.fed_source();
        if let Some(nested) = self.static_rung_nested_type_name(&source, owner, name) {
            return Some(nested);
        }
        let mut enclosing = owner.nested_owner();
        while let Some(candidate_owner) = enclosing {
            if let Some(candidate) =
                self.static_rung_nested_type_name(&source, candidate_owner, name)
            {
                return Some(candidate);
            }
            enclosing = candidate_owner.nested_owner();
        }
        None
    }

    fn invalid_alias_selection(
        name: &str,
    ) -> (
        InheritedNestedClassifier,
        Option<ClassifierMiss>,
        Option<SelectedTypeAlias>,
    ) {
        (
            InheritedNestedClassifier::Ambiguous,
            Some(ClassifierMiss::Unresolved(name.to_string())),
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
        selected_alias_from_expansion(identity, classifier, self.source_alias_binding(identity))
            .map(Some)
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
        let alias =
            selected_alias_from_declaration(classifier, record.classifier_declaration.as_ref());
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
                                record.classifier_declaration.clone(),
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
            for (declaration, _) in candidates {
                let Some(declaration) = declaration else {
                    // The selected provider did not publish alias provenance. Do not reinterpret
                    // that classifier through an equally named declaration from another source.
                    return Err(InvalidAliasProvenance);
                };
                let Some(alias) = selected_alias_from_declaration(classifier, Some(&declaration))?
                else {
                    // An ordinary classifier is the complete selected declaration, not missing
                    // alias provenance. Multiple provider/package records may normalize onto one
                    // common semantic classifier; the target-equality check above has already
                    // proved that identity. A same-target alias still conflicts with the ordinary
                    // declaration.
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
                        if previous.identity == alias.identity
                            && previous.formals == alias.formals
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
        assert!(selected_alias_from_expansion(alias, target, None).is_err());
        assert!(selected_alias_from_expansion(
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

    #[test]
    fn ordinary_declaration_may_normalize_to_a_different_classifier_identity() {
        let declaration = crate::types::type_name("platform/Declaration");
        let classifier = crate::types::type_name("common/Classifier");
        assert!(matches!(
            selected_alias_from_declaration(
                classifier,
                Some(&crate::libraries::ClassifierDeclaration::Ordinary(
                    declaration
                )),
            ),
            Ok(None)
        ));
    }
}
