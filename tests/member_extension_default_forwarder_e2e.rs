//! A class inheriting an interface's member extensions gets a forwarder for each, as kotlinc writes
//! them, whether the interface is in the same module or a dependency: plain and generic extension
//! functions, and extension properties' getters and setters. An ordinary class labels an accessor's
//! receiver after its property (`$this$prop`); a value class labels its forwarder after the JVM
//! accessor name (`$this$getProp`) and its `-impl` static after the property.

use super::common;

const PICKER: &str = "\
interface Picker<E> {\n\
    fun String.plainExt(): Int = length\n\
    fun <T, R> T.convert(r: R): R where T : CharSequence, T : Runnable = r\n\
    fun <T : Number> E.pair(x: T): Pair<E, T> = this to x\n\
    val String.prop: Int get() = length\n\
    var String.mut: Int\n\
        get() = length\n\
        set(value) {}\n\
    val String.id: Id get() = Id(length)\n\
    fun String.idOf(): Id = Id(length)\n\
}\n\
\n\
@JvmInline\n\
value class Id(val n: Int)\n\
";

const IMPLEMENTATIONS: &str = "\
object Strings : Picker<String>\n\
\n\
class Numbers<Q : Number> : Picker<Q>\n\
\n\
@JvmInline\n\
value class Wrapped(val s: String) : Picker<String>\n\
";

const CLASSES: [&str; 3] = ["app/Strings", "app/Numbers", "app/Wrapped"];

#[test]
fn member_extensions_get_default_forwarders() {
    let source = format!("package app\n\n{PICKER}\n{IMPLEMENTATIONS}");
    for class in CLASSES {
        common::byte_diff_against_kotlinc_cp(
            "MemberExtensionDefaultForwarder",
            &source,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}

#[test]
fn a_dependency_s_member_extensions_get_default_forwarders() {
    let library_source = format!("package lib\n\n{PICKER}");
    let library = common::compile_libs_build(
        "DependencyMemberExtensionForwarderLibrary",
        &[("Lib.kt", library_source.as_str())],
    )
    .expect("krusty builds the dependency");
    let reference = library
        .reference_out()
        .expect("reference kotlinc builds the dependency")
        .to_path_buf();
    common::assert_classes_identical_to_kotlinc_over_own_libraries(
        "DependencyMemberExtensionForwarder",
        &format!("package app\n\nimport lib.Picker\n\n{IMPLEMENTATIONS}"),
        &CLASSES,
        &[reference],
        &[library.krusty_out().to_path_buf()],
    );
}
