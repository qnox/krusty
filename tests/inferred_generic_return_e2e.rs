//! Inferred generic return: `fun <T> id(x: T): T` called WITHOUT an explicit type argument and
//! WITHOUT an expected type, then used directly (`id("hi").length`). The result type is inferred from
//! the actual argument (`String`), so members of the result resolve — matching kotlinc, which erases
//! `T` to `Object` in the signature but knows the call's static type is `String`. Previously the result
//! erased to `Any` and the member read failed ("unresolved member 'length' on 'kotlin/Any'").

mod common;

#[test]
fn inferred_generic_return_member_resolves() {
    let Some(java_home) = common::java_home() else {
        eprintln!("skipping inferred_generic_return_e2e: set JAVA_HOME");
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping inferred_generic_return_e2e: no kotlin-stdlib jar found");
        return;
    };
    // `id("hi").length` — return inferred String, `.length` resolves to 2.
    // `firstOf` — return inferred from the first arg.
    // `pick` with a chained `.uppercase()` on the inferred String result.
    let src = "fun <T> id(x: T): T = x\n\
fun <T> firstOf(a: T, b: T): T = a\n\
fun box(): String {\n\
if (id(\"hi\").length != 2) return \"f1\"\n\
if (firstOf(\"AB\", \"CD\").length != 2) return \"f2\"\n\
if (id(\"ok\").uppercase() != \"OK\") return \"f3\"\n\
return \"OK\"\n\
}\n";
    let jdk = std::path::PathBuf::from(format!("{java_home}/lib/modules"));
    // The toolchain IS present (guards above), so a `None` here means krusty declined to COMPILE the
    // inferred-generic-return program — that is the regression under test, so fail loudly, don't skip.
    let out = common::compile_and_run_box(src, "G", &[stdlib], Some(&jdk))
        .expect("krusty must compile an inferred generic return used directly (id(\"hi\").length)");
    assert_eq!(out, "OK");
}
