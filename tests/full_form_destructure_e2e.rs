//! `(val a, val b) = expression` without `NameBasedDestructuring` is the language-version
//! diagnostic kotlinc reports on each `val`. It is not a parse error: a parse error drops the
//! file's signatures, and capture discovery then panics with "checked class has a collected
//! signature" on the file's anonymous object.

use super::common;

const SOURCE: &str = "\
data class Pair2(val a: Int, val b: Int)\n\
interface Marker\n\
fun select() {\n\
(val a, val b) = Pair2(1, 2)\n\
object : Marker {}\n\
}\n\
";

#[test]
fn full_form_destructure_reports_the_language_version() {
    let mut diagnostics = krusty::diag::DiagSink::new();
    let (_file, symbols, info) =
        krusty::frontend::analyze_source_standalone(SOURCE, &mut diagnostics);
    assert!(symbols.is_some());
    assert!(info.is_some());
    let rendered = diagnostics.render("Full.kt", SOURCE);
    assert_eq!(
        rendered,
        "\
Full.kt:4:2: error: the feature \"name based destructuring\" is only available since language version 2.5
(val a, val b) = Pair2(1, 2)
 ^^^
Full.kt:4:9: error: the feature \"name based destructuring\" is only available since language version 2.5
(val a, val b) = Pair2(1, 2)
        ^^^
"
    );
    common::assert_errors_match_kotlinc(&[("Full.kt", SOURCE)], &[]);
}
