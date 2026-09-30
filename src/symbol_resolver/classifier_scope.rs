//! Classifier-only traversal of the shared import scope tower.
//!
//! Callable collection deliberately drops records without callable facets. Classifier selection
//! walks the same packages without that filter so a constructor-only name remains visible and
//! aliases resolving to one identity collapse before ambiguity is decided. The order is not the
//! callable order: a classifier declared in a `.kotlin_builtins` fragment is a simple default
//! import and outranks a star import. A stdlib class in the same package is only a default star.

use crate::symbol_source::{SymbolNamespace, SymbolSource};
use crate::types::TypeName;

use super::{classifier_candidates_at_scope_level, CandidateSelection, FunctionScopeRef};

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
    let mut candidates = classifier_candidates_at_scope_level(source, name, &level.packages);
    if level.builtins_only {
        candidates.retain(|candidate| {
            let (namespace, leaf) = SymbolNamespace::classifier_key(*candidate);
            let record = source.symbols(namespace, leaf);
            record.builtin_classifier && record.classifier_name == Some(*candidate)
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
) -> CandidateSelection<TypeName> {
    match scope {
        None => CandidateSelection::None,
        Some(FunctionScopeRef::Flat(packages)) => {
            choose(classifier_candidates_at_scope_level(source, name, packages))
        }
        Some(FunctionScopeRef::Imports(imports)) => {
            if imports.explicit_is_ambiguous(name) {
                return CandidateSelection::Ambiguous;
            }
            if let Some((owner, declared_name)) = imports.explicit_target(name) {
                if let Some(candidate) = source.symbols(owner, &declared_name).classifier_name {
                    return CandidateSelection::Selected(candidate);
                }
            }
            for level in imports.classifier_levels() {
                match choose(classifier_candidates_at_import_level(source, name, level)) {
                    CandidateSelection::None => {}
                    selected => return selected,
                }
            }
            CandidateSelection::None
        }
    }
}

fn choose(candidates: Vec<TypeName>) -> CandidateSelection<TypeName> {
    match candidates.as_slice() {
        [] => CandidateSelection::None,
        [candidate] => CandidateSelection::Selected(*candidate),
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
