//! A Kotlin atomic is its Kotlin declaration, not the JDK class that realizes it on the JVM: kotlinc
//! rejects extending the final `AtomicInt` and constructing it through `AtomicInteger`'s no-argument
//! constructor.

use super::common;
use super::diagnostics_parity_support::{errors, ObservedError};

#[test]
fn a_mapped_atomic_keeps_its_kotlin_modality_and_constructors() {
    let source = "@file:OptIn(kotlin.concurrent.atomics.ExperimentalAtomicApi::class)\n\
                  import kotlin.concurrent.atomics.*\n\
                  class Counter : AtomicInt(0)\n\
                  fun empty() = AtomicInt()\n";
    let stdlib = common::stdlib_jar();
    let result = common::compiler_diagnostics(
        &[("Atomics.kt", source)],
        std::slice::from_ref(&stdlib),
    );

    assert_ne!(result.krusty_code, 0, "krusty silently accepted source");
    assert_ne!(
        result.reference_code, 0,
        "kotlinc unexpectedly accepted source"
    );
    let mut krusty_errors = errors(&result.krusty_stderr);
    krusty_errors.extend(errors(&result.krusty_stdout));
    let error = |line, column, message: &str| ObservedError {
        file: "Atomics.kt".to_string(),
        line,
        column,
        message: message.to_string(),
    };
    let expected = vec![
        error(3, 17, "this type is final, so it cannot be extended."),
        error(4, 15, "no value passed for parameter 'value'."),
    ];
    assert_eq!(krusty_errors, expected);
    assert_eq!(errors(&result.reference_stderr), expected);
}
