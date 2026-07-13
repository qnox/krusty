//! A type parameter whose upper bound is ANOTHER type parameter of the same declaration
//! (`fun <T1 : C, T2 : T1> foo(x: T2)`) must erase `T2` to the transitive class bound `C`, so a member
//! access on `x` (`x.x`) resolves instead of erasing to `Any`. The bound chain is followed through
//! intermediate type parameters (cycle-guarded). Same-file, runnable.
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn transitive_type_parameter_bound_resolves_member() {
    const SRC: &str = "class C(val x: String)\n\
        fun <T1 : C, T2 : T1> foo(x: T2): String = x.x\n\
        fun box(): String = foo(C(\"OK\"))\n";
    assert_eq!(run(SRC).expect("transitive bound member"), "OK");
}

#[test]
fn three_level_transitive_bound_resolves_member() {
    const SRC: &str = "class C(val v: String)\n\
        fun <A : C, B : A, D : B> foo(x: D): String = x.v\n\
        fun box(): String = foo(C(\"OK\"))\n";
    assert_eq!(run(SRC).expect("three-level transitive bound"), "OK");
}
