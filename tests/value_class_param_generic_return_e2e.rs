//! A classpath member whose JVM name is MANGLED by a value-class PARAMETER (`findByOrg(id: OrgId):
//! List<Ws>` → `findByOrg-<hash>(String)List`) must still expose its GENERIC return (`List<Ws>`). The
//! mangled-member recovery built the return from the bare `ret_class` alone (`List`), dropping the type
//! arguments — so `repo.findByOrg(id)` typed `List<Any>` and a member access on an element
//! (`.firstOrNull { it.name … }`) failed with "unresolved member 'name' on 'kotlin/Any'". The single-type
//! value-class-param return recovery (build.1017) did not carry type arguments; this recovers them from
//! the metadata generic signature. Round-tripped on the JVM.
use super::common;

const LIB: &str = "package lib\n\
    @JvmInline value class OrgId(val value: String)\n\
    data class Ws(val name: String)\n\
    interface Repo { fun findByOrg(id: OrgId): List<Ws> }\n\
    object R : Repo { override fun findByOrg(id: OrgId): List<Ws> = listOf(Ws(\"a\"), Ws(\"default\")) }\n";

fn run(tag: &str, main: &str) -> Option<String> {
    let jdk = common::jdk_modules()?;
    let sl = common::stdlib_jar()?;
    let lo = common::compile_lib(tag, LIB)?;
    common::compile_and_run_box(main, "Main", &[lo, sl, jdk.clone()], Some(&jdk))
}

#[test]
fn vcparam_generic_return_element_member_resolves() {
    // `repo.findByOrg(id)` must yield `List<Ws>`, so `firstOrNull { it.name == … }` types `it` as `Ws`.
    const MAIN: &str = "import lib.*\n\
        fun box(): String {\n\
            val repo: Repo = R\n\
            val xs = repo.findByOrg(OrgId(\"o\"))\n\
            val w = xs.firstOrNull { it.name == \"default\" }\n\
            return w?.name ?: \"none\"\n\
        }\n";
    assert_eq!(
        run("vcpr_generic", MAIN).expect("vc-param generic return element member resolves"),
        "default"
    );
}
