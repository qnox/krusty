use super::*;
use crate::libraries::ClassifierDeclarationKey as Key;
use crate::types::ClassifierDeclarationName;

#[test]
fn declaration_name_query_jvm_keeps_own_alias_and_optional_annotation_records() {
    let stdlib = crate::toolchain::stdlib_jar().expect("selected reference stdlib");
    let libraries = JvmLibraries::new(std::rc::Rc::new(Classpath::new(vec![stdlib]))).unwrap();
    assert_eq!(
        libraries
            .classifier_declaration_name(Key::TypeAlias(type_name("kotlin/collections/ArrayList"))),
        Some(Some(ClassifierDeclarationName::TopLevel {
            package: type_name("kotlin/collections"),
            name: "ArrayList".into(),
        }))
    );
    let optional = libraries.symbols(SymbolNamespace::Package(type_name("kotlin/js")), "JsStatic");
    let selected = optional
        .classifier
        .as_ref()
        .expect("selected optional annotation");
    assert_eq!(
        libraries.classifier_declaration_name(Key::Ordinary(type_name("kotlin/js/JsStatic"))),
        Some(selected.declaration_name.clone())
    );
    assert_eq!(
        libraries.classifier_declaration_name(Key::Ordinary(type_name("name_query/Absent"))),
        None
    );
}

#[test]
fn declaration_name_query_jvm_preserves_snapshot_ownership_and_unknown_metadata() {
    use crate::jvm::classfile::{ClassWriter, MetadataStamp};
    let directory = std::env::temp_dir().join(format!(
        "krusty-name-records-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let identity = type_name("name_query/Literal$Name");
    let stdlib = crate::toolchain::stdlib_jar().expect("selected reference stdlib");
    let mut snapshots = Vec::new();
    for (index, metadata_name) in [
        Some("name_query/Literal$Name"),
        Some("name_query/Literal.Name"),
        None,
    ]
    .into_iter()
    .enumerate()
    {
        let root = directory.join(index.to_string());
        let path = root.join("name_query/Literal$Name.class");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut writer = ClassWriter::new("name_query/Literal$Name", "java/lang/Object");
        let mut message = crate::metadata::protobuf::Pb::new();
        message.field_varint(1, 6);
        if metadata_name.is_some() {
            message.field_varint(3, 0);
        }
        let mut payload = vec![0, 0];
        payload.extend_from_slice(message.as_bytes());
        writer.set_kotlin_metadata(
            1,
            MetadataStamp {
                version: [2, 4, 0],
                pre_release: false,
            },
            0,
            &[payload.into_iter().map(char::from).collect()],
            &metadata_name
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>(),
        );
        std::fs::write(path, writer.finish()).unwrap();
        if index == 1 {
            let alias = crate::metadata::builder::TypeAliasMeta {
                name: "Literal$Name".into(),
                formals: Vec::new(),
                expansion: Ty::String,
                visibility: crate::types::Visibility::Public,
                expansion_spelling: Default::default(),
                decl_order: 0,
            };
            let (d1, d2) =
                crate::metadata::builder::build_package(&[], &[], &[alias], (None, &[]), true);
            let mut facade = ClassWriter::new("name_query/AliasesKt", "java/lang/Object");
            facade.set_kotlin_metadata(
                2,
                MetadataStamp {
                    version: [2, 4, 0],
                    pre_release: false,
                },
                48,
                &crate::metadata::encoding::bytes_to_strings(&d1),
                &d2,
            );
            std::fs::write(root.join("name_query/AliasesKt.class"), facade.finish()).unwrap();
        }
        snapshots.push(
            JvmLibraries::new(std::rc::Rc::new(Classpath::new(vec![root, stdlib.clone()])))
                .unwrap(),
        );
    }
    for _ in 0..2 {
        assert_eq!(
            snapshots[0].classifier_declaration_name(Key::TypeAlias(identity)),
            None
        );
        assert_eq!(
            snapshots[1].classifier_declaration_name(Key::TypeAlias(identity)),
            Some(Some(ClassifierDeclarationName::TopLevel {
                package: type_name("name_query"),
                name: "Literal$Name".into(),
            }))
        );
        assert_eq!(
            snapshots[2].classifier_declaration_name(Key::TypeAlias(identity)),
            None
        );
        assert_eq!(
            snapshots[0].classifier_declaration_name(Key::Ordinary(identity)),
            Some(Some(ClassifierDeclarationName::TopLevel {
                package: type_name("name_query"),
                name: "Literal$Name".into(),
            }))
        );
        assert_eq!(
            snapshots[1].classifier_declaration_name(Key::Ordinary(identity)),
            Some(Some(ClassifierDeclarationName::Nested {
                owner: type_name("name_query/Literal"),
                name: "Name".into(),
            }))
        );
        assert_eq!(
            snapshots[2].classifier_declaration_name(Key::Ordinary(identity)),
            Some(None)
        );
    }
    std::fs::remove_dir_all(directory).unwrap();
}
