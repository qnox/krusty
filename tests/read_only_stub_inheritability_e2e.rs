//! A read-only face's throwing stub is suppressed only by a member the class actually inherits.
//!
//! kotlinc's `CollectionStubComputer` asks whether the class's fake override of the mutable member
//! is implemented. A private superclass member is never inherited, and a package-private Java
//! member only into a class of its package, so neither implements `MutableIterator.remove` for a
//! class in another package: kotlinc emits the public throwing `remove()` stub in both cases.

use super::common;

const REJECTED: &str = r#"
fun box(): String {
    val walk: Any = Walk()
    return try {
        walk.javaClass.getMethod("remove").invoke(walk)
        "accepted"
    } catch (e: java.lang.reflect.InvocationTargetException) {
        val cause = e.cause
        if (cause is UnsupportedOperationException &&
            cause.message == "Operation is not supported for read-only collection"
        ) "OK" else "$cause"
    }
}
"#;

const SOURCE_SUPERCLASS: &str = r#"open class Base {
    private fun remove() {}
}

class Walk : Base(), Iterator<String> {
    override fun hasNext() = false
    override fun next() = ""
}
"#;

const JAVA_SUPERCLASS: &str = r#"package lib;

public class JBase {
    void remove() {}
}
"#;

const JAVA_SUBCLASS: &str = r#"class Walk : lib.JBase(), Iterator<String> {
    override fun hasNext() = false
    override fun next() = ""
}
"#;

#[test]
fn a_private_source_superclass_member_does_not_suppress_the_stub_like_kotlinc() {
    let source = format!("package stubs\n\n{SOURCE_SUPERCLASS}");
    for class in ["stubs/Base", "stubs/Walk"] {
        common::byte_diff_against_kotlinc_cp(
            "PrivateSuperclassRemove",
            &source,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
    common::expect_box_same_as_kotlinc(
        &format!("{SOURCE_SUPERCLASS}{REJECTED}"),
        "private_superclass_remove",
    );
}

#[test]
fn a_package_private_java_member_in_another_package_does_not_suppress_the_stub_like_kotlinc() {
    let (java, _) = common::javac_compile(
        &[("JBase.java".to_string(), JAVA_SUPERCLASS.to_string())],
        &[],
    )
    .expect("javac compiles the Java superclass");
    let source = format!("package app\n\n{JAVA_SUBCLASS}");
    assert_walk_matches_kotlinc(&source, "app/Walk", &java);

    let program = format!("{JAVA_SUBCLASS}{REJECTED}");
    let reference = common::kotlinc_box_result_with_classpath(&program, &[java.clone()]);
    assert_eq!(reference, "OK", "kotlinc fixture must succeed");
    let jdk = common::jdk_modules();
    assert_eq!(
        common::expect_box_run(
            &program,
            "package_private_superclass_remove",
            &[java, common::stdlib_jar()],
            Some(jdk.as_path()),
        ),
        reference,
    );
}

/// In its own package the package-private member is inherited and implements `remove`: no stub.
#[test]
fn a_package_private_java_member_in_the_same_package_implements_the_member_like_kotlinc() {
    let (java, _) = common::javac_compile(
        &[("JBase.java".to_string(), JAVA_SUPERCLASS.to_string())],
        &[],
    )
    .expect("javac compiles the Java superclass");
    let source = format!(
        "package lib\n\n{}",
        JAVA_SUBCLASS.replace("lib.JBase", "JBase")
    );
    assert_walk_matches_kotlinc(&source, "lib/Walk", &java);
}

/// Both compilers compile `source` against the javac-built superclass, and every class they write
/// is byte-identical, `walk` among them.
fn assert_walk_matches_kotlinc(source: &str, walk: &str, java: &std::path::Path) {
    let classes = common::classes_against_kotlinc_module_cp(&[("Walk.kt", source)], &[java.into()]);
    assert!(
        classes.reference.contains_key(walk),
        "kotlinc writes {walk}"
    );
    assert_eq!(classes.differences(), Vec::<String>::new());
}
