//! Left-to-right qualified-name resolution over the common symbol-source boundary.

use crate::ast::ExprId;
use crate::symbol_source::{SymbolNamespace, SymbolSource};
use crate::types::TypeName;

/// The committed meaning of a dotted expression's prefix. Once the root is a value, a later miss
/// never backtracks and reinterprets it as a package or classifier.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ResolvedQualifier {
    Value,
    Package(TypeName),
    Classifier(TypeName),
}

impl ResolvedQualifier {
    pub(super) fn classifier(self) -> Option<TypeName> {
        match self {
            Self::Classifier(internal) => Some(internal),
            Self::Value | Self::Package(_) => None,
        }
    }
}

/// Why a qualifier walk could not commit its next segment. The syntax path that owns the final
/// expression decides whether and where to report it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum QualifierError {
    NotANameChain {
        expression: ExprId,
    },
    UnresolvedSegment {
        expression: Option<ExprId>,
        name: String,
    },
    AmbiguousRoot {
        expression: Option<ExprId>,
        name: String,
    },
}

pub(super) enum QualifierInput<'a> {
    Expression(ExprId),
    Root(&'a str),
}

pub(super) fn classifier_identity<S: SymbolSource + ?Sized>(
    source: &S,
    namespace: SymbolNamespace,
    name: &str,
) -> Option<TypeName> {
    source.symbols(namespace, name).classifier_name
}

pub(super) fn walk_qualifier<S: SymbolSource + ?Sized>(
    source: &S,
    mut prefix: ResolvedQualifier,
    segments: &[(Option<ExprId>, String)],
) -> Result<ResolvedQualifier, QualifierError> {
    for (segment_expression, segment) in segments {
        prefix = match prefix {
            ResolvedQualifier::Value => return Ok(ResolvedQualifier::Value),
            ResolvedQualifier::Package(package) => {
                if let Some(classifier) =
                    classifier_identity(source, SymbolNamespace::Package(package), segment)
                {
                    ResolvedQualifier::Classifier(classifier)
                } else if source.package_exists(package, segment) {
                    ResolvedQualifier::Package(crate::types::type_name_child(package, segment))
                } else {
                    return Err(QualifierError::UnresolvedSegment {
                        expression: *segment_expression,
                        name: segment.clone(),
                    });
                }
            }
            ResolvedQualifier::Classifier(owner) => {
                let Some(classifier) =
                    classifier_identity(source, SymbolNamespace::Classifier(owner), segment)
                else {
                    return Err(QualifierError::UnresolvedSegment {
                        expression: *segment_expression,
                        name: segment.clone(),
                    });
                };
                ResolvedQualifier::Classifier(classifier)
            }
        };
    }
    Ok(prefix)
}

/// Commit the first namespace facet after value-root selection has declined the spelling. A scoped
/// classifier outranks a root package with the same spelling; once selected, a later missing segment
/// is that classifier path's error and must not reinterpret the root as the package.
pub(super) fn walk_qualifier_namespace_facets<S: SymbolSource + ?Sized>(
    source: &S,
    classifier_root: Option<TypeName>,
    root_expression: Option<ExprId>,
    root_name: &str,
    segments: &[(Option<ExprId>, String)],
) -> Result<ResolvedQualifier, QualifierError> {
    if let Some(classifier) = classifier_root {
        return walk_qualifier(source, ResolvedQualifier::Classifier(classifier), segments);
    }
    if source.package_exists(TypeName::ROOT, root_name) {
        let package = crate::types::type_name_child(TypeName::ROOT, root_name);
        return walk_qualifier(source, ResolvedQualifier::Package(package), segments);
    }
    Err(QualifierError::UnresolvedSegment {
        expression: root_expression,
        name: root_name.to_string(),
    })
}

/// Walk an absolute import/package spelling once, without flattening it or retrying alternative
/// nested-class separator placements.
pub(super) fn qualifier_path<S: SymbolSource + ?Sized>(
    path: &str,
    source: &S,
    scoped_root: Option<TypeName>,
) -> Result<ResolvedQualifier, QualifierError> {
    let segments = path
        .split(['.', '/'])
        .filter(|segment| !segment.is_empty())
        .map(|segment| (None, segment.to_string()))
        .collect::<Vec<_>>();
    let Some((root_expression, root_name)) = segments.first() else {
        return Err(QualifierError::UnresolvedSegment {
            expression: None,
            name: String::new(),
        });
    };
    let prefix = if let Some(classifier) = scoped_root {
        ResolvedQualifier::Classifier(classifier)
    } else if let Some(classifier) =
        classifier_identity(source, SymbolNamespace::Package(TypeName::ROOT), root_name)
    {
        ResolvedQualifier::Classifier(classifier)
    } else if source.package_exists(TypeName::ROOT, root_name) {
        ResolvedQualifier::Package(crate::types::type_name_child(TypeName::ROOT, root_name))
    } else {
        return Err(QualifierError::UnresolvedSegment {
            expression: *root_expression,
            name: root_name.clone(),
        });
    };
    walk_qualifier(source, prefix, &segments[1..])
}

pub(super) fn classifier_path<S: SymbolSource + ?Sized>(
    path: &str,
    source: &S,
    scoped_root: Option<TypeName>,
) -> Result<TypeName, QualifierError> {
    match qualifier_path(path, source, scoped_root)? {
        ResolvedQualifier::Classifier(internal) => Ok(internal),
        ResolvedQualifier::Package(_) | ResolvedQualifier::Value => {
            Err(QualifierError::UnresolvedSegment {
                expression: None,
                name: path
                    .rsplit(['.', '/'])
                    .find(|segment| !segment.is_empty())
                    .unwrap_or_default()
                    .to_string(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct CollidingRoot;

    impl SymbolSource for CollidingRoot {
        fn package_exists(&self, parent: TypeName, name: &str) -> bool {
            parent == TypeName::ROOT && name == "Clash"
        }

        fn symbols(
            &self,
            namespace: SymbolNamespace,
            name: &str,
        ) -> std::rc::Rc<crate::libraries::ResolvedSymbols> {
            let package = crate::types::type_name("Clash");
            if namespace == SymbolNamespace::Package(package) && name == "Tail" {
                return std::rc::Rc::new(crate::libraries::ResolvedSymbols {
                    classifier_name: Some(crate::types::type_name("Clash/Tail")),
                    classifier: Some(std::sync::Arc::new(
                        crate::libraries::LibraryType::declaration_header(),
                    )),
                    ..Default::default()
                });
            }
            std::rc::Rc::new(crate::libraries::ResolvedSymbols::default())
        }
    }

    #[test]
    fn a_later_classifier_miss_does_not_reinterpret_its_root_as_a_package() {
        let result = walk_qualifier_namespace_facets(
            &CollidingRoot,
            Some(crate::types::type_name("scope/Clash")),
            None,
            "Clash",
            &[(None, "Tail".to_string())],
        );
        assert_eq!(
            result,
            Err(QualifierError::UnresolvedSegment {
                expression: None,
                name: "Tail".to_string(),
            })
        );
    }
}
