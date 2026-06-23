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
fn generic_member_method_compiles_runs_and_signs() {
    // A member method with its OWN type parameter (`fun <U> wrap(u: U): U`) — previously rejected with
    // "unresolved reference 'U'" because the method's type params weren't in scope for its return type.
    let src = "class Box(val n: Int) {\n  fun <U> wrap(u: U): U = u\n}\nfun box(): String = if (Box(1).wrap(\"OK\") == \"OK\") \"OK\" else \"no\"\n";
    let Some(cs) = classes(src) else {
        return;
    };
    assert_eq!(
        method_signature(&cs, "Box", "wrap").as_deref(),
        Some("<U:Ljava/lang/Object;>(TU;)TU;")
    );
    if let Some(box_class) = common::find_box_class(&cs) {
        let stdlib = common::stdlib_jar().unwrap();
        assert_eq!(
            common::run_box(&cs, &box_class, &[stdlib]).as_deref(),
            Some("OK")
        );
    }
}

#[test]
fn generic_class_emits_class_signature() {
    // `class Box<T>` gets a class-level generic Signature; a non-generic class gets none.
    let src = "class Box<T>(val n: Int)\nclass Plain(val n: Int)\n";
    let Some(cs) = classes(src) else {
        return;
    };
    let class_sig = |name: &str| -> Option<String> {
        cs.iter()
            .find(|(n, _)| n == name)
            .and_then(|(_, b)| krusty::jvm::classreader::parse_class(b).ok())
            .and_then(|ci| ci.signature)
    };
    assert_eq!(
        class_sig("Box").as_deref(),
        Some("<T:Ljava/lang/Object;>Ljava/lang/Object;")
    );
    assert_eq!(class_sig("Plain"), None);
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
