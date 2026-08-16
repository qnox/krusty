//! Invoking something that is not a function. A selected member property is a resolved callee
//! expression, so kotlinc reports FUNCTION_EXPECTED with its type. Bare `name()` syntax instead
//! searches callable candidates: a non-callable local/parameter contributes none, a same-named
//! constructor may win, and an otherwise missing callable is an unresolved reference.

use std::path::Path;

use super::common;

#[derive(Debug, PartialEq, Eq)]
struct ObservedError {
    file: String,
    line: usize,
    column: usize,
    message: String,
}

fn errors(output: &str) -> Vec<ObservedError> {
    output
        .lines()
        .filter_map(|rendered| {
            let (location, message) = rendered.split_once("error:")?;
            let location = location.trim().trim_end_matches(':');
            let mut fields = location.rsplitn(3, ':');
            let column = fields.next()?.trim().parse().ok()?;
            let line = fields.next()?.trim().parse().ok()?;
            let path = fields.next()?.trim();
            Some(ObservedError {
                file: Path::new(path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or(path)
                    .to_string(),
                line,
                column,
                message: message.trim().to_string(),
            })
        })
        .collect()
}

#[test]
fn function_expected_diagnostics_match_kotlinc() {
    let source = "class Registry\n\
                  class Holder(val count: Int)\n\
                  val propertyCount = 1\n\
                  fun local() { val count = 1; count() }\n\
                  fun parameter(label: String) { label() }\n\
                  fun shadow() { val Registry = 1; Registry() }\n\
                  fun property() { propertyCount() }\n\
                  fun errored() { val bad = missing; bad() }\n\
                  fun member(holder: Holder) { holder.count() }";
    let stdlib = common::stdlib_jar();
    let result = common::compiler_diagnostics(
        &[("FunctionExpected.kt", source)],
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
            "unresolved reference 'count'.",
            "unresolved reference 'label'.",
            "unresolved reference 'propertyCount'.",
            "unresolved reference 'missing'.",
            "unresolved reference 'bad'.",
            "expression 'count' of type 'Int' cannot be invoked as a function. Function 'invoke()' is not found.",
        ]
    );
}
