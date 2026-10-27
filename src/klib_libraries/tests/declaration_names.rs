use super::*;
use crate::libraries::ClassifierDeclarationKey as Key;

#[test]
fn declaration_name_query_klib_language_classifiers_use_the_admitted_identity() {
    let libraries = KlibLibraries::from_packages(Vec::new()).unwrap();
    for (package, name) in [
        ("kotlin", "Function0"),
        ("kotlin/coroutines", "SuspendFunction2"),
        ("kotlin/reflect", "KFunction3"),
    ] {
        let record = libraries.symbols(SymbolNamespace::Package(type_name(package)), name);
        let identity = record
            .classifier_name
            .expect("language function classifier");
        let selected = record.classifier.as_ref().unwrap();
        assert_eq!(
            libraries.classifier_declaration_name(Key::Ordinary(identity)),
            Some(selected.declaration_name.clone())
        );
        assert_eq!(
            libraries.classifier_declaration_name(Key::TypeAlias(identity)),
            None
        );
    }
    assert_eq!(
        libraries.classifier_declaration_name(Key::Ordinary(type_name("kotlin/Function000"))),
        None
    );
    assert_eq!(
        libraries.classifier_declaration_name(Key::Ordinary(type_name("other/Function0"))),
        None
    );
}

#[test]
fn declaration_name_query_klib_keeps_alias_without_a_target_shape() {
    let fragment = crate::metadata::klib_fragment::package_fragment(
        &["name_query"],
        &crate::metadata::klib_fragment::KlibFileMembers {
            file_name: "aliases.kt".into(),
            functions: Vec::new(),
            properties: Vec::new(),
            constants: Vec::new(),
            aliases: vec![crate::metadata::builder::TypeAliasMeta {
                name: "Alias$Own".into(),
                formals: Vec::new(),
                expansion: Ty::String,
                visibility: Visibility::Public,
                expansion_spelling: Default::default(),
                decl_order: 0,
            }],
        },
        &[],
        true,
    );
    let package = crate::metadata::semantic::parse_package_fragment_checked(&fragment).unwrap();
    let libraries =
        KlibLibraries::from_packages(vec![(segments(&["name_query"]), package)]).unwrap();
    let identity = type_name("name_query/Alias$Own");
    let record = libraries.symbols(
        SymbolNamespace::Package(type_name("name_query")),
        "Alias$Own",
    );
    assert_eq!(record.classifier_name, Some(type_name("kotlin/String")));
    assert!(record.classifier.is_none());
    let Some(crate::libraries::ClassifierDeclaration::TypeAlias(alias)) =
        &record.classifier_declaration
    else {
        panic!("selected alias record");
    };
    let expected = Some(Some(crate::types::ClassifierDeclarationName::TopLevel {
        package: type_name("name_query"),
        name: "Alias$Own".into(),
    }));
    assert_eq!(
        libraries.classifier_declaration_name(Key::Ordinary(identity)),
        None
    );
    assert_eq!(
        libraries.classifier_declaration_name(Key::TypeAlias(identity)),
        expected
    );
    assert_eq!(
        libraries.classifier_declaration_name(Key::TypeAlias(identity)),
        Some(alias.declaration_name.as_deref().cloned())
    );
    assert_eq!(
        libraries.classifier_declaration_name(Key::Ordinary(type_name("name_query/Absent"))),
        None
    );
}
