//! Top-level `const val` codegen: kotlinc emits a `public static final` field with a `ConstantValue`
//! attribute (no `<clinit>` store), and reads are direct `getstatic` field accesses — never accessor
//! (`getX()`) calls. (Cross-file const reads — lowered to `ExternalStaticField`/`getstatic` of the
//! public field rather than a `getX()` call — are exercised by the conformance gate's
//! `properties/const/anotherFile.kt`.) Round-tripped on the JVM.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
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
