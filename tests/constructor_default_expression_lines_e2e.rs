//! The lines of a constructor's `$default` overload while it evaluates a default, against kotlinc.
//!
//! kotlinc generates the overload's masked fills like any other code: each default expression maps
//! to its own lines, nested calls included, and the mask tests between them to the constructor's
//! line. krusty set the primary constructor's whole table by hand, putting each fill on the line
//! of the class field at the parameter's index (another parameter's line once a parameter is no
//! property), and a secondary constructor's on its default's first line only.

use super::common;

const SOURCE: &str = r#"fun k(s: String) = s

class Holder(
    val v: String =
        "O" +
            k(
                "K"
            ),
    w: Int =
        2,
    val z: Int = 1
) {
    constructor(
        a: Long,
        b: String =
            k(
                "q"
            ),
    ) : this(b + a)

    val q = w + z
}

fun box(): String = if (Holder(1L).v == "q1" && Holder().q == 3) Holder().v else "fail"
"#;

#[test]
fn default_fills_map_to_their_expressions_lines() {
    common::assert_classes_identical_to_kotlinc("ConstructorDefaultLines", SOURCE, &["Holder"]);
}

#[test]
fn default_fills_run() {
    common::expect_box_ok_with_stdlib(SOURCE, "ConstructorDefaultLines");
}

const LITERAL_DEFAULT: &str = r#"annotation class LiteralDefault(
    val value: Int = 7
)
fun box(): String = if (LiteralDefault().value == 7) "OK" else "bad"
"#;

const MULTIPLE_DEFAULTS: &str = r#"annotation class MultilineDefaults(
    val value: Int =
        7,
    val stamp: Long =
        9L,
    val valid: Boolean =
        true
)
fun box(): String {
    val all = MultilineDefaults()
    val partial = MultilineDefaults(stamp = 13L)
    return if (all.value == 7 && all.stamp == 9L && all.valid &&
        partial.value == 7 && partial.stamp == 13L && partial.valid) "OK" else "bad"
}
"#;

// These JVM controls compare complete generated constructor-realization classes, including every
// method, attribute and constant-pool entry, and authenticate the whole module's class inventory.
fn assert_realization(stem: &str, source: &str, owner: &str, libraries: &[std::path::PathBuf]) {
    let file = format!("{stem}.kt");
    let classes = common::classes_against_kotlinc_module_cp(&[(&file, source)], libraries);
    assert_eq!(
        classes.krusty.keys().collect::<Vec<_>>(),
        classes.reference.keys().collect::<Vec<_>>()
    );
    let reference = &classes.reference[owner];
    let actual = &classes.krusty[owner];
    assert_eq!(
        actual,
        reference,
        "{}",
        common::exact_class_difference(owner, reference, actual)
    );
}

#[test]
fn a_generated_default_constructor_keeps_its_expression_marks() {
    assert_realization(
        "AnnotationSparseDefaultLines",
        LITERAL_DEFAULT,
        "AnnotationSparseDefaultLinesKt$annotationImpl$LiteralDefault$0",
        &[],
    );
}

#[test]
fn a_generated_default_constructor_runs_like_kotlin() {
    common::expect_box_same_as_kotlinc(LITERAL_DEFAULT, "AnnotationSparseDefaultLines");
}

#[test]
fn multiple_default_fills_keep_their_complete_sparse_line_table() {
    assert_realization(
        "AnnotationMultipleDefaultLines",
        MULTIPLE_DEFAULTS,
        "AnnotationMultipleDefaultLinesKt$annotationImpl$MultilineDefaults$0",
        &[],
    );
}

#[test]
fn multiple_and_partial_default_fills_run_like_kotlin() {
    common::expect_box_same_as_kotlinc(MULTIPLE_DEFAULTS, "AnnotationMultipleDefaultLines");
}

const DEPENDENCY_DEFAULTS: &str = r#"package marks
annotation class DependencyDefault(
    val value: Int = 7,
    val stamp: Long = 9L
)
"#;

const DEPENDENCY_CONSUMER: &str = r#"import marks.DependencyDefault
fun box(): String {
    val value = DependencyDefault()
    return if (value.value == 7 && value.stamp == 9L) "OK" else "bad"
}
"#;

#[test]
fn dependency_defaults_do_not_invent_source_or_use_site_lines() {
    let dependency = common::kotlinc_lib_out(&[("Lib.kt", DEPENDENCY_DEFAULTS)])
        .expect("the reference dependency must compile");
    assert_realization(
        "DependencyDefaultLines",
        DEPENDENCY_CONSUMER,
        "DependencyDefaultLinesKt$annotationImpl$marks_DependencyDefault$0",
        &[dependency],
    );
}

#[test]
fn dependency_defaults_run_like_the_reference_producer() {
    let dependency = common::kotlinc_lib_out(&[("Lib.kt", DEPENDENCY_DEFAULTS)])
        .expect("the reference dependency must compile");
    let reference = common::kotlinc_box_result_with_classpath(
        DEPENDENCY_CONSUMER,
        std::slice::from_ref(&dependency),
    );
    assert_eq!(reference, "OK");
    let actual = common::expect_box_run(
        DEPENDENCY_CONSUMER,
        "DependencyDefaultLines",
        &[dependency, common::stdlib_jar()],
        Some(common::jdk_modules().as_path()),
    );
    assert_eq!(actual, reference);
}
