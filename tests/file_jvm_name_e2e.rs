//! `@file:JvmName("Facade")` names the class that holds a file's top-level declarations. kotlinc
//! reads the checked file annotation; krusty named every facade after the file (`FooKt`), so the
//! facade, every cross-file call into it, and the `.kotlin_module` part all disagreed — 17 files
//! of the Kotlin compiler's own core modules are written this way.
//!
//! The facade also carries the file's BINARY- and RUNTIME-retained annotations, `@JvmName` among
//! them, as kotlinc writes them.

use super::common;
use krusty::compilation_target::CompilationTarget;

const FACADE: &str = "/* a header comment ahead of the file annotations */\n\
    @file:JvmName(\"NameUtils\")\n\
    @file:Suppress(\"UNUSED\")\n\
    @file:Marker(\"m\")\n\
    \n\
    package p\n\
    \n\
    @Target(AnnotationTarget.FILE)\n\
    @Retention(AnnotationRetention.RUNTIME)\n\
    annotation class Marker(val v: String)\n\
    \n\
    fun helper(x: Int = 1): String = \"K$x\"\n\
    val answer: Int = 42\n\
    inline fun twice(f: () -> Unit) { f(); f() }\n";

const CALLER: &str = "package p\n\
    \n\
    fun box(): String {\n\
    \x20   val reference = ::helper\n\
    \x20   val joined = helper() + answer + reference(2)\n\
    \x20   return if (joined == \"K142K2\") \"OK\" else joined\n\
    }\n";

/// An inline function of the renamed facade, spliced from that facade's class into another file.
const INLINE_CALLER: &str = "package p\n\
    \n\
    fun box(): String {\n\
    \x20   var n = 0\n\
    \x20   twice { n++ }\n\
    \x20   return if (n == 2) \"OK\" else \"n=$n\"\n\
    }\n";

const PROPERTY_FACADE: &str = "@file:JvmName(\"Properties\")\n\
    package p\n\
    \n\
    import kotlin.reflect.KProperty\n\
    import kotlin.reflect.KProperty0\n\
    \n\
    class NameDelegate {\n\
    \x20   operator fun getValue(receiver: Any?, property: KProperty<*>): String = property.name\n\
    }\n\
    \n\
    val delegated: String by NameDelegate()\n\
    fun delegatedReference(): KProperty0<String> = ::delegated\n";

const PROPERTY_CALLER: &str = "package p\n\
    \n\
    fun box(): String {\n\
    \x20   val direct = delegated\n\
    \x20   val reference = delegatedReference()\n\
    \x20   return if (direct == \"delegated\" && reference.get() == direct) \"OK\" else \"$direct/${reference.get()}\"\n\
    }\n";

#[test]
fn a_file_jvm_name_names_the_facade_and_every_call_into_it() {
    let classes =
        common::classes_against_kotlinc_module(&[("nameUtils.kt", FACADE), ("Caller.kt", CALLER)]);
    assert!(classes.krusty.contains_key("p/NameUtils"));
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n"));
}

#[test]
fn a_file_jvm_name_facade_runs() {
    for caller in [CALLER, INLINE_CALLER] {
        common::expect_box_ok_files_with_stdlib(
            &[("nameUtils.kt", FACADE), ("Caller.kt", caller)],
            "p.CallerKt",
        );
    }
}

/// A renamed facade is also the physical owner selected for a top-level property reference and for
/// the delegated property's `KProperty` metadata. This covers the two paths separately from calls.
#[test]
fn a_file_jvm_name_owns_property_references_and_delegated_metadata() {
    let sources = [
        ("properties.kt", PROPERTY_FACADE),
        ("Caller.kt", PROPERTY_CALLER),
    ];
    let classes = common::classes_against_kotlinc_module(&sources);
    assert_eq!(
        classes.reference.keys().collect::<Vec<_>>(),
        classes.krusty.keys().collect::<Vec<_>>(),
        "exact class inventory"
    );
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n"));
    assert_eq!(
        common::compile_and_run_files_with_stdlib(&sources)
            .expect("compile and run the renamed delegated-property facade"),
        "OK"
    );
}

/// kotlinc rejects `@JvmField` on a top-level property of a multifile class file, at the
/// annotation: the part's fields are not the facade's.
#[test]
fn a_multifile_jvm_field_is_one_exact_frontend_error() {
    let source = "@file:JvmName(\"Shared\")\n\
        @file:JvmMultifileClass\n\
        package p\n\
        @JvmField val value = 1\n\
        val other = 2\n";
    let diagnostics = common::front_end_diagnostics_located_for(
        CompilationTarget::Jvm,
        source,
        &[common::stdlib_jar()],
        None,
    );
    assert_eq!(
        diagnostics,
        ["4:1: error: jvmField cannot be applied to top level property of a file annotated with JvmMultifileClass."]
    );
}
