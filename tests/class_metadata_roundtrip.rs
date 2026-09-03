//! Writer→reader round-trip for CLASS `@Metadata`: `metadata::class_builder::build_class` encodes a
//! class's member functions (with their SOURCE `value_parameter` types), and `metadata::class_functions`
//! decodes them back. This is the contract cross-module resolution relies on — a dependent module reads
//! a lib class's member signatures (their source arity) from the lib's emitted `@Metadata`, exactly as
//! `Classpath::metadata_call_facts` does for a classpath callee. `build_class` existed but was unwired
//! and untested; this pins the round-trip before it is wired into emit.

use krusty::jvm::classreader::ClassInfo;
use krusty::jvm::metadata::{
    class_constructors, class_functions, decode_metadata, package_functions,
};
use krusty::metadata::class_builder::{build_class, ClassTail, CtorMeta, FnMeta};
use krusty::types::{type_name, Ty, TypeVariance};

/// Wrap built `(d1_bytes, d2)` into a `ClassInfo` the reader consumes. `d1` is the protobuf payload with
/// one byte per `char` (the constant pool writes it as modified-UTF-8, the reader decodes it back).
fn class_info(internal: &str, d1: Vec<u8>, d2: Vec<String>) -> ClassInfo {
    class_info_kind(internal, d1, d2, None)
}

fn class_info_kind(internal: &str, d1: Vec<u8>, d2: Vec<String>, kind: Option<i32>) -> ClassInfo {
    let d1_strings = vec![d1.iter().map(|&b| b as char).collect()];
    ClassInfo {
        major: 52,
        access: 0,
        this_class: internal.into(),
        super_class: Some("java/lang/Object".into()),
        interfaces: Vec::<String>::new().into(),
        fields: Vec::new(),
        methods: Vec::new(),
        meta: decode_metadata(&d1_strings, &d2, kind, internal, None, &[]),
        signature: None,
        retention: None,
        kotlin_targets: Vec::new(),
        java_targets: Vec::new(),
        inner_classes: Vec::new(),
    }
}

#[test]
fn class_member_value_params_round_trip() {
    // A class with one member `fun greet(name: String, times: Int): String`.
    let methods = vec![FnMeta::plain(
        "greet".to_string(),
        vec![
            ("name".to_string(), Ty::String),
            ("times".to_string(), Ty::Int),
        ],
        Ty::String,
    )];
    let (d1, d2) = build_class(
        "com/example/Greeter",
        &[("name".to_string(), Ty::String)], // primary ctor
        "(Ljava/lang/String;)V",
        &[],
        &methods,
        &[],
        &ClassTail::default(),
    );
    let ci = class_info("com/example/Greeter", d1, d2);

    let fns = class_functions(&ci);
    let greet = fns
        .iter()
        .find(|f| f.jvm_name == "greet")
        .expect("the decoded class metadata must list the `greet` member");

    // The SOURCE value-parameter types must round-trip — this is what cross-module resolution reads to
    // recover a call's matchable arity (drop any synthetic trailing params the descriptor appends).
    assert_eq!(
        greet.value_params.iter().map(|p| p.ty).collect::<Vec<_>>(),
        vec![
            Some(type_name("kotlin/String")),
            Some(type_name("kotlin/Int"))
        ],
        "build_class → class_functions must preserve each member param's source type"
    );
}

#[test]
fn value_class_constructor_realization_name_round_trips() {
    let (d1, d2) = build_class(
        "sample/Name",
        &[("value".to_string(), Ty::String)],
        "(Ljava/lang/String;)Ljava/lang/String;",
        &[],
        &[],
        &[],
        &ClassTail {
            inline_underlying: Some(("value", Ty::String)),
            ctor_sig_name: Some("constructor-impl"),
            ..Default::default()
        },
    );
    let ci = class_info_kind("sample/Name", d1, d2, Some(1));
    let constructors = class_constructors(&ci);

    assert_eq!(constructors.len(), 1);
    assert_eq!(constructors[0].params.names, ["value"]);
    assert_eq!(constructors[0].params.types, [Ty::String]);
    assert_eq!(constructors[0].jvm_name, "constructor-impl");
    assert_eq!(
        constructors[0].jvm_desc,
        Some("(Ljava/lang/String;)Ljava/lang/String;")
    );
}

