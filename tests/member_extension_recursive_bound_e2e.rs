//! A member extension whose type parameter has a recursive bound (`R : Comparable<R>`).
//!
//! Before a lambda argument is analyzed, its call's unsolved formal stands for its declared bound.
//! That bound names the formal itself, and binding `R` to `Comparable<R>` made every later
//! substitution grow the type by one level, forever: compiling the kotlin repository's
//! `MarkdownReportRenderer` never finished. kotlinc approximates the self-reference to a star
//! projection, `Comparable<*>`.

use super::common;

const SOURCE: &str = "class Stat(val nanos: Long)\n\
    class Report(val stats: List<Stat>) {\n\
    \x20 fun render(): String = buildString {\n\
    \x20\x20 top(\"total\") { 7L }\n\
    \x20 }\n\
    \x20 private fun <R : Comparable<R>> StringBuilder.top(name: String, measure: () -> R) {\n\
    \x20\x20 append(name).append('=').append(measure())\n\
    \x20 }\n\
    }\n\
    fun box(): String {\n\
    \x20 val rendered = Report(listOf(Stat(1))).render()\n\
    \x20 return if (rendered == \"total=7\") \"OK\" else rendered\n\
    }\n";

#[test]
fn a_recursively_bounded_member_extension_compiles_as_kotlinc_does() {
    let classes = common::classes_against_kotlinc_module(&[("Report.kt", SOURCE)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn a_recursively_bounded_member_extension_runs() {
    common::expect_box_ok_with_stdlib(SOURCE, "recursively bounded member extension");
}
