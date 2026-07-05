//! build.840 mm1: a SAFE CALL to a lambda-taking extension whose lambda reads a member of the
//! (non-null) receiver — `c?.takeIf { it.at > 0 }`. The safe-call path typed the lambda argument
//! naively (no expected parameter type), so `it` defaulted to `Any` and `it.at` failed with
//! "unresolved member 'at' on kotlin/Any". The lambda is now typed against the extension's block
//! parameter bound by the NON-NULL receiver (as the non-safe path does), so `it` is the receiver type.
//! `?.let`/`?.also`/`?.run` already worked (they route through the scope-function path); this covers
//! `takeIf`/`takeUnless` and any other lambda extension reached by `?.`.
use super::common;

const LIB: &str = "package lib\n\
    data class Ch(val at: Int, val active: Boolean)\n\
    object M { fun mk(n: Int, a: Boolean): Ch = Ch(n, a) }\n";

fn run(tag: &str, main: &str) -> Option<String> {
    let jdk = common::jdk_modules()?;
    let sl = common::stdlib_jar()?;
    let lo = common::compile_lib(tag, LIB)?;
    common::compile_and_run_box(main, "Main", &[lo, sl, jdk.clone()], Some(&jdk))
}

#[test]
fn safe_call_take_if_reads_member() {
    const MAIN: &str = "import lib.*\n\
        fun box(): String {\n\
        \x20 val c: Ch? = M.mk(5, true)\n\
        \x20 val r = c?.takeIf { it.at > 0 }\n\
        \x20 return if (r?.at == 5) \"OK\" else \"F\"\n\
        }\n";
    assert_eq!(
        run("mm1_takeif", MAIN).expect("safe-call takeIf reads member"),
        "OK"
    );
}

#[test]
fn safe_call_take_if_filters_to_null() {
    const MAIN: &str = "import lib.*\n\
        fun box(): String {\n\
        \x20 val c: Ch? = M.mk(-1, true)\n\
        \x20 val r = c?.takeIf { it.at > 0 }\n\
        \x20 return if (r == null) \"OK\" else \"F\"\n\
        }\n";
    assert_eq!(
        run("mm1_null", MAIN).expect("safe-call takeIf filters"),
        "OK"
    );
}

#[test]
fn safe_call_take_unless_reads_member() {
    const MAIN: &str = "import lib.*\n\
        fun box(): String {\n\
        \x20 val c: Ch? = M.mk(5, false)\n\
        \x20 val r = c?.takeUnless { it.active }\n\
        \x20 return if (r?.at == 5) \"OK\" else \"F\"\n\
        }\n";
    assert_eq!(
        run("mm1_unless", MAIN).expect("safe-call takeUnless reads member"),
        "OK"
    );
}

#[test]
fn chained_safe_call_take_if_then_member() {
    const MAIN: &str = "import lib.*\n\
        fun box(): String {\n\
        \x20 val c: Ch? = M.mk(7, true)\n\
        \x20 val n: Int? = c?.takeIf { it.at > 0 }?.at\n\
        \x20 return if (n == 7) \"OK\" else \"F\"\n\
        }\n";
    assert_eq!(
        run("mm1_chain", MAIN).expect("chained safe-call takeIf + member"),
        "OK"
    );
}
