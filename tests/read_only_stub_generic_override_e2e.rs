//! A generic superclass member implements a read-only face's member in the class's substitution
//! context. `Base<T>.set(e: T)` under `Base<String>` implements `MutableListIterator.set(String)`,
//! so the class writes no throwing stub. Its physical descriptor is already the erased
//! `set(Object)`, so it gets no bridge either. A bounded `T : CharSequence` implementation is
//! `set(CharSequence)`, and the class bridges `set(Object)` to it. The same holds for a superclass
//! from a Kotlin dependency, whose member's declared type comes from its metadata.

use super::common;

const STRING_MEMBERS: &str = "    override fun hasNext() = false
    override fun next() = \"\"
    override fun hasPrevious() = false
    override fun previous() = \"\"
    override fun nextIndex() = 0
    override fun previousIndex() = -1
";

const INT_MEMBERS: &str = "    override fun hasNext() = false
    override fun next() = 0
    override fun hasPrevious() = false
    override fun previous() = 0
    override fun nextIndex() = 0
    override fun previousIndex() = -1
";

const GENERIC_BASE: &str = "open class Base<T> {
    var last: Any? = null
    fun set(e: T) { last = e }
}
";

fn generic_superclass() -> String {
    format!("{GENERIC_BASE}\nclass Walk : Base<String>(), ListIterator<String> {{\n{STRING_MEMBERS}}}\n")
}

fn bounded_superclass() -> String {
    format!(
        "open class Base<T : CharSequence> {{
    var last: Any? = null
    fun set(e: T) {{ last = e }}
}}

class Walk : Base<String>(), ListIterator<String> {{
{STRING_MEMBERS}}}
"
    )
}

/// The member reaches the class through a generic intermediate superclass.
fn through_generic_superclass() -> String {
    format!(
        "{GENERIC_BASE}\nopen class Mid<U> : Base<U>()\n\nclass Walk : Mid<String>(), ListIterator<String> {{\n{STRING_MEMBERS}}}\n"
    )
}

fn primitive_argument() -> String {
    format!("{GENERIC_BASE}\nclass Walk : Base<Int>(), ListIterator<Int> {{\n{INT_MEMBERS}}}\n")
}

/// The class's own type parameter: the stub itself already has the erased descriptor.
fn generic_class() -> String {
    format!(
        "{GENERIC_BASE}
class Walk<V>(val v: V) : Base<V>(), ListIterator<V> {{
    override fun hasNext() = false
    override fun next() = v
    override fun hasPrevious() = false
    override fun previous() = v
    override fun nextIndex() = 0
    override fun previousIndex() = -1
}}
"
    )
}

const DEPENDENCY: &str = "package dep

open class DBase<T> {
    var last: Any? = null
    fun set(e: T) { last = e }
}
";

fn dependency_superclass() -> String {
    format!("class Walk : dep.DBase<String>(), ListIterator<String> {{\n{STRING_MEMBERS}}}\n")
}

fn assert_classes_match_kotlinc(source: &str, classpath: &[std::path::PathBuf]) {
    let classes = common::classes_against_kotlinc_module_cp(
        &[("Walk.kt", &format!("package generic\n\n{source}"))],
        classpath,
    );
    assert!(
        classes.reference.contains_key("generic/Walk"),
        "kotlinc writes generic/Walk"
    );
    assert_eq!(classes.differences(), Vec::<String>::new());
}

/// `set(Object)` as a Java caller of `java.util.ListIterator` reaches it, then the stored value.
fn box_program(source: &str, argument: &str, expected: &str) -> String {
    format!(
        "{source}
fun box(): String {{
    val walk = Walk()
    walk.javaClass.getMethod(\"set\", Any::class.java).invoke(walk, {argument})
    return if (walk.last == {expected}) \"OK\" else \"${{walk.last}}\"
}}
"
    )
}

#[test]
fn a_generic_superclass_member_implements_the_member_like_kotlinc() {
    assert_classes_match_kotlinc(&generic_superclass(), &[]);
    common::expect_box_same_as_kotlinc(
        &box_program(&generic_superclass(), "\"x\"", "\"x\""),
        "read_only_generic_superclass_member",
    );
}

#[test]
fn a_bounded_generic_superclass_member_gets_a_bridge_to_its_bound_like_kotlinc() {
    assert_classes_match_kotlinc(&bounded_superclass(), &[]);
    common::expect_box_same_as_kotlinc(
        &box_program(&bounded_superclass(), "\"x\"", "\"x\""),
        "read_only_bounded_superclass_member",
    );
}

#[test]
fn a_member_through_a_generic_intermediate_superclass_implements_the_member_like_kotlinc() {
    assert_classes_match_kotlinc(&through_generic_superclass(), &[]);
    common::expect_box_same_as_kotlinc(
        &box_program(&through_generic_superclass(), "\"x\"", "\"x\""),
        "read_only_intermediate_superclass_member",
    );
}

#[test]
fn a_generic_superclass_member_over_int_implements_the_member_like_kotlinc() {
    assert_classes_match_kotlinc(&primitive_argument(), &[]);
    common::expect_box_same_as_kotlinc(
        &box_program(&primitive_argument(), "7", "7"),
        "read_only_int_superclass_member",
    );
}

#[test]
fn a_generic_class_over_a_generic_superclass_member_like_kotlinc() {
    assert_classes_match_kotlinc(&generic_class(), &[]);
}

#[test]
fn a_generic_dependency_superclass_member_implements_the_member_like_kotlinc() {
    let library = common::kotlinc_lib_out(&[("Dep.kt", DEPENDENCY)])
        .expect("reference kotlinc compiles the dependency");
    assert_classes_match_kotlinc(&dependency_superclass(), std::slice::from_ref(&library));

    let program = box_program(&dependency_superclass(), "\"x\"", "\"x\"");
    let reference =
        common::kotlinc_box_result_with_classpath(&program, std::slice::from_ref(&library));
    assert_eq!(reference, "OK", "kotlinc fixture must succeed");
    let jdk = common::jdk_modules();
    assert_eq!(
        common::expect_box_run(
            &program,
            "read_only_dependency_superclass_member",
            &[library, common::stdlib_jar()],
            Some(jdk.as_path()),
        ),
        reference,
    );
}
