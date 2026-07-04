//! build.722 dd1: a `suspend` CLASS METHOD with a ctor-call DEFAULT parameter, calling another suspend
//! member — `class Service(val repo){ suspend fun list(f: Filt = Filt()) = repo.find(f) }`. Two coupled
//! bugs:
//!
//!  1. The CHECKER never type-checked a class METHOD's default-parameter expressions (only `check_fun`
//!     did top-level ones), so a NON-literal member default (`f: Filt = Filt()`) typed `Error` and the
//!     `$default` stub lowering bailed with "call Filt". `check_method` now types member defaults.
//!  2. The coroutine `box_returns` pass didn't handle an `ExternalStaticField` node (reading a classpath
//!     `object`'s `INSTANCE`, e.g. `Service(R)` where `R` is a classpath object) → the suspend lambda's
//!     state machine bailed. `box_returns` now treats it as a leaf value.
use super::common;

const LIB: &str = "package lib\n\
    data class Filt(val a: Int = 0)\n\
    interface Repo { suspend fun find(f: Filt): Int }\n\
    object R : Repo { override suspend fun find(f: Filt): Int = f.a + 1 }\n";

fn run(tag: &str, main: &str) -> Option<String> {
    let jdk = common::jdk_modules()?;
    let sl = common::stdlib_jar()?;
    let coro = common::coroutines_jar()?;
    let lo = common::compile_lib(tag, LIB)?;
    common::compile_and_run_box(main, "Main", &[lo, sl, coro, jdk.clone()], Some(&jdk))
}

#[test]
fn suspend_member_ctor_default_calling_suspend_member() {
    const MAIN: &str = "import lib.*\n\
        import kotlinx.coroutines.runBlocking\n\
        class Service(val repo: Repo) { suspend fun list(f: Filt = Filt()) = repo.find(f) }\n\
        fun box(): String = runBlocking { if (Service(R).list() == 1) \"OK\" else \"F\" }\n";
    assert_eq!(
        run("dd1_722", MAIN).expect("suspend member ctor-default"),
        "OK"
    );
}

#[test]
fn nonsuspend_member_ctor_default() {
    // The isolated CHECKER fix: a non-suspend member with a ctor-call default (`Filt()`) now records the
    // default's type so the `$default` stub lowers.
    const MAIN: &str = "import lib.*\n\
        class S { fun list(f: Filt = Filt()): Int = f.a }\n\
        fun box(): String = if (S().list() == 0) \"OK\" else \"F\"\n";
    assert_eq!(
        run("dd1_722_ns", MAIN).expect("nonsuspend member ctor-default"),
        "OK"
    );
}
