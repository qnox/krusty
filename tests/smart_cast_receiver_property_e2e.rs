//! A smart cast on a property of the receiver holds at the read however the receiver is spelled,
//! as kotlinc reads it. `if (error is Throwable) throw error` inside an extension on `Box<*>`, or
//! behind a smart-cast `this`, threw the erased `Object` without kotlinc's `checkcast Throwable`
//! and failed verification.

use super::common;

const SOURCE: &str = "\
    open class Base\n\
    class Box<E : Any>(val error: E) : Base()\n\
    fun Box<*>.extension(): Int {\n\
    \x20   if (error is Throwable) throw error\n\
    \x20   return 1\n\
    }\n\
    fun Base.narrowedImplicit(): Int {\n\
    \x20   if (this is Box<*>) {\n\
    \x20       if (error is Throwable) throw error\n\
    \x20   }\n\
    \x20   return 2\n\
    }\n\
    fun Base.narrowedExplicit(): Int {\n\
    \x20   if (this is Box<*>) {\n\
    \x20       if (this.error is Throwable) throw this.error\n\
    \x20   }\n\
    \x20   return 3\n\
    }\n\
    fun Box<*>.returned(): String {\n\
    \x20   if (error is String) return error\n\
    \x20   return \"\"\n\
    }\n\
    ";

#[test]
fn a_receiver_property_smart_cast_reads_like_kotlinc() {
    let pair = common::ModuleClassPair::compile(&[("Receiver.kt", SOURCE)], "ReceiverKt");
    for method in [
        "extension",
        "narrowedImplicit",
        "narrowedExplicit",
        "returned",
    ] {
        let (kotlinc, krusty) = pair.method_code("ReceiverKt", method);
        assert_eq!(krusty, kotlinc, "{method}");
    }
}

#[test]
fn a_receiver_property_smart_cast_throws_the_narrowed_value() {
    let source = format!(
        "{SOURCE}\
        class Failure : Exception()\n\
        fun thrown(block: () -> Int): String = try {{ block(); \"returned\" }} catch (e: Failure) {{ \"caught\" }}\n\
        fun box(): String {{\n\
        \x20   if (thrown {{ Box(Failure()).extension() }} != \"caught\") return \"extension\"\n\
        \x20   if (thrown {{ Box(Failure()).narrowedImplicit() }} != \"caught\") return \"implicit\"\n\
        \x20   if (thrown {{ Box(Failure()).narrowedExplicit() }} != \"caught\") return \"explicit\"\n\
        \x20   if (Box(0).extension() != 1) return \"plain\"\n\
        \x20   return Box(\"OK\").returned()\n\
        }}\n"
    );
    common::expect_box_same_as_kotlinc(&source, "SmartCastReceiverProperty");
}

/// A proof is about one receiver's selected property. Inside a receiver lambda the nearest
/// receiver's `x` is another value, whether it is the same declaration on another instance or a
/// same-named declaration of another class, and kotlinc keeps it unproven.
#[test]
fn a_proof_does_not_reach_the_same_named_property_of_a_nearer_receiver() {
    const SOURCE: &str = "\
        class Target { fun member(): Int = 1 }\n\
        class Holder(val x: Any)\n\
        class Other(val x: Any)\n\
        fun <T> within(receiver: T, block: T.() -> Int): Int = receiver.block()\n\
        fun Holder.sameDeclaration(other: Holder): Int {\n\
        \x20   if (x is Target) return within(other) { x.member() }\n\
        \x20   return 0\n\
        }\n\
        fun Holder.otherDeclaration(other: Other): Int {\n\
        \x20   if (x is Target) return within(other) { x.member() }\n\
        \x20   return 0\n\
        }\n\
        fun Holder.explicitReceiver(other: Holder): Int {\n\
        \x20   if (this.x is Target) return within(other) { this.x.member() }\n\
        \x20   return 0\n\
        }\n\
        fun Holder.proven(other: Other): Int {\n\
        \x20   if (x is Target) return within(other) { this@proven.x.member() }\n\
        \x20   return 0\n\
        }\n\
        ";
    let result = common::compiler_diagnostics(&[("Receiver.kt", SOURCE)], &[]);
    common::expect_identical_rejection(&result, "a nearer receiver's same-named property");
}
