//! Exact diagnostics for `Array<Nothing>` and `Array<Nothing?>` class-literal left-hand sides.

use super::common;

const FORBID: &str = "-XXLanguage:+ForbidArrayOfNothingInLhsOfClassLiteral";
const FILE: &str = "ClassLiterals.kt";
const NOTHING: &str = "expression cannot have a type of 'Array<Nothing>'.";
const NULLABLE_NOTHING: &str = "'Array<Nothing?>' is not supported on the JVM.";

const CLASS_LITERALS: &str = "\
fun test() {
    val a = Array<Nothing>::class
    val b = Array<Nothing?>::class
    val f = kotlin.Array<Nothing>::class
    val h = Array<String>::class
}
@Suppress(\"UNSUPPORTED_ARRAY_OF_NOTHING_IN_CLASS_LITERAL_LHS\")
fun suppressed() { val a = Array<Nothing>::class }
";

fn diagnostic(line: usize, column: usize, message: &str) -> common::CompilerError {
    common::CompilerError {
        file: FILE.to_string(),
        line,
        column,
        message: message.to_string(),
    }
}

/// Compile with both compilers and require every error and warning, including file and order.
fn assert_matches_kotlinc(
    arguments: &[&str],
    errors: Vec<common::CompilerError>,
    warnings: Vec<common::CompilerError>,
) {
    let arguments = arguments
        .iter()
        .map(|argument| (*argument).to_string())
        .collect::<Vec<_>>();
    let result = common::compiler_diagnostics_with_shared_args(
        &[(FILE, CLASS_LITERALS)],
        &[common::stdlib_jar()],
        &arguments,
    );
    assert_eq!(
        common::compiler_errors(&result.krusty_stdout),
        [],
        "krusty errors on stdout"
    );
    assert_eq!(
        common::compiler_warnings(&result.krusty_stdout),
        [],
        "krusty warnings on stdout"
    );
    assert_eq!(common::compiler_errors(&result.krusty_stderr), errors);
    assert_eq!(common::compiler_warnings(&result.krusty_stderr), warnings);
    assert_eq!(common::compiler_errors(&result.reference_stderr), errors);
    assert_eq!(
        common::compiler_warnings(&result.reference_stderr),
        warnings
    );
    assert_eq!(result.krusty_code == 0, result.reference_code == 0);
}

#[test]
fn array_of_nothing_class_literals_are_exact_warnings() {
    assert_matches_kotlinc(
        &[],
        vec![],
        vec![
            diagnostic(2, 13, NOTHING),
            diagnostic(3, 13, NULLABLE_NOTHING),
            diagnostic(4, 13, NOTHING),
        ],
    );
}

#[test]
fn the_feature_turns_every_array_of_nothing_class_literal_into_an_error() {
    assert_matches_kotlinc(
        &[FORBID],
        vec![
            diagnostic(2, 13, NOTHING),
            diagnostic(3, 13, NULLABLE_NOTHING),
            diagnostic(4, 13, NOTHING),
            diagnostic(8, 28, NOTHING),
        ],
        vec![],
    );
}
