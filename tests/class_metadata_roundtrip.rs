//! Writer→reader round-trip for CLASS `@Metadata`: `metadata::class_builder::build_class` encodes a
//! class's member functions (with their SOURCE `value_parameter` types), and `metadata::class_functions`
//! decodes them back. This is the contract cross-module resolution relies on — a dependent module reads
//! a lib class's member signatures (their source arity) from the lib's emitted `@Metadata`, exactly as
//! `Classpath::metadata_call_facts` does for a classpath callee. `build_class` existed but was unwired
//! and untested; this pins the round-trip before it is wired into emit.

use krusty::jvm::classreader::ClassInfo;
use krusty::jvm::metadata::{class_functions, package_functions};
use krusty::metadata::class_builder::{build_class, FnMeta};
use krusty::types::{type_name, Ty};

/// Wrap built `(d1_bytes, d2)` into a `ClassInfo` the reader consumes. `d1` is the protobuf payload with
/// one byte per `char` (the constant pool writes it as modified-UTF-8, the reader decodes it back).
fn class_info(internal: &str, d1: Vec<u8>, d2: Vec<String>) -> ClassInfo {
    ClassInfo {
        major: 52,
        access: 0,
        this_class: internal.into(),
        super_class: Some("java/lang/Object".into()),
        interfaces: Vec::<String>::new().into(),
        fields: Vec::new(),
        methods: Vec::new(),
        kotlin_d1: vec![d1.iter().map(|&b| b as char).collect()],
        kotlin_d2: d2,
        signature: None,
        retention: None,
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
        0,
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
fn package_value_param_defaults_round_trip() {
    use krusty::metadata::builder::{build_package, FnMeta as PkgFnMeta};
    // A top-level `fun host(a: String, b: Int = 7): String` — only `b` DECLARES_DEFAULT_VALUE. The
    // per-parameter default flags must survive `build_package` → `package_functions`, so a dependent
    // module can omit `b` (the reader's `metadata_param_defaults` drives classpath default-omission).
    let funcs = vec![PkgFnMeta {
        name: "host".to_string(),
        params: vec![("a".to_string(), Ty::String), ("b".to_string(), Ty::Int)],
        ret: Ty::String,
        receiver: None,
        param_fun_recvs: Vec::new(),
        param_defaults: vec![false, true],
        suspend: false,
        jvm_desc: None,
    }];
    let (d1, d2) = build_package(&funcs, &[]);
    let ci = class_info("com/example/HostKt", d1, d2);

    let fns = package_functions(&ci);
    let host = fns
        .iter()
        .find(|f| f.kotlin_name == "host")
        .expect("the decoded package metadata must list `host`");
    assert_eq!(
        host.value_params
            .iter()
            .map(|p| p.has_default)
            .collect::<Vec<_>>(),
        vec![false, true],
        "build_package → package_functions must preserve each param's DECLARES_DEFAULT_VALUE flag"
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
        name: "composable".to_string(),
        params: vec![("route".to_string(), Ty::String)],
        ret: Ty::Unit,
        receiver: Some(Ty::obj("androidx/navigation/NavGraphBuilder")),
        param_fun_recvs: Vec::new(),
        param_defaults: Vec::new(),
        suspend: false,
        jvm_desc: None,
    }];
    let (d1, d2) = build_package(&funcs, &[]);
    let ci = class_info("com/example/NavGraphBuilderKt", d1, d2);

    let f = package_functions(&ci)
        .into_iter()
        .find(|f| f.kotlin_name == "composable")
        .expect("the decoded package metadata must list `composable`");
    assert!(
        f.is_extension,
        "the receiver_type must mark it an extension"
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
        name: "NavHost".to_string(),
        params: vec![("builder".to_string(), Ty::obj("kotlin/Function1"))],
        ret: Ty::Unit,
        receiver: None,
        param_fun_recvs: vec![Some(Ty::obj("androidx/navigation/NavGraphBuilder"))],
        param_defaults: Vec::new(),
        suspend: false,
        jvm_desc: None,
    }];
    let (d1, d2) = build_package(&funcs, &[]);
    let ci = class_info("com/example/NavHostKt", d1, d2);

    let f = package_functions(&ci)
        .into_iter()
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
