//! Subclassing a CLASSPATH abstract class whose obligations the source class fulfills.
//!
//! Shape (every framework command/base class): `abstract class Base { abstract fun run();
//! fun helper() = … }` on the classpath, `class Impl : Base() { override fun run() { … } }` in
//! source. krusty gated the whole file (`gate:nonlocal-superclass-abstract-obligations`) whenever
//! ANY abstract method existed in the classpath super chain — even fully-overridden ones, and even
//! ones a lower classpath class already implements.
use super::common;

fn run(tag: &str, lib: &str, main: &str) -> Option<String> {
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    let libout = common::compile_lib(tag, lib)?;
    Some(common::expect_box_run(
        main,
        "Main",
        &[libout, sl],
        Some(jdk.as_path()),
    ))
}

#[test]
fn overriding_all_classpath_abstract_obligations_subclasses() {
    const LIB: &str = "package lib\n\
        abstract class Base {\n\
            abstract fun runIt(): String\n\
            fun helper(): String = \"h\"\n\
        }\n";
    const MAIN: &str = "import lib.Base\n\
        class Impl : Base() {\n\
            override fun runIt(): String = \"r\"\n\
        }\n\
        fun box(): String {\n\
            val i = Impl()\n\
            return if (i.runIt() + i.helper() == \"rh\") \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(
        run("ao1", LIB, MAIN).expect("classpath abstract obligations overridden"),
        "OK"
    );
}

#[test]
fn mid_hierarchy_implementation_discharges_the_obligation() {
    // `Mid` (classpath) implements `Base`'s abstract member; the source subclass owes nothing.
    const LIB: &str = "package lib\n\
        abstract class Base { abstract fun runIt(): String }\n\
        abstract class Mid : Base() { override fun runIt(): String = \"m\" }\n";
    const MAIN: &str = "import lib.Mid\n\
        class Impl : Mid()\n\
        fun box(): String = if (Impl().runIt() == \"m\") \"OK\" else \"fail\"\n";
    assert_eq!(
        run("ao2", LIB, MAIN).expect("mid-hierarchy implementation"),
        "OK"
    );
}
