//! A `fun interface` whose method declares CONTEXT PARAMETERS.
//!
//! A context parameter is a leading VALUE parameter of the method that declares it — that is how
//! the callable header records one — so the object a SAM conversion builds needs nothing new. Its
//! thunk wears the interface member's own signature and hands every operand on positionally, which
//! is the same treatment an extension receiver already got: neither is a shape the thunk has to
//! know about, because both arrive as parameters in the order the declaration states.
//!
//! Every expectation is kotlinc's, taken by running the same program under it.

use super::common::{expect_box_ok_with_stdlib, expect_native_box, kotlinc_box_result};

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

/// The corpus shape: one context parameter ahead of one value parameter.
#[test]
fn a_context_parameter_sits_ahead_of_the_value_one() {
    let source = "class Context\n\
         fun interface SAM {\n\
         \x20   context(context: Context)\n\
         \x20   fun foo(x: Int): Int\n\
         }\n\
         fun box(): String {\n\
         \x20   val sam = SAM { x -> x + 1 }\n\
         \x20   with(Context()) {\n\
         \x20       if (sam.foo(0) != 1) return \"fail \" + sam.foo(0)\n\
         \x20   }\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("ContextSamNamedParameter", source);
}

/// The lambda spelled without a parameter list, and with `it`.
#[test]
fn the_lambda_may_name_its_parameter_or_not() {
    let source = "class Context\n\
         fun interface SAM {\n\
         \x20   context(context: Context)\n\
         \x20   fun foo(x: Int): Int\n\
         }\n\
         fun box(): String {\n\
         \x20   val constant = SAM { 2 }\n\
         \x20   val implicit = SAM { it + 1 }\n\
         \x20   with(Context()) {\n\
         \x20       if (constant.foo(0) != 2) return \"fail constant\"\n\
         \x20       if (implicit.foo(4) != 5) return \"fail implicit\"\n\
         \x20   }\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("ContextSamImplicitParameter", source);
}

/// The context is READABLE from inside the converted lambda, through a context-parameter function
/// resolved against it — which is the whole point of the parameter being there.
#[test]
fn the_context_reaches_the_converted_lambda() {
    let source = "open class A {\n\
         \x20   fun foo(a: String): String { return a }\n\
         }\n\
         context(ctx: T)\n\
         fun <T> implicit(): T = ctx\n\
         fun interface SamInterface {\n\
         \x20   context(i: A)\n\
         \x20   fun accept(s: String): String\n\
         }\n\
         val samObject = SamInterface { s: String -> implicit<A>().foo(s) }\n\
         fun box(): String {\n\
         \x20   with(A()) {\n\
         \x20       return samObject.accept(\"OK\")\n\
         \x20   }\n\
         }\n";
    every_backend_agrees_with_kotlinc("ContextSamReadsItsContext", source);
}

/// TWO context parameters, so the ordering is pinned rather than inferred from a single one.
#[test]
fn two_context_parameters_keep_their_order() {
    let source = "class Head(val text: String)\n\
         class Tail(val text: String)\n\
         fun interface Join {\n\
         \x20   context(head: Head, tail: Tail)\n\
         \x20   fun run(middle: String): String\n\
         }\n\
         context(head: Head, tail: Tail)\n\
         fun ends(): String = head.text + tail.text\n\
         fun box(): String {\n\
         \x20   val join = Join { middle -> ends() + middle }\n\
         \x20   with(Head(\"O\")) {\n\
         \x20       with(Tail(\"K\")) {\n\
         \x20           val said = join.run(\"!\")\n\
         \x20           if (said != \"OK!\") return \"fail \" + said\n\
         \x20       }\n\
         \x20   }\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("ContextSamTwoParameters", source);
}

/// A context parameter of a NON-generic interface whose method also takes an ordinary receiver-less
/// value: the object is built once and called twice, so nothing about the thunk is per-call.
#[test]
fn the_converted_object_answers_more_than_once() {
    let source = "class Tag(val text: String)\n\
         fun interface Render {\n\
         \x20   context(tag: Tag)\n\
         \x20   fun of(value: Int): String\n\
         }\n\
         fun box(): String {\n\
         \x20   val render = Render { value -> \"\" + value }\n\
         \x20   with(Tag(\"t\")) {\n\
         \x20       if (render.of(1) != \"1\") return \"fail one\"\n\
         \x20       if (render.of(2) != \"2\") return \"fail two\"\n\
         \x20   }\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("ContextSamCalledTwice", source);
}
