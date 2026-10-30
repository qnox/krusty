//! Complete ordinary array-bottom diagnostics, independent of class-literal feature policy.

use super::common;

const FILE: &str = "Arrays.kt";
const NOTHING: &str = "expression cannot have a type of 'Array<Nothing>'.";
const REIFIED: &str = "cannot use 'Nothing' as reified type parameter.";

fn diagnostic(line: usize, column: usize, message: &str) -> common::CompilerError {
    common::CompilerError {
        file: FILE.to_string(),
        line,
        column,
        message: message.to_string(),
    }
}

fn assert_diagnostics(source: &str, args: &[String], errors: Vec<common::CompilerError>) {
    let result = common::compiler_diagnostics_with_shared_args(
        &[(FILE, source)],
        &[common::stdlib_jar()],
        args,
    );
    assert_eq!(common::compiler_errors(&result.krusty_stdout), []);
    assert_eq!(common::compiler_warnings(&result.krusty_stdout), []);
    assert_eq!(common::compiler_errors(&result.krusty_stderr), errors);
    assert_eq!(common::compiler_errors(&result.reference_stderr), errors);
    assert_eq!(common::compiler_warnings(&result.krusty_stderr), []);
    assert_eq!(common::compiler_warnings(&result.reference_stderr), []);
    assert_eq!(result.krusty_code == 0, result.reference_code == 0);
}

#[test]
fn array_factories_keep_both_expression_and_reified_bottom_errors() {
    assert_diagnostics(
        "fun emptyFactory() = emptyArray<Nothing>()\n\
         fun varargFactory() = arrayOf<Nothing>()\n\
         fun sizeFactory() = Array<Nothing>(0) { error(\"unused\") }\n",
        &[],
        vec![
            diagnostic(1, 22, NOTHING),
            diagnostic(1, 33, REIFIED),
            diagnostic(2, 23, NOTHING),
            diagnostic(2, 31, REIFIED),
            diagnostic(3, 21, NOTHING),
            diagnostic(3, 27, REIFIED),
        ],
    );
}

#[test]
fn nested_array_bottom_is_checked_at_the_value_expression() {
    assert_diagnostics(
        "class Cell<T>(val value:T)\n\
         fun nested(@Suppress(\"UNSUPPORTED\") value: Cell<Array<Nothing>>) = value\n",
        &[],
        vec![diagnostic(2, 68, NOTHING)],
    );
}

const SUPPRESSED: &str = "\
@file:Suppress(\"UNSUPPORTED\")
fun passthrough(value: Array<Nothing>) = value
";

#[test]
fn the_ordinary_expression_error_respects_its_own_suppression() {
    // This fixture explicitly disables warnings; ERROR_SUPPRESSION warning rendering is a
    // separate policy from whether UNSUPPORTED remains suppressed on the returned value.
    assert_diagnostics(SUPPRESSED, &["-nowarn".to_string()], vec![]);
}

#[test]
fn suppressed_bottom_array_values_keep_the_complete_facade_abi() {
    // PLATFORM: JVM physical method signatures for a suppressed semantic array type.
    let classes = common::classes_against_kotlinc_module(&[("Suppressed.kt", SUPPRESSED)]);
    assert_eq!(
        classes.krusty.keys().collect::<Vec<_>>(),
        classes.reference.keys().collect::<Vec<_>>()
    );
    for (name, reference) in &classes.reference {
        assert_eq!(
            common::member_table(&classes.krusty[name]),
            common::member_table(reference),
            "{name} complete member table"
        );
    }
}
