//! Exact shared-language diagnostic wording and location parity with kotlinc.

use super::common;
use super::diagnostics_parity_support::{errors, first_error};

#[test]
fn shared_diagnostic_wording_matches_kotlinc() {
    let source = "fun breakOutside() { break }\n\
                  fun continueOutside() { continue }\n\
                  fun varargs(vararg first: Int, vararg second: Int) {}\n\
                  open class Base\n\
                  class Derived : Base() { override fun absent() {} }\n\
                  val topLevelThis: Any = this\n\
                  class ConstructorThis(val value: Any = this)";
    let stdlib = common::stdlib_jar();
    let result = common::compiler_diagnostics(
        &[("SharedDiagnostics.kt", source)],
        std::slice::from_ref(&stdlib),
    );
    assert_ne!(result.krusty_code, 0, "krusty silently accepted source");
    assert_ne!(
        result.reference_code, 0,
        "kotlinc unexpectedly accepted source"
    );
    let mut krusty_errors = errors(&result.krusty_stderr);
    krusty_errors.extend(errors(&result.krusty_stdout));
    let mut kotlinc_errors = errors(&result.reference_stderr);
    krusty_errors.sort_by_key(|error| (error.line, error.column));
    kotlinc_errors.sort_by_key(|error| (error.line, error.column));
    assert_eq!(krusty_errors, kotlinc_errors);
    assert_eq!(
        krusty_errors
            .iter()
            .map(|error| error.message.as_str())
            .collect::<Vec<_>>(),
        [
            "'break' and 'continue' are only allowed inside loops.",
            "'break' and 'continue' are only allowed inside loops.",
            "multiple vararg parameters are prohibited.",
            "multiple vararg parameters are prohibited.",
            "'absent' overrides nothing.",
            "'this' is not defined in this context.",
            "cannot access '<this>' before the instance has been initialized.",
        ]
    );
}

#[test]
fn prohibited_script_returns_match_kotlinc() {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    for (filename, source) in [
        ("ReturnStatement.kts", "return"),
        ("ReturnExpression.kts", "null ?: return"),
    ] {
        let input = krusty::frontend::SourceInput::kotlin_script(source);
        let krusty_diagnostics = common::front_end_diagnostics_inputs(
            &[input],
            std::slice::from_ref(&stdlib),
            Some(jdk.as_path()),
        );
        let (reference_code, reference_stderr) =
            common::kotlinc_named_source_result(filename, source);
        assert_ne!(
            reference_code, 0,
            "kotlinc unexpectedly accepted {source:?}"
        );
        let reference_error = first_error(&reference_stderr)
            .unwrap_or_else(|| panic!("kotlinc emitted no location diagnostic for {source:?}"));
        assert_eq!(
            krusty_diagnostics,
            [reference_error.message],
            "script source: {source:?}"
        );
        assert_eq!(
            krusty_diagnostics,
            ["'return' is prohibited here."],
            "script source: {source:?}"
        );
    }
}
