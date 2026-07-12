//! build.1017 oo1: the non-null return type of a classpath method whose parameter is a VALUE CLASS was
//! lost when the result flowed through a safe call into an inline HOF lambda — `r.byHash(h)?.takeIf { it.active }`
//! typed `it` as `kotlin/Any` → "unresolved member 'active' on 'kotlin/Any'". A value-class parameter
//! makes the JVM method name-mangled (`byHash-<hash>`), and the classpath return-type recovery keyed off
//! the un-mangled descriptor, so the recovered return type came back erased. The same shape with a plain
//! parameter, and the direct `r.byHash(h)?.active` member access, both already worked. The fix recovers
//! the declared (Kotlin) return type of a value-class-param method generically, so it flows into the
//! inline lambda's receiver like any other call result.
use super::common;

const LIB: &str = "package lib\n\
    @JvmInline value class Hash(val v: String)\n\
    data class Tok(val active: Boolean)\n\
    interface Repo { fun byHash(h: Hash): Tok? }\n\
    object R : Repo { override fun byHash(h: Hash): Tok? = if (h.v == \"x\") Tok(true) else null }\n";

fn run(tag: &str, main: &str) -> Option<String> {
    let jdk = common::jdk_modules()?;
    let sl = common::stdlib_jar()?;
    let lo = common::compile_lib(tag, LIB)?;
    common::compile_and_run_box(main, "Main", &[lo, sl, jdk.clone()], Some(&jdk))
}

#[test]
fn vcparam_result_safe_takeif_reads_member() {
    // The failing shape: value-class-param method result → `?.takeIf { it.active }`. `it` must be `Tok`.
    const MAIN: &str = "import lib.*\n\
        fun f(r: Repo, h: Hash): Tok? = r.byHash(h)?.takeIf { it.active }\n\
        fun box(): String {\n\
            val r: Repo = R\n\
            val t = f(r, Hash(\"x\"))\n\
            return if (t?.active == true) \"OK\" else \"F\"\n\
        }\n";
    assert_eq!(
        run("oo1_hit", MAIN).expect("vc-param result takeIf reads member"),
        "OK"
    );
}

#[test]
fn vcparam_result_safe_takeif_filters_to_null() {
    // The lambda predicate returning false collapses the safe-call chain to null.
    const MAIN: &str = "import lib.*\n\
        data class Tok2(val active: Boolean)\n\
        fun box(): String {\n\
            val r: Repo = R\n\
            val t = r.byHash(Hash(\"x\"))?.takeIf { !it.active }\n\
            return if (t == null) \"OK\" else \"F\"\n\
        }\n";
    assert_eq!(
        run("oo1_null", MAIN).expect("vc-param result takeIf filters"),
        "OK"
    );
}

#[test]
fn vcparam_result_direct_member_still_works() {
    // Regression lock for the already-working direct safe-member access on a value-class-param result.
    const MAIN: &str = "import lib.*\n\
        fun box(): String {\n\
            val r: Repo = R\n\
            val a: Boolean? = r.byHash(Hash(\"x\"))?.active\n\
            return if (a == true) \"OK\" else \"F\"\n\
        }\n";
    assert_eq!(
        run("oo1_direct", MAIN).expect("vc-param result direct member"),
        "OK"
    );
}
