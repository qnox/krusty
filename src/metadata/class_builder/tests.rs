use super::declarations::*;
use super::jvm::*;
use super::schema::*;
use crate::metadata::version_requirements::VersionRequirement;
use crate::types::{Ty, Visibility};

#[test]
fn const_property_flags_preserve_visibility() {
    let flags = |visibility| {
        property_flags(&PropMeta {
            return_value_status: Default::default(),
            spellings: crate::spelling::DeclaredSpellings::default(),
            name: "x".into(),
            ty: Ty::Int,
            context_params: Vec::new(),
            is_var: false,
            visibility,
            has_constant: true,
            is_const: true,
            modifiers: Default::default(),
            setter_visibility: visibility,
            tparam: None,
            receiver: None,
            type_params: Vec::new(),
            setter_parameter_name: None,
            annotations: Default::default(),
            field_annotations: Default::default(),
            accessor_annotations: Default::default(),
            companion: false,
        })
    };

    assert_eq!(flags(Visibility::Internal), 10752);
    assert_eq!(flags(Visibility::Private), 10754);
    assert_eq!(flags(Visibility::Protected), 10756);
    assert_eq!(flags(Visibility::Public), 10758);
}

// Ground truth: kotlinc 2.4.0 `package demo; class E` → @Metadata mv=[2,4,0] k=1 xi=48, and this
// exact d1 protobuf (mUTF-8-decoded to raw bytes) + d2 string table. Drives byte-for-byte parity.
#[test]
fn empty_class_metadata_byte_matches_kotlinc() {
    let (d1, d2) = build_class(
        crate::types::type_name("demo/E"),
        &[],
        &[],
        &[],
        &[],
        &ClassTail::default(),
        &JvmClassSignatures {
            primary_constructor: Some(JvmConstructorSignature {
                name: "<init>".into(),
                desc: "()V".into(),
            }),
            ..Default::default()
        },
    );
    assert_eq!(
        d2,
        vec![
            "Ldemo/E;".to_string(),
            "".to_string(),
            "<init>".to_string(),
            "()V".to_string(),
        ],
        "d2 string table",
    );
    assert_eq!(
        d1,
        vec![
            0x00, 0x0c, 0x0a, 0x02, 0x18, 0x02, 0x0a, 0x02, 0x10, 0x00, 0x0a, 0x02, 0x08, 0x02,
            0x18, 0x00, 0x32, 0x02, 0x30, 0x01, 0x42, 0x07, 0xa2, 0x06, 0x04, 0x08, 0x02, 0x10,
            0x03,
        ],
        "d1 protobuf",
    );
}

// Ground truth: kotlinc 2.4.0 `package demo; class C(val x: Int)` — one ctor-param property.
#[test]
fn one_property_class_metadata_byte_matches_kotlinc() {
    let (d1, d2) = build_class(
        crate::types::type_name("demo/C"),
        &[("x".into(), Ty::Int)],
        &[PropMeta {
            return_value_status: Default::default(),
            spellings: crate::spelling::DeclaredSpellings::default(),
            name: "x".into(),
            ty: Ty::Int,
            context_params: Vec::new(),
            is_var: false,
            has_constant: false,
            is_const: false,
            visibility: Visibility::Public,
            modifiers: Default::default(),
            setter_visibility: Visibility::Public,
            tparam: None,
            receiver: None,
            type_params: Vec::new(),
            setter_parameter_name: None,
            annotations: Default::default(),
            field_annotations: Default::default(),
            accessor_annotations: Default::default(),
            companion: false,
        }],
        &[],
        &[],
        &ClassTail::default(),
        &JvmClassSignatures {
            primary_constructor: Some(JvmConstructorSignature {
                name: "<init>".into(),
                desc: "(I)V".into(),
            }),
            properties: vec![JvmPropertySignature {
                getter: Some(("getX".into(), "()I".into())),
                field: Some(JvmFieldSignature::default()),
                ..Default::default()
            }],
            ..Default::default()
        },
    );
    assert_eq!(
        d2,
        vec!["Ldemo/C;", "", "x", "", "<init>", "(I)V", "getX", "()I"]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>(),
        "d2 string table",
    );
    assert_eq!(
        d1,
        vec![
            0x00, 0x12, 0x0a, 0x02, 0x18, 0x02, 0x0a, 0x02, 0x10, 0x00, 0x0a, 0x00, 0x0a, 0x02,
            0x10, 0x08, 0x0a, 0x02, 0x08, 0x04, 0x18, 0x00, 0x32, 0x02, 0x30, 0x01, 0x42, 0x0f,
            0x12, 0x06, 0x10, 0x02, 0x1a, 0x02, 0x30, 0x03, 0xa2, 0x06, 0x04, 0x08, 0x04, 0x10,
            0x05, 0x52, 0x11, 0x10, 0x02, 0x1a, 0x02, 0x30, 0x03, 0xa2, 0x06, 0x08, 0x0a, 0x00,
            0x1a, 0x04, 0x08, 0x06, 0x10, 0x07,
        ],
        "d1 protobuf",
    );
}

