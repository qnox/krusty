//! Classifier-only traversal of the shared import scope tower.
//!
//! Classifier selection retains records without callable facets so a constructor-only name remains
//! visible and aliases resolving to one identity collapse before ambiguity is decided. The order is not the
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
    pub(crate) scope_rung: crate::libraries::CallableScopeRung,
    pub(crate) scope_rank: u32,
}

impl ScopedClassifier {
    pub(crate) fn new(
        classifier: TypeName,
        alias: Option<crate::libraries::AliasExpansion>,
    ) -> Self {
        Self {
            classifier,
            alias,
            scope_rung: crate::libraries::CallableScopeRung::Unscoped,
            scope_rank: 0,
        }
    }
}

fn candidate_from_record(record: &crate::libraries::ResolvedSymbols) -> Option<ScopedClassifier> {
    record.classifier_name.map(|classifier| {
        ScopedClassifier::new(
            classifier,
            match &record.classifier_declaration {
                Some(crate::libraries::ClassifierDeclaration::TypeAlias(alias)) => {
                    Some(alias.clone())
                }
                Some(crate::libraries::ClassifierDeclaration::Ordinary(_)) | None => None,
            },
        )
    })
}

fn retain_classifier_candidate(
    candidates: &mut Vec<ScopedClassifier>,
    candidate: ScopedClassifier,
) {
    match candidates
        .iter_mut()
        .find(|previous| previous.classifier == candidate.classifier)
    {
        Some(previous) => {
            if previous.alias.as_ref().map(|alias| alias.identity)
                != candidate.alias.as_ref().map(|alias| alias.identity)
            {
                previous.alias = None;
            }
        }
        None => candidates.push(candidate),
    }
}

/// Select the classifier facet from the same records already read for a receiver call. Import
/// classifier precedence differs from callable precedence, but never requires another name query.
fn select_from_collected_levels(
    levels: &[super::scope_level_callables::FunctionScopeLevel],
) -> CandidateSelection<ScopedClassifier> {
    use super::scope_level_callables::FunctionScopeLevelKind as Kind;
    for (kind, rank, builtin_only) in [
        (Kind::Flat, 0, false),
        (Kind::Explicit, 0, false),
        (Kind::CurrentPackage, 0, false),
        (Kind::Import, 2, true),
        (Kind::Import, 1, false),
        (Kind::Import, 2, false),
        (Kind::Import, 3, false),
    ] {
        let mut candidates = Vec::new();
        for level in levels
            .iter()
            .filter(|level| level.kind == kind && level.rank == rank)
        {
            for record in &level.symbols {
                let Some(mut candidate) = candidate_from_record(record) else {
                    continue;
                };
                if builtin_only
                    && !(record.builtin_classifier
                        && named_default_package(candidate.classifier.namespace()))
                {
                    continue;
                }
                candidate.scope_rung = level.kind.callable_scope_rung();
                candidate.scope_rank = level.rank;
                if kind == Kind::Explicit {
                    if !candidates.iter().any(|previous: &ScopedClassifier| {
                        previous.classifier == candidate.classifier
                            && previous.alias.as_ref().map(|alias| alias.identity)
                                == candidate.alias.as_ref().map(|alias| alias.identity)
                    }) {
                        candidates.push(candidate);
                    }
                } else {
                    retain_classifier_candidate(&mut candidates, candidate);
                }
            }
        }
        match choose(candidates) {
            CandidateSelection::None => {}
            selected => return selected,
        }
    }
    CandidateSelection::None
}

