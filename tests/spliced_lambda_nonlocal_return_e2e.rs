//! A non-local `return` out of a lambda that suspends and is spliced into its caller's frame.
//!
//! `visit(n) { … api.get() … return v }` is one frame once the splice happens: the suspension
//! belongs to the enclosing suspend function, and so does the `return`. The emit-time coroutine
//! machine declined any spliced body holding a `return`, on the belief that such a return was not
//! boxed to the CPS `Object` result — but `box_returns` walks a lambda's retained `inline_body`
//! and had been boxing it all along. The decline only meant the enclosing function was emitted
//! with no continuation to pass, which the backend then reported as `call arity mismatch`.
//!
//! Each callee here really suspends (`yield()`), so the resume path — not just verification — runs.
use super::common;
use std::path::PathBuf;
use std::sync::OnceLock;

const LIB: &str = "package lib\n\
    interface Api { suspend fun get(id: String): String? }\n\
    inline fun <T> visit(count: Int, action: (Int) -> T) {\n\
        var index = 0\n\
        while (index < count) { action(index); index = index + 1 }\n\
    }\n";

fn library() -> &'static PathBuf {
    static LIBRARY: OnceLock<PathBuf> = OnceLock::new();
    LIBRARY.get_or_init(|| {
        common::kotlinc_library(LIB).expect("reference compiler must build the inline fixture")
    })
}

fn classpath() -> (Vec<PathBuf>, PathBuf) {
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    let coro = common::coroutines_jar();
    (vec![library().clone(), sl, coro, jdk.clone()], jdk)
}

fn run(tag: &str, main: &str) -> String {
    let (cp, jdk) = classpath();
    let ours = common::expect_box_run(main, "Main", &cp, Some(jdk.as_path()));
    let reference = common::kotlinc_box_result_with_classpath(
        main,
        &[library().clone(), common::coroutines_jar()],
    );
    assert_eq!(reference, "OK", "{tag}: kotlinc fixture must succeed");
    assert_eq!(ours, reference, "{tag}: krusty differs from kotlinc");
    ours
}

#[test]
fn a_reference_return_leaves_a_spliced_suspending_lambda() {
    // The corpus shape: poll a suspending API inside a generic inline loop and leave the whole function as soon
    // as it answers. Before the fix this file did not compile at all.
    const MAIN: &str = "import lib.*\n\
        import kotlinx.coroutines.runBlocking\n\
        import kotlinx.coroutines.yield\n\
        class Late(private val at: Int) : Api {\n\
            private var n = 0\n\
            override suspend fun get(id: String): String? {\n\
                yield()\n\
                n = n + 1\n\
                return if (n >= at) id else null\n\
            }\n\
        }\n\
        suspend fun poll(api: Api, id: String): String? {\n\
            visit(5) {\n\
                val v = api.get(id)\n\
                if (v != null) return v\n\
            }\n\
            return null\n\
        }\n\
        fun box(): String {\n\
            val v = runBlocking { poll(Late(3), \"OK\") }\n\
            return v ?: \"F: null\"\n\
        }\n";
    assert_eq!(run("nonlocal_ref", MAIN), "OK");
}

#[test]
fn a_return_never_taken_still_falls_through_the_whole_loop() {
    // The other side of the same branch: every iteration suspends, the guard never fires, and the
    // function reaches its own tail return. Counts the iterations so a short-circuit is visible.
    const MAIN: &str = "import lib.*\n\
        import kotlinx.coroutines.runBlocking\n\
        import kotlinx.coroutines.yield\n\
        class Never : Api {\n\
            var calls = 0\n\
            override suspend fun get(id: String): String? {\n\
                yield()\n\
                calls = calls + 1\n\
                return null\n\
            }\n\
        }\n\
        suspend fun poll(api: Api, id: String): String? {\n\
            visit(4) {\n\
                val v = api.get(id)\n\
                if (v != null) return v\n\
            }\n\
            return null\n\
        }\n\
        fun box(): String {\n\
            val api = Never()\n\
            val v = runBlocking { poll(api, \"x\") }\n\
            return if (v == null && api.calls == 4) \"OK\" else \"F: \" + v + \" \" + api.calls\n\
        }\n";
    assert_eq!(run("nonlocal_fallthrough", MAIN), "OK");
}

#[test]
fn a_primitive_return_is_boxed_to_the_cps_result() {
    // The CPS method returns `Object`, so an `Int` leaving through a spliced body has to be boxed;
    // a raw `iload`/`areturn` is a VerifyError ("Bad type on operand stack"), not a wrong answer.
    const MAIN: &str = "import lib.*\n\
        import kotlinx.coroutines.runBlocking\n\
        import kotlinx.coroutines.yield\n\
        class Late(private val at: Int) : Api {\n\
            private var n = 0\n\
            override suspend fun get(id: String): String? {\n\
                yield()\n\
                n = n + 1\n\
                return if (n >= at) id else null\n\
            }\n\
        }\n\
        suspend fun attempts(api: Api, id: String): Int {\n\
            visit(5) { i ->\n\
                if (api.get(id) != null) return i + 1\n\
            }\n\
            return -1\n\
        }\n\
        fun box(): String {\n\
            val n = runBlocking { attempts(Late(2), \"x\") }\n\
            return if (n == 2) \"OK\" else \"F: \" + n\n\
        }\n";
    assert_eq!(run("nonlocal_int", MAIN), "OK");
}

#[test]
fn a_bare_return_leaves_a_unit_returning_suspend_function() {
    // A `Unit` suspend function's bare `return` must `areturn Unit.INSTANCE`, not a void `return`
    // ("Method expects a return value") — the same rule as a return written in the body proper.
    const MAIN: &str = "import lib.*\n\
        import kotlinx.coroutines.runBlocking\n\
        import kotlinx.coroutines.yield\n\
        class Late(private val at: Int) : Api {\n\
            private var n = 0\n\
            override suspend fun get(id: String): String? {\n\
                yield()\n\
                n = n + 1\n\
                return if (n >= at) id else null\n\
            }\n\
        }\n\
        var seen = 0\n\
        suspend fun drain(api: Api, id: String) {\n\
            visit(5) {\n\
                seen = seen + 1\n\
                if (api.get(id) != null) return\n\
            }\n\
        }\n\
        fun box(): String {\n\
            runBlocking { drain(Late(3), \"x\") }\n\
            return if (seen == 3) \"OK\" else \"F: \" + seen\n\
        }\n";
    assert_eq!(run("nonlocal_unit", MAIN), "OK");
}

#[test]
fn a_non_local_return_crossing_finally_keeps_the_exact_fail_closed_diagnostic() {
    const MAIN: &str = "import lib.*\n\
        suspend fun one(): String? = null\n\
        fun consume(value: Int) {}\n\
        suspend fun guarded(): String? {\n\
            visit(1) {\n\
                try {\n\
                    val value = one()\n\
                    if (value != null) return value\n\
                } finally {\n\
                    consume(1)\n\
                }\n\
            }\n\
            return null\n\
        }\n";
    let (cp, jdk) = classpath();
    let outcome = common::backend_outcome_in_process(MAIN, "Main", &cp, Some(jdk.as_path()))
        .expect("the unsupported shape is frontend-valid");
    assert_eq!(
        outcome,
        common::BackendOutcome::Rejected(vec![
            "krusty: this suspend-function shape is not yet supported by the IR backend"
                .to_string()
        ])
    );
}
