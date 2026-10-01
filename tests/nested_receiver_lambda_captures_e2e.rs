//! A member of a local class or anonymous object declared in a receiver lambda reads the lambda's
//! labelled receiver (`this@outer`), directly or from a receiver lambda of its own
//! (`withInner(…) inner@{ this@outer.left() }`). The nested lambda's own receiver comes first in its
//! receiver tower, so the captured receiver sits one rung further out than it does in the member.
//! An anonymous object keeps the receiver it captured under the lambda's label, so `this@outer`
//! names that one receiver both in the object's body and at its construction.
//!
//! The comparisons cover the file facade, which holds the lambdas. The classes themselves differ
//! from kotlinc independently of these rules: kotlinc names the captured field after the label
//! (`$this_outer`) and hands a nested lambda the field's value rather than the class instance.

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

const NAMED_CONTEXT_LOCAL_FUNCTION: &str = "// LANGUAGE: +ContextParameters\n\
    class Outer(val text: String)\n\
    class Inner(val text: String)\n\
    class Marker(val text: String)\n\
    fun <T> withOuter(value: Outer, block: Outer.() -> T): T = value.block()\n\
    fun <T> withInner(value: Inner, block: Inner.() -> T): T = value.block()\n\
    fun box(): String = withOuter(Outer(\"O\")) outer@{\n\
    \x20   class Local {\n\
    \x20       fun read(): String {\n\
    \x20           context(marker: Marker)\n\
    \x20           fun local(): String = withInner(Inner(\"K\")) inner@{\n\
    \x20               marker.text + this@outer.text + this@inner.text\n\
    \x20           }\n\
    \x20           return with(Marker(\"\")) { local() }\n\
    \x20       }\n\
    \x20   }\n\
    \x20   Local().read()\n\
    }\n";

/// A named context parameter on the local function is a lexical value, not a receiver-tower rung.
/// Only the nested receiver lambda shifts the local class's captured `this@outer` coordinate.
#[test]
fn a_named_context_local_function_does_not_shift_the_captured_receiver() {
    common::expect_box_same_as_kotlinc(
        NAMED_CONTEXT_LOCAL_FUNCTION,
        "NestedReceiverNamedContextLocalFunction",
    );
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

const ANONYMOUS_OBJECT: &str = "class Outer { fun left() = \"O\" }\n\
    class Inner { fun right() = \"K\" }\n\
    fun <T> withOuter(value: Outer, block: Outer.() -> T): T = value.block()\n\
    fun <T> withInner(value: Inner, block: Inner.() -> T): T = value.block()\n\
    fun direct(): String = withOuter(Outer()) outer@{\n\
    \x20   object { fun read(): String = this@outer.left() }.read()\n\
    }\n\
    fun nested(): String = withOuter(Outer()) outer@{\n\
    \x20   object {\n\
    \x20       fun read(): String = withInner(Inner()) inner@{ this@outer.left() + this@inner.right() }\n\
    \x20   }.read()\n\
    }\n\
    fun box(): String = if (direct() == \"O\") nested() else \"fail\"\n";

/// An anonymous object captures the receiver its member names as `this@outer`, whether the member
/// reads it directly or from its own receiver lambda.
#[test]
fn an_anonymous_object_member_reads_the_labelled_receiver() {
    common::assert_class_matches_kotlinc(
        "NestedReceiverAnonymousObject",
        ANONYMOUS_OBJECT,
        "NestedReceiverAnonymousObjectKt",
    );
    common::expect_box_same_as_kotlinc(ANONYMOUS_OBJECT, "NestedReceiverAnonymousObjectRun");
}

const BUILDER_ANONYMOUS_OBJECT: &str = "class TargetType\n\
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
    \x20       object {\n\
    \x20           fun member() {\n\
    \x20               build innerBuild@ {\n\
    \x20                   this@outerBuild.setTypeVariable(TargetType())\n\
    \x20                   this@innerBuild.setTypeVariable(TargetType())\n\
    \x20               }\n\
    \x20           }\n\
    \x20       }.member()\n\
    \x20   }\n\
    \x20   return buildee\n\
    }\n\
    fun box(): String = if (outer().stored is TargetType) \"OK\" else \"fail\"\n";

/// The anonymous-object builder form (KT-49160): the object's member infers the outer builder's
/// variable through `this@outerBuild`.
#[test]
fn a_builder_lambda_in_an_anonymous_object_member_infers_the_outer_builder() {
    common::assert_class_matches_kotlinc(
        "NestedBuilderAnonymousObject",
        BUILDER_ANONYMOUS_OBJECT,
        "NestedBuilderAnonymousObjectKt",
    );
    common::expect_box_same_as_kotlinc(BUILDER_ANONYMOUS_OBJECT, "NestedBuilderAnonymousObjectRun");
}
