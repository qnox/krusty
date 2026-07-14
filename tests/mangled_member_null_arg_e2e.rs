//! Overload selection must accept a `null` (or `Nothing`) argument for a REFERENCE parameter of a
//! value-class-param (name-MANGLED) classpath member — `p.exec(id, actId, null)` where the member has a
//! value-class parameter (so its JVM name is mangled) and a nullable reference parameter (`params:
//! String?`). The descriptor-form matcher rejected `null` (it is neither an exact descriptor match nor an
//! `Obj` to widen through the hierarchy), so the whole member failed to resolve ("unresolved method
//! 'exec'"). Round-tripped on the JVM.

use super::common;

const LIB: &str = "package lib\n\
    @JvmInline value class A(val v: String)\n\
    data class R(val ok: Boolean)\n\
    interface P { fun f(a: A, params: String?): R }\n\
    object Impl : P { override fun f(a: A, params: String?): R = R(params == null) }\n";

fn run(tag: &str, main: &str) -> Option<String> {
    let jdk = common::jdk_modules()?;
    let sl = common::stdlib_jar()?;
    let lo = common::compile_lib(tag, LIB)?;
    common::compile_and_run_box(main, "Main", &[lo, sl, jdk.clone()], Some(&jdk))
}

#[test]
fn null_literal_arg_resolves_mangled_member() {
    const MAIN: &str = "import lib.*\n\
        fun box(): String {\n\
            val p: P = Impl\n\
            val nullArg = p.f(A(\"x\"), null)\n\
            val strArg = p.f(A(\"x\"), \"y\")\n\
            return if (nullArg.ok && !strArg.ok) \"OK\" else \"FAIL\"\n\
        }\n";
    assert_eq!(
        run("mm_null", MAIN).expect("null arg to a mangled member resolves + runs"),
        "OK"
    );
}
