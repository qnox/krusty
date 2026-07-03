//! build.702 dd1: a `suspend` member call whose result feeds an `if`/`when` CONDITION, where the callee
//! is a classpath `suspend` member with a DEFAULTED parameter (so the call goes through the `$default`
//! synthetic). Two coupled bugs:
//!
//!  1. A suspension in an if/when-CONDITION (as opposed to a bound `val`) was not hoisted to a preceding
//!     temp, so the coroutine state-machine builder bailed the whole function ("IR-backend bail").
//!  2. A `suspend` `$default` descriptor erases its return to `Object`; the hoisted temp was typed by that
//!     erased return, so a following `t == 5` compared an `Object` against an int → VerifyError. The
//!     hoisted temp now carries the member's LOGICAL return (`Int`), matching the manually-bound
//!     `val t = s.list()` path, so `bind_from_r` unboxes it.
//!
//! Faithful shape: `class Service(...) { suspend fun list(f: Filt = Filt()): Int }` — a suspend method on a
//! class with a constructor, taking a param defaulted to a constructor call, called with the arg omitted.
use std::path::PathBuf;
mod common;

const LIB: &str = "package lib\n\
    class Filt(val n: Int = 5)\n\
    class Service(val base: Int) {\n\
    \x20 suspend fun list(f: Filt = Filt()): Int = base + f.n\n\
    }\n";

fn run(tag: &str, main: &str) -> Option<String> {
    let jdk = common::jdk_modules()?;
    let sl = common::stdlib_jar()?;
    let coro =
        PathBuf::from("target/cache/kotlinc/2.4.0/kotlinc/lib/kotlinx-coroutines-core-jvm.jar");
    if !coro.exists() {
        return None;
    }
    let lo = common::compile_lib(tag, LIB)?;
    common::compile_and_run_box(main, "Main", &[lo, sl, coro, jdk.clone()], Some(&jdk))
}

#[test]
fn suspend_default_call_in_if_condition() {
    // `s.list()` omits the defaulted `f` → `$default` call, whose erased `Object` result feeds `== 15`.
    const MAIN: &str = "import lib.Service\n\
        import kotlinx.coroutines.runBlocking\n\
        suspend fun g(s: Service): Int = if (s.list() == 15) 1 else 0\n\
        fun box(): String {\n\
        \x20 val r = runBlocking { g(Service(10)) }\n\
        \x20 return if (r == 1) \"OK\" else \"fail: $r\"\n\
        }\n";
    assert_eq!(
        run("dd1_ifcond", MAIN).expect("suspend $default in if-cond"),
        "OK"
    );
}

#[test]
fn suspend_default_call_manually_bound() {
    // The manually-bound baseline: `val t = s.list()` (declared `Int`), then `t == 15`.
    const MAIN: &str = "import lib.Service\n\
        import kotlinx.coroutines.runBlocking\n\
        suspend fun g(s: Service): Int { val t = s.list(); return if (t == 15) 1 else 0 }\n\
        fun box(): String {\n\
        \x20 val r = runBlocking { g(Service(10)) }\n\
        \x20 return if (r == 1) \"OK\" else \"fail: $r\"\n\
        }\n";
    assert_eq!(
        run("dd1_bound", MAIN).expect("suspend $default bound"),
        "OK"
    );
}

#[test]
fn suspend_default_call_in_runblocking_lambda_condition() {
    // The suspension in an `if`-condition inside a `runBlocking { }` lambda tail.
    const MAIN: &str = "import lib.Service\n\
        import kotlinx.coroutines.runBlocking\n\
        fun box(): String = runBlocking {\n\
        \x20 val s = Service(10)\n\
        \x20 if (s.list() == 15) \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(
        run("dd1_rb", MAIN).expect("suspend $default in runBlocking if-cond"),
        "OK"
    );
}
