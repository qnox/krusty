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

impl SymbolResolver<'_> {
    /// Advance through `segments`, one namespace per segment. The result carries the provenance of
    /// the declaration the FINAL segment bound: `dep.Cargo` selects the `dep.Cargo` typealias
    /// itself, so package qualification keeps that alias, while `Alias.Nested` ends on a nested
    /// classifier that no alias names.
    fn advance_classifier_path(
        &self,
        mut prefix: ClassifierPathPrefix,
        segments: &[&str],
    ) -> Result<ScopedClassifier, (usize, String)> {
        let mut selected = None;
        for (index, segment) in segments.iter().enumerate() {
            let namespace = match prefix {
                ClassifierPathPrefix::Package(package) => SymbolNamespace::Package(package),
                ClassifierPathPrefix::Classifier(owner) => SymbolNamespace::Classifier(owner),
            };
            let record = self.src.symbols(namespace, segment);
            if let Some(classifier) = record.classifier_name {
                let alias = match &record.classifier_declaration {
                    Some(crate::libraries::ClassifierDeclaration::TypeAlias(alias)) => {
                        Some(alias.clone())
                    }
                    Some(crate::libraries::ClassifierDeclaration::Ordinary(_)) | None => None,
                };
                selected = Some(ScopedClassifier { classifier, alias });
                prefix = ClassifierPathPrefix::Classifier(classifier);
                continue;
            }
            match prefix {
                ClassifierPathPrefix::Package(package)
                    if self.src.package_exists(package, segment) =>
                {
                    selected = None;
                    prefix = ClassifierPathPrefix::Package(crate::types::type_name_child(
                        package, segment,
                    ));
                }
                _ => return Err((index + 1, (*segment).to_string())),
            }
        }
        match (prefix, selected) {
            (ClassifierPathPrefix::Classifier(_), Some(selected)) => Ok(selected),
            (ClassifierPathPrefix::Classifier(classifier), None) => {
                Ok(Self::unaliased_classifier(classifier))
            }
            (ClassifierPathPrefix::Package(_), _) => Err((
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
    /// same-package bindings are file-scoped facts. The selection keeps the alias declaration the
    /// final segment bound, whether the path reached it unqualified or through its package.
    pub(crate) fn qualified_scoped_classifier_binding_in_scope(
        &self,
        spelling: &str,
    ) -> (CandidateSelection<ScopedClassifier>, Option<String>) {
        let segments = spelling
            .split(['.', '/'])
            .filter(|segment| !segment.is_empty())
            .collect::<Vec<_>>();
        let Some(&first) = segments.first() else {
            return (CandidateSelection::None, Some(spelling.to_string()));
        };
        // Expression qualification commits the first segment once. A missing later segment never
        // reinterprets that root as a lower-priority classifier or package.
        match self.scoped_classifier_in_scope(first) {
            CandidateSelection::Selected(root) if segments.len() == 1 => {
                return (CandidateSelection::Selected(root), None);
            }
            CandidateSelection::Selected(root) => {
                match self.advance_classifier_path(
                    ClassifierPathPrefix::Classifier(root.classifier),
                    &segments[1..],
                ) {
                    Ok(selected) => return (CandidateSelection::Selected(selected), None),
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
            ) {
                Ok(selected) => return (CandidateSelection::Selected(selected), None),
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

    fn unaliased_classifier(classifier: TypeName) -> ScopedClassifier {
        ScopedClassifier {
            classifier,
            alias: None,
        }
    }

    /// Bind a type path by testing complete candidates at each classifier-scope rung. Selection
    /// happens only after the whole candidate path is applicable, so an incomplete same-named root
    /// does not hide a complete explicit-import or package path.
    pub(crate) fn qualified_type_classifier_binding_in_scope(
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
        let mut failure = None;
        let mut consider = |candidates: &[TypeName]| {
            let mut completed = Vec::new();
            for &candidate in candidates {
                match self.advance_classifier_path(
                    ClassifierPathPrefix::Classifier(candidate),
                    &segments[1..],
                ) {
                    Ok(ScopedClassifier { classifier, .. }) => {
                        if !completed.contains(&classifier) {
                            completed.push(classifier);
                        }
                    }
                    Err(miss) => {
                        if failure.as_ref().is_none_or(|(depth, _)| miss.0 > *depth) {
                            failure = Some(miss);
                        }
                    }
                }
            }
            match completed.as_slice() {
                [] => None,
                [classifier] => Some(CandidateSelection::Selected(*classifier)),
                _ => Some(CandidateSelection::Ambiguous),
            }
        };
        let selected = |selection: CandidateSelection<TypeName>| match selection {
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
                    if let Some(candidate) = self.src.symbols(owner, &declared_name).classifier_name
                    {
                        if let Some(selection) = consider(&[candidate]) {
                            return selected(selection);
                        }
                    }
                }
                for level in imports.classifier_levels() {
                    let candidates = super::classifier_scope::classifier_candidates_at_import_level(
                        &self.src, first, level,
                    );
                    if let Some(selection) = consider(&candidates) {
                        return selected(selection);
                    }
                }
            }
            Some(FunctionScopeRef::Flat(packages)) => {
                let candidates =
                    super::classifier_candidates_at_scope_level(&self.src, first, packages);
                if let Some(selection) = consider(&candidates) {
                    return selected(selection);
                }
            }
            None => {}
        }
        if self.src.package_exists(TypeName::ROOT, first) {
            match self.advance_classifier_path(
                ClassifierPathPrefix::Package(crate::types::type_name_child(TypeName::ROOT, first)),
                &segments[1..],
            ) {
                Ok(selected) => {
                    return (CandidateSelection::Selected(selected.classifier), None);
                }
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
