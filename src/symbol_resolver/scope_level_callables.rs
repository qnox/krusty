//! Import-scope ownership and callable views over one scope-tower level.

use super::{classifier_scope, imported_object_member_symbols};
use crate::libraries::{
    Callables, FunctionInfo, FunctionSet, Origin, PropertyInfo, PropertySet, ResolvedSymbols,
};
use crate::symbol_source::{SymbolNamespace, SymbolSource};
use crate::types::TypeName;
use std::rc::Rc;

#[derive(Clone, Debug)]
pub(crate) struct CallableImport {
    owner: SymbolNamespace,
    declared_name: String,
}

impl CallableImport {
    pub(crate) fn new(owner: SymbolNamespace, declared_name: String) -> Self {
        Self {
            owner,
            declared_name,
        }
    }
}

/// Name-aware import scope for unqualified top-level and extension callables.
#[derive(Clone, Debug)]
pub(crate) struct FunctionImportScope {
    explicit: std::collections::HashMap<String, CallableImport>,
    ambiguous_explicit: std::collections::HashSet<String>,
    /// Callable precedence: own package, explicit stars, Kotlin default stars, platform defaults.
    levels: [Vec<TypeName>; 4],
    /// Classifier precedence. Builtin classifiers outrank explicit stars; see
    /// [`classifier_scope::classifier_precedence_levels`].
    classifier_levels: Vec<classifier_scope::ClassifierImportLevel>,
}

impl FunctionImportScope {
    pub(crate) fn new(
        explicit: std::collections::HashMap<String, CallableImport>,
        levels: [Vec<TypeName>; 4],
    ) -> Self {
        let classifier_levels = classifier_scope::classifier_precedence_levels(&levels);
        Self {
            explicit,
            ambiguous_explicit: std::collections::HashSet::new(),
            levels,
            classifier_levels,
        }
    }

    pub(crate) fn with_ambiguous_explicit(
        mut self,
        names: std::collections::HashSet<String>,
    ) -> Self {
        self.ambiguous_explicit = names;
        self
    }

    pub(crate) fn explicit_is_ambiguous(&self, name: &str) -> bool {
        self.ambiguous_explicit.contains(name)
    }

    pub(crate) fn explicit_owner(&self, name: &str) -> Option<SymbolNamespace> {
        self.explicit
            .get(name)
            .map(|import| import.owner)
            .or_else(|| {
                name.strip_suffix("$default")
                    .and_then(|base| self.explicit.get(base).map(|import| import.owner))
            })
    }

    pub(crate) fn explicit_target(&self, name: &str) -> Option<(SymbolNamespace, String)> {
        if let Some(import) = self.explicit.get(name) {
            return Some((import.owner, import.declared_name.clone()));
        }
        let base = name.strip_suffix("$default")?;
        let import = self.explicit.get(base)?;
        Some((import.owner, format!("{}$default", import.declared_name)))
    }

    pub(crate) fn levels(&self) -> &[Vec<TypeName>; 4] {
        &self.levels
    }

    pub(crate) fn classifier_levels(&self) -> &[classifier_scope::ClassifierImportLevel] {
        &self.classifier_levels
    }
}

#[derive(Clone, Copy)]
pub(super) enum FunctionScopeRef<'a> {
    Flat(&'a [TypeName]),
    Imports(&'a FunctionImportScope),
}

impl FunctionScopeRef<'_> {
    pub(super) fn package_count(self) -> usize {
        match self {
            Self::Flat(packages) => packages.len(),
            Self::Imports(scope) => {
                scope.explicit.len() + scope.levels.iter().map(Vec::len).sum::<usize>()
            }
        }
    }
}

/// The shared unqualified-name resolution loop. Each caller applies its own classifier or callable
/// selection rules to the namespace-separated records returned for this one scope rung.
fn symbols_at_scope_level(
    src: &dyn SymbolSource,
    name: &str,
    packages: &[TypeName],
) -> Vec<Rc<ResolvedSymbols>> {
    packages
        .iter()
        .filter_map(|pkg| {
            let record = src.symbols(SymbolNamespace::Package(*pkg), name);
            (!record.is_empty()).then_some(record)
        })
        .collect()
}