#[test]
fn secondary_constructor_default_flags_round_trip() {
    let parameters = vec![
        ("required".to_string(), Ty::String),
        ("fallback".to_string(), Ty::String),
    ];
    let defaults = [false, true];
    let secondary = [CtorMeta {
        params: &parameters,
        param_defaults: &defaults,
        desc: "(Ljava/lang/String;Ljava/lang/String;)V",
        sig_name: None,
        vararg_index: None,
        flags: krusty::metadata::class_builder::SECONDARY_CTOR_FLAGS,
        annotations: &[],
    }];
    let (d1, d2) = build_class(
        "sample/Secondary",
        &[],
        "()V",
        &[],
        &[],
        &[],
        &ClassTail {
            emit_primary_ctor: false,
            secondary_ctors: &secondary,
            ..Default::default()
        },
    );
    let constructors = class_constructors(&class_info("sample/Secondary", d1, d2)).to_vec();

    assert_eq!(constructors.len(), 1);
    assert_eq!(constructors[0].params.names, ["required", "fallback"]);
    assert_eq!(constructors[0].params.defaults, defaults);
}

#[test]
fn class_member_equality_bound_round_trips() {
    let owner = Ty::obj("sample/Base");
    let mut equals = FnMeta::plain(
        "equals".to_string(),
        vec![("other".to_string(), Ty::nullable(Ty::obj("kotlin/Any")))],
        Ty::Boolean,
    );
    equals.equality_bound = Some(owner);
    let (d1, d2) = build_class(
        "sample/Base",
        &[],
        "()V",
        &[],
        &[equals],
        &[],
        &ClassTail::default(),
    );
    let ci = class_info("sample/Base", d1, d2);
    let equals = class_functions(&ci)
        .iter()
        .find(|function| function.jvm_name == "equals")
        .expect("equals metadata");

    assert_eq!(equals.equality_bound, Some(owner));
}

#[test]
fn class_type_parameter_bound_and_variance_round_trip() {
    let parameter = krusty::ir::IrTypeParameter {
        name: "T".to_string(),
        semantic_name: "T".to_string(),
        bounds: vec![(Ty::obj("kotlin/CharSequence"), true)],
        variance: TypeVariance::Out,
    };
    let names = vec!["T".to_string()];
    let (d1, d2) = build_class(
        "com/example/Producer",
        &[],
        "()V",
        &[],
        &[],
        &[],
        &ClassTail {
            type_params: &names,
            type_param_bounds: std::slice::from_ref(&parameter),
            ..Default::default()
        },
    );
    let ci = class_info_kind("com/example/Producer", d1, d2, Some(1));
    assert_eq!(
        ci.meta.class_type_parameters.type_param_variances(),
        &vec![TypeVariance::Out]
    );
    assert_eq!(
        ci.meta.class_type_parameters.type_param_bounds(),
        &vec![vec![Ty::obj("kotlin/CharSequence")]]
    );
}

