use super::*;
use crate::libraries::ClassifierDeclarationKey as Key;
use crate::types::{type_name, ClassifierDeclarationName};
use std::cell::Cell;

struct NameRecord {
    identity: TypeName,
    name: Option<ClassifierDeclarationName>,
    probes: Cell<usize>,
}

impl SymbolSource for NameRecord {
    fn classifier_declaration_name(
        &self,
        declaration: Key,
    ) -> Option<Option<ClassifierDeclarationName>> {
        self.probes.set(self.probes.get() + 1);
        (declaration == Key::Ordinary(self.identity)).then(|| self.name.clone())
    }

    fn symbols(&self, _: SymbolNamespace, _: &str) -> std::rc::Rc<ResolvedSymbols> {
        panic!("a bound declaration-name query must not reopen source-name lookup")
    }
}

fn record(name: Option<ClassifierDeclarationName>) -> NameRecord {
    NameRecord {
        identity: type_name("name_query/Literal$Name"),
        name,
        probes: Cell::new(0),
    }
}

fn top_name() -> ClassifierDeclarationName {
    ClassifierDeclarationName::TopLevel {
        package: type_name("name_query"),
        name: "Literal$Name".into(),
    }
}

#[test]
fn declaration_name_query_distinguishes_alias_and_class_with_equal_flattened_identity() {
    let source =
        "package name_query\nclass Owner { class Member }\ntypealias `Owner$Member` = String\n";
    let mut diagnostics = crate::diag::DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &[crate::source::SourceInput::kotlin(source)],
        crate::frontend::PlatformProvider::jvm(Box::new(crate::libraries::EmptySymbolSource)),
        &crate::features::LangFeatures::new(),
        &mut diagnostics,
    );
    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis.streamed.as_ref().unwrap().module.index();
    let provider = crate::fir::StreamedModuleSymbols::for_file(index, 0);
    let identity = type_name("name_query/Owner$Member");
    let alias = index.type_alias_by_identity(identity).unwrap();
    let class = index
        .classifier_header(index.classifier_declaration(identity).unwrap())
        .unwrap();
    assert_eq!(alias.identity, class.classifier);
    let nested = ClassifierDeclarationName::Nested {
        owner: type_name("name_query/Owner"),
        name: "Member".into(),
    };
    let top = ClassifierDeclarationName::TopLevel {
        package: type_name("name_query"),
        name: "Owner$Member".into(),
    };
    assert_eq!(class.declaration_name, Some(nested.clone()));
    assert_eq!(alias.declaration_name, Some(top.clone()));
    let cache = SymbolQueryCache::default();
    let cached = CachedCompositeSource::new(vec![&provider], &cache);
    let module = crate::backend::BackendModuleFacts::from_resolved_index(index).unwrap();
    let dependencies = record(None);
    let checked = crate::backend::CheckedBackendClassifiers::new(&module, &dependencies);
    for (key, expected) in [
        (Key::Ordinary(identity), nested),
        (Key::TypeAlias(identity), top),
    ] {
        for _ in 0..2 {
            assert_eq!(
                provider.classifier_declaration_name(key),
                Some(Some(expected.clone()))
            );
            assert_eq!(
                cached.classifier_declaration_name(key),
                Some(Some(expected.clone()))
            );
            assert_eq!(
                crate::backend::BackendClassifierSource::classifier_declaration_name(&checked, key),
                Some(Some(expected.clone()))
            );
        }
    }
    assert_eq!(dependencies.probes.get(), 0);
}

#[test]
fn declaration_name_query_preserves_selected_unknown_before_a_known_later_record() {
    let selected = record(None);
    let later = record(Some(top_name()));
    let source = CompositeSource::new(vec![&selected, &later]);
    assert_eq!(
        source.classifier_declaration_name(Key::Ordinary(selected.identity)),
        Some(None)
    );
    assert_eq!(selected.probes.get(), 1);
    assert_eq!(later.probes.get(), 0);
}

