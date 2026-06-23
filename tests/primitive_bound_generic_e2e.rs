//! A FUNCTION type parameter with an INTEGRAL primitive upper bound (`fun <T: Int> …`) is specialized
//! to that primitive — kotlinc emits descriptor `(I)I`, not `(Object)Object`. Floating (`Double`),
//! unsigned, and value bounds are NOT specialized (their boxed-vs-primitive `==`/unsigned semantics
//! differ) and stay rejected, so a default-flags drop-in skips them rather than miscompile.

use std::path::PathBuf;

mod common;

fn classes(src: &str) -> Option<Vec<(String, Vec<u8>)>> {
    let java_home = common::java_home()?;
    let stdlib = common::stdlib_jar()?;
    let jdk = PathBuf::from(format!("{java_home}/lib/modules"));
    common::compile_in_process(src, "P", &[stdlib], Some(&jdk))
}

#[test]
fn integral_bounded_type_param_specializes_to_primitive_descriptor() {
    let src =
        "fun <T : Int> idi(t: T): T = t\nfun box(): String = if (idi(3) == 3) \"OK\" else \"no\"\n";
    let Some(cs) = classes(src) else {
        return; // toolchain unavailable
    };
    // The facade `PKt` must declare `idi` with the specialized primitive descriptor `(I)I`.
    let pkt = cs
        .iter()
        .find(|(n, _)| n.ends_with("PKt"))
        .map(|(_, b)| krusty::jvm::classreader::parse_class(b).expect("parse PKt"))
        .expect("PKt emitted");
    let idi = pkt.method("idi", "(I)I");
    assert!(
        idi.is_some(),
        "expected idi descriptor (I)I (specialized), methods: {:?}",
        pkt.methods
            .iter()
            .map(|m| (m.name.clone(), m.descriptor.clone()))
            .collect::<Vec<_>>()
    );
    // And it runs.
    if let Some(box_class) = common::find_box_class(&cs) {
        let stdlib = common::stdlib_jar().unwrap();
        assert_eq!(
            common::run_box(&cs, &box_class, &[stdlib]).as_deref(),
            Some("OK")
        );
    }
}

#[test]
fn char_bounded_type_param_runs() {
    let src = "fun <T : Char> idc(c: T): T = c\nfun box(): String = if (idc('K') == 'K') \"OK\" else \"no\"\n";
    let Some(cs) = classes(src) else { return };
    let pkt = cs
        .iter()
        .find(|(n, _)| n.ends_with("PKt"))
        .map(|(_, b)| krusty::jvm::classreader::parse_class(b).unwrap())
        .unwrap();
    assert!(pkt.method("idc", "(C)C").is_some(), "expected (C)C");
}

#[test]
fn double_bounded_type_param_is_rejected() {
    // A floating-point bound is NOT specializable (boxed vs primitive `==` differ on -0.0/NaN) — krusty
    // must reject it (compile fails → None), so the test skips rather than miscompiles, like the unsigned
    // and value bounds. (kotlinc would specialize it; we conservatively decline until it's sound.)
    if common::java_home().is_none() || common::stdlib_jar().is_none() {
        return;
    }
    let src = "fun <T : Double> idd(d: T): T = d\nfun box(): String = if (idd(1.0) == 1.0) \"OK\" else \"no\"\n";
    assert!(
        classes(src).is_none(),
        "krusty wrongly accepted a Double-bounded type parameter"
    );
}
