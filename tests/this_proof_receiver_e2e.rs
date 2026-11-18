//! A bare member after `this is B` on a receiver unrelated to `B`: the proof keeps the declared
//! receiver beside `B`, and a nullable declaration stays nullable.

use super::common;
use super::diagnostics_parity_support::{errors, ObservedError};

#[test]
fn nullable_declared_receiver_stays_nullable_after_a_nullable_proof() {
    let source = "interface A { val marker: String }\n\
                  interface B\n\
                  fun A?.bad(): String {\n\
                  \x20   if (this is B?) return marker\n\
                  \x20   return \"other\"\n\
                  }\n";
    let result = common::compiler_diagnostics(&[("NullableProof.kt", source)], &[]);
    assert_eq!((result.krusty_code, result.reference_code), (1, 1));
    assert_eq!(
        errors(&result.krusty_stderr),
        vec![ObservedError {
            file: "NullableProof.kt".to_string(),
            line: 4,
            column: 28,
            message: "unresolved reference 'marker'.".to_string(),
        }]
    );
    assert_eq!(
        errors(&result.reference_stderr),
        vec![ObservedError {
            file: "NullableProof.kt".to_string(),
            line: 4,
            column: 28,
            message: "only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable \
                      receiver of type 'B? & A?'."
                .to_string(),
        }]
    );
}