#[test]
fn inner_member_metadata_maps_captured_and_own_type_parameters_to_distinct_ids() {
    let outer = "outer-semantic".to_string();
    let own = "inner-semantic".to_string();
    let own_parameter = krusty::ir::IrTypeParameter {
        name: "U".to_string(),
        semantic_name: own.clone(),
        bounds: vec![(Ty::obj("kotlin/Any"), false)],
        variance: TypeVariance::Invariant,
    };
    let methods = vec![FnMeta {
        context_count: 0,
        spellings: krusty::spelling::DeclaredSpellings::default(),
        name: "pair".to_string(),
        equality_bound: None,
        params: vec![
            (
                "outer".to_string(),
                Ty::ty_param(&outer, Ty::obj("kotlin/Any")),
            ),
            (
                "inner".to_string(),
                Ty::ty_param(&own, Ty::obj("kotlin/Any")),
            ),
        ],
        ret: Ty::obj_args(
            "kotlin/Pair",
            &[
                Ty::ty_param(&outer, Ty::obj("kotlin/Any")),
                Ty::ty_param(&own, Ty::obj("kotlin/Any")),
            ],
        ),
        type_params: Vec::new(),
        semantic_type_params: Vec::new(),
        type_param_bounds: Vec::new(),
        flags: krusty::metadata::class_builder::DEFAULT_FUNCTION_FLAGS,
        receiver: None,
        params_have_defaults: false,
        param_defaults: Vec::new(),
        vararg_index: None,
        jvm_sig: None,
        jvm_sig_name: None,
        annotations: Vec::new(),
        param_annotations: Vec::new(),
        no_infer_params: Vec::new(),
    }];
    let own_names = vec!["U".to_string()];
    let captured = vec![outer];
    let (d1, d2) = build_class(
        "sample/Outer$Inner",
        &[],
        "(Lsample/Outer;)V",
        &[],
        &methods,
        &[],
        &ClassTail {
            type_params: &own_names,
            type_param_bounds: std::slice::from_ref(&own_parameter),
            captured_type_params: &captured,
            ..Default::default()
        },
    );
    let ci = class_info_kind("sample/Outer$Inner", d1, d2, Some(1));
    assert_eq!(ci.meta.class_type_parameters.type_params(), &["U"]);
    let pair = class_functions(&ci)
        .iter()
        .find(|function| function.jvm_name == "pair")
        .expect("pair metadata");
    let signature = pair.generic_sig.as_ref().expect("generic member signature");
    assert!(matches!(
        signature.params[0],
        Ty::TyParam("outer-semantic", _)
    ));
    assert!(matches!(signature.params[1], Ty::TyParam("U", _)));
}

#[test]
fn nested_inner_metadata_numbers_captures_from_outermost_to_innermost() {
    let outer = "outer-semantic".to_string();
    let middle = "middle-semantic".to_string();
    let own = "inner-semantic".to_string();
    let bound = Ty::obj("kotlin/Any");
    let own_parameter = krusty::ir::IrTypeParameter {
        name: "V".to_string(),
        semantic_name: own.clone(),
        bounds: vec![(bound, false)],
        variance: TypeVariance::Invariant,
    };
    let parameter = |name: &str| (name.to_string(), Ty::ty_param(name, bound));
    let methods = vec![FnMeta {
        context_count: 0,
        spellings: krusty::spelling::DeclaredSpellings::default(),
        name: "triple".to_string(),
        equality_bound: None,
        params: vec![parameter(&outer), parameter(&middle), parameter(&own)],
        ret: Ty::Unit,
        type_params: Vec::new(),
        semantic_type_params: Vec::new(),
        type_param_bounds: Vec::new(),
        flags: krusty::metadata::class_builder::DEFAULT_FUNCTION_FLAGS,
        receiver: None,
        params_have_defaults: false,
        param_defaults: Vec::new(),
        vararg_index: None,
        jvm_sig: None,
        jvm_sig_name: None,
        annotations: Vec::new(),
        param_annotations: Vec::new(),
        no_infer_params: Vec::new(),
    }];
    let own_names = vec!["V".to_string()];
    let captured = vec![outer, middle];
    let (d1, d2) = build_class(
        "sample/Outer$Middle$Inner",
        &[],
        "(Lsample/Outer$Middle;)V",
        &[],
        &methods,
        &[],
        &ClassTail {
            type_params: &own_names,
            type_param_bounds: std::slice::from_ref(&own_parameter),
            captured_type_params: &captured,
            ..Default::default()
        },
    );
    let ci = class_info_kind("sample/Outer$Middle$Inner", d1, d2, Some(1));
    let signature = class_functions(&ci)
        .iter()
        .find(|function| function.jvm_name == "triple")
        .and_then(|function| function.generic_sig.as_ref())
        .expect("triple metadata signature");
    assert!(matches!(
        signature.params[0],
        Ty::TyParam("outer-semantic", _)
    ));
    assert!(matches!(
        signature.params[1],
        Ty::TyParam("middle-semantic", _)
    ));
    assert!(matches!(signature.params[2], Ty::TyParam("V", _)));
}

