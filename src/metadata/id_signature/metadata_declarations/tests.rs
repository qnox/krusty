use super::*;
use crate::metadata::semantic::KotlinFunctionTypeShape;
use crate::types::{ReturnValueStatus, TypeVariance, Visibility};

// Every expected member id below is the one the Kotlin/Native 2.4.20 stdlib KLIB serializes for
// the named declaration.

fn class(internal: &str, args: Vec<KotlinType>) -> KotlinType {
    KotlinType::Class {
        internal: internal.to_owned(),
        args,
        nullable: false,
        shape: KotlinFunctionTypeShape::default(),
    }
}

fn nullable(ty: KotlinType) -> KotlinType {
    match ty {
        KotlinType::Class {
            internal,
            args,
            shape,
            ..
        } => KotlinType::Class {
            internal,
            args,
            nullable: true,
            shape,
        },
        KotlinType::Param { name, id, .. } => KotlinType::Param {
            name,
            id,
            nullable: true,
        },
        other => other,
    }
}

fn param(id: u64, name: &str) -> KotlinType {
    KotlinType::Param {
        name: name.to_owned(),
        id: KotlinTypeParameterId(id),
        nullable: false,
    }
}

fn type_parameter(id: u64, name: &str, bounds: Vec<KotlinType>) -> KotlinTypeParameter {
    KotlinTypeParameter {
        id: KotlinTypeParameterId(id),
        name: name.to_owned(),
        bounds,
        variance: TypeVariance::Invariant,
        only_input: false,
        reified: false,
    }
}

fn function(
    name: &str,
    receiver: Option<KotlinType>,
    params: Vec<KotlinType>,
    formals: Vec<KotlinTypeParameter>,
) -> KotlinFunction {
    KotlinFunction {
        name: name.to_owned(),
        receiver,
        param_names: vec![String::new(); params.len()],
        param_defaults: vec![false; params.len()],
        params,
        ret: class("kotlin/Unit", Vec::new()),
        formals,
        vararg: None,
        visibility: Visibility::Public,
        is_inline: false,
        has_reified_type_params: false,
        is_suspend: false,
        is_operator: false,
        is_infix: false,
        is_expect: false,
        context_count: 0,
        annotations: Vec::new(),
    }
}

fn member(name: &str, params: Vec<KotlinType>, is_property: bool) -> KotlinMember {
    KotlinMember {
        name: name.to_owned(),
        receiver: None,
        context_params: Vec::new(),
        visibility: Visibility::Public,
        param_names: vec![String::new(); params.len()],
        param_defaults: vec![false; params.len()],
        params,
        ret: class("kotlin/Unit", Vec::new()),
        is_property,
        is_var: false,
        is_expect: false,
        is_operator: false,
        is_infix: false,
        is_abstract: false,
        return_value_status: ReturnValueStatus::default(),
        formals: Vec::new(),
        ret_nullable: false,
        constant: None,
        vararg: None,
        annotations: Vec::new(),
    }
}

fn package(segments: &[&str]) -> Vec<String> {
    segments
        .iter()
        .map(|segment| (*segment).to_owned())
        .collect()
}

fn top_level(package: &[String]) -> MetadataContainer<'_> {
    MetadataContainer {
        package,
        classes: &[],
        native_interop_library: false,
    }
}

fn member_id(signature: &KlibPublicIdSignature) -> i64 {
    signature.member_id().expect("a callable has a member id") as i64
}

#[test]
fn a_function_without_a_result_or_type_parameters() {
    let kotlin_io = package(&["kotlin", "io"]);
    let println = function(
        "println",
        None,
        vec![nullable(class("kotlin/Any", Vec::new()))],
        Vec::new(),
    );
    let signature = package_function_signature(top_level(&kotlin_io), &println).unwrap();
    assert_eq!(signature.package().segments(), ["kotlin", "io"]);
    assert_eq!(signature.declaration().segments(), ["println"]);
    assert_eq!(member_id(&signature), -3_363_048_611_743_956_379);
}

#[test]
fn an_extension_function() {
    let kotlin_ranges = package(&["kotlin", "ranges"]);
    let int = class("kotlin/Int", Vec::new());
    let coerce = function("coerceAtLeast", Some(int.clone()), vec![int], Vec::new());
    let signature = package_function_signature(top_level(&kotlin_ranges), &coerce).unwrap();
    assert_eq!(member_id(&signature), 3_998_717_805_095_061_419);
}

