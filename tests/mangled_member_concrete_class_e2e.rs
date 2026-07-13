//! A PUBLIC value-class-param member on a CONCRETE (non-interface) classpath class must resolve. Its JVM
//! name is mangled (`findResource-<hash>`), so it is recovered from @Metadata under its source name — but
//! the recovery is gated on `is_public`, and a public FINAL function serializes NO `flags` field (it
//! equals the proto default `6`), so it decoded as visibility INTERNAL and was skipped. An interface's
//! ABSTRACT method has non-default flags (serialized), which decoded correctly and hid the bug. Decoding
//! `Function.flags` from its proto default now yields the right visibility. Round-tripped on the JVM.
use super::common;

const LIB: &str = "package lib\n\
    @JvmInline value class RId(val value: String)\n\
    data class Res(val name: String)\n\
    class Catalog(val resources: List<Res>) {\n\
        fun findResource(id: RId): Res? = resources.firstOrNull { it.name == id.value }\n\
    }\n";

fn run(tag: &str, main: &str) -> Option<String> {
    let jdk = common::jdk_modules()?;
    let sl = common::stdlib_jar()?;
    let lo = common::compile_lib(tag, LIB)?;
    common::compile_and_run_box(main, "Main", &[lo, sl, jdk.clone()], Some(&jdk))
}

#[test]
fn value_class_param_member_on_concrete_class_resolves() {
    const MAIN: &str = "import lib.*\n\
        fun box(): String {\n\
            val c = Catalog(listOf(Res(\"a\"), Res(\"b\")))\n\
            val hit = c.findResource(RId(\"b\"))\n\
            val miss = c.findResource(RId(\"z\"))\n\
            return if (hit?.name == \"b\" && miss == null) \"OK\" else \"FAIL\"\n\
        }\n";
    assert_eq!(
        run("mm_concrete", MAIN)
            .expect("public value-class-param member on a concrete class resolves + runs"),
        "OK"
    );
}
