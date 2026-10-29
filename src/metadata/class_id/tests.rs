use super::*;
use crate::metadata::type_encoder::StringTable;
use crate::types::type_name;

#[test]
fn resolved_headers_keep_alias_and_classifier_ownership_separate() {
    use ClassifierDeclarationKey::{Ordinary, TypeAlias};
    let owner = type_name("header_names/Owner$Literal");
    let child = owner.nested_child("Child$Name");
    let lookup = |key| match key {
        Ordinary(identity) if identity == owner => {
            Some(Some(ClassifierDeclarationName::TopLevel {
                package: type_name("header_names"),
                name: "Owner$Literal".into(),
            }))
        }
        Ordinary(identity) if identity == child => Some(Some(ClassifierDeclarationName::Nested {
            owner,
            name: "Child$Name".into(),
        })),
        TypeAlias(identity) if identity == child => {
            Some(Some(ClassifierDeclarationName::TopLevel {
                package: type_name("header_names"),
                name: "Owner$Literal$Child$Name".into(),
            }))
        }
        _ => None,
    };
    for (key, expected) in [
        (Ordinary(child), "header_names/Owner$Literal.Child$Name"),
        (TypeAlias(child), "header_names/Owner$Literal$Child$Name"),
    ] {
        let name = ResolvedClassId::resolve(key, &lookup).unwrap();
        let mut strings = StringTable::default();
        assert_eq!(strings.declaration_class_id(&name), Ok(0));
        assert_eq!(strings.serialize_types().as_bytes(), &[10, 0]);
        assert_eq!(strings.into_strings(), [expected]);
    }
}

#[test]
fn resolved_headers_use_predefined_and_descriptor_records_only_for_exact_names() {
    let identity = type_name("kotlin/collections/Map$Entry");
    for (segments, expected_record, expected_string) in [
        (&["Map", "Entry"][..], &[10, 2, 16, 38][..], ""),
        (
            &["Map$Entry"][..],
            &[10, 0][..],
            "kotlin/collections/Map$Entry",
        ),
    ] {
        let name = ResolvedClassId::declared(identity, type_name("kotlin/collections"), segments);
        let mut strings = StringTable::default();
        assert_eq!(strings.declaration_class_id(&name), Ok(0));
        assert_eq!(strings.serialize_types().as_bytes(), expected_record);
        assert_eq!(strings.into_strings(), [expected_string]);
    }
    let nested = ResolvedClassId::declared(
        type_name("header_names/Outer$Inner"),
        type_name("header_names"),
        &["Outer", "Inner"],
    );
    let mut strings = StringTable::default();
    assert_eq!(strings.declaration_class_id(&nested), Ok(0));
    assert_eq!(strings.serialize_types().as_bytes(), &[10, 2, 24, 2]);
    assert_eq!(strings.into_strings(), ["Lheader_names/Outer$Inner;"]);
}

#[test]
fn resolved_headers_reject_missing_unknown_and_cyclic_owner_evidence() {
    use ClassifierDeclarationKey::Ordinary;
    let identity = type_name("header_names/Missing");
    assert_eq!(
        ResolvedClassId::resolve(Ordinary(identity), &|_| None),
        Err(ClassIdError::MissingDeclaration(Ordinary(identity)))
    );
    assert_eq!(
        ResolvedClassId::resolve(Ordinary(identity), &|_| Some(None)),
        Err(ClassIdError::UnknownName(Ordinary(identity)))
    );
    let owner = type_name("header_names/Owner");
    let cycle = |key| {
        Some(Some(ClassifierDeclarationName::Nested {
            owner: if key == Ordinary(identity) {
                owner
            } else {
                identity
            },
            name: "Part".into(),
        }))
    };
    assert_eq!(
        ResolvedClassId::resolve(Ordinary(identity), &cycle),
        Err(ClassIdError::CyclicOwner(Ordinary(identity)))
    );
}

#[test]
fn resolved_local_headers_preserve_raw_jvm_names_and_decline_klib_cleanly() {
    let identity = type_name("header_names/AKt$run$Local");
    let local = ResolvedClassId::local(identity);
    let mut strings = StringTable::with_local_classifiers(
        &std::iter::once(identity).collect(),
        &Default::default(),
        None,
        crate::metadata::MetadataTypeLookups::default().classifier_applications,
    );
    assert_eq!(strings.declaration_class_id(&local), Ok(0));
    assert_eq!(strings.serialize_types().as_bytes(), &[10, 0, 42, 1, 0]);
    assert_eq!(strings.into_strings(), ["header_names/AKt$run$Local"]);
    let mut klib =
        StringTable::klib(crate::metadata::MetadataTypeLookups::default().classifier_applications);
    assert_eq!(
        klib.declaration_class_id(&local),
        Err(ClassIdError::LocalInKlib(identity))
    );
}

#[test]
fn resolved_klib_headers_and_class_inventory_share_the_same_qualified_name() {
    use crate::metadata::class_builder::{ClassDeclaration, ClassTail};
    use crate::metadata::klib_fragment::{KlibClass, KlibFileMembers};
    let identity = type_name("header_names/Owner$Literal$Child$Name");
    let name = ResolvedClassId::declared(
        identity,
        type_name("header_names"),
        &["Owner$Literal", "Child$Name"],
    );
    let tail = ClassTail::default();
    let classes = [KlibClass {
        declaration: ClassDeclaration {
            name: identity,
            class_id: &name,
            ctor_params: &[],
            props: &[],
            methods: &[],
            enum_entries: &[],
            tail: &tail,
        },
        constants: &[],
        in_source_file: true,
    }];
    let file = KlibFileMembers {
        file_name: "names.kt".into(),
        functions: Vec::new(),
        properties: Vec::new(),
        constants: Vec::new(),
        aliases: Vec::new(),
    };
    let bytes =
        crate::metadata::klib_fragment::package_fragment(&["header_names"], &file, &classes, true);
    let decoded = crate::metadata::semantic::parse_package_fragment_checked(&bytes).unwrap();
    assert_eq!(decoded.classes.len(), 1);
    assert_eq!(
        decoded
            .classes
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["header_names/Owner$Literal.Child$Name"]
    );
    let mut fragment = crate::metadata::decode::Pb::new(&bytes);
    let mut class_id = None;
    let mut inventory = None;
    while !fragment.at_end() {
        let tag = fragment.varint().unwrap();
        if tag >> 3 == 4 || tag >> 3 == 174 {
            let length = fragment.varint().unwrap() as usize;
            let body = fragment.bytes(length).unwrap();
            if tag >> 3 == 174 {
                inventory = Some(crate::metadata::decode::packed_varints(body).unwrap());
            } else {
                let mut class = crate::metadata::decode::Pb::new(body);
                while !class.at_end() {
                    let tag = class.varint().unwrap();
                    if tag >> 3 == 3 {
                        class_id = class.varint();
                    } else {
                        class.skip(tag & 7).unwrap();
                    }
                }
            }
        } else {
            fragment.skip(tag & 7).unwrap();
        }
    }
    assert_eq!(inventory, Some(vec![class_id.expect("Class.fq_name")]));
}