// Ground truth: kotlinc 2.4.20 `package app; class A { var x: Int = 0 private set }`. A bodiless
// `private set` is not the default setter; its unnamed value parameter is `value`, serialized
// before the property name. kotlinc emits no `setX` for it, so no setter signature is recorded.
#[test]
fn private_setter_records_value_parameter_before_the_property_name() {
    let (d1, d2) = build_class(
        crate::types::type_name("app/A"),
        &[],
        &[PropMeta {
            spellings: crate::spelling::DeclaredSpellings::default(),
            name: "x".into(),
            ty: Ty::Int,
            context_params: Vec::new(),
            is_var: true,
            has_constant: false,
            is_const: false,
            return_value_status: Default::default(),
            visibility: Visibility::Public,
            modifiers: Default::default(),
            setter_visibility: Visibility::Private,
            tparam: None,
            receiver: None,
            type_params: Vec::new(),
            setter_parameter_name: None,
            annotations: Default::default(),
            field_annotations: Default::default(),
            accessor_annotations: Default::default(),
            companion: false,
        }],
        &[],
        &[],
        &ClassTail::default(),
        &JvmClassSignatures {
            primary_constructor: Some(JvmConstructorSignature {
                name: "<init>".into(),
                desc: "()V".into(),
            }),
            properties: vec![JvmPropertySignature {
                getter: Some(("getX".into(), "()I".into())),
                field: Some(JvmFieldSignature::default()),
                ..Default::default()
            }],
            ..Default::default()
        },
    );
    assert_eq!(
        d2,
        vec!["Lapp/A;", "", "<init>", "()V", "value", "", "x", "getX", "()I"]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>(),
        "d2 string table",
    );
    assert_eq!(
        d1,
        vec![
            0x00, 0x14, 0x0a, 0x02, 0x18, 0x02, 0x0a, 0x02, 0x10, 0x00, 0x0a, 0x02, 0x08, 0x03,
            0x0a, 0x02, 0x10, 0x08, 0x0a, 0x02, 0x08, 0x03, 0x18, 0x00, 0x32, 0x02, 0x30, 0x01,
            0x42, 0x07, 0xa2, 0x06, 0x04, 0x08, 0x02, 0x10, 0x03, 0x52, 0x1e, 0x10, 0x06, 0x1a,
            0x02, 0x30, 0x05, 0x32, 0x06, 0x10, 0x04, 0x1a, 0x02, 0x30, 0x05, 0x40, 0x42, 0x58,
            0x86, 0x0e, 0xa2, 0x06, 0x08, 0x0a, 0x00, 0x1a, 0x04, 0x08, 0x07, 0x10, 0x08,
        ],
        "d1 protobuf",
    );
}