#[test]
fn type_parameters_are_referenced_by_container_and_index() {
    let kotlin_comparisons = package(&["kotlin", "comparisons"]);
    let max_of = function(
        "maxOf",
        None,
        vec![param(0, "T"), param(0, "T")],
        vec![type_parameter(
            0,
            "T",
            vec![class("kotlin/Comparable", vec![param(0, "T")])],
        )],
    );
    let signature = package_function_signature(top_level(&kotlin_comparisons), &max_of).unwrap();
    assert_eq!(member_id(&signature), -5_030_133_889_946_118_250);
}

#[test]
fn a_star_projection_and_an_unbounded_type_parameter() {
    let kotlin_sequences = package(&["kotlin", "sequences"]);
    let filter = function(
        "filterIsInstance",
        Some(class("kotlin/sequences/Sequence", vec![KotlinType::Star])),
        Vec::new(),
        vec![type_parameter(0, "R", Vec::new())],
    );
    let signature = package_function_signature(top_level(&kotlin_sequences), &filter).unwrap();
    assert_eq!(member_id(&signature), 298_316_304_477_115_805);
}

#[test]
fn a_suspend_function_type_is_its_suspend_function_class() {
    let kotlin_coroutines = package(&["kotlin", "coroutines"]);
    let continuation = class("kotlin/coroutines/Continuation", vec![param(0, "T")]);
    let block = KotlinType::Class {
        internal: "kotlin/Function1".to_owned(),
        args: vec![
            continuation.clone(),
            nullable(class("kotlin/Any", Vec::new())),
        ],
        nullable: false,
        shape: KotlinFunctionTypeShape {
            suspend: true,
            ..KotlinFunctionTypeShape::default()
        },
    };
    let create = function(
        "createCoroutine",
        Some(block),
        vec![continuation],
        vec![type_parameter(0, "T", Vec::new())],
    );
    let signature = package_function_signature(top_level(&kotlin_coroutines), &create).unwrap();
    assert_eq!(member_id(&signature), -5_059_621_468_224_635_510);
}

#[test]
fn context_parameters_come_first() {
    let kotlin = package(&["kotlin"]);
    let mut context_of = function(
        "contextOf",
        None,
        vec![param(0, "A")],
        vec![type_parameter(0, "A", Vec::new())],
    );
    context_of.context_count = 1;
    let signature = package_function_signature(top_level(&kotlin), &context_of).unwrap();
    assert_eq!(member_id(&signature), -12_594_581_380_294_987);
}

#[test]
fn an_extension_property_with_a_type_parameter() {
    let kotlin_collections = package(&["kotlin", "collections"]);
    let last_index = KotlinProperty {
        name: "lastIndex".to_owned(),
        receiver: Some(class("kotlin/collections/List", vec![param(0, "T")])),
        context_params: Vec::new(),
        ty: class("kotlin/Int", Vec::new()),
        formals: vec![type_parameter(0, "T", Vec::new())],
        visibility: Visibility::Public,
        is_var: false,
        is_expect: false,
        context_count: 0,
        constant: None,
    };
    let signature =
        package_property_signature(top_level(&kotlin_collections), &last_index).unwrap();
    assert_eq!(member_id(&signature), -7_238_914_123_027_933_299);
}

#[test]
fn class_members_see_the_class_type_parameters_as_the_next_container() {
    let kotlin = package(&["kotlin"]);
    let parameters = [
        type_parameter(0, "A", Vec::new()),
        type_parameter(1, "B", Vec::new()),
    ];
    let classes = [MetadataClass {
        name: "Pair",
        type_params: &parameters,
        expect: false,
    }];
    let pair = MetadataContainer {
        package: &kotlin,
        classes: &classes,
        native_interop_library: false,
    };
    let class_identity = metadata_class_signature(top_level(&kotlin), classes[0]);
    assert_eq!(class_identity.declaration().segments(), ["Pair"]);
    assert_eq!(class_identity.member_id(), None);

    let constructor = constructor_signature(
        pair,
        &KotlinConstructor {
            params: vec![param(0, "A"), param(1, "B")],
            param_names: vec![String::new(); 2],
            param_defaults: vec![false; 2],
            vararg: None,
            visibility: Visibility::Public,
        },
    )
    .unwrap();
    assert_eq!(constructor.declaration().segments(), ["Pair", "<init>"]);
    assert_eq!(member_id(&constructor), 3_086_114_026_882_374_588);

    let first = member_signature(pair, &member("first", Vec::new(), true)).unwrap();
    assert_eq!(first.declaration().segments(), ["Pair", "first"]);
    assert_eq!(member_id(&first), 1_497_393_077_339_299_626);
}

