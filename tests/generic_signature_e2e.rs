//! Generic `Signature` attribute emission: kotlinc emits a JVM generic `Signature` for a
//! type-parameterized function (the descriptor erases the type params; the Signature preserves them).
//! krusty must too, for bytecode parity. A non-generic function gets no Signature. The exact strings
//! are verified byte-identical to kotlinc in the differential harness; here we assert krusty's output.

use std::path::PathBuf;

mod common;

fn classes(src: &str) -> Option<Vec<(String, Vec<u8>)>> {
    let java_home = common::java_home()?;
    let stdlib = common::stdlib_jar()?;
    let jdk = PathBuf::from(format!("{java_home}/lib/modules"));
    common::compile_in_process(src, "G", &[stdlib], Some(&jdk))
}

fn method_signature(cs: &[(String, Vec<u8>)], facade: &str, name: &str) -> Option<String> {
    let ci = cs
        .iter()
        .find(|(n, _)| n.ends_with(facade))
        .map(|(_, b)| krusty::jvm::classreader::parse_class(b).expect("parse"))?;
    ci.methods
        .iter()
        .find(|m| m.name == name)
        .and_then(|m| m.signature.clone())
}

#[test]
fn generic_function_emits_signature() {
    let src = "fun <T> id(t: T): T = t\nfun plain(x: Int): Int = x\n";
    let Some(cs) = classes(src) else {
        return; // toolchain unavailable
    };
    assert_eq!(
        method_signature(&cs, "GKt", "id").as_deref(),
        Some("<T:Ljava/lang/Object;>(TT;)TT;")
    );
    // A non-generic function must NOT carry a Signature attribute.
    assert_eq!(method_signature(&cs, "GKt", "plain"), None);
}

#[test]
fn primitive_bounded_type_param_signature_uses_wrapper() {
    // `<T: Int>` is specialized to descriptor `(I)I`, but its Signature bound is the boxed wrapper.
    let src = "fun <T : Int> idi(t: T): T = t\n";
    let Some(cs) = classes(src) else { return };
    assert_eq!(
        method_signature(&cs, "GKt", "idi").as_deref(),
        Some("<T:Ljava/lang/Integer;>(TT;)TT;")
    );
}
