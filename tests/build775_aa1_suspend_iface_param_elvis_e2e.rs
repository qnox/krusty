//! build.775 aa1: the result of a coroutine builder (`runBlocking { r.byId("x") }`) flows OUT into a
//! NON-suspend context, is `?: error("…")`-ed to non-null, then a member is accessed on it —
//! `val c = runBlocking { R.byId("x") } ?: error("nf"); c.at`. The real hit was `change.scheduledAt`
//! (`member … on Any` ×7). Root: `runBlocking<T>(block: suspend () -> T): T` never inferred `T` from the
//! block's body — its `$default` synthetic carries no generic `Signature`, and the suspend-SAM erases the
//! result into `Continuation<T>` (which the lambda argument erases to `Any`). The fix binds `T` from the
//! lambda's own return type via the BASE function's generic signature, so `runBlocking { … }` types as the
//! block's result and the `?:`/member access resolve — and the lowerer `checkcast`/unboxes the erased
//! `Object` return to it. (The earlier inside-a-`suspend fun` variant already worked; kept as a lock.)
use super::common;

const LIB: &str = "package lib\n\
    data class Ch(val at: Int, val name: String)\n\
    interface Repo { suspend fun byId(id: String): Ch? }\n\
    object R : Repo { override suspend fun byId(id: String): Ch? = if (id == \"x\") Ch(42, \"n\") else null }\n";

fn run(tag: &str, main: &str) -> Option<String> {
    let jdk = common::jdk_modules()?;
    let sl = common::stdlib_jar()?;
    let coro = common::coroutines_jar()?;
    let lo = common::compile_lib(tag, LIB)?;
    common::compile_and_run_box(main, "Main", &[lo, sl, coro, jdk.clone()], Some(&jdk))
}

#[test]
fn run_blocking_result_elvis_error_then_member() {
    // The real failing shape: the suspend result leaves `runBlocking` into a non-suspend `box`, is
    // elvis-error'd, then a member is read on it — the `runBlocking { … }` result must type `Ch`, not `Any`.
    const MAIN: &str = "import lib.*\n\
        import kotlinx.coroutines.runBlocking\n\
        fun box(): String {\n\
            val c = runBlocking { R.byId(\"x\") } ?: error(\"nf\")\n\
            return if (c.at == 42 && c.name == \"n\") \"OK\" else \"F\"\n\
        }\n";
    assert_eq!(
        run("aa1_775_rb", MAIN).expect("runBlocking-result elvis-error member"),
        "OK"
    );
}

#[test]
fn run_blocking_infers_value_result_type() {
    // The generic-return inference is not suspend/nullable-specific: `runBlocking { <expr> }` types as the
    // block's result for a plain value too, and the erased `Object` return unboxes to the primitive.
    const MAIN: &str = "import lib.*\n\
        import kotlinx.coroutines.runBlocking\n\
        fun box(): String {\n\
            val n = runBlocking { 40 + 2 }\n\
            val s = runBlocking { \"hi\" }\n\
            return if (n == 42 && s.length == 2) \"OK\" else \"F\"\n\
        }\n";
    assert_eq!(
        run("aa1_775_val", MAIN).expect("runBlocking value result inference"),
        "OK"
    );
}

#[test]
fn suspend_iface_param_nullable_elvis_error_then_member() {
    // The narrower variant (elvis-error + member INSIDE a `suspend fun`, on an interface-typed parameter) —
    // already worked before build.775; kept as a regression lock.
    const MAIN: &str = "import lib.*\n\
        import kotlinx.coroutines.runBlocking\n\
        suspend fun g(r: Repo): Int { val c = r.byId(\"x\") ?: error(\"nf\"); return c.at }\n\
        fun box(): String = runBlocking { if (g(R) == 42) \"OK\" else \"F\" }\n";
    assert_eq!(
        run("aa1_775", MAIN).expect("suspend iface-param nullable elvis-error member"),
        "OK"
    );
}
