//! Explicit type-argument bounds and lambda shapes, pinned to kotlinc 2.4.21.
//!
//! A bound violation is applicability while another top-level overload shares the call, and the
//! selected call's own diagnostic when that overload is the only one. The lambda is shaped from
//! the candidates that decision leaves, once.

use super::common;

const ONE_VALID: &str = "\
@JvmName(\"pickText\")\n\
fun <T : CharSequence> pick(block: (Int) -> Int): String = \"text\"\n\
@JvmName(\"pickNumber\")\n\
fun <T : Number> pick(block: (T) -> Int): String = \"number\"\n\
fun use(): String = pick<String> { v -> v.length }\n\
";

const BOTH_VIOLATE: &str = "\
fun <T : Number> pick(block: (T) -> Int): String = \"number\"\n\
fun <T : List<Int>> pick(block: (T, T) -> Int): String = \"list\"\n\
fun use(): String = pick<String> { v -> v.length }\n\
";

const ONLY_OVERLOAD: &str = "\
fun <T : Number> pick(block: (T) -> Int): String = \"number\"\n\
fun use(): String = pick<String> { v -> v.length }\n\
";

fn assert_pinned(file: &str, source: &str, diagnostics: usize, ledger: &[&str]) {
    let sources = [(file, source)];
    let kotlinc = common::reference_error_blocks(&sources, &[]);
    let headers = kotlinc
        .iter()
        .filter(|line| !line.starts_with("| "))
        .count();
    assert_eq!(
        kotlinc, ledger,
        "{file}: kotlinc's complete diagnostics changed"
    );
    assert_eq!(headers, diagnostics, "{file}: diagnostic count");
    common::assert_error_blocks_match_kotlinc(&sources, &[]);
}

/// One overload's explicit type argument is inside its bound and the other's is not. The valid
/// overload shapes the lambda (`v` is `Int`); the violating overload contributes no shape and no
/// bound diagnostic.
#[test]
fn valid_overload_shapes_when_another_explicit_type_argument_violates_its_bound() {
    assert_pinned(
        "OneValid.kt",
        ONE_VALID,
        1,
        &["OneValid.kt:5:43: unresolved reference 'length' on receiver of type 'Int'."],
    );
}

/// Both overloads' explicit type arguments violate their bounds. Neither shapes the lambda, even
/// the overload whose function type has one value parameter. The call is inapplicable and `v`
/// stays untyped.
#[test]
fn both_overloads_violating_explicit_bounds_leave_the_lambda_unshaped() {
    assert_pinned(
        "BothViolate.kt",
        BOTH_VIOLATE,
        3,
        &[
            "BothViolate.kt:3:21: none of the following candidates is applicable:",
            "| ",
            "| fun <T : Number> pick(block: (T) -> Int): String:",
            "|   Parameter type mismatch: actual type is '??? (Unknown type for value parameter v)', but 'uninferred T (of fun <T : Number> pick)' was expected.",
            "| ",
            "| fun <T : List<Int>> pick(block: (T, T) -> Int): String:",
            "|   Argument type mismatch: actual type is '(uninferred T (of fun <T : List<Int>> pick)) -> Int', but '(uninferred T (of fun <T : List<Int>> pick), uninferred T (of fun <T : List<Int>> pick)) -> Int' was expected.",
            "BothViolate.kt:3:36: cannot infer type for value parameter 'v'. Specify it explicitly.",
            "BothViolate.kt:3:43: unresolved reference 'length'.",
        ],
    );
}

/// The only overload is the selected call. Its explicit type argument still violates the bound,
/// and the lambda is shaped against that argument: `(String) -> Int`.
#[test]
fn only_overload_shapes_its_lambda_beside_the_explicit_bound_diagnostic() {
    assert_pinned(
        "OnlyOverload.kt",
        ONLY_OVERLOAD,
        2,
        &[
            "OnlyOverload.kt:2:26: type argument is not within its bounds: type parameter 'T (of fun <T : Number> pick)' must be subtype of 'Number', but actual: 'String'.",
            "OnlyOverload.kt:2:34: argument type mismatch: actual type is '(String) -> Int', but '(String) -> Int' was expected.",
        ],
    );
}
