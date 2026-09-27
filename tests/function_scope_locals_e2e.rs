//! A function's own locals stay in scope through its implicit `return`, as kotlinc's are: their
//! `LocalVariableTable` ranges end with the method. A local of a nested block ends with that block.
use super::common;

#[test]
fn a_functions_locals_cover_its_implicit_return() {
    let src = "fun run1(f: () -> Unit) = f()\n\
               fun sink(a: Any?) {}\n\
               fun plain() { val x = \"a\"; sink(x) }\n\
               fun nested(c: Boolean) { if (c) { val y = 1; sink(y) }; val z = 2; sink(z) }\n\
               fun lambda() { run1 { val q = \"b\"; sink(q) } }\n\
               class Owner { fun member() { val x = 1; sink(x) } }\n";
    for class in ["FunctionScopeLocalsKt", "Owner"] {
        common::byte_diff_against_kotlinc_cp(
            "FunctionScopeLocals",
            src,
            class,
            &[common::stdlib_jar()],
        )
        .expect("the reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc:\n{diff}"));
    }
}
