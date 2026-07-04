//! `when (s) { A -> … }` over a sealed subject, matching an *object* subtype by value (`==`), not `is`.
//! The checker's when-comparability rule rejected a condition whose type isn't promotable to the
//! subject's; an object subtype (`object A : S()`) is a valid `==` operand against `s: S` (one type is a
//! subtype of the other), so `when_objs_comparable` now permits it. Round-tripped on the JVM.

mod common;

fn run(src: &str) -> Option<String> {
    let sl = common::stdlib_jar()?;
    let jdk = common::jdk_modules()?;
    common::compile_and_run_box(src, "Main", &[sl], Some(&jdk))
}

#[test]
fn sealed_object_value_match() {
    const SRC: &str = "sealed class S\nobject A : S()\nobject B : S()\n\
fun f(s: S): String = when (s) { A -> \"O\"; B -> \"K\" }\n\
fun box(): String = f(A) + f(B)\n";
    assert_eq!(
        run(SRC).expect("sealed-object value-match compiles + runs"),
        "OK"
    );
}
