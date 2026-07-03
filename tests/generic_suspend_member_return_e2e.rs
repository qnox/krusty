//! aa1: a GENERIC classpath `suspend` member whose return is a type parameter (`interface Repo<T> {
//! suspend fun byId(id: Int): T? }`) called on a concrete receiver (`Repo<Cfg>`). The non-suspend member
//! path binds `T` from the receiver's type argument (`member_return`), but the suspend path recovered its
//! return from the `Continuation<T>` generic signature with NO substitution — so `T` erased to `Any`, and
//! `r.byId(1) ?: error(…)` then `c.at` failed with "member 'at' on 'kotlin/Any'" (the root of the reported
//! `member … on Any` cascade). `receiver_type_bindings` now computes the receiver→declaring-class type
//! bindings and substitutes them into the recovered suspend return, so `byId` types as `Cfg?`.
//!
//! The suspend variants assert LOWERING (a coroutine RUN needs a driver); one full RUN goes through
//! `runBlocking` when the coroutines runtime jar is available.
use std::path::PathBuf;
mod common;

fn coroutines_jar() -> Option<PathBuf> {
    let kc = std::env::var("KRUSTY_KOTLINC").ok()?;
    let jar = PathBuf::from(kc)
        .parent()?
        .parent()?
        .join("lib")
        .join("kotlinx-coroutines-core-jvm.jar");
    jar.exists().then_some(jar)
}

const LIB: &str = "package lib\n\
    class Cfg(val at: String)\n\
    interface Repo<T> { suspend fun byId(id: Int): T? }\n\
    class RealRepo : Repo<Cfg> {\n\
    \x20 override suspend fun byId(id: Int): Cfg? = Cfg(\"OK\")\n\
    }\n";

#[test]
fn generic_suspend_nullable_return_binds_receiver_type_argument() {
    // Checker-level: the recovered return binds `T = Cfg`, so `?: error(…)` then `c.at` resolves.
    let Some(diags) = common::checker_diags_against(
        "aa1_check",
        LIB,
        "import lib.Repo\nimport lib.Cfg\n\
         suspend fun f(r: Repo<Cfg>): String { val c = r.byId(1) ?: error(\"missing\"); return c.at }\n\
         fun box(): String = \"OK\"\n",
    ) else {
        return;
    };
    assert_eq!(
        diags,
        Vec::<String>::new(),
        "byId(): T? must bind T=Cfg, not erase to Any"
    );
}

#[test]
fn generic_suspend_nonnull_return_binds_receiver_type_argument() {
    // The non-null `T` return (no elvis) must also bind `T = Cfg`.
    let Some(diags) = common::checker_diags_against(
        "aa1_nn",
        "package lib\nclass Cfg(val at: String)\ninterface Repo<T> { suspend fun byId(id: Int): T }\n",
        "import lib.Repo\nimport lib.Cfg\n\
         suspend fun f(r: Repo<Cfg>): String { val c = r.byId(1); return c.at }\n\
         fun box(): String = \"OK\"\n",
    ) else {
        return;
    };
    assert_eq!(diags, Vec::<String>::new(), "byId(): T must bind T=Cfg");
}

#[test]
fn generic_suspend_return_runs_via_runblocking() {
    // Full end-to-end: build the impl with kotlinc, call `byId` inside `runBlocking`, dereference the
    // recovered `Cfg` after `?: error(…)`.
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    let Some(sl) = common::stdlib_jar() else {
        return;
    };
    let Some(corou) = coroutines_jar() else {
        return;
    };
    let Some(libout) = common::compile_lib("aa1_run", LIB) else {
        return;
    };
    let cp: Vec<PathBuf> = vec![libout, sl, corou, jdk.clone()];
    const MAIN: &str = "import lib.Repo\nimport lib.RealRepo\nimport lib.Cfg\n\
        import kotlinx.coroutines.runBlocking\n\
        fun box(): String = runBlocking {\n\
        \x20 val r: Repo<Cfg> = RealRepo()\n\
        \x20 val c = r.byId(1) ?: error(\"missing\")\n\
        \x20 c.at\n\
        }\n";
    assert_eq!(
        common::compile_and_run_box(MAIN, "Main", &cp, Some(&jdk))
            .expect("generic suspend return runs"),
        "OK"
    );
}