// Ground truth: kotlinc 2.4.0 `package demo; data class Point(val x: Int, var y: String)` — the
// full data-class shape (6 synthesized methods + IS_DATA flag + a var property).
#[test]
fn data_class_metadata_byte_matches_kotlinc() {
    let any_q = Ty::nullable(Ty::obj("kotlin/Any"));
    let methods = vec![
        FnMeta {
            has_source: true,
            contract: None,
            context_count: 0,
            context_parameter_kinds: Vec::new(),
            spellings: crate::spelling::DeclaredSpellings::default(),
            name: "component1".into(),
            params: vec![],
            ret: Ty::Int,
            type_params: Vec::new(),
            semantic_type_params: Vec::new(),
            type_param_bounds: Vec::new(),
            flags: COMPONENT_FN_FLAGS,
            has_function_typed_parameter: false,
            params_have_defaults: false,
            receiver: None,
            param_modifiers: Vec::new(),
            vararg_index: None,
            annotations: Default::default(),
            param_annotations: Vec::new(),
            no_infer_params: Vec::new(),
        },
        FnMeta {
            has_source: true,
            contract: None,
            context_count: 0,
            context_parameter_kinds: Vec::new(),
            spellings: crate::spelling::DeclaredSpellings::default(),
            name: "component2".into(),
            params: vec![],
            ret: Ty::String,
            type_params: Vec::new(),
            semantic_type_params: Vec::new(),
            type_param_bounds: Vec::new(),
            flags: COMPONENT_FN_FLAGS,
            has_function_typed_parameter: false,
            params_have_defaults: false,
            receiver: None,
            param_modifiers: Vec::new(),
            vararg_index: None,
            annotations: Default::default(),
            param_annotations: Vec::new(),
            no_infer_params: Vec::new(),
        },
        FnMeta {
            has_source: true,
            contract: None,
            context_count: 0,
            context_parameter_kinds: Vec::new(),
            spellings: crate::spelling::DeclaredSpellings::default(),
            name: "copy".into(),
            params: vec![("x".into(), Ty::Int), ("y".into(), Ty::String)],
            ret: Ty::obj("demo/Point"),
            type_params: Vec::new(),
            semantic_type_params: Vec::new(),
            type_param_bounds: Vec::new(),
            flags: COPY_FN_FLAGS,
            has_function_typed_parameter: false,
            params_have_defaults: true,
            receiver: None,
            param_modifiers: Vec::new(),
            vararg_index: None,
            annotations: Default::default(),
            param_annotations: Vec::new(),
            no_infer_params: Vec::new(),
        },
        FnMeta {
            has_source: true,
            contract: None,
            context_count: 0,
            context_parameter_kinds: Vec::new(),
            spellings: crate::spelling::DeclaredSpellings::default(),
            name: "equals".into(),
            params: vec![("other".into(), any_q)],
            ret: Ty::Boolean,
            type_params: Vec::new(),
            semantic_type_params: Vec::new(),
            type_param_bounds: Vec::new(),
            flags: EQUALS_FN_FLAGS,
            has_function_typed_parameter: false,
            params_have_defaults: false,
            receiver: None,
            param_modifiers: Vec::new(),
            vararg_index: None,
            annotations: Default::default(),
            param_annotations: Vec::new(),
            no_infer_params: Vec::new(),
        },
        FnMeta {
            has_source: true,
            contract: None,
            context_count: 0,
            context_parameter_kinds: Vec::new(),
            spellings: crate::spelling::DeclaredSpellings::default(),
            name: "hashCode".into(),
            params: vec![],
            ret: Ty::Int,
            type_params: Vec::new(),
            semantic_type_params: Vec::new(),
            type_param_bounds: Vec::new(),
            flags: HASHCODE_TOSTRING_FN_FLAGS,
            has_function_typed_parameter: false,
            params_have_defaults: false,
            receiver: None,
            param_modifiers: Vec::new(),
            vararg_index: None,
            annotations: Default::default(),
            param_annotations: Vec::new(),
            no_infer_params: Vec::new(),
        },
        FnMeta {
            has_source: true,
            contract: None,
            context_count: 0,
            context_parameter_kinds: Vec::new(),
            spellings: crate::spelling::DeclaredSpellings::default(),
            name: "toString".into(),
            params: vec![],
            ret: Ty::String,
            type_params: Vec::new(),
            semantic_type_params: Vec::new(),
            type_param_bounds: Vec::new(),
            flags: HASHCODE_TOSTRING_FN_FLAGS,
            has_function_typed_parameter: false,
            params_have_defaults: false,
            receiver: None,
            param_modifiers: Vec::new(),
            vararg_index: None,
            annotations: Default::default(),
            param_annotations: Vec::new(),
            no_infer_params: Vec::new(),
        },
    ];
    let props = vec![
        PropMeta {
            return_value_status: Default::default(),
            spellings: crate::spelling::DeclaredSpellings::default(),
            name: "x".into(),
            ty: Ty::Int,
            context_params: Vec::new(),
            is_var: false,
            has_constant: false,
            is_const: false,
            visibility: Visibility::Public,
            modifiers: Default::default(),
            setter_visibility: Visibility::Public,
            tparam: None,
            receiver: None,
            type_params: Vec::new(),
            setter_parameter_name: None,
            annotations: Default::default(),
            field_annotations: Default::default(),
            accessor_annotations: Default::default(),
            companion: false,
        },
        PropMeta {
            return_value_status: Default::default(),
            spellings: crate::spelling::DeclaredSpellings::default(),
            name: "y".into(),
            ty: Ty::String,
            context_params: Vec::new(),
            is_var: true,
            has_constant: false,
            is_const: false,
            visibility: Visibility::Public,
            modifiers: Default::default(),
            setter_visibility: Visibility::Public,
            tparam: None,
            receiver: None,
            type_params: Vec::new(),
            setter_parameter_name: None,
            annotations: Default::default(),
            field_annotations: Default::default(),
            accessor_annotations: Default::default(),
            companion: false,
        },
    ];
    let (d1, _d2) = build_class(
        crate::types::type_name("demo/Point"),
        &[("x".into(), Ty::Int), ("y".into(), Ty::String)],
        &props,
        &methods,
        &[],
        &ClassTail {
            // public + final + IS_DATA, as `class_metadata_flags` derives for a `data class`.
            flags: 1030,
            ..Default::default()
        },
        &JvmClassSignatures {
            primary_constructor: Some(JvmConstructorSignature {
                name: "<init>".into(),
                desc: "(ILjava/lang/String;)V".into(),
            }),
            properties: vec![
                JvmPropertySignature {
                    getter: Some(("getX".into(), "()I".into())),
                    field: Some(JvmFieldSignature::default()),
                    ..Default::default()
                },
                JvmPropertySignature {
                    getter: Some(("getY".into(), "()Ljava/lang/String;".into())),
                    setter: Some(("setY".into(), "(Ljava/lang/String;)V".into())),
                    field: Some(JvmFieldSignature::default()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        },
    );
    assert_eq!(
        d1,
        vec![
            0x00, 0x20, 0x0a, 0x02, 0x18, 0x02, 0x0a, 0x02, 0x10, 0x00, 0x0a, 0x00, 0x0a, 0x02,
            0x10, 0x08, 0x0a, 0x00, 0x0a, 0x02, 0x10, 0x0e, 0x0a, 0x02, 0x08, 0x0c, 0x0a, 0x02,
            0x10, 0x0b, 0x0a, 0x02, 0x08, 0x03, 0x08, 0x86, 0x08, 0x18, 0x00, 0x32, 0x02, 0x30,
            0x01, 0x42, 0x17, 0x12, 0x06, 0x10, 0x02, 0x1a, 0x02, 0x30, 0x03, 0x12, 0x06, 0x10,
            0x04, 0x1a, 0x02, 0x30, 0x05, 0xa2, 0x06, 0x04, 0x08, 0x06, 0x10, 0x07, 0x4a, 0x09,
            0x10, 0x0e, 0x1a, 0x02, 0x30, 0x03, 0x48, 0xc6, 0x03, 0x4a, 0x09, 0x10, 0x0f, 0x1a,
            0x02, 0x30, 0x05, 0x48, 0xc6, 0x03, 0x4a, 0x1d, 0x10, 0x10, 0x1a, 0x02, 0x30, 0x00,
            0x32, 0x08, 0x08, 0x02, 0x10, 0x02, 0x1a, 0x02, 0x30, 0x03, 0x32, 0x08, 0x08, 0x02,
            0x10, 0x04, 0x1a, 0x02, 0x30, 0x05, 0x48, 0xc6, 0x01, 0x4a, 0x14, 0x10, 0x11, 0x1a,
            0x02, 0x30, 0x12, 0x32, 0x08, 0x10, 0x13, 0x1a, 0x04, 0x18, 0x01, 0x30, 0x01, 0x48,
            0xd6, 0x83, 0x04, 0x4a, 0x0a, 0x10, 0x14, 0x1a, 0x02, 0x30, 0x03, 0x48, 0xd6, 0x81,
            0x04, 0x4a, 0x0a, 0x10, 0x15, 0x1a, 0x02, 0x30, 0x05, 0x48, 0xd6, 0x81, 0x04, 0x52,
            0x11, 0x10, 0x02, 0x1a, 0x02, 0x30, 0x03, 0xa2, 0x06, 0x08, 0x0a, 0x00, 0x1a, 0x04,
            0x08, 0x08, 0x10, 0x09, 0x52, 0x1a, 0x10, 0x04, 0x1a, 0x02, 0x30, 0x05, 0x58, 0x86,
            0x0e, 0xa2, 0x06, 0x0e, 0x0a, 0x00, 0x1a, 0x04, 0x08, 0x0a, 0x10, 0x0b, 0x22, 0x04,
            0x08, 0x0c, 0x10, 0x0d,
        ],
        "d1 protobuf",
    );
}

// A generic property (`List<String>`) + a defaulted ctor param — the shape real production domain
// models use. Verified byte-identical to kotlinc 2.4.0 on a real config data class; this pins the
// pieces it needs: `List` encoded as a `predefinedIndex` builtin (NOT a class-id descriptor), the
// `Type.argument` (String), and the `DECLARES_DEFAULT_VALUE` ctor-param flag.
#[test]
fn generic_property_and_default_ctor_param() {
    let list_string = Ty::obj_args("kotlin/collections/List", &[Ty::String]);
    let (_d1, d2) = build_class(
        crate::types::type_name("demo/D"),
        &[("r".into(), list_string)],
        &[PropMeta {
            return_value_status: Default::default(),
            spellings: crate::spelling::DeclaredSpellings::default(),
            name: "r".into(),
            ty: list_string,
            context_params: Vec::new(),
            is_var: false,
            has_constant: false,
            is_const: false,
            visibility: Visibility::Public,
            modifiers: Default::default(),
            setter_visibility: Visibility::Public,
            tparam: None,
            receiver: None,
            type_params: Vec::new(),
            setter_parameter_name: None,
            annotations: Default::default(),
            field_annotations: Default::default(),
            accessor_annotations: Default::default(),
            companion: false,
        }],
        &[],
        &[],
        &ClassTail {
            ctor_param_defaults: &[true],
            ..Default::default()
        },
        &JvmClassSignatures {
            primary_constructor: Some(JvmConstructorSignature {
                name: "<init>".into(),
                desc: "(Ljava/util/List;)V".into(),
            }),
            properties: vec![JvmPropertySignature {
                getter: Some(("getR".into(), "()Ljava/util/List;".into())),
                field: Some(JvmFieldSignature::default()),
                ..Default::default()
            }],
            ..Default::default()
        },
    );
    // `List` is a builtin (predefinedIndex 32) → an EMPTY d2 slot, never the literal descriptor.
    assert!(
        !d2.iter()
            .any(|s| s == "Ljava/util/List;" || s == "Lkotlin/collections/List;"),
        "List must encode as a builtin predefinedIndex, not a class-id descriptor: {d2:?}",
    );
    // The ctor value parameter carries `DECLARES_DEFAULT_VALUE` (f1=2) — the `08 02` prefix inside
    // the constructor's value_parameter, before its name. Its absence would drop the flag.
    assert!(
        _d1.windows(2).any(|w| w == [0x08, 0x02]),
        "the defaulted ctor param must encode DECLARES_DEFAULT_VALUE",
    );
}

// Ground truth: kotlinc 2.4.0 `package demo; class S { fun f(n: Int): Int = n }` — a regular
// (non-synthesized) member function. A plain public-final member has metadata flags omitted (0).
#[test]
fn regular_method_class_metadata_byte_matches_kotlinc() {
    let (d1, _d2) = build_class(
        crate::types::type_name("demo/S"),
        &[],
        &[],
        &[FnMeta::plain(
            "f".into(),
            vec![("n".into(), Ty::Int)],
            Ty::Int,
        )],
        &[],
        &ClassTail::default(),
        &JvmClassSignatures {
            primary_constructor: Some(JvmConstructorSignature {
                name: "<init>".into(),
                desc: "()V".into(),
            }),
            ..Default::default()
        },
    );
    assert_eq!(
        d1,
        vec![
            0x00, 0x12, 0x0a, 0x02, 0x18, 0x02, 0x0a, 0x02, 0x10, 0x00, 0x0a, 0x02, 0x08, 0x03,
            0x0a, 0x02, 0x10, 0x08, 0x0a, 0x00, 0x18, 0x00, 0x32, 0x02, 0x30, 0x01, 0x42, 0x07,
            0xa2, 0x06, 0x04, 0x08, 0x02, 0x10, 0x03, 0x4a, 0x0e, 0x10, 0x04, 0x1a, 0x02, 0x30,
            0x05, 0x32, 0x06, 0x10, 0x06, 0x1a, 0x02, 0x30, 0x05,
        ],
        "d1 protobuf",
    );
}

// Ground truth: kotlinc 2.4.0 `package demo; class C { companion object }`. The companion object
// adds `companionObjectName` (f4) + a `nestedClassName` (f7), both referencing `Companion`, interned
// after the ctor.
#[test]
fn companion_object_metadata_byte_matches_kotlinc() {
    let (d1, d2) = build_class(
        crate::types::type_name("demo/C"),
        &[],
        &[],
        &[],
        &[],
        &ClassTail {
            companion: Some("Companion"),
            nested: &["Companion"],
            ..Default::default()
        },
        &JvmClassSignatures {
            primary_constructor: Some(JvmConstructorSignature {
                name: "<init>".into(),
                desc: "()V".into(),
            }),
            ..Default::default()
        },
    );
    assert_eq!(
        d2,
        vec!["Ldemo/C;", "", "<init>", "()V", "Companion"]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>(),
        "d2",
    );
    assert_eq!(
        d1,
        vec![
            0x00, 0x0c, 0x0a, 0x02, 0x18, 0x02, 0x0a, 0x02, 0x10, 0x00, 0x0a, 0x02, 0x08, 0x03,
            0x18, 0x00, 0x20, 0x04, 0x32, 0x02, 0x30, 0x01, 0x3a, 0x01, 0x04, 0x42, 0x07, 0xa2,
            0x06, 0x04, 0x08, 0x02, 0x10, 0x03,
        ],
        "d1 protobuf",
    );
}

// Ground truth from kotlinc 2.4.10's `FirJvmSerializerExtension`: an interface compiled with
// `-jvm-default=no-compatibility` references requirement-table slot 0 (Class f31), whose single
// entry requires compiler 1.4.0 and has VersionKind.COMPILER_VERSION (Class f32). The JVM class
// flags extension follows it in field order.
#[test]
fn no_compatibility_compiler_requirement_matches_kotlinc() {
    let (d1, _) = build_class(
        crate::types::type_name("demo/I"),
        &[],
        &[],
        &[],
        &[],
        &ClassTail {
            flags: 102,
            emit_primary_ctor: false,
            compiler_version_requirement: Some(VersionRequirement::compiler(1, 4, 0)),
            ..Default::default()
        },
        &JvmClassSignatures {
            class_flags: Some(1),
            primary_constructor: Some(JvmConstructorSignature {
                name: "<init>".into(),
                desc: "()V".into(),
            }),
            ..Default::default()
        },
    );
    let requirement_and_flags = [
        0xf8, 0x01, 0x00, // Class.versionRequirement = table index 0
        0x82, 0x02, 0x06, 0x0a, 0x04, 0x08, 0x21, 0x30, 0x01, // table: compiler 1.4.0
        0xc0, 0x06, 0x01, // JvmProtoBuf.jvmClassFlags = 1
    ];
    assert!(
        d1.windows(requirement_and_flags.len())
            .any(|window| window == requirement_and_flags),
        "missing no-compatibility requirement: {d1:02x?}"
    );
}

// Ground truth: kotlinc 2.4.0 `class C(val x: Int)` compiled with `-module-name mymod`. Adds
// `classModuleName` (f101) = the module name, interned last.
#[test]
fn module_name_metadata_byte_matches_kotlinc() {
    let (d1, d2) = build_class(
        crate::types::type_name("demo/C"),
        &[("x".into(), Ty::Int)],
        &[PropMeta {
            return_value_status: Default::default(),
            spellings: crate::spelling::DeclaredSpellings::default(),
            name: "x".into(),
            ty: Ty::Int,
            context_params: Vec::new(),
            is_var: false,
            has_constant: false,
            is_const: false,
            visibility: Visibility::Public,
            modifiers: Default::default(),
            setter_visibility: Visibility::Public,
            tparam: None,
            receiver: None,
            type_params: Vec::new(),
            setter_parameter_name: None,
            annotations: Default::default(),
            field_annotations: Default::default(),
            accessor_annotations: Default::default(),
            companion: false,
        }],
        &[],
        &[],
        &ClassTail {
            ..Default::default()
        },
        &JvmClassSignatures {
            module_name: Some("mymod"),
            primary_constructor: Some(JvmConstructorSignature {
                name: "<init>".into(),
                desc: "(I)V".into(),
            }),
            properties: vec![JvmPropertySignature {
                getter: Some(("getX".into(), "()I".into())),
                field: Some(JvmFieldSignature::default()),
                ..Default::default()
            }],
            ..Default::default()
        },
    );
    assert_eq!(
        d2,
        vec!["Ldemo/C;", "", "x", "", "<init>", "(I)V", "getX", "()I", "mymod"]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>(),
        "d2",
    );
    assert_eq!(
        d1,
        vec![
            0x00, 0x12, 0x0a, 0x02, 0x18, 0x02, 0x0a, 0x02, 0x10, 0x00, 0x0a, 0x00, 0x0a, 0x02,
            0x10, 0x08, 0x0a, 0x02, 0x08, 0x05, 0x18, 0x00, 0x32, 0x02, 0x30, 0x01, 0x42, 0x0f,
            0x12, 0x06, 0x10, 0x02, 0x1a, 0x02, 0x30, 0x03, 0xa2, 0x06, 0x04, 0x08, 0x04, 0x10,
            0x05, 0x52, 0x11, 0x10, 0x02, 0x1a, 0x02, 0x30, 0x03, 0xa2, 0x06, 0x08, 0x0a, 0x00,
            0x1a, 0x04, 0x08, 0x06, 0x10, 0x07, 0xa8, 0x06, 0x08,
        ],
        "d1 protobuf",
    );
}

// Ground truth: kotlinc 2.4.0 `class C(val x: Int) { constructor() : this(0) }` — a second
// (secondary) constructor. `Class.constructor` (f8) is repeated; the secondary carries flags 22.
#[test]
fn secondary_ctor_metadata_byte_matches_kotlinc() {
    let (d1, d2) = build_class(
        crate::types::type_name("demo/C"),
        &[("x".into(), Ty::Int)],
        &[PropMeta {
            return_value_status: Default::default(),
            spellings: crate::spelling::DeclaredSpellings::default(),
            name: "x".into(),
            ty: Ty::Int,
            context_params: Vec::new(),
            is_var: false,
            has_constant: false,
            is_const: false,
            visibility: Visibility::Public,
            modifiers: Default::default(),
            setter_visibility: Visibility::Public,
            tparam: None,
            receiver: None,
            type_params: Vec::new(),
            setter_parameter_name: None,
            annotations: Default::default(),
            field_annotations: Default::default(),
            accessor_annotations: Default::default(),
            companion: false,
        }],
        &[],
        &[],
        &ClassTail {
            secondary_ctors: &[CtorMeta {
                params: &[],
                param_spellings: &[],
                param_defaults: &[],
                vararg_index: None,
                flags: 22,
                annotations: &crate::metadata::NO_ANNOTATIONS,
            }],
            ..Default::default()
        },
        &JvmClassSignatures {
            primary_constructor: Some(JvmConstructorSignature {
                name: "<init>".into(),
                desc: "(I)V".into(),
            }),
            secondary_constructors: vec![JvmConstructorSignature {
                name: "<init>".into(),
                desc: "()V".into(),
            }],
            properties: vec![JvmPropertySignature {
                getter: Some(("getX".into(), "()I".into())),
                field: Some(JvmFieldSignature::default()),
                ..Default::default()
            }],
            ..Default::default()
        },
    );
    assert_eq!(
        d2,
        vec!["Ldemo/C;", "", "x", "", "<init>", "(I)V", "()V", "getX", "()I"]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>(),
        "d2",
    );
    assert_eq!(
        d1,
        vec![
            0x00, 0x12, 0x0a, 0x02, 0x18, 0x02, 0x0a, 0x02, 0x10, 0x00, 0x0a, 0x00, 0x0a, 0x02,
            0x10, 0x08, 0x0a, 0x02, 0x08, 0x05, 0x18, 0x00, 0x32, 0x02, 0x30, 0x01, 0x42, 0x0f,
            0x12, 0x06, 0x10, 0x02, 0x1a, 0x02, 0x30, 0x03, 0xa2, 0x06, 0x04, 0x08, 0x04, 0x10,
            0x05, 0x42, 0x09, 0x08, 0x16, 0xa2, 0x06, 0x04, 0x08, 0x04, 0x10, 0x06, 0x52, 0x11,
            0x10, 0x02, 0x1a, 0x02, 0x30, 0x03, 0xa2, 0x06, 0x08, 0x0a, 0x00, 0x1a, 0x04, 0x08,
            0x07, 0x10, 0x08,
        ],
        "d1 protobuf",
    );
}

#[test]
fn class_metadata_has_expected_strings() {
    let (d1, d2) = build_class(
        crate::types::type_name("demo/Point"),
        &[("x".into(), Ty::Int), ("y".into(), Ty::String)],
        &[
            PropMeta {
                return_value_status: Default::default(),
                spellings: crate::spelling::DeclaredSpellings::default(),
                name: "x".into(),
                ty: Ty::Int,
                context_params: Vec::new(),
                is_var: false,
                has_constant: false,
                is_const: false,
                visibility: Visibility::Public,
                modifiers: Default::default(),
                setter_visibility: Visibility::Public,
                tparam: None,
                receiver: None,
                type_params: Vec::new(),
                setter_parameter_name: None,
                annotations: Default::default(),
                field_annotations: Default::default(),
                accessor_annotations: Default::default(),
                companion: false,
            },
            PropMeta {
                return_value_status: Default::default(),
                spellings: crate::spelling::DeclaredSpellings::default(),
                name: "y".into(),
                ty: Ty::String,
                context_params: Vec::new(),
                is_var: true,
                has_constant: false,
                is_const: false,
                visibility: Visibility::Public,
                // A named setter parameter exists only on a setter with a written body.
                modifiers: crate::ir::IrPropertyModifiers {
                    declared_setter: true,
                    ..Default::default()
                },
                setter_visibility: Visibility::Public,
                tparam: None,
                receiver: None,
                type_params: Vec::new(),
                setter_parameter_name: Some("replacement".into()),
                annotations: Default::default(),
                field_annotations: Default::default(),
                accessor_annotations: Default::default(),
                companion: false,
            },
        ],
        &[],
        &[],
        &ClassTail::default(),
        &JvmClassSignatures {
            primary_constructor: Some(JvmConstructorSignature {
                name: "<init>".into(),
                desc: "(ILjava/lang/String;)V".into(),
            }),
            properties: vec![
                JvmPropertySignature {
                    getter: Some(("getX".into(), "()I".into())),
                    field: Some(JvmFieldSignature::default()),
                    ..Default::default()
                },
                JvmPropertySignature {
                    getter: Some(("getY".into(), "()Ljava/lang/String;".into())),
                    setter: Some(("setY".into(), "(Ljava/lang/String;)V".into())),
                    field: Some(JvmFieldSignature::default()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        },
    );
    // The class id descriptor and the JVM signatures must all appear verbatim in d2.
    assert!(d2.contains(&"Ldemo/Point;".to_string()));
    assert!(d2.contains(&"getX".to_string()));
    assert!(d2.contains(&"setY".to_string()));
    assert!(d2.contains(&"(ILjava/lang/String;)V".to_string()));
    let d1 = String::from_iter(d1.into_iter().map(char::from));
    let metadata =
        crate::jvm::metadata::decode_metadata(&[d1], &d2, Some(1), "demo/Point", None, &[])
            .expect("generated class metadata decodes");
    assert_eq!(metadata.class_properties.len(), 2);
    assert_eq!(metadata.class_properties[0].setter_parameter_name, None);
    assert_eq!(
        metadata.class_properties[1]
            .setter_parameter_name
            .as_deref(),
        Some("replacement")
    );
}
