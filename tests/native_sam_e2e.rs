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

/// A `fun interface` may INHERIT the single member it converts rather than declaring it.
///
/// `fun interface I : Base` writes nothing of its own, and the member a lambda supplies is still
/// `Base`'s. The search for it therefore walks up from the named interface — as a `super` property
/// access does — and answers the first declaration it meets. Searching the named interface alone
/// found nothing and declined.
///
/// The slot is keyed by the class that DECLARES the member, which is what the layout keyed it by;
/// a slot assigned at the declaring class is valid for every subclass, so the object still wears
/// the interface the conversion named.
#[test]
fn a_fun_interface_may_inherit_the_member_it_converts() {
    expect_native_box(
        "fun interface Base {\n\
         \x20   fun doStuff(): String\n\
         }\n\
         fun interface I : Base\n\
         fun interface Proxy : I {\n\
         \x20   override fun doStuff(): String = doStuffInt().toString()\n\
         \x20   fun doStuffInt(): Int\n\
         }\n\
         fun runBase(b: Base) = b.doStuff()\n\
         fun runI(i: I) = i.doStuff()\n\
         fun runProxy(p: Proxy) = p.doStuff()\n\
         fun box(): String {\n\
         \x20   if (runI { \"i\" } != \"i\") return \"fail inherited\"\n\
         \x20   if (runProxy { 10 } != \"10\") return \"fail overridden\"\n\
         \x20   return runBase { \"OK\" }\n\
         }\n",
        "SamInheritedMember",
        "OK",
    );
}

/// Two steps up, and the object still wears the interface the conversion NAMED — not the one that
/// declares the member, which is what its rendered name would otherwise say.
#[test]
fn an_inherited_sam_member_keeps_the_named_interfaces_identity() {
    expect_native_box(
        "fun interface Deep {\n\
         \x20   fun run(value: Int): String\n\
         }\n\
         fun interface Middle : Deep\n\
         fun interface Leaf : Middle\n\
         fun box(): String {\n\
         \x20   val leaf = Leaf { n -> \"got \" + n }\n\
         \x20   if (leaf.run(1) != \"got 1\") return \"fail call\"\n\
         \x20   if (leaf !is Deep) return \"fail is Deep\"\n\
         \x20   if (leaf !is Middle) return \"fail is Middle\"\n\
         \x20   val asDeep: Deep = leaf\n\
         \x20   if (asDeep.run(2) != \"got 2\") return \"fail through the base\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "SamInheritedTwoSteps",
        "OK",
    );
}
