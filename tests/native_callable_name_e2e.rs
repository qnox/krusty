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

#[test]
fn a_function_value_is_compared_through_its_own_table() {
    // Three things wear a function type, and `==` answers each differently. The site that lowers
    // the comparison cannot tell them apart — the static type is `() -> …` for all three — and
    // does not need to: it dispatches `equals` on the receiver, and the OBJECT's table says what
    // it is. Both `==` and an explicit `.equals` used to decline here rather than answer.
    expect_native_box(
        "fun topLevel(): String = \"t\"\n\
         class Box(val value: String) {\n\
         \x20   fun member(): String = value\n\
         }\n\
         fun box(): String {\n\
         \x20   // A callable reference: equal by the DECLARATION it names, though each `::topLevel`\n\
         \x20   // written here is a separate object.\n\
         \x20   if (::topLevel != ::topLevel) return \"fail: reference\"\n\
         \x20   if (!(::topLevel).equals(::topLevel)) return \"fail: reference equals\"\n\
         \x20   // A bound reference adds the receiver to that comparison.\n\
         \x20   val one = Box(\"a\")\n\
         \x20   val other = Box(\"a\")\n\
         \x20   if (one::member != one::member) return \"fail: same receiver\"\n\
         \x20   if (one::member == other::member) return \"fail: different receivers\"\n\
         \x20   // A lambda has identity equality, which is what Kotlin gives one.\n\
         \x20   val lambda: () -> String = { \"l\" }\n\
         \x20   if (lambda != lambda) return \"fail: lambda identity\"\n\
         \x20   // And a reference compared against something that is not callable at all is\n\
         \x20   // simply not equal, rather than a crash on the way to asking.\n\
         \x20   if ((::topLevel).equals(null)) return \"fail: null\"\n\
         \x20   if ((::topLevel).equals(\"not a callable\")) return \"fail: string\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "FunctionValueEquality",
        "OK",
    );
}

#[test]
fn a_non_capturing_lambda_is_one_object_however_often_it_is_produced() {
    // `hashCode` reaches the same slot `equals` does, so it was declined for the same stale
    // reason. A lambda that captures nothing is emitted once for the program, so two calls to a
    // function returning it hand back the same object — which is what makes the hashes agree.
    expect_native_box(
        "fun produce(): () -> Unit = {}\n\
         fun box(): String =\n\
         \x20   if (produce().hashCode() == produce().hashCode()) \"OK\" else \"fail\"\n",
        "NonCapturingLambdaSingleton",
        "OK",
    );
}