fn named_default_package(package: TypeName) -> bool {
    crate::resolve::KOTLIN_NAMED_DEFAULT_IMPORT_PACKAGES
        .iter()
        .any(|name| package == crate::types::type_name(&name.replace('.', "/")))
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
        let namespace = if source.classifier(owner).is_some() {
            SymbolNamespace::Classifier(owner)
        } else {
            SymbolNamespace::Package(owner)
        };
        let record = source.symbols(namespace, name);
        let candidate = candidate_from_record(&record);
        let Some(candidate) = candidate else {
            continue;
        };
        retain_classifier_candidate(&mut candidates, candidate);
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
            let mut selected = ScopedClassifier::new(classifier, alias);
            selected.scope_rung = crate::libraries::CallableScopeRung::ExplicitImport;
            candidates.push(selected);
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
    /// Lexical classifier declarations precede the file/import tower for a receiver alias call.
    /// The selected declaration, including an ordinary classifier that shadows an alias, is final.
    pub(super) fn scoped_receiver_constructor_classifier(
        &self,
        name: &str,
        levels: &[super::scope_level_callables::FunctionScopeLevel],
    ) -> CandidateSelection<ScopedClassifier> {
        for (rank, &owner) in self.lexical_classes.iter().enumerate() {
            let mut candidates =
                scoped_classifier_candidates_at_scope_level(&self.src, name, &[owner]);
            for candidate in &mut candidates {
                candidate.scope_rank =
                    u32::try_from(rank).expect("lexical scope depth exceeds callable rank");
            }
            match choose(candidates) {
                CandidateSelection::None => {}
                selected => return selected,
            }
        }
        select_from_collected_levels(levels)
    }

    /// [`Self::classifier_in_scope`] with the alias declaration that named the selection, if any.
    pub(crate) fn scoped_classifier_in_scope(
        &self,
        name: &str,
    ) -> CandidateSelection<ScopedClassifier> {
        select(&self.src, self.fn_scope, name)
    }

    /// Whether the nearest receiver-less function named `name` sits on a file/import rung strictly
    /// nearer than the rung that selects `classifier` for the same spelling. A bare call then offers
    /// that function before the classifier's constructors: kotlinc 2.4.21 calls a same-package
    /// `fun Pair(a: Int, b: Int)` rather than the default-imported `kotlin.Pair` constructor.
    pub(crate) fn top_level_function_precedes_classifier(
        &self,
        name: &str,
        classifier: TypeName,
    ) -> bool {
        let (Some(scope), CandidateSelection::Selected(selected)) =
            (self.fn_scope, self.scoped_classifier_in_scope(name))
        else {
            return false;
        };
        selected.classifier == classifier
            && super::scope_level_callables::tagged_symbol_levels_in_function_scope(
                &self.src, name, scope,
            )
            .into_iter()
            .find(|level| {
                super::scope_level_callables::level_functions(&level.symbols)
                    .next()
                    .is_some()
            })
            .is_some_and(|level| {
                level.kind.callable_scope_rung().selection_key(level.rank)
                    < selected.scope_rung.selection_key(selected.scope_rank)
            })
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
    pub(crate) callable_position: (crate::libraries::CallableScopeRung, u32),
}

impl From<Vec<TypeName>> for ClassifierImportLevel {
    fn from(packages: Vec<TypeName>) -> Self {
        Self {
            packages,
            builtins_only: false,
            callable_position: (crate::libraries::CallableScopeRung::Unscoped, 0),
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
    for candidate in &mut candidates {
        (candidate.scope_rung, candidate.scope_rank) = level.callable_position;
    }
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
    let named = levels[2]
        .iter()
        .copied()
        .filter(|&package| named_default_package(package))
        .collect();
    vec![
        ClassifierImportLevel {
            packages: levels[0].clone(),
            builtins_only: false,
            callable_position: (crate::libraries::CallableScopeRung::CurrentPackage, 0),
        },
        ClassifierImportLevel {
            packages: named,
            builtins_only: true,
            callable_position: (crate::libraries::CallableScopeRung::Import, 2),
        },
        ClassifierImportLevel {
            packages: levels[1].clone(),
            builtins_only: false,
            callable_position: (crate::libraries::CallableScopeRung::Import, 1),
        },
        ClassifierImportLevel {
            packages: levels[2].clone(),
            builtins_only: false,
            callable_position: (crate::libraries::CallableScopeRung::Import, 2),
        },
        ClassifierImportLevel {
            packages: levels[3].clone(),
            builtins_only: false,
            callable_position: (crate::libraries::CallableScopeRung::Import, 3),
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
