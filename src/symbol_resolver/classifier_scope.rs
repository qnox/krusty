//! Classifier-only traversal of the shared import scope tower.
//!
//! Callable collection deliberately drops records without callable facets. Classifier selection
//! walks the same packages without that filter so a constructor-only name remains visible and
//! aliases resolving to one identity collapse before ambiguity is decided. The order is not the
//! callable order: a classifier declared in a `.kotlin_builtins` fragment is a simple default
//! import and outranks a star import. A stdlib class in the same package is only a default star.

use crate::symbol_source::{SymbolNamespace, SymbolSource};
use crate::types::TypeName;

use super::{CandidateSelection, FunctionScopeRef};

/// A classifier selected on the classifier tower together with the typealias declaration, when
/// any, whose record named it on the winning rung. The provenance travels with the selection, so a
/// consumer never rediscovers alias-ness from the spelling after a nearer rung already won.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ScopedClassifier {
    pub(crate) classifier: TypeName,
    pub(crate) alias: Option<crate::libraries::AliasExpansion>,
}

/// The classifier candidates `name` denotes in `owners` (packages, or classifiers whose nested
/// classifiers are in scope), each with the declaration that supplied it. Candidates naming one
/// classifier collapse before ambiguity is decided; a collapsed candidate whose records name
/// different aliases names no single alias, so it carries none.
pub(crate) fn scoped_classifier_candidates_at_scope_level<S: SymbolSource + ?Sized>(
    source: &S,
    name: &str,
    owners: &[TypeName],
) -> Vec<ScopedClassifier> {
    let mut candidates: Vec<ScopedClassifier> = Vec::new();
    for &owner in owners {
        let candidate = if source.classifier(owner).is_some() {
            let nested = owner
                .existing_nested_child(name)
                .unwrap_or_else(|| crate::types::type_name_nested_child(owner, name));
            source
                .classifier(nested)
                .filter(|classifier| classifier.is_nested && nested.nested_owner() == Some(owner))
                .map(|_| ScopedClassifier {
                    classifier: nested,
                    alias: None,
                })
        } else {
            let record = source.symbols(SymbolNamespace::Package(owner), name);
            record.classifier_name.map(|classifier| ScopedClassifier {
                classifier,
                alias: match &record.classifier_declaration {
                    Some(crate::libraries::ClassifierDeclaration::TypeAlias(alias)) => {
                        Some(alias.clone())
                    }
                    Some(crate::libraries::ClassifierDeclaration::Ordinary(_)) | None => None,
                },
            })
        };
        let Some(candidate) = candidate else {
            continue;
        };
        match candidates
            .iter_mut()
            .find(|previous| previous.classifier == candidate.classifier)
        {
            Some(previous) => {
                let same_alias = previous.alias.as_ref().map(|alias| alias.identity)
                    == candidate.alias.as_ref().map(|alias| alias.identity);
                if !same_alias {
                    previous.alias = None;
                }
            }
            None => candidates.push(candidate),
        }
    }
    candidates
}

/// Classifier facets of every explicit import under `name`. Callable/property-only imports do not
/// participate. Repeated imports of the same declaration collapse, but two typealias declarations
/// remain distinct even when they expand to the same classifier.
pub(crate) fn explicit_classifier_candidates<S: SymbolSource + ?Sized>(
    source: &S,
    imports: &super::FunctionImportScope,
    name: &str,
) -> Vec<ScopedClassifier> {
    let mut candidates: Vec<ScopedClassifier> = Vec::new();
    for (owner, declared_name) in imports.explicit_targets(name) {
        let record = source.symbols(owner, &declared_name);
        let Some(classifier) = record.classifier_name else {
            continue;
        };
        let alias = match &record.classifier_declaration {
            Some(crate::libraries::ClassifierDeclaration::TypeAlias(alias)) => Some(alias.clone()),
            Some(crate::libraries::ClassifierDeclaration::Ordinary(_)) | None => None,
        };
        let alias_identity = alias.as_ref().map(|alias| alias.identity);
        if !candidates.iter().any(|previous| {
            previous.classifier == classifier
                && previous.alias.as_ref().map(|alias| alias.identity) == alias_identity
        }) {
            candidates.push(ScopedClassifier { classifier, alias });
        }
    }
    candidates
}

impl<T> CandidateSelection<T> {
    pub(crate) fn map<U>(self, f: impl FnOnce(T) -> U) -> CandidateSelection<U> {
        match self {
            Self::None => CandidateSelection::None,
            Self::Selected(selected) => CandidateSelection::Selected(f(selected)),
            Self::Ambiguous => CandidateSelection::Ambiguous,
        }
    }
}

impl super::SymbolResolver<'_> {
    /// [`Self::classifier_in_scope`] with the alias declaration that named the selection, if any.
    pub(crate) fn scoped_classifier_in_scope(
        &self,
        name: &str,
    ) -> CandidateSelection<ScopedClassifier> {
        select(&self.src, self.fn_scope, name)
    }
}

/// One rung of classifier import precedence.
///
/// `builtins_only` keeps a resolved classifier only when it is declared in a `.kotlin_builtins`
/// fragment. The package list says where to look; the resolved class decides whether that look is
/// a simple import.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ClassifierImportLevel {
    pub(crate) packages: Vec<TypeName>,
    pub(crate) builtins_only: bool,
}

impl From<Vec<TypeName>> for ClassifierImportLevel {
    fn from(packages: Vec<TypeName>) -> Self {
        Self {
            packages,
            builtins_only: false,
        }
    }
}

