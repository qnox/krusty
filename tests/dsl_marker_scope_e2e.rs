//! `@DslMarker` scope control: an implicit receiver is out of scope when a nearer receiver shares
//! one of its markers, so a member call, property read or context argument that binds it
//! implicitly is rejected, while an explicit receiver or label still reaches it. Every program is
//! compared against kotlinc's complete diagnostics.

use super::common;

const SCOPES: &str = "@DslMarker\n\
    annotation class Dsl\n\
    @DslMarker\n\
    annotation class OtherDsl\n\
    @Dsl\n\
    class Outer {\n\
    \x20   val value: Int = 1\n\
    \x20   fun outerMember() {}\n\
    }\n\
    @Dsl\n\
    class Inner\n\
    @OtherDsl\n\
    class Unrelated\n\
    fun outer(block: Outer.() -> Unit) = Outer().block()\n\
    fun inner(block: Inner.() -> Unit) = Inner().block()\n\
    fun unrelated(block: Unrelated.() -> Unit) = Unrelated().block()\n";

fn assert_matches_kotlinc(body: &str) {
    let source = format!("{SCOPES}{body}");
    common::assert_error_blocks_match_kotlinc(&[("test.kt", source.as_str())], &[]);
}

#[test]
fn nearer_receiver_with_the_same_marker_hides_the_outer_one() {
    assert_matches_kotlinc(
        "fun test() {\n\
        \x20   outer {\n\
        \x20       inner {\n\
        \x20           outerMember()\n\
        \x20           val v = value\n\
        \x20       }\n\
        \x20   }\n\
        }\n",
    );
}

fn assert_box_same_as_kotlinc(body: &str, stem: &str) {
    let source = format!("{SCOPES}{body}");
    common::expect_box_same_as_kotlinc(&source, stem);
}

#[test]
fn explicit_receivers_and_labels_still_reach_the_outer_receiver() {
    assert_box_same_as_kotlinc(
        "fun box(): String {\n\
        \x20   var result = \"\"\n\
        \x20   outer {\n\
        \x20       inner {\n\
        \x20           this@outer.outerMember()\n\
        \x20           result = if (this@outer.value == 1) \"OK\" else \"FAIL\"\n\
        \x20       }\n\
        \x20   }\n\
        \x20   return result\n\
        }\n",
        "DslMarkerExplicitReceiverRun",
    );
}

#[test]
fn a_nearer_receiver_with_another_marker_does_not_hide_the_outer_one() {
    assert_box_same_as_kotlinc(
        "fun box(): String {\n\
        \x20   var result = \"\"\n\
        \x20   outer {\n\
        \x20       unrelated {\n\
        \x20           outerMember()\n\
        \x20           result = if (value == 1) \"OK\" else \"FAIL\"\n\
        \x20       }\n\
        \x20   }\n\
        \x20   return result\n\
        }\n",
        "DslMarkerOtherMarkerRun",
    );
}

#[test]
fn a_context_argument_filled_from_a_hidden_receiver_is_rejected() {
    assert_matches_kotlinc(
        "context(_: Outer)\n\
        fun contextFun() {}\n\
        fun test() {\n\
        \x20   outer {\n\
        \x20       contextFun()\n\
        \x20       inner {\n\
        \x20           contextFun()\n\
        \x20       }\n\
        \x20   }\n\
        }\n",
    );
}
