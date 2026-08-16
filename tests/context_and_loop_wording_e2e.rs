//! Complete diagnostic differentials for a missing context argument and an invalid loop-range
//! `hasNext`. Extracted message templates are audit leads; the emitted compiler output is proof.

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

fn compare(file: &str, source: &str) -> Vec<ObservedError> {
    let stdlib = common::stdlib_jar();
    let result = common::compiler_diagnostics(&[(file, source)], std::slice::from_ref(&stdlib));
    assert_ne!(result.krusty_code, 0, "krusty silently accepted source");
    assert_ne!(result.reference_code, 0, "kotlinc silently accepted source");
    let mut krusty = errors(&result.krusty_stderr);
    krusty.extend(errors(&result.krusty_stdout));
    let reference = errors(&result.reference_stderr);
    assert_eq!(krusty, reference);
    krusty
}

#[test]
fn a_missing_context_argument_names_the_context_parameter() {
    let errors = compare(
        "ContextProperty.kt",
        "// LANGUAGE: +ContextParameters\n\
         class C\n\
         context(c: C) val label: String get() = \"OK\"\n\
         fun box(): String = label\n",
    );
    assert_eq!(
        errors
            .iter()
            .map(|error| error.message.as_str())
            .collect::<Vec<_>>(),
        ["no context argument for 'c: C' found."]
    );
}

#[test]
fn a_loop_range_whose_has_next_is_not_boolean_names_the_iterator() {
    let errors = compare(
        "LoopRange.kt",
        "class Cursor {\n\
         \x20   operator fun hasNext(): Int = 0\n\
         \x20   operator fun next(): String = \"x\"\n\
         }\n\
         class Range { operator fun iterator(): Cursor = Cursor() }\n\
         fun box() { for (item in Range()) {} }\n",
    );
    assert_eq!(
        errors
            .iter()
            .map(|error| error.message.as_str())
            .collect::<Vec<_>>(),
        [
            "'operator' modifier is not applicable to function: must return 'Boolean'.",
            "the 'iterator().hasNext()' function of the loop range must return 'Boolean', but returns 'Int'."
        ]
    );
}
