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
    Some(common::expect_box_run(
        main,
        "Main",
        &[libout, sl],
        Some(jdk.as_path()),
    ))
}

/// The REFERENCE-compiled variant: kotlinc's inline body carries reification MARKERS, so a direct
/// call to it throws at runtime — only a real splice with the selected `T` survives. A krusty-built
/// lib body has no marker and would mask a missing binding hand-off.
fn run_ref(tag: &str, lib: &str, main: &str) -> Option<String> {
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    let libout = common::compile_lib_ref(tag, lib)?;
    common::compile_and_run_box(main, "Main", &[libout, sl], Some(jdk.as_path()))
}

const LIB: &str = "package lib\n\
    class Resp(val payload: Any)\n\
    inline fun <reified T> Resp.body(): T = payload as T\n";

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
fn declared_return_reifies_against_kotlinc_marker_body() {
    // Same shape over the kotlinc-compiled lib: the marker body must be SPLICED with the selected
    // `T`, never direct-called.
    const MAIN: &str = "import lib.Resp\n\
        import lib.body\n\
        class Plan(val name: String)\n\
        fun readPlan(r: Resp): Plan = r.body()\n\
        fun box(): String = if (readPlan(Resp(Plan(\"p\"))).name == \"p\") \"OK\" else \"fail\"\n";
    assert_eq!(
        run_ref("cre3", LIB, MAIN).expect("reified splice against kotlinc marker body"),
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
