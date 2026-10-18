//! A Java type-variable occurrence is flexible (`T!`). A nullable argument fixes a root flexible
//! parameter to its nullable type, while a flexible variable nested below an array keeps the
//! enclosing structural inference used by kotlinc.

use super::common;
use super::diagnostics_parity_support::errors;
use std::path::Path;

fn java_arrays_classpath() -> std::path::PathBuf {
    common::javac_compile(
        &[(
            "JavaArrays.java".to_string(),
            "public final class JavaArrays {\n\
             \x20 public static <T> T[] copy(T[] values) { return values.clone(); }\n\
             }\n"
            .to_string(),
        )],
        &[],
    )
    .expect("javac compiles the generic Java array producer")
    .0
}

fn expect_box_with_java_arrays(source: &str, stem: &str, dependency: &Path) {
    let classpath = [dependency.to_path_buf()];
    let reference = common::kotlinc_box_result_with_classpath(source, &classpath);
    assert_eq!(reference, "OK", "{stem}: kotlinc fixture must succeed");
    assert_eq!(
        common::expect_box_run(
            source,
            stem,
            &[dependency.to_path_buf(), common::stdlib_jar()],
            Some(common::jdk_modules().as_path()),
        ),
        reference,
        "{stem}: krusty and kotlinc box results differ",
    );
}

#[test]
fn nullable_argument_to_a_flexible_variable_is_rejected_like_kotlinc() {
    let source = "import java.util.Collections\n\
                  \n\
                  fun <E> single(x: E?): List<E> = Collections.singletonList(x)\n\
                  fun named(x: String?): List<String> = Collections.singletonList(x)\n";
    let result =
        common::compiler_diagnostics(&[("FlexibleVariable.kt", source)], &[common::stdlib_jar()]);
    assert_ne!(result.reference_code, 0, "kotlinc must reject the fixture");
    assert_ne!(result.krusty_code, 0, "krusty must reject the fixture");
    let mut krusty = errors(&result.krusty_stderr);
    krusty.extend(errors(&result.krusty_stdout));
    let reference = errors(&result.reference_stderr);
    assert_eq!(krusty, reference);
    assert_eq!(
        krusty
            .iter()
            .map(|error| (error.line, error.column, error.message.as_str()))
            .collect::<Vec<_>>(),
        [
            (
                3,
                34,
                "return type mismatch: expected 'List<E (of fun <E> single)>', actual '(Mutable)List<E? (of fun <E> single)>!'."
            ),
            (
                4,
                39,
                "return type mismatch: expected 'List<String>', actual '(Mutable)List<String?>!'."
            ),
        ]
    );
}

#[test]
fn flexible_array_element_reaches_an_expected_non_null_list_like_kotlinc() {
    let dependency = java_arrays_classpath();
    expect_box_with_java_arrays(
        "import java.util.Arrays\n\
         \n\
         fun copy(): List<String> =\n\
         \x20   Arrays.asList(*JavaArrays.copy(arrayOf(\"O\", \"K\")))\n\
         \n\
         fun box(): String = copy()[0] + copy()[1]\n",
        "FlexibleArrayElement",
        &dependency,
    );
}

#[test]
fn named_argument_preserves_a_nested_flexible_array_element_like_kotlinc() {
    let dependency = java_arrays_classpath();
    expect_box_with_java_arrays(
        "import java.util.Arrays\n\
         \n\
         fun <T> forward(value: List<T>): List<T> = value\n\
         fun copy(): List<String> =\n\
         \x20   forward(value = Arrays.asList(*JavaArrays.copy(arrayOf(\"O\", \"K\"))))\n\
         \n\
         fun box(): String = copy()[0] + copy()[1]\n",
        "NamedFlexibleArrayElement",
        &dependency,
    );
}

#[test]
fn nullable_argument_result_publishes_a_rigid_nullable_element_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc_jdk(
        "NullableElement",
        "fun <E> s(x: E?) = java.util.Collections.singletonList(x)\n\
         fun n(x: String?) = java.util.Collections.singletonList(x)\n\
         fun z() = java.util.Collections.singletonList(null)\n",
        &["NullableElementKt"],
    );
}
