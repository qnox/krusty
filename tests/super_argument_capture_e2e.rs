//! Captures reached while a CONSTRUCTOR PREFIX is in scope — a superclass or sibling-constructor
//! argument. The instance does not exist there, so every capture the argument reads or hands on has
//! to come from the constructor's synthetic prefix parameter rather than off `this`.

use super::common;

/// Run `body` under krusty, and under the REFERENCE compiler when it is provisioned.
///
/// What a capture in a constructor prefix must read from is a claim about what kotlinc emits, so
/// pin it against kotlinc rather than only against krusty. A missing reference compiler means this
/// environment does not provision one; CI does.
fn run_ok(stem: &str, body: &str) {
    common::expect_box_ok_with_stdlib(body, stem);
    let Some(out) = common::kotlinc_library(body) else {
        return;
    };
    assert_eq!(
        common::run_box(&[], "LibKt", &[out, common::stdlib_jar()]).as_deref(),
        Some("OK"),
        "{stem}: reference compiler"
    );
}

#[test]
fn a_lambda_in_a_super_constructor_argument_reads_a_captured_local() {
    run_ok(
        "LambdaInSuperArgumentLocal",
        "open class Base(val fn: () -> String)\n\
         fun box(): String {\n\
         val o = \"O\"\n\
         class Local(k: String) : Base({ o + k })\n\
         return Local(\"K\").fn() }\n",
    );
}

#[test]
fn a_lambda_in_a_super_constructor_argument_reads_an_enclosing_property() {
    run_ok(
        "LambdaInSuperArgumentProperty",
        "abstract class Base(val fn: () -> String)\n\
         class Outer {\n\
         val ok = \"OK\"\n\
         fun foo(): String { class Local : Base({ ok })\n\
         return Local().fn() } }\n\
         fun box() = Outer().foo()\n",
    );
}

#[test]
fn a_nested_lambda_in_a_super_constructor_argument_reads_a_captured_local() {
    run_ok(
        "NestedLambdaInSuperArgument",
        "open class Base(val fn: () -> String)\n\
         fun box(): String {\n\
         val o = \"O\"\n\
         class Local(k: String) : Base({ { o + k }() })\n\
         return Local(\"K\").fn() }\n",
    );
}

#[test]
fn two_lambdas_in_a_super_constructor_argument_reach_one_captured_cell() {
    // Both lambdas capture the SAME local, and one writes what the other reads — so a pair that
    // had been given a cell each would answer "fail" while still compiling and running. Two
    // lambdas capturing two different locals proves nothing here: each would work with a copy.
    run_ok(
        "TwoLambdasShareOneCell",
        "open class Base(val write: () -> Unit, val read: () -> String)\n\
         fun box(): String {\n\
         var shared = \"fail\"\n\
         class Local : Base({ shared = \"OK\" }, { shared })\n\
         val local = Local()\n\
         local.write()\n\
         return local.read() }\n",
    );
}

#[test]
fn two_lambdas_in_a_super_constructor_argument_carry_their_own_captures() {
    // The companion to the cell test: two lambdas capturing two DIFFERENT locals each get their
    // own, which is what would look identical to sharing if the test above were the only one.
    run_ok(
        "TwoLambdasInSuperArgument",
        "open class Base(val first: () -> String, val second: () -> String)\n\
         fun box(): String {\n\
         val o = \"O\"\n\
         val k = \"K\"\n\
         class Local : Base({ o }, { k })\n\
         val local = Local()\n\
         return local.first() + local.second() }\n",
    );
}

#[test]
fn a_lambda_in_a_super_constructor_argument_writes_through_a_shared_cell() {
    run_ok(
        "LambdaInSuperArgumentSharedCell",
        "open class Base(val fn: () -> Unit)\n\
         fun box(): String {\n\
         var seen = \"fail\"\n\
         class Local : Base({ seen = \"OK\" })\n\
         Local().fn()\n\
         return seen }\n",
    );
}

#[test]
fn a_lambda_in_a_nested_anonymous_objects_super_argument_reaches_the_enclosing_instance() {
    run_ok(
        "LambdaInNestedAnonSuperArgument",
        "open class X(val fn: () -> Unit)\n\
         open class C(val x: X)\n\
         class B(var value: Int) {\n\
         fun update() { object : C(object : X({ value = 3 }) {}) {}.x.fn() } }\n\
         fun box(): String {\n\
         val b = B(1)\n\
         b.update()\n\
         return if (b.value == 3) \"OK\" else \"fail\" }\n",
    );
}

#[test]
fn a_lambda_in_a_sibling_constructor_argument_reads_a_captured_local() {
    // A `this(…)` delegation is the other prefix form, and the instance does not exist there
    // either. The super cases alone cannot show it: they reach the superclass call directly, while
    // this one hands the lambda to a SIBLING constructor, which then passes it on.
    run_ok(
        "LambdaInSiblingArgument",
        "open class Base(val fn: () -> String)\n\
         fun box(): String {\n\
         val o = \"O\"\n\
         class Local : Base {\n\
         constructor(fn: () -> String) : super(fn)\n\
         constructor(k: String) : this({ o + k }) }\n\
         return Local(\"K\").fn() }\n",
    );
}

#[test]
fn a_lambda_in_a_constructor_default_reads_a_captured_local() {
    // A defaulted parameter's expression is evaluated in the prefix as well — before the instance
    // exists and before the superclass call it feeds. The third form, and the only one where the
    // lambda is not written inside an argument list at all.
    run_ok(
        "LambdaInConstructorDefault",
        "open class Base(val fn: () -> String)\n\
         fun box(): String {\n\
         val o = \"OK\"\n\
         class Local(val make: () -> String = { o }) : Base(make)\n\
         return Local().fn() }\n",
    );
}

#[test]
fn a_lambda_in_a_super_constructor_argument_calls_an_enclosing_local_function() {
    run_ok(
        "LocalFunInSuperArgument",
        "open class Base(val fn: () -> String)\n\
         fun box(): String {\n\
         val x = \"O\"\n\
         fun localFn() = x\n\
         class Local(y: String) : Base({ localFn() + y })\n\
         return Local(\"K\").fn() }\n",
    );
}