#[test]
fn package_value_param_defaults_round_trip() {
    use krusty::metadata::builder::{build_package, FnMeta as PkgFnMeta};
    // A top-level `fun host(a: String, b: Int = 7): String` — only `b` DECLARES_DEFAULT_VALUE. The
    // per-parameter default flags must survive `build_package` → `package_functions`, so a dependent
    // module can omit `b` (the reader's `metadata_param_defaults` drives classpath default-omission).
    let funcs = vec![PkgFnMeta {
        spellings: krusty::spelling::DeclaredSpellings::default(),
        annotations: Vec::new(),
        decl_order: 0,
        jvm_name: None,
        name: "host".to_string(),
        equality_bound: None,
        params: vec![("a".to_string(), Ty::String), ("b".to_string(), Ty::Int)],
        ret: Ty::String,
        receiver: None,
        param_defaults: vec![false, true],
        suspend: false,
        jvm_desc: None,
        contract: None,
        inline: false,
        operator: false,
        infix: false,
        type_params: Vec::new(),
        semantic_type_params: Vec::new(),
        type_param_bounds: Vec::new(),
        context_count: 0,
        vararg_index: None,
        visibility: krusty::types::Visibility::Public,
        param_annotations: Vec::new(),
        no_infer_params: Vec::new(),
    }];
    let (d1, d2) = build_package(&funcs, &[], &[], None);
    let ci = class_info("com/example/HostKt", d1, d2);

    let fns = package_functions(&ci);
    let host = fns
        .iter()
        .find(|f| f.kotlin_name == "host")
        .expect("the decoded package metadata must list `host`");
    assert_eq!(
        host.value_params
            .iter()
            .map(|p| p.has_default())
            .collect::<Vec<_>>(),
        vec![false, true],
        "build_package → package_functions must preserve each param's DECLARES_DEFAULT_VALUE flag"
    );
}

#[test]
fn package_function_type_parameter_bound_round_trips() {
    use krusty::jvm::metadata::package_functions;
    use krusty::metadata::builder::{build_package, FnMeta as PkgFnMeta};
    let t = Ty::ty_param("T", Ty::obj("kotlin/CharSequence"));
    let funcs = vec![PkgFnMeta {
        spellings: krusty::spelling::DeclaredSpellings::default(),
        annotations: Vec::new(),
        decl_order: 0,
        jvm_name: None,
        name: "identity".to_string(),
        equality_bound: None,
        params: vec![("value".to_string(), t)],
        ret: t,
        receiver: None,
        param_defaults: Vec::new(),
        suspend: false,
        jvm_desc: Some("(Ljava/lang/CharSequence;)Ljava/lang/CharSequence;".to_string()),
        contract: None,
        inline: false,
        operator: false,
        infix: false,
        type_params: vec![("T".to_string(), false)],
        semantic_type_params: vec!["T".to_string()],
        type_param_bounds: vec![vec![Ty::obj("kotlin/CharSequence")]],
        context_count: 0,
        vararg_index: None,
        visibility: krusty::types::Visibility::Public,
        param_annotations: Vec::new(),
        no_infer_params: Vec::new(),
    }];
    let (d1, d2) = build_package(&funcs, &[], &[], None);
    let ci = class_info("com/example/HostKt", d1, d2);
    let function = package_functions(&ci)
        .iter()
        .find(|function| function.kotlin_name == "identity")
        .expect("identity metadata");
    assert_eq!(
        function
            .generic_sig
            .as_ref()
            .expect("generic signature")
            .formal_bounds
            .as_slice(),
        &[vec![Ty::obj("kotlin/CharSequence")]],
    );
}

