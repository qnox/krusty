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

#[test]
fn one_same_arity_override_does_not_discharge_a_distinct_overload() {
    const LIB: &str = "package lib\n\
        abstract class Base {\n\
            abstract fun choose(value: Int): String\n\
            abstract fun choose(value: String): String\n\
        }\n";
    const MAIN: &str = "import lib.Base\n\
        class Impl : Base() {\n\
            override fun choose(value: Int): String = value.toString()\n\
        }\n\
        fun box(): String = \"OK\"\n";
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let dependency = common::compile_lib("ao-overloads", LIB).expect("dependency");
    assert!(
        common::compile_in_process(MAIN, "Main", &[dependency, stdlib], Some(jdk.as_path()))
            .is_none(),
        "one semantic override must not clear a different same-name/same-arity obligation",
    );
}

#[test]
fn suspend_override_uses_the_semantic_parameter_shape() {
    const LIB: &str = "package lib\n\
        abstract class Base { abstract suspend fun runIt(): String }\n";
    const MAIN: &str = "import lib.Base\n\
        class Impl : Base() {\n\
            override suspend fun runIt(): String = \"implemented\"\n\
        }\n\
        fun box(): String = \"OK\"\n";
    assert_eq!(
        run("ao-suspend", LIB, MAIN).expect("semantic suspend obligation"),
        "OK",
    );
}

#[test]
fn mapped_number_has_one_semantic_superclass_path() {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    const MAIN: &str = "class Num(val raw: Int) : Number() {\n\
        override fun toByte(): Byte = raw.toByte()\n\
        override fun toDouble(): Double = raw.toDouble()\n\
        override fun toFloat(): Float = raw.toFloat()\n\
        override fun toInt(): Int = raw\n\
        override fun toLong(): Long = raw.toLong()\n\
        override fun toShort(): Short = raw.toShort()\n\
    }\n\
    fun box(): String = if (Num(7).toInt() == 7) \"OK\" else \"fail\"\n";
    let classes = common::compile_in_process(
        MAIN,
        "Main",
        std::slice::from_ref(&stdlib),
        Some(jdk.as_path()),
    )
    .expect("mapped Number obligations must follow one normalized superclass path");
    assert_eq!(
        common::run_box(&classes, "MainKt", &[stdlib]).expect("box runner"),
        "OK",
    );
}

#[test]
fn concrete_kotlin_property_discharges_mapped_abstract_accessor() {
    const MAIN: &str = "class Strings : AbstractSet<String>() {\n\
        override val size: Int get() = 0\n\
        override fun iterator(): Iterator<String> = emptyList<String>().iterator()\n\
    }\n\
    fun box(): String = \"OK\"\n";
    common::expect_front_end_ok_files_with_stdlib(
        &[MAIN],
        "mapped abstract accessor implemented by property",
    );
}

#[test]
fn inherited_interface_delegation_discharges_abstract_function() {
    const MAIN: &str = "interface Scope {\n\
        fun value(text: String = \"OK\"): String\n\
    }\n\
    class ScopeImpl : Scope {\n\
        override fun value(text: String): String = text\n\
    }\n\
    abstract class Delegated(scope: Scope) : Scope by scope\n\
    class Child : Delegated(ScopeImpl())\n\
    fun box(): String = \"OK\"\n";
    common::expect_front_end_ok_files_with_stdlib(
        &[MAIN],
        "inherited interface delegation abstract obligation",
    );
}

#[test]
fn alpha_renamed_generic_override_discharges_abstract_function() {
    const MAIN: &str = "interface Transform {\n\
        fun <T> apply(value: T): T\n\
    }\n\
    class Identity : Transform {\n\
        override fun <U> apply(value: U): U = value\n\
    }\n\
    fun box(): String = \"OK\"\n";
    common::expect_front_end_ok_files_with_stdlib(
        &[MAIN],
        "alpha-renamed generic abstract obligation",
    );
}
