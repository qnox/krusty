//! kotlinc builds a primary constructor's `super(…)` delegation at the declaration's start,
//! annotations included: its operands mark their own lines, and the call is back on that start line
//! at its first synthesized operand (an omitted default's placeholder, built at the call's offsets)
//! or at the `invokespecial`. The constructor's trailing `return` maps to the constructor's own
//! start: the class header for a declared parameter list, the declaration's start otherwise.
use super::common;

const SOURCE: &str = "package store\n\
                      \n\
                      fun make(n: Int): String = \"v$n\"\n\
                      \n\
                      open class Base(val s: String, val n: Int = 0)\n\
                      \n\
                      class OneLine : Base(make(1))\n\
                      \n\
                      class Defaulted : Base(\n\
                      \x20   make(2)\n\
                      )\n\
                      \n\
                      class Explicit : Base(\n\
                      \x20   make(3),\n\
                      \x20   4\n\
                      )\n\
                      \n\
                      class Parameter(x: Int) : Base(\n\
                      \x20   make(x)\n\
                      )\n\
                      \n\
                      class Split :\n\
                      \x20   Base(\n\
                      \x20       make(5),\n\
                      \x20       6\n\
                      \x20   )\n\
                      \n\
                      @Suppress(\"unused\")\n\
                      class Annotated : Base(make(7), 8)\n\
                      \n\
                      @Suppress(\"unused\")\n\
                      class AnnotatedBody : Base(make(9), 10) {\n\
                      \x20   val q = make(11)\n\
                      }\n\
                      \n\
                      @Suppress(\"unused\")\n\
                      class AnnotatedParameters(\n\
                      \x20   val a: Int\n\
                      ) : Base(make(a), 12)\n\
                      \n\
                      class Concatenated(y: Int) : Base(\n\
                      \x20   \"a\" + y\n\
                      )\n\
                      \n\
                      class Secondary : Base {\n\
                      \x20   constructor(x: Int) : super(\n\
                      \x20       make(x)\n\
                      \x20   )\n\
                      }\n";

#[test]
fn super_delegation_lines_are_byte_identical_to_kotlinc() {
    for class in [
        "store/Annotated",
        "store/AnnotatedBody",
        "store/AnnotatedParameters",
        "store/Concatenated",
        "store/Defaulted",
        "store/Explicit",
        "store/OneLine",
        "store/Parameter",
        "store/Secondary",
        "store/Split",
    ] {
        common::byte_diff_against_kotlinc_cp(
            "SuperDelegationLine",
            SOURCE,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}
