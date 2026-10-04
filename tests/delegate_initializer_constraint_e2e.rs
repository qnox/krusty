//! A builder lambda collects a constraint from a local `by` initializer even when the property
//! is not the block result. KT-65262 (`pclaRootIsTrySyntheticCallWithDelegate`) reads the property
//! only from an `if` condition; kotlinc still infers the builder's type argument from the
//! initializer.

use super::common;

const SOURCE: &str = r#"interface Consumer<in T>
fun <T> buildConsumer(block: (Consumer<T>) -> Unit): T = "OK" as T
fun expectConsumerString(x: Consumer<String>) {}
class Lazy<E>(val value: E)
fun <L> lazy(initializer: () -> L): Lazy<L> = Lazy(initializer())
operator fun <G> Lazy<G>.getValue(thisRef: Any?, property: Any?): G = value
fun usedInCondition() = buildConsumer { x ->
    val y by lazy { expectConsumerString(x); "OK" }
    if (y.length != 2) error("fail")
}
fun unused() = buildConsumer { x ->
    val y by lazy { expectConsumerString(x); "OK" }
}
fun usedAsResult() = buildConsumer { x ->
    val y by lazy { expectConsumerString(x); "OK" }
    y
}
fun inTry() = try {
    buildConsumer { x ->
        val y by lazy { expectConsumerString(x); "OK" }
        if (y.length != 2) error("fail")
    }
} finally {
}
fun box(): String {
    if (usedInCondition() != "OK") return "condition"
    if (unused() != "OK") return "unused"
    if (usedAsResult() != "OK") return "result"
    if (inTry() != "OK") return "try"
    return "OK"
}
"#;

/// Each expression-bodied function returns `String`: the initializer's
/// `Consumer<T> <: Consumer<String>` fixes `T` when the property is used only in an `if`
/// condition, when it is unused, when it is the lambda result, and when that `if` sits in `try`.
#[test]
fn a_delegate_initializer_fixes_the_builder_type_argument() {
    common::assert_class_code_matches_kotlinc(
        "DelegateInitializerConstraint",
        SOURCE,
        "DelegateInitializerConstraintKt",
    );
    common::expect_box_same_as_kotlinc(SOURCE, "DelegateInitializerConstraintRun");
}
