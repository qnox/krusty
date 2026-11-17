//! Initializers report their value; assignment retains its operator boundary.

use super::common;

const SOURCE: &str = include_str!("fixtures/diagnostics/initializerValueSpans.kt");

#[test]
fn initializer_and_assignment_errors_keep_their_own_source_boundaries() {
    let sources = [("InitializerValueSpans.kt", SOURCE)];
    common::assert_error_blocks_match_kotlinc(&sources, &[]);
    assert_eq!(
        common::reference_error_blocks(&sources, &[]),
        [
            "InitializerValueSpans.kt:1:19: initializer type mismatch: expected 'String', actual 'Int'.",
            "InitializerValueSpans.kt:3:26: initializer type mismatch: expected 'String', actual 'Int'.",
            "InitializerValueSpans.kt:5:29: initializer type mismatch: expected 'String', actual 'Int'.",
            "InitializerValueSpans.kt:7:18: assignment type mismatch: actual type is 'Int', but 'String' was expected.",
            "InitializerValueSpans.kt:8:30: null cannot be a value of a non-null type 'String'.",
            "InitializerValueSpans.kt:13:32: initializer type mismatch: expected 'String', actual 'Int'.",
        ]
    );
}
