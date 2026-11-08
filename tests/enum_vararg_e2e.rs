//! A `vararg` primary-constructor parameter on an enum class (`enum class E(vararg val xs: Int)`) is
//! exposed as an array; an entry that supplies no arguments constructs an empty array. Runnable.
use super::common;
fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn enum_vararg_val_empty() {
    const SRC: &str = "enum class E(vararg val xs: Int) {\n\
        \x20 A;\n\
        }\n\
        fun box(): String = if (E.A.xs.size == 0) \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC).expect("enum vararg empty"), "OK");
}

#[test]
fn enum_plain_vararg_param_body_prop() {
    const SRC: &str = "enum class Test(vararg xs: Int) {\n    OK;\n    val values = xs\n}\nfun box(): String = if (Test.OK.values.size == 0) \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC).expect("enum plain vararg"), "OK");
}

#[test]
fn enum_entry_vararg_takes_an_empty_or_named_whole_array() {
    // `arrayOf()` takes its element type from the selected vararg parameter, and a named argument
    // passes its array whole rather than as one element.
    common::expect_box_same_as_kotlinc(
        include_str!("box_regressions/emptyArrayOfEnumEntryDelegation.kt"),
        "EnumEntryVarargArray",
    );
}

#[test]
fn enum_entry_takes_a_shadowing_array_factory_as_an_ordinary_call() {
    // A declared zero-argument `arrayOf` shadows the stdlib factory in an enum entry argument too.
    common::expect_box_same_as_kotlinc(
        include_str!("box_regressions/emptyArrayOfShadowedEnumFactory.kt"),
        "EnumEntryShadowedArrayFactory",
    );
}
