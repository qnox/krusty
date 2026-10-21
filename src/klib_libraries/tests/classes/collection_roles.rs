//! Collection roles are authenticated declaration facts, not consequences of a familiar name.

use super::*;
use crate::types::{CollectionKind, MappedCollection};

const FACES: &[(&str, CollectionKind, bool, usize)] = &[
    ("Iterable", CollectionKind::Iterable, false, 1),
    ("MutableIterable", CollectionKind::Iterable, true, 1),
    ("Iterator", CollectionKind::Iterator, false, 1),
    ("MutableIterator", CollectionKind::Iterator, true, 1),
    ("Collection", CollectionKind::Collection, false, 1),
    ("MutableCollection", CollectionKind::Collection, true, 1),
    ("List", CollectionKind::List, false, 1),
    ("MutableList", CollectionKind::List, true, 1),
    ("ListIterator", CollectionKind::ListIterator, false, 1),
    ("MutableListIterator", CollectionKind::ListIterator, true, 1),
    ("Set", CollectionKind::Set, false, 1),
    ("MutableSet", CollectionKind::Set, true, 1),
    ("Map", CollectionKind::Map, false, 2),
    ("MutableMap", CollectionKind::Map, true, 2),
    ("Map.Entry", CollectionKind::MapEntry, false, 2),
    ("MutableMap.MutableEntry", CollectionKind::MapEntry, true, 2),
];

fn collection_class(arity: usize) -> KotlinClass {
    let mut declaration = in_memory_class(0);
    declaration.constructors.clear();
    declaration.functions.clear();
    declaration.kind = TypeKind::Interface;
    declaration.modality = KotlinModality::Abstract;
    declaration.type_params = (0..arity)
        .map(|ordinal| KotlinTypeParameter {
            id: KotlinTypeParameterId(ordinal as u64),
            name: format!("T{ordinal}"),
            bounds: vec![class("kotlin/Any", true)],
            variance: TypeVariance::Invariant,
            only_input: false,
            reified: false,
        })
        .collect();
    declaration
}

fn publish(
    package: &[&str],
    declarations: Vec<(String, KotlinClass)>,
    authenticated: bool,
) -> Result<KlibLibraries, KlibLibraryError> {
    let declarations = KotlinPackage {
        classes: declarations.into_iter().collect(),
        ..KotlinPackage::default()
    };
    let inventory = inventory::PackageInventory::from_packages(vec![(
        segments(package),
        declarations,
        authenticated,
    )]);
    inventory.map(|inventory| KlibLibraries::over(Rc::new(inventory)))
}

#[test]
fn authenticated_collection_interfaces_publish_every_face() {
    let declarations = FACES
        .iter()
        .map(|(local, _, _, arity)| {
            let mut declaration = collection_class(*arity);
            declaration.is_nested = local.contains('.');
            (format!("kotlin/collections/{local}"), declaration)
        })
        .collect();
    let libraries = publish(&["kotlin", "collections"], declarations, true)
        .expect("the authenticated canonical interfaces are consistent");
    for (local, kind, mutable, _) in FACES {
        let declaration = libraries
            .classifier(type_name(&format!("kotlin/collections/{local}")))
            .expect("the canonical interface is published");
        assert_eq!(
            declaration.mapped_collection,
            Some(MappedCollection {
                kind: *kind,
                mutable: *mutable,
            }),
            "{local}"
        );
    }
}

#[test]
fn untrusted_same_named_collection_interface_has_no_role() {
    let libraries = publish(
        &["kotlin", "collections"],
        vec![("kotlin/collections/Iterable".into(), collection_class(1))],
        false,
    )
    .expect("an ordinary dependency can declare this interface");
    assert_eq!(
        libraries
            .classifier(type_name("kotlin/collections/Iterable"))
            .expect("the dependency interface is published")
            .mapped_collection,
        None
    );
}

#[test]
fn authenticated_unrelated_collection_spelling_has_no_role() {
    let libraries = publish(
        &["sample"],
        vec![("sample/Iterable".into(), collection_class(1))],
        true,
    )
    .expect("unrelated authenticated declarations stay ordinary interfaces");
    assert_eq!(
        libraries
            .classifier(type_name("sample/Iterable"))
            .expect("the unrelated interface is published")
            .mapped_collection,
        None
    );
}

#[test]
fn malformed_authenticated_collection_interface_is_rejected() {
    for (kind, arity) in [(TypeKind::Class, 1), (TypeKind::Interface, 0)] {
        let mut declaration = collection_class(arity);
        declaration.kind = kind;
        let error = publish(
            &["kotlin", "collections"],
            vec![("kotlin/collections/Iterable".into(), declaration)],
            true,
        )
        .err()
        .expect("an inconsistent canonical collection cannot be published");
        assert_eq!(
            error.to_string(),
            format!(
                "cannot sign KLIB declaration Iterable in package `kotlin.collections`: \
                 authenticated collection kotlin/collections/Iterable requires an interface \
                 with 1 own and total type parameters and no outer instance; \
                 found {kind:?} with {arity} own and {arity} total type parameters and outer instance false"
            )
        );
    }
}
