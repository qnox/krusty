//! Accessor declaration modifiers survive the metadata/provider boundary independently.

use super::super::KlibLibraries;
use crate::libraries::InlineKind;
use crate::metadata::protobuf::Pb;
use crate::metadata::semantic::{parse_package_fragment_checked, KotlinModality};
use crate::symbol_source::{SymbolNamespace, SymbolSource};
use crate::types::{type_name, Visibility};

const PROPERTY_FLAGS: u64 = 6 | (1 << 8) | (1 << 9) | (1 << 10);

fn property_fragment(
    flags: u64,
    getter: Option<u64>,
    setter: Option<u64>,
    annotation_fields: &[u32],
) -> Vec<u8> {
    let mut strings = Pb::new();
    for name in ["value", "kotlin", "Int", "internal", "InlineOnly"] {
        strings.field_bytes(1, name.as_bytes());
    }
    let mut kotlin = Pb::new();
    kotlin.field_varint(2, 1);
    kotlin.field_varint(3, 1);
    let mut int = Pb::new();
    int.field_varint(1, 0);
    int.field_varint(2, 2);
    int.field_varint(3, 0);
    let mut names = Pb::new();
    names.field_message(1, &kotlin);
    names.field_message(1, &int);
    let mut internal = Pb::new();
    internal.field_varint(1, 0);
    internal.field_varint(2, 3);
    internal.field_varint(3, 1);
    names.field_message(1, &internal);
    let mut inline_only = Pb::new();
    inline_only.field_varint(1, 2);
    inline_only.field_varint(2, 4);
    inline_only.field_varint(3, 0);
    names.field_message(1, &inline_only);
    let mut ty = Pb::new();
    ty.field_varint(6, 1);
    let mut property = Pb::new();
    property.field_varint(2, 0);
    property.field_message(3, &ty);
    // Public/final var, with getter and setter: visibility=3; property bits 8, 9 and 10.
    property.field_varint(11, flags);
    if let Some(flags) = getter {
        property.field_varint(7, flags);
    }
    if let Some(flags) = setter {
        property.field_varint(8, flags);
    }
    let mut annotation = Pb::new();
    annotation.field_varint(1, 3);
    for field in annotation_fields {
        property.field_message(*field, &annotation);
    }
    let mut package = Pb::new();
    package.field_message(4, &property);
    let mut fragment = Pb::new();
    fragment.field_message(1, &strings);
    fragment.field_message(2, &names);
    fragment.field_message(3, &package);
    fragment.into_bytes()
}

#[test]
fn inline_getter_and_setter_modifiers_are_published_independently() {
    // Accessor bits: not-default=6, inline=8, after the shared declaration prefix.
    let inline = 6 | (1 << 6) | (1 << 8);
    for (getter, setter, expected_getter, expected_setter) in [
        (
            Some(inline),
            Some(6),
            InlineKind::CanInline,
            InlineKind::None,
        ),
        (
            Some(6),
            Some(inline),
            InlineKind::None,
            InlineKind::CanInline,
        ),
        (None, None, InlineKind::None, InlineKind::None),
    ] {
        let declarations =
            parse_package_fragment_checked(&property_fragment(PROPERTY_FLAGS, getter, setter, &[]))
                .expect("valid property metadata");
        let libraries = KlibLibraries::from_packages(vec![(vec!["p".to_owned()], declarations)])
            .expect("a property with declaration-owned accessors is signable");
        let symbols = libraries.symbols(SymbolNamespace::Package(type_name("p")), "value");
        let [property] = symbols.callables.properties() else {
            panic!("one declared property must be published");
        };
        assert_eq!(property.getter.inline, expected_getter);
        assert_eq!(
            property
                .setter
                .as_ref()
                .expect("a var has its setter")
                .inline,
            expected_setter
        );
    }
}

#[test]
fn absent_accessor_words_inherit_only_the_shared_prefix_and_zero_is_explicit() {
    let flags = PROPERTY_FLAGS | (1 << 4) | (1 << 14);
    let decoded = parse_package_fragment_checked(&property_fragment(flags, Some(0), None, &[]))
        .expect("explicit zero is a complete accessor word");
    let [property] = decoded.properties.as_slice() else {
        panic!("one property")
    };
    assert_eq!(
        (property.visibility, property.modality),
        (Visibility::Public, KotlinModality::Open)
    );
    assert!(property.is_external);
    assert_eq!(
        (
            property.getter_modifiers.visibility,
            property.getter_modifiers.modality
        ),
        (Visibility::Internal, KotlinModality::Final)
    );
    assert_eq!(
        (
            property.setter_modifiers.visibility,
            property.setter_modifiers.modality
        ),
        (Visibility::Public, KotlinModality::Open)
    );
    for accessor in [&property.getter_modifiers, &property.setter_modifiers] {
        assert!(accessor.is_default);
        assert!(!accessor.is_inline);
        assert!(
            !accessor.is_external,
            "property external does not become accessor external"
        );
    }
}

