//! kotlinc mangles a suspend function's JVM name from its ORIGINAL signature, which carries a trailing
//! `Continuation` value parameter (a non-inline `_` element). So a suspend `create(id: Id)` mangles
//! differently from the non-suspend overload. krusty omitted the `Continuation` element, producing the
//! wrong hash (and one that collided with the non-suspend mangle). This drives a suspend value-class
//! method end-to-end (same-file call → the decl and the call must agree on the mangled name).

use super::common;

fn run(tag: &str, main: &str) -> Option<String> {
    let jdk = common::jdk_modules()?;
    let sl = common::stdlib_jar()?;
    let coro = common::coroutines_jar()?;
    let lo = common::compile_lib(
        tag,
        "package lib\ninterface Dep { suspend fun ping(): Int }\n\
         object D : Dep { override suspend fun ping(): Int = 1 }\n",
    )?;
    common::compile_and_run_box(main, "Main", &[lo, sl, coro, jdk.clone()], Some(&jdk))
}

#[test]
fn suspend_value_class_param_method_runs() {
    // `create` takes a value class `Id` and suspends (calls `D.ping()`), so kotlinc mangles its name.
    // The box() call is same-file — index-resolved — so it must target the same mangled method.
    const MAIN: &str = "import lib.*\n\
        import kotlinx.coroutines.runBlocking\n\
        @JvmInline value class Id(val v: String)\n\
        class C(val d: Dep) { suspend fun create(id: Id): Int { d.ping(); return id.v.length } }\n\
        fun box(): String = runBlocking { if (C(D).create(Id(\"abc\")) == 3) \"OK\" else \"F\" }\n";
    assert_eq!(run("svcm", MAIN).expect("suspend value-class method"), "OK");
}
