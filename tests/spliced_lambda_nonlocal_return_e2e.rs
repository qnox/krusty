//! A non-local `return` out of a lambda that suspends and is spliced into its caller's frame.
//!
//! `repeat(n) { … api.get() … return v }` is one frame once the splice happens: the suspension
//! belongs to the enclosing suspend function, and so does the `return`. The emit-time coroutine
//! machine declined any spliced body holding a `return`, on the belief that such a return was not
//! boxed to the CPS `Object` result — but `box_returns` walks a lambda's retained `inline_body`
//! and had been boxing it all along. The decline only meant the enclosing function was emitted
//! with no continuation to pass, which the backend then reported as `call arity mismatch`.
//!
//! Each callee here really suspends (`yield()`), so the resume path — not just verification — runs.
use super::common;

const LIB: &str = "package lib\n\
    interface Api { suspend fun get(id: String): String? }\n";

fn run(tag: &str, main: &str) -> Option<String> {
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    let coro = common::coroutines_jar();
    let lo = common::compile_lib(tag, LIB)?;
    common::compile_and_run_box(
        main,
        "Main",
        &[lo, sl, coro, jdk.clone()],
        Some(jdk.as_path()),
    )
}

#[test]
fn a_reference_return_leaves_a_spliced_suspending_lambda() {
    // The corpus shape: poll a suspending API inside `repeat` and leave the whole function as soon
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
            repeat(5) {\n\
                val v = api.get(id)\n\
                if (v != null) return v\n\
            }\n\
            return null\n\
        }\n\
        fun box(): String {\n\
            val v = runBlocking { poll(Late(3), \"OK\") }\n\
            return v ?: \"F: null\"\n\
        }\n";
    assert_eq!(
        run("nonlocal_ref", MAIN).expect("a reference non-local return out of a spliced lambda"),
        "OK"
    );
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
            repeat(4) {\n\
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
    assert_eq!(
        run("nonlocal_fallthrough", MAIN)
            .expect("a spliced lambda whose non-local return never fires"),
        "OK"
    );
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
            repeat(5) { i ->\n\
                if (api.get(id) != null) return i + 1\n\
            }\n\
            return -1\n\
        }\n\
        fun box(): String {\n\
            val n = runBlocking { attempts(Late(2), \"x\") }\n\
            return if (n == 2) \"OK\" else \"F: \" + n\n\
        }\n";
    assert_eq!(
        run("nonlocal_int", MAIN).expect("a primitive non-local return out of a spliced lambda"),
        "OK"
    );
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
            repeat(5) {\n\
                seen = seen + 1\n\
                if (api.get(id) != null) return\n\
            }\n\
        }\n\
        fun box(): String {\n\
            runBlocking { drain(Late(3), \"x\") }\n\
            return if (seen == 3) \"OK\" else \"F: \" + seen\n\
        }\n";
    assert_eq!(
        run("nonlocal_unit", MAIN).expect("a bare non-local return out of a spliced lambda"),
        "OK"
    );
}
