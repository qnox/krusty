//! A class of the program that implements a FUNCTION TYPE.
//!
//! A function value's body sits at a fixed vtable number — the runtime names it `KT_SLOT_INVOKE` —
//! and every caller through a function type reads it rather than asking, passing and reading
//! references, because that is the one signature every function value shares. A class whose
//! `invoke` carries references throughout stands there itself. One that does not — `class A : (Int)
//! -> Int` carries machine integers — cannot: a caller would read an integer as a pointer. It takes
//! a slot of its own and the fixed number gets a converting stand-in, which forwards by DISPATCH so
//! that a further subclass's override is reached through the same number.
//!
//! Every expectation is kotlinc's, taken by running the same program under it.

use super::common::{
    expect_box_ok_with_stdlib, expect_native_box, expect_native_decline, kotlinc_box_result,
};

/// Require kotlinc's answer, krusty's JVM answer and the NATIVE answer to agree.
fn every_backend_agrees_with_kotlinc(stem: &str, source: &str) {
    assert_eq!(
        kotlinc_box_result(source),
        "OK",
        "{stem}: unexpected kotlinc result"
    );
    expect_box_ok_with_stdlib(source, stem);
    expect_native_box(source, stem, "OK");
}

/// References throughout: the method stands in the fixed number itself.
#[test]
fn an_invoke_of_references_takes_the_function_slot() {
    let source = "class A : (String) -> String {\n\
         \x20   override fun invoke(p: String): String = p\n\
         }\n\
         fun box(): String {\n\
         \x20   val f: (String) -> String = A()\n\
         \x20   return f(\"OK\")\n\
         }\n";
    every_backend_agrees_with_kotlinc("InvokeOfReferences", source);
}

/// MACHINE INTEGERS, which is the shape that needs the stand-in: the operand is unboxed on the way
/// in and the answer boxed on the way out.
#[test]
fn an_invoke_of_machine_operands_is_reached_through_a_stand_in() {
    let source = "class A : (Int) -> Int {\n\
         \x20   override fun invoke(p: Int): Int = p + 1\n\
         }\n\
         fun box(): String {\n\
         \x20   val f: (Int) -> Int = A()\n\
         \x20   return if (f(1) == 2) \"OK\" else \"fail \" + f(1)\n\
         }\n";
    every_backend_agrees_with_kotlinc("InvokeOfMachineOperands", source);
}

/// The method is still reachable by its OWN name, at its own slot, which is what the stand-in
/// forwards through.
#[test]
fn the_method_keeps_its_own_slot() {
    let source = "class A : (Int) -> Int {\n\
         \x20   override fun invoke(p: Int): Int = p + 1\n\
         }\n\
         fun box(): String {\n\
         \x20   val a = A()\n\
         \x20   if (a.invoke(1) != 2) return \"fail direct\"\n\
         \x20   val f: (Int) -> Int = a\n\
         \x20   if (f(1) != 2) return \"fail through the type\"\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("InvokeKeepsItsOwnSlot", source);
}

/// A `Unit` answer: the stand-in has a reference to hand back where the body produces no machine
/// value at all, and the runtime's singleton is that value.
#[test]
fn a_unit_answer_crosses_the_stand_in() {
    let source = "var seen = 0\n\
         class A : (Int) -> Unit {\n\
         \x20   override fun invoke(p: Int) { seen += p }\n\
         }\n\
         fun box(): String {\n\
         \x20   val f: (Int) -> Unit = A()\n\
         \x20   f(2)\n\
         \x20   f(3)\n\
         \x20   return if (seen == 5) \"OK\" else \"fail \" + seen\n\
         }\n";
    every_backend_agrees_with_kotlinc("InvokeAnsweringUnit", source);
}

/// SEVERAL operands of mixed representation, so the conversion is pinned per position rather than
/// inferred from a single one.
#[test]
fn every_operand_converts_at_its_own_position() {
    let source = "class A : (Int, String, Boolean) -> String {\n\
         \x20   override fun invoke(n: Int, s: String, b: Boolean): String =\n\
         \x20       if (b) s + n else \"no\"\n\
         }\n\
         fun box(): String {\n\
         \x20   val f: (Int, String, Boolean) -> String = A()\n\
         \x20   val said = f(1, \"OK\", true)\n\
         \x20   return if (said == \"OK1\") \"OK\" else \"fail \" + said\n\
         }\n";
    every_backend_agrees_with_kotlinc("InvokeMixedOperands", source);
}

/// The stand-in forwards by DISPATCH, so a SUBCLASS's override is reached through the same number.
#[test]
fn a_subclasss_override_is_reached_through_the_stand_in() {
    let source = "open class A : (Int) -> Int {\n\
         \x20   override fun invoke(p: Int): Int = p + 1\n\
         }\n\
         class B : A() {\n\
         \x20   override fun invoke(p: Int): Int = p + 10\n\
         }\n\
         fun box(): String {\n\
         \x20   val f: (Int) -> Int = B()\n\
         \x20   return if (f(1) == 11) \"OK\" else \"fail \" + f(1)\n\
         }\n";
    every_backend_agrees_with_kotlinc("InvokeSubclassThroughStandIn", source);
}

/// An `object` rather than a class, which is the spelling the corpus uses most.
#[test]
fn an_object_implements_a_function_type_too() {
    let source = "object Doubler : (Int) -> Int {\n\
         \x20   override fun invoke(p: Int): Int = p * 2\n\
         }\n\
         fun apply(f: (Int) -> Int, n: Int): Int = f(n)\n\
         fun box(): String =\n\
         \x20   if (apply(Doubler, 21) == 42) \"OK\" else \"fail \" + apply(Doubler, 21)\n";
    every_backend_agrees_with_kotlinc("InvokeObjectFunctionType", source);
}

/// TWO function types at once has more `invoke`s than there are fixed numbers to put them in, so
/// it declines rather than picking one and answering the other call with the wrong body.
#[test]
fn two_function_types_at_once_decline() {
    expect_native_decline(
        "var result = \"\"\n\
         object Test : () -> Unit, (Boolean) -> Unit {\n\
         \x20   override fun invoke() { result += \"O\" }\n\
         \x20   override fun invoke(p1: Boolean) { if (p1) result += \"K\" }\n\
         }\n\
         fun box(): String {\n\
         \x20   val nullary: () -> Unit = Test\n\
         \x20   val unary: (Boolean) -> Unit = Test\n\
         \x20   nullary()\n\
         \x20   unary(true)\n\
         \x20   return result\n\
         }\n",
        "TwoFunctionTypes",
        "function types at once",
    );
}

/// A function type of more than 22 parameters has no `Function23` or wider to declare `invoke` on,
/// so nothing records which method is the function's body, and a call through the type would reach
/// an empty function slot. It declines rather than choosing a method by its name.
#[test]
fn a_function_type_wider_than_22_parameters_declines() {
    let params = (1..=23)
        .map(|n| format!("p{n}: Int"))
        .collect::<Vec<_>>()
        .join(", ");
    let types = ["Int"; 23].join(", ");
    let args = ["1"; 23].join(", ");
    expect_native_decline(
        &format!(
            "class Wide : ({types}) -> Int {{\n\
             \x20   override fun invoke({params}): Int = p23\n\
             }}\n\
             fun box(): String = if (Wide()({args}) == 1) \"OK\" else \"fail\"\n"
        ),
        "WideFunctionType",
        "a function type of 23 parameters",
    );
}
