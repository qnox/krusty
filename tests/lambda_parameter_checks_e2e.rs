//! kotlinc's `generateNonNullAssertions` skips a private function unless it is a lambda literal's
//! local function (`LOCAL_FUNCTION_FOR_LAMBDA`), which guards each non-null reference parameter of
//! its own with `Intrinsics.checkNotNullParameter`: an extension receiver as `<this>`, a bare `_`
//! as `<unused var>`, an escaped `` `_` `` by its name. Captured values are not guarded, and an
//! anonymous function (`fun(…) {}`) guards nothing. A bare `_` has no `LocalVariableTable` entry.
use super::common;

const SOURCE: &str = "package store\n\
    \n\
    class UserKlass(val name: String)\n\
    fun interface Action { fun run(u: UserKlass): Int }\n\
    \n\
    fun take(f: (UserKlass, UserKlass) -> String): String = \"\"\n\
    fun takeReceiver(f: UserKlass.(String) -> Unit) {}\n\
    fun takeAction(a: Action): Int = 0\n\
    \n\
    class Checks {\n\
    \x20   fun named(): Any = { u: UserKlass -> }\n\
    \x20   fun underscore(): Any = { _: UserKlass -> }\n\
    \x20   fun mixed(): String = take { _, u -> u.name }\n\
    \x20   fun escaped(): String = take { `_`, u -> u.name }\n\
    \x20   fun receiver() = takeReceiver { s -> }\n\
    \x20   fun captured(c: String, n: String?): Any = { u: UserKlass -> c.length + (n?.length ?: 0) }\n\
    \x20   fun nullable(): Any = { u: UserKlass?, i: Int -> }\n\
    \x20   fun sam(): Int = takeAction { u -> 1 }\n\
    \x20   fun anonymous(): Any = fun(_: UserKlass, u: UserKlass) {}\n\
    \x20   fun anonymousReceiver(): Any = fun UserKlass.(s: String) {}\n\
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
