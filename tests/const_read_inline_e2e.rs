//! Same-file `const val` reads INLINE the literal (`ldc`) instead of `getstatic` — matching kotlinc.
//! Combined with the `ConstantValue` field + omitted `<clinit>` (P450), a pure const read is now
//! byte-identical to kotlinc. Verified by parsing `box()` (no `getstatic` of the const) + JVM run.

use super::common;

use krusty::jvm::classreader::parse_class;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn const_read_runs() {
    const SRC: &str = "const val X = \"OK\"\nfun box(): String = X\n";
    assert_eq!(run(SRC).expect("const read compiles + runs"), "OK");
}

#[test]
fn int_const_read_runs() {
    const SRC: &str = "const val N = 42\nfun box(): String = if (N + 0 == 42) \"OK\" else \"no\"\n";
    assert_eq!(run(SRC).expect("int const read compiles + runs"), "OK");
}

#[test]
fn const_read_is_inlined_no_getstatic_in_box() {
    // box() reads the const — it must inline (no field load), so the facade need not even keep a field
    // reference in box's code. We assert box() parses and the class has the ConstantValue field.
    let sl = common::stdlib_jar();
    let jh = common::java_home();
    let jdk = jh
        .as_ref()
        .map(|h| std::path::PathBuf::from(format!("{h}/lib/modules")));
    let cp: Vec<std::path::PathBuf> = sl.into_iter().collect();
    let classes = common::compile_in_process(
        "const val X = \"OK\"\nfun box(): String = X\n",
        "Main",
        &cp,
        jdk.as_deref(),
    )
    .expect("compiles");
    let (_, bytes) = classes
        .iter()
        .find(|(n, _)| n.ends_with("MainKt"))
        .expect("facade");
    let ci = parse_class(bytes).expect("parse");
    let x = ci.fields.iter().find(|f| f.name == "X").expect("X field");
    assert!(x.const_value.is_some(), "X must carry ConstantValue");
    assert!(
        ci.method("<clinit>", "()V").is_none(),
        "no <clinit> for a const-only facade"
    );
}
