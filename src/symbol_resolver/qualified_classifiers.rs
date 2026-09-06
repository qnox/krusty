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
        let advance = |mut prefix| {
            for (index, segment) in segments[1..].iter().enumerate() {
                prefix = match prefix {
                    ClassifierPathPrefix::Package(package) => {
                        let symbols = self.src.symbols(SymbolNamespace::Package(package), segment);
                        if let Some(classifier) = symbols.classifier_name {
                            ClassifierPathPrefix::Classifier(classifier)
                        } else if self.src.package_exists(package, segment) {
                            ClassifierPathPrefix::Package(crate::types::type_name_child(
                                package, segment,
                            ))
                        } else {
                            return Err((index + 1, (*segment).to_string()));
                        }
                    }
                    ClassifierPathPrefix::Classifier(owner) => {
                        let Some(classifier) = self
                            .src
                            .symbols(SymbolNamespace::Classifier(owner), segment)
                            .classifier_name
                        else {
                            return Err((index + 1, (*segment).to_string()));
                        };
                        ClassifierPathPrefix::Classifier(classifier)
                    }
                };
            }
            match prefix {
                ClassifierPathPrefix::Classifier(classifier) => Ok(classifier),
                ClassifierPathPrefix::Package(_) => Err((
                    segments.len().saturating_sub(1),
                    segments.last().copied().unwrap_or(first).to_string(),
                )),
            }
        };

        // A complete classifier-rooted path outranks a package-rooted path: a visible classifier
        // named `pkg1` therefore makes `pkg1.Cls` mean its nested `Cls` when that child exists. An
        // incomplete classifier path does not hide a complete package path, however; this is what
        // lets package `Package.Outer` coexist with default-imported `java.lang.Package`.
        let mut failure = None;
        match self.classifier_in_scope(first) {
            CandidateSelection::Selected(classifier) => {
                match advance(ClassifierPathPrefix::Classifier(classifier)) {
                    Ok(classifier) => return (CandidateSelection::Selected(classifier), None),
                    Err(classifier_failure) => failure = Some(classifier_failure),
                }
            }
            CandidateSelection::Ambiguous => {
                return (CandidateSelection::Ambiguous, Some(first.to_string()));
            }
            CandidateSelection::None => {}
        }
        if self.src.package_exists(TypeName::ROOT, first) {
            match advance(ClassifierPathPrefix::Package(
                crate::types::type_name_child(TypeName::ROOT, first),
            )) {
                Ok(classifier) => return (CandidateSelection::Selected(classifier), None),
                Err(package_failure) => {
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
