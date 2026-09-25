//! `buildString { … }` and `buildList { … }` in the native backend.
//!
//! Both are `inline`, so a provider holding their bodies splices them and nothing reaches a
//! backend. A klib publishes no body to splice and the call arrives whole — the same shape the
//! scope functions arrive in, with one difference: the subject the block fills is made by the
//! call rather than written by the caller.
//!
//! `buildString` answers the builder's `toString()`, which is a COPY. That is observable: a
//! program holding the builder through a capture and appending after the call would otherwise see
//! the answer change under it.
//!
//! Every expectation is kotlinc's, taken by running the same program under it.

use super::common::{expect_box_ok_with_stdlib, expect_native_box};

/// The text builder, with and without the capacity hint, and the copy the answer is.
#[test]
fn build_string_answers_a_copy_of_what_the_block_appended() {
    let source = "fun box(): String {\n\
         \x20   val plain = buildString {\n\
         \x20       append(\"a\")\n\
         \x20       append(1)\n\
         \x20       append('c')\n\
         \x20   }\n\
         \x20   if (plain != \"a1c\") return \"fail plain: \" + plain\n\
         \x20   val sized = buildString(16) { append(\"sized\") }\n\
         \x20   if (sized != \"sized\") return \"fail sized: \" + sized\n\
         \x20   if (buildString { } != \"\") return \"fail empty\"\n\
         \x20   // The answer is a copy: appending afterwards leaves it as it was.\n\
         \x20   var kept: StringBuilder? = null\n\
         \x20   val taken = buildString { kept = this; append(\"x\") }\n\
         \x20   kept!!.append(\"y\")\n\
         \x20   if (taken != \"x\") return \"fail copy: \" + taken\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "BuildStringScope");
    expect_native_box(source, "BuildStringScope", "OK");
}

/// The block is an ordinary closure: it captures, and it runs exactly once, when the subject
/// already exists and before the answer is taken.
#[test]
fn a_builder_block_captures_and_runs_once_over_the_subject() {
    let source = "fun box(): String {\n\
         \x20   var runs = 0\n\
         \x20   val prefix = \"p\"\n\
         \x20   val text = buildString {\n\
         \x20       runs++\n\
         \x20       append(prefix)\n\
         \x20       append(runs)\n\
         \x20   }\n\
         \x20   if (runs != 1) return \"fail runs: \" + runs\n\
         \x20   if (text != \"p1\") return \"fail text: \" + text\n\
         \x20   val nested = buildString { append(buildString { append(\"in\") }) }\n\
         \x20   if (nested != \"in\") return \"fail nested: \" + nested\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "BuilderBlockCaptures");
    expect_native_box(source, "BuilderBlockCaptures", "OK");
}
