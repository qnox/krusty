//! build.775 aa1: `suspend` member on an INTERFACE-TYPED PARAMETER returning a nullable type,
//! `?: error("…")`-ed to non-null, then a member accessed on the result —
//! `val c = r.byId("x") ?: error("nf"); c.at` where `r: Repo` is a parameter (not a concrete
//! `object`). Earlier the elvis result typed `Any`, losing the non-null branch type → "member
//! 'at' on Any". This is the interface-param analogue of build.722 aa1 (which used a concrete
//! `object R`); the return-type recovery must work through the interface param too.
use super::common;

const LIB: &str = "package lib\n\
    data class Ch(val at: Int)\n\
    interface Repo { suspend fun byId(id: String): Ch? }\n\
    object R : Repo { override suspend fun byId(id: String): Ch? = if (id == \"x\") Ch(42) else null }\n";

fn run(tag: &str, main: &str) -> Option<String> {
    let jdk = common::jdk_modules()?;
    let sl = common::stdlib_jar()?;
    let coro = common::coroutines_jar()?;
    let lo = common::compile_lib(tag, LIB)?;
    common::compile_and_run_box(main, "Main", &[lo, sl, coro, jdk.clone()], Some(&jdk))
}

#[test]
fn suspend_iface_param_nullable_elvis_error_then_member() {
    const MAIN: &str = "import lib.*\n\
        import kotlinx.coroutines.runBlocking\n\
        suspend fun g(r: Repo): Int { val c = r.byId(\"x\") ?: error(\"nf\"); return c.at }\n\
        fun box(): String = runBlocking { if (g(R) == 42) \"OK\" else \"F\" }\n";
    assert_eq!(
        run("aa1_775", MAIN).expect("suspend iface-param nullable elvis-error member"),
        "OK"
    );
}
