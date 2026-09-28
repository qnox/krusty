//! A receiver lambda inside a member of a local class reads a receiver the class captured from an
//! enclosing receiver lambda (`this@outer` inside `withInner(…) inner@{ … }`). The nested lambda's
//! own receiver comes first in its receiver tower, so the captured receiver sits one rung further
//! out than it does in the member.
//!
//! The comparisons cover the file facade, which holds the lambdas. The local class itself differs
//! from kotlinc independently of these rules: kotlinc names the captured field after the label
//! (`$this_outer`) and hands the nested lambda the field's value rather than the class instance.

use super::common;

const LOCAL_CLASS: &str = "class Outer { fun left() = \"O\" }\n\
    class Inner { fun right() = \"K\" }\n\
    fun <T> withOuter(value: Outer, block: Outer.() -> T): T = value.block()\n\
    fun <T> withInner(value: Inner, block: Inner.() -> T): T = value.block()\n\
    fun box(): String = withOuter(Outer()) outer@{\n\
    \x20   class Local {\n\
    \x20       fun read(): String = withInner(Inner()) inner@{ this@outer.left() + this@inner.right() }\n\
    \x20   }\n\
    \x20   Local().read()\n\
    }\n";

/// Both labelled receivers resolve inside the nested receiver lambda: `this@inner` is its own
/// receiver and `this@outer` the local class's captured one.
#[test]
fn a_receiver_lambda_in_a_local_class_member_reads_the_captured_receiver() {
    common::assert_class_matches_kotlinc(
        "NestedReceiverLocalClass",
        LOCAL_CLASS,
        "NestedReceiverLocalClassKt",
    );
    common::expect_box_same_as_kotlinc(LOCAL_CLASS, "NestedReceiverLocalClassRun");
}

const BUILDER_LOCAL_CLASS: &str = "class TargetType\n\
    class Buildee<TV> {\n\
    \x20   var stored: Any? = null\n\
    \x20   fun setTypeVariable(value: TV) { stored = value }\n\
    }\n\
    fun <PTV> build(instructions: Buildee<PTV>.() -> Unit): Buildee<PTV> {\n\
    \x20   val buildee = Buildee<PTV>()\n\
    \x20   buildee.instructions()\n\
    \x20   return buildee\n\
    }\n\
    fun outer(): Buildee<TargetType> {\n\
    \x20   val buildee = build outerBuild@ {\n\
    \x20       class LocalClass {\n\
    \x20           fun member() {\n\
    \x20               build innerBuild@ {\n\
    \x20                   this@outerBuild.setTypeVariable(TargetType())\n\
    \x20                   this@innerBuild.setTypeVariable(TargetType())\n\
    \x20               }\n\
    \x20           }\n\
    \x20       }\n\
    \x20       LocalClass().member()\n\
    \x20   }\n\
    \x20   return buildee\n\
    }\n\
    fun box(): String = if (outer().stored is TargetType) \"OK\" else \"fail\"\n";

/// The builder form (KT-49160): the call inside the local class member infers the outer builder's
/// variable through `this@outerBuild`, so `buildee` is a `Buildee<TargetType>`.
#[test]
fn a_builder_lambda_in_a_local_class_member_infers_the_outer_builder() {
    common::assert_class_matches_kotlinc(
        "NestedBuilderLocalClass",
        BUILDER_LOCAL_CLASS,
        "NestedBuilderLocalClassKt",
    );
    common::expect_box_same_as_kotlinc(BUILDER_LOCAL_CLASS, "NestedBuilderLocalClassRun");
}
