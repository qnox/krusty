//! A value-class-param (name-MANGLED) classpath member whose parameter is a NESTED type must resolve.
//! @Metadata spells a nested classifier with `.` after the package (`lib/Flex.FMap`), while a resolved
//! type reference uses the JVM `$` form (`lib/Flex$FMap`) — the same type under two spellings — so the
//! member's parameter never matched its argument ("unresolved method 'exec'"). Overload selection now
//! canonicalizes the nested separator before comparing. Round-tripped on the JVM.

use super::common;

const LIB: &str = "package lib\n\
    @JvmInline value class A(val v: String)\n\
    sealed class Flex { data class FMap(val m: Map<String, String>) : Flex() }\n\
    data class R(val n: Int)\n\
    interface P { fun exec(a: A, params: Flex.FMap?): R }\n\
    object Impl : P { override fun exec(a: A, params: Flex.FMap?): R = R(params?.m?.size ?: -1) }\n";

fn run(tag: &str, main: &str) -> Option<String> {
    let jdk = common::jdk_modules()?;
    let sl = common::stdlib_jar()?;
    let lo = common::compile_lib(tag, LIB)?;
    common::compile_and_run_box(main, "Main", &[lo, sl, jdk.clone()], Some(&jdk))
}

#[test]
fn mangled_member_nested_type_param_resolves() {
    const MAIN: &str = "import lib.*\n\
        fun box(): String {\n\
            val p: P = Impl\n\
            val withMap = p.exec(A(\"x\"), Flex.FMap(mapOf(\"a\" to \"b\")))\n\
            val withNull = p.exec(A(\"x\"), null)\n\
            return if (withMap.n == 1 && withNull.n == -1) \"OK\" else \"FAIL\"\n\
        }\n";
    assert_eq!(
        run("mm_nested", MAIN).expect("mangled member with a nested-type param resolves + runs"),
        "OK"
    );
}
