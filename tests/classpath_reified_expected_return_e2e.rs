//! A classpath reified extension whose `T` comes only from the EXPECTED type.
//!
//! Shape (every HTTP-client call site): `suspend inline fun <reified T> Resp.body(): T` on the
//! classpath, consumed as `fun readPlan(r: Resp): Plan = r.body()` — nothing but the declared
//! return determines `T`. krusty left `T` unbound and returned the erased `Any` ("return type
//! mismatch: expected 'Plan', actual 'Any'"), the dominant sub-shape of the corpus's 1900-error
//! return-mismatch pile.
use super::common;

fn run(tag: &str, lib: &str, main: &str) -> Option<String> {
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    let libout = common::compile_lib(tag, lib)?;
    common::compile_and_run_box(main, "Main", &[libout, sl], Some(jdk.as_path()))
}

const LIB: &str = "package lib\n\
    class Resp(val payload: Any)\n\
    inline fun <reified T> Resp.body(): T = payload as T\n";

const SUSPEND_LIB: &str = "package lib\n\
    class Resp(val payload: Any)\n\
    suspend inline fun <reified T> Resp.body(): T = payload as T\n";

#[test]
fn declared_return_binds_classpath_reified_extension() {
    const MAIN: &str = "import lib.Resp\n\
        import lib.body\n\
        class Plan(val name: String)\n\
        fun readPlan(r: Resp): Plan = r.body()\n\
        fun box(): String = if (readPlan(Resp(Plan(\"p\"))).name == \"p\") \"OK\" else \"fail\"\n";
    assert_eq!(
        run("cre1", LIB, MAIN).expect("classpath reified expected return"),
        "OK"
    );
}

#[test]
fn emitted_reified_extension_publishes_kotlinc_metadata() {
    assert_eq!(
        common::metadata_diff_against_kotlinc_cp("Lib", LIB, "lib/LibKt", &[])
            .expect("kotlinc is required for metadata comparison"),
        Ok(())
    );
}

#[test]
fn emitted_suspend_reified_extension_publishes_kotlinc_metadata() {
    assert_eq!(
        common::metadata_diff_against_kotlinc_cp("Lib", SUSPEND_LIB, "lib/LibKt", &[])
            .expect("kotlinc is required for metadata comparison"),
        Ok(())
    );
}

#[test]
fn suspend_declared_return_reifies_from_krusty_dependency() {
    // The motivating HTTP-client declaration is suspend. Its CPS realization must preserve the
    // expected-type binding through the same inline handoff instead of direct-calling the marker
    // body after adding the continuation parameter.
    const MAIN: &str = "import kotlin.coroutines.*\n\
        import lib.Resp\n\
        import lib.body\n\
        class Plan(val name: String)\n\
        fun <T> runBlocking(block: suspend () -> T): T {\n\
        \x20   var result: Result<T>? = null\n\
        \x20   block.startCoroutine(Continuation(EmptyCoroutineContext) { result = it })\n\
        \x20   return result!!.getOrThrow()\n\
        }\n\
        suspend fun readPlan(r: Resp): Plan = r.body()\n\
        fun box(): String = runBlocking {\n\
        \x20   if (readPlan(Resp(Plan(\"s\"))).name == \"s\") \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(
        run("cre5", SUSPEND_LIB, MAIN)
            .expect("suspend reified splice from krusty-built dependency"),
        "OK"
    );
}

#[test]
fn bounded_reified_formal_refines_past_its_bound() {
    // `<reified T : Number>` erases the unlearned return to Number, not Any — the refinement must
    // still fire.
    const BLIB: &str = "package lib\n\
        class Resp(val payload: Any)\n\
        inline fun <reified T : Number> Resp.nbody(): T = payload as T\n";
    const MAIN: &str = "import lib.Resp\n\
        import lib.nbody\n\
        fun readInt(r: Resp): Int = r.nbody()\n\
        fun box(): String = if (readInt(Resp(41)) + 1 == 42) \"OK\" else \"fail\"\n";
    assert_eq!(
        run("cre4", BLIB, MAIN).expect("bounded reified formal"),
        "OK"
    );
}

#[test]
fn local_annotation_binds_classpath_reified_extension() {
    // The `val x: T = r.body()` spelling of the same channel.
    const MAIN: &str = "import lib.Resp\n\
        import lib.body\n\
        class Plan(val name: String)\n\
        fun box(): String {\n\
        \x20   val p: Plan = Resp(Plan(\"q\")).body()\n\
        \x20   return if (p.name == \"q\") \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(
        run("cre2", LIB, MAIN).expect("annotated local binds reified extension"),
        "OK"
    );
}
