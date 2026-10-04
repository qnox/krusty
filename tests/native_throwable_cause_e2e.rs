//! `Throwable(message, cause)`, `Throwable(cause)` and `e.cause` — the second field a throwable
//! carries.
//!
//! Kotlin declares four constructors, and the two one-argument forms are told apart by the
//! parameter TYPE: a `Throwable` is a cause and anything else is a message. They differ in more
//! than which field they fill — `Throwable(cause)` takes its MESSAGE from the cause as well — so
//! neither can be written as the other.
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

/// `Throwable(message, cause)`: each operand reaches its own field.
#[test]
fn a_throwable_carries_the_message_and_the_cause_it_was_given() {
    let source = "fun box(): String {\n\
         \x20   val t = Throwable(\"O\", Throwable(\"K\"))\n\
         \x20   return if (t.message == \"O\" && t.cause?.message == \"K\") \"OK\" else \"fail\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("ThrowableMessageAndCause", source);
}

/// `Throwable(cause)`: the MESSAGE comes from the cause, so it is neither null nor the cause's own
/// message.
#[test]
fn a_throwable_built_from_a_cause_renders_its_message_from_it() {
    let source = "fun box(): String {\n\
         \x20   val t = Throwable(Throwable(\"OK\"))\n\
         \x20   if (t.cause?.message != \"OK\") return \"fail cause\"\n\
         \x20   val message = t.message ?: return \"fail null\"\n\
         \x20   if (message == \"OK\") return \"fail verbatim\"\n\
         \x20   return if (message.endsWith(\"OK\")) \"OK\" else \"fail \" + message\n\
         }\n";
    every_backend_agrees_with_kotlinc("ThrowableFromCause", source);
}

/// The one-argument MESSAGE form still leaves the cause empty.
#[test]
fn a_throwable_built_from_a_message_has_no_cause() {
    let source = "fun box(): String {\n\
         \x20   val t = Throwable(\"OK\")\n\
         \x20   return if (t.message == \"OK\" && t.cause == null) \"OK\" else \"fail\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("ThrowableMessageOnly", source);
}

/// The no-argument form leaves both empty.
#[test]
fn a_throwable_built_from_nothing_has_neither() {
    let source = "fun box(): String {\n\
         \x20   val t = Throwable()\n\
         \x20   return if (t.message == null && t.cause == null) \"OK\" else \"fail\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("ThrowableEmpty", source);
}

/// A cause SURVIVES being thrown and caught, so the field is real storage rather than something
/// the construction site folded.
#[test]
fn a_cause_survives_a_throw_and_a_catch() {
    let source = "fun box(): String {\n\
         \x20   try {\n\
         \x20       throw IllegalStateException(\"O\", Throwable(\"K\"))\n\
         \x20   } catch (e: IllegalStateException) {\n\
         \x20       return if (e.message == \"O\" && e.cause?.message == \"K\") \"OK\" else \"fail\"\n\
         \x20   }\n\
         }\n";
    every_backend_agrees_with_kotlinc("ThrownCauseSurvives", source);
}

/// A cause chain of three, read through two hops — the field holds a reference the collector must
/// keep, not a copy of one throwable's text.
#[test]
fn a_cause_chain_reads_through_every_hop() {
    let source = "fun box(): String {\n\
         \x20   val t = Throwable(\"a\", Throwable(\"b\", Throwable(\"OK\")))\n\
         \x20   return t.cause?.cause?.message ?: \"fail\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("ThrowableCauseChain", source);
}
