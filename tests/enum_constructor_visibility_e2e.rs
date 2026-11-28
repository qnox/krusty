//! An enum class's constructors are private, so one that writes another visibility is an error
//! (NON_PRIVATE_CONSTRUCTOR_IN_ENUM) over the whole declaration, primary constructor first.

use super::common;
use super::diagnostics_parity_support::errors;

#[test]
fn non_private_enum_constructors_are_rejected_like_kotlinc() {
    let source = "enum class E public constructor(val x: Int) {\n\
                  \x20   FIRST(1);\n\
                  \n\
                  \x20   internal constructor(): this(42)\n\
                  \n\
                  \x20   protected constructor(a: Int, b: Int, c: Int): this(a + b + c)\n\
                  \n\
                  \x20   private constructor(y: Int, z: Int): this(y + z)\n\
                  \n\
                  \x20   constructor(w: String): this(w.length)\n\
                  }\n\
                  \n\
                  enum class Fine private constructor(val x: Int) {\n\
                  \x20   A(1)\n\
                  }\n\
                  enum class Implicit(val x: Int) {\n\
                  \x20   A(1);\n\
                  \x20   constructor(): this(0)\n\
                  }\n";
    let stdlib = common::stdlib_jar();
    let result = common::compiler_diagnostics(
        &[("EnumConstructors.kt", source)],
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
            .map(|error| (error.line, error.column, error.message.as_str()))
            .collect::<Vec<_>>(),
        [
            (1, 14, "constructor must be private in enum class."),
            (4, 5, "constructor must be private in enum class."),
            (6, 5, "constructor must be private in enum class."),
        ]
    );
}
