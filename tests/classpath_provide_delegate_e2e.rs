//! Member properties delegated to a CLASSPATH delegate through `provideDelegate`.
//!
//! Shape (CLI-parser DSLs): `private val configFile by option("--config")` where `option` returns a
//! classpath `OptionDelegate<T>` declaring `operator fun provideDelegate(thisRef, prop)` and
//! `operator fun getValue(thisRef, prop): T?`. kotlinc initializes the backing delegate field with
//! the `provideDelegate` result and routes reads through `getValue`. krusty declined the whole
//! file ("this construct is not yet supported"), and an FQ constructor whose argument read such a
//! property mis-blamed the path root ("unresolved reference 'java'").
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

// Non-generic delegates: a generic `val value: T?` member read is a separate open gap in the
// krusty-built dependency lib; the channel under test here is `provideDelegate` itself.
const LIB: &str = "package lib\n\
    import kotlin.reflect.KProperty\n\
    class StringOption(private val value: String?) {\n\
        operator fun provideDelegate(thisRef: Any?, prop: KProperty<out Any?>): StringOption = this\n\
        operator fun getValue(thisRef: Any?, property: KProperty<out Any?>): String? = value\n\
    }\n\
    class FlagOption(private val value: Boolean) {\n\
        operator fun provideDelegate(thisRef: Any?, prop: KProperty<out Any?>): FlagOption = this\n\
        operator fun getValue(thisRef: Any?, property: KProperty<out Any?>): Boolean = value\n\
    }\n\
    fun option(name: String): StringOption = StringOption(name.removePrefix(\"--\"))\n\
    fun StringOption.flag(): FlagOption = FlagOption(true)\n";

#[test]
fn provide_delegate_from_base_class_extension() {
    // The CLI-parser shape exactly: the delegate comes from an EXTENSION on a classpath base type
    // (`fun Holder.optionExt(...)`), reached through the enclosing class's implicit `this`. The
    // member-property pre-pass must try that receiver — a receiver-less top-level query never
    // sees the extension, the delegate types as Error, and every member read cascades.
    const EXT_LIB: &str = "package lib\n\
        import kotlin.reflect.KProperty\n\
        interface Holder\n\
        abstract class Command : Holder\n\
        class StringOption(private val value: String?) {\n\
            operator fun provideDelegate(thisRef: Any?, prop: KProperty<out Any?>): StringOption = this\n\
            operator fun getValue(thisRef: Any?, property: KProperty<out Any?>): String? = value\n\
        }\n\
        fun Holder.optionExt(name: String): StringOption = StringOption(name.removePrefix(\"--\"))\n";
    const MAIN: &str = "import lib.Command\n\
        import lib.optionExt\n\
        class Cmd : Command() {\n\
            private val configFile by optionExt(\"--config\")\n\
            fun plan(): String = java.io.File(configFile ?: \"infra.yml\").name\n\
        }\n\
        fun box(): String = if (Cmd().plan() == \"config\") \"OK\" else \"fail\"\n";
    assert_eq!(
        run("pd2", EXT_LIB, MAIN).expect("base-class extension provideDelegate"),
        "OK"
    );
}

#[test]
fn classpath_provide_delegate_member_property_reads() {
    const MAIN: &str = "import lib.option\n\
        import lib.flag\n\
        class Cmd {\n\
            private val local by option(\"--local\").flag()\n\
            private val configFile by option(\"--config\")\n\
            fun plan(): String {\n\
                val file = java.io.File(configFile ?: \"infra.yml\")\n\
                return if (local) file.name else \"off\"\n\
            }\n\
        }\n\
        fun box(): String = if (Cmd().plan() == \"config\") \"OK\" else \"fail\"\n";
    assert_eq!(
        run("pd1", LIB, MAIN).expect("classpath provideDelegate member property"),
        "OK"
    );
}

#[test]
fn provide_delegate_result_is_the_stored_delegate() {
    const PROVIDER_LIB: &str = "package lib\n\
        import kotlin.reflect.KProperty\n\
        class Stored(private var value: String) {\n\
            operator fun getValue(thisRef: Any?, property: KProperty<out Any?>): String = value\n\
            operator fun setValue(thisRef: Any?, property: KProperty<out Any?>, next: String) { value = next }\n\
        }\n\
        class Provider(private val initial: String) {\n\
            operator fun provideDelegate(thisRef: Any?, property: KProperty<out Any?>): Stored = Stored(initial)\n\
        }\n\
        fun provided(initial: String): Provider = Provider(initial)\n";
    const MAIN: &str = "import lib.provided\n\
        class Box {\n\
            var value by provided(\"before\")\n\
            fun update(): String { value = \"OK\"; return value }\n\
        }\n\
        fun box(): String = Box().update()\n";
    assert_eq!(
        run("pd3", PROVIDER_LIB, MAIN).expect("provided stored delegate"),
        "OK"
    );
}
