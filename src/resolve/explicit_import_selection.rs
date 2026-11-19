//! A file's explicit imports as classifier candidates: the visible-name map and the selection of the
//! classifier an import set binds a name to. Imports share syntax but not namespaces, so a
//! callable import never hides a classifier of the same name.

use super::{qualifier_path, ResolvedQualifier};
use crate::ast::File;
use crate::fx_hash::HashMap;
use crate::symbol_resolver::InheritedNestedClassifier;
use crate::symbol_source::SymbolSource;
use crate::types::TypeName;

/// Map a file's EXPLICIT imports `simple name -> internal names` (e.g. `Calc -> [util/Calc]`). Imports
/// share syntax but not namespaces, so one visible name can be imported from several owners (a
/// class and a same-named factory function); every import stays, in source order, and each
/// namespace selects from the ones that declare something in it. A wildcard import (`a.b.*`) has
/// no simple name — those are represented by [`import_levels`].
pub fn import_map(file: &File) -> HashMap<String, Vec<String>> {
    let mut m: HashMap<String, Vec<String>> = HashMap::default();
    for import in &file.import_paths {
        if import.wildcard {
            continue;
        }
        let fq = import.path();
        if let Some(simple) = import.imported_name() {
            let internal = fq.replace('.', "/");
            let paths = m.entry(simple.to_string()).or_default();
            if !paths.contains(&internal) {
                paths.push(internal);
            }
        }
    }
    m
}

/// Resolve a name to its fully-qualified internal name against a file's import set — the single
/// kotlinc-conforming resolver both the signature pass and the [`Checker`] use, so a name resolves
/// identically wherever it appears. A name is ALWAYS an FQN; an unqualified use forms candidate FQNs
/// `pkg/name` from the in-scope packages. Precedence (spec § resolution): an explicit non-star import
/// names the FQN directly (highest); otherwise each precedence level (`levels`, descending) supplies
/// candidates and the FIRST level with any resolving candidate wins. Two or more DISTINCT resolutions
/// within one level are AMBIGUOUS — kotlinc rejects a name two star-imports both supply — and resolve
/// to `None` (a genuinely ambiguous name never appears in a compiling program). A classpath `typealias`
/// candidate resolves to its target internal. Existence is verified via `resolve_type`.
pub(super) fn classifier_from_imports<S: SymbolSource + ?Sized>(
    name: &str,
    explicit: &HashMap<String, Vec<String>>,
    levels: &[crate::symbol_resolver::ClassifierImportLevel],
    source: &S,
) -> InheritedNestedClassifier {
    match imported_classifier_selection(name, explicit, levels, source) {
        crate::symbol_resolver::CandidateSelectionWithTies::Selected(classifier) => {
            InheritedNestedClassifier::Found(classifier)
        }
        crate::symbol_resolver::CandidateSelectionWithTies::Ambiguous(_) => {
            InheritedNestedClassifier::Ambiguous
        }
        crate::symbol_resolver::CandidateSelectionWithTies::None => {
            InheritedNestedClassifier::NotFound
        }
    }
}

/// The classifier candidates of one visible name's explicit imports: the imports whose path names
/// a classifier. The imports are normalized into an import scope and selected by the one
/// declaration-aware rule, [`crate::symbol_resolver::explicit_classifier_candidates`], so two
/// paths that reach one classifier (a typealias and its target) stay two candidates.
pub(crate) fn explicit_classifier_selection<S: SymbolSource + ?Sized>(
    name: &str,
    paths: Option<&[String]>,
    source: &S,
) -> Vec<crate::symbol_resolver::ScopedClassifier> {
    use crate::symbol_source::SymbolNamespace;
    let imports = paths.unwrap_or_default().iter().filter_map(|path| {
        let (parent, declared_name) = path.rsplit_once('/').unwrap_or(("", path.as_str()));
        let owner = if parent.is_empty() {
            SymbolNamespace::Package(TypeName::ROOT)
        } else {
            match qualifier_path(parent, source, None).ok()? {
                ResolvedQualifier::Package(package) => SymbolNamespace::Package(package),
                ResolvedQualifier::Classifier(classifier) => {
                    SymbolNamespace::Classifier(classifier)
                }
                ResolvedQualifier::Value => return None,
            }
        };
        Some((
            name.to_owned(),
            crate::symbol_resolver::CallableImport::new(owner, declared_name.to_owned()),
        ))
    });
    let scope = crate::symbol_resolver::FunctionImportScope::new(imports, Default::default());
    crate::symbol_resolver::explicit_classifier_candidates(source, &scope, name)
}

