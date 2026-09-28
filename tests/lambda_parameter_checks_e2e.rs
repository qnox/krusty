//! kotlinc's `generateNonNullAssertions` skips a private function unless it is a lambda literal's
//! local function (`LOCAL_FUNCTION_FOR_LAMBDA`), which guards each non-null reference parameter of
//! its own with `Intrinsics.checkNotNullParameter`: an extension receiver as `$this$<label>`, a
//! bare `_` as `<unused var>`, an escaped `` `_` `` by its name. Captured values are not guarded,
//! and an anonymous function (`fun(…) {}`) guards nothing. A bare `_` has no `LocalVariableTable`
//! entry.
use super::common;

const SOURCE: &str = "package store\n\
    \n\
    class UserKlass(val name: String)\n\
    fun interface Action { fun run(u: UserKlass): Int }\n\
    \n\
    fun takePair(f: (UserKlass, UserKlass) -> String): String = \"\"\n\
    fun takeReceiver(f: UserKlass.(String) -> Unit) {}\n\
    fun takeAction(a: Action): Int = 0\n\
    \n\
    class Checks {\n\
    \x20   fun named(): Any = { u: UserKlass -> }\n\
    \x20   fun underscore(): Any = { _: UserKlass -> }\n\
    \x20   fun mixed(): String = takePair { _, u -> u.name }\n\
    \x20   fun escaped(): String = takePair { `_`, u -> u.name }\n\
    \x20   fun receiver() = takeReceiver { s -> }\n\
    \x20   fun captured(c: String, n: String?): Any = { u: UserKlass -> c.length + (n?.length ?: 0) }\n\
    \x20   fun nullable(): Any = { u: UserKlass?, i: Int -> }\n\
    \x20   fun sam(): Int = takeAction { u -> 1 }\n\
    \x20   fun anonymous(): Any = fun(_: UserKlass, u: UserKlass) {}\n\
    \x20   fun anonymousReceiver(): Any = fun UserKlass.(s: String) {}\n\
    }\n\
    \n\
    data class Entry(val key: String, val count: Int)\n\
    fun takeUnit(f: (Unit, UserKlass) -> Int): Int = 0\n\
    fun takeEntry(f: (Entry) -> String): String = \"\"\n\
    \n\
    class UnitChecks {\n\
    \x20   fun unit(): Int = takeUnit { u, k -> k.name.length }\n\
    \x20   fun unitOnly(): Any = { u: Unit -> }\n\
    }\n\
    \n\
    class DestructuredChecks {\n\
    \x20   fun destructured(): String = takeEntry { (key, count) -> key + count }\n\
    \x20   fun partly(): String = takeEntry { (_, count) -> count.toString() }\n\
    }\n";

fn assert_identical(class: &str) {
    common::byte_diff_against_kotlinc_cp(
        "LambdaParameterChecks",
        SOURCE,
        class,
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|error| panic!("{class} byte-identical to kotlinc: {error}"));
}

#[test]
fn lambda_parameter_checks_are_byte_identical_to_kotlinc() {
    assert_identical("store/Checks");
}

/// `Unit` erases to a reference (`Lkotlin/Unit;`), so kotlinc guards a non-null `Unit` parameter
/// like any other reference parameter.
#[test]
fn unit_lambda_parameters_are_guarded_like_kotlinc() {
    assert_identical("store/UnitChecks");
}

/// A destructured parameter is guarded as `<destruct>`, has no `LocalVariableTable` row, and its
/// component calls carry no line of their own.
#[test]
fn destructured_lambda_parameters_are_guarded_like_kotlinc() {
    assert_identical("store/DestructuredChecks");
}
