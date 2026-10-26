//! A member implements a read-only face's mutable member only when, besides matching its
//! parameters, its result is a subtype of the mutable member's. That is kotlinc's override check.
//! `fun set(e: String): Int` does not implement `MutableListIterator.set(String): Unit`, whether
//! the class declares it, inherits it from a Kotlin or Java superclass, or inherits a generic one
//! whose result substitutes to `String`. The class then keeps the throwing `set(String)` stub and
//! `set(Object)` bridge. A `Nothing` member does implement it. Its bridge calls the member's
//! `Void` descriptor and pops the result, also when a separately compiled, non-generic Kotlin
//! superclass declares it: its metadata, not its physical `Void`, gives the `Nothing` result.

use super::common;

const MEMBERS: &str = "    override fun hasNext() = false
    override fun next() = \"\"
    override fun hasPrevious() = false
    override fun previous() = \"\"
    override fun nextIndex() = 0
    override fun previousIndex() = -1
";

fn own_int_member() -> String {
    format!("class Walk : ListIterator<String> {{\n{MEMBERS}    fun set(e: String): Int = 1\n}}\n")
}

fn superclass_int_member() -> String {
    format!(
        "open class Base {{\n    fun set(e: String): Int = 1\n}}\n\nclass Walk : Base(), ListIterator<String> {{\n{MEMBERS}}}\n"
    )
}

fn generic_superclass_result() -> String {
    format!(
        "open class Base<T> {{\n    fun set(e: T): T = e\n}}\n\nclass Walk : Base<String>(), ListIterator<String> {{\n{MEMBERS}}}\n"
    )
}

fn nothing_member() -> String {
    format!(
        "class Walk : ListIterator<String> {{\n{MEMBERS}    fun set(e: String): Nothing = throw IllegalStateException(\"set \" + e)\n}}\n"
    )
}

/// A separately compiled, non-generic Kotlin superclass with a `Nothing` member.
const NOTHING_DEPENDENCY: &str = "package dep

open class NBase {
    fun set(e: String): Nothing = throw IllegalStateException(\"set \" + e)
}
";

fn nothing_dependency_member() -> String {
    format!("class Walk : dep.NBase(), ListIterator<String> {{\n{MEMBERS}}}\n")
}

const JAVA_SUPERCLASS: &str = "package jl;

public class JBase {
    public int set(String e) {
        return 1;
    }
}
";

fn java_superclass_member() -> String {
    format!("class Walk : jl.JBase(), ListIterator<String> {{\n{MEMBERS}}}\n")
}

fn assert_classes_match_kotlinc(source: &str, classpath: &[std::path::PathBuf]) {
    let classes = common::classes_against_kotlinc_module_cp(
        &[("Walk.kt", &format!("package results\n\n{source}"))],
        classpath,
    );
    assert!(
        classes.reference.contains_key("results/Walk"),
        "kotlinc writes results/Walk"
    );
    assert_eq!(classes.differences(), Vec::<String>::new());
}

/// What `set(Object)` does when a Java caller of `java.util.ListIterator` reaches it.
fn box_program(source: &str, expected: &str) -> String {
    format!(
        "{source}
fun box(): String {{
    val walk = Walk()
    val outcome = try {{
        walk.javaClass.getMethod(\"set\", Any::class.java).invoke(walk, \"x\")
        \"returned\"
    }} catch (e: java.lang.reflect.InvocationTargetException) {{
        \"${{e.cause!!::class.simpleName}}: ${{e.cause!!.message}}\"
    }}
    return if (outcome == \"{expected}\") \"OK\" else outcome
}}
"
    )
}

const READ_ONLY: &str =
    "UnsupportedOperationException: Operation is not supported for read-only collection";

#[test]
fn an_own_member_with_another_result_keeps_the_stub_like_kotlinc() {
    assert_classes_match_kotlinc(&own_int_member(), &[]);
    common::expect_box_same_as_kotlinc(
        &box_program(&own_int_member(), READ_ONLY),
        "read_only_own_int_member",
    );
}

#[test]
fn a_superclass_member_with_another_result_keeps_the_stub_like_kotlinc() {
    assert_classes_match_kotlinc(&superclass_int_member(), &[]);
    common::expect_box_same_as_kotlinc(
        &box_program(&superclass_int_member(), READ_ONLY),
        "read_only_superclass_int_member",
    );
}

#[test]
fn a_generic_superclass_member_whose_result_substitutes_keeps_the_stub_like_kotlinc() {
    assert_classes_match_kotlinc(&generic_superclass_result(), &[]);
    common::expect_box_same_as_kotlinc(
        &box_program(&generic_superclass_result(), READ_ONLY),
        "read_only_generic_superclass_result",
    );
}

#[test]
fn a_nothing_member_implements_the_member_and_its_bridge_pops_the_result_like_kotlinc() {
    assert_classes_match_kotlinc(&nothing_member(), &[]);
    common::expect_box_same_as_kotlinc(
        &box_program(&nothing_member(), "IllegalStateException: set x"),
        "read_only_nothing_member",
    );
}

#[test]
fn a_nothing_dependency_member_implements_the_member_like_kotlinc() {
    let library = common::kotlinc_lib_out(&[("NBase.kt", NOTHING_DEPENDENCY)])
        .expect("reference kotlinc compiles the dependency");
    assert_classes_match_kotlinc(&nothing_dependency_member(), std::slice::from_ref(&library));

    let program = box_program(&nothing_dependency_member(), "IllegalStateException: set x");
    let reference =
        common::kotlinc_box_result_with_classpath(&program, std::slice::from_ref(&library));
    assert_eq!(reference, "OK", "kotlinc fixture must succeed");
    let jdk = common::jdk_modules();
    assert_eq!(
        common::expect_box_run(
            &program,
            "read_only_nothing_dependency_member",
            &[library, common::stdlib_jar()],
            Some(jdk.as_path()),
        ),
        reference,
    );
}

#[test]
fn a_java_superclass_member_with_another_result_keeps_the_stub_like_kotlinc() {
    let (java, _) = common::javac_compile(
        &[("JBase.java".to_string(), JAVA_SUPERCLASS.to_string())],
        &[],
    )
    .expect("javac compiles the Java superclass");
    assert_classes_match_kotlinc(&java_superclass_member(), std::slice::from_ref(&java));

    let program = box_program(&java_superclass_member(), READ_ONLY);
    let reference =
        common::kotlinc_box_result_with_classpath(&program, std::slice::from_ref(&java));
    assert_eq!(reference, "OK", "kotlinc fixture must succeed");
    let jdk = common::jdk_modules();
    assert_eq!(
        common::expect_box_run(
            &program,
            "read_only_java_superclass_int_member",
            &[java, common::stdlib_jar()],
            Some(jdk.as_path()),
        ),
        reference,
    );
}
