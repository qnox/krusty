use super::*;
use crate::types::ClassifierDeclarationName;
use std::path::{Path, PathBuf};

fn bytes_field(field: u8, bytes: &[u8], out: &mut Vec<u8>) {
    assert!(bytes.len() < 128, "small metadata fixture");
    out.extend_from_slice(&[(field << 3) | 2, bytes.len() as u8]);
    out.extend_from_slice(bytes);
}

fn write_builtins(root: &Path, package: &[&str], classes: &[&str], interface: bool) {
    let mut strings = Vec::new();
    let mut names = Vec::new();
    let mut fragment = Vec::new();
    for (index, segment) in package.iter().chain(classes).enumerate() {
        bytes_field(1, segment.as_bytes(), &mut strings);
        let mut name = vec![0x10, index as u8, 0x18, u8::from(index < package.len())];
        if index > 0 {
            name.extend_from_slice(&[0x08, (index - 1) as u8]);
        }
        bytes_field(1, &name, &mut names);
    }
    bytes_field(1, &strings, &mut fragment);
    bytes_field(2, &names, &mut fragment);
    for index in package.len()..package.len() + classes.len() {
        bytes_field(
            4,
            &[0x08, if interface { 70 } else { 6 }, 0x18, index as u8],
            &mut fragment,
        );
    }
    let mut bytes = Vec::new();
    for word in [3u32, 1, 0, 7] {
        bytes.extend_from_slice(&word.to_be_bytes());
    }
    bytes.extend_from_slice(&fragment);
    let directory = package
        .iter()
        .fold(root.to_path_buf(), |path, part| path.join(part));
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join(format!("{}.kotlin_builtins", package.last().unwrap())),
        bytes,
    )
    .unwrap();
}

