//! Import-scope ownership and callable views over one scope-tower level.

use super::{classifier_scope, imported_object_member_symbols, SymbolResolver};
use crate::libraries::{
    Callables, FunctionInfo, FunctionSet, Origin, PropKind, PropertyInfo, PropertySet,
    ResolvedSymbols,
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
    /// Every explicit import of a visible name, in import order. Callables imported under one name
    /// from several owners are all candidates at the explicit level, as in kotlinc.
    explicit: crate::fx_hash::HashMap<String, Vec<CallableImport>>,
    /// Callable precedence: own package, explicit stars, Kotlin default stars, platform defaults.
    levels: [Vec<TypeName>; 4],
    /// Classifier precedence. Builtin classifiers outrank explicit stars; see
    /// [`classifier_scope::classifier_precedence_levels`].
    classifier_levels: Vec<classifier_scope::ClassifierImportLevel>,
}

impl FunctionImportScope {
    pub(crate) fn new(
        explicit_imports: impl IntoIterator<Item = (String, CallableImport)>,
        levels: [Vec<TypeName>; 4],
    ) -> Self {
        let classifier_levels = classifier_scope::classifier_precedence_levels(&levels);
        let mut explicit = crate::fx_hash::HashMap::<String, Vec<CallableImport>>::default();
        for (name, import) in explicit_imports {
            let imports = explicit.entry(name).or_default();
            imports.retain(|existing| {
                existing.owner != import.owner || existing.declared_name != import.declared_name
            });
            imports.push(import);
        }
        Self {
            explicit,
            levels,
            classifier_levels,
        }
    }

    pub(crate) fn has_explicit(&self, name: &str) -> bool {
        !self.explicit_targets(name).is_empty()
    }

    pub(crate) fn explicit_classifier_member_targets(&self, name: &str) -> Vec<(TypeName, String)> {
        self.explicit_targets(name)
            .into_iter()
            .filter_map(|(owner, declared_name)| match owner {
                SymbolNamespace::Classifier(owner) => Some((owner, declared_name)),
                SymbolNamespace::Package(_) => None,
            })
            .collect()
    }

