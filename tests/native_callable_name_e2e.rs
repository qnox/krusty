//! `KCallable.name` of a callable reference, through krusty's own code generator and runtime.
//!
//! A function reference is a lambda object here: it carries the code, not the declaration, so
//! nothing in the emitted object knows what the source called it. It does not have to. The one
//! place a program can ask — `::foo.name` — names the declaration in the very node being lowered,
//! so the answer is a compile-time constant and each program below pins one.
//!
//! What the constant must NOT do is swallow the receiver: `x::foo` evaluates `x`, and a program
//! that can see that happen is the last test here.

use super::common::expect_native_box;

#[test]
fn a_top_level_function_reference_answers_its_name() {
    expect_native_box(
        "fun greet() {}\n\
         fun box(): String = if (::greet.name == \"greet\") \"OK\" else \"fail: ${::greet.name}\"\n",
        "TopLevelCallableName",
        "OK",
    );
}

#[test]
fn a_local_function_reference_answers_its_name() {
    expect_native_box(
        "fun box(): String {\n\
         \x20   fun helper(): Int = 1\n\
         \x20   return if (::helper.name == \"helper\") \"OK\" else \"fail: ${::helper.name}\"\n\
         }\n",
        "LocalCallableName",
        "OK",
    );
}

#[test]
fn a_member_function_reference_answers_the_members_name() {
    expect_native_box(
        "class Greeter {\n\
         \x20   fun greet(): Int = 1\n\
         }\n\
         fun box(): String {\n\
         \x20   val name = Greeter()::greet.name\n\
         \x20   return if (name == \"greet\") \"OK\" else \"fail: $name\"\n\
         }\n",
        "MemberCallableName",
        "OK",
    );
}

#[test]
fn a_constructor_reference_is_called_init() {
    expect_native_box(
        "class Holder(val value: Int)\n\
         fun box(): String {\n\
         \x20   val name = ::Holder.name\n\
         \x20   return if (name == \"<init>\") \"OK\" else \"fail: $name\"\n\
         }\n",
        "ConstructorCallableName",
        "OK",
    );
}

#[test]
fn the_bound_receiver_is_still_evaluated() {
    // The name is a constant, but `x::foo` is not: the receiver runs, and here it is visible.
    expect_native_box(
        "var evaluated = 0\n\
         class Counter {\n\
         \x20   fun step(): Int = 1\n\
         }\n\
         fun next(): Counter {\n\
         \x20   evaluated++\n\
         \x20   return Counter()\n\
         }\n\
         fun box(): String {\n\
         \x20   val name = next()::step.name\n\
         \x20   if (evaluated != 1) return \"fail: receiver ran $evaluated times\"\n\
         \x20   return if (name == \"step\") \"OK\" else \"fail: $name\"\n\
         }\n",
        "CallableNameEvaluatesReceiver",
        "OK",
    );
}

#[test]
fn a_reference_reaching_the_read_through_a_variable_declines() {
    // The fold needs the REFERENCE at the read. Stored in a variable, the read's receiver is a
    // variable read and the declaration is no longer in hand — so the generator declines rather
    // than inventing a name, which is the honest answer until the object carries one itself.
    //
    // The decline NAMES the property. It used to read `Checked(ExternalPropertyRead)`, the node's
    // shape, which every dependency property in the corpus shares — so the backlog carried one row
    // for eighteen unrelated properties and could not be worked from. Asserting the name here is
    // what keeps that phrasing from silently regressing to the shape.
    super::common::expect_native_decline(
        "fun greet() {}\n\
         fun box(): String {\n\
         \x20   val reference = ::greet\n\
         \x20   return if (reference.name == \"greet\") \"OK\" else \"fail: ${reference.name}\"\n\
         }\n",
        "StoredCallableName",
        "a read of the property `kotlin/reflect/KCallable.name`",
    );
}