/// Classifier candidates at one import rung. A builtins rung resolves the name in the builtin
/// packages, then keeps the candidate only when that class is a `.kotlin_builtins` declaration.
pub(crate) fn classifier_candidates_at_import_level<S: SymbolSource + ?Sized>(
    source: &S,
    name: &str,
    level: &ClassifierImportLevel,
) -> Vec<TypeName> {
    scoped_classifier_candidates_at_import_level(source, name, level)
        .into_iter()
        .map(|candidate| candidate.classifier)
        .collect()
}

/// [`classifier_candidates_at_import_level`] with each candidate's declaration provenance.
pub(crate) fn scoped_classifier_candidates_at_import_level<S: SymbolSource + ?Sized>(
    source: &S,
    name: &str,
    level: &ClassifierImportLevel,
) -> Vec<ScopedClassifier> {
    let mut candidates = scoped_classifier_candidates_at_scope_level(source, name, &level.packages);
    if level.builtins_only {
        candidates.retain(|candidate| {
            let (namespace, leaf) = SymbolNamespace::classifier_key(candidate.classifier);
            let record = source.symbols(namespace, leaf);
            record.builtin_classifier && record.classifier_name == Some(candidate.classifier)
        });
    }
    candidates
}

/// Classifier precedence derived from the callable import levels
/// `[own package, explicit stars, Kotlin default stars, platform defaults]`.
///
/// Own package stays first. Builtins in `kotlin`, `kotlin.annotation`, `kotlin.collections`, and
/// `kotlin.ranges` are the next rung and outrank explicit stars. Those same packages remain
/// default stars after the explicit stars, so `kotlin.collections.ArrayList` is still found when
/// no star import supplies another `ArrayList`. Callable lookup keeps the stored order, so
/// `import other.*` still outranks `kotlin.collections.map`.
pub(crate) fn classifier_precedence_levels(
    levels: &[Vec<TypeName>; 4],
) -> Vec<ClassifierImportLevel> {
    let named_default = |package: TypeName| {
        crate::resolve::KOTLIN_NAMED_DEFAULT_IMPORT_PACKAGES
            .iter()
            .any(|name| package == crate::types::type_name(&name.replace('.', "/")))
    };
    let named = levels[2]
        .iter()
        .copied()
        .filter(|&package| named_default(package))
        .collect();
    vec![
        ClassifierImportLevel {
            packages: levels[0].clone(),
            builtins_only: false,
        },
        ClassifierImportLevel {
            packages: named,
            builtins_only: true,
        },
        ClassifierImportLevel {
            packages: levels[1].clone(),
            builtins_only: false,
        },
        ClassifierImportLevel {
            packages: levels[2].clone(),
            builtins_only: false,
        },
        ClassifierImportLevel {
            packages: levels[3].clone(),
            builtins_only: false,
        },
    ]
}

pub(super) fn select(
    source: &dyn SymbolSource,
    scope: Option<FunctionScopeRef<'_>>,
    name: &str,
) -> CandidateSelection<ScopedClassifier> {
    match scope {
        None => CandidateSelection::None,
        Some(FunctionScopeRef::Flat(packages)) => choose(
            scoped_classifier_candidates_at_scope_level(source, name, packages),
        ),
        Some(FunctionScopeRef::Imports(imports)) => {
            match choose(explicit_classifier_candidates(source, imports, name)) {
                CandidateSelection::None => {}
                selected => return selected,
            }
            for level in imports.classifier_levels() {
                match choose(scoped_classifier_candidates_at_import_level(
                    source, name, level,
                )) {
                    CandidateSelection::None => {}
                    selected => return selected,
                }
            }
            CandidateSelection::None
        }
    }
}

fn choose(mut candidates: Vec<ScopedClassifier>) -> CandidateSelection<ScopedClassifier> {
    match candidates.len() {
        0 => CandidateSelection::None,
        1 => CandidateSelection::Selected(candidates.remove(0)),
        _ => CandidateSelection::Ambiguous,
    }
}

#[cfg(test)]
mod tests {
    use crate::types::type_name;

    #[test]
    fn named_default_classifiers_outrank_explicit_stars() {
        let levels = super::classifier_precedence_levels(&[
            vec![type_name("own")],
            vec![type_name("foo"), type_name("java/util")],
            crate::resolve::KOTLIN_DEFAULT_IMPORT_PACKAGES
                .iter()
                .map(|package| type_name(&package.replace('.', "/")))
                .collect(),
            vec![type_name("java/lang")],
        ]);
        assert_eq!(levels[0].packages, vec![type_name("own")]);
        assert!(!levels[0].builtins_only);
        assert_eq!(
            levels[1].packages,
            crate::resolve::KOTLIN_NAMED_DEFAULT_IMPORT_PACKAGES
                .iter()
                .map(|package| type_name(&package.replace('.', "/")))
                .collect::<Vec<_>>()
        );
        assert!(levels[1].builtins_only);
        assert_eq!(
            levels[2].packages,
            vec![type_name("foo"), type_name("java/util")]
        );
        assert!(!levels[2].builtins_only);
        assert_eq!(
            levels[3].packages,
            crate::resolve::KOTLIN_DEFAULT_IMPORT_PACKAGES
                .iter()
                .map(|package| type_name(&package.replace('.', "/")))
                .collect::<Vec<_>>()
        );
        assert!(!levels[3].builtins_only);
        assert_eq!(levels[4].packages, vec![type_name("java/lang")]);
        assert!(!levels[4].builtins_only);
    }
}
