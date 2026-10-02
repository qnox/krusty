//! Lifted callables preserve the source identity of every captured implicit receiver. Common IR
//! carries that identity; the JVM backend alone formats kotlinc's physical parameter/local names.

use super::common;

const RECEIVERS: &str = r#"
class Outcome

class Receiver {
    fun outcome(): Outcome = Outcome()
}

class Left { class Token }
class Right { class Token }

context(value: Receiver)
fun consume(): Outcome = value.outcome()

context(value: Right.Token)
fun consumeRight(): Outcome = Outcome()

class Holder(val owned: Outcome) {
    fun member(): () -> Outcome = { owned }
    val initialized: () -> Outcome = { owned }
}

fun <T> within(receiver: T, block: T.() -> Outcome): Outcome = receiver.block()

fun Receiver.extension(): () -> Outcome = { outcome() }

val Receiver.property: () -> Outcome get() = { outcome() }

fun labelled(receiver: Receiver): Outcome =
    within(receiver) { val nested = { outcome() }; nested.invoke() }

fun relabelled(receiver: Receiver): Outcome =
    within(receiver) outer@{ val nested = { this@outer.outcome() }; nested.invoke() }

context(_: Receiver)
fun anonymous(): () -> Outcome = { consume() }

context(_: Left.Token, _: Right.Token)
fun repeatedLabel(): () -> Outcome = { consumeRight() }

context(_: Receiver)
fun forwarded(): () -> () -> Outcome = { { consume() } }

fun functionType(): context(Receiver) () -> () -> Outcome = { { consume() } }

fun repeatedFunctionType(): context(Left.Token, Right.Token) () -> () -> Outcome =
    { { consumeRight() } }

context(named: Receiver)
fun namedValue(): () -> Outcome = { named.outcome() }
"#;

#[test]
fn lifted_callables_name_captured_receivers_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "CapturedReceiverNames",
        RECEIVERS,
        &["Holder", "CapturedReceiverNamesKt"],
    );
}

/// Kotlin 2.4.20 removed the legacy `context(Type)` source form. On older supported references,
/// pin its distinct `$context_receiver_N` provenance as an exact facade-byte comparison.
#[test]
fn a_lifted_legacy_context_receiver_keeps_its_own_role() {
    if krusty::kotlin_version::target() >= krusty::kotlin_version::KotlinVersion::V2_4_20 {
        return;
    }
    let source = "// LANGUAGE: +ContextReceivers\n\
        class LegacyOutcome\n\
        class LegacyReceiver\n\
        context(value: LegacyReceiver)\n\
        fun consumeLegacy(): LegacyOutcome = LegacyOutcome()\n\
        context(LegacyReceiver)\n\
        fun legacy(): () -> LegacyOutcome = { consumeLegacy() }\n";
    common::assert_classes_identical_to_kotlinc(
        "LegacyCapturedReceiver",
        source,
        &["LegacyCapturedReceiverKt"],
    );
}