/// The classifier the imports bind `name` to: an explicit import, else the nearest import level
/// that has any candidate, keeping every candidate of an ambiguous level.
pub(super) fn imported_classifier_selection<S: SymbolSource + ?Sized>(
    name: &str,
    explicit: &HashMap<String, Vec<String>>,
    levels: &[crate::symbol_resolver::ClassifierImportLevel],
    source: &S,
) -> crate::symbol_resolver::CandidateSelectionWithTies<TypeName> {
    use crate::symbol_resolver::CandidateSelectionWithTies;
    // Imports share syntax but not namespaces. An explicit import of a callable named `Foo` must
    // not erase a same-file classifier `Foo`, nor an explicit class import of the same name; only
    // the imports whose path resolves to a classifier are classifier candidates, and two distinct
    // classifiers under one name are an ambiguity.
    let candidates =
        explicit_classifier_selection(name, explicit.get(name).map(Vec::as_slice), source);
    match candidates.as_slice() {
        [] => {}
        [candidate] => return CandidateSelectionWithTies::Selected(candidate.classifier),
        _ => {
            return CandidateSelectionWithTies::Ambiguous(
                candidates
                    .into_iter()
                    .map(|candidate| candidate.classifier)
                    .collect(),
            )
        }
    }
    for level in levels {
        let hits =
            crate::symbol_resolver::classifier_candidates_at_import_level(source, name, level);
        match hits.as_slice() {
            [] => continue,
            [classifier] => return CandidateSelectionWithTies::Selected(*classifier),
            _ => return CandidateSelectionWithTies::Ambiguous(hits),
        }
    }
    CandidateSelectionWithTies::None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Ty;

    struct ClassifierImportSource;

    fn import_test_classifier(alias_target: Option<TypeName>) -> crate::libraries::LibraryType {
        crate::libraries::LibraryType {
            is_kotlin: true,
            alias_target,
            ..crate::libraries::LibraryType::declaration_header()
        }
    }

    fn import_classifier(
        internal: crate::types::TypeName,
    ) -> Option<std::sync::Arc<crate::libraries::LibraryType>> {
        if ["fallback/Thing", "distinct_a/Thing", "distinct_b/Thing"]
            .iter()
            .any(|name| internal.matches(name))
        {
            Some(std::sync::Arc::new(import_test_classifier(None)))
        } else if ["alias_a/Thing", "alias_b/Thing"]
            .iter()
            .any(|name| internal.matches(name))
        {
            Some(std::sync::Arc::new(import_test_classifier(Some(
                crate::types::type_name("target/Thing"),
            ))))
        } else {
            None
        }
    }

    impl crate::symbol_source::SymbolSource for ClassifierImportSource {
        fn symbols(
            &self,
            namespace: crate::symbol_source::SymbolNamespace,
            name: &str,
        ) -> std::rc::Rc<crate::libraries::ResolvedSymbols> {
            if namespace
                == crate::symbol_source::SymbolNamespace::Package(crate::types::type_name(
                    "callable",
                ))
                && name == "Thing"
            {
                let callable = crate::libraries::LibraryCallable::library(
                    "callable/ThingKt",
                    "Thing",
                    Vec::new(),
                    Ty::Unit,
                    Ty::Unit,
                    "()V",
                );
                return std::rc::Rc::new(crate::libraries::ResolvedSymbols {
                    builtin_classifier: false,
                    classifier_name: None,
                    classifier_declaration: None,
                    classifier: None,
                    callables: crate::libraries::Callables::Functions(
                        crate::libraries::FunctionSet {
                            overloads: vec![crate::libraries::FunctionInfo::plain(
                                crate::libraries::FnKind::TopLevel,
                                None,
                                callable,
                            )],
                        },
                    ),
                    companion_extensions: crate::libraries::Callables::None,
                    importable_declaration: false,
                });
            }
            // A provider owns the textual index for its declarations. It may intern the identity
            // after a HIT, but a speculative miss must not pollute the global name tree merely so
            // `existing_classifier` can find it on a later probe.
            let internal = match namespace {
                crate::symbol_source::SymbolNamespace::Package(package)
                    if name == "Thing"
                        && ["fallback", "distinct_a", "distinct_b", "alias_a", "alias_b"]
                            .iter()
                            .any(|known| package.matches(known)) =>
                {
                    Some(crate::types::type_name_child(package, name))
                }
                _ => None,
            };
            let classifier = internal.and_then(import_classifier);
            std::rc::Rc::new(crate::libraries::ResolvedSymbols {
                builtin_classifier: false,
                classifier_declaration: classifier
                    .as_ref()
                    .and(internal)
                    .map(crate::libraries::ClassifierDeclaration::Ordinary),
                classifier_name: internal.map(|internal| {
                    classifier
                        .as_ref()
                        .and_then(|classifier| classifier.alias_target)
                        .unwrap_or(internal)
                }),
                classifier,
                callables: crate::libraries::Callables::None,
                companion_extensions: crate::libraries::Callables::None,
                importable_declaration: false,
            })
        }
    }

    #[test]
    fn classifier_import_resolution_ignores_callable_only_higher_precedence_candidates() {
        let resolved = classifier_from_imports(
            "Thing",
            &HashMap::default(),
            &[
                vec![crate::types::type_name("callable")].into(),
                vec![crate::types::type_name("fallback")].into(),
            ],
            &ClassifierImportSource,
        );
        assert_eq!(
            resolved,
            InheritedNestedClassifier::Found(crate::types::type_name("fallback/Thing"))
        );
    }

    #[test]
    fn classifier_import_resolution_collapses_aliases_to_the_same_target() {
        let resolved = classifier_from_imports(
            "Thing",
            &HashMap::default(),
            &[vec![
                crate::types::type_name("alias_a"),
                crate::types::type_name("alias_b"),
            ]
            .into()],
            &ClassifierImportSource,
        );
        assert_eq!(
            resolved,
            InheritedNestedClassifier::Found(crate::types::type_name("target/Thing"))
        );
    }

    #[test]
    fn classifier_import_resolution_rejects_distinct_same_level_candidates() {
        let resolved = classifier_from_imports(
            "Thing",
            &HashMap::default(),
            &[vec![
                crate::types::type_name("distinct_a"),
                crate::types::type_name("distinct_b"),
            ]
            .into()],
            &ClassifierImportSource,
        );
        assert_eq!(resolved, InheritedNestedClassifier::Ambiguous);
    }
}
