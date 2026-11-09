//! A compiled library member overloaded on a receiver function type and on a functional
//! interface, called with a lambda.
//!
//! Both overloads take the lambda, so the call's lambda is postponed until one is selected.
//! kotlinc prefers the overload that needs no SAM conversion and checks the lambda against that
//! overload alone: `this` is the function type's receiver, and the interface's `it` does not
//! exist. A library the reference compiler built makes the overloads reach krusty through its
//! classpath provider rather than as source declarations.

use std::path::PathBuf;

use super::common;

const LIB: &str = "package lib\n\
fun interface Action<T> { fun execute(t: T) }\n\
class Handler { var log = \"\"; fun api(s: String) { log += s } }\n\
interface HasDeps {\n\
    fun dependencies(configure: Action<Handler>): String\n\
    fun dependencies(configure: Handler.() -> Unit): String\n\
}\n\
class Deps : HasDeps {\n\
    override fun dependencies(configure: Action<Handler>): String =\n\
        Handler().also { configure.execute(it) }.log + \":action\"\n\
    override fun dependencies(configure: Handler.() -> Unit): String =\n\
        Handler().apply(configure).log + \":function\"\n\
}\n\
object Registry {\n\
    fun register(configure: Action<Handler>): String = Deps().dependencies(configure)\n\
    fun register(configure: Handler.() -> Unit): String = Deps().dependencies(configure)\n\
}\n";

fn classpath() -> Option<Vec<PathBuf>> {
    Some(vec![common::kotlinc_library(LIB)?, common::stdlib_jar()])
}

#[test]
fn a_lambda_is_checked_only_against_the_conversion_free_overload() {
    let Some(classpath) = classpath() else {
        return;
    };
    const MAIN: &str = "import lib.*\n\
fun receiver(deps: HasDeps) { deps.dependencies { val x: Int = this } }\n\
fun implicitParameter(deps: HasDeps) { deps.dependencies { val x: Int = it } }\n\
fun singleton() { Registry.register { val x: Int = this } }\n";
    let result = common::compiler_diagnostics(&[("Main.kt", MAIN)], &classpath);
    common::expect_identical_rejection(&result, "lambda against SAM and function-type overloads");
}

#[test]
fn the_conversion_free_overload_runs_with_its_receiver() {
    let Some(classpath) = classpath() else {
        return;
    };
    const MAIN: &str = "import lib.*\n\
fun box(): String {\n\
    val deps: HasDeps = Deps()\n\
    val member = deps.dependencies { api(\"m\") }\n\
    val singleton = Registry.register { api(\"s\") }\n\
    val converted = deps.dependencies(Action { it.api(\"a\") })\n\
    val result = \"$member $singleton $converted\"\n\
    return if (result == \"m:function s:function a:action\") \"OK\" else result\n\
}\n";
    let result = common::compiler_diagnostics(&[("Main.kt", MAIN)], &classpath);
    assert_eq!(
        result.reference_code, 0,
        "kotlinc rejected the fixture: {}",
        result.reference_stderr
    );
    assert_eq!(
        result.krusty_code, 0,
        "krusty rejected a kotlinc-valid fixture: {}{}",
        result.krusty_stdout, result.krusty_stderr
    );
    let jdk = common::jdk_modules();
    assert_eq!(
        common::expect_box_run(MAIN, "Main", &classpath, Some(jdk.as_path())),
        "OK"
    );
}
