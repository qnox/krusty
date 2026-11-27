//! An omitted parameter's default of a same-module inline function is evaluated inside the
//! expansion's frame, as kotlinc inlines the callee's `$default` body: its lines map through the
//! callee's source, the supplied operands' ranges start where the frame opens, and the line is
//! written again where the real body begins.

use super::common;

const LIB: &str = "package lib\n\
                   \n\
                   inline fun inlineFoo1(a: Int, vararg b: String, c: Int = 0) = 0\n\
                   inline fun Int.inlineFoo2(vararg b: String, c: Int = 0) = 0\n\
                   inline fun nested() = 7\n\
                   inline fun withNestedDefault(a: Int = nested()) = a + 1\n\
                   inline fun readsSuppliedOperand(a: Int, b: Int = nested() + a) = a + b\n\
                   inline fun twoNestedDefaults(a: Int = nested(), b: Int = nested() + a) = a + b\n\
                   inline fun f4(a: Int = 1, b: Int = nested()) = a + b\n\
                   inline fun lambdaDefault(a: Int = 1, noinline b: () -> Int = { nested() }) = a + b()\n\
                   inline fun suppliedMiddle(a: Int = nested(), b: Int = nested(), c: Int = nested() + 1) = a + b + c\n";

fn assert_matches_kotlinc(main: &str) {
    let classes = common::classes_against_kotlinc_module(&[("lib.kt", LIB), ("main.kt", main)]);
    let differences = classes.differences();
    assert!(
        differences.is_empty(),
        "classes differ from kotlinc's:\n\n{}",
        differences.join("\n\n")
    );
}

#[test]
fn an_inline_call_with_defaults_and_a_constant_body_matches_kotlincs_lines() {
    assert_matches_kotlinc(
        "package main\n\
         \n\
         import lib.inlineFoo1\n\
         \n\
         fun t(): Int {\n\
         \x20   val x = inlineFoo1(1, \"s\")\n\
         \x20   return x\n\
         }\n",
    );
}

#[test]
fn an_inline_adapted_reference_that_fills_a_default_matches_kotlincs_lines() {
    assert_matches_kotlinc(
        "package main\n\
         \n\
         import lib.inlineFoo1\n\
         import lib.inlineFoo2\n\
         \n\
         fun defaultAndVarargConversion(ref: Int.(String) -> Int) = ref\n\
         fun defaultVarargAndReceiverConversion(ref: (Int, String) -> Int) = ref\n\
         \n\
         fun testInline() {\n\
         \x20   defaultAndVarargConversion(::inlineFoo1)\n\
         \x20   defaultAndVarargConversion(Int::inlineFoo2)\n\
         \x20   defaultVarargAndReceiverConversion(Int::inlineFoo2)\n\
         }\n",
    );
}

#[test]
fn an_inline_call_whose_omitted_default_is_itself_an_inline_call_matches_kotlincs_classes() {
    assert_matches_kotlinc(
        "package main\n\
         \n\
         import lib.withNestedDefault\n\
         \n\
         fun t(): Int {\n\
         \x20   val x = withNestedDefault()\n\
         \x20   return x\n\
         }\n",
    );
}

#[test]
fn an_inline_call_with_two_omitted_nested_inline_defaults_matches_kotlincs_classes() {
    assert_matches_kotlinc(
        "package main\n\
         \n\
         import lib.twoNestedDefaults\n\
         \n\
         fun t(): Int {\n\
         \x20   val x = twoNestedDefaults()\n\
         \x20   return x\n\
         }\n",
    );
}

#[test]
fn a_nested_inline_default_reading_a_supplied_operand_matches_kotlincs_classes() {
    assert_matches_kotlinc(
        "package main\n\
         \n\
         import lib.readsSuppliedOperand\n\
         \n\
         fun t(): Int {\n\
         \x20   val x = readsSuppliedOperand(1)\n\
         \x20   return x\n\
         }\n",
    );
}

#[test]
fn a_supplied_first_parameter_before_a_nested_inline_default_matches_kotlincs_classes() {
    assert_matches_kotlinc(
        "package main\n\
         \n\
         import lib.twoNestedDefaults\n\
         \n\
         fun t(): Int {\n\
         \x20   val x = twoNestedDefaults(1)\n\
         \x20   return x\n\
         }\n",
    );
}

#[test]
fn a_named_argument_after_an_omitted_constant_default_matches_kotlincs_classes() {
    assert_matches_kotlinc(
        "package main\n\
         \n\
         import lib.f4\n\
         \n\
         fun t(): Int {\n\
         \x20   val x = f4(b = 2)\n\
         \x20   return x\n\
         }\n",
    );
}

#[test]
fn a_supplied_parameter_between_omitted_nested_inline_defaults_matches_kotlincs_classes() {
    assert_matches_kotlinc(
        "package main\n\
         \n\
         import lib.suppliedMiddle\n\
         \n\
         fun t(): Int {\n\
         \x20   val x = suppliedMiddle(b = 3)\n\
         \x20   return x\n\
         }\n",
    );
}

#[test]
fn a_supplied_lambda_whose_default_would_call_an_inline_function_matches_kotlincs_classes() {
    assert_matches_kotlinc(
        "package main\n\
         \n\
         import lib.lambdaDefault\n\
         \n\
         fun t(): Int {\n\
         \x20   val x = lambdaDefault(b = { 2 })\n\
         \x20   return x\n\
         }\n",
    );
}
