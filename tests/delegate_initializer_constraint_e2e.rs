//! A builder lambda collects constraints from initialized local statements even when their
//! bindings are not the block result. KT-65262 (`pclaRootIsTrySyntheticCallWithDelegate`) reads a
//! delegated property only from an `if` condition; kotlinc still evaluates the initializer for
//! inference.

use super::common;

const SOURCE: &str = r#"interface ValueSink<in T>
fun <T> buildValue(block: (ValueSink<T>) -> Unit): T = "OK" as T
fun requireTextSink(x: ValueSink<String>) {}
class DeferredCell<E>(val value: E)
fun <L> deferValue(initializer: () -> L): DeferredCell<L> = DeferredCell(initializer())
operator fun <G> DeferredCell<G>.getValue(thisRef: Any?, property: Any?): G = value
fun usedInCondition() = buildValue { x ->
    val y by deferValue { requireTextSink(x); "OK" }
    if (y.length != 2) error("fail")
}
fun unusedDelegate() = buildValue { x ->
    val y by deferValue { requireTextSink(x); "OK" }
}
fun ordinaryInitializer() = buildValue { x ->
    val ignored = requireTextSink(x)
}
fun usedAsResult() = buildValue { x ->
    val y by deferValue { requireTextSink(x); "OK" }
    y
}
fun box(): String {
    if (usedInCondition() != "OK") return "condition"
    if (unusedDelegate() != "OK") return "unused delegate"
    if (ordinaryInitializer() != "OK") return "ordinary initializer"
    if (usedAsResult() != "OK") return "result"
    return "OK"
}
"#;

/// Each expression-bodied function returns `String`: an initializer fixes the builder type
/// variable when the binding is used only in an `if` condition, when it is unused, and when it is
/// the lambda result. The `try`/`finally` wrapper from KT-65262 is the box
/// `inference/pcla/pclaRootIsTrySyntheticCallWithDelegate.kt`.
#[test]
fn local_initializers_fix_the_builder_type_argument() {
    common::assert_class_code_matches_kotlinc(
        "DelegateInitializerConstraint",
        SOURCE,
        "DelegateInitializerConstraintKt",
    );
    common::expect_box_same_as_kotlinc(SOURCE, "DelegateInitializerConstraintRun");
}
