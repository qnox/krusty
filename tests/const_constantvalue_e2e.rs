//! `const val` byte-parity with kotlinc: a compile-time-literal `const val` field carries a
//! `ConstantValue` attribute (the JVM initializes it), and when ALL statics are so folded the facade has
//! NO `<clinit>` at all — exactly kotlinc's output (previously krusty emitted no `ConstantValue` and a
//! `<clinit>` with `putstatic`). Verified by parsing the emitted facade class.

use super::common;

use krusty::jvm::classreader::parse_class;

fn facade(src: &str) -> krusty::jvm::classreader::ClassInfo {
    let sl = common::stdlib_jar();
    let jh = common::java_home();
    let jdk = jh
        .as_ref()
        .map(|h| std::path::PathBuf::from(format!("{h}/lib/modules")));
    let cp: Vec<std::path::PathBuf> = sl.into_iter().collect();
    let classes =
        common::compile_in_process(src, "Main", &cp, jdk.as_deref()).expect("const file compiles");
    let (_, bytes) = classes
        .iter()
        .find(|(n, _)| n.ends_with("MainKt"))
        .expect("facade class emitted");
    parse_class(bytes).expect("facade parses")
}

#[test]
fn const_field_has_constantvalue_and_no_clinit() {
    let ci = facade("const val X = \"OK\"\nconst val N = 42\nfun box() = X\n");
    let x = ci.fields.iter().find(|f| f.name == "X").expect("X field");
    assert!(
        x.const_value.is_some(),
        "const val X must carry a ConstantValue attribute"
    );
    let n = ci.fields.iter().find(|f| f.name == "N").expect("N field");
    assert!(
        n.const_value.is_some(),
        "const val N must carry a ConstantValue attribute"
    );
    assert!(
        ci.method("<clinit>", "()V").is_none(),
        "an all-const-folded facade must have NO <clinit> (kotlinc emits none)"
    );
}
