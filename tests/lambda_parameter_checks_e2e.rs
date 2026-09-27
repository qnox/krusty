//! kotlinc's `generateNonNullAssertions` skips a private function unless it is a lambda literal's
//! local function (`LOCAL_FUNCTION_FOR_LAMBDA`), which guards each non-null reference parameter of
//! its own with `Intrinsics.checkNotNullParameter`: an extension receiver as `<this>`, a bare `_`
//! as `<unused var>`, a destructured parameter as `<destruct>`, and an escaped `` `_` `` by its
//! name. Captured values are not guarded, and an anonymous function (`fun(…) {}`) guards nothing.
//! A bare `_` has no `LocalVariableTable` entry.
use super::common;

const SOURCE: &str = "package store\n\
    \n\
    class UserKlass(val code: Int)\n\
    class Pair2(val first: Int, val second: Int) {\n\
    \x20   operator fun component1(): Int = first\n\
    \x20   operator fun component2(): Int = second\n\
    }\n\
    fun interface Action { fun run(u: UserKlass): Int }\n\
    \n\
    fun acceptPair(f: (UserKlass, UserKlass) -> Int): Int = 0\n\
    fun acceptReceiver(f: UserKlass.(UserKlass) -> Unit) {}\n\
    fun acceptAction(a: Action): Int = 0\n\
    fun acceptUnit(f: (Unit) -> Int): Int = 0\n\
    fun acceptDestructured(f: (Pair2) -> Int): Int = 0\n\
    \n\
    class Checks {\n\
    \x20   fun named(): Any = { u: UserKlass -> }\n\
    \x20   fun underscore(): Any = { _: UserKlass -> }\n\
    \x20   fun mixed(): Int = acceptPair { _, u -> u.code }\n\
    \x20   fun escaped(): Int = acceptPair { `_`, u -> u.code }\n\
    \x20   fun receiver() = acceptReceiver { s -> }\n\
    \x20   fun unit(): Int = acceptUnit { u -> 1 }\n\
    \x20   fun destructured(): Int = acceptDestructured { (a, b) -> a + b }\n\
    \x20   fun captured(c: UserKlass, n: UserKlass?): Any = { u: UserKlass -> c.code + (n?.code ?: 0) }\n\
    \x20   fun nullable(): Any = { u: UserKlass?, i: Int -> }\n\
    \x20   fun sam(): Int = acceptAction { u -> 1 }\n\
    \x20   fun anonymous(): Any = fun(_: UserKlass, u: UserKlass) {}\n\
    \x20   fun anonymousReceiver(): Any = fun UserKlass.(s: UserKlass) {}\n\
    }\n";

#[test]
fn lambda_parameter_checks_are_byte_identical_to_kotlinc() {
    common::byte_diff_against_kotlinc_cp(
        "LambdaParameterChecks",
        SOURCE,
        "store/Checks",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .expect("store/Checks byte-identical to kotlinc");
}
