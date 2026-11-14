use super::*;
use crate::metadata::id_signature::{constructor_signature, MetadataClass, MetadataContainer};
use crate::metadata::semantic::KotlinConstructor;

fn declaration() -> KlibDependencyCallable {
    let package = ["sample".to_owned()];
    let classes = [MetadataClass {
        name: "AbstractBase",
        type_params: &[],
        expect: false,
    }];
    let constructor = KotlinConstructor {
        is_primary: true,
        params: Vec::new(),
        param_names: Vec::new(),
        param_defaults: Vec::new(),
        vararg: None,
        visibility: crate::types::Visibility::Public,
    };
    KlibDependencyCallable {
        module: "producer".into(),
        declaration: crate::libraries::KlibDeclarationSignature::Public(
            constructor_signature(
                MetadataContainer {
                    package: &package,
                    classes: &classes,
                    native_interop_library: false,
                },
                &constructor,
            )
            .unwrap(),
        ),
    }
}

#[test]
fn initializer_declaration_does_not_promise_an_allocation_entry() {
    let mut exports = DependencyExportSignatures::default();
    let constructor = ExportedConstructor {
        class: 7,
        secondary: None,
    };
    let identity = declaration();
    exports
        .record_constructor_declaration(constructor, identity.clone())
        .unwrap();
    assert_eq!(
        exports.constructor_declarations(),
        &[(constructor, identity)]
    );
    assert!(exports.constructors().is_empty());
    assert!(exports.callables().next().is_none());
}

#[test]
fn allocation_and_initializer_share_one_exact_declaration() {
    let mut exports = DependencyExportSignatures::default();
    let constructor = ExportedConstructor {
        class: 7,
        secondary: None,
    };
    let identity = declaration();
    exports
        .record_constructor_declaration(constructor, identity.clone())
        .unwrap();
    exports.record_constructor(constructor, identity.clone());
    assert_eq!(
        exports.constructor_declarations(),
        &[(constructor, identity.clone())]
    );
    assert_eq!(exports.constructors(), &[(constructor, identity)]);
}

#[test]
fn one_constructor_key_cannot_identify_different_classes_or_ordinals() {
    let primary = ExportedConstructor {
        class: 7,
        secondary: None,
    };
    for conflicting in [
        ExportedConstructor {
            class: 8,
            secondary: None,
        },
        ExportedConstructor {
            class: 7,
            secondary: Some(0),
        },
    ] {
        for (first, second) in [(primary, conflicting), (conflicting, primary)] {
            let mut exports = DependencyExportSignatures::default();
            let identity = declaration();
            exports
                .record_constructor_declaration(first, identity.clone())
                .unwrap();
            assert_eq!(
                exports.record_constructor_declaration(second, identity.clone()),
                Err("one constructor declaration identifies conflicting initializer ordinals")
            );
            exports
                .record_constructor_declaration(first, identity.clone())
                .unwrap();
            assert_eq!(exports.constructor_declarations(), &[(first, identity)]);
            assert!(exports.constructors().is_empty());
        }
    }
}
