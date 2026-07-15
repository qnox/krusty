//! A `Type(args)` factory call where `Type` is a CLASSPATH type whose companion object declares
//! `operator fun invoke(args)` — kotlinc lowers this to `Type.Companion.invoke(args)`. An interface has
//! no constructor, so this is the only way to "construct" it. Mission-core hit:
//! `InstanceInternalId(Base58Uuid.generate())` in `MissionApplicationCatalogService`.
//! Needs the JVM toolchain + real kotlinc; skips otherwise.
use super::common;

const LIB: &str = "package lib\n\
    interface Id {\n\
        val v: String\n\
        companion object {\n\
            operator fun invoke(s: String): Id = object : Id { override val v = \"id-\" + s }\n\
        }\n\
    }\n";

#[test]
fn classpath_companion_invoke_factory() {
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    let Some(sl) = common::stdlib_jar() else {
        return;
    };
    let Some(lo) = common::compile_lib("companion_invoke", LIB) else {
        return;
    };
    const MAIN: &str = "import lib.Id\n\
        fun box(): String {\n\
            val id = Id(\"x\")\n\
            return if (id.v == \"id-x\") \"OK\" else \"F:\" + id.v\n\
        }\n";
    let out = common::compile_and_run_box(MAIN, "Main", &[lo, sl, jdk.clone()], Some(&jdk));
    assert_eq!(
        out.as_deref(),
        Some("OK"),
        "classpath companion invoke factory"
    );
}
