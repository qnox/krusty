//! Top-level `const val` codegen: kotlinc emits a `public static final` field with a `ConstantValue`
//! attribute (no `<clinit>` store), and reads are direct `getstatic` field accesses — never accessor
//! (`getX()`) calls. (Cross-file const reads — lowered to `ExternalStaticField`/`getstatic` of the
//! public field rather than a `getX()` call — are exercised by the conformance gate's
//! `properties/const/anotherFile.kt`.) Round-tripped on the JVM.

mod common;

fn run(src: &str) -> Option<String> {
    let jh = common::java_home()?;
    let sl = common::stdlib_jar()?;
    let jdk = std::path::PathBuf::from(format!("{jh}/lib/modules"));
    common::compile_and_run_box(src, "Main", &[sl], Some(&jdk))
}

#[test]
fn same_file_const_read() {
    const SRC: &str = "const val MSG = \"OK\"\nfun box(): String = MSG\n";
    assert_eq!(
        run(SRC).expect("same-file const read compiles + runs"),
        "OK"
    );
}

#[test]
fn const_concat_at_use_site() {
    const SRC: &str = "const val A = \"O\"\nconst val B = \"K\"\nfun box(): String = A + B\n";
    assert_eq!(run(SRC).expect("const concat compiles + runs"), "OK");
}