fn has_callables(record: &ResolvedSymbols) -> bool {
    !matches!(record.callables, Callables::None)
}

pub(super) fn symbols_in_function_scope(
    src: &dyn SymbolSource,
    name: &str,
    scope: FunctionScopeRef<'_>,
) -> Vec<Rc<ResolvedSymbols>> {
    symbol_levels_in_function_scope(src, name, scope)
        .into_iter()
        .next()
        .unwrap_or_default()
}

pub(super) fn symbol_levels_in_function_scope(
    src: &dyn SymbolSource,
    name: &str,
    scope: FunctionScopeRef<'_>,
) -> Vec<Vec<Rc<ResolvedSymbols>>> {
    tagged_symbol_levels_in_function_scope(src, name, scope)
        .into_iter()
        .map(|level| level.symbols)
        .collect()
}

#[derive(Clone, Copy)]
pub(super) enum FunctionScopeLevelKind {
    Flat,
    Explicit,
    CurrentPackage,
    Import,
}

impl FunctionScopeLevelKind {
    pub(super) fn ambiguity_checks(self) -> bool {
        matches!(self, Self::Import)
    }

    pub(super) fn callable_scope_rung(self) -> crate::libraries::CallableScopeRung {
        match self {
            Self::Flat => crate::libraries::CallableScopeRung::Unscoped,
            Self::Explicit => crate::libraries::CallableScopeRung::ExplicitImport,
            Self::CurrentPackage => crate::libraries::CallableScopeRung::CurrentPackage,
            Self::Import => crate::libraries::CallableScopeRung::Import,
        }
    }
}

pub(super) struct FunctionScopeLevel {
    pub(super) kind: FunctionScopeLevelKind,
    pub(super) symbols: Vec<Rc<ResolvedSymbols>>,
}

pub(super) fn tagged_symbol_levels_in_function_scope(
    src: &dyn SymbolSource,
    name: &str,
    scope: FunctionScopeRef<'_>,
) -> Vec<FunctionScopeLevel> {
    match scope {
        FunctionScopeRef::Flat(packages) => {
            let records = symbols_at_scope_level(src, name, packages);
            (!records.is_empty())
                .then_some(FunctionScopeLevel {
                    kind: FunctionScopeLevelKind::Flat,
                    symbols: records,
                })
                .into_iter()
                .collect()
        }
        FunctionScopeRef::Imports(imports) => {
            let mut result = Vec::new();
            if let Some((owner, declared_name)) = imports.explicit_target(name) {
                let record = match owner {
                    SymbolNamespace::Classifier(classifier) => {
                        imported_object_member_symbols(src, classifier, &declared_name)
                            .unwrap_or_else(|| src.symbols(owner, &declared_name))
                    }
                    SymbolNamespace::Package(_) => src.symbols(owner, &declared_name),
                };
                let records = (!record.is_empty())
                    .then_some(record)
                    .into_iter()
                    .collect::<Vec<_>>();
                crate::trace_compiler!(
                    "resolve",
                    "import scope {name}: explicit target={:?}.{} records={}",
                    owner,
                    declared_name,
                    records.len()
                );
                if !records.is_empty() {
                    result.push(FunctionScopeLevel {
                        kind: FunctionScopeLevelKind::Explicit,
                        symbols: records,
                    });
                }
            }
            for (index, level) in imports.levels().iter().enumerate() {
                let records = symbols_at_scope_level(src, name, level)
                    .into_iter()
                    .filter(|record| has_callables(record))
                    .collect::<Vec<_>>();
                if !records.is_empty() {
                    crate::trace_compiler!(
                        "resolve",
                        "import scope {name}: level packages={} records={}",
                        level.len(),
                        records.len()
                    );
                    result.push(FunctionScopeLevel {
                        kind: if index == 0 {
                            FunctionScopeLevelKind::CurrentPackage
                        } else {
                            FunctionScopeLevelKind::Import
                        },
                        symbols: records,
                    });
                }
            }
            result
        }
    }
}

