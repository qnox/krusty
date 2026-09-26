//! `ReturnValueStatus` in `@Metadata` (`Function.flags` bits 16-17, `Property.flags` bits 17-18).
//!
//! Measured against kotlinc 2.4.0, 2.4.10 and 2.4.20, which agree: with the return-value checker
//! disabled (their default) a declaration takes the status of the first declaration it overrides.
//! The stdlib records `MustUse` on most of its API and `ExplicitlyIgnorable` on members such as
//! `MutableCollection.add`, so an override of either repeats that status; an override of a member
//! declared in the current module, or of a Java-only method, records none. A Java method in between
//! is transparent: overriding `java.util.ArrayList.add` repeats Kotlin's `MutableCollection.add`.
//! Members of local classes and anonymous objects inherit the same way, and so do `operator` and
//! `infix`.

use super::common;

fn assert_identical(stem: &str, src: &str, class_internal: &str) {
    let classpath = [common::stdlib_jar(), common::jdk_modules()];
    common::metadata_diff_against_kotlinc_cp(stem, src, class_internal, &classpath)
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{diff}"));
}

#[test]
fn a_continuation_override_repeats_must_use() {
    const SRC: &str = "package app\n\
        \n\
        import kotlin.coroutines.*\n\
        \n\
        class C : Continuation<Unit> {\n\
        \x20   override val context: CoroutineContext get() = EmptyCoroutineContext\n\
        \x20   override fun resumeWith(result: Result<Unit>) {}\n\
        }\n";
    assert_identical("continuation_override", SRC, "app/C");
}

#[test]
fn an_override_of_a_module_declaration_records_no_status() {
    const SRC: &str = "package app\n\
        \n\
        open class B {\n\
        \x20   open fun f(): Int = 1\n\
        \x20   open val p: Int get() = 1\n\
        }\n\
        \n\
        class D : B() {\n\
        \x20   override fun f(): Int = 2\n\
        \x20   override val p: Int get() = 2\n\
        \x20   override fun toString(): String = \"d\"\n\
        }\n";
    assert_identical("module_override", SRC, "app/D");
}

#[test]
fn an_override_of_a_module_override_repeats_the_library_status() {
    const SRC: &str = "package app\n\
        \n\
        abstract class Base : Comparable<Base> {\n\
        \x20   override fun compareTo(other: Base): Int = 0\n\
        }\n\
        \n\
        class Leaf : Base() {\n\
        \x20   override fun compareTo(other: Base): Int = 1\n\
        }\n";
    assert_identical("module_override_chain", SRC, "app/Leaf");
}

#[test]
fn an_abstract_list_override_repeats_must_use() {
    const SRC: &str = "package app\n\
        \n\
        class L : AbstractList<String>() {\n\
        \x20   override val size: Int get() = 0\n\
        \x20   override fun get(index: Int): String = \"\"\n\
        }\n";
    assert_identical("abstract_list", SRC, "app/L");
}

#[test]
fn a_java_collection_override_repeats_the_kotlin_status() {
    const SRC: &str = "package app\n\
        \n\
        class ML : java.util.ArrayList<Int>() {\n\
        \x20   override fun add(element: Int): Boolean = true\n\
        }\n";
    assert_identical("java_collection_override", SRC, "app/ML");
}

#[test]
fn a_java_only_override_records_no_status() {
    const SRC: &str = "package app\n\
        \n\
        class R : Runnable {\n\
        \x20   override fun run() {}\n\
        }\n";
    assert_identical("java_only_override", SRC, "app/R");
}

#[test]
fn a_declared_equals_repeats_any_equals_must_use() {
    const SRC: &str = "package app\n\
        \n\
        class U {\n\
        \x20   override fun equals(other: Any?): Boolean = other is U\n\
        \x20   override fun hashCode(): Int = 1\n\
        }\n";
    assert_identical("declared_equals", SRC, "app/U");
}

/// An object in a default argument is checked once for each body that carries the default, and
/// its override statuses are published with the first of those checks.
#[test]
fn an_object_in_a_default_argument_publishes_its_statuses_once() {
    const SRC: &str = "class A(\n\
        \x20   val a: String = object {\n\
        \x20       override fun toString(): String = \"OK\"\n\
        \x20   }.toString()\n\
        )\n\
        \n\
        fun box(): String = A().a\n";
    let result = common::compile_and_run_box(
        SRC,
        "default_argument_object",
        &[common::stdlib_jar()],
        None,
    );
    assert_eq!(result.as_deref(), Some("OK"));
}

#[test]
fn an_anonymous_object_override_repeats_must_use() {
    const SRC: &str = "package app\n\
        \n\
        import kotlin.coroutines.*\n\
        \n\
        fun make(): Continuation<Unit> = object : Continuation<Unit> {\n\
        \x20   override val context: CoroutineContext get() = EmptyCoroutineContext\n\
        \x20   override fun resumeWith(result: Result<Unit>) {}\n\
        }\n";
    assert_identical("anonymous_override", SRC, "app/Anonymous_overrideKt$make$1");
}

#[test]
fn a_local_class_override_repeats_must_use() {
    const SRC: &str = "package app\n\
        \n\
        fun make(): Any {\n\
        \x20   class Local { override fun toString(): String = \"local\" }\n\
        \x20   return Local()\n\
        }\n";
    assert_identical("local_override", SRC, "app/Local_overrideKt$make$Local");
}

#[test]
fn a_local_class_override_inherits_operator_and_infix() {
    const SRC: &str = "package app\n\
        \n\
        interface Op {\n\
        \x20   operator fun plus(other: Op): Op\n\
        \x20   infix fun join(other: Op): Op\n\
        }\n\
        \n\
        fun make(): Op = object : Op {\n\
        \x20   override fun plus(other: Op): Op = this\n\
        \x20   override fun join(other: Op): Op = this\n\
        }\n";
    assert_identical("local_operator", SRC, "app/Local_operatorKt$make$1");
}