    /// Every explicit import of `name`, in import order. `name$default` is the default stub of
    /// each imported callable.
    pub(crate) fn explicit_targets(&self, name: &str) -> Vec<(SymbolNamespace, String)> {
        if let Some(imports) = self.explicit.get(name) {
            return imports
                .iter()
                .map(|import| (import.owner, import.declared_name.clone()))
                .collect();
        }
        let Some(base) = name.strip_suffix("$default") else {
            return Vec::new();
        };
        self.explicit.get(base).map_or_else(Vec::new, |imports| {
            imports
                .iter()
                .map(|import| (import.owner, format!("{}$default", import.declared_name)))
                .collect()
        })
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

#[derive(Clone, Copy, PartialEq, Eq)]
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
    pub(super) rank: u32,
    pub(super) symbols: Vec<Rc<ResolvedSymbols>>,
}

impl FunctionScopeLevel {
    /// Whether the file brings this level into scope itself: its own package, an explicit import
    /// or an explicit star import, rather than a default import.
    pub(super) fn is_written_scope(&self) -> bool {
        match self.kind {
            FunctionScopeLevelKind::Explicit | FunctionScopeLevelKind::CurrentPackage => true,
            FunctionScopeLevelKind::Import => self.rank == EXPLICIT_STAR_IMPORT_RANK,
            FunctionScopeLevelKind::Flat => false,
        }
    }
}

/// The rank of the explicit star imports in [`FunctionImportScope`]'s callable levels.
const EXPLICIT_STAR_IMPORT_RANK: u32 = 1;

pub(super) fn tagged_symbol_levels_in_function_scope(
    src: &dyn SymbolSource,
    name: &str,
    scope: FunctionScopeRef<'_>,
) -> Vec<FunctionScopeLevel> {
    declaration_levels_in_function_scope(src, name, scope)
        .into_iter()
        .filter_map(|mut level| {
            if matches!(
                level.kind,
                FunctionScopeLevelKind::CurrentPackage | FunctionScopeLevelKind::Import
            ) {
                level.symbols.retain(|record| has_callables(record));
            }
            (!level.symbols.is_empty()).then_some(level)
        })
        .collect()
}

/// One name inventory retains every declaration facet. Callable-only consumers keep their
/// established filtered view through `tagged_symbol_levels_in_function_scope`.
pub(super) fn declaration_levels_in_function_scope(
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
                    rank: 0,
                    symbols: records,
                })
                .into_iter()
                .collect()
        }
        FunctionScopeRef::Imports(imports) => {
            let mut result = Vec::new();
            let mut explicit = Vec::new();
            for (owner, declared_name) in imports.explicit_targets(name) {
                let record = match owner {
                    SymbolNamespace::Classifier(classifier) => {
                        imported_object_member_symbols(src, classifier, &declared_name)
                            .unwrap_or_else(|| src.symbols(owner, &declared_name))
                    }
                    SymbolNamespace::Package(_) => src.symbols(owner, &declared_name),
                };
                crate::trace_compiler!(
                    "resolve",
                    "import scope {name}: explicit target={:?}.{} empty={}",
                    owner,
                    declared_name,
                    record.is_empty()
                );
                if !record.is_empty() {
                    explicit.push(record);
                }
            }
            if !explicit.is_empty() {
                result.push(FunctionScopeLevel {
                    kind: FunctionScopeLevelKind::Explicit,
                    rank: 0,
                    symbols: explicit,
                });
            }
            for (index, level) in imports.levels().iter().enumerate() {
                let records = symbols_at_scope_level(src, name, level);
                if !records.is_empty() {
                    crate::trace_compiler!(
                        "resolve",
                        "import scope {name}: level packages={} records={}",
                        level.len(),
                        records.len()
                    );
                    result.push(FunctionScopeLevel {
                        rank: u32::try_from(index).expect("import scope depth exceeds u32"),
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

impl SymbolResolver<'_> {
    /// The top-level properties named `name` at the nearest scope level that declares a visible
    /// one: the property facet of [`SymbolResolver::select_symbol`] on a receiver-less name,
    /// without collecting and selecting among the same name's function overloads.
    pub(crate) fn top_level_properties(&self, name: &str) -> Vec<PropertyInfo> {
        self.top_level_properties_for_access(name, super::PropertyAccess::Read)
    }

    pub(crate) fn top_level_properties_for_write(&self, name: &str) -> Vec<PropertyInfo> {
        self.top_level_properties_for_access(name, super::PropertyAccess::Write)
    }

    fn top_level_properties_for_access(
        &self,
        name: &str,
        access: super::PropertyAccess,
    ) -> Vec<PropertyInfo> {
        self.symbol_levels_in_scope(name)
            .iter()
            .map(|symbols| {
                level_properties(symbols)
                    .filter(|property| property.kind == PropKind::TopLevel)
                    .filter(|property| self.property_visible(property))
                    .filter(|property| match access {
                        super::PropertyAccess::Read => property.readable(),
                        super::PropertyAccess::Write => property.write_selectable(),
                    })
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .find(|properties| !properties.is_empty())
            .unwrap_or_default()
    }

    pub(super) fn nearest_top_level_properties(
        &self,
        levels: Vec<Vec<Rc<ResolvedSymbols>>>,
    ) -> Vec<PropertyInfo> {
        levels
            .iter()
            .map(|symbols| {
                level_properties(symbols)
                    .filter(|property| property.kind == PropKind::TopLevel)
                    .filter(|property| self.property_visible(property))
                    .filter(|property| property.readable())
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .find(|properties| !properties.is_empty())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::libraries::{Callables, FnKind, FunctionSet, LibraryCallable, PropertySet};
    use crate::types::Ty;

    #[test]
    fn classifier_facets_do_not_stop_callable_scope_selection() {
        struct SplitScope;
        impl SymbolSource for SplitScope {
            fn symbols(&self, namespace: SymbolNamespace, name: &str) -> Rc<ResolvedSymbols> {
                let mut record = ResolvedSymbols::default();
                if namespace == SymbolNamespace::Package(crate::types::type_name("near")) {
                    let classifier = crate::types::type_name("near/Selected");
                    record.classifier_name = Some(classifier);
                    record.classifier_declaration = Some(
                        crate::libraries::ClassifierDeclaration::Ordinary(classifier),
                    );
                } else if namespace == SymbolNamespace::Package(crate::types::type_name("far")) {
                    record.callables = Callables::Functions(FunctionSet {
                        overloads: vec![FunctionInfo::plain(
                            FnKind::Extension,
                            Some(Ty::String),
                            LibraryCallable::library(
                                "far/CallsKt",
                                name,
                                vec![Ty::String],
                                Ty::Unit,
                                Ty::Unit,
                                "(Ljava/lang/String;)V",
                            ),
                        )],
                    });
                }
                Rc::new(record)
            }
        }
        let imports = FunctionImportScope::new(
            [],
            [
                vec![crate::types::type_name("near")],
                vec![crate::types::type_name("far")],
                Vec::new(),
                Vec::new(),
            ],
        );
        for name in ["pick", "pick$default"] {
            let inventory = declaration_levels_in_function_scope(
                &SplitScope,
                name,
                FunctionScopeRef::Imports(&imports),
            );
            assert_eq!(inventory.len(), 2);
            assert_eq!(
                inventory[0].symbols[0].classifier_name,
                Some(crate::types::type_name("near/Selected"))
            );
            let callable =
                symbols_in_function_scope(&SplitScope, name, FunctionScopeRef::Imports(&imports));
            assert_eq!(callable.len(), 1);
            assert_eq!(callable[0].classifier_name, None);
            let functions = level_functions(&callable).collect::<Vec<_>>();
            assert_eq!(functions.len(), 1);
            assert_eq!(
                functions[0].callable.owner,
                crate::types::type_name("far/CallsKt")
            );
        }
    }

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
