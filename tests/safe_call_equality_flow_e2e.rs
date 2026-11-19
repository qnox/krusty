//! A safe call narrows its receivers only in the branch where its comparison is known to hold.

use super::common;
use super::diagnostics_parity_support::{errors, ObservedError};

const NULLABLE_RECEIVER: &str =
    "only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'H?'.";

#[test]
fn identity_and_inequality_branches_match_kotlinc() {
    let source = "class H(val p: String)\n\
                  \n\
                  fun identityElse(h: H?, v: String): Int = if (h?.p === v) 0 else h.p.length\n\
                  \n\
                  fun notIdentityThen(h: H?, v: String): Int = if (h?.p !== v) h.p.length else 0\n\
                  \n\
                  fun identityThen(h: H?, v: String): Int = if (h?.p === v) h.p.length else -1\n\
                  \n\
                  fun notIdentityElse(h: H?, v: String): Int = if (h?.p !== v) -1 else h.p.length\n\
                  \n\
                  fun notEqualsThen(h: H?, v: String): Int = if (h?.p != v) h.p.length else 0\n\
                  \n\
                  fun notEqualsElse(h: H?, v: String): Int = if (h?.p != v) 0 else h.p.length\n";
    let result = common::compiler_diagnostics(&[("Flow.kt", source)], &[common::stdlib_jar()]);
    assert_eq!((result.krusty_code, result.reference_code), (1, 1));

    let rejected = |line: usize, column: usize| ObservedError {
        file: "Flow.kt".to_string(),
        line,
        column,
        message: NULLABLE_RECEIVER.to_string(),
    };
    let expected = vec![rejected(3, 67), rejected(5, 63), rejected(11, 60)];
    assert_eq!(errors(&result.reference_stderr), expected);
    let mut krusty = errors(&result.krusty_stderr);
    krusty.extend(errors(&result.krusty_stdout));
    assert_eq!(krusty, expected);
}
