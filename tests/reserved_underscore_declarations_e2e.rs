//! A declaration named with underscores only and without back-ticks is reserved: kotlinc reports
//! `UNDERSCORE_IS_RESERVED` at the name of a classifier, type alias, function or property, while a
//! back-ticked name or one that only starts with an underscore is ordinary.

use super::common;
use super::diagnostics_parity_support::{errors, ObservedError};

#[test]
fn an_unescaped_underscore_declaration_name_is_reserved() {
    const SOURCE: &str = "class Inv<T>\n\
                          fun <E> Inv<E>._() {}\n\
                          fun __() {}\n\
                          val ___ = 1\n\
                          val _x = 1\n\
                          fun `____`() {}\n\
                          class _\n\
                          typealias _____ = Inv<Int>\n\
                          class C {\n\
                          \x20   fun _() {}\n\
                          \x20   val __: Int = 1\n\
                          }\n";
    let result = common::compiler_diagnostics(&[("ReservedUnderscores.kt", SOURCE)], &[]);
    let reserved = |line, column| ObservedError {
        file: "ReservedUnderscores.kt".to_string(),
        line,
        column,
        message: "names _, __, ___, ... are reserved in Kotlin.".to_string(),
    };
    let expected = vec![
        reserved(2, 16),
        reserved(3, 5),
        reserved(4, 5),
        reserved(7, 7),
        reserved(8, 11),
        reserved(10, 9),
        reserved(11, 9),
    ];
    assert_eq!(
        errors(&result.reference_stderr),
        expected,
        "kotlinc's diagnostics"
    );
    let mut krusty_errors = errors(&result.krusty_stderr);
    krusty_errors.extend(errors(&result.krusty_stdout));
    assert_eq!(krusty_errors, expected, "krusty's diagnostics");
    assert_ne!(result.krusty_code, 0, "krusty accepted the reserved names");
}
