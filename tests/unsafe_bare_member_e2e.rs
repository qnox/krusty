//! A bare member of a nullable implicit receiver is kotlinc's UNSAFE_CALL, not an unresolved name.

use super::common;
use super::diagnostics_parity_support::{errors, ObservedError};

#[test]
fn bare_property_of_a_nullable_extension_receiver_is_an_unsafe_call() {
    let source = "interface A { val marker: String }\n\
                  fun A?.plain(): String = marker\n";
    let result = common::compiler_diagnostics(&[("UnsafeBare.kt", source)], &[]);
    assert_eq!((result.krusty_code, result.reference_code), (1, 1));
    let expected = vec![ObservedError {
        file: "UnsafeBare.kt".to_string(),
        line: 2,
        column: 26,
        message: "only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable \
                  receiver of type 'A?'."
            .to_string(),
    }];
    assert_eq!(errors(&result.krusty_stderr), expected);
    assert_eq!(errors(&result.reference_stderr), expected);
}

#[test]
fn bare_property_of_a_nullable_lambda_receiver_is_an_unsafe_call() {
    let source = "interface A { val marker: String }\n\
                  fun use(a: A?): String = with(a) { marker }\n";
    let result = common::compiler_diagnostics(&[("UnsafeLambda.kt", source)], &[]);
    assert_eq!((result.krusty_code, result.reference_code), (1, 1));
    let expected = vec![ObservedError {
        file: "UnsafeLambda.kt".to_string(),
        line: 2,
        column: 36,
        message: "only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable \
                  receiver of type 'A?'."
            .to_string(),
    }];
    assert_eq!(errors(&result.krusty_stderr), expected);
    assert_eq!(errors(&result.reference_stderr), expected);
}
