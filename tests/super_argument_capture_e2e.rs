//! Captures reached while a CONSTRUCTOR PREFIX is in scope — a superclass or sibling-constructor
//! argument. The instance does not exist there, so every capture the argument reads or hands on has
//! to come from the constructor's synthetic prefix parameter rather than off `this`.

use super::common;

fn run_ok(stem: &str, body: &str) {
    common::expect_box_ok_with_stdlib(body, stem);
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
fn two_lambdas_in_a_super_constructor_argument_share_one_captured_local() {
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