pub(super) fn callables_from_symbols(symbols: &[Rc<ResolvedSymbols>]) -> Callables {
    let mut functions = FunctionSet::default();
    let mut properties = PropertySet::default();
    for record in symbols {
        functions
            .overloads
            .extend(record.callables.functions().iter().cloned());
        properties
            .overloads
            .extend(record.callables.properties().iter().cloned());
    }
    Callables::from_parts(functions, properties)
}

/// Whether a classpath callable is visible for an unqualified call in `fn_scope`.
pub(super) fn fn_in_scope(o: &FunctionInfo, fn_scope: Option<FunctionScopeRef<'_>>) -> bool {
    if !matches!(o.callable.origin, Origin::Library) {
        return true;
    }
    match fn_scope {
        None => true,
        Some(FunctionScopeRef::Flat(scope)) => scope
            .iter()
            .any(|&package| o.callable.owner_package_matches_name(package)),
        Some(FunctionScopeRef::Imports(_)) => true,
    }
}

/// Materialize one scope level's functions when the consumer needs ownership of the full set.
pub(super) fn function_set_from_symbols(symbols: &[Rc<ResolvedSymbols>]) -> FunctionSet {
    let capacity = symbols
        .iter()
        .map(|record| record.callables.functions().len())
        .sum();
    let mut overloads = Vec::with_capacity(capacity);
    for record in symbols {
        overloads.extend(record.callables.functions().iter().cloned());
    }
    FunctionSet { overloads }
}

/// Function overloads borrowed in the same record order as the materialized callable union.
/// Receiver walks filter a level down to applicable extensions, so they copy only retained
/// candidates rather than every overload in every record.
pub(super) fn level_functions(
    symbols: &[Rc<ResolvedSymbols>],
) -> impl Iterator<Item = &FunctionInfo> {
    symbols
        .iter()
        .flat_map(|record| record.callables.functions().iter())
}

/// Property overloads borrowed in record order.
pub(super) fn level_properties(
    symbols: &[Rc<ResolvedSymbols>],
) -> impl Iterator<Item = &PropertyInfo> {
    symbols
        .iter()
        .flat_map(|record| record.callables.properties().iter())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::libraries::{Callables, FnKind, FunctionSet, LibraryCallable, PropertySet};
    use crate::types::Ty;

    #[test]
    fn overloads_are_borrowed_in_record_order() {
        let function = |owner: &str| {
            FunctionInfo::plain(
                FnKind::Extension,
                Some(Ty::String),
                LibraryCallable::library(
                    owner,
                    "pick",
                    vec![Ty::String],
                    Ty::Unit,
                    Ty::Unit,
                    "(Ljava/lang/String;)V",
                ),
            )
        };
        let record = |owners: &[&str]| {
            Rc::new(ResolvedSymbols {
                builtin_classifier: false,
                classifier_name: None,
                classifier_declaration: None,
                classifier: None,
                callables: Callables::from_parts(
                    FunctionSet {
                        overloads: owners.iter().map(|owner| function(owner)).collect(),
                    },
                    PropertySet::default(),
                ),
                importable_declaration: false,
            })
        };
        let symbols = vec![
            record(&["levels/AKt", "levels/BKt"]),
            record(&[]),
            record(&["levels/CKt"]),
        ];

        let borrowed = level_functions(&symbols).collect::<Vec<_>>();
        let copied = super::callables_from_symbols(&symbols);
        assert_eq!(borrowed.len(), copied.functions().len());
        assert!(borrowed
            .iter()
            .zip(copied.functions())
            .all(|(borrowed, copied)| borrowed.callable.owner == copied.callable.owner));
        assert!(std::ptr::eq(
            borrowed[2],
            &symbols[2].callables.functions()[0]
        ));
        assert_eq!(level_properties(&symbols).count(), 0);
    }
}
