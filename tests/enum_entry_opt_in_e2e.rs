//! An enum entry calls a constructor, so it needs the opt-in its constructor requires: at the
//! empty range after the name when it writes the call, at the name when it does not.

use super::common;
use super::diagnostics_parity_support::errors;

const SOURCE: &str = "@RequiresOptIn
annotation class O

enum class Enum1 @O constructor() {
    ENTRY(),
    ENTRY2,
    @OptIn(O::class) ENTRY3;
}

enum class Enum2 {
    ENTRY(),
    ENTRY2,
    ENTRY3(0);

    val x: Int

    @O constructor(x: Int = 0) {
        this.x = x
    }
}

enum class Enum3 @O constructor(x: Int = 42) {
    ENTRY(),
    ENTRY2,
    ENTRY3(3);

    val x: Int = x

    @OptIn(O::class)
    constructor() : this(0)
}
";

#[test]
fn enum_entries_need_the_opt_in_of_their_constructor_like_kotlinc() {
    let stdlib = common::stdlib_jar();
    let result = common::compiler_diagnostics(
        &[("EnumEntryOptIn.kt", SOURCE)],
        std::slice::from_ref(&stdlib),
    );
    assert_ne!(result.krusty_code, 0, "krusty silently accepted source");
    assert_ne!(
        result.reference_code, 0,
        "kotlinc unexpectedly accepted source"
    );
    let mut krusty_errors = errors(&result.krusty_stderr);
    krusty_errors.extend(errors(&result.krusty_stdout));
    let kotlinc_errors = errors(&result.reference_stderr);
    assert_eq!(krusty_errors, kotlinc_errors);
    assert_eq!(
        krusty_errors
            .iter()
            .map(|error| (error.line, error.column))
            .collect::<Vec<_>>(),
        [(5, 5), (6, 5), (11, 5), (12, 5), (13, 5), (25, 5)]
    );
}
