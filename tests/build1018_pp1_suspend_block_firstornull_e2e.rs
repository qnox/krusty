//! build.1018 pp1: a `suspend fun` with a BLOCK body binds a suspend-call result to a local and then
//! `return`s an inline-HOF call on it — `val xs = r.list(); return xs.firstOrNull { it.name == "x" }`.
//! The suspend state-machine lowering mis-tracked the type of the suspend-read result bound to `xs`, so
//! the inline `firstOrNull` in return position produced a self-contradictory
//! "type mismatch: inferred type is C but C was expected". The equivalent EXPRESSION body
//! (`= r.list().firstOrNull { … }`) and a non-suspend `firstOrNull` both already worked. The fix
//! propagates the suspend-read result type through the block-local + return path generically.
use super::common;

const LIB: &str = "package lib\n\
    data class C(val name: String)\n\
    interface Repo { suspend fun list(): List<C> }\n\
    object R : Repo { override suspend fun list(): List<C> = listOf(C(\"a\"), C(\"x\"), C(\"b\")) }\n";

fn run(tag: &str, main: &str) -> Option<String> {
    let jdk = common::jdk_modules()?;
    let sl = common::stdlib_jar()?;
    let coro = common::coroutines_jar()?;
    let lo = common::compile_lib(tag, LIB)?;
    common::compile_and_run_box(main, "Main", &[lo, sl, coro, jdk.clone()], Some(&jdk))
}

#[test]
fn suspend_block_local_firstornull_return() {
    // The failing shape: suspend block body, suspend-read bound to a local, inline call on it in return.
    // `box` drives it through `runBlocking` (the suspend result is bound OUTSIDE the builder, so the
    // builder lambda is a single suspend call — keeping the harness off unrelated suspend-lowering limits).
    const MAIN: &str = "import lib.*\n\
        import kotlinx.coroutines.runBlocking\n\
        suspend fun f(r: Repo): C? {\n\
            val xs = r.list()\n\
            return xs.firstOrNull { it.name == \"x\" }\n\
        }\n\
        fun box(): String {\n\
            val c = runBlocking { f(R) }\n\
            return if (c?.name == \"x\") \"OK\" else \"F\"\n\
        }\n";
    assert_eq!(
        run("pp1_block", MAIN).expect("suspend block-local firstOrNull return"),
        "OK"
    );
}

#[test]
fn suspend_block_local_firstornull_no_match_is_null() {
    // The predicate matching nothing collapses to `null` — the return still types as the nullable `C?`.
    const MAIN: &str = "import lib.*\n\
        import kotlinx.coroutines.runBlocking\n\
        suspend fun f(r: Repo): C? {\n\
            val xs = r.list()\n\
            return xs.firstOrNull { it.name == \"zzz\" }\n\
        }\n\
        fun box(): String {\n\
            val c = runBlocking { f(R) }\n\
            return if (c == null) \"OK\" else \"F\"\n\
        }\n";
    assert_eq!(
        run("pp1_null", MAIN).expect("suspend block-local firstOrNull no match"),
        "OK"
    );
}

#[test]
fn suspend_expr_body_firstornull_still_works() {
    // Regression lock for the already-working expression-body variant (routes through the
    // "function body" return-position path that already stripped reference nullability).
    const MAIN: &str = "import lib.*\n\
        import kotlinx.coroutines.runBlocking\n\
        suspend fun f(r: Repo): C? = r.list().firstOrNull { it.name == \"x\" }\n\
        fun box(): String {\n\
            val c = runBlocking { f(R) }\n\
            return if (c?.name == \"x\") \"OK\" else \"F\"\n\
        }\n";
    assert_eq!(
        run("pp1_expr", MAIN).expect("suspend expr-body firstOrNull"),
        "OK"
    );
}
