//! `fun interface` conversion through krusty's own code generator.
//!
//! A SAM conversion changes the TYPE a function value wears, and a type is a table here: the object
//! holds its captures exactly as a lambda's does, and what differs is that a caller reaches it
//! through the interface's own member number rather than through the single invoke slot.
//!
//! These programs are about the member having a RECEIVER. Kotlin lets a `fun interface`'s single
//! abstract method be an extension — `fun String.foo(): String` — and inside the lambda that
//! implements it, `this` is that receiver. Nothing about the object changes: the receiver is a
//! declared parameter of the interface method, so it arrives where every other argument does. The
//! generator used to decline the shape on the strength of a flag saying a receiver was there,
//! without asking whether that made any difference to it.

use super::common::expect_native_box;

#[test]
fn a_fun_interface_method_may_be_an_extension() {
    expect_native_box(
        "fun interface Greet {\n\
         \x20   fun String.shout(): String\n\
         }\n\
         val loudly = Greet { this + \"!\" }\n\
         fun apply(text: String, how: Greet): String = with(how) { text.shout() }\n\
         fun box(): String {\n\
         \x20   val direct = with(loudly) { \"OK\".shout() }\n\
         \x20   if (direct != \"OK!\") return \"fail direct: $direct\"\n\
         \x20   val passed = apply(\"O\") { this + \"K\" }\n\
         \x20   return if (passed == \"OK\") \"OK\" else \"fail passed: $passed\"\n\
         }\n",
        "SamWithReceiver",
        "OK",
    );
}

#[test]
fn a_fun_interface_extension_takes_its_own_arguments_too() {
    // The receiver leads, and the method's declared parameters follow it — the lambda's body sees
    // them in that order, which is the only thing the thunk has to get right.
    expect_native_box(
        "fun interface Joiner {\n\
         \x20   fun Int.join(text: String, mark: String): String\n\
         }\n\
         val joiner = Joiner { text, mark -> \"\" + this + text + mark }\n\
         fun box(): String {\n\
         \x20   val joined = with(joiner) { 4.join(\"2\", \"!\") }\n\
         \x20   return if (joined == \"42!\") \"OK\" else \"fail: $joined\"\n\
         }\n",
        "SamReceiverAndArguments",
        "OK",
    );
}

#[test]
fn a_fun_interface_extension_keeps_what_the_lambda_captured() {
    // The captures are read out of the object and the receiver off the call, so a body that uses
    // both is what pins the thunk's argument order.
    expect_native_box(
        "fun interface Wrap {\n\
         \x20   fun String.wrap(): String\n\
         }\n\
         fun wrapper(prefix: String): Wrap = Wrap { prefix + this + prefix }\n\
         fun box(): String {\n\
         \x20   val wrapped = with(wrapper(\"-\")) { \"OK\".wrap() }\n\
         \x20   return if (wrapped == \"-OK-\") \"OK\" else \"fail: $wrapped\"\n\
         }\n",
        "SamReceiverWithCaptures",
        "OK",
    );
}
