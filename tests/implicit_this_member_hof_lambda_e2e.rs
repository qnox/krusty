//! An implicit-`this` member higher-order call (`update { … }` reached with no explicit receiver,
//! inside a member or extension body) must pre-type its lambda argument from the member's declared
//! function-type parameter. Otherwise a no-parameter lambda (`{ null }`) adopts the erased zero-arg
//! form and fails the arity check against the parameter's `(T?) -> T?`. Same-file, runs on the JVM.
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn no_param_lambda_to_implicit_this_member_adopts_param_arity() {
    // `apply1 { "OK" }` is an implicit-`this` member call inside `go()`; the lambda declares no
    // parameter yet the member's `(Int) -> String` parameter supplies the arity. Before the fix the
    // lambda defaulted to the zero-arg form and failed the arity check.
    const SRC: &str = "\
class C {\n\
    fun apply1(f: (Int) -> String): String = f(5)\n\
    fun go(): String = apply1 { \"OK\" }\n\
}\n\
fun box(): String = C().go()\n";
    assert_eq!(run(SRC).expect("implicit-this member HOF lambda"), "OK");
}

#[test]
fn implicit_this_member_hof_types_it() {
    // Regression companion: the pre-typed lambda parameter must carry the real element type so `it`
    // resolves a member — here `it.length` on a `String` element.
    const SRC: &str = "\
class Box(val v: String) {\n\
    fun transform(f: (String) -> Int): Int = f(v)\n\
    fun run(): Int = transform { it.length }\n\
}\n\
fun box(): String {\n\
    return if (Box(\"OK!\").run() == 3) \"OK\" else \"FAIL\"\n\
}\n";
    assert_eq!(run(SRC).expect("implicit-this member HOF it-typing"), "OK");
}