fn directory() -> PathBuf {
    let directory = std::env::temp_dir().join(format!(
        "krusty-builtin-declaration-names-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    directory
}

fn libraries(root: &Path) -> JvmLibraries {
    JvmLibraries::new(std::rc::Rc::new(Classpath::new(vec![root.to_path_buf()]))).unwrap()
}

#[test]
fn builtin_declaration_names_keep_raw_snapshot_nesting_in_classless_shapes() {
    let top_directory = directory();
    write_builtins(
        &top_directory,
        &["builtin_name_snapshot"],
        &["Literal$Name"],
        false,
    );
    let top = libraries(&top_directory)
        .builtin_library_type(type_name("builtin_name_snapshot/Literal$Name"))
        .expect("decoded top-level builtin");
    let nested_directory = directory();
    write_builtins(
        &nested_directory,
        &["builtin_name_snapshot"],
        &["Literal", "Name"],
        false,
    );
    let nested = libraries(&nested_directory)
        .builtin_library_type(type_name("builtin_name_snapshot/Literal.Name"))
        .expect("decoded nested builtin");
    assert_eq!(
        top.declaration_name,
        Some(ClassifierDeclarationName::TopLevel {
            package: type_name("builtin_name_snapshot"),
            name: "Literal$Name".into(),
        })
    );
    assert_eq!(
        nested.declaration_name,
        Some(ClassifierDeclarationName::Nested {
            owner: type_name("builtin_name_snapshot/Literal"),
            name: "Name".into(),
        })
    );
    std::fs::remove_dir_all(top_directory).unwrap();
    std::fs::remove_dir_all(nested_directory).unwrap();
}

#[test]
fn builtin_declaration_names_preserve_semantic_and_physical_classifier_authority() {
    let directory = directory();
    write_builtins(&directory, &["kotlin", "collections"], &["List"], true);
    let mut writer = crate::jvm::classfile::ClassWriter::new("java/util/List", "java/lang/Object");
    writer.set_access(
        crate::jvm::classfile::ACC_PUBLIC
            | crate::jvm::classfile::ACC_INTERFACE
            | crate::jvm::classfile::ACC_ABSTRACT,
    );
    let path = directory.join("java/util/List.class");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, writer.finish()).unwrap();
    let libraries = libraries(&directory);
    for (identity, package, name) in [
        ("kotlin/collections/List", "kotlin/collections", "List"),
        ("java/util/List", "java/util", "List"),
    ] {
        assert_eq!(
            libraries
                .classifier(type_name(identity))
                .unwrap()
                .declaration_name,
            Some(ClassifierDeclarationName::TopLevel {
                package: type_name(package),
                name: name.into(),
            }),
            "{identity} retains its declaring classifier"
        );
    }
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn builtin_declaration_names_cover_customizer_owned_array_and_cloneable_factories() {
    let customizer = builtins_customizer::JvmBuiltInsCustomizer;
    for name in ["Cloneable", "Array", "IntArray", "UIntArray"] {
        let identity = crate::types::type_name_child(type_name("kotlin"), name);
        assert_eq!(
            customizer
                .customize(identity, None)
                .unwrap()
                .declaration_name,
            Some(ClassifierDeclarationName::TopLevel {
                package: type_name("kotlin"),
                name: name.into(),
            })
        );
    }
    assert!(customizer
        .customize(type_name("other/IntArray"), None)
        .is_none());
    assert_eq!(
        crate::types::array_declaration_name(type_name("kotlin/Int$Array")),
        None
    );
    let mut existing = LibraryType::declaration_header();
    existing.declaration_name = Some(ClassifierDeclarationName::TopLevel {
        package: type_name("array_seed"),
        name: "Declared".into(),
    });
    let expected = existing.declaration_name.clone();
    assert_eq!(
        customizer
            .customize(type_name("kotlin/IntArray"), Some(existing))
            .unwrap()
            .declaration_name,
        expected
    );
    assert_eq!(
        customizer
            .customize(
                type_name("kotlin/IntArray"),
                Some(LibraryType::declaration_header())
            )
            .unwrap()
            .declaration_name,
        None
    );
    assert_eq!(
        mapped_builtin_signature(type_name("kotlin/String"))
            .unwrap()
            .declaration_name,
        Some(ClassifierDeclarationName::TopLevel {
            package: type_name("kotlin"),
            name: "String".into(),
        })
    );
}

#[test]
fn builtin_declaration_names_do_not_replace_missing_exact_kotlin_metadata_names() {
    let directory = directory();
    write_builtins(&directory, &["kotlin"], &["Unit"], false);
    let mut writer = crate::jvm::classfile::ClassWriter::new("kotlin/Unit", "java/lang/Object");
    writer.set_kotlin_metadata(
        1,
        crate::jvm::classfile::MetadataStamp {
            version: [2, 2, 0],
            pre_release: false,
        },
        48,
        &[],
        &[],
    );
    std::fs::write(directory.join("kotlin/Unit.class"), writer.finish()).unwrap();
    let classpath = std::rc::Rc::new(Classpath::new(vec![directory.clone()]));
    let class = classpath.find_name(type_name("kotlin/Unit")).unwrap();
    assert!(class.meta.class_visibility.is_some());
    assert_eq!(class.meta.class_declaration_name, None);
    let libraries = JvmLibraries::new(classpath).unwrap();
    assert_eq!(
        libraries
            .classifier(type_name("kotlin/Unit"))
            .unwrap()
            .declaration_name,
        None
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn builtin_declaration_names_retain_selected_stdlib_nested_and_companion_owners() {
    let stdlib = crate::toolchain::stdlib_jar().expect("selected reference stdlib");
    let libraries = JvmLibraries::new(std::rc::Rc::new(Classpath::new(vec![stdlib]))).unwrap();
    for (identity, owner, name) in [
        (
            "kotlin/collections/Map.Entry",
            "kotlin/collections/Map",
            "Entry",
        ),
        ("kotlin/Int.Companion", "kotlin/Int", "Companion"),
    ] {
        assert_eq!(
            libraries
                .classifier(type_name(identity))
                .unwrap()
                .declaration_name,
            Some(ClassifierDeclarationName::Nested {
                owner: type_name(owner),
                name: name.into(),
            }),
            "{identity} owns its Kotlin declaration"
        );
    }
}