#[test]
fn a_member_function_references_its_class_type_parameter() {
    let kotlin_collections = package(&["kotlin", "collections"]);
    let parameters = [type_parameter(0, "E", Vec::new())];
    let classes = [MetadataClass {
        name: "ArrayList",
        type_params: &parameters,
        expect: false,
    }];
    let array_list = MetadataContainer {
        package: &kotlin_collections,
        classes: &classes,
        native_interop_library: false,
    };
    let add = member_signature(array_list, &member("add", vec![param(0, "E")], false)).unwrap();
    assert_eq!(add.declaration().segments(), ["ArrayList", "add"]);
    assert_eq!(member_id(&add), 4_996_012_557_724_047_373);
}

#[test]
fn an_extension_property_without_type_parameters() {
    let kotlin_text = package(&["kotlin", "text"]);
    let last_index = KotlinProperty {
        name: "lastIndex".to_owned(),
        receiver: Some(class("kotlin/CharSequence", Vec::new())),
        context_params: Vec::new(),
        ty: class("kotlin/Int", Vec::new()),
        formals: Vec::new(),
        visibility: Visibility::Public,
        is_var: false,
        is_expect: false,
        context_count: 0,
        constant: None,
    };
    let signature = package_property_signature(top_level(&kotlin_text), &last_index).unwrap();
    assert_eq!(member_id(&signature), 1_266_685_057_648_611_082);
}

#[test]
fn a_getter_is_a_function_with_the_property_receiver_and_type_parameters() {
    let kotlin_collections = package(&["kotlin", "collections"]);
    let last_index = KotlinProperty {
        name: "lastIndex".to_owned(),
        receiver: Some(class("kotlin/collections/List", vec![param(0, "T")])),
        context_params: Vec::new(),
        ty: class("kotlin/Int", Vec::new()),
        formals: vec![type_parameter(0, "T", Vec::new())],
        visibility: Visibility::Public,
        is_var: false,
        is_expect: false,
        context_count: 0,
        constant: None,
    };
    let getter = package_property_accessor_signature(
        top_level(&kotlin_collections),
        &last_index,
        MetadataAccessor::Getter,
    )
    .unwrap();
    assert_eq!(getter.property().declaration().segments(), ["lastIndex"]);
    assert_eq!(member_id(getter.property()), -7_238_914_123_027_933_299);
    assert_eq!(getter.name(), "<get-lastIndex>");
    assert_eq!(getter.member_id() as i64, 1_631_619_787_052_076_373);
    assert_eq!(
        package_property_accessor_signature(
            top_level(&kotlin_collections),
            &last_index,
            MetadataAccessor::Setter,
        )
        .unwrap_err()
        .to_string(),
        "property lastIndex is a val and has no setter"
    );
}

#[test]
fn a_setter_takes_the_property_type() {
    let kotlin_concurrent = package(&["kotlin", "concurrent"]);
    let parameters: [KotlinTypeParameter; 0] = [];
    let classes = [MetadataClass {
        name: "AtomicInt",
        type_params: &parameters,
        expect: false,
    }];
    let atomic_int = MetadataContainer {
        package: &kotlin_concurrent,
        classes: &classes,
        native_interop_library: false,
    };
    let mut value = member("value", Vec::new(), true);
    value.ret = class("kotlin/Int", Vec::new());
    value.is_var = true;
    let setter =
        member_property_accessor_signature(atomic_int, &value, MetadataAccessor::Setter).unwrap();
    assert_eq!(
        setter.property().declaration().segments(),
        ["AtomicInt", "value"]
    );
    assert_eq!(setter.name(), "<set-value>");
    assert_eq!(setter.member_id() as i64, -195_057_410_739_577_239);
}

#[test]
fn an_enum_entry_is_a_path_inside_its_enum_class() {
    let kotlin_native = package(&["kotlin", "native"]);
    let classes = [MetadataClass {
        name: "OsFamily",
        type_params: &[],
        expect: false,
    }];
    let os_family = MetadataContainer {
        package: &kotlin_native,
        classes: &classes,
        native_interop_library: false,
    };
    let entry = metadata_enum_entry_signature(os_family, "MACOSX");
    assert_eq!(entry.package().segments(), ["kotlin", "native"]);
    assert_eq!(entry.declaration().segments(), ["OsFamily", "MACOSX"]);
    assert_eq!(entry.member_id(), None);
    assert_eq!(entry.mask(), 0);
}

#[test]
fn an_unknown_type_parameter_is_an_error() {
    let kotlin = package(&["kotlin"]);
    let broken = function("f", None, vec![param(9, "T")], Vec::new());
    assert_eq!(
        package_function_signature(top_level(&kotlin), &broken)
            .unwrap_err()
            .to_string(),
        "type parameter KotlinTypeParameterId(9) is not in scope"
    );
}
