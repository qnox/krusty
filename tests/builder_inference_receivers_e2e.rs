//! A builder lambda sees the type variables of the call it is an argument of. kotlinc's PCLA lets an
//! extension whose declared receiver fixes such a variable apply to the lambda's receiver
//! (`Buildee<UserKlass>.f()` on `Buildee<FT>`), and the receiver constraint is what infers the
//! builder call's type argument.

use super::common;

const EXTENSION_FUNCTION: &str = "class Buildee<CT>\n\
    fun <FT> build(instructions: Buildee<FT>.() -> Unit): Buildee<FT> {\n\
    \x20 val buildee = Buildee<FT>()\n\
    \x20 buildee.instructions()\n\
    \x20 return buildee\n\
    }\n\
    class UserKlass\n\
    fun Buildee<UserKlass>.source() {}\n\
    fun implicitReceiver() = build { source() }\n\
    fun explicitReceiver() = build { this.source() }\n\
    fun local(): Buildee<UserKlass> {\n\
    \x20 val buildee = build { source() }\n\
    \x20 return buildee\n\
    }\n\
    fun box(): String {\n\
    \x20 implicitReceiver()\n\
    \x20 explicitReceiver()\n\
    \x20 local()\n\
    \x20 return \"OK\"\n\
    }\n";

/// An extension on `Buildee<UserKlass>` called on the builder receiver, implicitly or through
/// `this`, infers `FT = UserKlass`: the expression-bodied functions return `Buildee<UserKlass>`.
#[test]
fn an_extension_receiver_infers_the_builder_variable() {
    common::assert_class_matches_kotlinc(
        "BuilderExtensionReceiver",
        EXTENSION_FUNCTION,
        "BuilderExtensionReceiverKt",
    );
    common::expect_box_same_as_kotlinc(EXTENSION_FUNCTION, "BuilderExtensionReceiverRun");
}

const EXTENSION_PROPERTY: &str = "class Buildee<CT>\n\
    fun <FT> build(instructions: Buildee<FT>.() -> Unit): Buildee<FT> {\n\
    \x20 val buildee = Buildee<FT>()\n\
    \x20 buildee.instructions()\n\
    \x20 return buildee\n\
    }\n\
    class UserKlass\n\
    val Buildee<UserKlass>.value: UserKlass get() = UserKlass()\n\
    var Buildee<UserKlass>.variable: UserKlass get() = UserKlass(); set(value) {}\n\
    fun implicitRead() = build { value }\n\
    fun explicitRead() = build { this.variable }\n\
    fun implicitWrite(): Buildee<UserKlass> {\n\
    \x20 val buildee = build { variable = UserKlass() }\n\
    \x20 return buildee\n\
    }\n\
    fun explicitWrite(): Buildee<UserKlass> {\n\
    \x20 val buildee = build { this.variable = UserKlass() }\n\
    \x20 return buildee\n\
    }\n\
    fun box(): String {\n\
    \x20 implicitRead()\n\
    \x20 explicitRead()\n\
    \x20 implicitWrite()\n\
    \x20 explicitWrite()\n\
    \x20 return \"OK\"\n\
    }\n";

/// Reading or writing an extension property declared on `Buildee<UserKlass>` through the builder
/// receiver infers `FT = UserKlass` the same way. The comparison covers code only: the one-line
/// accessor pair's metadata getter flags differ from kotlinc independently of inference.
#[test]
fn an_extension_property_receiver_infers_the_builder_variable() {
    common::assert_class_code_matches_kotlinc(
        "BuilderExtensionProperty",
        EXTENSION_PROPERTY,
        "BuilderExtensionPropertyKt",
    );
    common::expect_box_same_as_kotlinc(EXTENSION_PROPERTY, "BuilderExtensionPropertyRun");
}

const MEMBER_SHADOWS_EXTENSION_WRITE: &str = "class B<T> { var p: Int = 0 }\n\
    fun <T> build(block: B<T>.() -> Unit): B<T> {\n\
    \x20 val b = B<T>()\n\
    \x20 b.block()\n\
    \x20 return b\n\
    }\n\
    var B<String>.p: Int get() = 0; set(value) {}\n\
    fun B<Int>.fixInt() {}\n\
    fun explicitWrite(): B<Int> = build { this.p = 1 }\n\
    fun implicitWrite(): B<Int> = build { p = 2 }\n\
    fun explicitRead(): B<Int> = build { this.p }\n\
    fun postponedWrite(): B<Int> {\n\
    \x20 val b = build { this.p = 3; fixInt() }\n\
    \x20 return b\n\
    }\n\
    fun box(): String {\n\
    \x20 if (explicitWrite().p != 1) return \"explicit\"\n\
    \x20 if (implicitWrite().p != 2) return \"implicit\"\n\
    \x20 if (explicitRead().p != 0) return \"read\"\n\
    \x20 if (postponedWrite().p != 3) return \"postponed\"\n\
    \x20 return \"OK\"\n\
    }\n";

/// A member property outranks an extension property on the builder receiver. The shadowed
/// `B<String>.p` is never selected, so it contributes no `T = String` constraint: the expected
/// `B<Int>`, or the later `fixInt()` while `T` is still postponed, alone fixes `T`, and the write
/// goes to the member.
#[test]
fn a_member_write_on_the_builder_receiver_ignores_a_shadowed_extension() {
    common::assert_class_code_matches_kotlinc(
        "BuilderMemberShadowsExtension",
        MEMBER_SHADOWS_EXTENSION_WRITE,
        "BuilderMemberShadowsExtensionKt",
    );
    common::expect_box_same_as_kotlinc(
        MEMBER_SHADOWS_EXTENSION_WRITE,
        "BuilderMemberShadowsExtensionRun",
    );
}

const SELECTED_EXTENSION_WRITE: &str = "class B<T>\n\
    fun <T> build(block: B<T>.() -> Unit): B<T> {\n\
    \x20 val b = B<T>()\n\
    \x20 b.block()\n\
    \x20 return b\n\
    }\n\
    var written = -1\n\
    var B<String>.p: Int get() = written; set(value) { written = value }\n\
    fun explicitWrite(): B<String> {\n\
    \x20 val b = build { this.p = 1 }\n\
    \x20 return b\n\
    }\n\
    fun implicitWrite(): B<String> {\n\
    \x20 val b = build { p = 2 }\n\
    \x20 return b\n\
    }\n\
    fun box(): String {\n\
    \x20 if (explicitWrite().p != 1) return \"explicit\"\n\
    \x20 if (implicitWrite().p != 2) return \"implicit\"\n\
    \x20 return \"OK\"\n\
    }\n";

/// With no member `p`, the extension on `B<String>` is the selected write target and its
/// receiver constraint infers `T = String` for the builder call.
#[test]
fn a_selected_extension_write_on_the_builder_receiver_infers_the_variable() {
    common::assert_class_code_matches_kotlinc(
        "BuilderSelectedExtensionWrite",
        SELECTED_EXTENSION_WRITE,
        "BuilderSelectedExtensionWriteKt",
    );
    common::expect_box_same_as_kotlinc(
        SELECTED_EXTENSION_WRITE,
        "BuilderSelectedExtensionWriteRun",
    );
}