#[test]
fn accessor_external_status_is_independent_of_the_property_and_other_accessor() {
    let external = 6 | (1 << 6) | (1 << 7);
    let decoded = parse_package_fragment_checked(&property_fragment(
        PROPERTY_FLAGS,
        Some(external),
        None,
        &[],
    ))
    .expect("one independently external getter");
    let [property] = decoded.properties.as_slice() else {
        panic!("one property")
    };
    assert!(!property.is_external);
    assert!(property.getter_modifiers.is_external);
    assert!(!property.getter_modifiers.is_default);
    assert!(!property.setter_modifiers.is_external);
    assert!(property.setter_modifiers.is_default);
}

#[test]
fn inline_only_obligation_belongs_to_its_annotated_accessor() {
    let inline = 6 | (1 << 6) | (1 << 8);
    for (field, getter, setter) in [
        (177, InlineKind::MustInline, InlineKind::CanInline),
        (178, InlineKind::CanInline, InlineKind::MustInline),
        (15, InlineKind::MustInline, InlineKind::CanInline),
        (16, InlineKind::CanInline, InlineKind::MustInline),
        (152, InlineKind::MustInline, InlineKind::CanInline),
        (153, InlineKind::CanInline, InlineKind::MustInline),
        (170, InlineKind::CanInline, InlineKind::CanInline),
    ] {
        let decoded = parse_package_fragment_checked(&property_fragment(
            PROPERTY_FLAGS,
            Some(inline),
            Some(inline),
            &[field],
        ))
        .expect("the annotation belongs to its exact declaration");
        let libraries = KlibLibraries::from_packages(vec![(vec!["p".to_owned()], decoded)])
            .expect("valid accessor declarations");
        let symbols = libraries.symbols(SymbolNamespace::Package(type_name("p")), "value");
        let [property] = symbols.callables.properties() else {
            panic!("one property")
        };
        assert_eq!(property.getter.inline, getter, "annotation field {field}");
        assert_eq!(
            property.setter.as_ref().expect("setter").inline,
            setter,
            "annotation field {field}"
        );
    }
}

#[test]
fn each_accessor_publishes_the_propertys_declaration_owned_reified_ordinals() {
    use crate::metadata::semantic::{
        KotlinFunctionTypeShape, KotlinType, KotlinTypeParameter, KotlinTypeParameterId,
    };
    use crate::types::TypeVariance;

    let inline = 6 | (1 << 6) | (1 << 8);
    let mut decoded = parse_package_fragment_checked(&property_fragment(
        PROPERTY_FLAGS,
        Some(inline),
        Some(inline),
        &[],
    ))
    .expect("inline accessor metadata");
    let [property] = decoded.properties.as_mut_slice() else {
        panic!("one property")
    };
    let formal = |id, name: &str, reified| KotlinTypeParameter {
        id: KotlinTypeParameterId(id),
        name: name.to_owned(),
        bounds: Vec::new(),
        variance: TypeVariance::Invariant,
        only_input: false,
        reified,
    };
    property.formals = vec![formal(13, "A", false), formal(41, "B", true)];
    property.receiver = Some(KotlinType::Class {
        internal: "kotlin/Pair".to_owned(),
        args: property
            .formals
            .iter()
            .map(|formal| KotlinType::Param {
                name: formal.name.clone(),
                id: formal.id,
                nullable: false,
            })
            .collect(),
        nullable: false,
        shape: KotlinFunctionTypeShape::default(),
    });
    let libraries = KlibLibraries::from_packages(vec![(vec!["p".to_owned()], decoded)])
        .expect("property formals retain their metadata identities");
    let symbols = libraries.symbols(SymbolNamespace::Package(type_name("p")), "value");
    let [property] = symbols.callables.properties() else {
        panic!("one property")
    };
    for accessor in [
        &property.getter,
        property.setter.as_deref().expect("setter"),
    ] {
        assert_eq!(accessor.reified_type_parameter_ordinals.as_ref(), [1]);
        assert_eq!(
            accessor
                .generic_sig
                .as_ref()
                .expect("generic declaration")
                .formals
                .len(),
            2
        );
        assert_eq!(accessor.inline, InlineKind::MustInline);
    }
}

#[test]
fn default_accessors_cannot_claim_inline_or_external_implementations() {
    for (getter, setter, context) in [
        (Some(6 | (1 << 8)), None, "getter"),
        (None, Some(6 | (1 << 7)), "setter"),
    ] {
        let error =
            parse_package_fragment_checked(&property_fragment(PROPERTY_FLAGS, getter, setter, &[]))
                .err()
                .expect("inconsistent accessor implementation modifiers must reject");
        assert_eq!(error.offset, 0);
        assert_eq!(
            error.detail,
            format!(
                "property {context} declares inline or external without a non-default accessor"
            )
        );
    }
}
