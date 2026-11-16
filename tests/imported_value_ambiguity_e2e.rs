//! kotlinc collects every value the nearest import rung offers for a receiver-less name: the enum
//! entries and static fields of star-imported classifiers and the top-level properties of
//! star-imported packages. Two of them on that rung are an ambiguity listing each candidate in
//! import order, not an unresolved reference and not a silent choice of the first one.

use super::common;

const JAVA_CONSTANTS: (&str, &str) = (
    "first/KtNodeTypes.java",
    "package first;\npublic interface KtNodeTypes {\n    String SOME = \"Some\";\n}\n",
);
const SECOND_ENUM: (&str, &str) = ("SomeEnum.kt", "package second\nenum class SomeEnum { SOME; }\n");
const THIRD_ENUM: (&str, &str) = ("OtherEnum.kt", "package third\nenum class SomeEnum { SOME; }\n");
const TOP_LEVEL: (&str, &str) = ("Top.kt", "package fourth\nval SOME = 1\n");

/// Require krusty's complete errors to be kotlinc's and to read exactly `expected`.
fn assert_errors(sources: &[(&str, &str)], expected: &[&str]) {
    common::assert_error_blocks_match_kotlinc(sources, &[]);
    assert_eq!(common::reference_error_blocks(sources, &[]), expected);
}

#[test]
fn a_static_field_and_two_enum_entries_on_one_star_rung_are_ambiguous() {
    let test = "\
import first.KtNodeTypes.*
import second.SomeEnum.*
import third.SomeEnum.*

fun test(arg: String): Boolean {
    return when (arg) {
        SOME -> true
        else -> false
    }
}
";
    assert_errors(
        &[("test.kt", test), JAVA_CONSTANTS, SECOND_ENUM, THIRD_ENUM],
        &[
            "test.kt:7:9: overload resolution ambiguity between candidates:",
            "| static field SOME: String",
            "| enum entry SOME: SomeEnum",
            "| enum entry SOME: SomeEnum",
        ],
    );
}

#[test]
fn two_star_imported_enum_entries_are_ambiguous_in_import_order() {
    let test = "\
import second.SomeEnum.*
import first.KtNodeTypes.*
fun test() = SOME
fun subject(x: second.SomeEnum) = when (x) { SOME -> 1; else -> 2 }
";
    assert_errors(
        &[("test.kt", test), JAVA_CONSTANTS, SECOND_ENUM],
        &[
            "test.kt:3:14: overload resolution ambiguity between candidates:",
            "| enum entry SOME: SomeEnum",
            "| static field SOME: String",
            "test.kt:4:46: overload resolution ambiguity between candidates:",
            "| enum entry SOME: SomeEnum",
            "| static field SOME: String",
        ],
    );
}

#[test]
fn an_enum_entry_ties_a_star_imported_top_level_property() {
    let test = "\
import fourth.*
import second.SomeEnum.*
import third.SomeEnum.*
fun test() = SOME
";
    assert_errors(
        &[("test.kt", test), SECOND_ENUM, THIRD_ENUM, TOP_LEVEL],
        &[
            "test.kt:4:14: overload resolution ambiguity between candidates:",
            "| val SOME: Int",
            "| enum entry SOME: SomeEnum",
            "| enum entry SOME: SomeEnum",
        ],
    );
}

/// Compile and run `sources` with the standard library and JDK.
fn run_box(sources: &[(&str, &str)]) -> Option<String> {
    common::compile_and_run_box_files(
        sources,
        &[common::stdlib_jar()],
        Some(&common::jdk_modules()),
    )
}

#[test]
fn a_nearer_rung_selects_its_own_value() {
    // The explicit import outranks the star imports, and the current package outranks them too.
    let explicit = "\
import second.SomeEnum.SOME
import third.SomeEnum.*
fun box(): String = if (SOME == second.SomeEnum.SOME) \"OK\" else \"fail\"
";
    assert_eq!(
        run_box(&[("test.kt", explicit), SECOND_ENUM, THIRD_ENUM]).as_deref(),
        Some("OK")
    );
    let own_package = "\
import second.SomeEnum.*
val SOME = \"OK\"
fun box(): String = SOME
";
    assert_eq!(
        run_box(&[("test.kt", own_package), SECOND_ENUM]).as_deref(),
        Some("OK")
    );
}