#[test]
fn declaration_name_query_caches_absent_unknown_and_known_records_per_snapshot() {
    for name in [None, Some(top_name())] {
        let selected = record(name.clone());
        let cache = SymbolQueryCache::default();
        let source = CachedCompositeSource::new(vec![&selected], &cache);
        let absent = type_name("name_query/Absent");
        for _ in 0..2 {
            assert_eq!(
                source.classifier_declaration_name(Key::Ordinary(selected.identity)),
                Some(name.clone())
            );
            assert_eq!(
                source.classifier_declaration_name(Key::Ordinary(absent)),
                None
            );
        }
        assert_eq!(selected.probes.get(), 2);
    }
    let nested = ClassifierDeclarationName::Nested {
        owner: type_name("name_query/Literal"),
        name: "Name".into(),
    };
    let top = record(Some(top_name()));
    let child = record(Some(nested.clone()));
    let top_cache = SymbolQueryCache::default();
    let child_cache = SymbolQueryCache::default();
    let top_source = CachedCompositeSource::new(vec![&top], &top_cache);
    let child_source = CachedCompositeSource::new(vec![&child], &child_cache);
    assert_eq!(
        top_source.classifier_declaration_name(Key::Ordinary(top.identity)),
        Some(Some(top_name()))
    );
    assert_eq!(
        child_source.classifier_declaration_name(Key::Ordinary(child.identity)),
        Some(Some(nested))
    );
}

#[test]
fn declaration_name_query_reads_source_and_alias_own_headers_by_identity() {
    let source = "package name_query\nclass `Literal$Name`\nclass `Owner$Literal` { class `Child$Name` }\ntypealias `Alias$Own` = `Literal$Name`\n";
    let mut diagnostics = crate::diag::DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &[crate::source::SourceInput::kotlin(source)],
        crate::frontend::PlatformProvider::jvm(Box::new(crate::libraries::EmptySymbolSource)),
        &crate::features::LangFeatures::new(),
        &mut diagnostics,
    );
    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis.streamed.as_ref().unwrap().module.index();
    let provider = crate::fir::StreamedModuleSymbols::for_file(index, 0);
    let module = crate::backend::BackendModuleFacts::from_resolved_index(index).unwrap();
    let later = record(None);
    let checked = crate::backend::CheckedBackendClassifiers::new(&module, &later);
    for (declaration, expected) in [
        (
            Key::Ordinary(type_name("name_query/Literal$Name")),
            top_name(),
        ),
        (
            Key::Ordinary(type_name("name_query/Owner$Literal$Child$Name")),
            ClassifierDeclarationName::Nested {
                owner: type_name("name_query/Owner$Literal"),
                name: "Child$Name".into(),
            },
        ),
        (
            Key::TypeAlias(type_name("name_query/Alias$Own")),
            ClassifierDeclarationName::TopLevel {
                package: type_name("name_query"),
                name: "Alias$Own".into(),
            },
        ),
    ] {
        assert_eq!(
            provider.classifier_declaration_name(declaration),
            Some(Some(expected))
        );
        assert_eq!(
            crate::backend::BackendClassifierSource::classifier_declaration_name(
                &checked,
                declaration
            ),
            provider.classifier_declaration_name(declaration)
        );
    }
    assert_eq!(later.probes.get(), 0);
    assert_eq!(
        provider.classifier_declaration_name(Key::Ordinary(type_name("name_query/Absent"))),
        None
    );
}

#[test]
fn declaration_name_query_frozen_unknown_source_blocks_known_dependency() {
    use crate::backend::{BackendClassifierSource, BackendModuleFacts, CheckedBackendClassifiers};
    let dependency = record(Some(top_name()));
    let module = BackendModuleFacts::from_classifiers(
        [(
            dependency.identity,
            crate::libraries::LibraryType::declaration_header(),
        )],
        [],
    )
    .unwrap();
    let checked = CheckedBackendClassifiers::new(&module, &dependency);
    assert_eq!(
        checked.classifier_declaration_name(Key::Ordinary(dependency.identity)),
        Some(None)
    );
    assert_eq!(dependency.probes.get(), 0);
    assert_eq!(
        checked.classifier_declaration_name(Key::Ordinary(type_name("name_query/Absent"))),
        None
    );
    assert_eq!(dependency.probes.get(), 1);
}
