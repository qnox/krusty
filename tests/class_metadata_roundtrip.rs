//! Writer→reader round-trip for CLASS `@Metadata`: `metadata::class_builder::build_class` encodes a
//! class's member functions (with their SOURCE `value_parameter` types), and `metadata::class_functions`
//! decodes them back. This is the contract cross-module resolution relies on — a dependent module reads
//! a lib class's member signatures (their source arity) from the lib's emitted `@Metadata`, exactly as
//! `Classpath::metadata_kept_params` does for a classpath callee. `build_class` existed but was unwired
//! and untested; this pins the round-trip before it is wired into emit.

use krusty::jvm::classreader::ClassInfo;
use krusty::jvm::metadata::class_functions;
use krusty::metadata::class_builder::{build_class, FnMeta};
use krusty::types::Ty;

/// Wrap built `(d1_bytes, d2)` into a `ClassInfo` the reader consumes. `d1` is the protobuf payload with
/// one byte per `char` (the constant pool writes it as modified-UTF-8, the reader decodes it back).
fn class_info(internal: &str, d1: Vec<u8>, d2: Vec<String>) -> ClassInfo {
    ClassInfo {
        major: 52,
        access: 0,
        this_class: internal.to_string(),
        super_class: Some("java/lang/Object".to_string()),
        interfaces: Vec::new(),
        fields: Vec::new(),
        methods: Vec::new(),
        kotlin_d1: vec![d1.iter().map(|&b| b as char).collect()],
        kotlin_d2: d2,
        signature: None,
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
        greet.value_param_types,
        vec![
            Some("kotlin/String".to_string()),
            Some("kotlin/Int".to_string())
        ],
        "build_class → class_functions must preserve each member param's source type"
    );
}
