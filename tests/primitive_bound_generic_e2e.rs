//! A function type parameter with a primitive upper bound is specialized to that primitive, matching
//! kotlinc's callable descriptor and runtime semantics.

use super::common;

fn expect_classes(src: &str) -> Vec<(String, Vec<u8>)> {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    common::expect_compile_in_process(src, "P", &[stdlib], Some(jdk.as_path()))
}

#[test]
fn integral_bounded_type_param_specializes_to_primitive_descriptor() {
    let src =
        "fun <T : Int> idi(t: T): T = t\nfun box(): String = if (idi(3) == 3) \"OK\" else \"no\"\n";
    let cs = expect_classes(src);
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
        let stdlib = common::stdlib_jar();
        assert_eq!(
            common::run_box(&cs, &box_class, &[stdlib]).as_deref(),
            Some("OK")
        );
    }
}

#[test]
fn char_bounded_type_param_runs() {
    let src = "fun <T : Char> idc(c: T): T = c\nfun box(): String = if (idc('K') == 'K') \"OK\" else \"no\"\n";
    let cs = expect_classes(src);
    let pkt = cs
        .iter()
        .find(|(n, _)| n.ends_with("PKt"))
        .map(|(_, b)| krusty::jvm::classreader::parse_class(b).unwrap())
        .unwrap();
    assert!(pkt.method("idc", "(C)C").is_some(), "expected (C)C");
}

#[test]
fn double_bounded_type_param_specializes_and_runs() {
    let src = "fun <T : Double> idd(d: T): T = d\nfun box(): String = if (idd(1.0) == 1.0) \"OK\" else \"no\"\n";
    let cs = expect_classes(src);
    let pkt = cs
        .iter()
        .find(|(name, _)| name.ends_with("PKt"))
        .map(|(_, bytes)| krusty::jvm::classreader::parse_class(bytes).unwrap())
        .unwrap();
    assert!(pkt.method("idd", "(D)D").is_some(), "expected (D)D");
    let box_class = common::find_box_class(&cs).expect("box class");
    let stdlib = common::stdlib_jar();
    assert_eq!(
        common::run_box(&cs, &box_class, &[stdlib]).as_deref(),
        Some("OK")
    );
}