#[test]
fn package_extension_receiver_round_trips() {
    use krusty::jvm::metadata::package_functions;
    use krusty::metadata::builder::{build_package, FnMeta as PkgFnMeta};
    // An extension `fun NavGraphBuilder.composable(route: String): Unit` — the receiver must be recorded
    // as `Function.receiver_type`, NOT a value parameter, so the decoded LOGICAL arity is 1 (just
    // `route`), not 2. Without this a dependent counts the receiver as an argument and can't resolve a
    // `builder.composable("x")` call.
    let funcs = vec![PkgFnMeta {
        spellings: krusty::spelling::DeclaredSpellings::default(),
        annotations: Vec::new(),
        decl_order: 0,
        jvm_name: None,
        name: "composable".to_string(),
        equality_bound: None,
        params: vec![("route".to_string(), Ty::String)],
        ret: Ty::Unit,
        receiver: Some(Ty::obj("androidx/navigation/NavGraphBuilder")),
        param_defaults: Vec::new(),
        suspend: false,
        jvm_desc: None,
        contract: None,
        inline: false,
        operator: true,
        infix: false,
        type_params: Vec::new(),
        semantic_type_params: Vec::new(),
        type_param_bounds: Vec::new(),
        context_count: 0,
        vararg_index: None,
        visibility: krusty::types::Visibility::Public,
        param_annotations: Vec::new(),
        no_infer_params: Vec::new(),
    }];
    let (d1, d2) = build_package(&funcs, &[], &[], None);
    let ci = class_info("com/example/NavGraphBuilderKt", d1, d2);

    let f = package_functions(&ci)
        .iter()
        .find(|f| f.kotlin_name == "composable")
        .expect("the decoded package metadata must list `composable`");
    assert!(
        f.is_extension(),
        "the receiver_type must mark it an extension"
    );
    assert!(
        f.is_operator(),
        "the function flags must preserve the operator convention"
    );
    assert_eq!(
        f.receiver_class,
        Some(type_name("androidx/navigation/NavGraphBuilder")),
        "the extension receiver class must round-trip"
    );
    assert_eq!(
        f.value_params.len(),
        1,
        "only the logical value param `route` is recorded — the receiver is NOT a value parameter"
    );
}

#[test]
fn package_receiver_function_type_param_round_trips() {
    use krusty::jvm::metadata::package_functions;
    use krusty::metadata::builder::{build_package, FnMeta as PkgFnMeta};
    // `fun NavHost(builder: NGB.() -> Unit)` — the `builder` param is a RECEIVER function type. Its
    // metadata Type must carry @ExtensionFunctionType + the receiver as the first type argument, so a
    // dependent recognizes a lambda passed to `builder` binds `this` to NGB (drives classpath lambda_recv).
    let funcs = vec![PkgFnMeta {
        spellings: krusty::spelling::DeclaredSpellings::default(),
        annotations: Vec::new(),
        decl_order: 0,
        jvm_name: None,
        name: "NavHost".to_string(),
        equality_bound: None,
        params: vec![(
            "builder".to_string(),
            Ty::fun_with_shape(
                vec![Ty::obj("androidx/navigation/NavGraphBuilder")],
                Ty::Unit,
                0,
                true,
                false,
            ),
        )],
        ret: Ty::Unit,
        receiver: None,
        param_defaults: Vec::new(),
        suspend: false,
        jvm_desc: None,
        contract: None,
        inline: false,
        operator: false,
        infix: false,
        type_params: Vec::new(),
        semantic_type_params: Vec::new(),
        type_param_bounds: Vec::new(),
        context_count: 0,
        vararg_index: None,
        visibility: krusty::types::Visibility::Public,
        param_annotations: Vec::new(),
        no_infer_params: Vec::new(),
    }];
    let (d1, d2) = build_package(&funcs, &[], &[], None);
    let ci = class_info("com/example/NavHostKt", d1, d2);

    let f = package_functions(&ci)
        .iter()
        .find(|f| f.kotlin_name == "NavHost")
        .expect("the decoded package metadata must list `NavHost`");
    assert_eq!(
        f.value_params
            .iter()
            .map(|p| p.recv_fun_receiver)
            .collect::<Vec<_>>(),
        vec![Some(type_name("androidx/navigation/NavGraphBuilder"))],
        "the receiver-function-type param's @ExtensionFunctionType + receiver class must round-trip"
    );
}
