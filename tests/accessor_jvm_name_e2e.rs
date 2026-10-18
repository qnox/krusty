//! `@JvmName` on a compiler-default accessor (`@get:JvmName`, `@set:JvmName`, or written on a
//! bodiless `get`/`set`) is that accessor's whole JVM name: the declaration, its callers, its
//! `$annotations` marker and its metadata signature all use it, and neither the internal module
//! suffix nor value-class mangling is applied to it.

use super::common;

/// Both compilers build `sources` as one module and every class they write is byte-identical.
fn assert_module_matches_kotlinc(sources: &[(&str, &str)]) {
    let classes = common::classes_against_kotlinc_module(sources);
    let differences = classes.differences();
    assert!(
        differences.is_empty(),
        "classes differ from kotlinc's:\n\n{}",
        differences.join("\n\n")
    );
}

#[test]
fn a_default_accessor_takes_its_jvm_name_like_kotlinc() {
    assert_module_matches_kotlinc(&[(
        "Names.kt",
        "package named\n\
         \n\
         class Member {\n\
         \x20   @get:JvmName(\"isReady\") val ready: Boolean = true\n\
         \x20   val written: Int = 1\n\
         \x20       @JvmName(\"readWritten\") get\n\
         \x20   @set:JvmName(\"assign\") var value: Int = 1\n\
         }\n\
         \n\
         object Single {\n\
         \x20   @get:JvmName(\"readName\") val name = \"o\"\n\
         \x20   @set:JvmName(\"storeCount\") var count = 1\n\
         }\n\
         \n\
         @get:JvmName(\"topName\") val top = 3\n\
         @set:JvmName(\"storeTop\") var mutableTop = 1\n\
         \n\
         fun use(member: Member): Int {\n\
         \x20   member.value = 2\n\
         \x20   Single.count = 3\n\
         \x20   mutableTop = 4\n\
         \x20   return if (member.ready) member.written + member.value + Single.count + top else 0\n\
         }\n",
    )]);
}

#[test]
fn an_internal_or_value_class_accessor_keeps_its_exact_jvm_name() {
    assert_module_matches_kotlinc(&[(
        "Exact.kt",
        "package named\n\
         \n\
         @JvmInline value class Label(val text: String)\n\
         \n\
         class Exact {\n\
         \x20   @JvmName(\"compute\") internal fun f() = 1\n\
         \x20   internal val custom: Int @JvmName(\"readCustom\") get() = 2\n\
         \x20   internal val plain: Int = 3\n\
         \x20       @JvmName(\"readPlain\") get\n\
         \x20   internal var stored: Int = 4\n\
         \x20       @JvmName(\"store\") set\n\
         \x20   @get:JvmName(\"readLabel\") val label: Label = Label(\"a\")\n\
         \x20   fun use(): Int { stored = 5; return f() + custom + plain + stored + label.text.length }\n\
         }\n",
    )]);
}

#[test]
fn a_companion_default_accessor_keeps_its_written_annotations_and_jvm_name() {
    assert_module_matches_kotlinc(&[(
        "Companion.kt",
        "package named\n\
         \n\
         annotation class Marked\n\
         \n\
         class Holder {\n\
         \x20   companion object {\n\
         \x20       @get:JvmName(\"readName\") val name = \"c\"\n\
         \x20       @set:JvmName(\"storeCount\") var count = 1\n\
         \x20       @Marked @get:JvmName(\"readMarked\") val marked = 2\n\
         \x20       @get:Deprecated(\"old\") val old = 3\n\
         \x20   }\n\
         \x20   class Nested {\n\
         \x20       fun use(): Int { count = 2; return name.length + count + marked }\n\
         \x20   }\n\
         }\n",
    )]);
}
