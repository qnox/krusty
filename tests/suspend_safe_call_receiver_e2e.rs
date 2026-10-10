//! A suspend call behind `?.` on a receiver read from a local. The coroutine transform saves every
//! value already on the operand stack when a suspension point's `beforeInlineCall` bracket opens,
//! so the receiver must not be left there by a `dup` ahead of the bracket: the frame computer then
//! met an unsteppable stack at the call's continuation label and the compile panicked.
use super::common;

const LIB: &str = "package lib\n\
    interface Sweeper {\n\
    \x20 suspend fun sweep(): Unit\n\
    \x20 suspend fun name(): String\n\
    }\n\
    suspend fun Sweeper.touch(): Unit {}\n";

const MAIN: &str = "import lib.Sweeper\n\
    import lib.touch\n\
    import kotlinx.coroutines.runBlocking\n\
    var log = \"\"\n\
    class Impl : Sweeper {\n\
    \x20 override suspend fun sweep() { log += \"s\" }\n\
    \x20 override suspend fun name() = \"impl\"\n\
    }\n\
    suspend fun unit(s: Sweeper?) { s?.sweep() }\n\
    suspend fun value(s: Sweeper?): String? = s?.name()\n\
    suspend fun extension(s: Sweeper?) { s?.touch(); log += \"e\" }\n\
    suspend fun chained(s: Sweeper?): Int = s?.name()?.length ?: -1\n\
    fun box(): String = runBlocking {\n\
    \x20 unit(Impl()); unit(null); extension(Impl()); extension(null)\n\
    \x20 val result = \"$log ${value(Impl())} ${value(null)} ${chained(Impl())} ${chained(null)}\"\n\
    \x20 if (result == \"see impl null 4 -1\") \"OK\" else \"fail: $result\"\n\
    }\n";

#[test]
fn a_suspend_safe_call_keeps_its_receiver_inside_the_suspension_bracket() {
    let jdk = common::jdk_modules();
    let lib = common::compile_lib("suspend_safe_call_receiver", LIB).expect("library compiles");
    let answer = common::compile_and_run_box(
        MAIN,
        "Main",
        &[
            lib,
            common::stdlib_jar(),
            common::coroutines_jar(),
            jdk.clone(),
        ],
        Some(jdk.as_path()),
    );
    assert_eq!(answer.as_deref(), Some("OK"));

    // Run the same nullable/non-null member, extension, Unit, value, and chained cases through the
    // selected kotlinc release. Keep the dependency reference-built for both compiler comparisons
    // below so this oracle isolates the safe-call coroutine emission in MAIN.
    let reference_lib = common::kotlinc_library(LIB).expect("reference library compiles");
    let coroutines = common::coroutines_jar();
    let reference_classpath = [reference_lib, coroutines];
    let reference = common::kotlinc_box_result_with_classpath(MAIN, &reference_classpath);
    assert_eq!(reference, "OK");
    assert_eq!(answer.as_deref(), Some(reference.as_str()));

    // The optimizer is expected to fold the conservative receiver temporary into kotlinc's final
    // shape. Compare complete normalized instruction streams, not just successful verification.
    let pair = common::ModuleClassPair::compile_with_classpath(
        &[("Main.kt", MAIN)],
        &reference_classpath,
        "MainKt",
    );
    for method in ["unit", "value", "extension", "chained"] {
        let (kotlinc, krusty) = pair.method_code("MainKt", method);
        assert_eq!(krusty, kotlinc, "{method} instruction stream");
    }
}
