//! Left-to-right binding of qualified classifier paths.
//!
//! The first segment contributes ordinary classifier-scope and root-package namespace facets. Each
//! candidate then advances through exactly one package or classifier namespace; resolution never
//! flattens the spelling or retries alternative nesting layouts. Value roots are selected before
//! this operation by expression resolution and are never reinterpreted here.

use super::*;

#[derive(Clone, Copy)]
enum ClassifierPathPrefix {
    Package(TypeName),
    Classifier(TypeName),
}

/// A completed classifier path and the typealias declaration on its final segment, when that
/// segment's provider record is an alias whose target is the selected classifier.
struct CompletedClassifierPath {
    classifier: TypeName,
    alias: Option<crate::libraries::AliasExpansion>,
}

fn alias_on_selected_classifier(
    classifier: TypeName,
    declaration: Option<&crate::libraries::ClassifierDeclaration>,
) -> Option<crate::libraries::AliasExpansion> {
    let Some(crate::libraries::ClassifierDeclaration::TypeAlias(binding)) = declaration else {
        return None;
    };
    (binding.target == classifier).then(|| binding.clone())
}

impl SymbolResolver<'_> {
    fn advance_classifier_path(
        &self,
        mut prefix: ClassifierPathPrefix,
        segments: &[&str],
        mut alias: Option<crate::libraries::AliasExpansion>,
    ) -> Result<CompletedClassifierPath, (usize, String)> {
        for (index, segment) in segments.iter().enumerate() {
            let (classifier, declaration) = match prefix {
                ClassifierPathPrefix::Package(package) => {
                    let symbols = self.src.symbols(SymbolNamespace::Package(package), segment);
                    if let Some(classifier) = symbols.classifier_name {
                        (classifier, symbols.classifier_declaration.clone())
                    } else if self.src.package_exists(package, segment) {
                        alias = None;
                        prefix = ClassifierPathPrefix::Package(crate::types::type_name_child(
                            package, segment,
                        ));
                        continue;
                    } else {
                        return Err((index + 1, (*segment).to_string()));
                    }
                }
                ClassifierPathPrefix::Classifier(owner) => {
                    let symbols = self
                        .src
                        .symbols(SymbolNamespace::Classifier(owner), segment);
                    let Some(classifier) = symbols.classifier_name else {
                        return Err((index + 1, (*segment).to_string()));
                    };
                    (classifier, symbols.classifier_declaration.clone())
                }
            };
            alias = alias_on_selected_classifier(classifier, declaration.as_ref());
            prefix = ClassifierPathPrefix::Classifier(classifier);
        }
        match prefix {
            ClassifierPathPrefix::Classifier(classifier) => {
                Ok(CompletedClassifierPath { classifier, alias })
            }
            ClassifierPathPrefix::Package(_) => Err((
                segments.len(),
                segments.last().copied().unwrap_or_default().to_string(),
            )),
        }
    }

    /// Advance a qualified classifier path from an already selected first-segment identity.
    /// Failure is final: the caller must not retry another root or reconstruct the path spelling.
    pub(crate) fn classifier_path_from_selected_root(
        &self,
        mut classifier: TypeName,
        segments: &[&str],
    ) -> (Option<TypeName>, Option<String>) {
        for segment in segments {
            let symbols = self
                .src
                .symbols(SymbolNamespace::Classifier(classifier), segment);
            let Some(nested) = symbols.classifier_name else {
                return (None, Some((*segment).to_string()));
            };
            classifier = nested;
        }
        (Some(classifier), None)
    }

    /// Bind a qualified classifier and retain the first segment that could not advance from the
    /// selected namespace facet. Signature diagnostics consume the failed segment directly; they
    /// must not reconstruct it later from a module-wide spelling map, because import and
    /// same-package bindings are file-scoped facts.
    pub(crate) fn qualified_classifier_binding_in_scope(
        &self,
        spelling: &str,
    ) -> (CandidateSelection<TypeName>, Option<String>) {
        let segments = spelling
            .split(['.', '/'])
            .filter(|segment| !segment.is_empty())
            .collect::<Vec<_>>();
        let Some(&first) = segments.first() else {
            return (CandidateSelection::None, Some(spelling.to_string()));
        };
        // Expression qualification commits the first segment once. A missing later segment never
        // reinterprets that root as a lower-priority classifier or package.
        match self.classifier_in_scope(first) {
            CandidateSelection::Selected(classifier) => {
                match self.advance_classifier_path(
                    ClassifierPathPrefix::Classifier(classifier),
                    &segments[1..],
                    None,
                ) {
                    Ok(path) => return (CandidateSelection::Selected(path.classifier), None),
                    Err((_, segment)) => return (CandidateSelection::None, Some(segment)),
                }
            }
            CandidateSelection::Ambiguous => {
                return (CandidateSelection::Ambiguous, Some(first.to_string()));
            }
            CandidateSelection::None => {}
        }
        if self.src.package_exists(TypeName::ROOT, first) {
            match self.advance_classifier_path(
                ClassifierPathPrefix::Package(crate::types::type_name_child(TypeName::ROOT, first)),
                &segments[1..],
                None,
            ) {
                Ok(path) => return (CandidateSelection::Selected(path.classifier), None),
                Err((_, segment)) => {
                    let segment = if !segment.is_empty() {
                        segment
                    } else {
                        first.to_string()
                    };
                    return (CandidateSelection::None, Some(segment));
                }
            }
        }
        (CandidateSelection::None, Some(first.to_string()))
    }

    /// Bind a type path by testing complete candidates at each classifier-scope rung. Selection
    /// happens only after the whole candidate path is applicable, so an incomplete same-named root
    /// does not hide a complete explicit-import or package path.
    pub(crate) fn qualified_type_classifier_binding_in_scope(
        &self,
        spelling: &str,
    ) -> (CandidateSelection<TypeName>, Option<String>) {
        let (selection, failed) = self.qualified_type_path_in_scope(spelling);
        (
            match selection {
                CandidateSelection::Selected(path) => CandidateSelection::Selected(path.classifier),
                CandidateSelection::Ambiguous => CandidateSelection::Ambiguous,
                CandidateSelection::None => CandidateSelection::None,
            },
            failed,
        )
    }

    /// The typealias on the same winning type path as
    /// [`Self::qualified_type_classifier_binding_in_scope`]. An ordinary classifier completion
    /// returns `None`; the caller then builds that classifier. A disputed alias on one classifier
    /// is also `None`, so a later phase does not apply one provider's template to another's class.
    pub(crate) fn qualified_type_alias_expansion(
        &self,
        spelling: &str,
    ) -> Option<crate::libraries::AliasExpansion> {
        match self.qualified_type_path_in_scope(spelling).0 {
            CandidateSelection::Selected(path) => path.alias,
            CandidateSelection::Ambiguous | CandidateSelection::None => None,
        }
    }

    fn qualified_type_path_in_scope(
        &self,
        spelling: &str,
    ) -> (CandidateSelection<CompletedClassifierPath>, Option<String>) {
        let segments = spelling
            .split(['.', '/'])
            .filter(|segment| !segment.is_empty())
            .collect::<Vec<_>>();
        let Some(&first) = segments.first() else {
            return (CandidateSelection::None, Some(spelling.to_string()));
        };
        let mut failure = None;
        let mut consider =
            |candidates: Vec<(TypeName, Option<crate::libraries::AliasExpansion>)>| {
                let mut completed: Vec<CompletedClassifierPath> = Vec::new();
                for (candidate, alias) in candidates {
                    match self.advance_classifier_path(
                        ClassifierPathPrefix::Classifier(candidate),
                        &segments[1..],
                        alias,
                    ) {
                        Ok(path) => record_completed_path(&mut completed, path),
                        Err(miss) => {
                            if failure.as_ref().is_none_or(|(depth, _)| miss.0 > *depth) {
                                failure = Some(miss);
                            }
                        }
                    }
                }
                match completed.len() {
                    0 => None,
                    1 => completed.pop().map(CandidateSelection::Selected),
                    _ => Some(CandidateSelection::Ambiguous),
                }
            };
        let selected = |selection: CandidateSelection<CompletedClassifierPath>| match selection {
            CandidateSelection::Selected(_) => (selection, None),
            CandidateSelection::Ambiguous => {
                (CandidateSelection::Ambiguous, Some(first.to_string()))
            }
            CandidateSelection::None => (CandidateSelection::None, Some(first.to_string())),
        };
        match self.fn_scope {
            Some(FunctionScopeRef::Imports(imports)) => {
                if imports.explicit_is_ambiguous(first) {
                    return (CandidateSelection::Ambiguous, Some(first.to_string()));
                }
                if let Some((owner, declared_name)) = imports.explicit_target(first) {
                    let record = self.src.symbols(owner, &declared_name);
                    if let Some(candidate) = record.classifier_name {
                        let alias = alias_on_selected_classifier(
                            candidate,
                            record.classifier_declaration.as_ref(),
                        );
                        if let Some(selection) = consider(vec![(candidate, alias)]) {
                            return selected(selection);
                        }
                    }
                }
                for level in imports.classifier_levels() {
                    let candidates = level
                        .packages
                        .iter()
                        .filter_map(|&package| {
                            let record = self.src.symbols(SymbolNamespace::Package(package), first);
                            let classifier = record.classifier_name?;
                            if level.builtins_only && !record.builtin_classifier {
                                return None;
                            }
                            let alias = alias_on_selected_classifier(
                                classifier,
                                record.classifier_declaration.as_ref(),
                            );
                            Some((classifier, alias))
                        })
                        .collect();
                    if let Some(selection) = consider(candidates) {
                        return selected(selection);
                    }
                }
            }
            Some(FunctionScopeRef::Flat(packages)) => {
                let candidates = packages
                    .iter()
                    .filter_map(|&package| {
                        let record = self.src.symbols(SymbolNamespace::Package(package), first);
                        let classifier = record.classifier_name?;
                        let alias = alias_on_selected_classifier(
                            classifier,
                            record.classifier_declaration.as_ref(),
                        );
                        Some((classifier, alias))
                    })
                    .collect();
                if let Some(selection) = consider(candidates) {
                    return selected(selection);
                }
            }
            None => {}
        }
        if self.src.package_exists(TypeName::ROOT, first) {
            match self.advance_classifier_path(
                ClassifierPathPrefix::Package(crate::types::type_name_child(TypeName::ROOT, first)),
                &segments[1..],
                None,
            ) {
                Ok(path) => return (CandidateSelection::Selected(path), None),
                Err(mut package_failure) => {
                    if package_failure.1.is_empty() {
                        package_failure.1 = first.to_string();
                    }
                    if failure
                        .as_ref()
                        .is_none_or(|(depth, _)| package_failure.0 > *depth)
                    {
                        failure = Some(package_failure);
                    }
                }
            }
        }
        (
            CandidateSelection::None,
            failure
                .map(|(_, segment)| segment)
                .or_else(|| Some(first.to_string())),
        )
    }
}

/// Keep one completion per classifier. Two providers that normalize to the same classifier but
/// disagree about its alias template leave no template: applying either one would hide the clash.
fn record_completed_path(
    completed: &mut Vec<CompletedClassifierPath>,
    path: CompletedClassifierPath,
) {
    if let Some(existing) = completed
        .iter_mut()
        .find(|candidate| candidate.classifier == path.classifier)
    {
        if existing.alias != path.alias {
            existing.alias = None;
        }
        return;
    }
    completed.push(path);
}
